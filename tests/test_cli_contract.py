"""Behavioral contract for the supervisor's public CLI.

Nothing in this module imports the coordinator implementation. It drives the CLI and
reads SQLite only for durable details the CLI does not expose, so the suite can target
a replacement executable by setting CHAINSAW_SUPERVISOR_COMMAND.
"""

import json
import sqlite3
import threading
import subprocess
import time
from pathlib import Path

from tests.support import SupervisorContractCase


class TaskContractTests(SupervisorContractCase):
    def test_new_task_is_visible_to_a_later_process(self):
        task_id = self.new_task(
            text="Change the worker behavior.",
            files="worker.py,README.md",
            lines=24,
        )

        state = self.assert_success(self.cli("state"))

        self.assertEqual(task_id, 1)
        self.assertIn("1 drafted", state.stdout)

    def test_task_requires_nonempty_text(self):
        result = self.cli(
            "task", "new", "--files", "worker.py", "--predicted-lines", "5",
            input_text=" \n",
        )

        self.assert_failure(result, "task text on stdin is empty")

    def test_task_requires_a_file_list_or_file_count(self):
        result = self.cli(
            "task", "new", "--predicted-lines", "5", input_text="Do work.",
        )

        self.assert_failure(result, "needs --files a,b,c or --predicted-files N")

    def test_task_rejects_disagreeing_file_count_and_file_list(self):
        result = self.cli(
            "task", "new", "--files", "one.py,two.py", "--predicted-files", "1",
            "--predicted-lines", "5", input_text="Do work.",
        )

        self.assert_failure(result, "disagrees with --files")

    def test_retry_must_reference_an_aborted_dispatched_or_in_flight_task(self):
        original = self.new_task()

        result = self.cli(
            "task", "new", "--files", "retry.py", "--predicted-lines", "5",
            "--retry-of", str(original), input_text="Retry it.",
        )

        self.assert_failure(result, "is not aborted, dispatched, or in flight")

    def test_aborted_task_can_be_retried(self):
        original = self.new_task()
        self.launch()
        self.assert_success(self.dispatch(original))
        self.assert_success(self.cli("abort", str(original), "--reason", "ordinary failure"))

        retry = self.assert_success(self.cli(
            "task", "new", "--files", "retry.py", "--predicted-lines", "5",
            "--retry-of", str(original), input_text="Retry it.",
        ))
        state = self.assert_success(self.cli("state"))

        self.assertEqual(retry.stdout.strip(), "2")
        self.assertIn("2 drafted", state.stdout)
        self.assertIn("retry of 1", state.stdout)

    def test_config_round_trips_across_processes(self):
        self.assert_success(self.cli("config", "lead", "lead-7"))

        result = self.assert_success(self.cli("config", "lead"))

        self.assertEqual(result.stdout, "lead-7\n")


class PromptAndDispatchContractTests(SupervisorContractCase):
    def test_prompt_wait_prints_the_agent_reply(self):
        self.launch()
        self.update_zero_cost_dummy(reply_on_prompt="fixture reply")

        result = self.assert_success(
            self.cli("prompt", "worker", "hello agent", "--wait")
        )

        self.assertEqual(result.stdout, "fixture reply\n")
        self.assertEqual(
            [(operation["operation"], operation["session_id"])
             for operation in self.runtime_operations()],
            [
                ("start", "worker"),
                ("query", "worker"),
                ("prompt", "worker"),
                ("query", "worker"),
                ("wait", "worker"),
            ],
        )

    def test_a_lost_prompt_is_retried_three_times_and_reported(self):
        (self.run_dir / "chainsaw.json").write_text(
            '{"prompt-landing-seconds": 0}\n'
        )
        self.launch()
        self.update_zero_cost_dummy(drop_prompts=3)

        result = self.cli("prompt", "worker", "lost in transit")
        state = self.assert_success(self.cli("state"))

        self.assert_failure(result, "never landed after 3 attempts")
        self.assertEqual(
            [operation["operation"] for operation in self.runtime_operations()
             if operation["operation"] == "prompt"],
            ["prompt", "prompt", "prompt"],
        )
        self.assertIn("prompt-failed worker", state.stdout)

    def test_dispatch_refuses_a_session_that_is_not_an_implementer(self):
        task = self.new_task()
        self.assert_success(self.cli(
            "start-commentator", "--role-prompt", str(self.run_dir / "commentator.md"),
        ))
        commentator = next(
            name for name in self.zero_cost_dummy_state()["agents"]
            if name.startswith("commentator-")
        )

        result = self.dispatch(task, commentator)
        state = self.assert_success(self.cli("state"))

        self.assert_failure(result, "is the commentator, not an implementer")
        self.assertIn(f"{task} drafted", state.stdout)

    def test_dispatch_delivers_task_and_contract_then_rests_as_dispatched(self):
        task = self.new_task(text="Implement normal dispatch behavior.")
        self.launch()

        result = self.assert_success(self.dispatch(task))
        state = self.assert_success(self.cli("state"))
        log = self.session_log("worker").read_text()

        self.assertIn("task 1 dispatched to worker", result.stdout)
        self.assertIn("1 dispatched", state.stdout)
        self.assertIn("Implement normal dispatch behavior.", log)
        self.assertIn("Verify the tree is clean; stop if dirty.", log)
        self.assertIn("git log -1", log)

    def test_dispatch_rests_until_the_daemon_observes_implementer_log_growth(self):
        task = self.new_task(text="Start only after dispatch returns.")
        self.launch()

        dispatched = self.assert_success(self.dispatch(task))
        dispatch_state = self.assert_success(self.cli("state"))
        dispatch_offset = self.session_log("worker").stat().st_size
        observed_head = self.commit_file(
            "between.txt", "between dispatch and observation\n",
            "test: move head before observation",
        )
        self.append_usage("worker", input_tokens=17)

        daemon = self.start_daemon()
        flight_state = self.wait_for_state(f"{task} in_flight")
        self.assert_success(self.cli("stop"))
        daemon.wait(timeout=10)
        with sqlite3.connect(self.logs_dir / "chainsaw-supervisor.db") as database:
            recorded_offset, base_head = database.execute(
                "select log_offset, base_head from tasks where id=?", (task,),
            ).fetchone()

        self.assertIn(f"task {task} dispatched to worker", dispatched.stdout)
        self.assertIn(f"{task} dispatched", dispatch_state.stdout)
        self.assertNotIn(f"{task} in_flight", dispatch_state.stdout)
        self.assertIn(f"{task} in_flight", flight_state.stdout)
        self.assertEqual(recorded_offset, dispatch_offset)
        self.assertEqual(base_head, observed_head)

    def test_dispatch_requires_an_existing_session(self):
        task = self.new_task()

        result = self.dispatch(task, name="missing")

        self.assert_failure(result, "no session missing; launch it first")

    def test_only_one_implementer_may_be_in_flight(self):
        first = self.new_task(text="First task.")
        second = self.new_task(text="Second task.")
        self.launch("worker-one")
        self.assert_success(self.cli(
            "launch", "worker-two",
        ))
        self.assert_success(self.dispatch(first, "worker-one"))

        result = self.dispatch(second, "worker-two")

        self.assert_failure(result, "an implementer is already in flight")
        self.assertIn("worker-one is in flight on task 1", result.stderr)

    def test_abort_is_reachable_from_every_state_but_a_terminal_one(self):
        task = self.new_task()

        drafted = self.cli("abort", str(task), "--reason", "spec withdrawn")
        state = self.assert_success(self.cli("state"))
        again = self.cli("abort", str(task), "--reason", "already gone")

        self.assert_success(drafted)
        self.assertIn(f"{task} aborted", state.stdout)
        self.assert_failure(again, f"supervisor: task {task} is already aborted")

    def test_abort_reports_a_missing_task_before_validating_the_reason(self):
        missing = self.cli("abort", "999", "--reason", "ordinary failure")
        missing_with_blank_reason = self.cli("abort", "999", "--reason", "  ")

        self.assert_failure(missing, "supervisor: no task 999")
        self.assert_failure(missing_with_blank_reason, "supervisor: no task 999")

    def test_abort_requires_a_nonempty_reason(self):
        task = self.new_task()
        self.launch()
        self.assert_success(self.dispatch(task))

        result = self.cli("abort", str(task), "--reason", "  ")
        state = self.assert_success(self.cli("state"))

        self.assert_failure(result, "supervisor: abort requires a non-empty --reason")
        self.assertIn(f"{task} dispatched", state.stdout)

    def test_abort_interrupts_and_informs_a_live_implementer(self):
        task = self.new_task()
        self.launch()
        self.assert_success(self.dispatch(task))

        result = self.assert_success(
            self.cli("abort", str(task), "--reason", "the brief is wrong")
        )
        state = self.assert_success(self.cli("state"))
        operations = self.runtime_operations()

        self.assertIn(f"task {task} aborted", result.stdout)
        self.assertEqual(
            [operation["operation"] for operation in operations[-4:]],
            ["interrupt", "query", "prompt", "query"],
        )
        self.assertEqual(
            next(
                operation["text"] for operation in reversed(operations)
                if operation["operation"] == "prompt"
            ),
            "supervisor: task 1 is aborted: the brief is wrong. Stop, leave "
            "the tree clean, do not commit.",
        )
        self.assertIn("abort-interrupt task 1 -> worker", state.stdout)

    def test_abort_succeeds_and_records_unreachable_when_the_session_is_gone(self):
        task = self.new_task()
        self.launch()
        self.assert_success(self.dispatch(task))
        runtime_state = self.zero_cost_dummy_state()
        del runtime_state["agents"]["worker"]
        self.update_zero_cost_dummy(**runtime_state)

        result = self.assert_success(
            self.cli("abort", str(task), "--reason", "the session disappeared")
        )
        state = self.assert_success(self.cli("state"))

        self.assertIn(f"task {task} aborted", result.stdout)
        self.assertIn(f"{task} aborted", state.stdout)
        self.assertIn("abort-unreachable task 1 -> worker", state.stdout)

    def test_retry_of_a_dispatched_task_requires_a_reason_then_supersedes_it(self):
        original = self.new_task()
        self.launch()
        self.assert_success(self.dispatch(original))

        missing_reason = self.cli(
            "task", "new", "--files", "retry.py", "--predicted-lines", "5",
            "--retry-of", str(original), input_text="Retry it.",
        )
        still_dispatched = self.assert_success(self.cli("state"))
        retry = self.assert_success(self.cli(
            "task", "new", "--files", "retry.py", "--predicted-lines", "5",
            "--retry-of", str(original), "--reason", "correct the brief",
            input_text="Retry it.",
        ))
        superseded = self.assert_success(self.cli("state"))

        self.assert_failure(missing_reason, "requires a non-empty --reason")
        self.assertIn(f"{original} dispatched", still_dispatched.stdout)
        self.assertEqual(retry.stdout.strip(), "2")
        self.assertIn(f"{original} aborted", superseded.stdout)
        self.assertIn("2 drafted", superseded.stdout)
        self.assertIn("retry of 1", superseded.stdout)

    def test_retry_of_an_in_flight_task_aborts_interrupts_and_creates(self):
        original = self.new_task()
        self.launch()
        self.assert_success(self.dispatch(original))
        self.append_usage("worker", input_tokens=17)
        daemon = self.start_daemon()
        self.wait_for_state(f"{original} in_flight")
        self.assert_success(self.cli("stop"))
        daemon.wait(timeout=10)

        retry = self.assert_success(self.cli(
            "task", "new", "--files", "retry.py", "--predicted-lines", "5",
            "--retry-of", str(original), "--reason", "correct the brief",
            input_text="Retry it.",
        ))
        state = self.assert_success(self.cli("state"))

        self.assertEqual(retry.stdout.strip(), "2")
        self.assertIn(f"{original} aborted", state.stdout)
        self.assertIn("2 drafted", state.stdout)
        self.assertIn("retry of 1", state.stdout)
        self.assertIn("abort-interrupt task 1 -> worker", state.stdout)
        self.assertEqual(
            [operation["operation"] for operation in self.runtime_operations()[-4:]],
            ["interrupt", "query", "prompt", "query"],
        )


