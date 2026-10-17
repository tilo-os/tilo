#!/usr/bin/env python3
"""Run the Containerfile's UKI projection on nonbootable, ordinary PE fixtures.

Only the bounded command-line/copy operations are extracted, never the whole RUN
instruction. Unknown operations fail extraction before any command is launched.
These fixtures prove byte/token handling, not image capacity, signatures or boot.
"""

import os
from pathlib import Path
import re
import shlex
import struct
import subprocess
import tempfile
import tomllib
import unittest


ROOT = Path(__file__).resolve().parents[2]
CONTAINERFILE = ROOT / "os/Containerfile.installer"
# The UKI editor under test: tilo-esp-write run as tilo-uki-cmdline. Its
# Cargo test (crates/system/esp-write/tests/uki_cmdline.rs) builds it and
# names the link here.
HELPER = os.environ["TILO_UKI_CMDLINE"]
INSTALLED_HELPER = "/usr/libexec/tilo-uki-cmdline"
TARGET = "/boot/tilo-target/tilo-target.efi"
ROOT_TOKEN = "root=tilo-auto"
OPTIONS_TOKEN = "rd.luks.options=tpm2-device=auto,fido2-device=auto"
LIVE_ONLY_KEYS = {"tilo.install"}
BOOT_MOUNT_TOKEN = "systemd.mount-extra=LABEL=TILO_BOOT:/boot:ext4:ro,nosuid,nodev,noexec"


def projection_commands(source):
    """Observe one contiguous production span; do not filter unknown commands."""
    commands = []
    for raw in source.splitlines():
        line = raw.strip()
        if not line or line.startswith("#"):
            continue
        line = line.removesuffix("\\").rstrip()
        if line.startswith(("RUN ", "&& ")):
            commands.append(line.removeprefix("&& "))
        elif commands:
            commands[-1] += " " + line
    start = f"{INSTALLED_HELPER} remove systemd.show_status"
    required_services = {
        "tilo-firstboot-user.service", "tilo-firstboot-autologin-cleanup.service",
        "tilo-installer-setup.service", "tilo-live-recovery.service",
    }
    allowed_services = required_services | {"tilo-installer-code-refresh.service"}
    stops = []
    for command in commands:
        words = shlex.split(command)
        if words[:2] == ["systemctl", "enable"] and required_services <= set(words[2:]):
            if set(words[2:]) != allowed_services or len(words[2:]) != len(allowed_services):
                raise ValueError("unexpected services at UKI projection boundary")
            stops.append(command)
    if commands.count(start) != 1 or len(stops) != 1:
        raise ValueError("cannot observe exactly one UKI projection span")
    stop = stops[0]
    begin, end = commands.index(start), commands.index(stop)
    if begin >= end:
        raise ValueError("invalid UKI projection span")
    selected = commands[begin:end]
    boundary = "install -d -m 0755 /boot/tilo-target/predictions"
    if selected.count(boundary) != 1:
        raise ValueError("cannot observe target projection boundary")
    helper = re.escape(INSTALLED_HELPER)
    target = re.escape(TARGET)
    token = r"[A-Za-z0-9_=.,-]+"
    patterns = [
        rf"{helper} (?:remove|append) {token}(?: {target})?",
        rf"{helper} show(?: {target})? \| grep -qw {token}",
        rf"! {helper} show {target} \| grep -qw {token}",
    ]
    fixed = {
        boundary,
        "set -- /boot/EFI/Linux/*.efi",
        '[ "$#" -eq 1 ] && [ -f "$1" ]',
        f'cp "$1" {TARGET}',
        f"{INSTALLED_HELPER} append {BOOT_MOUNT_TOKEN} {TARGET}",
        f"{INSTALLED_HELPER} show {TARGET} | grep -qw {BOOT_MOUNT_TOKEN}",
        f'live_cmdline="$({INSTALLED_HELPER} show)"',
        f'target_cmdline="$({INSTALLED_HELPER} show {TARGET})"',
    }
    # Accept the narrow shell checks as production code, not a test-side
    # reimplementation. Their result is evaluated by the real shell below.
    for variable in ("live_cmdline", "target_cmdline"):
        for key in ("root", "rd[.]luks[.]options"):
            check = ('test "$(printf \'%s\\n\' "$' + variable + '" | '
                     'tr \'[:space:]\' \'\\n\' | grep -E \'^' + key + '(=|$)\')" = ')
            patterns.append(re.escape(check) + token)
    for command in selected:
        if command not in fixed and not any(re.fullmatch(p, command) for p in patterns):
            raise ValueError(f"unsupported UKI projection command: {command}")
    split = selected.index(boundary)
    return selected[:split], selected[split:]


