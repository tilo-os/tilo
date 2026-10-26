"""Tests for tools/policy/check_image_interpreters.py."""

from __future__ import annotations

import contextlib
import importlib.util
import io
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest
from unittest import mock

sys.path.insert(0, str(Path(__file__).resolve().parent))
from git_fixture import isolated_git_environment  # noqa: E402


ROOT = Path(__file__).resolve().parents[2]
SPEC = importlib.util.spec_from_file_location(
    "check_image_interpreters", ROOT / "tools/policy/check_image_interpreters.py"
)
assert SPEC and SPEC.loader
checker = importlib.util.module_from_spec(SPEC)
sys.modules["check_image_interpreters"] = checker
SPEC.loader.exec_module(checker)

HEADER = "# Interpreter scripts the image still ships.\n\n"


class ImageInterpreterTests(unittest.TestCase):
    def setUp(self) -> None:
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.git("init", "-q")
        self.write("os/usr/libexec/tilo-legacy", "#!/usr/bin/env python3\nprint()\n")
        self.write("os/usr/libexec/tilo-compiled-wrapper", "#!/bin/bash\nexec true\n")
        self.baseline("os/usr/libexec/tilo-legacy  # predates the rule\n")

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

    def baseline(self, body: str) -> None:
        self.write(str(checker.BASELINE), HEADER + body, track=False)

    def test_the_repository_ships_no_new_interpreter_script(self) -> None:
        self.assertEqual(checker.check(ROOT), ([], []))

    def test_baselined_script_and_shell_glue_pass(self) -> None:
        self.assertEqual(checker.check(self.root), ([], []))

    def test_unreadable_tracked_file_is_not_a_clean_inventory(self) -> None:
        relative = "os/usr/libexec/unobserved"
        self.write(relative, "#!/usr/bin/env python3\n")
        path = self.root / relative
        path.unlink()
        # Both a missing checkout file and a directory in its place used to
        # disappear from the inventory, hiding a new extensionless script.
        for directory in (False, True):
            with self.subTest(directory=directory):
                if directory:
                    path.mkdir()
                out = io.StringIO()
                with mock.patch.object(checker, "ROOT", self.root), contextlib.redirect_stderr(out):
                    self.assertEqual(checker.main(), 1)
                self.assertIn("Cannot inspect", out.getvalue())
                self.assertIn("unobserved", out.getvalue())

    def test_new_shebang_script_fails(self) -> None:
        self.write("os/usr/libexec/new-thing", "#!/usr/bin/env python3\n")
        self.write("os/etc/tilo/hook", "#!/usr/bin/python3 -I\n")
        self.write("os/usr/lib/tilo/serve", "#!/usr/bin/env -S node --no-warnings\n")
        self.write("os/usr/lib/tilo/report", "#!/usr/bin/perl -w\n")
        new, stale = checker.check(self.root)
        self.assertEqual(
            new,
            [
                "os/etc/tilo/hook",
                "os/usr/lib/tilo/report",
                "os/usr/lib/tilo/serve",
                "os/usr/libexec/new-thing",
            ],
        )
        self.assertEqual(stale, [])

    def test_source_file_without_shebang_fails(self) -> None:
        self.write("os/usr/lib/tilo/helper.py", "import os\n")
        self.write("os/usr/share/tilo/panel.mjs", "export {}\n")
        new, _ = checker.check(self.root)
        self.assertEqual(new, ["os/usr/lib/tilo/helper.py", "os/usr/share/tilo/panel.mjs"])

    def test_renaming_a_baselined_script_does_not_admit_it(self) -> None:
        self.git("mv", "os/usr/libexec/tilo-legacy", "os/usr/libexec/tilo-renamed")
        self.assertEqual(
            checker.check(self.root),
            (["os/usr/libexec/tilo-renamed"], ["os/usr/libexec/tilo-legacy"]),
        )

    def test_stale_entry_fails_and_names_its_line(self) -> None:
        self.git("rm", "-qf", "os/usr/libexec/tilo-legacy")
        self.assertEqual(checker.check(self.root), ([], ["os/usr/libexec/tilo-legacy"]))
        out = io.StringIO()
        with mock.patch.object(checker, "ROOT", self.root), contextlib.redirect_stdout(out):
            self.assertEqual(checker.main(), 1)
        self.assertIn(
            f"{checker.BASELINE}:3: os/usr/libexec/tilo-legacy no longer ships",
            out.getvalue(),
        )

    def test_ported_script_is_stale(self) -> None:
        self.write("os/usr/libexec/tilo-legacy", "#!/bin/sh\nexec /usr/libexec/tilo/tilo-legacy\n")
        self.assertEqual(checker.check(self.root), ([], ["os/usr/libexec/tilo-legacy"]))

    def test_build_and_ci_scripts_outside_the_overlay_are_ignored(self) -> None:
        self.write("os/scripts/build-step.py", "#!/usr/bin/env python3\n")
        self.write("os/dev/vm-helper", "#!/usr/bin/env python3\n")
        self.write("os/Containerfile", "RUN python3 /tilo/windows_provider.py\n")
        self.write("tools/ci/report.py", "#!/usr/bin/env python3\n")
        self.assertEqual(checker.check(self.root), ([], []))

    def test_untracked_files_are_ignored(self) -> None:
        self.write("os/usr/libexec/local-experiment", "#!/usr/bin/env python3\n", track=False)
        self.assertEqual(checker.check(self.root), ([], []))

    def test_shell_scripts_that_run_python_fail_and_mentions_do_not(self) -> None:
        self.write("os/usr/libexec/tilo-pipe", "#!/bin/bash\nsfdisk --json | python3 -c 'import json'\n")
        self.write("os/usr/libexec/tilo-heredoc", "#!/bin/sh\nout=$(python3 - <<PY\nPY\n)\n")
        self.write("os/usr/libexec/tilo-mention", "#!/bin/sh\n# python3 used to do this\necho done\n")
        new, _ = checker.check(self.root)
        self.assertEqual(new, ["os/usr/libexec/tilo-heredoc", "os/usr/libexec/tilo-pipe"])

    def test_host_payload_scripts_count_and_its_tests_do_not(self) -> None:
        self.write("dist/session/tilo-helper.py", "print()\n")
        self.write("dist/session/tests/fixture.py", "print()\n")
        new, _ = checker.check(self.root)
        self.assertEqual(new, ["dist/session/tilo-helper.py"])

    def test_python_the_image_build_installs_counts(self) -> None:
        self.write("tools/client.py", "print()\n")
        self.write("os/dev/build-helper.py", "print()\n")
        self.write(
            "os/Containerfile",
            "COPY tools/client.py /usr/libexec/tilo/client.py\n"
            "COPY --chmod=0755 os/dev/build-helper.py \\\n    /usr/local/bin/\n"
            "COPY --from=stage /out/bin/runner.py /usr/libexec/runner.py\n",
        )
        self.write(
            "os/components.toml",
            '[[component]]\nname = "x"\nfiles = [\n'
            '  { from = "@manifest/scripts/check.py", to = "/usr/libexec/tilo-check", mode = "0755" },\n]\n',
        )
        new, _ = checker.check(self.root)
        self.assertEqual(
            new,
            ["image:/usr/libexec/runner.py", "image:/usr/libexec/tilo-check", "image:/usr/libexec/tilo/client.py"],
        )

    def test_baseline_entry_needs_a_reason(self) -> None:
        self.baseline("os/usr/libexec/tilo-legacy\n")
        with self.assertRaisesRegex(checker.BaselineError, "needs a reason"):
            checker.check(self.root)

    def test_duplicate_baseline_entry_fails(self) -> None:
        self.baseline(
            "os/usr/libexec/tilo-legacy  # predates the rule\n"
            "os/usr/libexec/tilo-legacy  # again\n"
        )
        with self.assertRaisesRegex(checker.BaselineError, "already listed on line 3"):
            checker.check(self.root)

    def test_shebang_interpreter(self) -> None:
        cases = {
            b"#!/usr/bin/env python3\n": "python3",
            b"#!/usr/bin/python3 -I\n": "python3",
            b"#!/usr/bin/env -S PYTHONSAFEPATH=1 python3 -I\n": "python3",
            b"#!/bin/bash\n": "bash",
            b"#!\n": None,
            b"import os\n": None,
        }
        for line, expected in cases.items():
            with self.subTest(line=line):
                self.assertEqual(checker.shebang_interpreter(line), expected)


if __name__ == "__main__":
    unittest.main()
