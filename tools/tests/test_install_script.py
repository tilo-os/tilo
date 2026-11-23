"""The installers people run with `curl -fsSL https://tilo.sh | sh` (`dist/install/tilo.sh`) and
`curl -fsSL https://tilolang.dev/install | sh` (`dist/install/tilolang.sh`, written from it).

It is run by whatever `sh` a computer has. On macOS that is bash 3.2, which in a UTF-8 locale
takes the first byte of a character outside ASCII that follows a bare variable name as part of
the name, and which ends a script that stopped on an unset variable with status 0 once an exit
trap is set. Together they made a failed install on a Mac look like a success with nothing
installed.
"""
from __future__ import annotations

import os
from pathlib import Path
import re
import shutil
import subprocess
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[2]
SCRIPT = ROOT / "dist/install/tilo.sh"
SCRIPTS = (SCRIPT, ROOT / "dist/install/tilolang.sh")


class InstallScript(unittest.TestCase):
    def test_no_bare_variable_stands_before_a_character_outside_ascii(self) -> None:
        found = [
            (script.name, number, line.decode("utf-8", "replace").strip())
            for script in SCRIPTS
            for number, line in enumerate(script.read_bytes().splitlines(), 1)
            if re.search(rb"\$[A-Za-z_][A-Za-z0-9_]*[\x80-\xff]", line)
        ]
        self.assertEqual(found, [], "write ${name} where a character outside ASCII follows the name")

    def test_an_install_that_did_not_finish_never_ends_as_a_success(self) -> None:
        for script in SCRIPTS:
            with self.subTest(script=script.name):
                self.an_install_that_did_not_finish_never_ends_as_a_success(script)

    def an_install_that_did_not_finish_never_ends_as_a_success(self, script: Path) -> None:
        text = script.read_text(encoding="utf-8")
        exit_traps = [line for line in text.splitlines() if re.search(r"^\s*trap '.*' EXIT\s*$", line)]
        self.assertEqual(len(exit_traps), 1, exit_traps)
        trap = exit_traps[0]
        # The trap fails an exit the install did not mark as finished, whatever status sh reports.
        self.assertIn('[ -n "$finished" ]', trap)
        self.assertIn("status=1", trap)
        self.assertIn('exit "$status"', trap)
        self.assertRegex(text, r"\n\s*finished=\n", "the mark starts empty, before the trap")
        self.assertRegex(text, r"\n\s*finished=1\n\s*return\n", "the install marks itself finished where it ends")

    def test_a_release_that_cannot_be_fetched_fails_in_a_utf8_locale(self) -> None:
        if not (shutil.which("curl") or shutil.which("wget")):
            self.skipTest("the installer needs curl or wget")
        with tempfile.TemporaryDirectory() as home:
            env = {
                "HOME": home,
                "PATH": os.environ.get("PATH", "/usr/bin:/bin"),
                "LC_ALL": "en_US.UTF-8",
                "LANG": "en_US.UTF-8",
                # Nothing listens on this computer's discard port.
                "TILO_RELEASES": "http://127.0.0.1:9/none",
            }
            ran = subprocess.run(
                ["sh", str(SCRIPT)], env=env, capture_output=True, encoding="utf-8", errors="replace",
                timeout=120, check=False,
            )
            said = ran.stderr
            self.assertNotEqual(ran.returncode, 0, said)
            self.assertIn("Getting Tilo (latest) for ", said)
            self.assertNotIn("unbound variable", said)
            self.assertIn("could not get http://127.0.0.1:9/none/latest/SHA256SUMS", said)
            self.assertFalse((Path(home) / ".local/bin/tilo").exists())


if __name__ == "__main__":
    unittest.main()
