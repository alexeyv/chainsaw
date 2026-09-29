//! Getting a prompt into a session. A send is only done once the session's
//! transcript shows the prompt was taken, or the session visibly went to work
//! on it; until then it is sent again as often as its agent allows. Sends are
//! serialized through a lock file so two of them cannot interleave.

use std::fs::{File, OpenOptions};
use std::path::PathBuf;
use std::thread;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use chrono::Utc;
use fs2::FileExt;

use super::{record_run_event, session_transcript};
use crate::domain::{Agent, PromptEcho, PromptState, RunEventKind, Session, SessionStatus};
use crate::infra::transcript_monitor::transcript_size;
use crate::persistence::prompt;
use crate::persistence::store::Store;
use crate::run::Run;

/// How many times an agent that echoes its prompts as it takes them is sent
/// one that has not shown up, and so how many prompt timeouts every prompt
/// has to be taken in, however many sends they are spread over.
const PROMPT_ATTEMPTS: i64 = 3;

pub(super) fn cmd_prompt(
  run: &Run,
  store: &Store,
  name: &str,
  text: &str,
  wait: bool,
  timeout: u64,
) -> Result<()> {
  let lock = OpenOptions::new()
    .create(true)
    .write(true)
    .truncate(false)
    .open(run.prompt_lock_path())?;
  lock.lock_exclusive()?;
  let Some(session) = store.read(|tx| run.session_named(tx, name))? else {
    bail!("supervisor: no session {name}; launch it first");
  };
  // Only the prompt's opening is matched in the transcript.
  let opening: String = text.chars().take(80).collect();
  let prompt_id = store
    .write(|tx| prompt::create(tx, session.id(), text))?
    .id();
  let prompt_timeout_millis = i64::try_from(run.settings().prompt_timeout().as_millis())
    .context("prompt-timeout-seconds is too large")?;
  let transcript = || -> Result<Option<PathBuf>> { session_transcript(run, store, &session) };
  let agent = session.agent();
  // Every prompt has the same time to be taken in, spread over as many sends
  // as its agent allows: one that echoes a prompt as it takes it can be sent
  // it again; one that echoes it only with its reply may be at work on it.
  let echo = agent.prompt_echo();
  let attempts = match echo {
    PromptEcho::OnTake => PROMPT_ATTEMPTS,
    PromptEcho::WithReply => 1,
  };
  let window_millis = prompt_timeout_millis * PROMPT_ATTEMPTS / attempts;

  for attempt in 1..=attempts {
    // Polling the runtime gives it a turn to deliver what a busy session has
    // queued; the state check below then reads what actually arrived. What it
    // reports is the state the send finds the session in.
    let idle_before = session.status() == Some(SessionStatus::Idle);
    let path_before = transcript()?;
    let mut offset = transcript_size(path_before.as_deref());
    store.write(|tx| prompt::record_attempt(tx, prompt_id))?;
    let _ = session.prompt(text);
    let deadline = Utc::now().timestamp_millis() + window_millis;
    while Utc::now().timestamp_millis() < deadline {
      let status = session.status();
      let path = transcript()?;
      if path != path_before {
        offset = 0;
      }
      if let Some(path) = path
        && let state @ (PromptState::Started | PromptState::Queued) =
          agent.prompt_state(&path, offset, &opening)
      {
        store.write(|tx| prompt::record_seen(tx, prompt_id))?;
        if state == PromptState::Queued {
          record_run_event(store, RunEventKind::PromptQueued, name)?;
        }
        return prompt_taken(&lock, &session, agent, &transcript, wait, timeout);
      }
      // An agent that echoes a prompt only with its reply has taken it once
      // the session the send found idle is busy. A session busy already, on
      // its launch prompt or something else, proves nothing about this one.
      if echo == PromptEcho::WithReply && idle_before && status == Some(SessionStatus::Busy) {
        record_run_event(
          store,
          RunEventKind::PromptTaken,
          &format!("{name}: session went busy before its transcript showed the prompt"),
        )?;
        return prompt_taken(&lock, &session, agent, &transcript, wait, timeout);
      }
      thread::sleep(Duration::from_secs(1));
    }
    if attempt < attempts {
      eprintln!("prompt did not show up in the transcript (attempt {attempt}), resending");
    }
  }
  record_run_event(store, RunEventKind::PromptFailed, name)?;
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

/// The prompt is taken: let the next one through and, when asked, wait for
/// the turn to end and print the last thing the agent said.
fn prompt_taken(
  lock: &File,
  session: &Session<'_>,
  agent: &dyn Agent,
  transcript: &dyn Fn() -> Result<Option<PathBuf>>,
  wait: bool,
  timeout: u64,
) -> Result<()> {
  FileExt::unlock(lock)?;
  if wait {
    let _ = session.wait(Duration::from_secs(timeout));
    println!(
      "{}",
      transcript()?
        .and_then(|path| agent.latest_assistant_text(&path))
        .unwrap_or_else(|| "(no assistant text)".to_owned())
    );
  }
  Ok(())
}

pub(super) fn daemon_prompt(run: &Run, store: &Store, name: &str, text: &str) -> bool {
  match cmd_prompt(run, store, name, text, false, 300) {
    Ok(()) => true,
    Err(error) => {
      let _ = record_run_event(
        store,
        RunEventKind::PromptUnreachable,
        &format!("{name}: {error}"),
      );
      false
    }
  }
}
