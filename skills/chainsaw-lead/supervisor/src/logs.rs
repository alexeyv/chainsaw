use std::collections::{BTreeMap, HashMap};
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

use regex::Regex;
use serde_json::Value;

pub fn file_size(path: Option<&Path>) -> u64 {
  path
    .and_then(|path| path.metadata().ok())
    .map_or(0, |metadata| metadata.len())
}

pub fn context_size(path: Option<&Path>) -> u64 {
  let Some(path) = path else {
    return 0;
  };
  let Ok(text) = read_lossy(path, 0, None) else {
    return 0;
  };
  text
    .lines()
    .rev()
    .take(50)
    .find_map(usage_of_line)
    .unwrap_or_default()
}

pub fn context_before(path: Option<&Path>, offset: u64) -> u64 {
  let Some(path) = path else {
    return 0;
  };
  read_lossy(path, 0, Some(offset))
    .map(|text| {
      text
        .lines()
        .filter_map(usage_of_line)
        .next_back()
        .unwrap_or(0)
    })
    .unwrap_or_default()
}

pub fn context_peak(path: Option<&Path>, start: u64, end: Option<u64>) -> u64 {
  let Some(path) = path else {
    return 0;
  };
  read_lossy(path, start, end)
    .map(|text| text.lines().filter_map(usage_of_line).max().unwrap_or(0))
    .unwrap_or_default()
}

fn usage_of_line(line: &str) -> Option<u64> {
  let entry: Value = serde_json::from_str(line).ok()?;
  if entry
    .get("isSidechain")
    .and_then(Value::as_bool)
    .unwrap_or(false)
  {
    return None;
  }
  let kind = entry.get("type")?.as_str()?;
  if kind != "assistant" && kind != "tool_result" {
    return None;
  }
  let usage = entry
    .pointer("/message/usage")
    .or_else(|| entry.get("usage"))?;
  Some(
    token_field(usage, "input_tokens")
      + token_field(usage, "cache_read_input_tokens")
      + token_field(usage, "cache_creation_input_tokens"),
  )
}

fn token_field(usage: &Value, key: &str) -> u64 {
  usage.get(key).and_then(Value::as_u64).unwrap_or_default()
}

/// Byte size of every transcript in the directory, keyed by session id.
pub fn transcript_sizes(dir: &Path) -> BTreeMap<String, u64> {
  let Ok(entries) = std::fs::read_dir(dir) else {
    return BTreeMap::new();
  };
  entries
    .filter_map(Result::ok)
    .filter_map(|entry| {
      let path = entry.path();
      let name = path.file_stem()?.to_str()?.to_owned();
      (path.extension()?.to_str()? == "jsonl").then(|| (name, file_size(Some(&path))))
    })
    .collect()
}

/// Which transcripts grew between two size snapshots, and by how many bytes.
/// A transcript that did not exist before counts as growth from zero.
///
/// This is the commentator's wake signal. A watch keyed on file creation sees
/// a new implementer's transcript appear and is then blind while it fills; a
/// watch keyed on modification fires on every appended line, hundreds of
/// times per task. Comparing snapshots on a coarse cadence reports only what
/// grew since the last look, at most once per interval.
pub fn transcript_growth(
  before: &BTreeMap<String, u64>,
  after: &BTreeMap<String, u64>,
) -> Vec<(String, u64)> {
  after
    .iter()
    .filter_map(|(name, &size)| {
      let previous = before.get(name).copied().unwrap_or(0);
      (size > previous).then(|| (name.clone(), size - previous))
    })
    .collect()
}

