import unittest

from tests.support import PROJECT_ROOT


class ReviewPromptContractTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.lead_path = PROJECT_ROOT / "skills" / "chainsaw-lead" / "SKILL.md"
        cls.commentator_path = (
            PROJECT_ROOT / "skills" / "chainsaw-lead" / "references" /
            "commentator.md"
        )
        cls.lead = cls.lead_path.read_text()
        cls.commentator = cls.commentator_path.read_text()

    def test_lead_retains_cursor_and_unresolved_findings(self):
        self.assertIn('$SUP poll --after-observation "$OBSERVATION_CURSOR"', self.lead)
        self.assertIn("replace `OBSERVATION_CURSOR` with the returned cursor exactly", self.lead)
        self.assertIn("returned again on every poll while unresolved", self.lead)
        self.assertIn("until its resolution command\nsucceeds", self.lead)
        self.assertIn("through `/compact`,\ncontinuation prompts, and handoffs", self.lead)

    def test_lead_describes_dropped_and_fix_task_resolutions(self):
        self.assertIn("$SUP resolve 17 --verdict dropped --reason", self.lead)
        self.assertIn("create the fix task first", self.lead)
        self.assertIn("$SUP resolve 17 --verdict task --fix-task", self.lead)
        self.assertIn("concrete verdict reason", self.lead)

    def test_commentator_uses_observations_and_stable_findings(self):
        self.assertIn('$SUP observe --task 12 "Read commit', self.commentator)
        self.assertIn('$SUP observe "Run-wide convention', self.commentator)
        self.assertIn("$SUP finding --task 12", self.commentator)
        self.assertIn("printed number is the finding's stable run-wide identity", self.commentator)
        self.assertIn("Observations are chronological narration only", self.commentator)
        self.assertIn("A finding is unresolved work requiring a verdict", self.commentator)

    def test_resolutions_are_reconciled_run_wide_by_finding_number(self):
        self.assertIn("$SUP resolutions", self.commentator)
        self.assertIn("Reconcile\n  entries by `finding_id`", self.commentator)
        self.assertIn("visible to every\n  commentator", self.commentator)
        self.assertIn("no commentator identity or session scope", self.commentator)

    def test_prompts_and_coordinator_do_not_name_legacy_review_files(self):
        legacy_names = (
            "chainsaw-" + "comments.md",
            "chainsaw-" + "dispositions.md",
        )
        coordinator = (PROJECT_ROOT / "src" / "coordinator.rs").read_text()
        for path, text in (
            (self.lead_path, self.lead),
            (self.commentator_path, self.commentator),
            (PROJECT_ROOT / "src" / "coordinator.rs", coordinator),
        ):
            for legacy_name in legacy_names:
                self.assertNotIn(legacy_name, text, str(path))


if __name__ == "__main__":
    unittest.main()
