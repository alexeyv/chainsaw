import os
import subprocess
import unittest
from pathlib import Path

from tests.support import CRATE, PROJECT_ROOT, SKILL


def _is_compiled_binary(path):
    header = path.read_bytes()[:4]
    return header in {
        b"\x7fELF",
        b"\xcf\xfa\xed\xfe",
        b"\xfe\xed\xfa\xce",
        b"\xfe\xed\xfa\xcf",
        b"\xce\xfa\xed\xfe",
    } or header.startswith(b"MZ")


class SkillTests(unittest.TestCase):
    """The skill folder is what `npx skills add` installs, so it carries the
    prompts, the wrapper, and the supervisor crate the wrapper builds."""

    def test_carries_the_lead_and_commentator_prompts(self):
        self.assertTrue((SKILL / "SKILL.md").exists())
        self.assertTrue((SKILL / "references" / "commentator.md").exists())

    def test_carries_the_supervisor_crate(self):
        for name in ("Cargo.toml", "Cargo.lock", "rust-toolchain.toml"):
            self.assertTrue((CRATE / name).exists(), name)
        self.assertTrue((CRATE / "src" / "main.rs").exists())

    def test_wrapper_is_present_and_executable(self):
        wrapper = SKILL / "bin" / "chainsaw"
        self.assertTrue(os.access(wrapper, os.X_OK))
        self.assertTrue(wrapper.read_bytes().startswith(b"#!"))

    def test_tracks_no_compiled_binaries(self):
        tracked = subprocess.run(
            ["git", "ls-files", "-z", str(SKILL)],
            cwd=PROJECT_ROOT, check=True, capture_output=True, text=True,
        ).stdout.split("\0")
        binaries = sorted(
            name for name in tracked
            if name and _is_compiled_binary(PROJECT_ROOT / name)
        )
        self.assertEqual(binaries, [])


if __name__ == "__main__":
    unittest.main()
