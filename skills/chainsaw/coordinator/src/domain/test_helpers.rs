use std::cell::RefCell;
use std::fmt;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use anyhow::{Result, bail};
use chrono::{DateTime, SecondsFormat, Utc};

use super::{
  Agent, AgentKind, Calibration, ContextSize, Finding, FindingVerdict, HumanWait, Launched,
  Observation, Prompt, PromptState, Role, Run, RunEvent, RunEventKind, Session, SessionKind,
  SessionRuntime, SessionStatus, StartSession, StartedSession, Task, TaskEvent, TaskState,
  Transcript,
};

/// A runtime that answers every status query the same way and accepts every
/// prompt, interrupt and wait, or refuses everything when unreachable.
pub struct FakeSessionRuntime {
  pub status: Option<SessionStatus>,
  pub reachable: bool,
}

impl SessionRuntime for FakeSessionRuntime {
  fn start(&self, _session: StartSession<'_>) -> Result<StartedSession> {
    bail!("the fake runtime starts nothing")
  }

  fn status(&self, _session_id: &str) -> Result<Option<SessionStatus>> {
    if !self.reachable {
      bail!("runtime unreachable");
    }
    Ok(self.status)
  }

  fn prompt(&self, _session_id: &str, _text: &str) -> Result<()> {
    self.reach()
  }

  fn interrupt(&self, _session_id: &str) -> Result<()> {
    self.reach()
  }

  fn wait(&self, _session_id: &str, _timeout: Duration) -> Result<()> {
    self.reach()
  }
}

impl FakeSessionRuntime {
  fn reach(&self) -> Result<()> {
    if !self.reachable {
      bail!("runtime unreachable");
    }
    Ok(())
  }
}

static IDLE_RUNTIME: FakeSessionRuntime = FakeSessionRuntime {
  status: Some(SessionStatus::Idle),
  reachable: true,
};

/// The runtime every fixture session borrows: reachable and always idle.
pub fn runtime() -> &'static dyn SessionRuntime {
  &IDLE_RUNTIME
}

/// An agent that has written nothing but, when it names one, the transcript
/// at `transcript` once that file exists: no context, no prompt seen.
pub struct FakeAgent {
  pub transcript: Option<PathBuf>,
}

static SILENT_AGENT: FakeAgent = FakeAgent { transcript: None };

impl Agent for FakeAgent {
  fn start(
    &self,
    _runtime: &dyn SessionRuntime,
    _session: StartSession<'_>,
    _prompt: &str,
  ) -> Result<Launched> {
    bail!("the fake agent starts nothing")
  }

  fn program(&self) -> &'static str {
    "fake-agent"
  }

  fn default_args(&self, _kind: SessionKind) -> String {
    String::new()
  }

  fn compact_prompt(&self) -> &'static str {
    "/compact"
  }

  fn session_id_args(&self, _id: &str) -> Option<Vec<String>> {
    None
  }

  fn session_started_since(&self, _run_dir: &Path, _since: SystemTime) -> Option<String> {
    None
  }

  fn transcript(&self, _run_dir: &Path, _external_session_id: &str) -> Option<PathBuf> {
    self.transcript.clone().filter(|path| path.is_file())
  }

  fn open_transcript(&self, path: &Path) -> Option<Box<dyn Transcript>> {
    path.is_file().then(|| {
      Box::new(SilentTranscript {
        path: path.to_owned(),
      }) as Box<dyn Transcript>
    })
  }
}

/// A transcript the fake agent opens: nothing in it, no context, no prompt
/// seen.
struct SilentTranscript {
  path: PathBuf,
}

impl Transcript for SilentTranscript {
  fn path(&self) -> &Path {
    &self.path
  }

  fn size(&self) -> u64 {
    0
  }

  fn context_size(&self) -> ContextSize {
    ContextSize::UNKNOWN
  }

  fn context_before(&self, _offset: u64) -> ContextSize {
    ContextSize::UNKNOWN
  }

  fn context_peak(&self, _start: u64, _end: Option<u64>) -> ContextSize {
    ContextSize::UNKNOWN
  }

