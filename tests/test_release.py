import os
import subprocess
import unittest

from tests.support import PROJECT_ROOT


def _is_compiled_binary(path):
    header = path.read_bytes()[:4]
    return header in {
        b"\x7fELF",
        b"\xcf\xfa\xed\xfe",
        b"\xfe\xed\xfa\xce",
        b"\xfe\xed\xfa\xcf",
        b"\xce\xfa\xed\xfe",
    } or header.startswith(b"MZ")


class ReleaseAssemblyTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.script = PROJECT_ROOT / "scripts" / "release.sh"
        cls.skill = PROJECT_ROOT / "skills" / "chainsaw-lead"
        subprocess.run([str(cls.script)], cwd=PROJECT_ROOT, check=True)

    def test_assembles_skill_and_lead_prompt_files(self):
        self.assertTrue((self.skill / "SKILL.md").exists())
        self.assertTrue((self.skill / "references" / "commentator.md").exists())

    def test_assembles_supervisor_source(self):
        supervisor = self.skill / "supervisor"
        self.assertTrue((supervisor / "Cargo.toml").exists())
        self.assertTrue((supervisor / "Cargo.lock").exists())
        self.assertTrue((supervisor / "rust-toolchain.toml").exists())
        self.assertTrue((supervisor / "src" / "main.rs").exists())

    def test_supervisor_sources_match_the_crate_without_tests(self):
        expected_src = PROJECT_ROOT / "src"
        shipped_src = self.skill / "supervisor" / "src"
        expected_files = {
            path.relative_to(expected_src)
            for path in expected_src.rglob("*.rs")
            if path.name != "tests.rs" and not path.name.startswith("test_")
        }
        shipped_files = {
            path.relative_to(shipped_src) for path in shipped_src.rglob("*.rs")
        }
        self.assertEqual(shipped_files, expected_files)
        for relative in expected_files:
            self.assertEqual(
                (expected_src / relative).read_text(),
                (shipped_src / relative).read_text(),
            )

    def test_leaves_test_sources_out_of_the_supervisor(self):
        src = self.skill / "supervisor" / "src"
        shipped = sorted(
            str(path.relative_to(src))
            for path in src.rglob("*.rs")
            if path.name == "tests.rs" or path.name.startswith("test_")
        )
        self.assertEqual(shipped, [])

    def test_wrapper_is_present_and_executable(self):
        wrapper = self.skill / "bin" / "chainsaw"
        self.assertTrue(wrapper.exists())
        self.assertTrue(os.access(wrapper, os.X_OK))
        self.assertTrue(wrapper.read_bytes().startswith(b"#!"))

    def test_ships_no_compiled_binaries(self):
        binaries = sorted(
            str(path.relative_to(self.skill))
            for path in self.skill.rglob("*")
            if path.is_file() and _is_compiled_binary(path)
        )
        self.assertEqual(binaries, [])


if __name__ == "__main__":
    unittest.main()