def fixture_bytes(tokens):
    """PE32+ geometry only; every payload is explicitly nonbootable test data."""
    sections = [
        (b".linux", b"NONBOOTABLE fixture kernel"),
        (b".cmdline", " ".join(tokens).encode() + b"\0"),
        (b".initrd", b"NONBOOTABLE fixture initrd"),
        (b".osrel", b"ID=tilo-fixture\n"),
        (b".dtb", b"NONBOOTABLE fixture device tree"),
    ]
    data = bytearray(1024 + len(sections) * 2048)
    data[:2] = b"MZ"
    struct.pack_into("<I", data, 0x3C, 128)
    data[128:132] = b"PE\0\0"
    struct.pack_into("<HH", data, 132, 0x8664, len(sections))
    struct.pack_into("<HH", data, 148, 240, 0x2022)
    struct.pack_into("<H", data, 152, 0x20B)
    struct.pack_into("<H", data, 152 + 68, 10)
    for i, (name, payload) in enumerate(sections):
        offset = 1024 + i * 2048
        if len(payload) > 2048:
            raise ValueError("fixture payload exceeds synthetic section room")
        struct.pack_into("<8sIIII", data, 392 + i * 40, name, len(payload),
                         (i + 1) * 4096, 2048, offset)
        data[offset:offset + len(payload)] = payload
    return bytes(data)


def restart_fixture(test):
    """Give the next subTest phase of a merged test the fresh fixture it had
    as its own test: tear down, run cleanups, reset instance state, set up."""
    test.tearDown()
    test.doCleanups()
    fresh = type(test)(test._testMethodName).__dict__
    keep = {key: test.__dict__[key] for key in ("_outcome", "_subtest") if key in test.__dict__}
    test.__dict__.clear()
    test.__dict__.update(fresh, **keep)
    test.setUp()


class TargetCmdlineTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix="tilo-target-cmdline-")
        self.addCleanup(self.temp.cleanup)
        self.directory = Path(self.temp.name)
        self.live = self.directory / "boot/EFI/Linux/fixture.efi"
        self.live.parent.mkdir(parents=True)
        self.target = self.directory / TARGET.lstrip("/")
        self.prepare, self.project = projection_commands(CONTAINERFILE.read_text())
        self.base = []
        for path in sorted((ROOT / "os/usr/lib/bootc/kargs.d").glob("*.toml")):
            with path.open("rb") as stream:
                self.base.extend(tomllib.load(stream)["kargs"])
        # Generation is generated at build time; the digest comes from bootc.
        self.base += ["tilo.generation=42", "composefs=" + "a" * 128]

    def cli(self, *args):
        return subprocess.run([HELPER, *map(str, args)],
                              capture_output=True, text=True, check=False)

    def run_commands(self, commands):
        mapped = []
        for command in commands:
            # Exact paths only; the host's /boot and installed helper are never
            # read or executed. Commands were fully checked before mapping.
            uses_default = command.startswith(INSTALLED_HELPER) and " /boot/" not in command
            for path in (TARGET, "/boot/tilo-target/predictions", "/boot/EFI/Linux"):
                command = command.replace(path, shlex.quote(str(self.directory / path.lstrip("/"))))
            command = command.replace(
                f"$({INSTALLED_HELPER} show)",
                f"$({INSTALLED_HELPER} show {shlex.quote(str(self.live))})",
            )
            if uses_default:
                if " | " in command:
                    command = command.replace(" | ", f" {shlex.quote(str(self.live))} | ", 1)
                else:
                    command += " " + shlex.quote(str(self.live))
            command = command.replace(INSTALLED_HELPER, shlex.quote(HELPER))
            mapped.append(command)
        return subprocess.run(["bash", "-c", "set -x\n" + " &&\n".join(mapped)],
                              cwd=self.directory, env=os.environ,
                              capture_output=True, text=True, check=False)

    def prepare_live(self, tokens=None):
        self.live.write_bytes(fixture_bytes(self.base if tokens is None else tokens))
        result = self.run_commands(self.prepare)
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        return self.live.read_bytes()

    def tokens(self, path):
        result = self.cli("show", path)
        self.assertEqual(result.returncode, 0, result.stderr)
        return result.stdout.split()

    def assert_only_cmdline_changed(self, before, after):
        self.assertEqual(len(after), len(before))
        # Only .cmdline's VirtualSize and its raw bytes may differ. Checking all
        # remaining bytes covers section geometry, headers and every payload.
        header = 392 + 40
        raw = 1024 + 2048
        self.assertEqual(after[:header + 8], before[:header + 8])
        self.assertEqual(after[header + 12:raw], before[header + 12:raw])
        self.assertEqual(after[raw + 2048:], before[raw + 2048:])
        size = struct.unpack_from("<I", after, header + 8)[0]
        self.assertLessEqual(size, 2048)
        self.assertEqual(after[raw + size:raw + 2048], bytes(2048 - size))

    def test_target_projection_and_extraction(self):
        with self.subTest(phase="current_base_projects_without_duplicate_inherited_keys"):
            # Duplicate appends, a missed live-token removal, or a lost inherited
            # argument must break this real producer path.
            self.assertEqual(self.base.count(ROOT_TOKEN), 1)
            self.assertEqual(self.base.count(OPTIONS_TOKEN), 1)
            live_before = self.prepare_live()
            live_tokens = self.tokens(self.live)
            self.assertIn("tilo.install", live_tokens)
            result = self.run_commands(self.project)
            self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
            target_tokens = self.tokens(self.target)
            self.assertEqual(target_tokens.count(ROOT_TOKEN), 1)
            self.assertEqual(target_tokens.count(OPTIONS_TOKEN), 1)
            self.assertEqual(
                target_tokens,
                [t for t in live_tokens if t.split("=", 1)[0] not in LIVE_ONLY_KEYS]
                + [BOOT_MOUNT_TOKEN],
            )
            self.assertTrue(LIVE_ONLY_KEYS.isdisjoint({t.split("=", 1)[0] for t in target_tokens}))
            for token in ("composefs=" + "a" * 128, "tilo.generation=42", "tilo.mode=normal", "preempt=full"):
                self.assertIn(token, target_tokens)
            self.assertEqual(self.live.read_bytes(), live_before)
            self.assert_only_cmdline_changed(live_before, self.target.read_bytes())
        restart_fixture(self)
        with self.subTest(phase="shared_helper_still_refuses_duplicate_append_without_writing"):
            self.prepare_live()
            projected = self.run_commands(self.project)
            self.assertEqual(projected.returncode, 0, projected.stdout + projected.stderr)
            target_before = self.target.read_bytes()
            for token in (ROOT_TOKEN, OPTIONS_TOKEN):
                with self.subTest(token=token):
                    result = self.cli("append", token, self.target)
                    self.assertNotEqual(result.returncode, 0)
                    self.assertIn("is already on the sealed command line", result.stderr)
                    self.assertEqual(self.target.read_bytes(), target_before)
        restart_fixture(self)
        with self.subTest(phase="extraction_fails_when_subject_is_missing_or_changes_unsupported"):
            source = CONTAINERFILE.read_text()
            marker = f"&& {INSTALLED_HELPER} remove tilo.install {TARGET}"
            self.assertEqual(source.count(marker), 1)
            for changed in ("", source.replace(marker, marker + " && true"),
                            source.replace(marker, "&& unexpected_command\n    " + marker),
                            source.replace("enable tilo-installer-code-refresh.service", "enable unknown.service tilo-installer-code-refresh.service"),
                            source.replace("enable tilo-installer-code-refresh.service", "enable")):
                with self.subTest(source=changed[-100:]):
                    with self.assertRaises(ValueError):
                        projection_commands(changed)

    def test_invalid_inherited_keys_are_rejected_before_target_projection(self):
        for expected in (ROOT_TOKEN, OPTIONS_TOKEN):
            key = expected.split("=", 1)[0]
            cases = {
                "missing": [],
                "duplicate": [expected, expected],
                "conflicting": [key + "=unexpected"],
                "bare": [key],
                "empty": [key + "="],
                "extra equals": [expected + "=unexpected"],
                "additional conflict": [expected, key + "=unexpected"],
                "additional bare": [expected, key],
            }
            for name, replacement in cases.items():
                with self.subTest(key=key, invalid=name):
                    tokens = [t for t in self.base if t != expected] + replacement
                    self.live.write_bytes(fixture_bytes(tokens))
                    result = self.run_commands(self.prepare)
                    self.assertEqual(result.returncode, 1, result.stdout + result.stderr)
                    trace = [line for line in result.stderr.splitlines() if line.startswith("+ ")]
                    self.assertTrue(trace[-1].startswith("+ test "), result.stderr)
                    self.assertFalse(self.target.exists())
                    remaining = [
                        token for token in self.tokens(self.live)
                        if token.split("=", 1)[0] == key
                    ]
                    self.assertEqual(remaining, replacement)


if __name__ == "__main__":
    unittest.main()
