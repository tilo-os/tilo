"""The bash scripts the live installer runs to put Tilo on a disk (`os/usr/libexec`).

Bash finds a call to a function nobody defines only when the line runs, and in `tilo-install` the
first such line runs minutes into an install, after the image is built, the medium boots and the
person has answered every step. A function removed while one of its calls stays now fails here,
in seconds. The scan reads the commands a line starts (at its start, after `;`, `&&`, `||`, `|`,
`!`, `then`, `do`, `if` and inside `$(...)`), and only names written as Tilo's own helpers are
(snake_case with an underscore): a command the image installs never looks like one.
"""
from __future__ import annotations

import os
from pathlib import Path
import re
import subprocess
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[2]
LIBEXEC = ROOT / "os/usr/libexec"
SCRIPTS = tuple(
    LIBEXEC / name
    for name in (
        "tilo-install",
        "tilo-guided-partition",
        "tilo-install-boot-menu",
        "tilo-install-efi-artifacts",
        "tilo-installer-profile-extract",
        "tilo-disk-encrypt",
        "tilo-store-volume",
        "tilo-boot-mount-contract",
        "tilo-verified-boot-install-preflight",
    )
)
SIZES = ROOT / "os/usr/share/tilo/install-sizes"
GIB = 1 << 30

DEFINITION = re.compile(r"^\s*(?:function\s+)?([A-Za-z_][\w:-]*)\s*\(\)", re.MULTILINE)
HEREDOC = re.compile(r"<<-?\s*(['\"]?)([A-Za-z_]\w*)\1")
# Arithmetic, parameter expansions and `{fd}>` redirections hold names that are not commands.
NOT_COMMANDS = re.compile(r"\$\(\(.*?\)\)|\$\{[^}]*\}|\{[A-Za-z_]\w*\}(?=[<>])")
COMMAND_START = re.compile(
    r"\$\(|`|&&|\|\||[;|(){}!]|\b(?:then|do|else|elif|if|while|until|time|exec|command)\b"
)
HELPER = re.compile(r"[a-z][a-z0-9]*(?:_[a-z0-9]+)+")


def undefined_calls(text: str) -> list[tuple[int, str]]:
    """The helpers a script calls that it defines nowhere, as (line, name)."""
    defined = set(DEFINITION.findall(text))
    found = []
    heredoc_end = None
    for number, line in enumerate(text.splitlines(), 1):
        if heredoc_end is not None:
            if line.strip() == heredoc_end:
                heredoc_end = None
            continue
        if heredoc := HEREDOC.search(line):
            heredoc_end = heredoc.group(2)
        code = re.sub(r"'[^']*'", "''", line)
        code = re.sub(r"(^|\s)#.*$", "", code)
        code = NOT_COMMANDS.sub("", code)
        for command in COMMAND_START.split(code):
            words = command.split()
            if not words:
                continue
            name = words[0].strip('"')
            if HELPER.fullmatch(name) and name not in defined:
                found.append((number, name))
    return found


class InstallerScripts(unittest.TestCase):
    def test_every_installer_script_parses(self) -> None:
        for script in SCRIPTS:
            with self.subTest(script=script.name):
                result = subprocess.run(
                    ["bash", "-n", str(script)], capture_output=True, text=True, check=False
                )
                self.assertEqual(result.returncode, 0, result.stderr)

    def test_no_installer_script_calls_a_function_it_does_not_define(self) -> None:
        found = [
            f"{script.name}:{number}: {name}"
            for script in SCRIPTS
            for number, name in undefined_calls(script.read_text())
        ]
        self.assertEqual(found, [], "calls to functions no installer script defines")

    def test_the_scan_finds_a_call_whose_function_was_removed(self) -> None:
        script = """#!/bin/bash
validate_disk_identity() { :; }
log() { printf '%s\\n' "$*"; }
validate_disk_identity
validate_home_target_capacity
if [ -n "$X" ]; then cleanup_alongside; fi
size="$(read_target_size "$DISK")"
log "$((free_bytes / 2)) ${root_bytes} 'not_a_call' # also_not_a_call"
exec {installer_lock}>/run/lock
cat <<EOF
heredoc_text_is_not_a_call
EOF
"""
        self.assertEqual(
            undefined_calls(script),
            [
                (5, "validate_home_target_capacity"),
                (6, "cleanup_alongside"),
                (7, "read_target_size"),
            ],
        )


class WholeDiskCapacity(unittest.TestCase):
    """`tilo-install` refuses a disk too small for Tilo before it writes anything to it."""

    SOURCE = (LIBEXEC / "tilo-install").read_text()

    def run_capacity_check(self, disk_bytes: int) -> subprocess.CompletedProcess[str]:
        body = re.search(r"^validate_disk_capacity\(\) \{\n.*?^\}$", self.SOURCE, re.MULTILINE | re.DOTALL)
        assert body
        with tempfile.TemporaryDirectory() as scratch:
            bin_dir = Path(scratch)
            blockdev = bin_dir / "blockdev"
            # The disk as the kernel reports it; the check's own logic runs unchanged.
            blockdev.write_text(
                '#!/bin/sh\n[ "$1" = --getsize64 ] && [ "$2" = /dev/vdb ] && echo "$DISK_BYTES"\n'
            )
            blockdev.chmod(0o755)
            script = "\n".join(
                [
                    "set -euo pipefail",
                    'die() { echo "$*" >&2; exit 1; }',
                    "DISK=/dev/vdb",
                    f". {SIZES}",
                    body.group(0),
                    "validate_disk_capacity",
                    "echo accepted",
                ]
            )
            return subprocess.run(
                ["bash", "-c", script],
                env=dict(os.environ, PATH=f"{bin_dir}:{os.environ['PATH']}", DISK_BYTES=str(disk_bytes)),
                capture_output=True,
                text=True,
                check=False,
            )

    def test_a_disk_below_35_gib_is_refused_and_one_of_35_gib_is_accepted(self) -> None:
        refused = self.run_capacity_check(32 * GIB)
        self.assertNotEqual(refused.returncode, 0)
        self.assertIn("/dev/vdb holds 32 GiB", refused.stderr)
        self.assertIn("at least 35 GiB", refused.stderr)
        self.assertNotEqual(self.run_capacity_check(35 * GIB - 1).returncode, 0)

        accepted = self.run_capacity_check(35 * GIB)
        self.assertEqual(accepted.returncode, 0, accepted.stderr)
        self.assertEqual(accepted.stdout.strip(), "accepted")

    def test_the_check_runs_for_erase_installs_before_anything_is_written(self) -> None:
        call = '[ "$ALONGSIDE" -eq 1 ] || validate_disk_capacity\n'
        self.assertEqual(self.SOURCE.count(call), 1)
        # tilo-disk-encrypt's `prepare` is what wipes and partitions the disk of an erase install.
        erase = '"$DISK_ENCRYPT" prepare "$DISK"'
        self.assertEqual(self.SOURCE.count(erase), 1)
        self.assertLess(self.SOURCE.index(call), self.SOURCE.index(erase))


if __name__ == "__main__":
    unittest.main()