class VerificationContractTests(SupervisorContractCase):
    def test_daemon_owned_transitions_require_a_forced_coordinator_remedy(self):
        task = self.new_task()

        commit = self.cli("task", "record-commit", str(task), self.head())
        commentary = self.cli("task", "record-commentary", str(task))

        message = (
            "normally the coordinator records this on its own; use --force "
            "--reason only to remedy a coordinator failure"
        )
        self.assert_failure(commit, message)
        self.assert_failure(commentary, message)

    def test_forced_coordinator_remedies_record_transitions_and_reasons(self):
        task = self.new_task()
        self.launch()
        self.assert_success(self.dispatch(task))
        self.observe_in_flight(task)
        sha = self.commit_file()

        commit = self.assert_success(self.cli(
            "task", "record-commit", str(task), sha, "--force", "--reason",
            "daemon restarted before observing the marker",
        ))
        commentary = self.assert_success(self.cli(
            "task", "record-commentary", str(task), "--force", "--reason",
            "commentator ingestion was missed after restart",
        ))
        state = self.assert_success(self.cli("state"))

        self.assertIn("commit recorded by force", commit.stdout)
        self.assertIn("commentary delivery recorded by force", commentary.stdout)
        self.assertIn(f"{task} committed_unverified", state.stdout)
        self.assertIn("commentary-delivered@", state.stdout)
        self.assertIn("forced-commit", state.stdout)
        self.assertIn("daemon restarted before observing the marker", state.stdout)
        self.assertIn("forced-commentary", state.stdout)
        self.assertIn("commentator ingestion was missed after restart", state.stdout)

    def test_forced_commit_requires_valid_new_task_commit_evidence(self):
        task = self.new_task()
        self.launch()
        self.assert_success(self.dispatch(task))
        self.observe_in_flight(task)

        missing = self.cli(
            "task", "record-commit", str(task), "deadbeef", "--force",
            "--reason", "marker was missed",
        )
        base = self.cli(
            "task", "record-commit", str(task), self.head(), "--force",
            "--reason", "marker was missed",
        )

        self.assert_failure(missing, "does not exist in the run repository")
        self.assert_failure(base, "does not descend from task")

    def test_forced_commit_rejects_a_commit_from_an_unrelated_history(self):
        self.git("checkout", "--orphan", "unrelated")
        self.git("rm", "--cached", "seed.txt")
        (self.run_dir / "seed.txt").unlink()
        unrelated_sha = self.commit_file(
            "unrelated.txt", "unrelated\n", "test: unrelated fixture commit",
        )
        self.git("checkout", "master")
        task = self.new_task()
        self.launch()
        self.assert_success(self.dispatch(task))
        self.observe_in_flight(task)

        result = self.cli(
            "task", "record-commit", str(task), unrelated_sha, "--force",
            "--reason", "marker was missed",
        )

        self.assert_failure(result, "does not descend from task")

    def test_forced_commit_rejects_a_commit_recorded_for_another_task(self):
        first = self.new_task()
        self.launch()
        self.assert_success(self.dispatch(first))
        self.observe_in_flight(first)
        sha = self.commit_file()
        self.assert_success(self.cli(
            "task", "record-commit", str(first), sha, "--force", "--reason",
            "first marker was missed",
        ))
        second = self.new_task(text="Second task.", files="second.txt")
        self.assert_success(self.cli(
            "launch", "replacement",
        ))
        self.assert_success(self.dispatch(second, "replacement"))

        result = self.cli(
            "task", "record-commit", str(second), sha, "--force", "--reason",
            "second marker was missed",
        )

        self.assert_failure(result, f"already recorded for task {first}")

    def test_forced_coordinator_remedies_require_nonempty_reasons(self):
        task = self.new_task()

        commit_with_blank_reason = self.cli(
            "task", "record-commit", str(task), self.head(), "--force",
            "--reason", "  ",
        )
        commentary_without_reason = self.cli(
            "task", "record-commentary", str(task), "--force",
        )
        commit_reason_without_force = self.cli(
            "task", "record-commit", str(task), self.head(), "--reason",
            "manual intervention",
        )
        commentary_with_blank_reason = self.cli(
            "task", "record-commentary", str(task), "--force", "--reason", "  ",
        )

        self.assert_failure(
            commit_with_blank_reason,
            "task record-commit --force requires a non-empty --reason",
        )
        self.assert_failure(
            commentary_without_reason,
            "task record-commentary --force requires a non-empty --reason",
        )
        self.assert_failure(
            commit_reason_without_force,
            "--reason only applies with --force",
        )
        self.assert_failure(
            commentary_with_blank_reason,
            "task record-commentary --force requires a non-empty --reason",
        )

    def test_a_clean_commit_is_accepted_without_reproving_the_gate(self):
        task, sha = self.prepare_committed_task()

        result = self.assert_success(self.cli("accept", str(task)))
        state = self.assert_success(self.cli("state"))

        self.assertIn(f"task {task} accepted: checks passed at {sha[:10]}", result.stdout)
        self.assertIn("1 accepted", state.stdout)
        self.assertIn("reason: checks passed at", state.stdout)

    def test_force_requires_a_reason_and_a_reason_requires_force(self):
        task, _ = self.prepare_committed_task()

        forced_without_reason = self.cli("accept", str(task), "--force")
        reason_without_force = self.cli(
            "accept", str(task), "--reason", "looks fine to me",
        )

        self.assert_failure(
            forced_without_reason, "accept --force requires a non-empty --reason",
        )
        self.assert_failure(
            reason_without_force,
            "--reason only applies with --force; accept without it runs the checks",
        )

    def test_accept_rejects_missing_commit_evidence(self):
        task = self.new_task()
        self.launch()
        self.assert_success(self.dispatch(task))

        result = self.cli("accept", str(task))

        self.assert_failure(result, "no commit found in the implementer's log")

    def test_accept_rejects_a_dirty_tree(self):
        task, _ = self.prepare_committed_task()
        (self.run_dir / "untracked.txt").write_text("dirty\n")

        result = self.cli("accept", str(task))

        self.assert_failure(result, "tree is dirty")

    def test_accept_rejects_attribution_trailers(self):
        task, _ = self.prepare_committed_task(trailer=True)

        result = self.cli("accept", str(task))

        self.assert_failure(result, "commit carries an attribution trailer")

    def test_accept_passes_a_commit_that_later_tasks_built_on(self):
        task, task_sha = self.prepare_committed_task()
        self.commit_file("later.txt", "later\n", "feat: later fixture commit")
        self.assertNotEqual(task_sha, self.head())

        result = self.assert_success(self.cli("accept", str(task)))

        self.assertIn(f"task {task} accepted: checks passed at {task_sha[:10]}", result.stdout)

    def test_accept_rejects_a_commit_the_history_no_longer_contains(self):
        task, task_sha = self.prepare_committed_task()
        self.git("reset", "-q", "--hard", "HEAD~1")
        self.assertNotEqual(task_sha, self.head())

        result = self.cli("accept", str(task))

        self.assert_failure(
            result, "commit is not on the run's history (not an ancestor of HEAD)",
        )

    def committed_through_the_daemon(self, *, leave_behind=None):
        """A task whose commit the daemon observed, with the tree as it was then."""
        task = self.new_task()
        self.launch()
        self.assert_success(self.dispatch(task))
        self.observe_in_flight(task)
        sha = self.commit_file()
        if leave_behind:
            (self.run_dir / leave_behind).write_text("left behind\n")
        self.record_commit("worker", sha)
        daemon = self.start_daemon()
        self.wait_for_state(f"{task} committed_unverified")
        self.assert_success(self.cli("stop"))
        daemon.wait(timeout=10)
        return task, sha

    def test_accept_judges_the_tree_as_it_was_when_the_commit_landed(self):
        task, _ = self.committed_through_the_daemon(leave_behind="forgotten.txt")
        (self.run_dir / "forgotten.txt").unlink()
        self.assertEqual(self.git("status", "--porcelain").stdout, "")

        result = self.cli("accept", str(task))

        self.assert_failure(result, "tree was dirty when the commit landed: ?? forgotten.txt")

    def test_accept_ignores_the_next_implementers_edits(self):
        task, sha = self.committed_through_the_daemon()
        (self.run_dir / "next-task.txt").write_text("in flight\n")

        result = self.assert_success(self.cli("accept", str(task)))

        self.assertIn(f"task {task} accepted: checks passed at {sha[:10]}", result.stdout)

    def test_accept_retries_a_commit_marker_after_clean_head_advance(self):
        task = self.new_task()
        self.launch()
        self.assert_success(self.dispatch(task))
        self.observe_in_flight(task)
        sha = self.commit_file()
        self.append_bash("worker", "git commit -m 'fixture commit'")
        timer = threading.Timer(
            0.2, self.append_text, args=("worker", f"[chainsaw {sha[:10]}]"),
        )
        timer.start()
        self.addCleanup(timer.cancel)

        result = self.assert_success(self.cli("accept", str(task)))
        timer.join(timeout=2)

        self.assertIn(f"task {task} accepted: checks passed at {sha[:10]}", result.stdout)


