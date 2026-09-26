//! The commentator's wake signal: which transcripts grew since the last look.
//!
//! A watch keyed on file creation sees a new implementer's transcript appear
//! and is then blind while it fills; a watch keyed on modification fires on
//! every appended line, hundreds of times per task. Comparing size snapshots
//! on a coarse cadence reports only what grew since the last look, at most
//! once per interval.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// Bytes in a transcript, or zero while it does not exist yet.
pub fn transcript_size(path: Option<&Path>) -> u64 {
  path
    .and_then(|path| path.metadata().ok())
    .map_or(0, |metadata| metadata.len())
}

/// Holds the sizes seen at the last look. Transcripts are named by the
/// caller; the wake line repeats those names.
pub struct TranscriptMonitor {
  sizes: BTreeMap<String, u64>,
}

impl TranscriptMonitor {
  /// Starts from a first look, so the first poll reports only what grew
  /// after it.
  pub fn new(transcripts: &[(String, PathBuf)]) -> Self {
    Self {
      sizes: sizes(transcripts),
    }
  }

  /// One wake line, or None when nothing grew: `transcripts grew: a +5, b +2`.
  /// A transcript not seen at the last look counts as growth from zero; one
  /// that shrank or vanished is not growth.
  pub fn poll(&mut self, transcripts: &[(String, PathBuf)]) -> Option<String> {
    let after = sizes(transcripts);
    let grown = after
      .iter()
      .filter_map(|(name, &size)| {
        let previous = self.sizes.get(name).copied().unwrap_or(0);
        (size > previous).then(|| format!("{name} +{}", size - previous))
      })
      .collect::<Vec<_>>();
    self.sizes = after;
    if grown.is_empty() {
      return None;
    }
    Some(format!("transcripts grew: {}", grown.join(", ")))
  }
}

fn sizes(transcripts: &[(String, PathBuf)]) -> BTreeMap<String, u64> {
  transcripts
    .iter()
    .map(|(name, path)| (name.clone(), transcript_size(Some(path))))
    .collect()
}

#[cfg(test)]
mod tests;