  fn prompt_state(&self, _offset: u64, _prompt: &str) -> PromptState {
    PromptState::Unseen
  }

  fn latest_assistant_text(&self) -> Option<String> {
    None
  }

  fn output_mentions(&self, _text: &str) -> bool {
    false
  }

  fn commit_candidates(&self, _offset: u64, _head: &str) -> Vec<String> {
    Vec::new()
  }
}

/// The agent every fixture session borrows.
pub fn agent() -> &'static dyn Agent {
  &SILENT_AGENT
}

/// A runtime that starts every session it is asked to, under the external id
/// `external-<name>`, and remembers the flags each was started with.
#[derive(Default)]
pub struct RecordingSessionRuntime {
  pub started_args: RefCell<Vec<Vec<String>>>,
}

impl SessionRuntime for RecordingSessionRuntime {
  fn start(&self, session: StartSession<'_>) -> Result<StartedSession> {
    self.started_args.borrow_mut().push(session.args.to_vec());
    Ok(StartedSession {
      external_id: format!("external-{}", session.id),
      pane_id: format!("pane-{}", session.id),
      tab_id: format!("tab-{}", session.id),
    })
  }

  fn status(&self, _session_id: &str) -> Result<Option<SessionStatus>> {
    Ok(Some(SessionStatus::Idle))
  }

  fn prompt(&self, _session_id: &str, _text: &str) -> Result<()> {
    Ok(())
  }

  fn interrupt(&self, _session_id: &str) -> Result<()> {
    Ok(())
  }

  fn wait(&self, _session_id: &str, _timeout: Duration) -> Result<()> {
    Ok(())
  }
}

/// A request to start an implementer called `name` in `run_dir` with `args`.
pub fn start_request<'a>(name: &'a str, run_dir: &'a Path, args: &'a [String]) -> StartSession<'a> {
  StartSession {
    id: name,
    run_dir,
    kind: SessionKind::Implementer,
    agent: AgentKind::Claude,
    args,
  }
}

pub fn created_at() -> DateTime<Utc> {
  DateTime::from_timestamp(1_700_000_000, 0).unwrap()
}

pub fn resolved_at() -> DateTime<Utc> {
  DateTime::from_timestamp(1_700_000_001, 0).unwrap()
}

pub fn finding_from_record(
  id: i64,
  task_id: i64,
  description: &str,
  verdict: Option<FindingVerdict>,
  verdict_reason: Option<&str>,
  fix_task_id: Option<i64>,
  resolved_at: Option<DateTime<Utc>>,
) -> Result<Finding> {
  Finding::from_record(
    id,
    task_id,
    description.to_owned(),
    verdict,
    verdict_reason.map(str::to_owned),
    fix_task_id,
    created_at(),
    resolved_at,
  )
}

pub fn format_finding(finding: &Finding) -> String {
  let verdict = match finding.verdict() {
    Some(verdict) => verdict.to_string(),
    None => "none".to_owned(),
  };
  let verdict_reason = match finding.verdict_reason() {
    Some(reason) => format!("{reason:?}"),
    None => "none".to_owned(),
  };
  let fix_task_id = match finding.fix_task_id() {
    Some(id) => id.to_string(),
    None => "none".to_owned(),
  };
  let created_at = finding
    .created_at()
    .to_rfc3339_opts(SecondsFormat::Secs, true);
  let resolved_at = match finding.resolved_at() {
    Some(time) => time.to_rfc3339_opts(SecondsFormat::Secs, true),
    None => "none".to_owned(),
  };
  format!(
    "id: {}\ntask_id: {}\ndescription: {:?}\nverdict: {}\nverdict_reason: {}\nfix_task_id: {}\ncreated_at: {}\nresolved_at: {}\nis_resolved: {}",
    finding.id(),
    finding.task_id(),
    finding.description(),
    verdict,
    verdict_reason,
    fix_task_id,
    created_at,
    resolved_at,
    finding.is_resolved(),
  )
}

pub fn timestamp(seconds: i64) -> DateTime<Utc> {
  DateTime::from_timestamp(seconds, 0).unwrap()
}