class SessionContinuationContractTests(SupervisorContractCase):
    """An implementer whose task has landed takes the next one while its measured
    context is under 100k; otherwise the next task needs a fresh session."""

    def verified_first_task(self):
        task, sha = self.prepare_committed_task()
        self.assert_success(self.cli("accept", str(task)))
        return task, sha

    def test_a_session_whose_task_landed_continues_onto_the_next(self):
        self.verified_first_task()
        landed = self.commit_file(
            "between.txt", "landed meanwhile\n",
            "feat: land between tasks\n\nBody of the intermediate commit.",
        )
        second = self.new_task(text="Second task.", files="second.txt")

        result = self.assert_success(self.dispatch(second))
        prompt = self.prompts_to("worker")[-1]
        state = self.assert_success(self.cli("state"))

        self.assertIn(
            f"task {second} dispatched to worker (continuing; estimated starting context",
            result.stdout,
        )
        self.assertIn("These commits landed after your last commit at", prompt)
        self.assertIn("feat: land between tasks", prompt)
        self.assertIn("Body of the intermediate commit.", prompt)
        self.assertIn("+landed meanwhile", prompt)
        self.assertNotIn("fixture commit", prompt.split("Second task.")[0])
        self.assertIn("Second task.", prompt)
        self.assertIn("changed-file manifest", prompt)
        self.assertIn(f"{second} dispatched worker", state.stdout)
        self.assertEqual(landed, self.head())

    def test_a_continuation_omits_history_when_nothing_landed_since_its_commit(self):
        self.verified_first_task()
        second = self.new_task(text="Second task.", files="second.txt")

        self.assert_success(self.dispatch(second))
        prompt = self.prompts_to("worker")[-1]

        self.assertTrue(prompt.startswith("Second task."), prompt[:80])

    def test_a_session_past_the_continuation_limit_cannot_take_another_task(self):
        self.verified_first_task()
        self.append_usage("worker", input_tokens=500, cache_read=100_000)
        second = self.new_task(text="Second task.", files="second.txt")

        result = self.dispatch(second)

        self.assert_failure(result, "worker is at 100500 tokens")
        self.assert_failure(result, "past the 100000 continuation limit; launch a fresh implementer")

    def test_a_session_with_a_task_in_flight_cannot_take_another(self):
        first = self.new_task()
        self.launch()
        self.assert_success(self.dispatch(first))
        second = self.new_task(text="Second task.", files="second.txt")

        result = self.dispatch(second)

        self.assert_failure(result, "an implementer is already in flight (worker is in flight on task 1)")

    def test_the_last_task_is_the_most_recently_moved_not_the_highest_id(self):
        first = self.new_task(text="First.")
        second = self.new_task(text="Second.", files="second.txt")
        third = self.new_task(text="Third.", files="third.txt")
        self.launch()
        self.assert_success(self.dispatch(second))
        self.observe_in_flight(second)
        sha = self.commit_file("second.txt", "second\n", "feat: second")
        self.record_commit("worker", sha)
        self.assert_success(self.cli("task", "record-commit", str(second), sha, "--force",
                                     "--reason", "fixture"))
        self.assert_success(self.dispatch(first))
        self.assert_success(self.cli("abort", str(first), "--reason", "gave up"))

        result = self.dispatch(third)

        self.assert_failure(result, f"already took task {first} (aborted)")

    def test_an_aborted_session_cannot_take_another_task(self):
        first = self.new_task()
        self.launch()
        self.assert_success(self.dispatch(first))
        self.assert_success(self.cli("abort", str(first), "--reason", "implementation failed"))
        second = self.new_task(text="Retry elsewhere.", files="retry.txt")

        result = self.dispatch(second)

        self.assert_failure(result, "already took task 1 (aborted)")

    def test_repeated_implementer_name_creates_a_distinct_session(self):
        first = self.new_task()
        self.launch()
        self.assert_success(self.dispatch(first))
        self.assert_success(self.cli("abort", str(first), "--reason", "first session failed"))

        second = self.new_task(text="Try in a new session.", files="second.txt")
        self.assert_success(self.launch())
        dispatched = self.assert_success(self.dispatch(second))

        self.assertIn("task 2 dispatched to worker", dispatched.stdout)

    def test_launch_no_longer_refuses_while_an_earlier_session_is_idle(self):
        self.verified_first_task()

        self.assert_success(self.cli("launch", "replacement"))

    def test_dispatch_refuses_an_unreadable_settings_file(self):
        self.verified_first_task()
        self.assert_success(self.cli("launch", "replacement"))
        (self.run_dir / "chainsaw.json").write_text('{"prompt-landing-secnds": 1}\n')
        second = self.new_task(text="Second task.", files="second.txt")

        result = self.dispatch(second, "replacement")

        self.assert_failure(result, "invalid settings in")
        self.assert_failure(result, 'unknown setting "prompt-landing-secnds"')

    def test_a_committed_predecessor_releases_the_next_dispatch(self):
        self.prepare_committed_task()
        daemon = self.start_daemon()
        self.wait_for_state("1 committed_unverified")
        self.assert_success(self.cli("stop"))
        daemon.wait(timeout=10)

        second = self.new_task(text="Immediate successor.", files="second.txt")
        self.assert_success(self.cli("launch", "replacement"))

        result = self.dispatch(second, "replacement")
        state = self.assert_success(self.cli("state"))

        self.assert_success(result)
        self.assertIn("1 committed_unverified", state.stdout)
        self.assertIn(f"{second} dispatched", state.stdout)
        self.assertIn("task 2 dispatched to replacement", result.stdout)

    def test_accepting_with_a_reason_skips_the_gate_and_records_the_override(self):
        task, _ = self.prepare_committed_task()
        daemon = self.start_daemon()
        self.wait_for_state("1 committed_unverified")
        self.assert_success(self.cli("stop"))
        daemon.wait(timeout=10)

        accepted = self.assert_success(self.cli(
            "accept", str(task), "--force", "--reason", "gate failure was a known false positive",
        ))
        state = self.assert_success(self.cli("state"))

        self.assertIn("task 1 accepted without the gate", accepted.stdout)
        self.assertIn("1 accepted", state.stdout)
        self.assertIn("reason: gate failure was a known false positive", state.stdout)


