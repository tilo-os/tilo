#!/usr/bin/env python3
"""Exercise the installer boot-identity guard on ordinary nonbootable PE data."""

import hashlib
import os
from pathlib import Path
import re
import shlex
import struct
import subprocess
import sys
import tempfile
import unittest


ROOT = Path(__file__).resolve().parents[2]
HELPER = ROOT / "os/dev/uki-cmdline.py"
INSTALLER = ROOT / "os/usr/libexec/tilo-install"
INSTALLED_HELPER = "/usr/libexec/tilo-uki-cmdline"
START_MARKER = "  # bootc stores SHA256(.linux + .initrd) in deployment origin metadata."
END_MARKER = '  install -d -m 0755 "$esp/EFI/tilo/pcr-predictions"'
KERNEL = b"NONBOOTABLE-KERNEL-VIRTUAL-BYTES"
INITRD = b"NONBOOTABLE-INITRAMFS-VIRTUAL-BYTES"
EXPECTED_DIGEST = hashlib.sha256(KERNEL + INITRD).hexdigest()
DESTINATION_SENTINEL = b"destination sentinel"
NEW_SENTINEL = b"new sentinel"
UNRELATED_SENTINEL = b"unrelated sentinel"


def pe_fixture(kernel=KERNEL, initrd=INITRD, cmdline=b"tilo.mode=install", padding=b"P",
               omit=(), duplicate=()):
    """Build PE section geometry only; the returned bytes cannot boot."""
    sections = [
        (b".linux", kernel, len(kernel) + 17),
        (b".cmdline", cmdline, len(cmdline) + 29),
        (b".initrd", initrd, len(initrd) + 23),
    ]
    sections = [row for row in sections if row[0].decode() not in omit]
    for name in duplicate:
        original = next(row for row in sections if row[0].decode() == name)
        sections.append(original)

    raw_stride = 256
    raw_base = 1024
    data = bytearray(raw_base + raw_stride * len(sections))
    data[:2] = b"MZ"
    pe = 128
    struct.pack_into("<I", data, 0x3C, pe)
    data[pe:pe + 4] = b"PE\0\0"
    struct.pack_into("<HH", data, pe + 4, 0x8664, len(sections))
    optional_size = 240
    struct.pack_into("<H", data, pe + 20, optional_size)
    table = pe + 24 + optional_size
    fill = (padding or b"P")[:1]
    for index, (name, payload, raw_size) in enumerate(sections):
        raw = raw_base + index * raw_stride
        header = table + index * 40
        struct.pack_into(
            "<8sIIII",
            data,
            header,
            name,
            len(payload),
            (index + 1) * 4096,
            raw_size,
            raw,
        )
        data[raw:raw + raw_size] = payload + fill * (raw_size - len(payload))
    return bytes(data)


def mutate_required_section(data, name, *, virtual_size=None, raw_size=None, raw_offset=None):
    changed = bytearray(data)
    pe = struct.unpack_from("<I", changed, 0x3C)[0]
    count = struct.unpack_from("<H", changed, pe + 6)[0]
    optional_size = struct.unpack_from("<H", changed, pe + 20)[0]
    table = pe + 24 + optional_size
    matches = []
    for index in range(count):
        header = table + index * 40
        section_name = changed[header:header + 8].rstrip(b"\0").decode("ascii")
        if section_name == name:
            matches.append(header)
    if len(matches) != 1:
        raise ValueError(f"fixture needs exactly one {name} header")
    header = matches[0]
    if virtual_size is not None:
        struct.pack_into("<I", changed, header + 8, virtual_size)
    if raw_size is not None:
        struct.pack_into("<I", changed, header + 16, raw_size)
    if raw_offset is not None:
        struct.pack_into("<I", changed, header + 20, raw_offset)
    return bytes(changed)


def logical_commands(snippet):
    commands = []
    pending = ""
    for raw in snippet.splitlines():
        line = raw.strip()
        if not line or line.startswith("#"):
            continue
        pending += (" " if pending else "") + line.removesuffix("\\").rstrip()
        if not line.endswith("\\"):
            commands.append(re.sub(r"\s+", " ", pending))
            pending = ""
    if pending:
        raise ValueError("unterminated installer guard continuation")
    return commands


