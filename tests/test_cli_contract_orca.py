"""The contract again, with the run living in Orca.

Every class of `test_cli_contract` runs once more with `tests/fake_orca.py` on
PATH as `orca` and the environment an Orca terminal gives the lead. Herdr knows
sessions by name and is asked `agent get` and `agent wait`; Orca knows terminals
by handle and is asked `terminal wait` for both, so the cases that spell out the
runtime conversation are restated here in Orca's terms. What only Orca does,
such as learning a session id it was never told, is proven at the end.
"""

import json
import sqlite3

import tests.test_cli_contract as herdr
from tests.support import SupervisorContractCase

#: How long the supervisor gives a terminal to prove itself idle in one probe.
PROBE_MS = 250
#: How long `prompt --wait` waits the turn out by default.
WAIT_TIMEOUT_MS = 300_000


class TaskContractTests(herdr.TaskContractTests):
    runtime = "orca"


class PromptAndDispatchContractTests(herdr.PromptAndDispatchContractTests):
    runtime = "orca"

    def test_prompt_wait_prints_the_agent_reply(self):
        self.launch()
        self.update_runtime_state(reply_on_prompt="fixture reply")

        result = self.assert_success(
            self.cli("prompt", "worker", "hello agent", "--wait")
        )

        self.assertEqual(result.stdout, "fixture reply\n")
        self.assertEqual(
            [(operation["operation"], operation.get("timeout_ms"))
             for operation in self.operations_on("worker")],
            [
                ("start", None),
                ("wait", PROBE_MS),             # is it idle before the prompt
                ("prompt", None),
                ("wait", PROBE_MS),             # did the prompt land
                ("wait", PROBE_MS),             # has the turn begun
                ("wait", WAIT_TIMEOUT_MS),      # wait the turn out
            ],
        )

    def test_abort_interrupts_and_informs_a_live_implementer(self):
        task = self.new_task()
        self.launch()
        self.assert_success(self.dispatch(task))

        result = self.assert_success(
            self.cli("abort", str(task), "--reason", "the brief is wrong")
        )
        state = self.assert_success(self.cli("state"))
        operations = self.operations_on("worker")

        self.assertIn(f"task {task} aborted", result.stdout)
        self.assertEqual(
            [operation["operation"] for operation in operations[-4:]],
            ["interrupt", "wait", "prompt", "wait"],
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
            [operation["operation"] for operation in self.operations_on("worker")[-4:]],
            ["interrupt", "wait", "prompt", "wait"],
        )


class VerificationContractTests(herdr.VerificationContractTests):
    runtime = "orca"


class FreshSessionContractTests(herdr.FreshSessionContractTests):
    runtime = "orca"


class CommunicationProtocolContractTests(herdr.CommunicationProtocolContractTests):
    runtime = "orca"


class ReportingAndDaemonContractTests(herdr.ReportingAndDaemonContractTests):
    runtime = "orca"


class BusySessionContractTests(herdr.BusySessionContractTests):
    runtime = "orca"


class DottedRunDirectoryContractTests(herdr.DottedRunDirectoryContractTests):
    runtime = "orca"


class WatchTranscriptsContractTests(herdr.WatchTranscriptsContractTests):
    runtime = "orca"


class StandingWarningTests(herdr.StandingWarningTests):
    runtime = "orca"


class SettingsContractTests(herdr.SettingsContractTests):
    runtime = "orca"


class CodexImplementerContractTests(herdr.CodexImplementerContractTests):
    runtime = "orca"


class OrcaTerminalContractTests(SupervisorContractCase):
    """What the supervisor does with a runtime that knows terminals, not agents."""

    runtime = "orca"

    def external_session_id(self, name):
        with sqlite3.connect(self.transcripts_dir / "chainsaw-supervisor.db") as database:
            (external_id,) = database.execute(
                "select external_session_id from sessions where name=? and stopped_at is null",
                (name,),
            ).fetchone()
        return external_id

    def test_launch_opens_a_terminal_in_the_run_directory_titled_after_the_session(self):
        self.launch()

        (start,) = [
            operation for operation in self.operations_on("worker")
            if operation["operation"] == "start"
        ]
        self.assertEqual(start["title"], "worker")
        self.assertEqual(start["kind"], "claude")
        self.assertEqual(self.session_state("worker")["run_dir"], str(self.run_dir.resolve()))

    def test_claude_is_launched_with_the_session_id_the_supervisor_minted(self):
        self.launch()

        external_id = self.external_session_id("worker")
        self.assertRegex(
            external_id,
            r"^[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$",
        )
        self.assertEqual(self.session_state("worker")["session_id"], external_id)
        self.assertEqual(self.session_transcript("worker").stem, external_id)

    def test_codex_names_its_own_session_and_the_supervisor_reads_it_from_the_rollout(self):
        self.write_settings('[implementer]\nagent = "codex"\n')
        self.launch()

        external_id = self.external_session_id("worker")
        self.assertEqual(external_id, "session-worker-1")
        self.assertEqual(self.session_state("worker")["session_id"], external_id)

    def test_the_commentator_splits_the_leads_own_terminal(self):
        commentator = self.start_commentator()

        (start,) = [
            operation for operation in self.operations_on(commentator)
            if operation["operation"] == "start"
        ]
        self.assertEqual(start["split_from"], "term-lead")
        self.assertNotIn("title", start)

    def test_the_registry_beside_the_database_maps_names_to_terminals(self):
        self.launch("first")
        self.launch("second")
        commentator = self.start_commentator()

        registry = json.loads(
            (self.transcripts_dir / "chainsaw-orca-terminals.json").read_text()
        )
        self.assertEqual(
            {name: terminal["handle"] for name, terminal in registry.items()},
            {"first": "term-1", "second": "term-2", commentator: "term-3"},
        )
        self.assertEqual(
            registry["first"]["external_id"], self.external_session_id("first"),
        )

    def test_a_session_whose_terminal_is_gone_reads_as_absent(self):
        task = self.new_task()
        self.launch()
        self.assert_success(self.dispatch(task))
        self.forget_session("worker")

        result = self.assert_success(
            self.cli("abort", str(task), "--reason", "the terminal was closed")
        )
        state = self.assert_success(self.cli("state"))

        self.assertIn(f"task {task} aborted", result.stdout)
        self.assertIn("abort-unreachable task 1 -> worker", state.stdout)

    def test_agent_flags_survive_the_shell(self):
        self.write_settings(
            '[implementer]\nargs = \'--append-system-prompt "say \\"hi\\" & wait"\'\n'
        )
        self.launch()

        self.assertEqual(
            self.launch_args("worker"), ["--append-system-prompt", 'say "hi" & wait'],
        )


class OrcaSpacedRunDirectoryContractTests(SupervisorContractCase):
    """A run directory with a space in its name is shell-quoted into the command."""

    runtime = "orca"
    run_dir_name = "my run"

    def test_launch_lands_in_the_run_directory(self):
        self.launch()

        self.assertEqual(self.session_state("worker")["run_dir"], str(self.run_dir.resolve()))