class CommunicationProtocolContractTests(SupervisorContractCase):
    def test_task_lifecycle_does_not_gate_observations_or_findings(self):
        task = self.new_task(text="Commentary can arrive at any time.")
        self.assert_success(self.cli(
            "observe", "--task", str(task), "draft observation",
        ))
        self.assert_success(self.cli(
            "finding", "--task", str(task), "draft finding",
        ))
        self.launch()
        self.assert_success(self.dispatch(task))
        self.observe_in_flight(task)
        sha = self.commit_file()
        self.record_commit("worker", sha)
        self.assert_success(self.cli("accept", str(task)))

        self.assert_success(self.cli(
            "observe", "--task", str(task), "terminal observation",
        ))
        self.assert_success(self.cli(
            "finding", "--task", str(task), "terminal finding",
        ))
        polled = self.assert_success(self.cli("poll", "--task", str(task)))
        payload = json.loads(polled.stdout)

        self.assertEqual(
            [item["text"] for item in payload["observations"]],
            ["draft observation", "terminal observation"],
        )
        self.assertEqual(
            [item["description"] for item in payload["findings"]],
            ["draft finding", "terminal finding"],
        )

    def test_complete_incremental_run_wide_protocol(self):
        first_task = self.new_task(text="First reviewed task.", files="first.txt")
        second_task = self.new_task(text="Second reviewed task.", files="second.txt")

        first_observation = self.assert_success(self.cli(
            "observe", "--task", str(first_task), "first observation",
        ))
        second_observation = self.assert_success(self.cli(
            "observe", "run-wide observation",
        ))
        first_finding = self.assert_success(self.cli(
            "finding", "--task", str(first_task), "first defect",
        ))
        second_finding = self.assert_success(self.cli(
            "finding", "--task", str(second_task), "second defect",
        ))

        self.assertEqual(first_observation.stdout, "1\n")
        self.assertEqual(second_observation.stdout, "2\n")
        self.assertEqual(first_finding.stdout, "1\n")
        self.assertEqual(second_finding.stdout, "2\n")

        initial = json.loads(self.assert_success(self.cli("poll")).stdout)
        repeated = json.loads(self.assert_success(self.cli(
            "poll", "--after-observation", str(initial["observation_cursor"]),
        )).stdout)
        task_filtered = json.loads(self.assert_success(self.cli(
            "poll", "--task", str(first_task),
        )).stdout)

        self.assertEqual(initial["observation_cursor"], 2)
        self.assertEqual([item["id"] for item in initial["observations"]], [1, 2])
        self.assertEqual([item["id"] for item in initial["findings"]], [1, 2])
        self.assertEqual(repeated["observations"], [])
        self.assertEqual([item["id"] for item in repeated["findings"]], [1, 2])
        self.assertEqual(
            [item["id"] for item in task_filtered["observations"]], [1, 2],
        )
        self.assertEqual([item["id"] for item in task_filtered["findings"]], [1])

        self.assert_success(self.cli(
            "resolve", "1", "--verdict", "dropped", "--reason", "not actionable",
        ))
        after_resolution = json.loads(self.assert_success(self.cli(
            "poll", "--after-observation", "2",
        )).stdout)
        self.assert_success(self.cli(
            "resolve", "2", "--verdict", "dropped", "--reason", "also resolved",
        ))
        resolutions = json.loads(self.assert_success(self.cli("resolutions")).stdout)
        second_commentator_view = json.loads(
            self.assert_success(self.cli("resolutions")).stdout
        )

        self.assertEqual(after_resolution["observations"], [])
        self.assertEqual([item["id"] for item in after_resolution["findings"]], [2])
        self.assertEqual(
            [item["finding_id"] for item in resolutions["resolutions"]], [1, 2],
        )
        self.assertEqual(resolutions["resolutions"][0]["verdict"], "dropped")
        self.assertEqual(second_commentator_view, resolutions)
        self.assertFalse((self.logs_dir / "chainsaw-comments.md").exists())
        self.assertFalse((self.logs_dir / "chainsaw-dispositions.md").exists())

    def test_task_filtered_cursor_does_not_skip_later_relevant_observations(self):
        relevant_task = self.new_task(text="Relevant task.", files="relevant.txt")
        other_task = self.new_task(text="Other task.", files="other.txt")
        self.assert_success(self.cli(
            "observe", "--task", str(other_task), "other-task observation",
        ))

        empty = json.loads(self.assert_success(self.cli(
            "poll", "--task", str(relevant_task),
        )).stdout)
        self.assertEqual(empty["observation_cursor"], 0)
        self.assertEqual(empty["observations"], [])

        self.assert_success(self.cli(
            "observe", "--task", str(relevant_task), "relevant observation",
        ))
        later = json.loads(self.assert_success(self.cli(
            "poll", "--task", str(relevant_task), "--after-observation",
            str(empty["observation_cursor"]),
        )).stdout)

        self.assertEqual([item["id"] for item in later["observations"]], [2])
        self.assertEqual(later["observation_cursor"], 2)

    def test_resolution_validation_preserves_unresolved_findings(self):
        source = self.new_task()
        fix = self.new_task(text="Fix it.", files="fix.txt")
        self.assert_success(self.cli(
            "finding", "--task", str(source), "a defect",
        ))

        self.assert_failure(self.cli(
            "resolve", "1", "--verdict", "task", "--reason", "worth fixing",
        ), "task verdict requires a fix_task_id")
        self.assert_failure(self.cli(
            "resolve", "1", "--verdict", "dropped", "--fix-task", str(fix),
            "--reason", "not actionable",
        ), "dropped verdict cannot have a fix_task_id")
        self.assert_failure(self.cli(
            "resolve", "1", "--verdict", "task", "--fix-task", "99",
            "--reason", "worth fixing",
        ), "supervisor: no task 99")

        poll = json.loads(self.assert_success(self.cli("poll")).stdout)
        self.assertEqual([item["id"] for item in poll["findings"]], [1])

        self.assert_success(self.cli(
            "resolve", "1", "--verdict", "task", "--fix-task", str(fix),
            "--reason", "worth fixing",
        ))
        resolution = json.loads(
            self.assert_success(self.cli("resolutions")).stdout
        )["resolutions"][0]
        self.assertEqual(resolution["finding_id"], 1)
        self.assertEqual(resolution["verdict"], "task")
        self.assertEqual(resolution["fix_task_id"], fix)
        self.assertEqual(resolution["reason"], "worth fixing")
        self.assert_failure(self.cli(
            "resolve", "1", "--verdict", "dropped", "--reason", "changed mind",
        ), "finding 1 is already resolved")

    def test_legacy_commands_are_absent_and_historical_files_are_untouched(self):
        self.logs_dir.mkdir(parents=True, exist_ok=True)
        historical = {
            self.logs_dir / "chainsaw-comments.md": "historical comments\n",
            self.logs_dir / "chainsaw-dispositions.md": "historical dispositions\n",
        }
        for path, text in historical.items():
            path.write_text(text)

        self.assert_failure(
            self.cli("comments"), "unrecognized subcommand 'comments'",
        )
        self.assert_failure(
            self.cli("disposition"), "unrecognized subcommand 'disposition'",
        )
        state = self.assert_success(self.cli("state"))

        self.assertNotIn("bytes unread", state.stdout)
        for path, text in historical.items():
            self.assertEqual(path.read_text(), text)

    def test_missing_protocol_references_are_explicit_errors(self):
        self.assert_failure(self.cli(
            "observe", "--task", "99", "observation",
        ), "supervisor: no task 99")
        self.assert_failure(self.cli(
            "finding", "--task", "99", "defect",
        ), "supervisor: no task 99")
        self.assert_failure(self.cli(
            "resolve", "99", "--verdict", "dropped", "--reason", "not found",
        ), "supervisor: no finding 99")
        self.assert_failure(self.cli(
            "poll", "--task", "99",
        ), "supervisor: no task 99")


