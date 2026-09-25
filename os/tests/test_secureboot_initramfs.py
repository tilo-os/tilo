"""Regression tests for the Secure Boot probe's production initramfs list."""

from __future__ import annotations

import hashlib
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest


HERE = Path(__file__).resolve().parent
HELPER = HERE / "secureboot_initramfs.py"
PROBE = HERE / "secureboot_vm_probe.sh"
STARTUP_FIXTURE = HERE / "fixtures" / "secureboot-startup.nsh"


class SecureBootProbeFixture:
    """Executable host doubles around the real production probe and parser."""

    def __init__(self, root: Path, *, arch: str = "aarch64"):
        self.root = root
        self.arch = arch
        self.bin = root / "bin"
        self.bin.mkdir(parents=True)
        self.work_root = root / "work"
        self.work_root.mkdir()
        self.calls = root / "calls"
        self.calls.mkdir()
        self.crypto_calls = self.calls / "crypto"
        self.stat_calls = self.calls / "stat"
        self.rpm_calls = self.calls / "rpm"
        self.vm_calls = self.calls / "vm.jsonl"
        self.capability_calls = self.calls / "capability"

        self.kernel = self.write("inputs/vmlinuz")
        self.app = self.write("inputs/app.efi")
        self.crypto = self.write(
            "inputs/tilo-crypto",
            """#!/bin/bash
set -eu
printf '%s\n' "$*" >> "$CRYPTO_CALLS"
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
        self.gen_cpio = self.write(
            "inputs/gen_init_cpio",
            """#!/bin/bash
set -eu
test "$#" -eq 3
test "$1" = -o
printf cpio > "$2"
""",
            executable=True,
        )
        self.loader = self.write("deps/lib/ld-linux-aarch64.so.1")
        self.libc = self.write("deps/lib64/libc.so.6")
        self.firmware = self.write("firmware/QEMU_EFI.qemuvars.fd", "firmware")
        self.template_contents = '{"variables":[]}\n'
        self.template = self.write(
            "firmware/vars.blank.json", self.template_contents
        )
        self.descriptor = self.root / "firmware/91-edk2-aarch64-qemuvars-sb.json"
        self.write_descriptor()
        self.x86_code = self.write("firmware/OVMF_CODE.secboot.fd", "code")
        self.x86_vars = self.write("firmware/OVMF_VARS.fd", "vars")
        self._install_tools()

        self.environment = os.environ.copy()
        self.environment.pop("BASH_ENV", None)
        self.environment.pop("ENV", None)
        self.environment.update(
            {
                "PATH": f"{self.bin}:{self.environment['PATH']}",
                "TILO_SECUREBOOT_VM_ARCH": arch,
                "TILO_KERNEL_PATH": str(self.kernel),
                "TILO_GEN_INIT_CPIO": str(self.gen_cpio),
                "TILO_CRYPTO_BIN": str(self.crypto),
                "TILO_SECUREBOOT_VM_APP": str(self.app),
                "TILO_SECUREBOOT_VM_WORK_ROOT": str(self.work_root),
                "CRYPTO_CALLS": str(self.crypto_calls),
                "STAT_CALLS": str(self.stat_calls),
                "RPM_CALLS": str(self.rpm_calls),
                "VM_CALLS": str(self.vm_calls),
                "CAPABILITY_CALLS": str(self.capability_calls),
                "FAKE_LOADER": str(self.loader),
                "FAKE_LIBC": str(self.libc),
                "FIRMWARE_DESCRIPTOR": str(self.descriptor),
                "FIRMWARE_CODE": str(self.firmware),
                "FIRMWARE_TEMPLATE": str(self.template),
                "STARTUP_FIXTURE": str(STARTUP_FIXTURE),
                "SIGNED_MODE": "app",
                "QEMU_CAPABILITY_MODE": "valid",
            }
        )
        if arch == "aarch64":
            self.environment["TILO_SECUREBOOT_VM_DESCRIPTOR"] = str(self.descriptor)
        else:
            self.environment["TILO_SECUREBOOT_VM_CODE"] = str(self.x86_code)
            self.environment["TILO_SECUREBOOT_VM_VARS"] = str(self.x86_vars)

    def write(
        self, relative: str, contents: str = "fixture", *, executable: bool = False
    ) -> Path:
        path = self.root / relative
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(contents)
        if executable:
            path.chmod(0o755)
        return path

    def descriptor_data(self) -> dict:
        return {
            "description": "UEFI firmware for ARM64 virtual machines, SB disabled",
            "interface-types": ["uefi"],
            "mapping": {
                "device": "memory",
                "filename": str(self.firmware),
                "uefi-vars": {"template": str(self.template)},
            },
            "targets": [{"architecture": "aarch64", "machines": ["virt-*"]}],
            "features": ["secure-boot", "host-uefi-vars"],
        }

    def write_descriptor(self, data: object | None = None, *, raw: str | None = None):
        self.descriptor.parent.mkdir(parents=True, exist_ok=True)
        if raw is None:
            raw = json.dumps(self.descriptor_data() if data is None else data)
        self.descriptor.write_text(raw)

    def _install_tools(self):
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
printf 'libc.so.6 => %s (0x00000001)\n' "$FAKE_LIBC"
printf '%s (0x00000002)\n' "$FAKE_LOADER"
""",
            executable=True,
        )
        self.write(
            "bin/timeout",
            """#!/bin/bash
set -eu
shift
exec "$@"
""",
            executable=True,
        )
        self.write(
            "bin/mkfs.vfat",
            """#!/bin/bash
set -eu
test "$#" -eq 3
test "$1" = -n
label=$2
image=$3
test -f "$image"
LC_ALL=C
export LC_ALL
if [ "${#label}" -gt 11 ]; then
  echo "fixture mkfs.vfat label exceeds FAT limit: $label" >&2
  exit 64
fi
""",
            executable=True,
        )
        for tool in ("mmd", "mcopy"):
            self.write(f"bin/{tool}", "#!/bin/bash\nexit 0\n", executable=True)
        self.write(
            "bin/truncate",
            """#!/bin/bash
set -eu
test "$1" = -s
: > "$3"
""",
            executable=True,
        )
        self.write(
            "bin/rpm",
            """#!/bin/bash
set -eu
printf '%s\n' "$*" >> "$RPM_CALLS"
case "$1" in
  -qf)
    test "$#" -eq 5
    test "$2" = --qf
    test "$3" = '%{NEVRA}'
    test "$4" = --
    path=$5
    case "$path" in
      "$FIRMWARE_DESCRIPTOR"|"$FIRMWARE_CODE"|"$FIRMWARE_TEMPLATE") ;;
      *) echo "unexpected RPM fixture path: $path" >&2; exit 92 ;;
    esac
    if [ "${RPM_UNOWNED_PATH:-}" = "$path" ]; then
      echo "file $path is not owned by any package" >&2
      exit 1
    fi
    if [ "${RPM_DIFFERENT_PATH:-}" = "$path" ]; then
      printf '%s\n' other-package-2.0-1.noarch
    else
      printf '%s\n' edk2-aarch64-1.0-1.noarch
    fi
    ;;
  -V)
    test "$#" -eq 2
    if [ "${RPM_VERIFY_FAIL:-0}" = 1 ]; then
      echo 'package verification failed' >&2
      exit 1
    fi
    ;;
  *) exit 91 ;;
esac
""",
            executable=True,
        )
        self.write(
            "bin/stat",
            """#!/bin/bash
set -eu
test "$#" -eq 4
test "$1" = -c
test "$2" = '%u:%a'
test "$3" = --
path=$4
case "$path" in
  "$FIRMWARE_DESCRIPTOR"|"$FIRMWARE_CODE"|"$FIRMWARE_TEMPLATE") ;;
  *) echo "unexpected stat fixture path: $path" >&2; exit 92 ;;
esac
printf '%s\n' "$path" >> "$STAT_CALLS"
if [ "${STAT_FAIL_PATH:-}" = "$path" ]; then exit 7; fi
if [ "${STAT_MALFORMED_PATH:-}" = "$path" ]; then
  printf '%b' "${STAT_MALFORMED_VALUE:-0:644 extra\\n}"
elif [ "${STAT_NONROOT_PATH:-}" = "$path" ]; then
  printf '%s\n' '501:644'
elif [ "${STAT_MUTABLE_PATH:-}" = "$path" ]; then
  printf '%s\n' '0:664'
else
  printf '%s\n' '0:644'
fi
""",
            executable=True,
        )
        qemu = self.write(
            "bin/qemu-system-aarch64",
            r"""#!/usr/bin/env python3
import hashlib
import json
import os
from pathlib import Path
import sys

args = sys.argv[1:]
if args == ["-device", "uefi-vars-sysbus,help"]:
    Path(os.environ["CAPABILITY_CALLS"]).write_text("called\n")
    mode = os.environ.get("QEMU_CAPABILITY_MODE", "valid")
    if mode == "fail":
        raise SystemExit(7)
    if mode == "missing-device":
        print("virtio-device options:")
    elif mode == "no-json":
        print("uefi-vars-sysbus options:\n  size=<size>")
    else:
        print("uefi-vars-sysbus options:\n  jsonfile=<str>")
    raise SystemExit(0)

def option_value(flag):
    try:
        return args[args.index(flag) + 1]
    except (ValueError, IndexError):
        return None

store = None
for index, arg in enumerate(args):
    if arg == "-device" and index + 1 < len(args):
        device = args[index + 1]
        if device.startswith("uefi-vars-sysbus,jsonfile="):
            store = device.split("=", 1)[1]
for index, arg in enumerate(args):
    if arg == "-drive" and index + 1 < len(args):
        drive = args[index + 1]
        if "if=pflash" in drive and "readonly=on" not in drive:
            for field in drive.split(","):
                if field.startswith("file="):
                    store = field.split("=", 1)[1]

if store is None:
    print("no variable store in VM invocation", file=sys.stderr)
    raise SystemExit(92)
store_path = Path(store)
data = store_path.read_bytes()
disk = next(
    (
        args[index + 1]
        for index, arg in enumerate(args)
        if arg == "-drive" and index + 1 < len(args) and "if=virtio" in args[index + 1]
    ),
    "",
)
stage = "enrollment" if "-kernel" in args else ("unsigned" if "unsigned" in disk else "signed")
record = {
    "argv": args,
    "stage": stage,
    "variable_store": store,
    "inode": store_path.stat().st_ino,
    "sha256": hashlib.sha256(data).hexdigest(),
    "data": data.decode(errors="replace"),
}
with Path(os.environ["VM_CALLS"]).open("a") as output:
    output.write(json.dumps(record) + "\n")
if stage == "enrollment":
    if store_path.suffix == ".json":
        store_path.write_text('{"variables":[{"name":"PK"}]}\n')
    print("TILO_SECURE_BOOT_ENROLL_PASS")
    print("TILO_SECURE_BOOT_SETUP_MODE=1")
    print("TILO_SECURE_BOOT_SECURE_BOOT=0")
elif stage == "unsigned":
    print("Access Denied -- rejected probably by Secure Boot")
elif os.environ.get("SIGNED_MODE", "app") == "app":
    print("TILO_SECURE_BOOT_SIGNED_EFI_PASS")
else:
    for line in Path(os.environ["STARTUP_FIXTURE"]).read_text().splitlines():
        if line.startswith("echo "):
            print(line.removeprefix("echo "))
""",
            executable=True,
        )
        (self.bin / "qemu-system-x86_64").write_bytes(qemu.read_bytes())
        (self.bin / "qemu-system-x86_64").chmod(0o755)

    def run(self, **environment_overrides: str) -> subprocess.CompletedProcess[str]:
        environment = self.environment.copy()
        environment.update(environment_overrides)
        return subprocess.run(
            ["/bin/bash", str(PROBE)],
            env=environment,
            capture_output=True,
            text=True,
            timeout=15,
        )

    def records(self) -> list[dict]:
        if not self.vm_calls.exists():
            return []
        return [json.loads(line) for line in self.vm_calls.read_text().splitlines()]

    @staticmethod
    def sha256(path: Path) -> str:
        return hashlib.sha256(path.read_bytes()).hexdigest()


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
        self.root = Path(temporary.name).resolve()
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
if [ "$#" -eq 2 ] && [ "$1" = -device ] && [ "$2" = uefi-vars-sysbus,help ]; then
  printf '%s\n' 'uefi-vars-sysbus options:' '  jsonfile=<str>'
  exit 0