def guard_statements(source):
    """Extract only the checked identity observation and two promotion commands."""
    function_marker = "promote_target_uki() {"
    if source.count(function_marker) != 1:
        raise ValueError("cannot observe exactly one promote_target_uki function")
    body = source.split(function_marker, 1)[1].split("\n}\n", 1)[0]
    if body.count(START_MARKER) != 1 or body.count(END_MARKER) != 1:
        raise ValueError("cannot observe exactly one target boot identity guard")
    before, remainder = body.split(START_MARKER, 1)
    del before
    guarded, after = remainder.split(END_MARKER, 1)
    del after
    snippet = START_MARKER + guarded

    legacy = [
        ('[ "$(/usr/libexec/tilo-uki-cmdline boot-digest "$TARGET_UKI")" = '
         '"$(/usr/libexec/tilo-uki-cmdline boot-digest "$TARGET_VARIANT")" ] '
         '|| die "target UKI changes the bootc kernel/initramfs identity"'),
        'install -m 0644 "$TARGET_VARIANT" "$TARGET_UKI.new"',
        'mv -f "$TARGET_UKI.new" "$TARGET_UKI"',
    ]
    checked = [
        "local installed_boot_digest target_boot_digest",
        ('installed_boot_digest="$(/usr/libexec/tilo-uki-cmdline boot-digest '
         '"$TARGET_UKI")" || die "cannot read installed UKI boot identity"'),
        ('target_boot_digest="$(/usr/libexec/tilo-uki-cmdline boot-digest '
         '"$TARGET_VARIANT")" || die "cannot read target UKI boot identity"'),
        ('[[ "$installed_boot_digest" =~ ^[0-9a-f]{64}$ ]] && '
         '[[ "$target_boot_digest" =~ ^[0-9a-f]{64}$ ]] '
         '|| die "UKI boot identity is not a SHA-256 digest"'),
        ('[ "$installed_boot_digest" = "$target_boot_digest" ] '
         '|| die "target UKI changes the bootc kernel/initramfs identity"'),
        'install -m 0644 "$TARGET_VARIANT" "$TARGET_UKI.new"',
        'mv -f "$TARGET_UKI.new" "$TARGET_UKI"',
    ]
    commands = logical_commands(snippet)
    if commands not in (legacy, checked):
        raise ValueError("unsupported statements in target boot identity guard")
    if snippet.count(INSTALLED_HELPER + " boot-digest") != 2:
        raise ValueError("guard must make exactly two boot-digest observations")
    return snippet


class BootDigestCliTests(unittest.TestCase):
    def cli(self, *arguments):
        return subprocess.run(
            [sys.executable, "-B", str(HELPER), *map(str, arguments)],
            env=dict(os.environ, PYTHONDONTWRITEBYTECODE="1"),
            capture_output=True,
            text=True,
            check=False,
        )

    def test_real_boot_digest_cli(self):
        with self.subTest(phase="real_cli_hashes_only_virtual_kernel_and_initramfs_bytes"):
            with tempfile.TemporaryDirectory(prefix="tilo-boot-digest-") as temporary:
                root = Path(temporary)
                fixtures = [
                    pe_fixture(),
                    pe_fixture(cmdline=b"root=tilo-auto tilo.mode=normal"),
                    pe_fixture(cmdline=b"different command line", padding=b"Z"),
                ]
                for index, contents in enumerate(fixtures):
                    with self.subTest(index=index):
                        path = root / f"fixture-{index}.efi"
                        path.write_bytes(contents)
                        result = self.cli("boot-digest", path)
                        self.assertEqual(result.returncode, 0, result.stderr)
                        self.assertEqual(result.stdout, EXPECTED_DIGEST + "\n")
                        self.assertEqual(result.stderr, "")
                        self.assertEqual(path.read_bytes(), contents)

                for label, contents in (
                    ("kernel", pe_fixture(kernel=b"X" + KERNEL[1:])),
                    ("initrd", pe_fixture(initrd=b"X" + INITRD[1:])),
                ):
                    with self.subTest(changed=label):
                        path = root / f"changed-{label}.efi"
                        path.write_bytes(contents)
                        result = self.cli("boot-digest", path)
                        self.assertEqual(result.returncode, 0, result.stderr)
                        self.assertRegex(result.stdout, r"^[0-9a-f]{64}\n$")
                        self.assertNotEqual(result.stdout, EXPECTED_DIGEST + "\n")
                        self.assertEqual(path.read_bytes(), contents)
        with self.subTest(phase="real_cli_rejects_invalid_required_sections_without_a_digest"):
            good = pe_fixture()
            initrd_header_raw_size = len(INITRD) + 23
            cases = {
                "missing linux": pe_fixture(omit=(".linux",)),
                "missing initrd": pe_fixture(omit=(".initrd",)),
                "duplicate linux": pe_fixture(duplicate=(".linux",)),
                "duplicate initrd": pe_fixture(duplicate=(".initrd",)),
                "zero linux": mutate_required_section(good, ".linux", virtual_size=0),
                "virtual larger than raw": mutate_required_section(
                    good, ".initrd", virtual_size=initrd_header_raw_size + 1
                ),
                "required bytes outside file": mutate_required_section(
                    good, ".linux", raw_offset=len(good) - 1
                ),
            }
            with tempfile.TemporaryDirectory(prefix="tilo-boot-digest-invalid-") as temporary:
                root = Path(temporary)
                for label, contents in cases.items():
                    with self.subTest(case=label):
                        path = root / (label.replace(" ", "-") + ".efi")
                        path.write_bytes(contents)
                        result = self.cli("boot-digest", path)
                        self.assertNotEqual(result.returncode, 0)
                        self.assertEqual(result.stdout, "")
                        self.assertEqual(path.read_bytes(), contents)

                absent = root / "absent.efi"
                result = self.cli("boot-digest", absent)
                self.assertNotEqual(result.returncode, 0)
                self.assertEqual(result.stdout, "")
                self.assertFalse(absent.exists())
        with self.subTest(phase="boot_digest_arity_fails_before_any_default_discovery"):
            for arguments in (("boot-digest",), ("boot-digest", "one", "two")):
                with self.subTest(arguments=arguments):
                    result = self.cli(*arguments)
                    self.assertNotEqual(result.returncode, 0)
                    self.assertEqual(result.stdout, "")
                    self.assertIn("boot-digest needs one UKI path", result.stderr)


class InstallerBootDigestGuardTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.guard = guard_statements(INSTALLER.read_text())

    def run_real_guard(self, installed, variant):
        invocation = shlex.join([sys.executable, "-B", str(HELPER)])
        snippet = self.guard.replace(INSTALLED_HELPER, invocation)
        script = """set -euo pipefail
die() { printf '%s\n' "$*" >&2; exit 1; }
run_guard() {
  local TARGET_UKI="$1" TARGET_VARIANT="$2"
""" + snippet + """
}
run_guard "$1" "$2"
"""
        return subprocess.run(
            ["bash", "-c", script, "boot-digest-guard", str(installed), str(variant)],
            env=dict(os.environ, PYTHONDONTWRITEBYTECODE="1"),
            capture_output=True,
            text=True,
            check=False,
        )

    def run_fault_guard(self, installed, variant, output):
        snippet = self.guard.replace(INSTALLED_HELPER, "fixture_digest")
        script = """set -euo pipefail
die() { printf '%s\n' "$*" >&2; exit 1; }
fixture_digest() {
  [ "$1" = boot-digest ] || return 97
  printf '%s' "$FAULT_DIGEST_OUTPUT"
}
run_guard() {
  local TARGET_UKI="$1" TARGET_VARIANT="$2"
""" + snippet + """
}
run_guard "$1" "$2"
"""
        return subprocess.run(
            ["bash", "-c", script, "boot-digest-fault", str(installed), str(variant)],
            env=dict(os.environ, FAULT_DIGEST_OUTPUT=output),
            capture_output=True,
            text=True,
            check=False,
        )

    def make_paths(self, root, installed_bytes, variant_bytes):
        installed = root / "installed.efi"
        variant = root / "variant.efi"
        if installed_bytes is not None:
            installed.write_bytes(installed_bytes)
        if variant_bytes is not None:
            variant.write_bytes(variant_bytes)
        new = Path(str(installed) + ".new")
        new.write_bytes(NEW_SENTINEL)
        unrelated = root / "unrelated"
        unrelated.write_bytes(UNRELATED_SENTINEL)
        return installed, variant, new, unrelated

    def assert_refused_without_promotion(self, installed_bytes, variant_bytes, expected_error):
        with tempfile.TemporaryDirectory(prefix="tilo-boot-guard-refusal-") as temporary:
            installed, variant, new, unrelated = self.make_paths(
                Path(temporary), installed_bytes, variant_bytes
            )
            result = self.run_real_guard(installed, variant)
            self.assertNotEqual(result.returncode, 0, result.stdout + result.stderr)
            self.assertIn(expected_error, result.stderr)
            if installed_bytes is None:
                self.assertFalse(installed.exists())
            else:
                self.assertEqual(installed.read_bytes(), installed_bytes)
            self.assertEqual(new.read_bytes(), NEW_SENTINEL)
            self.assertEqual(unrelated.read_bytes(), UNRELATED_SENTINEL)

    def test_promotion_guard(self):
        with self.subTest(phase="equal_virtual_payloads_with_different_cmdlines_are_promoted"):
            with tempfile.TemporaryDirectory(prefix="tilo-boot-guard-success-") as temporary:
                root = Path(temporary)
                installed_bytes = pe_fixture(cmdline=b"tilo.mode=install", padding=b"I")
                variant_bytes = pe_fixture(cmdline=b"root=tilo-auto tilo.mode=normal", padding=b"V")
                installed, variant, new, unrelated = self.make_paths(
                    root, installed_bytes, variant_bytes
                )
                result = self.run_real_guard(installed, variant)
                self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
                self.assertEqual(installed.read_bytes(), variant_bytes)
                self.assertFalse(new.exists())
                self.assertEqual(unrelated.read_bytes(), UNRELATED_SENTINEL)
        with self.subTest(phase="virtual_payload_changes_are_refused_before_promotion"):
            for label, variant_bytes in (
                ("kernel", pe_fixture(kernel=b"X" + KERNEL[1:])),
                ("initrd", pe_fixture(initrd=b"X" + INITRD[1:])),
            ):
                with self.subTest(changed=label):
                    self.assert_refused_without_promotion(
                        pe_fixture(),
                        variant_bytes,
                        "target UKI changes the bootc kernel/initramfs identity",
                    )

    def test_guard_faults_are_refused(self):
        with self.subTest(phase="failed_real_observations_are_refused_before_promotion"):
            malformed = b"not a PE image"
            cases = (
                ("installed malformed", malformed, pe_fixture(),
                 "cannot read installed UKI boot identity"),
                ("variant malformed", pe_fixture(), malformed,
                 "cannot read target UKI boot identity"),
                ("installed absent", None, pe_fixture(),
                 "cannot read installed UKI boot identity"),
                ("variant absent", pe_fixture(), None,
                 "cannot read target UKI boot identity"),
                ("both helper reads fail", malformed, malformed,
                 "cannot read installed UKI boot identity"),
            )
            for label, installed, variant, expected_error in cases:
                with self.subTest(case=label):
                    self.assert_refused_without_promotion(installed, variant, expected_error)
        with self.subTest(phase="successful_malformed_digest_output_is_a_fault_and_is_refused"):
            faults = {
                "empty": "",
                "nonhex": "g" * 64,
                "wrong length": "a" * 63,
                "multiline": "a" * 64 + "\n" + "a" * 64,
            }
            for label, output in faults.items():
                with self.subTest(fault=label), tempfile.TemporaryDirectory(
                    prefix="tilo-boot-guard-fault-"
                ) as temporary:
                    installed, variant, new, unrelated = self.make_paths(
                        Path(temporary), DESTINATION_SENTINEL, b"variant sentinel"
                    )
                    result = self.run_fault_guard(installed, variant, output)
                    self.assertNotEqual(result.returncode, 0, result.stdout + result.stderr)
                    self.assertIn("UKI boot identity is not a SHA-256 digest", result.stderr)
                    self.assertEqual(installed.read_bytes(), DESTINATION_SENTINEL)
                    self.assertEqual(new.read_bytes(), NEW_SENTINEL)
                    self.assertEqual(unrelated.read_bytes(), UNRELATED_SENTINEL)
        with self.subTest(phase="extraction_fails_closed_when_the_guard_subject_changes"):
            source = INSTALLER.read_text()
            mutations = (
                "",
                source.replace(START_MARKER, "", 1),
                source.replace(END_MARKER, "", 1),
                source.replace(
                    '  mv -f "$TARGET_UKI.new" "$TARGET_UKI"',
                    '  unexpected_command\n  mv -f "$TARGET_UKI.new" "$TARGET_UKI"',
                    1,
                ),
                source.replace(" boot-digest \"$TARGET_VARIANT\"", " show \"$TARGET_VARIANT\"", 1),
            )
            for changed in mutations:
                with self.subTest(tail=changed[-80:]):
                    with self.assertRaises(ValueError):
                        guard_statements(changed)


if __name__ == "__main__":
    unittest.main()