class ReportingAndDaemonContractTests(SupervisorContractCase):
    def test_context_reports_latest_non_sidechain_usage(self):
        self.launch()
        self.append_usage("worker", input_tokens=10, cache_read=20, cache_creation=3)
        self.append_usage("worker", input_tokens=999, sidechain=True)

        result = self.assert_success(self.cli("context", "worker"))

        self.assertEqual(result.stdout, "worker\t33\n")

    def test_calibration_reports_git_and_task_context_cost(self):
        task = self.new_task(lines=20)
        self.launch()
        self.assert_success(self.dispatch(task))
        self.observe_in_flight(task)
        sha = self.commit_file("work.txt", "one\ntwo\n")
        self.append_usage("worker", input_tokens=15, cache_read=40, cache_creation=5)
        self.record_commit("worker", sha)
        self.assert_success(self.cli("accept", str(task)))

        result = self.assert_success(self.cli("calibrate", str(task)))

        self.assertIn("predicted 1 files/20 lines", result.stdout)
        self.assertIn("actual 1 files/2 lines", result.stdout)
        self.assertIn("context 60 (session 60, base 0)", result.stdout)

    def test_lead_context_is_read_from_a_different_project_directory(self):
        harness = self.sandbox / "harness"
        harness.mkdir()
        session_id = "lead-outside-run"
        self.update_zero_cost_dummy(
            agents={"lead": {
                "session_id": session_id,
                "status": "idle",
                "run_dir": str(harness),
            }},
            panes={},
            sequence=0,
            drop_prompts=0,
        )
        log = self.logs_dir_for(harness) / f"{session_id}.jsonl"
        log.parent.mkdir(parents=True, exist_ok=True)
        log.write_text(json.dumps({
            "type": "assistant",
            "message": {"usage": {
                "input_tokens": 20,
                "cache_read_input_tokens": 100,
                "cache_creation_input_tokens": 3,
            }},
        }) + "\n")

        daemon = self.start_daemon(session_id=session_id)
        self.wait_for_state("context     123")
        context = self.assert_success(self.cli("context", "lead"))
        self.assert_success(self.cli("stop"))
        daemon.wait(timeout=10)

        self.assertEqual(context.stdout, "lead\t123\n")

    def test_missing_lead_log_is_not_reported_as_zero_context(self):
        daemon = self.start_daemon()
        state = self.wait_for_state("context UNAVAILABLE")
        context = self.assert_success(self.cli("context", "lead"))
        self.assert_success(self.cli("stop"))
        daemon.wait(timeout=10)

        self.assertIn("lead stop threshold disabled", state.stdout)
        self.assertEqual(
            context.stdout,
            "lead\tUNAVAILABLE (session log not found)\n",
        )

    def test_daemon_observes_a_commit_marker_and_marks_task_committed(self):
        task, sha = self.prepare_committed_task()

        daemon = self.start_daemon()
        state = self.wait_for_state("1 committed_unverified")
        self.assert_success(self.cli("stop"))
        daemon.wait(timeout=10)

        self.assertIn(sha[:10], state.stdout)

    def test_daemon_wakes_the_commentator_once_for_a_pending_commit(self):
        task, sha = self.prepare_committed_task()
        commentator = self.start_commentator()
        wake = (
            f"supervisor: commit {sha[:10]} landed for task {task}; review it from git"
        )

        daemon = self.start_daemon()
        state = self.wait_for_state("commentary-wake")
        time.sleep(0.1)

        self.assertIn(f"commentary-wake task {task} {sha[:10]}", state.stdout)
        self.assertNotIn("commentary-delivered@", state.stdout)
        self.assertEqual(self.prompts_to(commentator).count(wake), 1)

        self.append_text(commentator, f"Reviewed commit {sha[:7]}")
        self.wait_for_state("commentary-delivered@")
        time.sleep(0.1)
        self.assertEqual(self.prompts_to(commentator).count(wake), 1)

        self.assert_success(self.cli("stop"))
        daemon.wait(timeout=10)

    def test_daemon_counts_a_queued_commentator_wake_as_sent_once(self):
        task, sha = self.prepare_committed_task()
        commentator = self.start_commentator()
        self.set_agent_status(commentator, "busy")
        wake = (
            f"supervisor: commit {sha[:10]} landed for task {task}; review it from git"
        )

        daemon = self.start_daemon()
        state = self.wait_for_state("commentary-wake")
        time.sleep(0.1)

        entries = [
            json.loads(line)
            for line in self.session_log(commentator).read_text().splitlines()
        ]
        queued = [
            entry for entry in entries
            if entry.get("type") == "queue-operation"
            and entry.get("content") == wake
        ]
        self.assertIn(f"commentary-wake task {task} {sha[:10]}", state.stdout)
        self.assertNotIn("commentary-delivered@", state.stdout)
        self.assertEqual(len(queued), 1)
        self.assertEqual(self.prompts_to(commentator).count(wake), 1)

        self.assert_success(self.cli("stop"))
        daemon.wait(timeout=10)

    def test_daemon_observes_commentator_ingestion(self):
        task, sha = self.prepare_committed_task()
        self.assert_success(self.cli("accept", str(task)))
        commentator = self.start_commentator()
        self.append_text(commentator, f"Reviewed commit {sha[:10]}")

        daemon = self.start_daemon()
        state = self.wait_for_state("commentary-delivered@")
        wakes = [
            text for text in self.prompts_to(commentator)
            if text.startswith("supervisor: commit ")
        ]
        self.assert_success(self.cli("stop"))
        daemon.wait(timeout=10)

        self.assertIn("1 accepted", state.stdout)
        self.assertEqual(wakes, [])

    def test_stop_is_durable_and_ends_a_running_daemon(self):
        daemon = self.start_daemon()
        self.wait_for_state("lead             lead")

        result = self.assert_success(self.cli("stop"))
        daemon.wait(timeout=10)

        self.assertIn("the daemon will exit", result.stdout)
        self.assertEqual(daemon.returncode, 0)

    def test_human_wait_open_and_close_are_visible_in_state(self):
        self.assert_success(self.cli("human-wait", "start"))

        open_state = self.assert_success(self.cli("state"))
        self.assert_success(self.cli("human-wait", "end"))
        closed_state = self.assert_success(self.cli("state"))

        self.assertIn("a human wait is open", open_state.stdout)
        self.assertNotIn("a human wait is open", closed_state.stdout)


class BusySessionContractTests(SupervisorContractCase):
    """A busy agent queues a prompt and works through it once it goes idle."""

    def test_a_prompt_is_withheld_while_busy_and_lands_when_the_session_goes_idle(self):
        (self.run_dir / "chainsaw.json").write_text(
            '{"prompt-landing-seconds": 1}\n'
        )
        self.launch()
        self.set_agent_status("worker", "busy")
        log = self.session_log("worker")
        landed_while_busy = []

        def release():
            entries = [json.loads(line) for line in log.read_text().splitlines()]
            landed_while_busy.append(any(
                entry.get("type") == "user" for entry in entries
            ))
            self.set_agent_status("worker", "idle")

        timer = threading.Timer(1.3, release)
        timer.start()
        self.addCleanup(timer.cancel)

        self.assert_success(self.cli(
            "prompt", "worker", "queued while busy", "--wait"
        ))
        timer.join(timeout=5)

        entries = [json.loads(line) for line in log.read_text().splitlines()]
        enqueues = [
            entry for entry in entries
            if entry.get("type") == "queue-operation"
            and entry.get("operation") == "enqueue"
            and entry.get("content") == "queued while busy"
        ]
        users = [
            entry for entry in entries
            if entry.get("type") == "user"
            and entry.get("message", {}).get("content") == "queued while busy"
        ]

        self.assertEqual(
            landed_while_busy, [False],
            "a busy session must withhold the prompt, not answer it synchronously",
        )
        self.assertEqual(
            len(enqueues), 1,
            "the busy session should record exactly one queued copy",
        )
        self.assertEqual(
            len(users), 1,
            "the queued prompt should be delivered exactly once",
        )
        self.assertEqual(
            [operation["operation"] for operation in self.runtime_operations()
             if operation["operation"] == "prompt"],
            ["prompt"],
            "the supervisor waited the queue out; it should not have resent",
        )
        state = self.assert_success(self.cli("state"))
        self.assertIn("prompt-queued worker", state.stdout)


class DottedRunDirectoryContractTests(SupervisorContractCase):
    """Claude Code munges dots as well as path separators, so a dotted run directory loses its dot."""

    run_dir_name = "run.wt"

    def test_commentator_is_pointed_at_the_directory_claude_code_actually_writes(self):
        self.assert_success(self.cli(
            "start-commentator", "--role-prompt", str(self.run_dir / "commentator.md"),
        ))

        prompt = next(
            operation["text"] for operation in self.runtime_operations()
            if operation["operation"] == "prompt"
            and operation["session_id"].startswith("commentator-")
        )
        prefix = "Session-log directory: "
        announced = next(
            line.removeprefix(prefix)
            for line in prompt.splitlines() if line.startswith(prefix)
        )

        self.assertTrue(
            announced.endswith("-run-wt"),
            f"the run directory's dot was not munged: {announced}",
        )
        self.assertTrue(Path(announced).is_dir(), announced)

    def test_supervisor_state_is_stored_beside_the_transcripts(self):
        self.assert_success(self.cli("launch", "worker"))

        self.assertTrue(
            (self.logs_dir / "chainsaw-supervisor.db").is_file(),
            f"no database under {self.logs_dir}",
        )


if __name__ == "__main__":
    import unittest

    unittest.main()