pub fn format_time(time: DateTime<Utc>) -> String {
  time.to_rfc3339_opts(SecondsFormat::Secs, true)
}

/// Stored times are whole milliseconds, so compare at that grain.
pub fn within(time: DateTime<Utc>, before: DateTime<Utc>, after: DateTime<Utc>) -> bool {
  let millis = time.timestamp_millis();
  millis >= before.timestamp_millis() && millis <= after.timestamp_millis()
}

fn format_option<T: fmt::Display>(value: Option<T>) -> String {
  match value {
    Some(value) => value.to_string(),
    None => "none".to_owned(),
  }
}

fn format_option_text(value: Option<&str>) -> String {
  match value {
    Some(value) => format!("{value:?}"),
    None => "none".to_owned(),
  }
}

/// A calibration for task 7 with every measurement filled in.
pub fn calibration(id: i64, task_id: i64, wall_seconds: Option<f64>) -> Result<Calibration> {
  calibration_measuring(id, task_id, wall_seconds, [2, 20, 4, 35], [100, 900])
}

/// `counts` are predicted files, predicted lines, actual files, actual lines;
/// `context` is the context size at the start and at the end.
pub fn calibration_measuring(
  id: i64,
  task_id: i64,
  wall_seconds: Option<f64>,
  counts: [i64; 4],
  context: [u64; 2],
) -> Result<Calibration> {
  let [predicted_files, predicted_lines, actual_files, actual_lines] = counts;
  let [start, end] = context;
  Calibration::new(
    id,
    task_id,
    predicted_files,
    predicted_lines,
    actual_files,
    actual_lines,
    wall_seconds,
    created_at(),
    ContextSize::tokens(start),
    ContextSize::tokens(end),
  )
}

/// A calibration for a task whose session's context could not be read.
pub fn calibration_without_context(id: i64, task_id: i64) -> Result<Calibration> {
  Calibration::new(
    id,
    task_id,
    2,
    20,
    4,
    35,
    Some(12.5),
    created_at(),
    ContextSize::UNKNOWN,
    ContextSize::UNKNOWN,
  )
}

pub fn format_calibration(calibration: &Calibration) -> String {
  format!(
    "id: {}\ntask_id: {}\npredicted_files: {}\npredicted_lines: {}\nactual_files: {}\nactual_lines: {}\nwall_seconds: {}\ncreated_at: {}\ncontext_size_start: {}\ncontext_size_end: {}",
    calibration.id(),
    calibration.task_id(),
    calibration.predicted_files(),
    calibration.predicted_lines(),
    calibration.actual_files(),
    calibration.actual_lines(),
    format_option(calibration.wall_seconds()),
    format_time(calibration.created_at()),
    format_option(calibration.context_size_start().known()),
    format_option(calibration.context_size_end().known()),
  )
}

pub fn observation(id: i64, task_id: Option<i64>, text: &str) -> Result<Observation> {
  Observation::new(id, task_id, text.to_owned(), created_at())
}

pub fn format_observation(observation: &Observation) -> String {
  format!(
    "id: {}\ntask_id: {}\ntext: {:?}\ncreated_at: {}",
    observation.id(),
    format_option(observation.task_id()),
    observation.text(),
    format_time(observation.created_at()),
  )
}

/// A prompt just sent, not yet seen, never resent.
pub fn prompt(id: i64, session_id: i64, text: &str) -> Result<Prompt> {
  Prompt::new(id, session_id, text.to_owned(), created_at(), None, 0)
}

pub fn format_prompt(prompt: &Prompt) -> String {
  format!(
    "id: {}\nsession_id: {}\ntext: {:?}\nsent_at: {}\nseen_at: {}\nattempts: {}",
    prompt.id(),
    prompt.session_id(),
    prompt.text(),
    format_time(prompt.sent_at()),
    format_option(prompt.seen_at().map(format_time)),
    prompt.attempts(),
  )
}

/// A wait that started at the shared creation time and is still open.
pub fn open_wait(id: i64) -> Result<HumanWait> {
  HumanWait::new(id, created_at(), None)
}

