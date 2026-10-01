use std::path::Path;

use anyhow::Result;
use chrono::Utc;
use rusqlite::{Connection, Transaction};

use super::{
  all, create, get, latest_named, record_kick, record_over_limit, record_reading, stop_named,
};
use crate::domain::test_helpers::{
  format_session, format_sessions, format_time, runtime, timestamp, within,
};
use crate::domain::{AgentKind, ContextSize, Role, Session};
use crate::infra::agent::implementing;
use crate::persistence::test_fixture::database;

fn implementer(
  transaction: &Transaction<'_>,
  name: &str,
  external: &str,
) -> Result<Session<'static>> {
  create(
    transaction,
    runtime(),
    implementing,
    name,
    Role::Implementer,
    AgentKind::Claude,
    external,
    Some("base123"),
    Path::new("/transcripts/session.jsonl"),
  )
}

/// The stored row as the daemon would see it, with times in milliseconds.
fn stored_row(db: &Connection, id: i64) -> Result<String> {
  let row = db.query_row(
    "
      select name, role, agent, external_session_id, launched_head, started_at, stopped_at,
             context, context_max, last_growth, kicked_at, over_limit_at, transcript
      from sessions where id=?
      ",
    [id],
    |row| {
      Ok(format!(
        "{} {} {} {} {:?} started={} stopped={:?} context={:?}/{:?} growth={} kicked={:?} over_limit={:?} transcript={:?}",
        row.get::<_, String>(0)?,
        row.get::<_, String>(1)?,
        row.get::<_, String>(2)?,
        row.get::<_, String>(3)?,
        row.get::<_, Option<String>>(4)?,
        row.get::<_, i64>(5)?,
        row.get::<_, Option<i64>>(6)?,
        row.get::<_, Option<i64>>(7)?,
        row.get::<_, Option<i64>>(8)?,
        row.get::<_, i64>(9)?,
        row.get::<_, Option<i64>>(10)?,
        row.get::<_, Option<i64>>(11)?,
        row.get::<_, String>(12)?,
      ))
    },
  )?;
  Ok(row)
}

mod create {
  use super::*;

  #[test]
  fn should_work() -> Result<()> {
    let mut db = database();

    let before = Utc::now();
    let transaction = db.transaction()?;
    let session = implementer(&transaction, "implementer-1", "uuid-1")?;
    transaction.commit()?;
    let after = Utc::now();

    assert!(within(session.started_at(), before, after));
    assert_eq!(
      format_session(&session),
      format!(
        r#"id: 1
name: "implementer-1"
role: implementer
agent: claude
external_session_id: "uuid-1"
launched_head: "base123"
started_at: {started}
stopped_at: none
context: none
context_max: none
last_growth: {started}
kicked_at: none
over_limit_at: none
transcript: /transcripts/session.jsonl
is_live: true
can_take_task: true
can_be_kicked: true
can_latch_over_limit: true"#,
        started = format_time(session.started_at())
      )
    );
    assert_eq!(
      stored_row(&db, 1)?,
      format!(
        "implementer-1 implementer claude uuid-1 Some(\"base123\") started={millis} stopped=None context=None/None growth={millis} kicked=None over_limit=None transcript=\"/transcripts/session.jsonl\"",
        millis = session.started_at().timestamp_millis()
      )
    );
    Ok(())
  }

  #[test]
  fn should_register_a_lead_without_a_launch_head() -> Result<()> {
    let mut db = database();

    let transaction = db.transaction()?;
    let session = create(
      &transaction,
      runtime(),
      implementing,
      "lead",
      Role::Lead,
      AgentKind::Claude,
      "uuid-lead",
      None,
      Path::new("/transcripts/session.jsonl"),
    )?;
    transaction.commit()?;

    assert_eq!(session.role(), Role::Lead);
    assert_eq!(session.launched_head(), None);
    assert!(!session.can_take_task());
    Ok(())
  }