class WatchTranscriptsContractTests(SupervisorContractCase):
    """`watch-transcripts` prints one line per interval naming transcripts that grew."""

    def test_reports_growth_of_existing_and_new_transcripts_once_per_interval(self):
        self.logs_dir.mkdir(parents=True, exist_ok=True)
        existing = self.logs_dir / "impl-1.jsonl"
        existing.write_text("{}\n")
        command = [*self.supervisor_command, "--run-dir", str(self.run_dir),
                   "watch-transcripts", "--interval-ms", "200"]
        process = subprocess.Popen(
            command, text=True, stdout=subprocess.PIPE, stderr=subprocess.PIPE, env=self.env,
        )
        self.addCleanup(process.wait, timeout=5)
        self.addCleanup(process.kill)

        time.sleep(0.1)
        with existing.open("a") as transcript:
            transcript.write("{\"more\":1}\n")
        (self.logs_dir / "impl-2.jsonl").write_text("{}\n")

        line = process.stdout.readline()
        self.assertEqual(line, "transcripts grew: impl-1 +11, impl-2 +3\n")

    def test_ignores_the_commentators_own_transcript(self):
        """Each wake appends to the commentator's transcript; reporting that growth
        would wake it again for no other reason than having been woken."""
        commentator = self.start_commentator()
        self.launch("worker")
        self.append_text(commentator, "reviewing")
        self.append_text("worker", "working")
        command = [*self.supervisor_command, "--run-dir", str(self.run_dir),
                   "watch-transcripts", "--interval-ms", "200"]
        process = subprocess.Popen(
            command, text=True, stdout=subprocess.PIPE, stderr=subprocess.PIPE, env=self.env,
        )
        self.addCleanup(process.wait, timeout=5)
        self.addCleanup(process.kill)

        time.sleep(0.1)
        self.append_text(commentator, "woken again")
        self.append_text("worker", "still working")

        line = process.stdout.readline()
        worker_id = self.session_log("worker").stem
        self.assertEqual(line, f"transcripts grew: {worker_id} +84\n")

    def test_stays_silent_while_nothing_grows(self):
        self.logs_dir.mkdir(parents=True, exist_ok=True)
        (self.logs_dir / "impl-1.jsonl").write_text("{}\n")
        command = [*self.supervisor_command, "--run-dir", str(self.run_dir),
                   "watch-transcripts", "--interval-ms", "50"]
        process = subprocess.Popen(
            command, text=True, stdout=subprocess.PIPE, stderr=subprocess.PIPE, env=self.env,
        )

        time.sleep(0.4)
        process.kill()
        stdout, _ = process.communicate(timeout=5)
        self.assertEqual(stdout, "")


class StandingWarningTests(SupervisorContractCase):
    """Facts the lead must act on ride on the stderr of every command it runs."""

    FIVE_MINUTES_AGO = "(strftime('%s','now') - 360) * 1000"

    def test_state_for_one_task_prints_only_its_id_and_state_name(self):
        task = self.new_task()
        self.launch()
        self.assert_success(self.dispatch(task))

        result = self.assert_success(self.cli("state", "--task", str(task)))
        missing = self.assert_failure(self.cli("state", "--task", "9"), "no task 9")

        self.assertEqual(result.stdout, f"{task} dispatched\n")
        self.assertNotIn("dispatched@", result.stdout)
        self.assertEqual(missing.stdout, "")

    def test_dispatch_names_the_line_a_monitor_should_wait_for(self):
        task = self.new_task()
        self.launch()

        result = self.assert_success(self.dispatch(task))

        self.assertEqual(
            result.stdout,
            f"task {task} dispatched to worker; watch `state --task {task}` — "
            f"it prints `{task} committed_unverified` when the commit lands\n",
        )

    def test_a_commit_left_unjudged_for_five_minutes_is_flagged(self):
        task, _ = self.prepare_committed_task()
        daemon = self.start_daemon()
        self.wait_for_state(f"{task} committed_unverified")
        fresh = self.assert_success(self.cli("state"))
        self.write_supervisor_db(
            f"update task_events set created_at={self.FIVE_MINUTES_AGO} "
            "where state='committed_unverified'",
        )
        stale = self.assert_success(self.cli("state"))
        self.assert_success(self.cli("accept", str(task)))
        judged = self.assert_success(self.cli("state"))
        self.assert_success(self.cli("stop"))
        daemon.wait(timeout=10)

        self.assertNotIn("committed_unverified for", fresh.stderr)
        self.assertIn(
            f"WARNING: task {task} committed_unverified for 6m, not accepted or aborted",
            stale.stderr,
        )
        self.assertNotIn("committed_unverified for", judged.stderr)

    def test_silence_on_state_while_a_task_is_out_asks_about_the_monitor(self):
        task = self.new_task()
        self.launch()
        never = self.assert_success(self.dispatch(task))
        self.assert_success(self.cli("state"))
        watched = self.assert_success(self.cli("observe", "watching"))
        self.write_supervisor_db(
            f"update config set value={self.FIVE_MINUTES_AGO} where key='last-state-read'",
        )
        silent = self.assert_success(self.cli("observe", "still here"))
        self.assert_success(self.cli("abort", str(task), "--reason", "fixture"))
        nothing_out = self.assert_success(self.cli("observe", "nothing out"))

        self.assertIn(
            f"WARNING: state has never been read while task {task} is out: "
            f"is `poll --wait` or a monitor on `state --task {task}` armed?",
            never.stderr,
        )
        self.assertNotIn("state read", watched.stderr)
        self.assertIn(
            f"WARNING: no state read for 6m while task {task} is out: "
            f"is `poll --wait` or a monitor on `state --task {task}` armed?",
            silent.stderr,
        )
        self.assertNotIn("state read", nothing_out.stderr)

    def test_an_absent_daemon_is_flagged_until_one_polls_again(self):
        never = self.assert_success(self.cli("state"))
        daemon = self.start_daemon()
        self.wait_for_state("context UNAVAILABLE")
        running = self.assert_success(self.cli("state"))
        self.assert_success(self.cli("stop"))
        daemon.wait(timeout=10)
        self.write_supervisor_db(
            f"update config set value={self.FIVE_MINUTES_AGO} where key='daemon-seen'",
        )
        stopped = self.assert_success(self.cli("state"))
        restarted = self.start_daemon()
        self.wait_for_state("context UNAVAILABLE")
        again = self.assert_success(self.cli("state"))
        self.assert_success(self.cli("stop"))
        restarted.wait(timeout=10)

        suffix = ": nothing observes sessions until `daemon` is started"
        self.assertIn(f"WARNING: no daemon has run for this run{suffix}", never.stderr)
        self.assertNotIn("daemon", running.stderr)
        self.assertIn(f"WARNING: no daemon has polled for 6m{suffix}", stopped.stderr)
        self.assertNotIn("daemon", again.stderr)

    def test_lead_context_is_announced_near_and_past_the_stop_threshold(self):
        self.write_lead_log(210_000)
        daemon = self.start_daemon()
        self.wait_for_state("context  210000")
        near = self.assert_success(self.cli("state"))
        self.write_lead_log(260_000)
        self.wait_for_state("context  260000")
        past = self.assert_success(self.cli("state"))
        self.assert_success(self.cli("stop"))
        daemon.wait(timeout=10)
        with sqlite3.connect(self.logs_dir / "chainsaw-supervisor.db") as database:
            events = database.execute(
                "select detail from events where kind='stop-lead'",
            ).fetchall()

        self.assertIn("WARNING: lead context 210000 of 250000", near.stderr)
        self.assertNotIn("past 250000", near.stderr)
        self.assertIn(
            "WARNING: lead context 260000 is past 250000: stop the run per the "
            "skill's Stopping section",
            past.stderr,
        )
        self.assertEqual(events, [("context 260000",)])
        # The runtime was never touched: no prompt was pushed at the lead.
        self.assertFalse(self.runtime_state_path.exists())

    def test_the_poll_json_stays_clean_while_a_warning_is_printed(self):
        task = self.new_task()
        self.launch()
        self.assert_success(self.dispatch(task))

        result = self.assert_success(self.cli("poll"))

        self.assertIn("WARNING:", result.stderr)
        self.assertEqual(json.loads(result.stdout)["findings"], [])


class WaitingPollContractTests(SupervisorContractCase):
    """`poll --wait` is the one clock the lead waits on: it returns on the
    commentator's next word or on any task moving, whichever comes first."""

    def start_waiting_poll(self, *args, timeout=10):
        command = [
            *self.supervisor_command, "--run-dir", str(self.run_dir),
            "poll", "--wait", "--timeout", str(timeout), *map(str, args),
        ]
        process = subprocess.Popen(
            command, text=True, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
            env=self.env,
        )
        time.sleep(1)  # let the wait take its baseline before the trigger
        return process

    def finish(self, process):
        started = time.monotonic()
        stdout, stderr = process.communicate(timeout=30)
        self.assertEqual(process.returncode, 0, stderr)
        return json.loads(stdout), time.monotonic() - started

    def test_a_waiting_poll_returns_the_moment_a_task_moves(self):
        task = self.new_task()
        self.launch()
        waiting = self.start_waiting_poll()

        self.assert_success(self.dispatch(task))
        result, elapsed = self.finish(waiting)

        self.assertEqual(
            result["task_transitions"], [{"task_id": task, "state": "dispatched"}],
        )
        self.assertEqual(result["observations"], [])
        self.assertLess(elapsed, 5)

    def test_a_waiting_poll_returns_the_moment_the_commentator_writes(self):
        waiting = self.start_waiting_poll()

        self.assert_success(self.cli("observe", "fresh context"))
        result, elapsed = self.finish(waiting)

        self.assertEqual(
            [observation["text"] for observation in result["observations"]],
            ["fresh context"],
        )
        self.assertEqual(result["observation_cursor"], result["observations"][0]["id"])
        self.assertEqual(result["task_transitions"], [])
        self.assertLess(elapsed, 5)

    def test_a_waiting_poll_returns_a_finding_no_poll_has_printed(self):
        task = self.new_task()
        waiting = self.start_waiting_poll()

        self.assert_success(self.cli("finding", "--task", str(task), "off by one"))
        result, elapsed = self.finish(waiting)

        self.assertEqual(
            [finding["description"] for finding in result["findings"]], ["off by one"],
        )
        self.assertEqual(result["task_transitions"], [])
        self.assertLess(elapsed, 5)

    def test_a_waiting_poll_sits_through_a_finding_it_already_printed(self):
        task = self.new_task()
        self.assert_success(self.cli("finding", "--task", str(task), "off by one"))
        self.assert_success(self.cli("poll"))

        started = time.monotonic()
        result = self.assert_success(self.cli("poll", "--wait", "--timeout", "2"))
        elapsed = time.monotonic() - started

        self.assertEqual(
            [finding["description"] for finding in json.loads(result.stdout)["findings"]],
            ["off by one"],
        )
        self.assertGreaterEqual(elapsed, 1.5)

    def test_a_waiting_poll_returns_at_once_when_commentary_is_pending(self):
        self.assert_success(self.cli("observe", "already here"))

        started = time.monotonic()
        result = self.assert_success(self.cli("poll", "--wait", "--timeout", "10"))
        elapsed = time.monotonic() - started

        self.assertEqual(
            [observation["text"] for observation in json.loads(result.stdout)["observations"]],
            ["already here"],
        )
        self.assertLess(elapsed, 5)

    def test_a_waiting_poll_times_out_with_what_there_is(self):
        started = time.monotonic()
        result = self.assert_success(self.cli("poll", "--wait", "--timeout", "1"))
        elapsed = time.monotonic() - started

        self.assertEqual(
            json.loads(result.stdout),
            {
                "observation_cursor": 0, "observations": [], "findings": [],
                "task_transitions": [],
            },
        )
        self.assertGreaterEqual(elapsed, 1)

    def test_a_plain_poll_lists_no_transitions(self):
        task = self.new_task()
        self.launch()
        self.assert_success(self.dispatch(task))

        result = self.assert_success(self.cli("poll"))

        self.assertEqual(json.loads(result.stdout)["task_transitions"], [])

    def test_a_waiting_poll_counts_as_reading_state(self):
        task = self.new_task()
        self.launch()
        never = self.assert_success(self.dispatch(task))
        self.assert_success(self.cli("poll", "--wait", "--timeout", "1"))
        read = self.assert_success(self.cli("observe", "after the wait"))

        self.assertIn("state has never been read", never.stderr)
        self.assertNotIn("state", read.stderr)


