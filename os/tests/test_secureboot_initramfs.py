"""Regression tests for the Secure Boot probe's production initramfs list."""

from __future__ import annotations

import os
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest


HERE = Path(__file__).resolve().parent
HELPER = HERE / "secureboot_initramfs.py"
PROBE = HERE / "secureboot_vm_probe.sh"


class SecureBootInitramfsTests(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory(prefix="tilo-secureboot-initramfs-")
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name)
        self.work = self.root / "work"
        self.work.mkdir()
        for name in (
            "init",
            "tilo-crypto",
            "PK.esl",
            "KEK.esl",
            "db.esl",
            "PK.key",
            "PK.crt",
        ):
            (self.work / name).write_bytes(name.encode())

    def dependency(self, relative: str) -> Path:
        source = self.root / "dependencies" / relative
        source.parent.mkdir(parents=True, exist_ok=True)
        source.write_bytes(relative.encode())
        return source

    @staticmethod
    def ldd_output(loader: Path, libraries: list[tuple[str, Path]]) -> str:
        lines = ["linux-vdso.so.1 (0x0000ffff00000000)"]
        lines.extend(
            f"{name} => {path} (0x0000ffff10000000)"
            for name, path in libraries
        )
        lines.append(f"{loader} (0x0000ffff20000000)")
        return "\n".join(lines) + "\n"

    def run_helper(
        self,
        init_ldd: str,
        crypto_ldd: str,
        *,
        ldd_status: int = 0,
        include_ldd: bool = True,
    ):
        output = self.root / "initramfs.list"
        output.unlink(missing_ok=True)
        tools = self.root / "tools"
        tools.mkdir(exist_ok=True)
        ldd = tools / "ldd"
        if include_ldd:
            ldd.write_text(
                """#!/bin/bash
set -eu
if [ "$1" = "$INIT_BINARY" ]; then
  printf '%s' "$INIT_LDD"
elif [ "$1" = "$CRYPTO_BINARY" ]; then
  printf '%s' "$CRYPTO_LDD"
else
  echo "unexpected ldd input: $1" >&2
  exit 91
fi
if [ "$LDD_STATUS" -ne 0 ]; then
  echo 'fixture ldd failure' >&2
  exit "$LDD_STATUS"
fi
"""
            )
            ldd.chmod(0o755)
        else:
            ldd.unlink(missing_ok=True)
        environment = os.environ.copy()
        environment.update(
            {
                "PATH": str(tools),
                "INIT_BINARY": str(self.work / "init"),
                "CRYPTO_BINARY": str(self.work / "tilo-crypto"),
                "INIT_LDD": init_ldd,
                "CRYPTO_LDD": crypto_ldd,
                "LDD_STATUS": str(ldd_status),
            }
        )
        command = [
            sys.executable,
            str(HELPER),
            str(output),
            "--init",
            str(self.work / "init"),
            "--crypto",
            str(self.work / "tilo-crypto"),
            "--material-dir",
            str(self.work),
        ]
        result = subprocess.run(
            command,
            env=environment,
            capture_output=True,
            text=True,
            timeout=10,
        )
        return result, output

    def test_arm64_loader_parent_is_emitted_once_before_dependency_files(self):
        loader = self.dependency("arm/lib/ld-linux-aarch64.so.1")
        libc = self.dependency("arm/lib/aarch64-linux-gnu/libc.so.6")
        libgcc = self.dependency("arm/lib/aarch64-linux-gnu/libgcc_s.so.1")
        libm = self.dependency("arm/lib/aarch64-linux-gnu/libm.so.6")
        init_ldd = self.ldd_output(loader, [("libc.so.6", libc)])
        crypto_ldd = self.ldd_output(
            loader,
            [("libgcc_s.so.1", libgcc), ("libm.so.6", libm), ("libc.so.6", libc)],
        )
        result, output = self.run_helper(init_ldd, crypto_ldd)
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        lines = output.read_text().splitlines()
        lib_dir = f"dir {loader.parent} 0755 0 0"
        nested_dir = f"dir {libc.parent} 0755 0 0"
        self.assertEqual(lines.count(lib_dir), 1)
        self.assertEqual(lines.count(nested_dir), 1)
        lib_index = lines.index(lib_dir)
        nested_index = lines.index(nested_dir)
        self.assertLess(lib_index, nested_index)
        for dependency in (loader, libc, libgcc, libm):
            file_index = lines.index(f"file {dependency} {dependency} 0755 0 0")
            self.assertLess(lib_index, file_index)
            if dependency.parent == libc.parent:
                self.assertLess(nested_index, file_index)
        self.assertEqual(lines.count(f"file {loader} {loader} 0755 0 0"), 1)
        self.assertEqual(lines.count(f"file {libc} {libc} 0755 0 0"), 1)

    def test_x86_64_lib64_parent_is_emitted_before_dependency_files(self):
        loader = self.dependency("x86/lib64/ld-linux-x86-64.so.2")
        libc = self.dependency("x86/lib64/libc.so.6")
        libgcc = self.dependency("x86/lib64/libgcc_s.so.1")
        init_ldd = self.ldd_output(loader, [("libc.so.6", libc)])
        crypto_ldd = self.ldd_output(
            loader, [("libgcc_s.so.1", libgcc), ("libc.so.6", libc)]
        )
        result, output = self.run_helper(init_ldd, crypto_ldd)
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        lines = output.read_text().splitlines()
        directory = f"dir {loader.parent} 0755 0 0"
        self.assertEqual(lines.count(directory), 1)
        directory_index = lines.index(directory)
        for dependency in (loader, libc, libgcc):
            self.assertLess(
                directory_index,
                lines.index(f"file {dependency} {dependency} 0755 0 0"),
            )

    def test_malformed_missing_and_duplicate_dependency_evidence_is_rejected(self):
        loader = self.dependency("valid/lib/ld-linux-aarch64.so.1")
        libc = self.dependency("valid/lib64/libc.so.6")
        other_libc = self.dependency("other/lib64/libc.so.6")
        valid = self.ldd_output(loader, [("libc.so.6", libc)])
        cases = {
            "relative destination": (
                "libbad.so => relative/path (0x1)\n",
                "malformed tilo-crypto ldd output",
            ),
            "double-root destination": (
                "libbad.so => //lib/bad.so (0x1)\n",
                "invalid absolute tilo-crypto dependency path",
            ),
            "parent traversal": (
                f"libbad.so => {self.root}/valid/../bad.so (0x1)\n",
                "invalid absolute tilo-crypto dependency path",
            ),
            "whitespace destination": (
                f"libbad.so => {self.root}/bad path.so (0x1)\n",
                "malformed tilo-crypto ldd output",
            ),
            "missing source": (
                self.ldd_output(
                    loader, [("libbad.so", self.root / "missing.so")]
                ),
                "missing or invalid absolute",
            ),
            "unknown line": (
                "this is not ldd output\n",
                "malformed tilo-crypto ldd output",
            ),
            "unresolved": (
                "libbad.so => not found\n",
                "unresolved tilo-crypto dependency",
            ),
            "malformed address": (
                f"libbad.so => {libc} (address)\n",
                "malformed tilo-crypto ldd output",
            ),
            "ambiguous soname": (
                self.ldd_output(loader, [("libc.so.6", other_libc)]),
                "ambiguous runtime dependency",
            ),
            "zero resolution": (
                "linux-vdso.so.1 (0x0000ffff00000000)\n",
                "resolved no shared libraries",
            ),
            "fixed-directory conflict": (
                self.ldd_output(loader, [("libsys.so", Path("/sys"))]),
                "both a file and directory: /sys",
            ),
        }
        for name, (crypto_ldd, expected_error) in cases.items():
            with self.subTest(name=name):
                result, output = self.run_helper(valid, crypto_ldd)
                self.assertNotEqual(result.returncode, 0)
                self.assertIn(expected_error, result.stderr)
                self.assertFalse(output.exists())

    def test_missing_required_dependency_is_not_silently_skipped(self):
        result, output = self.run_helper("", "", include_ldd=False)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("required ldd utility not found on PATH", result.stderr)
        self.assertFalse(output.exists())

    def test_ldd_failure_and_output_caps_fail_closed(self):
        loader = self.dependency("caps/lib/ld-linux-aarch64.so.1")
        libc = self.dependency("caps/lib64/libc.so.6")
        valid = self.ldd_output(loader, [("libc.so.6", libc)])
        cases = {
            "nonzero": (valid, 7),
            "line cap": ("linux-vdso.so.1 (0x1)\n" * 129, 0),
            "byte cap": ("x" * (64 * 1024 + 1), 0),
        }
        for name, (crypto_ldd, status) in cases.items():
            with self.subTest(name=name):
                result, output = self.run_helper(
                    valid, crypto_ldd, ldd_status=status
                )
                self.assertNotEqual(result.returncode, 0)
                self.assertFalse(output.exists())

    def test_discovered_destination_conflicts_and_closure_cap_are_rejected(self):
        loader = self.dependency("conflicts/lib/ld-linux-aarch64.so.1")
        libc = self.dependency("conflicts/lib64/libc.so.6")
        valid = self.ldd_output(loader, [("libc.so.6", libc)])
        parent = self.dependency("conflicts/tree/library.so")
        child = Path(str(parent) + "/child.so")
        same_destination = self.ldd_output(
            loader, [("libone.so", parent), ("libtwo.so", parent)]
        )
        ancestor_collision = self.ldd_output(
            loader, [("libparent.so", parent), ("libchild.so", child)]
        )
        many_init = self.ldd_output(
            loader,
            [
                (f"libinit{index}.so", self.root / "many" / f"init-{index}.so")
                for index in range(64)
            ],
        )
        other_loader = self.dependency("many/lib/ld-linux-aarch64.so.1")
        many_crypto = self.ldd_output(
            other_loader,
            [
                (f"libcrypto{index}.so", self.root / "many" / f"crypto-{index}.so")
                for index in range(64)
            ],
        )
        cases = (
            (valid, same_destination, "destination collision"),
            (valid, ancestor_collision, "both a file and directory"),
            (many_init, many_crypto, "runtime closure exceeds 128 unique files"),
        )
        for init_ldd, crypto_ldd, expected_error in cases:
            with self.subTest(expected_error=expected_error):
                result, output = self.run_helper(init_ldd, crypto_ldd)
                self.assertNotEqual(result.returncode, 0)
                self.assertIn(expected_error, result.stderr)
                self.assertFalse(output.exists())


