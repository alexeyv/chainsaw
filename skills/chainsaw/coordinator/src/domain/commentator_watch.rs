//! What the supervisor does about the commentator's context, one poll at a
//! time. A commentator on Claude or Codex is launched to compact itself at
//! `COMMENTATOR_COMPACT_TOKENS`; the watch sees the drop when it does and
//! asks for it to be brought back to work. One that stays over the line did
//! not compact itself: it is asked to stop, interrupted if it will not, then
//! compacted, and replaced if even that does not take. A commentator on an
//! agent that cannot be told when to compact is replaced after so many
//! reviews or so long instead. Counts are in polls, five seconds apart at the
//! daemon's default interval.

use super::ContextSize;

/// Where a commentator's context should be compacted: well before the agent's
/// own default, which on a million-token model is far too late.
pub const COMMENTATOR_COMPACT_TOKENS: u64 = 150_000;

/// Polls in a row over the line before the commentator is taken to have not
/// compacted itself.
pub const OVER_LINE_POLLS: u32 = 6;

/// Polls a commentator asked to stop gets to go idle before it is
/// interrupted, and again after the interrupt before it is replaced.
pub const STOP_GRACE_POLLS: u32 = 24;

/// Polls a compaction gets to show in the context before the commentator is
/// replaced, and between attempts at replacing it.
pub const COMPACT_SETTLE_POLLS: u32 = 60;

/// A reading under half the one before it is a compaction, so long as the one
/// before held at least this much. Below it, a drop is a reading the agent
/// wrote without its usage, not a compaction.
pub const COMPACTED_FROM_TOKENS: u64 = 40_000;

/// Reviews after which a commentator that cannot compact itself is replaced.
pub const TURNOVER_REVIEWS: u32 = 8;

/// Seconds after which a commentator that cannot compact itself is replaced.
pub const TURNOVER_SECONDS: i64 = 90 * 60;

/// What one poll read of the commentator.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CommentatorReading {
  pub context: ContextSize,
  /// Reviews it has delivered since it started.
  pub reviews: u32,
  /// Seconds since it started.
  pub age_seconds: i64,
}

/// What the supervisor is to do about the commentator after a poll.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CommentatorAction {
  /// Nothing.
  Wait,
  /// Its context was compacted: tell it so, where its role prompt and notes
  /// are, and which commits wait for its review.
  Recover,
  /// Ask it to finish the review it is on, write its notes and stop.
  AskToStop,
  /// It would not stop: interrupt its turn.
  Interrupt,
  /// It has stopped: compact its context.
  Compact,
  /// Start a fresh commentator on the same role prompt and notes in its place.
  Restart,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Ending {
  Compact,
  Restart,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Phase {
  /// At work, or idle with nothing to review.
  Watching,
  /// Asked to stop, and to end in `then` once idle.
  Stopping {
    then: Ending,
    polls: u32,
    interrupted: bool,
  },
  /// Sent a compaction that has not shown yet.
  Compacting { polls: u32 },
  /// Owed a replacement that has not happened yet.
  Restarting { polls: u32 },
}

/// One commentator session under watch. A replacement is a new session, and
/// gets a new watch.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CommentatorWatch {
  compacts_itself: bool,
  last: ContextSize,
  over_polls: u32,
  phase: Phase,
  recovery_owed: bool,
}

impl CommentatorWatch {
  /// A watch on a commentator last read at `context`, on an agent that was
  /// told at launch to compact itself or one that could not be.
  pub fn new(compacts_itself: bool, context: ContextSize) -> Self {
    Self {
      compacts_itself,
      last: context,
      over_polls: 0,
      phase: Phase::Watching,
      recovery_owed: false,
    }
  }

  /// Takes one poll's reading and says what to do. `is_idle` asks the
  /// runtime, and is asked only of a commentator that was asked to stop.
  pub fn poll(
    &mut self,
    reading: CommentatorReading,
    is_idle: impl FnOnce() -> bool,
  ) -> CommentatorAction {
    let compacted = self.compacts_itself && compacted(self.last, reading.context);
    if reading.context.is_known() {
      self.last = reading.context;
    }
    let replacing = matches!(self.phase, Phase::Restarting { .. });
    if !replacing && (compacted || self.recovery_owed) {
      self.phase = Phase::Watching;
      self.over_polls = 0;
      self.recovery_owed = false;
      return CommentatorAction::Recover;
    }
    match &mut self.phase {
      Phase::Watching if self.compacts_itself => {
        if reading.context.exceeds(COMMENTATOR_COMPACT_TOKENS) {
          self.over_polls += 1;
        } else {
          self.over_polls = 0;
        }
        if self.over_polls < OVER_LINE_POLLS {
          return CommentatorAction::Wait;
        }
        self.stop_then(Ending::Compact)
      }
      Phase::Watching => {
        if reading.reviews < TURNOVER_REVIEWS && reading.age_seconds < TURNOVER_SECONDS {
          return CommentatorAction::Wait;
        }
        self.stop_then(Ending::Restart)
      }
      Phase::Stopping {
        then,
        polls,
        interrupted,
      } => {
        if is_idle() {
          let then = *then;
          return self.end_in(then);
        }
        *polls += 1;
        if *polls < STOP_GRACE_POLLS {
          return CommentatorAction::Wait;
        }
        if *interrupted {
          return self.end_in(Ending::Restart);
        }
        *polls = 0;
        *interrupted = true;
        CommentatorAction::Interrupt
      }
      Phase::Compacting { polls } => {
        *polls += 1;
        if *polls < COMPACT_SETTLE_POLLS {
          return CommentatorAction::Wait;
        }
        self.end_in(Ending::Restart)
      }
      Phase::Restarting { polls } => {
        *polls += 1;
        if *polls < COMPACT_SETTLE_POLLS {
          return CommentatorAction::Wait;
        }
        *polls = 0;
        CommentatorAction::Restart
      }
    }
  }

  /// Whether new commits are held back from the commentator: from the moment
  /// it is asked to stop until it is back at work or replaced.
  pub fn holds_commits(&self) -> bool {
    self.phase != Phase::Watching
  }

  /// The recovery the last poll asked for could not be sent; the next poll
  /// asks for it again.
  pub fn owe_recovery(&mut self) {
    self.recovery_owed = true;
  }

  fn stop_then(&mut self, then: Ending) -> CommentatorAction {
    self.phase = Phase::Stopping {
      then,
      polls: 0,
      interrupted: false,
    };
    CommentatorAction::AskToStop
  }

  fn end_in(&mut self, ending: Ending) -> CommentatorAction {
    match ending {
      Ending::Compact => {
        self.phase = Phase::Compacting { polls: 0 };
        CommentatorAction::Compact
      }
      Ending::Restart => {
        self.phase = Phase::Restarting { polls: 0 };
        CommentatorAction::Restart
      }
    }
  }
}

/// Whether the context fell from `before` to `now` as only a compaction
/// makes it fall: to under half, from a reading that held enough to compact.
fn compacted(before: ContextSize, now: ContextSize) -> bool {
  match (before.known(), now.known()) {
    (Some(before), Some(now)) => before >= COMPACTED_FROM_TOKENS && now < before / 2,
    _ => false,
  }
}

#[cfg(test)]
mod tests;
