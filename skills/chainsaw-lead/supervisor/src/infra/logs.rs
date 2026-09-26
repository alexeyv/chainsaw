//! Transcript bookkeeping that needs no knowledge of what is inside one:
//! sizes, and which transcripts grew between two looks.

use std::collections::BTreeMap;
use std::path::Path;

pub fn file_size(path: Option<&Path>) -> u64 {
  path
    .and_then(|path| path.metadata().ok())
    .map_or(0, |metadata| metadata.len())
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

#[cfg(test)]
mod tests;
