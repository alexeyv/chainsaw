import os
import subprocess
import unittest

from tests.support import PROJECT_ROOT


class ReleaseAssemblyTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.script = PROJECT_ROOT / "scripts" / "release.sh"
        cls.dist = PROJECT_ROOT / "dist" / "skills" / "chainsaw-lead"
        subprocess.run([str(cls.script)], cwd=PROJECT_ROOT, check=True)

    def test_assembles_skill_and_lead_prompt_files(self):
        self.assertTrue((self.dist / "SKILL.md").exists())
        self.assertTrue((self.dist / "references" / "commentator.md").exists())

    def test_assembles_supervisor_source(self):
        supervisor = self.dist / "supervisor"
        self.assertTrue((supervisor / "Cargo.toml").exists())
        self.assertTrue((supervisor / "Cargo.lock").exists())
        self.assertTrue((supervisor / "rust-toolchain.toml").exists())
        self.assertTrue((supervisor / "src" / "main.rs").exists())

    def test_leaves_test_sources_out_of_the_supervisor(self):
        src = self.dist / "supervisor" / "src"
        shipped = sorted(
            str(path.relative_to(src))
            for path in src.rglob("*.rs")
            if path.name == "tests.rs" or path.name.startswith("test_")
        )
        self.assertEqual(shipped, [])

    def test_wrapper_is_present_and_executable(self):
        wrapper = self.dist / "bin" / "chainsaw"
        self.assertTrue(wrapper.exists())
        self.assertTrue(os.access(wrapper, os.X_OK))


if __name__ == "__main__":
    unittest.main()