/// One wake line, or None when nothing grew: `transcripts grew: a +5, b +2`.
pub fn format_growth(growth: &[(String, u64)]) -> Option<String> {
  if growth.is_empty() {
    return None;
  }
  let parts = growth
    .iter()
    .map(|(name, delta)| format!("{name} +{delta}"))
    .collect::<Vec<_>>()
    .join(", ");
  Some(format!("transcripts grew: {parts}"))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PromptLanding {
  Landed,
  Queued,
}

pub fn prompt_landed(path: &Path, offset: u64, needle: &str) -> Option<PromptLanding> {
  let mut queued = false;
  for entry in entries(path, offset) {
    if entry.get("type").and_then(Value::as_str) == Some("user")
      && text_of(entry.pointer("/message/content").unwrap_or(&Value::Null)).contains(needle)
    {
      return Some(PromptLanding::Landed);
    }
    if entry.get("type").and_then(Value::as_str) == Some("queue-operation")
      && entry.get("operation").and_then(Value::as_str) == Some("enqueue")
      && entry
        .get("content")
        .and_then(Value::as_str)
        .is_some_and(|content| content.contains(needle))
    {
      queued = true;
    }
  }
  queued.then_some(PromptLanding::Queued)
}

pub fn latest_assistant_text(path: &Path) -> Option<String> {
  let mut last = None;
  for entry in entries(path, 0) {
    if entry.get("type").and_then(Value::as_str) != Some("assistant") {
      continue;
    }
    if let Some(content) = entry.pointer("/message/content").and_then(Value::as_array) {
      for block in content {
        if block.get("type").and_then(Value::as_str) == Some("text")
          && let Some(text) = block.get("text").and_then(Value::as_str)
          && !text.trim().is_empty()
        {
          last = Some(text.to_owned());
        }
      }
    }
  }
  last
}

pub fn bash_commands(path: &Path, offset: u64) -> Vec<(String, bool)> {
  let mut calls = Vec::new();
  let mut results = HashMap::new();
  for entry in entries(path, offset) {
    let kind = entry.get("type").and_then(Value::as_str);
    let Some(content) = entry.pointer("/message/content").and_then(Value::as_array) else {
      continue;
    };
    for block in content {
      if kind == Some("assistant")
        && block.get("type").and_then(Value::as_str) == Some("tool_use")
        && block.get("name").and_then(Value::as_str) == Some("Bash")
      {
        let id = block
          .get("id")
          .and_then(Value::as_str)
          .unwrap_or_default()
          .to_owned();
        let command = block
          .pointer("/input/command")
          .and_then(Value::as_str)
          .unwrap_or_default()
          .to_owned();
        calls.push((id, command));
      } else if block.get("type").and_then(Value::as_str) == Some("tool_result") {
        let id = block
          .get("tool_use_id")
          .and_then(Value::as_str)
          .unwrap_or_default()
          .to_owned();
        let body = text_of(block.get("content").unwrap_or(&Value::Null));
        let is_error = block
          .get("is_error")
          .and_then(Value::as_bool)
          .unwrap_or(false);
        let exit_error = body.trim_start().starts_with("Exit code ")
          && body
            .trim_start()
            .strip_prefix("Exit code ")
            .and_then(|tail| tail.split_whitespace().next())
            .is_some_and(|code| code.parse::<u32>().is_ok());
        results.insert(id, !is_error && !exit_error);
      }
    }
  }
  calls
    .into_iter()
    .map(|(id, command)| {
      let ok = results.get(&id).copied().unwrap_or(false);
      (command, ok)
    })
    .collect()
}

pub fn commits_in_log(path: &Path, offset: u64) -> Vec<String> {
  let Ok(text) = read_lossy(path, offset, None) else {
    return Vec::new();
  };
  let pattern = Regex::new(r"\[[\w/.-]+ ([0-9a-f]{7,40})\]").expect("valid commit regex");
  pattern
    .captures_iter(&text)
    .map(|capture| capture[1].to_owned())
    .collect()
}

fn entries(path: &Path, offset: u64) -> Vec<Value> {
  let Ok(text) = read_lossy(path, offset, None) else {
    return Vec::new();
  };
  text
    .lines()
    .filter_map(|line| serde_json::from_str(line).ok())
    .collect()
}

fn read_lossy(path: &Path, start: u64, end: Option<u64>) -> std::io::Result<String> {
  let mut file = File::open(path)?;
  file.seek(SeekFrom::Start(start))?;
  let mut bytes = Vec::new();
  match end {
    Some(end) => {
      file
        .take(end.saturating_sub(start))
        .read_to_end(&mut bytes)?;
    }
    None => {
      file.read_to_end(&mut bytes)?;
    }
  }
  Ok(String::from_utf8_lossy(&bytes).into_owned())
}

fn text_of(content: &Value) -> String {
  match content {
    Value::Array(blocks) => blocks
      .iter()
      .filter_map(|block| block.get("text").and_then(Value::as_str))
      .collect::<Vec<_>>()
      .join(" "),
    Value::String(text) => text.clone(),
    other => other.to_string(),
  }
}

#[cfg(test)]
mod tests {
  use std::fs;
  use std::sync::atomic::{AtomicU64, Ordering};

  use std::collections::BTreeMap;

  use super::{PromptLanding, format_growth, prompt_landed, transcript_growth, usage_of_line};

  fn sizes(pairs: &[(&str, u64)]) -> BTreeMap<String, u64> {
    pairs
      .iter()
      .map(|(name, size)| ((*name).to_owned(), *size))
      .collect()
  }

  mod transcript_growth {
    use super::*;

    #[test]
    fn should_work() {
      let before = sizes(&[("a", 10), ("b", 20)]);
      let after = sizes(&[("a", 15), ("b", 20)]);

      assert_eq!(
        format!("{:?}", transcript_growth(&before, &after)),
        r#"[("a", 5)]"#
      );
    }

    #[test]
    fn should_count_a_new_transcript_as_growth_from_zero() {
      let before = sizes(&[("a", 10)]);
      let after = sizes(&[("a", 10), ("b", 7)]);

      assert_eq!(
        format!("{:?}", transcript_growth(&before, &after)),
        r#"[("b", 7)]"#
      );
    }

    #[test]
    fn should_ignore_a_transcript_that_shrank_or_vanished() {
      let before = sizes(&[("a", 10), ("b", 20)]);
      let after = sizes(&[("a", 4)]);

      assert_eq!(format!("{:?}", transcript_growth(&before, &after)), "[]");
    }
  }

  mod format_growth {
    use super::*;

    #[test]
    fn should_work() {
      let growth = vec![("a".to_owned(), 5), ("b".to_owned(), 2)];

      assert_eq!(
        format_growth(&growth).as_deref(),
        Some("transcripts grew: a +5, b +2")
      );
    }

    #[test]
    fn should_be_silent_when_nothing_grew() {
      assert_eq!(format_growth(&[]), None);
    }
  }

  fn landing_in(transcript: &str, needle: &str) -> Option<PromptLanding> {
    static NEXT_TRANSCRIPT: AtomicU64 = AtomicU64::new(0);
    let path = std::env::temp_dir().join(format!(
      "chainsaw-prompt-landing-{}-{}.jsonl",
      std::process::id(),
      NEXT_TRANSCRIPT.fetch_add(1, Ordering::Relaxed)
    ));
    fs::write(&path, transcript).unwrap();
    let landing = prompt_landed(&path, 0, needle);
    let _ = fs::remove_file(path);
    landing
  }

  mod prompt_landed {
    use super::*;

    #[test]
    fn should_work() {
      let transcript = r#"{"type":"user","message":{"content":"deliver this prompt"}}"#;

      assert_eq!(
        landing_in(transcript, "deliver this"),
        Some(PromptLanding::Landed)
      );
    }

    #[test]
    fn should_report_a_matching_enqueue() {
      let transcript =
        r#"{"type":"queue-operation","operation":"enqueue","content":"deliver this prompt"}"#;

      assert_eq!(
        landing_in(transcript, "deliver this"),
        Some(PromptLanding::Queued)
      );
    }

    #[test]
    fn should_report_neither_when_no_entry_matches() {
      let transcript = r#"{"type":"assistant","message":{"content":"deliver this prompt"}}"#;

      assert_eq!(landing_in(transcript, "deliver this"), None);
    }

    #[test]
    fn should_ignore_an_enqueue_for_a_different_prompt() {
      let transcript =
        r#"{"type":"queue-operation","operation":"enqueue","content":"something else"}"#;

      assert_eq!(landing_in(transcript, "deliver this"), None);
    }
  }

  #[test]
  fn sums_context_tokens() {
    let line = r#"{"type":"assistant","message":{"usage":{"input_tokens":2,"cache_read_input_tokens":3,"cache_creation_input_tokens":5}}}"#;
    assert_eq!(usage_of_line(line), Some(10));
  }

  #[test]
  fn ignores_sidechain_usage() {
    let line = r#"{"type":"assistant","isSidechain":true,"message":{"usage":{"input_tokens":99}}}"#;
    assert_eq!(usage_of_line(line), None);
  }
}
