import os
import subprocess
import tempfile
import unittest
from pathlib import Path

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
    """`scripts/release.sh` assembles the shipped supervisor; the checked-in copy
    must equal what it assembles. The tests build into a scratch directory and
    never write into the repository."""

    @classmethod
    def setUpClass(cls):
        cls.script = PROJECT_ROOT / "scripts" / "release.sh"
        cls.skill = PROJECT_ROOT / "skills" / "chainsaw-lead"
        cls.scratch = tempfile.TemporaryDirectory(prefix="chainsaw-release-")
        cls.assembled = Path(cls.scratch.name) / "supervisor"
        subprocess.run(
            [str(cls.script), str(cls.assembled)], cwd=PROJECT_ROOT, check=True,
            capture_output=True,
        )

    @classmethod
    def tearDownClass(cls):
        cls.scratch.cleanup()

    def test_assembles_skill_and_lead_prompt_files(self):
        self.assertTrue((self.skill / "SKILL.md").exists())
        self.assertTrue((self.skill / "references" / "commentator.md").exists())

    def test_assembles_supervisor_source(self):
        self.assertTrue((self.assembled / "Cargo.toml").exists())
        self.assertTrue((self.assembled / "Cargo.lock").exists())
        self.assertTrue((self.assembled / "rust-toolchain.toml").exists())
        self.assertTrue((self.assembled / "src" / "main.rs").exists())

    def test_checked_in_supervisor_matches_the_assembled_one(self):
        """Drift between src/ and the shipped copy fails here; fix by running the script."""
        shipped = self.skill / "supervisor"
        shipped_files = {p.relative_to(shipped) for p in shipped.rglob("*") if p.is_file()}
        assembled_files = {
            p.relative_to(self.assembled) for p in self.assembled.rglob("*") if p.is_file()
        }
        self.assertEqual(
            shipped_files, assembled_files,
            "shipped supervisor is out of date; run scripts/release.sh",
        )
        for relative in sorted(assembled_files):
            self.assertEqual(
                (shipped / relative).read_bytes(), (self.assembled / relative).read_bytes(),
                f"{relative} is out of date; run scripts/release.sh",
            )

    def test_supervisor_sources_match_the_crate_without_tests(self):
        expected_src = PROJECT_ROOT / "src"
        shipped_src = self.assembled / "src"
        expected_files = {
            path.relative_to(expected_src)
            for path in expected_src.rglob("*.rs")
            if path.name != "tests.rs" and not path.name.startswith("test_")
        }
        shipped_files = {
            path.relative_to(shipped_src) for path in shipped_src.rglob("*.rs")
        }
        self.assertEqual(shipped_files, expected_files)

    def test_leaves_test_sources_out_of_the_supervisor(self):
        src = self.assembled / "src"
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