  #[test]
  fn should_fail_when_the_claude_session_is_already_registered() -> Result<()> {
    let mut db = database();

    let transaction = db.transaction()?;
    implementer(&transaction, "implementer-1", "uuid-1")?;
    let error = implementer(&transaction, "implementer-2", "uuid-1").unwrap_err();

    assert!(
      error.to_string().contains("UNIQUE constraint failed"),
      "{error}"
    );
    Ok(())
  }
}

mod get {
  use super::*;

  #[test]
  fn should_work() -> Result<()> {
    let mut db = database();
    let transaction = db.transaction()?;
    let created = implementer(&transaction, "implementer-1", "uuid-1")?;

    let found = get(&transaction, runtime(), implementing, created.id())?;

    assert_eq!(found, Some(created));
    Ok(())
  }

  #[test]
  fn should_find_nothing_when_the_session_does_not_exist() -> Result<()> {
    let mut db = database();
    let transaction = db.transaction()?;

    assert_eq!(get(&transaction, runtime(), implementing, 42)?, None);
    Ok(())
  }

  #[test]
  fn should_fail_naming_the_session_when_the_stored_role_is_unknown() -> Result<()> {
    let mut db = database();
    db.execute(
      "
        insert into sessions(
          name, role, agent, external_session_id, started_at, last_growth, transcript
        )
        values('implementer-1', 'reviewer', 'claude', 'uuid-1', 0, 0, '/transcripts/session.jsonl')
        ",
      [],
    )?;
    let transaction = db.transaction()?;

    let error = get(&transaction, runtime(), implementing, 1).unwrap_err();

    assert_eq!(
      error.to_string(),
      "session implementer-1: unknown session role \"reviewer\""
    );
    Ok(())
  }

  #[test]
  fn should_fail_naming_the_session_when_the_stored_agent_is_unknown() -> Result<()> {
    let mut db = database();
    db.execute(
      "
        insert into sessions(
          name, role, agent, external_session_id, started_at, last_growth, transcript
        )
        values('implementer-1', 'implementer', 'gemini', 'uuid-1', 0, 0, '/transcripts/session.jsonl')
        ",
      [],
    )?;
    let transaction = db.transaction()?;

    let error = get(&transaction, runtime(), implementing, 1).unwrap_err();

    assert_eq!(
      error.to_string(),
      "session implementer-1: unknown agent \"gemini\""
    );
    Ok(())
  }
}

mod latest_named {
  use super::*;

  #[test]
  fn should_work() -> Result<()> {
    let mut db = database();
    let transaction = db.transaction()?;
    implementer(&transaction, "implementer-1", "uuid-1")?;
    stop_named(&transaction, "implementer-1")?;
    let relaunched = implementer(&transaction, "implementer-1", "uuid-2")?;
    implementer(&transaction, "implementer-2", "uuid-3")?;

    let found = latest_named(&transaction, runtime(), implementing, "implementer-1")?;

    assert_eq!(found, Some(relaunched));
    Ok(())
  }

  #[test]
  fn should_find_nothing_when_no_session_has_the_name() -> Result<()> {
    let mut db = database();
    let transaction = db.transaction()?;
    implementer(&transaction, "implementer-1", "uuid-1")?;

    assert_eq!(
      latest_named(&transaction, runtime(), implementing, "implementer-2")?,
      None
    );
    Ok(())
  }
}

mod all {
  use super::*;

  #[test]
  fn should_work() -> Result<()> {
    let mut db = database();
    let transaction = db.transaction()?;
    let lead = create(
      &transaction,
      runtime(),
      implementing,
      "lead",
      Role::Lead,
      AgentKind::Claude,
      "uuid-lead",
      None,
      Path::new("/transcripts/session.jsonl"),
    )?;
    let first = implementer(&transaction, "implementer-1", "uuid-1")?;
    stop_named(&transaction, "implementer-1")?;
    let second = implementer(&transaction, "implementer-1", "uuid-2")?;

    let sessions = all(&transaction, runtime(), implementing)?;

    let stopped = get(&transaction, runtime(), implementing, first.id())?.unwrap();
    assert_eq!(
      format_sessions(&sessions),
      format_sessions(&[lead, stopped, second])
    );
    Ok(())
  }

