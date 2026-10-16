"""Tests for tools/policy/check_seeded_state_history.py."""

from __future__ import annotations

import importlib.util
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest

sys.path.insert(0, str(Path(__file__).resolve().parent))
from git_fixture import isolated_git_environment  # noqa: E402


ROOT = Path(__file__).resolve().parents[2]
SPEC = importlib.util.spec_from_file_location(
    "check_seeded_state_history", ROOT / "tools/policy/check_seeded_state_history.py"
)
assert SPEC and SPEC.loader
checker = importlib.util.module_from_spec(SPEC)
sys.modules["check_seeded_state_history"] = checker
SPEC.loader.exec_module(checker)

CONF = "os/usr/lib/tmpfiles.d/tilo.conf"
SEED = "C  /var/lib/tilo/list.json  0644  root  root  -  /usr/share/tilo/list.json\n"
HISTORY = "os/tests/seeded-state/var/lib/tilo/list.json"


class SeededStateHistoryTests(unittest.TestCase):
    def setUp(self) -> None:
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.git("init", "-q")
        self.write(CONF, "# seeds\n" + SEED)
        self.write("os/usr/share/tilo/list.json", '{"v": 2}\n')
        self.write(f"{HISTORY}/2026-01-01.json", '["v1"]\n')
        self.write(f"{HISTORY}/2026-02-01.json", '{"v": 2}\n')
        self.write("crates/reader/src/lib.rs", f'const HISTORY: &str = "{HISTORY}";\n')

    def git(self, *args: str) -> None:
        subprocess.run(
            ["git", *args],
            cwd=self.root,
            check=True,
            capture_output=True,
            env=isolated_git_environment(),
        )

    def write(self, name: str, body: str, *, track: bool = True) -> None:
        path = self.root / name
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(body, encoding="utf-8")
        if track:
            self.git("add", name)

    def test_the_repository_keeps_every_seeded_version(self) -> None:
        self.assertEqual(checker.check(ROOT), [])

    def test_a_seed_with_its_history_and_a_reader_passes(self) -> None:
        self.assertEqual(checker.check(self.root), [])

    def test_changing_the_shipped_default_needs_its_version_in_the_history(self) -> None:
        self.write("os/usr/share/tilo/list.json", '{"v": 3}\n')
        problems = checker.check(self.root)
        self.assertEqual(len(problems), 1)
        self.assertIn(f"{CONF}:2", problems[0])
        self.assertIn("lacks this version", problems[0])

    def test_a_seed_without_history_fails(self) -> None:
        self.git("rm", "-rqf", "os/tests/seeded-state")
        problems = checker.check(self.root)
        self.assertEqual(len(problems), 1)
        self.assertIn("holds none of the versions", problems[0])

    def test_a_history_no_test_reads_fails(self) -> None:
        self.git("rm", "-qf", "crates/reader/src/lib.rs")
        problems = checker.check(self.root)
        self.assertEqual(len(problems), 1)
        self.assertIn("no test names", problems[0])

    def test_a_seed_copied_from_nothing_shipped_fails(self) -> None:
        self.write(CONF, SEED.replace("/usr/share/tilo/list.json", "/usr/share/tilo/gone.json"))
        problems = checker.check(self.root)
        self.assertEqual(len(problems), 1)
        self.assertIn("no tracked file", problems[0])

    def test_inline_content_written_once_is_checked_too(self) -> None:
        self.write(
            "dist/session/tmpfiles.d/x.conf",
            "f /var/lib/tilo/flag 0644 root root - enabled=1\n",
        )
        problems = checker.check(self.root)
        self.assertEqual(len(problems), 1)
        self.assertIn("/var/lib/tilo/flag", problems[0])

    def test_lines_that_overwrite_or_write_nothing_are_ignored(self) -> None:
        self.write(
            CONF,
            SEED
            + "C+ /var/lib/tilo/fresh.json - - - - /usr/share/tilo/fresh.json\n"
            + "f+ /var/lib/tilo/state 0644 root root - fresh\n"
            + "f /var/lib/systemd/linger/tilo 0644 root root -\n"
            + "d /var/lib/tilo 0755 root root -\n"
            + "L /etc/x - - - - /usr/share/x\n",
        )
        self.assertEqual(checker.check(self.root), [])


if __name__ == "__main__":
    unittest.main()
