use std::collections::HashMap;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

use regex::Regex;
use serde_json::Value;

pub fn file_size(path: Option<&Path>) -> u64 {
    path.and_then(|path| path.metadata().ok())
        .map_or(0, |metadata| metadata.len())
}

pub fn context_size(path: Option<&Path>) -> u64 {
    let Some(path) = path else {
        return 0;
    };
    let Ok(text) = read_lossy(path, 0, None) else {
        return 0;
    };
    text.lines()
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
            text.lines()
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

pub fn prompt_landed(path: &Path, offset: u64, needle: &str) -> bool {
    entries(path, offset).into_iter().any(|entry| {
        entry.get("type").and_then(Value::as_str) == Some("user")
            && text_of(entry.pointer("/message/content").unwrap_or(&Value::Null)).contains(needle)
    })
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
    text.lines()
        .filter_map(|line| serde_json::from_str(line).ok())
        .collect()
}

fn read_lossy(path: &Path, start: u64, end: Option<u64>) -> std::io::Result<String> {
    let mut file = File::open(path)?;
    file.seek(SeekFrom::Start(start))?;
    let mut bytes = Vec::new();
    match end {
        Some(end) => {
            file.take(end.saturating_sub(start))
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
    use super::usage_of_line;

    #[test]
    fn sums_context_tokens() {
        let line = r#"{"type":"assistant","message":{"usage":{"input_tokens":2,"cache_read_input_tokens":3,"cache_creation_input_tokens":5}}}"#;
        assert_eq!(usage_of_line(line), Some(10));
    }

    #[test]
    fn ignores_sidechain_usage() {
        let line =
            r#"{"type":"assistant","isSidechain":true,"message":{"usage":{"input_tokens":99}}}"#;
        assert_eq!(usage_of_line(line), None);
    }
}
