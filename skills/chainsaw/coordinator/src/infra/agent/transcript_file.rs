//! The transcript an agent hands a session: a file on disk, read in the
//! agent's own format. The format never fails; the file does, before any
//! read, when it cannot be opened.

use std::fs::File;
use std::path::{Path, PathBuf};

use anyhow::Result;

use crate::domain::{ContextSize, PromptState, Transcript};
use crate::infra::transcript_monitor::transcript_size;

/// How an agent reads a transcript in its own format, handed the file.
pub(super) trait TranscriptFormat: Sync {
  fn context_size(&self, transcript: &Path) -> ContextSize;
  fn context_before(&self, transcript: &Path, offset: u64) -> ContextSize;
  fn context_peak(
    &self,
    transcript: &Path,
    from_offset: u64,
    to_offset: Option<u64>,
  ) -> ContextSize;
  fn prompt_state(&self, transcript: &Path, offset: u64, prompt: &str) -> PromptState;
  fn latest_assistant_text(&self, transcript: &Path) -> Option<String>;
  fn output_mentions(&self, transcript: &Path, text: &str) -> bool;
  fn commit_candidates(&self, transcript: &Path, offset: u64, head: &str) -> Vec<String>;
}

/// A session's transcript file, read in its agent's format.
struct TranscriptFile {
  format: &'static dyn TranscriptFormat,
  path: PathBuf,
}

/// The transcript at `path` in `format`.
pub(super) fn transcript_file(
  format: &'static dyn TranscriptFormat,
  path: &Path,
) -> Box<dyn Transcript> {
  Box::new(TranscriptFile {
    format,
    path: path.to_owned(),
  })
}

impl Transcript for TranscriptFile {
  fn path(&self) -> &Path {
    &self.path
  }

  fn check_readable(&self) -> Result<()> {
    // The top of the error is all the lead sees, so it carries the cause; the
    // cause stays underneath for whoever wants its kind.
    File::open(&self.path).map_err(|error| {
      let message = format!(
        "supervisor: cannot read transcript {}: {error}",
        self.path.display()
      );
      anyhow::Error::new(error).context(message)
    })?;
    Ok(())
  }

  fn size(&self) -> Result<u64> {
    self.check_readable()?;
    Ok(transcript_size(&self.path))
  }

  fn context_size(&self) -> Result<ContextSize> {
    self.check_readable()?;
    Ok(self.format.context_size(&self.path))
  }

  fn context_before(&self, offset: u64) -> Result<ContextSize> {
    self.check_readable()?;
    Ok(self.format.context_before(&self.path, offset))
  }

  fn context_peak(&self, from_offset: u64, to_offset: Option<u64>) -> Result<ContextSize> {
    self.check_readable()?;
    Ok(self.format.context_peak(&self.path, from_offset, to_offset))
  }

  fn prompt_state(&self, offset: u64, prompt: &str) -> Result<PromptState> {
    self.check_readable()?;
    Ok(self.format.prompt_state(&self.path, offset, prompt))
  }

  fn latest_assistant_text(&self) -> Result<Option<String>> {
    self.check_readable()?;
    Ok(self.format.latest_assistant_text(&self.path))
  }

  fn output_mentions(&self, text: &str) -> Result<bool> {
    self.check_readable()?;
    Ok(self.format.output_mentions(&self.path, text))
  }

  fn commit_candidates(&self, offset: u64, head: &str) -> Result<Vec<String>> {
    self.check_readable()?;
    Ok(self.format.commit_candidates(&self.path, offset, head))
  }
}

#[cfg(test)]
mod tests;