  #[test]
  fn should_find_nothing_when_there_are_no_sessions() -> Result<()> {
    let mut db = database();
    let transaction = db.transaction()?;

    assert_eq!(all(&transaction, runtime(), implementing)?, Vec::new());
    Ok(())
  }
}

mod stop_named {
  use super::*;

  #[test]
  fn should_work() -> Result<()> {
    let mut db = database();
    let transaction = db.transaction()?;
    let session = implementer(&transaction, "implementer-1", "uuid-1")?;
    let other = implementer(&transaction, "implementer-2", "uuid-2")?;

    let before = Utc::now();
    let stopped = stop_named(&transaction, "implementer-1")?;
    let after = Utc::now();

    let session = get(&transaction, runtime(), implementing, session.id())?.unwrap();
    let stopped_at = session.stopped_at().unwrap();
    assert_eq!(stopped, 1);
    assert!(within(stopped_at, before, after));
    assert!(!session.is_live());
    assert_eq!(
      get(&transaction, runtime(), implementing, other.id())?,
      Some(other)
    );
    Ok(())
  }

  #[test]
  fn should_leave_an_already_stopped_session_alone() -> Result<()> {
    let mut db = database();
    let transaction = db.transaction()?;
    let session = implementer(&transaction, "implementer-1", "uuid-1")?;
    stop_named(&transaction, "implementer-1")?;
    let first_stop = get(&transaction, runtime(), implementing, session.id())?.unwrap();

    let stopped = stop_named(&transaction, "implementer-1")?;

    assert_eq!(stopped, 0);
    assert_eq!(
      get(&transaction, runtime(), implementing, session.id())?,
      Some(first_stop)
    );
    Ok(())
  }

  #[test]
  fn should_stop_nothing_when_no_session_has_the_name() -> Result<()> {
    let mut db = database();
    let transaction = db.transaction()?;
    implementer(&transaction, "implementer-1", "uuid-1")?;

    assert_eq!(stop_named(&transaction, "implementer-2")?, 0);
    assert!(
      latest_named(&transaction, runtime(), implementing, "implementer-1")?
        .unwrap()
        .is_live()
    );
    Ok(())
  }
}

mod record_reading {
  use super::*;

  #[test]
  fn should_work() -> Result<()> {
    let mut db = database();
    let transaction = db.transaction()?;
    let session = implementer(&transaction, "implementer-1", "uuid-1")?;
    let started = session.started_at();
    let polled = started + chrono::Duration::seconds(30);

    let read = record_reading(
      &transaction,
      runtime(),
      implementing,
      session.id(),
      ContextSize::tokens(4_000),
      true,
      polled,
    )?;

    assert_eq!(
      format_session(&read),
      format!(
        r#"id: 1
name: "implementer-1"
role: implementer
agent: claude
external_session_id: "uuid-1"
launched_head: "base123"
started_at: {started}
stopped_at: none
context: 4000
context_max: 4000
last_growth: {polled}
kicked_at: none
over_limit_at: none
transcript: /transcripts/session.jsonl
is_live: true
can_take_task: true
can_be_kicked: true
can_latch_over_limit: true"#,
        started = format_time(started),
        polled = format_time(polled)
      )
    );
    Ok(())
  }