/// A wait that started at the shared creation time and ended five minutes later.
pub fn ended_wait(id: i64) -> Result<HumanWait> {
  HumanWait::new(id, created_at(), Some(timestamp(1_700_000_300)))
}

pub fn format_human_wait(wait: &HumanWait) -> String {
  format!(
    "id: {}\nstarted: {}\nended: {}\nis_open: {}",
    wait.id(),
    format_time(wait.started()),
    format_option(wait.ended().map(format_time)),
    wait.is_open(),
  )
}

pub fn run_event(id: i64, kind: RunEventKind, detail: &str) -> Result<RunEvent> {
  RunEvent::new(id, kind, detail.to_owned(), created_at())
}

pub fn format_run_event(event: &RunEvent) -> String {
  format!(
    "id: {}\nkind: {}\ndetail: {:?}\ncreated_at: {}",
    event.id(),
    event.kind(),
    event.detail(),
    format_time(event.created_at()),
  )
}

/// An event stamped `id` seconds after the shared creation time.
pub fn event(id: i64, state: TaskState, reason: Option<&str>) -> Result<TaskEvent> {
  TaskEvent::new(
    id,
    state,
    reason.map(str::to_owned),
    timestamp(1_700_000_000 + id.clamp(0, 1_000)),
  )
}

pub fn format_event(event: &TaskEvent) -> String {
  format!(
    "id: {}\nstate: {}\nreason: {}\ncreated_at: {}",
    event.id(),
    event.state(),
    format_option_text(event.reason()),
    format_time(event.created_at()),
  )
}

/// The shortest legal event history ending in `state`, with `reason` on the
/// final event. Ids are 1, 2, 3, … in order.
pub fn events_through(state: TaskState, reason: Option<&str>) -> Vec<TaskEvent> {
  let path = match state {
    TaskState::Drafted => vec![TaskState::Drafted],
    TaskState::Dispatched => vec![TaskState::Drafted, TaskState::Dispatched],
    TaskState::InFlight => vec![
      TaskState::Drafted,
      TaskState::Dispatched,
      TaskState::InFlight,
    ],
    TaskState::CommittedUnverified => vec![
      TaskState::Drafted,
      TaskState::Dispatched,
      TaskState::InFlight,
      TaskState::CommittedUnverified,
    ],
    TaskState::Accepted => vec![
      TaskState::Drafted,
      TaskState::Dispatched,
      TaskState::InFlight,
      TaskState::CommittedUnverified,
      TaskState::Accepted,
    ],
    TaskState::Aborted => vec![
      TaskState::Drafted,
      TaskState::Dispatched,
      TaskState::InFlight,
      TaskState::Aborted,
    ],
  };
  let last = path.len();
  path
    .into_iter()
    .zip(1..)
    .map(|(state, id)| event(id, state, if id == last as i64 { reason } else { None }).unwrap())
    .collect()
}

/// Every `Task::new` argument, so a case can override exactly one of them.
pub struct TaskSpec {
  pub id: i64,
  pub text: &'static str,
  pub predicted_files: i64,
  pub predicted_lines: i64,
  pub session_id: Option<i64>,
  pub commit_sha: Option<&'static str>,
  pub created_at: DateTime<Utc>,
  pub retry_of_task_id: Option<i64>,
  pub transcript_offset: i64,
  pub base_head: Option<&'static str>,
  pub predicted_file_list: Option<Vec<&'static str>>,
  pub context_size_start: ContextSize,
  pub commentary_requested_at: Option<DateTime<Utc>>,
  pub commentary_delivered_at: Option<DateTime<Utc>>,
  pub events: Vec<TaskEvent>,
}

/// A drafted task with no session, commit, or file list.
pub fn drafted_task() -> TaskSpec {
  TaskSpec {
    id: 3,
    text: "implement the task",
    predicted_files: 2,
    predicted_lines: 20,
    session_id: None,
    commit_sha: None,
    created_at: created_at(),
    retry_of_task_id: None,
    transcript_offset: 0,
    base_head: None,
    predicted_file_list: None,
    context_size_start: ContextSize::UNKNOWN,
    commentary_requested_at: None,
    commentary_delivered_at: None,
    events: events_through(TaskState::Drafted, None),
  }
}