class SecureBootProbeClosureIntegrationTests(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory(prefix="tilo-secureboot-probe-test-")
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name)
        self.bin = self.root / "bin"
        self.bin.mkdir()

    def write(self, relative: str, contents: str = "fixture", *, executable=False) -> Path:
        path = self.root / relative
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(contents)
        if executable:
            path.chmod(0o755)
        return path

    def test_production_probe_includes_complete_init_and_crypto_runtime_closure(self):
        loader = self.write("deps/lib/ld-linux-aarch64.so.1")
        libc = self.write("deps/lib64/libc.so.6")
        libgcc = self.write("deps/lib64/libgcc_s.so.1")
        libm = self.write("deps/lib64/libm.so.6")
        calls = self.root / "ldd.calls"
        capture = self.root / "captured-initramfs.list"
        qemu_called = self.root / "qemu-called"

        crypto = self.write(
            "tilo-crypto",
            """#!/bin/bash
set -eu
case "$1" in
  generate-secure-boot-cert) : > "$3"; : > "$4" ;;
  certificate-esl) : > "$3" ;;
  sign-efi) /bin/cp "$4" "$5" ;;
  verify-efi) ;;
  *) exit 90 ;;
esac
""",
            executable=True,
        )
        self.write(
            "bin/gcc",
            """#!/bin/bash
set -eu
out=
while [ "$#" -gt 0 ]; do
  if [ "$1" = -o ]; then out=$2; shift 2; else shift; fi
done
test -n "$out"
printf init > "$out"
chmod 0755 "$out"
""",
            executable=True,
        )
        self.write(
            "bin/ldd",
            """#!/bin/bash
set -eu
printf '%s\n' "$1" >> "$LDD_CALLS"
printf '%s\n' 'linux-vdso.so.1 (0x0000ffff00000000)'
case "$1" in
  "$EXPECTED_CRYPTO")
    printf 'libgcc_s.so.1 => %s (0x0000ffff10000000)\n' "$FAKE_LIBGCC"
    printf 'libm.so.6 => %s (0x0000ffff20000000)\n' "$FAKE_LIBM"
    printf 'libc.so.6 => %s (0x0000ffff30000000)\n' "$FAKE_LIBC"
    ;;
  */init)
    printf 'libc.so.6 => %s (0x0000ffff30000000)\n' "$FAKE_LIBC"
    ;;
  *) echo "unexpected ldd input: $1" >&2; exit 91 ;;
esac
printf '%s (0x0000ffff40000000)\n' "$FAKE_LOADER"
""",
            executable=True,
        )
        gen_cpio = self.write(
            "gen_init_cpio",
            """#!/bin/bash
set -eu
test "$#" -eq 3
test "$1" = -o
/bin/cp "$3" "$CAPTURE_LIST"
exit 42
""",
            executable=True,
        )
        self.write(
            "bin/qemu-system-aarch64",
            """#!/bin/bash
: > "$QEMU_CALLED"
exit 99
""",
            executable=True,
        )

        environment = os.environ.copy()
        environment.pop("BASH_ENV", None)
        environment.pop("ENV", None)
        environment.update(
            {
                "PATH": f"{self.bin}:{environment['PATH']}",
                "TILO_SECUREBOOT_VM_ARCH": "aarch64",
                "TILO_KERNEL_PATH": str(self.write("vmlinuz")),
                "TILO_GEN_INIT_CPIO": str(gen_cpio),
                "TILO_CRYPTO_BIN": str(crypto),
                "TILO_SECUREBOOT_VM_CODE": str(self.write("firmware-code")),
                "TILO_SECUREBOOT_VM_VARS": str(self.write("firmware-vars")),
                "TILO_SECUREBOOT_VM_APP": str(self.write("app.efi")),
                "TILO_SECUREBOOT_VM_WORK_ROOT": str(self.root / "work"),
                "EXPECTED_CRYPTO": str(crypto),
                "FAKE_LOADER": str(loader),
                "FAKE_LIBC": str(libc),
                "FAKE_LIBGCC": str(libgcc),
                "FAKE_LIBM": str(libm),
                "LDD_CALLS": str(calls),
                "CAPTURE_LIST": str(capture),
                "QEMU_CALLED": str(qemu_called),
            }
        )
        result = subprocess.run(
            ["/bin/bash", str(PROBE)],
            env=environment,
            capture_output=True,
            text=True,
            timeout=10,
        )

        self.assertEqual(result.returncode, 42, result.stdout + result.stderr)
        self.assertFalse(qemu_called.exists(), "probe continued into QEMU")
        invoked = calls.read_text().splitlines()
        self.assertIn(str(crypto), invoked)
        self.assertTrue(any(path.endswith("/init") for path in invoked), invoked)
        lines = capture.read_text().splitlines()
        expected_libm = f"file {libm} {libm} 0755 0 0"
        self.assertIn(expected_libm, lines)
        self.assertEqual(lines.count(f"file {loader} {loader} 0755 0 0"), 1)
        self.assertEqual(lines.count(f"file {libc} {libc} 0755 0 0"), 1)
        lib64_dir = f"dir {libm.parent} 0755 0 0"
        self.assertLess(lines.index(lib64_dir), lines.index(expected_libm))


if __name__ == "__main__":
    unittest.main()
