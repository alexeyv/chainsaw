import re
import unittest

from tests.support import CRATE, SKILL


def commands(text):
    """The code spans of a prompt and every line of its fenced blocks, except
    blocks fenced as `text prompt`, which quote text the agent sends, each with
    its `<...>` placeholders filled in."""
    blocks = re.findall(r"```([^\n]*)\n(.*?)```", text, re.S)
    prose = re.sub(r"```.*?```", "", text, flags=re.S)
    found = re.findall(r"`([^`]+)`", prose)
    found += [
        line.strip() for info, block in blocks if info.split()[1:] != ["prompt"]
        for line in block.splitlines() if line.strip()
    ]
    return [re.sub(r"<[^<>]+>", "x", command) for command in found]


class ReviewPromptContractTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.lead_path = SKILL / "SKILL.md"
        cls.commentator_path = SKILL / "references" / "commentator.md"
        cls.lead = cls.lead_path.read_text(encoding="utf-8")
        cls.commentator = cls.commentator_path.read_text(encoding="utf-8")

    def test_lead_retains_cursor_and_unresolved_findings(self):
        self.assertIn("chainsaw poll --after-observation <OBSERVATION_CURSOR>", self.lead)
        self.assertIn("replace `OBSERVATION_CURSOR` with the returned cursor exactly", self.lead)
        self.assertIn("returned again on every poll while unresolved", self.lead)
        self.assertIn("until its resolution command\nsucceeds", self.lead)
        self.assertIn("through `/compact`,\ncontinuation prompts, and handoffs", self.lead)

    def test_lead_describes_dropped_and_fix_task_resolutions(self):
        self.assertIn("chainsaw resolve 17 --verdict dropped --reason", self.lead)
        self.assertIn("create the fix task first", self.lead)
        self.assertIn("chainsaw resolve 17 --verdict task --fix-task", self.lead)
        self.assertIn("concrete verdict reason", self.lead)

    def test_commentator_uses_observations_and_stable_findings(self):
        self.assertIn('chainsaw observe --task 12 "Read commit', self.commentator)
        self.assertIn('chainsaw observe "Run-wide convention', self.commentator)
        self.assertIn("chainsaw finding --task 12", self.commentator)
        self.assertIn("printed number is the finding's stable run-wide identity", self.commentator)
        self.assertIn("Observations are chronological narration only", self.commentator)
        self.assertIn("A finding is unresolved work requiring a verdict", self.commentator)

    def test_resolutions_are_reconciled_run_wide_by_finding_number(self):
        self.assertIn("chainsaw resolutions", self.commentator)
        self.assertIn("Reconcile\n  entries by `finding_id`", self.commentator)
        self.assertIn("visible to every\n  commentator", self.commentator)
        self.assertIn("no commentator identity or session scope", self.commentator)

    def test_lead_is_told_not_to_assert_unverified_facts(self):
        self.assertIn(
            "Don't state facts about the code you haven't verified. Say what the implementer\n"
            "   needs to find out, not what you assume the answer is.",
            self.lead,
        )

    def test_implementer_contract_runs_the_gate_once_in_both_copies(self):
        tasks = (CRATE / "src" / "coordinator" / "tasks.rs").read_text()
        phrase = "run the project's quality gate once, immediately before"
        self.assertIn(phrase, self.lead.replace("\n   ", " "))
        self.assertIn(phrase, tasks)
        self.assertIn("gate failures you judged pre-existing", tasks)
        self.assertIn("known pre-existing gate failures", self.lead)

    def test_prompts_use_no_syntax_only_one_shell_understands(self):
        """The lead and commentator run in bash on POSIX and in PowerShell on
        Windows, so every command they are told to run must mean the same in both.
        Commands live in code spans and fenced blocks; `<...>` is a placeholder the
        agent fills in, and double-quoted free text is an argument."""
        anywhere = {
            "a shell variable or substitution": r"\$",
            "realpath": r"\brealpath\b",
            "wc": r"\bwc\b",
            "a test command": r"^test |\[\[? ",
            "a backslash": r"\\",
            "a line continuation": r"`\s*$",
            "a curly quote": r"[‘’“”]",
        }
        outside_quotes = {
            "an ampersand (background, chain or call operator)": r"&",
            "a redirection": r"[<>]",
            "a pipe or command separator": r"[|;]",
        }
        for path, text in ((self.lead_path, self.lead), (self.commentator_path, self.commentator)):
            self.assertNotRegex(text, r"```(?!text\b)\w", f"{path.name} has a shell code block")
            for command in commands(text):
                for idiom, pattern in anywhere.items():
                    self.assertNotRegex(command, pattern, f"{path.name} uses {idiom}: {command}")
                bare = re.sub(r'"[^"]*"', '""', command)
                for idiom, pattern in outside_quotes.items():
                    self.assertNotRegex(bare, pattern, f"{path.name} uses {idiom}: {command}")

    def test_commands_are_every_line_of_a_command_block(self):
        text = "```text\n<skill>/chainsaw --run-dir <run> state\n\necho $HOME\n```\n"

        self.assertEqual(commands(text), ["x/chainsaw --run-dir x state", "echo $HOME"])

    def test_commands_leave_out_a_block_that_quotes_a_prompt(self):
        text = "   ```text prompt\n   Run the gate; stop if dirty.\n   ```\n"

        self.assertEqual(commands(text), [])

    def test_both_prompts_say_how_to_run_a_path_with_a_space(self):
        rule = (
            "A path with a space must be quoted, and PowerShell then needs an ampersand "
            "and a space before a command that starts with it"
        )
        for path, text in ((self.lead_path, self.lead), (self.commentator_path, self.commentator)):
            self.assertIn(rule, " ".join(text.split()), path.name)

    def test_prompts_and_coordinator_do_not_name_legacy_review_files(self):
        legacy_names = (
            "chainsaw-" + "comments.md",
            "chainsaw-" + "dispositions.md",
        )
        coordinator_sources = [CRATE / "src" / "coordinator.rs"] + sorted(
            (CRATE / "src" / "coordinator").glob("*.rs")
        )
        sources = [(self.lead_path, self.lead), (self.commentator_path, self.commentator)]
        sources += [(path, path.read_text()) for path in coordinator_sources]
        for path, text in sources:
            for legacy_name in legacy_names:
                self.assertNotIn(legacy_name, text, str(path))


if __name__ == "__main__":
    unittest.main()