/// A task that has been through every state up to and including `state`, with
/// a session and commit assigned whether or not the state needs them.
pub fn task_in(state: TaskState, reason: Option<&str>) -> TaskSpec {
  TaskSpec {
    session_id: Some(7),
    commit_sha: Some("abc123"),
    transcript_offset: 100,
    base_head: Some("base123"),
    context_size_start: ContextSize::tokens(900),
    events: events_through(state, reason),
    ..drafted_task()
  }
}

pub fn build(spec: TaskSpec) -> Result<Task> {
  Task::new(
    spec.id,
    spec.text.to_owned(),
    spec.predicted_files,
    spec.predicted_lines,
    spec.session_id,
    spec.commit_sha.map(str::to_owned),
    spec.created_at,
    spec.retry_of_task_id,
    spec.transcript_offset,
    spec.base_head.map(str::to_owned),
    spec
      .predicted_file_list
      .map(|files| files.into_iter().map(str::to_owned).collect()),
    spec.context_size_start,
    spec.commentary_requested_at,
    spec.commentary_delivered_at,
    spec.events,
  )
}

pub fn format_task(task: &Task) -> String {
  let file_list = match task.predicted_file_list() {
    Some(files) => format!("{files:?}"),
    None => "none".to_owned(),
  };
  let events = task
    .events()
    .iter()
    .map(|event| {
      format!(
        "  {} {} {}",
        event.id(),
        event.state(),
        format_option_text(event.reason())
      )
    })
    .collect::<Vec<_>>()
    .join("\n");
  format!(
    "id: {}\ntext: {:?}\npredicted_files: {}\npredicted_lines: {}\nstate: {}\nsession_id: {}\ncommit_sha: {}\ncreated_at: {}\nretry_of_task_id: {}\nreason: {}\ntranscript_offset: {}\nbase_head: {}\npredicted_file_list: {}\ncontext_size_start: {}\ncommentary_requested_at: {}\ncommentary_delivered_at: {}\nevents:\n{}",
    task.id(),
    task.text(),
    task.predicted_files(),
    task.predicted_lines(),
    task.state(),
    format_option(task.session_id()),
    format_option_text(task.commit_sha()),
    format_time(task.created_at()),
    format_option(task.retry_of_task_id()),
    format_option_text(task.reason()),
    task.transcript_offset(),
    format_option_text(task.base_head()),
    file_list,
    format_option(task.context_size_start().known()),
    format_option(task.commentary_requested_at().map(format_time)),
    format_option(task.commentary_delivered_at().map(format_time)),
    events,
  )
}

pub fn format_tasks(tasks: &[Task]) -> String {
  tasks
    .iter()
    .map(format_task)
    .collect::<Vec<_>>()
    .join("\n\n")
}

/// Every `Session::new` argument, so a case can override exactly one of them.
pub struct SessionSpec {
  pub id: i64,
  pub name: &'static str,
  pub role: Role,
  pub agent: AgentKind,
  pub external_session_id: &'static str,
  pub launched_head: Option<&'static str>,
  pub started_at: DateTime<Utc>,
  pub stopped_at: Option<DateTime<Utc>>,
  pub context: ContextSize,
  pub context_max: ContextSize,
  pub last_growth: DateTime<Utc>,
  pub kicked_at: Option<DateTime<Utc>>,
  pub over_limit_at: Option<DateTime<Utc>>,
  pub transcript: &'static str,
}

/// A live implementer that has just been launched and read nothing yet.
pub fn launched_implementer() -> SessionSpec {
  SessionSpec {
    id: 7,
    name: "implementer-1",
    role: Role::Implementer,
    agent: AgentKind::Claude,
    external_session_id: "0b5c2e6a-1d3f-4a8b-9c7e-2f1a3b4c5d6e",
    launched_head: Some("base123"),
    started_at: created_at(),
    stopped_at: None,
    context: ContextSize::UNKNOWN,
    context_max: ContextSize::UNKNOWN,
    last_growth: created_at(),
    kicked_at: None,
    over_limit_at: None,
    transcript: "/home/alex/.claude/projects/-run/0b5c2e6a-1d3f-4a8b-9c7e-2f1a3b4c5d6e.jsonl",
  }
}

