"""Behavioral contract for the supervisor's public CLI.

Nothing in this module imports the coordinator implementation. It drives the CLI and
reads SQLite only for durable details the CLI does not expose, so the suite can target
a replacement executable by setting CHAINSAW_SUPERVISOR_COMMAND.
"""

import json
import sqlite3
import threading
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
            "launch", "worker-two", "--fresh", "--reason", "parallel fixture",
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
            "launch", "replacement", "--fresh", "--reason", "independent fixture",
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

    def test_accept_rejects_a_commit_that_is_not_head(self):
        task, task_sha = self.prepare_committed_task()
        self.commit_file("later.txt", "later\n", "feat: later fixture commit")
        self.assertNotEqual(task_sha, self.head())

        result = self.cli("accept", str(task))

        self.assert_failure(result, "commit is not HEAD")

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


class ReuseContractTests(SupervisorContractCase):
    def verified_first_task(self):
        task, _ = self.prepare_committed_task()
        self.assert_success(self.cli("accept", str(task)))
        return task

    def test_idle_current_session_can_be_reused(self):
        self.verified_first_task()
        second = self.new_task(text="Second task.", files="second.txt")

        result = self.assert_success(self.dispatch(second, reuse=True))
        state = self.assert_success(self.cli("state"))

        self.assertIn("task 2 dispatched to worker (reuse)", result.stdout)
        self.assertIn("2 dispatched", state.stdout)
        self.assertIn("reuse (awaiting context base)", state.stdout)

    def test_reuse_flag_is_rejected_for_a_session_with_no_prior_task(self):
        task = self.new_task()
        self.launch()

        result = self.dispatch(task, reuse=True)

        self.assert_failure(result, "has never taken a task")

    def test_reusing_a_prior_session_requires_the_explicit_flag(self):
        self.verified_first_task()
        second = self.new_task(text="Second task.", files="second.txt")

        result = self.dispatch(second)

        self.assert_failure(result, "dispatching to it again is a reuse")

    def test_aborted_session_cannot_be_reused(self):
        first = self.new_task()
        self.launch()
        self.assert_success(self.dispatch(first))
        self.assert_success(self.cli("abort", str(first), "--reason", "implementation failed"))
        second = self.new_task(text="Retry elsewhere.", files="retry.txt")

        result = self.dispatch(second, reuse=True)

        self.assert_failure(result, "its last task (1) aborted")

    def test_repeated_implementer_name_creates_a_distinct_session(self):
        first = self.new_task()
        self.launch()
        self.assert_success(self.dispatch(first))
        self.assert_success(self.cli("abort", str(first), "--reason", "first session failed"))

        second = self.new_task(text="Try in a new session.", files="second.txt")
        self.assert_success(self.launch())
        dispatched = self.assert_success(self.dispatch(second))

        self.assertIn("task 2 dispatched to worker", dispatched.stdout)

    def test_session_over_configured_context_limit_cannot_be_reused(self):
        self.verified_first_task()
        (self.run_dir / "chainsaw.json").write_text('{"reuse-max-context": -1}\n')
        second = self.new_task(text="Second task.", files="second.txt")

        result = self.dispatch(second, reuse=True)

        self.assert_failure(result, "is over reuse-max-context -1")

    def test_reuse_refuses_an_unreadable_settings_file(self):
        self.verified_first_task()
        (self.run_dir / "chainsaw.json").write_text('{"reuse-max-contxt": 1}\n')
        second = self.new_task(text="Second task.", files="second.txt")

        result = self.dispatch(second, reuse=True)

        self.assert_failure(result, "invalid settings in")
        self.assert_failure(result, 'unknown setting "reuse-max-contxt"')

    def test_session_with_a_materially_stale_tree_cannot_be_reused(self):
        self.verified_first_task()
        changed = "".join(f"line {number}\n" for number in range(250))
        self.commit_file("large-change.txt", changed, "feat: large intervening change")
        second = self.new_task(text="Second task.", files="second.txt")

        result = self.dispatch(second, reuse=True)

        self.assert_failure(result, "over reuse-max-stale-lines 200")

    def test_launch_refuses_when_an_idle_session_is_reusable(self):
        self.verified_first_task()

        result = self.cli("launch", "replacement")

        self.assert_failure(result, "an idle implementer can take the next task")
        self.assertIn("dispatch <task-id> --to worker --reuse", result.stderr)

    def test_fresh_launch_requires_and_records_a_reason(self):
        self.verified_first_task()

        missing_reason = self.cli("launch", "replacement", "--fresh")
        launched = self.cli(
            "launch", "replacement", "--fresh", "--reason", "needs independent context",
        )
        state = self.assert_success(self.cli("state"))

        self.assert_failure(missing_reason, "--fresh requires a non-empty --reason")
        self.assert_success(launched)
        self.assertIn("launch-fresh", state.stdout)
        self.assertIn("needs independent context", state.stdout)

    def test_a_committed_predecessor_releases_the_next_dispatch(self):
        self.prepare_committed_task()
        daemon = self.start_daemon()
        self.wait_for_state("1 committed_unverified")
        self.assert_success(self.cli("stop"))
        daemon.wait(timeout=10)

        second = self.new_task(text="Immediate successor.", files="second.txt")
        self.assert_success(self.cli(
            "launch", "replacement", "--fresh", "--reason", "predecessor fixture",
        ))

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