fi
: > "$QEMU_CALLED"
exit 99
""",
            executable=True,
        )
        firmware = self.write("firmware/QEMU_EFI.qemuvars.fd")
        template = self.write("firmware/vars.blank.json", '{"variables":[]}\n')
        descriptor = self.write(
            "firmware/91-edk2-aarch64-qemuvars-sb.json",
            json.dumps(
                {
                    "interface-types": ["uefi"],
                    "mapping": {
                        "device": "memory",
                        "filename": str(firmware),
                        "uefi-vars": {"template": str(template)},
                    },
                    "targets": [
                        {"architecture": "aarch64", "machines": ["virt-*"]}
                    ],
                    "features": ["secure-boot", "host-uefi-vars"],
                }
            ),
        )
        self.write(
            "bin/rpm",
            """#!/bin/bash
set -eu
case "$1" in
  -qf) printf '%s\n' edk2-aarch64-1.0-1.noarch ;;
  -V) exit 0 ;;
  *) exit 91 ;;
esac
""",
            executable=True,
        )
        self.write(
            "bin/stat",
            """#!/bin/bash
set -eu
test "$#" -eq 4
test "$1" = -c
test "$2" = '%u:%a'
test "$3" = --
printf '%s\n' '0:644'
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
                "TILO_SECUREBOOT_VM_DESCRIPTOR": str(descriptor),
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