/// A transcript that is always on disk: the crate's own manifest.
pub const PRESENT_TRANSCRIPT: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/Cargo.toml");

/// A live implementer that has been polled: its transcript has grown, and
/// its context read.
pub fn working_implementer() -> SessionSpec {
  SessionSpec {
    context: ContextSize::tokens(4_000),
    context_max: ContextSize::tokens(5_000),
    last_growth: timestamp(1_700_000_600),
    ..launched_implementer()
  }
}

pub fn build_session(spec: SessionSpec) -> Result<Session<'static>> {
  build_session_on(spec, runtime())
}

/// A session driven by `runtime` instead of the fixture's idle one.
pub fn build_session_on(spec: SessionSpec, runtime: &dyn SessionRuntime) -> Result<Session<'_>> {
  Session::new(
    runtime,
    agent(),
    spec.id,
    spec.name.to_owned(),
    spec.role,
    spec.agent,
    spec.external_session_id.to_owned(),
    spec.launched_head.map(str::to_owned),
    spec.started_at,
    spec.stopped_at,
    spec.context,
    spec.context_max,
    spec.last_growth,
    spec.kicked_at,
    spec.over_limit_at,
    PathBuf::from(spec.transcript),
  )
}

pub fn format_session(session: &Session) -> String {
  format!(
    "id: {}\nname: {:?}\nrole: {}\nagent: {}\nexternal_session_id: {:?}\nlaunched_head: {}\nstarted_at: {}\nstopped_at: {}\ncontext: {}\ncontext_max: {}\nlast_growth: {}\nkicked_at: {}\nover_limit_at: {}\ntranscript: {}\nis_live: {}\ncan_take_task: {}\ncan_be_kicked: {}\ncan_latch_over_limit: {}",
    session.id(),
    session.name(),
    session.role(),
    session.agent_kind(),
    session.external_session_id(),
    format_option_text(session.launched_head()),
    format_time(session.started_at()),
    format_option(session.stopped_at().map(format_time)),
    format_option(session.context().known()),
    format_option(session.context_max().known()),
    format_time(session.last_growth()),
    format_option(session.kicked_at().map(format_time)),
    format_option(session.over_limit_at().map(format_time)),
    session.transcript_path().display(),
    session.is_live(),
    session.can_take_task(),
    session.can_be_kicked(),
    session.can_latch_over_limit(),
  )
}

pub fn format_sessions(sessions: &[Session]) -> String {
  sessions
    .iter()
    .map(format_session)
    .collect::<Vec<_>>()
    .join("\n\n")
}

/// Every `Run::new` argument, so a case can override exactly one of them.
pub struct RunSpec {
  pub daemon_seen_at: Option<DateTime<Utc>>,
  pub stop_requested_at: Option<DateTime<Utc>>,
  pub state_read_at: Option<DateTime<Utc>>,
}

/// A run as the schema seeds it: nothing has happened yet.
pub fn fresh_run() -> RunSpec {
  RunSpec {
    daemon_seen_at: None,
    stop_requested_at: None,
    state_read_at: None,
  }
}

/// A run whose state was read, then polled by a daemon five minutes later.
pub fn polled_run() -> RunSpec {
  RunSpec {
    daemon_seen_at: Some(timestamp(1_700_000_600)),
    state_read_at: Some(timestamp(1_700_000_300)),
    ..fresh_run()
  }
}

pub fn build_run(spec: RunSpec) -> Result<Run> {
  Run::new(
    spec.daemon_seen_at,
    spec.stop_requested_at,
    spec.state_read_at,
  )
}

pub fn format_run(run: &Run) -> String {
  format!(
    "daemon_seen_at: {}\nstop_requested_at: {}\nstate_read_at: {}\nis_stopping: {}",
    format_option(run.daemon_seen_at().map(format_time)),
    format_option(run.stop_requested_at().map(format_time)),
    format_option(run.state_read_at().map(format_time)),
    run.is_stopping(),
  )
}
