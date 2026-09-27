//! Getting a prompt into a session. A send is only done once the session's
//! transcript shows the prompt was taken, or the session visibly went to work
//! on it; until then it is sent again as often as its agent allows. Sends are
//! serialized through a lock file so two of them cannot interleave.

use std::fs::{File, OpenOptions};
use std::path::PathBuf;
use std::thread;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use fs2::FileExt;

use super::{Coordinator, record_run_event, session_transcript};
use crate::domain::RunEventKind;
use crate::infra::agent::{self, Agent, PromptEcho, PromptState};
use crate::infra::session_runtime::{SessionRuntime, SessionStatus};
use crate::infra::store::now;
use crate::infra::transcript_monitor::transcript_size;
use crate::persistence::{prompt, session};

/// How many times an agent that echoes its prompts as it takes them is sent
/// one that has not shown up, and so how many prompt timeouts every prompt
/// has to be taken in, however many sends they are spread over.
const PROMPT_ATTEMPTS: i64 = 3;

pub(super) fn cmd_prompt(
  coordinator: &Coordinator,
  name: &str,
  text: &str,
  wait: bool,
  timeout: u64,
) -> Result<()> {
  let lock_path = PathBuf::from(format!("{}.prompt-lock", coordinator.store.path.display()));
  let lock = OpenOptions::new()
    .create(true)
    .write(true)
    .truncate(false)
    .open(lock_path)?;
  lock.lock_exclusive()?;
  // Only the prompt's opening is matched in the transcript.
  let opening: String = text.chars().take(80).collect();
  let prompt_id = coordinator
    .store
    .write(|tx| prompt::create(tx, name, text))?;
  let prompt_timeout_millis = i64::try_from(coordinator.settings.prompt_timeout().as_millis())
    .context("prompt-timeout-seconds is too large")?;
  let session = coordinator
    .store
    .read(|tx| session::latest_named(tx, name))?;
  let transcript = || -> Result<Option<PathBuf>> {
    match &session {
      Some(session) => session_transcript(coordinator, session),
      None => Ok(None),
    }
  };
  let agent = session.as_ref().map(agent::for_session);
  // Every prompt has the same time to be taken in, spread over as many sends
  // as its agent allows: one that echoes a prompt as it takes it can be sent
  // it again; one that echoes it only with its reply may be at work on it.
  let echo = agent.map_or(PromptEcho::OnTake, Agent::prompt_echo);
  let attempts = match echo {
    PromptEcho::OnTake => PROMPT_ATTEMPTS,
    PromptEcho::WithReply => 1,
  };
  let window_millis = prompt_timeout_millis * PROMPT_ATTEMPTS / attempts;

  for attempt in 1..=attempts {
    // Polling the runtime gives it a turn to deliver what a busy session has
    // queued; the state check below then reads what actually arrived. What it
    // reports is the state the send finds the session in.
    let idle_before = status_of(coordinator.runtime, name) == Some(SessionStatus::Idle);
    let path_before = transcript()?;
    let mut offset = transcript_size(path_before.as_deref());
    coordinator
      .store
      .write(|tx| prompt::record_attempt(tx, prompt_id))?;
    let _ = coordinator.runtime.prompt(name, text);
    let deadline = now() + window_millis;
    while now() < deadline {
      let status = status_of(coordinator.runtime, name);
      let path = transcript()?;
      if path != path_before {
        offset = 0;
      }
      if let Some((agent, path)) = agent.zip(path)
        && let state @ (PromptState::Started | PromptState::Queued) =
          agent.prompt_state(&path, offset, &opening)
      {
        coordinator
          .store
          .write(|tx| prompt::record_seen(tx, prompt_id))?;
        if state == PromptState::Queued {
          record_run_event(coordinator, RunEventKind::PromptQueued, name)?;
        }
        return prompt_taken(&lock, coordinator, name, agent, &transcript, wait, timeout);
      }
      // An agent that echoes a prompt only with its reply has taken it once
      // the session the send found idle is busy. A session busy already, on
      // its launch prompt or something else, proves nothing about this one.
      if let Some(agent) = agent
        && echo == PromptEcho::WithReply
        && idle_before
        && status == Some(SessionStatus::Busy)
      {
        record_run_event(
          coordinator,
          RunEventKind::PromptTaken,
          &format!("{name}: session went busy before its transcript showed the prompt"),
        )?;
        return prompt_taken(&lock, coordinator, name, agent, &transcript, wait, timeout);
      }
      thread::sleep(Duration::from_secs(1));
    }
    if attempt < attempts {
      eprintln!("prompt did not show up in the transcript (attempt {attempt}), resending");
    }
  }
  record_run_event(coordinator, RunEventKind::PromptFailed, name)?;
  FileExt::unlock(&lock)?;
  match echo {
    PromptEcho::OnTake => {
      bail!(
        "supervisor: prompt to {name} never showed up in its transcript after {attempts} attempts"
      )
    }
    PromptEcho::WithReply => bail!(
      "supervisor: prompt to {name} never showed up in its transcript and the session never went busy"
    ),
  }
}

/// What the runtime says the session is doing, or None when it has no such
/// session or cannot be reached.
pub(super) fn status_of(runtime: &dyn SessionRuntime, name: &str) -> Option<SessionStatus> {
  runtime
    .query(name)
    .ok()
    .flatten()
    .map(|session| session.status)
}

/// The prompt is taken: let the next one through and, when asked, wait for
/// the turn to end and print the last thing the agent said.
fn prompt_taken(
  lock: &File,
  coordinator: &Coordinator,
  name: &str,
  agent: &dyn Agent,
  transcript: &dyn Fn() -> Result<Option<PathBuf>>,
  wait: bool,
  timeout: u64,
) -> Result<()> {
  FileExt::unlock(lock)?;
  if wait {
    let _ = coordinator.runtime.wait(name, Duration::from_secs(timeout));
    println!(
      "{}",
      transcript()?
        .and_then(|path| agent.latest_assistant_text(&path))
        .unwrap_or_else(|| "(no assistant text)".to_owned())
    );
  }
  Ok(())
}

pub(super) fn daemon_prompt(coordinator: &Coordinator, name: &str, text: &str) -> bool {
  match cmd_prompt(coordinator, name, text, false, 300) {
    Ok(()) => true,
    Err(error) => {
      let _ = record_run_event(
        coordinator,
        RunEventKind::PromptUnreachable,
        &format!("{name}: {error}"),
      );
      false
    }
  }
}