  #[test]
  fn should_keep_the_last_growth_and_the_kick_when_the_transcript_did_not_grow() -> Result<()> {
    let mut db = database();
    let transaction = db.transaction()?;
    let session = implementer(&transaction, "implementer-1", "uuid-1")?;
    let grown = session.started_at() + chrono::Duration::seconds(30);
    record_reading(
      &transaction,
      runtime(),
      implementing,
      session.id(),
      ContextSize::tokens(4_000),
      true,
      grown,
    )?;
    let kicked = record_kick(&transaction, runtime(), implementing, session.id())?;

    let read = record_reading(
      &transaction,
      runtime(),
      implementing,
      session.id(),
      ContextSize::tokens(4_000),
      false,
      grown + chrono::Duration::seconds(700),
    )?;

    assert_eq!(read.last_growth(), grown);
    assert_eq!(read.kicked_at(), kicked.kicked_at());
    assert!(!read.can_be_kicked());
    Ok(())
  }

  #[test]
  fn should_clear_the_kick_when_the_transcript_grows_again() -> Result<()> {
    let mut db = database();
    let transaction = db.transaction()?;
    let session = implementer(&transaction, "implementer-1", "uuid-1")?;
    record_kick(&transaction, runtime(), implementing, session.id())?;
    let grown = session.started_at() + chrono::Duration::seconds(900);

    let read = record_reading(
      &transaction,
      runtime(),
      implementing,
      session.id(),
      ContextSize::tokens(100),
      true,
      grown,
    )?;

    assert_eq!(read.kicked_at(), None);
    assert_eq!(read.last_growth(), grown);
    assert!(read.can_be_kicked());
    Ok(())
  }

  #[test]
  fn should_keep_the_maximum_when_the_context_shrinks() -> Result<()> {
    let mut db = database();
    let transaction = db.transaction()?;
    let session = implementer(&transaction, "implementer-1", "uuid-1")?;
    let at = session.started_at() + chrono::Duration::seconds(30);
    record_reading(
      &transaction,
      runtime(),
      implementing,
      session.id(),
      ContextSize::tokens(9_000),
      true,
      at,
    )?;

    let read = record_reading(
      &transaction,
      runtime(),
      implementing,
      session.id(),
      ContextSize::tokens(2_000),
      true,
      at,
    )?;

    assert_eq!(read.context(), ContextSize::tokens(2_000));
    assert_eq!(read.context_max(), ContextSize::tokens(9_000));
    Ok(())
  }

  #[test]
  fn should_leave_the_context_unknown_when_the_reading_cannot_say() -> Result<()> {
    let mut db = database();
    let transaction = db.transaction()?;
    let session = implementer(&transaction, "implementer-1", "uuid-1")?;
    let at = session.started_at() + chrono::Duration::seconds(30);

    let read = record_reading(
      &transaction,
      runtime(),
      implementing,
      session.id(),
      ContextSize::UNKNOWN,
      true,
      at,
    )?;

    assert_eq!(read.context(), ContextSize::UNKNOWN);
    assert_eq!(read.context_max(), ContextSize::UNKNOWN);
    assert_eq!(read.last_growth(), at);
    Ok(())
  }

  #[test]
  fn should_keep_the_maximum_when_a_later_reading_is_unknown() -> Result<()> {
    let mut db = database();
    let transaction = db.transaction()?;
    let session = implementer(&transaction, "implementer-1", "uuid-1")?;
    let at = session.started_at() + chrono::Duration::seconds(30);
    record_reading(
      &transaction,
      runtime(),
      implementing,
      session.id(),
      ContextSize::tokens(9_000),
      true,
      at,
    )?;

    let read = record_reading(
      &transaction,
      runtime(),
      implementing,
      session.id(),
      ContextSize::UNKNOWN,
      true,
      at,
    )?;

    assert_eq!(read.context(), ContextSize::UNKNOWN);
    assert_eq!(read.context_max(), ContextSize::tokens(9_000));
    Ok(())
  }

  #[test]
  fn should_fail_when_the_session_does_not_exist() -> Result<()> {
    let mut db = database();
    let transaction = db.transaction()?;

    let error = record_reading(
      &transaction,
      runtime(),
      implementing,
      42,
      ContextSize::tokens(1),
      true,
      timestamp(1_700_000_000),
    )
    .unwrap_err();

    assert_eq!(error.to_string(), "session 42 is missing");
    Ok(())
  }
}