class SecureBootProbeFirmwareInterfaceTests(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory(prefix="tilo-firmware-interface-")
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name).resolve()
        self.case_number = 0

    def fixture(self, *, arch: str = "aarch64") -> SecureBootProbeFixture:
        self.case_number += 1
        case_root = self.root / f"case-{self.case_number}"
        case_root.mkdir()
        return SecureBootProbeFixture(case_root, arch=arch)

    @staticmethod
    def drive_arguments(record: dict) -> list[str]:
        args = record["argv"]
        return [args[index + 1] for index, arg in enumerate(args) if arg == "-drive"]

    def test_aarch64_uses_qualified_qemuvars_with_isolated_private_stores(self):
        fixture = self.fixture()
        template_hash = fixture.sha256(fixture.template)

        result = fixture.run()

        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertIn("secureboot-vm-probe=PASS", result.stdout)
        self.assertEqual(
            fixture.stat_calls.read_text().splitlines(),
            [str(fixture.descriptor), str(fixture.firmware), str(fixture.template)],
        )
        self.assertEqual(fixture.sha256(fixture.template), template_hash)
        self.assertEqual(fixture.template.read_text(), fixture.template_contents)
        crypto_calls = fixture.crypto_calls.read_text().splitlines()
        self.assertEqual(
            sum(call.startswith("generate-secure-boot-cert ") for call in crypto_calls),
            3,
        )

        records = fixture.records()
        self.assertEqual([record["stage"] for record in records], ["enrollment", "unsigned", "signed"])
        stores = [Path(record["variable_store"]) for record in records]
        self.assertEqual(
            [store.name for store in stores],
            ["vars.enrollment.json", "vars.unsigned.json", "vars.signed.json"],
        )
        self.assertEqual(len(set(stores)), 3)
        self.assertNotIn(fixture.template, stores)
        self.assertEqual(len({record["inode"] for record in records}), 3)
        self.assertEqual(records[1]["data"], '{"variables":[{"name":"PK"}]}\n')
        self.assertEqual(records[2]["data"], records[1]["data"])
        self.assertEqual(records[1]["sha256"], records[2]["sha256"])

        for record, store in zip(records, stores):
            args = record["argv"]
            self.assertIn("-bios", args)
            self.assertEqual(args[args.index("-bios") + 1], str(fixture.firmware))
            self.assertIn("-device", args)
            self.assertIn(
                f"uefi-vars-sysbus,jsonfile={store}",
                args,
            )
            self.assertFalse(
                any("if=pflash" in drive for drive in self.drive_arguments(record)),
                args,
            )
            self.assertNotIn(
                f"uefi-vars-sysbus,jsonfile={fixture.template}",
                args,
            )
        self.assertIn(f"aarch64-firmware-descriptor={fixture.descriptor}", result.stdout)
        self.assertIn(f"aarch64-firmware-code={fixture.firmware}", result.stdout)
        self.assertIn(f"aarch64-firmware-template={fixture.template}", result.stdout)

    def test_x86_64_preserves_two_pflash_drives(self):
        fixture = self.fixture(arch="x86_64")

        result = fixture.run()

        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        records = fixture.records()
        self.assertEqual([record["stage"] for record in records], ["enrollment", "unsigned", "signed"])
        for record in records:
            drives = self.drive_arguments(record)
            pflash = [drive for drive in drives if "if=pflash" in drive]
            self.assertEqual(len(pflash), 2, record["argv"])
            self.assertTrue(any("readonly=on" in drive for drive in pflash))
            self.assertFalse(any("uefi-vars" in arg for arg in record["argv"]))
            self.assertNotIn("-bios", record["argv"])

    def test_aarch64_refuses_unqualified_descriptors_inputs_and_capabilities(self):
        cases = [
            ("malformed descriptor", "malformed AArch64 firmware descriptor"),
            ("oversized descriptor", "exceeds 65536 bytes"),
            ("non-object descriptor", "descriptor root must be an object"),
            ("flash mapping", "mapping.device must be memory"),
            ("relative firmware", "firmware path must be absolute and canonical"),
            ("noncanonical firmware", "firmware path must be absolute and canonical"),
            ("missing firmware", "missing AArch64 Secure Boot firmware"),
            ("symlinked firmware", "symlinked AArch64 Secure Boot firmware"),
            ("non-regular firmware", "non-regular AArch64 Secure Boot firmware"),
            ("wrong target", "descriptor lacks aarch64 virt-* target"),
            ("missing uefi", "descriptor lacks uefi interface"),
            ("missing secure boot", "descriptor lacks secure-boot feature"),
            ("missing host variables", "descriptor lacks host-uefi-vars feature"),
            ("enrolled keys", "descriptor must not use enrolled-keys"),
            ("absent template", "descriptor lacks mapping.uefi-vars.template"),
            ("path alias", "firmware and variable template must differ"),
            ("malformed template", "malformed AArch64 variable template"),
            ("oversized template", "variable template exceeds 65536 bytes"),
            ("different package", "firmware inputs do not share one RPM owner"),
            ("unowned input", "firmware input is not owned by an RPM package"),
            ("package verification", "firmware RPM verification failed"),
            ("metadata failure", "stat metadata query failed"),
            ("malformed metadata", "malformed stat metadata"),
            ("multiple metadata records", "malformed stat metadata"),
            ("trailing metadata record", "malformed stat metadata"),
            ("non-root input", "firmware input is not root-owned"),
            ("mutable input", "firmware input is group/world-writable"),
            ("missing device", "qemu-system-aarch64 lacks uefi-vars-sysbus"),
            ("failed device help", "qemu-system-aarch64 device help failed"),
            ("missing jsonfile", "uefi-vars-sysbus lacks jsonfile property"),
            ("legacy code override", "TILO_SECUREBOOT_VM_CODE is unsupported on aarch64"),
            ("legacy vars override", "TILO_SECUREBOOT_VM_VARS is unsupported on aarch64"),
        ]
        lstat_cases = {"missing firmware", "symlinked firmware", "non-regular firmware"}

        for name, expected_error in cases:
            with self.subTest(name=name):
                fixture = self.fixture()
                overrides: dict[str, str] = {}
                data = fixture.descriptor_data()
                if name == "malformed descriptor":
                    fixture.write_descriptor(raw="{not-json")
                elif name == "oversized descriptor":
                    fixture.write_descriptor(raw=" " * (64 * 1024 + 1))
                elif name == "non-object descriptor":
                    fixture.write_descriptor(data=[])
                elif name == "flash mapping":
                    data["mapping"]["device"] = "flash"
                    fixture.write_descriptor(data)
                elif name == "relative firmware":
                    data["mapping"]["filename"] = "QEMU_EFI.qemuvars.fd"
                    fixture.write_descriptor(data)
                elif name == "noncanonical firmware":
                    data["mapping"]["filename"] = str(
                        fixture.firmware.parent / ".." / "firmware" / fixture.firmware.name
                    )
                    fixture.write_descriptor(data)
                elif name == "missing firmware":
                    fixture.firmware.unlink()
                elif name == "symlinked firmware":
                    real_firmware = fixture.firmware.with_suffix(".real")
                    fixture.firmware.rename(real_firmware)
                    fixture.firmware.symlink_to(real_firmware)
                elif name == "non-regular firmware":
                    fixture.firmware.unlink()
                    fixture.firmware.mkdir()
                elif name == "wrong target":
                    data["targets"] = [{"architecture": "x86_64", "machines": ["virt-*"]}]
                    fixture.write_descriptor(data)
                elif name == "missing uefi":
                    data["interface-types"] = []
                    fixture.write_descriptor(data)
                elif name == "missing secure boot":
                    data["features"].remove("secure-boot")
                    fixture.write_descriptor(data)
                elif name == "missing host variables":
                    data["features"].remove("host-uefi-vars")
                    fixture.write_descriptor(data)
                elif name == "enrolled keys":
                    data["features"].append("enrolled-keys")
                    fixture.write_descriptor(data)
                elif name == "absent template":
                    del data["mapping"]["uefi-vars"]["template"]
                    fixture.write_descriptor(data)
                elif name == "path alias":
                    data["mapping"]["uefi-vars"]["template"] = str(fixture.firmware)
                    fixture.write_descriptor(data)
                elif name == "malformed template":
                    fixture.template.write_text("not-json")
                elif name == "oversized template":
                    fixture.template.write_text(" " * (64 * 1024 + 1))
                elif name == "different package":
                    overrides["RPM_DIFFERENT_PATH"] = str(fixture.template)
                elif name == "unowned input":
                    overrides["RPM_UNOWNED_PATH"] = str(fixture.firmware)
                elif name == "package verification":
                    overrides["RPM_VERIFY_FAIL"] = "1"
                elif name == "metadata failure":
                    overrides["STAT_FAIL_PATH"] = str(fixture.firmware)
                elif name == "malformed metadata":
                    overrides["STAT_MALFORMED_PATH"] = str(fixture.firmware)
                    overrides["STAT_MALFORMED_VALUE"] = "root:644\n"
                elif name == "multiple metadata records":
                    overrides["STAT_MALFORMED_PATH"] = str(fixture.firmware)
                    overrides["STAT_MALFORMED_VALUE"] = "0:644\n0:644\n"
                elif name == "trailing metadata record":
                    overrides["STAT_MALFORMED_PATH"] = str(fixture.firmware)
                    overrides["STAT_MALFORMED_VALUE"] = "0:644\n\n"
                elif name == "non-root input":
                    overrides["STAT_NONROOT_PATH"] = str(fixture.firmware)
                elif name == "mutable input":
                    overrides["STAT_MUTABLE_PATH"] = str(fixture.firmware)
                elif name == "missing device":
                    overrides["QEMU_CAPABILITY_MODE"] = "missing-device"
                elif name == "failed device help":
                    overrides["QEMU_CAPABILITY_MODE"] = "fail"
                elif name == "missing jsonfile":
                    overrides["QEMU_CAPABILITY_MODE"] = "no-json"
                elif name == "legacy code override":
                    overrides["TILO_SECUREBOOT_VM_CODE"] = str(fixture.firmware)
                elif name == "legacy vars override":
                    overrides["TILO_SECUREBOOT_VM_VARS"] = str(fixture.template)

                result = fixture.run(**overrides)
                evidence = result.stdout + result.stderr
                self.assertNotEqual(result.returncode, 0, evidence)
                self.assertIn(expected_error, evidence)
                self.assertFalse(fixture.crypto_calls.exists(), "key generation was reached")
                self.assertEqual(fixture.records(), [], "a VM invocation was reached")
                if name in lstat_cases and fixture.stat_calls.exists():
                    self.assertNotIn(
                        str(fixture.firmware), fixture.stat_calls.read_text().splitlines()
                    )

    def test_shell_only_signed_boot_cannot_supply_application_marker(self):
        fixture = self.fixture()

        result = fixture.run(SIGNED_MODE="shell")

        self.assertNotEqual(result.returncode, 0, result.stdout + result.stderr)
        records = fixture.records()
        self.assertEqual([record["stage"] for record in records], ["enrollment", "unsigned", "signed"])
        self.assertTrue(fixture.crypto_calls.exists())
        self.assertTrue(fixture.capability_calls.exists())
        self.assertEqual(
            fixture.stat_calls.read_text().splitlines(),
            [str(fixture.descriptor), str(fixture.firmware), str(fixture.template)],
        )
        self.assertIn("Access Denied -- rejected probably by Secure Boot", result.stdout)
        self.assertNotIn("secureboot-vm-probe=PASS", result.stdout)


if __name__ == "__main__":
    unittest.main()