class SeedAndForkContractTests(SupervisorContractCase):
    """Prototype: one prepared seed session, implementers forked from it, and
    the Git history since the seed's baseline as the dispatch preamble."""

    def launch_seed(self, name="seed-1"):
        result = self.assert_success(self.cli("launch", name, "--seed"))
        return json.loads(result.stdout)

    def launch_fork(self, name="worker", seed="seed-1"):
        result = self.assert_success(self.cli("launch", name, "--fork-of", seed))
        return json.loads(result.stdout)

    def test_seed_launch_records_its_role_and_baseline(self):
        launched = self.launch_seed()
        state = self.assert_success(self.cli("state"))

        self.assertEqual(launched["role"], "seed")
        self.assertEqual(launched["baseline"], self.head())
        self.assertIsNone(launched["fork_of"])
        self.assertIn("seed-1", state.stdout)
        self.assertIn("seed ", state.stdout)

    def test_a_seed_never_takes_a_task(self):
        task = self.new_task()
        self.launch_seed()

        result = self.dispatch(task, "seed-1")

        self.assert_failure(result, "seed-1 is the seed, not an implementer")

    def test_fork_launch_resumes_the_seed_and_inherits_its_baseline(self):
        seed = self.launch_seed()
        self.append_usage("seed-1", input_tokens=11, cache_read=1000)
        self.commit_file("moved.txt", "after the seed read\n", "feat: move after seed")

        fork = self.launch_fork()
        start = [operation for operation in self.runtime_operations()
                 if operation["operation"] == "start"][-1]

        self.assertEqual(start["session_id"], "worker")
        self.assertEqual(start["fork_of"], seed["session_id"])
        self.assertEqual(fork["fork_of"], "seed-1")
        self.assertEqual(fork["baseline"], seed["baseline"])
        self.assertNotEqual(fork["baseline"], self.head())
        self.assertEqual(
            self.session_log("worker").read_text(),
            self.session_log("seed-1").read_text(),
            "a fork starts from a copy of the seed's transcript",
        )

    def test_fork_launch_requires_a_live_seed(self):
        self.launch("cold")

        missing = self.cli("launch", "worker", "--fork-of", "seed-9")
        not_a_seed = self.cli("launch", "worker", "--fork-of", "cold")

        self.assert_failure(missing, "no live session seed-9 to fork")
        self.assert_failure(not_a_seed, "cold is the implementer, not a seed")

    def test_dispatch_to_a_fork_hands_over_git_history_and_the_silent_contract(self):
        self.launch_seed()
        self.append_usage("seed-1", input_tokens=5, cache_read=2000)
        self.commit_file(
            "moved.txt", "after the seed read\n",
            "feat: move after seed\n\nBody of the first landed task.",
        )
        task = self.new_task(text="You are task 2 of the map.")
        self.launch_fork()

        result = self.assert_success(self.dispatch(task))
        prompt = self.prompts_to("worker")[-1]

        self.assertIn("task 1 dispatched to worker (fork; estimated starting context", result.stdout)
        self.assertIn("These commits landed after the tree you read at", prompt)
        self.assertIn("feat: move after seed", prompt)
        self.assertIn("Body of the first landed task.", prompt)
        self.assertIn("+after the seed read", prompt)
        self.assertIn("You are task 2 of the map.", prompt)
        self.assertIn("Work silently.", prompt)
        self.assertIn("Your final response must contain only the commit SHA.", prompt)
        self.assertNotIn("changed-file manifest", prompt)
        self.assertNotIn("read them first", prompt)

    def test_dispatch_to_a_fork_omits_history_when_nothing_landed(self):
        self.launch_seed()
        task = self.new_task(text="First task of the map.")
        self.launch_fork()

        self.assert_success(self.dispatch(task))
        prompt = self.prompts_to("worker")[-1]

        self.assertTrue(prompt.startswith("First task of the map."), prompt[:80])
        self.assertIn("Work silently.", prompt)

    def test_dispatch_warns_when_the_fork_starts_over_budget(self):
        self.launch_seed()
        self.append_usage("seed-1", input_tokens=500, cache_read=80_000)
        task = self.new_task(text="A task on a heavy seed.")
        self.launch_fork()

        result = self.assert_success(self.dispatch(task))
        state = self.assert_success(self.cli("state"))

        self.assertIn("over the 70000 budget; prepare a replacement seed", result.stderr)
        self.assertIn(f"{task} dispatched", state.stdout)

    def test_commentator_wake_names_the_seed_of_a_forked_implementer(self):
        self.launch_seed()
        task = self.new_task(text="Forked task.")
        self.launch_fork()
        self.assert_success(self.dispatch(task))
        self.observe_in_flight(task)
        sha = self.commit_file()
        self.record_commit("worker", sha)
        commentator = self.start_commentator()

        daemon = self.start_daemon()
        self.wait_for_state("commentary-wake")
        self.assert_success(self.cli("stop"))
        daemon.wait(timeout=10)

        self.assertEqual(
            self.prompts_to(commentator)[-1],
            f"supervisor: commit {sha[:10]} landed for task {task} (implementer worker "
            "is a fork of seed seed-1; its transcript begins with a copy of the "
            "seed's); review it from git",
        )

    def test_fork_launch_warns_when_the_seed_is_heavy(self):
        self.launch_seed()
        self.append_usage("seed-1", input_tokens=200, cache_read=52_000)

        result = self.assert_success(self.cli("launch", "worker", "--fork-of", "seed-1"))

        self.assertIn(
            "WARNING: worker forks seed-1 at 52200 tokens, over the 50000 seed budget",
            result.stderr,
        )
        self.assertIn("Prepare a leaner seed from its task map", result.stderr)
        self.assertIn(("heavy-seed",), self.event_kinds())

    def test_fork_launch_is_quiet_when_the_seed_is_lean(self):
        self.launch_seed()
        self.append_usage("seed-1", input_tokens=200, cache_read=30_000)

        result = self.assert_success(self.cli("launch", "worker", "--fork-of", "seed-1"))

        self.assertNotIn("seed budget", result.stderr)
        self.assertNotIn(("heavy-seed",), self.event_kinds())

    def test_dispatch_estimates_from_the_seed_before_the_fork_has_a_transcript(self):
        self.launch_seed()
        self.append_usage("seed-1", input_tokens=500, cache_read=80_000)
        task = self.new_task(text="A task on a heavy seed.")
        self.launch_fork()
        self.session_log("worker").unlink()

        result = self.assert_success(self.dispatch(task))

        self.assertIn("estimated starting context 80", result.stdout)
        self.assertIn("over the 70000 budget; prepare a replacement seed", result.stderr)

    def test_a_fork_continues_with_the_silent_contract_and_its_own_history(self):
        self.launch_seed()
        first = self.new_task(text="First forked task.")
        self.launch_fork()
        self.assert_success(self.dispatch(first))
        self.observe_in_flight(first)
        own = self.commit_file("first.txt", "first\n", "feat: first forked task")
        self.record_commit("worker", own)
        self.assert_success(self.cli("task", "record-commit", str(first), own, "--force",
                                     "--reason", "fixture"))
        self.append_usage("worker", input_tokens=500, cache_read=75_000)
        later = self.commit_file("later.txt", "later\n", "feat: landed from elsewhere")
        second = self.new_task(text="Second forked task.", files="second.txt")

        result = self.assert_success(self.dispatch(second))
        prompt = self.prompts_to("worker")[-1]

        self.assertIn("(continuing; estimated starting context", result.stdout)
        self.assertNotIn("prepare a replacement seed", result.stderr,
                         "a continuation's size is its own, not the seed's")
        self.assertIn(f"after your last commit at {own[:10]}", prompt)
        self.assertIn("feat: landed from elsewhere", prompt)
        self.assertNotIn("feat: first forked task", prompt)
        self.assertIn("Second forked task.", prompt)
        self.assertIn("Work silently.", prompt)
        self.assertNotIn("changed-file manifest", prompt)
        self.assertEqual(later, self.head())

    def test_cold_dispatch_keeps_the_original_contract(self):
        task = self.new_task(text="Cold task.")
        self.launch()

        self.assert_success(self.dispatch(task))
        prompt = self.prompts_to("worker")[-1]

        self.assertIn("changed-file manifest", prompt)
        self.assertNotIn("Work silently.", prompt)