mod record_kick {
  use super::*;

  #[test]
  fn should_work() -> Result<()> {
    let mut db = database();
    let transaction = db.transaction()?;
    let session = implementer(&transaction, "implementer-1", "uuid-1")?;

    let before = Utc::now();
    let kicked = record_kick(&transaction, runtime(), implementing, session.id())?;
    let after = Utc::now();

    let kicked_at = kicked.kicked_at().unwrap();
    assert!(within(kicked_at, before, after));
    assert!(!kicked.can_be_kicked());
    Ok(())
  }

  #[test]
  fn should_fail_when_the_session_does_not_exist() -> Result<()> {
    let mut db = database();
    let transaction = db.transaction()?;

    let error = record_kick(&transaction, runtime(), implementing, 42).unwrap_err();

    assert_eq!(error.to_string(), "session 42 is missing");
    Ok(())
  }
}

mod record_over_limit {
  use super::*;

  #[test]
  fn should_work() -> Result<()> {
    let mut db = database();
    let transaction = db.transaction()?;
    let lead = create(
      &transaction,
      runtime(),
      implementing,
      "lead",
      Role::Lead,
      AgentKind::Claude,
      "uuid-lead",
      None,
      Path::new("/transcripts/session.jsonl"),
    )?;

    let before = Utc::now();
    let latched = record_over_limit(&transaction, runtime(), implementing, lead.id())?;
    let after = Utc::now();

    let over_limit_at = latched.over_limit_at().unwrap();
    assert!(within(over_limit_at, before, after));
    assert!(!latched.can_latch_over_limit());
    assert_eq!(latched.kicked_at(), None);
    assert!(latched.is_live());
    Ok(())
  }

  #[test]
  fn should_keep_the_latch_when_the_transcript_grows() -> Result<()> {
    let mut db = database();
    let transaction = db.transaction()?;
    let lead = create(
      &transaction,
      runtime(),
      implementing,
      "lead",
      Role::Lead,
      AgentKind::Claude,
      "uuid-lead",
      None,
      Path::new("/transcripts/session.jsonl"),
    )?;
    let latched = record_over_limit(&transaction, runtime(), implementing, lead.id())?;
    let grown = lead.started_at() + chrono::Duration::seconds(900);

    let read = record_reading(
      &transaction,
      runtime(),
      implementing,
      lead.id(),
      ContextSize::tokens(260_000),
      true,
      grown,
    )?;

    assert_eq!(read.over_limit_at(), latched.over_limit_at());
    assert!(!read.can_latch_over_limit());
    Ok(())
  }

  #[test]
  fn should_leave_a_relaunched_lead_unlatched() -> Result<()> {
    let mut db = database();
    let transaction = db.transaction()?;
    let first = create(
      &transaction,
      runtime(),
      implementing,
      "lead",
      Role::Lead,
      AgentKind::Claude,
      "uuid-lead-1",
      None,
      Path::new("/transcripts/session.jsonl"),
    )?;
    record_over_limit(&transaction, runtime(), implementing, first.id())?;
    stop_named(&transaction, "lead")?;

    let second = create(
      &transaction,
      runtime(),
      implementing,
      "lead",
      Role::Lead,
      AgentKind::Claude,
      "uuid-lead-2",
      None,
      Path::new("/transcripts/session.jsonl"),
    )?;

    assert_eq!(second.over_limit_at(), None);
    assert!(second.can_latch_over_limit());
    assert!(
      !get(&transaction, runtime(), implementing, first.id())?
        .unwrap()
        .can_latch_over_limit()
    );
    Ok(())
  }

  #[test]
  fn should_fail_when_the_session_does_not_exist() -> Result<()> {
    let mut db = database();
    let transaction = db.transaction()?;

    let error = record_over_limit(&transaction, runtime(), implementing, 42).unwrap_err();

    assert_eq!(error.to_string(), "session 42 is missing");
    Ok(())
  }
}
