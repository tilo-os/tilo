#!/usr/bin/env python3
"""Test Tilo's native FEX supervisor in a disposable Linux container.

Provide an init process, native FEX, a verified guest rootfs at /opt/tilo/x86,
and Wine's guest resource directories at their native paths. Keep networking
disabled, capabilities dropped, and the normal container seccomp filter.
No externally started FEXServer is used: Tilo must manage each invocation.
Provide a writable, empty /run/tilo mount for the fixed private prefix. The
verified guest rootfs must contain the separate upstream DXVK payload and its
build metadata. Completed test prefixes remain in this disposable container.
"""

import argparse
import hashlib
import json
import os
from pathlib import Path
import subprocess
import tempfile


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--helper", required=True, type=Path)
    parser.add_argument("--output-dir", required=True, type=Path)
    args = parser.parse_args()
    helper = args.helper.resolve(strict=True)
    args.output_dir.mkdir(parents=True, exist_ok=True)
    if any(args.output_dir.iterdir()):
        parser.error("output directory must be empty")
    results = []
    prefix = Path("/run/tilo/capsule/prefix")
    prefix.parent.mkdir(parents=True, exist_ok=True)
    for name in ("prefix", "prefix-win32", "prefix-win64"):
        if (prefix.parent / name).exists() or (prefix.parent / name).is_symlink():
            parser.error("disposable capsule directory must not contain a test prefix")
    for bits in (64, 32):
        prefix.mkdir()
        (prefix / "drive_c").mkdir()
        (prefix / "dosdevices").mkdir()
        (prefix / "dosdevices/c:").symlink_to("../drive_c", target_is_directory=True)
        with tempfile.TemporaryDirectory(prefix="tilo-managed-fex-") as home:
            environment = os.environ | {
                "HOME": home, "XDG_RUNTIME_DIR": home,
                "WINEPREFIX": str(prefix), "WINEARCH": f"win{bits}",
                "WINEDEBUG": "-all",
            }
            marker = f"TILO_MANAGED_FEX_PASS_{bits}"
            cases = [("console", f"echo {marker}", 0), ("status", "exit /b 23", 23)]
            for name, command, expected in cases:
                try:
                    process = subprocess.run(
                        [str(helper), f"wine{bits}", "cmd.exe", "/c", command],
                        env=environment, cwd=prefix / "drive_c", stdin=subprocess.DEVNULL,
                        stdout=subprocess.PIPE, stderr=subprocess.STDOUT,
                        timeout=180, text=True,
                    )
                    output, status, timed_out = process.stdout, process.returncode, False
                except subprocess.TimeoutExpired as error:
                    output = (error.stdout or b"").decode(errors="replace")
                    status, timed_out = None, True
                (args.output_dir / f"wine{bits}-{name}.log").write_text(output)
                marker_ok = name != "console" or marker in [line.strip() for line in output.splitlines()]
                c_drive = prefix / "dosdevices/c:"
                z_drive = prefix / "dosdevices/z:"
                private_drives_ok = (
                    c_drive.is_symlink() and c_drive.readlink() == Path("../drive_c")
                    and not z_drive.exists() and not z_drive.is_symlink()
                )
                mappings = [("x32", "system32")] if bits == 32 else [("x64", "system32"), ("x32", "syswow64")]
                dxvk_ok = True
                for source_arch, destination in mappings:
                    for interface in ("d3d8", "d3d9", "d3d10core", "d3d11", "dxgi"):
                        source = Path("/opt/tilo/x86/usr/share/tilo/dxvk") / source_arch / f"{interface}.dll"
                        installed = prefix / "drive_c/windows" / destination / f"{interface}.dll"
                        dxvk_ok = dxvk_ok and installed.is_file() and not installed.is_symlink()
                        if dxvk_ok:
                            dxvk_ok = hashlib.sha256(source.read_bytes()).digest() == hashlib.sha256(installed.read_bytes()).digest()
                results.append({
                    "bits": bits, "test": name, "exit_code": status,
                    "timed_out": timed_out,
                    "dxvk_payload_matches": dxvk_ok,
                    "private_drives": private_drives_ok,
                    "passed": not timed_out and status == expected and marker_ok and dxvk_ok and private_drives_ok,
                })
                print(json.dumps(results[-1]), flush=True)
                if timed_out:
                    break  # Container teardown owns any orphaned children.
        if timed_out:
            break
        prefix.rename(prefix.with_name(f"prefix-win{bits}"))
    (args.output_dir / "result.json").write_text(json.dumps(results, indent=2) + "\n")
    return 0 if len(results) == 4 and all(result["passed"] for result in results) else 1


if __name__ == "__main__":
    raise SystemExit(main())