class PreWarmContractTests(SupervisorContractCase):
    """A spare fork is fed the commits it has not seen, one warm at a time, so
    the dispatch that finally reaches it has little left to hand over."""

    def launch_seed(self):
        self.assert_success(self.cli("launch", "seed-1", "--seed"))

    def launch_fork(self, name="spare"):
        self.assert_success(self.cli("launch", name, "--fork-of", "seed-1"))

    def test_warm_feeds_a_fork_the_commits_since_the_tree_it_read(self):
        self.launch_seed()
        self.launch_fork()
        baseline = self.head()
        self.commit_file("a.txt", "first landed\n", "feat: first landed task\n\nBody one.")

        result = self.assert_success(self.cli("warm", "spare"))
        prompt = self.prompts_to("spare")[-1]

        self.assertIn(
            f"spare warmed with 1 commit ({baseline[:10]}..{self.head()[:10]}); "
            "estimated context",
            result.stdout,
        )
        self.assertIn(f"These commits landed after the tree you read at {baseline[:10]}", prompt)
        self.assertIn("feat: first landed task", prompt)
        self.assertIn("Body one.", prompt)
        self.assertIn("+first landed", prompt)
        self.assertIn("No task yet: this is reading only", prompt)
        self.assertIn("Reply with exactly `warm`", prompt)
        self.assertIn(("warm",), self.event_kinds())

    def test_warm_picks_up_where_the_last_warm_left_off(self):
        self.launch_seed()
        self.launch_fork()
        self.commit_file("a.txt", "first\n", "feat: first landed task")
        self.assert_success(self.cli("warm", "spare"))
        shown = self.head()

        nothing = self.assert_success(self.cli("warm", "spare"))
        self.commit_file("b.txt", "second\n", "feat: second landed task")
        again = self.assert_success(self.cli("warm", "spare"))
        prompt = self.prompts_to("spare")[-1]

        self.assertEqual(
            nothing.stdout.strip(),
            f"nothing to warm: spare has seen everything up to {shown[:10]}",
        )
        self.assertEqual(len(self.prompts_to("spare")), 2)
        self.assertIn("spare warmed with 1 commit", again.stdout)
        self.assertIn(f"after the last commit you were shown at {shown[:10]}", prompt)
        self.assertIn("feat: second landed task", prompt)
        self.assertNotIn("feat: first landed task", prompt)

    def test_dispatch_to_a_warmed_fork_hands_over_only_what_is_left(self):
        self.launch_seed()
        self.launch_fork()
        self.commit_file("a.txt", "first\n", "feat: first landed task")
        self.assert_success(self.cli("warm", "spare"))
        shown = self.head()
        self.commit_file("b.txt", "second\n", "feat: second landed task")
        task = self.new_task(text="The warmed fork's first task.")

        result = self.assert_success(self.cli("dispatch", str(task), "--to", "spare"))
        prompt = self.prompts_to("spare")[-1]

        self.assertIn(f"task {task} dispatched to spare (fork;", result.stdout)
        self.assertIn(f"after the last commit you were shown at {shown[:10]}", prompt)
        self.assertIn("feat: second landed task", prompt)
        self.assertNotIn("feat: first landed task", prompt)
        self.assertIn("The warmed fork's first task.", prompt)
        self.assertIn("Work silently.", prompt)

    def test_a_dispatch_counts_as_showing_the_tree(self):
        self.launch_seed()
        self.launch_fork()
        task = self.new_task(text="A task that gets aborted.")
        self.assert_success(self.cli("dispatch", str(task), "--to", "spare"))
        shown = self.head()
        self.assert_success(self.cli("abort", str(task), "--reason", "fixture"))
        self.commit_file("c.txt", "later\n", "feat: landed after the abort")

        self.assert_success(self.cli("warm", "spare"))
        prompt = self.prompts_to("spare")[-1]

        self.assertIn(f"after the last commit you were shown at {shown[:10]}", prompt)
        self.assertIn("feat: landed after the abort", prompt)

    def test_a_commit_after_a_warm_moves_the_base_to_the_commit(self):
        self.launch_seed()
        self.launch_fork("worker")
        self.commit_file("a.txt", "first\n", "feat: first landed task")
        self.assert_success(self.cli("warm", "worker"))
        first = self.new_task(text="First forked task.")
        self.assert_success(self.dispatch(first))
        self.observe_in_flight(first)
        own = self.commit_file("own.txt", "own\n", "feat: the fork's own task")
        self.record_commit("worker", own)
        self.assert_success(self.cli("task", "record-commit", str(first), own, "--force",
                                     "--reason", "fixture"))
        self.commit_file("d.txt", "elsewhere\n", "feat: landed from elsewhere")

        self.assert_success(self.cli("warm", "worker"))
        warmed = self.prompts_to("worker")[-1]
        second = self.new_task(text="Second forked task.", files="second.txt")
        self.assert_success(self.dispatch(second))
        dispatched = self.prompts_to("worker")[-1]

        self.assertIn(f"after your last commit at {own[:10]}", warmed)
        self.assertIn("feat: landed from elsewhere", warmed)
        self.assertNotIn("the fork's own task", warmed)
        self.assertTrue(dispatched.startswith("Second forked task."), dispatched[:80])

    def test_warm_refuses_a_fork_with_a_task_out(self):
        self.launch_seed()
        self.launch_fork()
        task = self.new_task()
        self.assert_success(self.cli("dispatch", str(task), "--to", "spare"))
        self.commit_file("e.txt", "meanwhile\n", "feat: meanwhile")

        result = self.cli("warm", "spare")

        self.assert_failure(
            result, f"spare has task {task} out (dispatched); only an idle implementer is warmed",
        )
        self.assertEqual(len(self.prompts_to("spare")), 1)

    def test_warm_refuses_cold_sessions_and_seeds(self):
        self.launch_seed()
        self.launch("cold")

        cold = self.cli("warm", "cold")
        seed = self.cli("warm", "seed-1")
        missing = self.cli("warm", "nobody")

        self.assert_failure(cold, "cold is not a forked implementer; only forks are warmed")
        self.assert_failure(seed, "seed-1 is not a forked implementer")
        self.assert_failure(missing, "no session nobody; launch it first")


class TaskImportContractTests(SupervisorContractCase):
    def test_import_registers_the_task_map_in_order(self):
        task_map = json.dumps([
            {"text": "Task A: add the parser.", "files": ["src/parser.rs"],
             "predicted_lines": 40},
            {"text": "Task B: wire the CLI.", "predicted_files": 2,
             "predicted_lines": 20},
        ])

        result = self.assert_success(self.cli("task", "import", input_text=task_map))
        state = self.assert_success(self.cli("state"))

        self.assertEqual(result.stdout, "1\n2\n")
        self.assertIn("1 drafted", state.stdout)
        self.assertIn("2 drafted", state.stdout)
        with sqlite3.connect(self.logs_dir / "chainsaw-supervisor.db") as database:
            rows = database.execute(
                "select id, text, predicted_files, predicted_lines, predicted_file_list "
                "from tasks order by id"
            ).fetchall()
        self.assertEqual(rows, [
            (1, "Task A: add the parser.", 1, 40, "src/parser.rs"),
            (2, "Task B: wire the CLI.", 2, 20, None),
        ])

    def test_import_rejects_an_incomplete_entry_and_registers_nothing(self):
        task_map = json.dumps([
            {"text": "Fine.", "files": ["a.rs"], "predicted_lines": 5},
            {"text": "No size.", "files": ["b.rs"]},
        ])

        result = self.cli("task", "import", input_text=task_map)
        state = self.assert_success(self.cli("state"))

        self.assert_failure(result, "task map entry 2 has no predicted_lines")
        self.assertNotIn("1 drafted", state.stdout)

    def test_import_rejects_an_empty_or_malformed_map(self):
        empty = self.cli("task", "import", input_text="[]")
        malformed = self.cli("task", "import", input_text="not json")

        self.assert_failure(empty, "task map on stdin is empty")
        self.assert_failure(malformed, "task map on stdin is not a JSON array")
