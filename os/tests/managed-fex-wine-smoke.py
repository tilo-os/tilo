#!/usr/bin/env python3
"""Test qOS's native FEX supervisor in a disposable Linux container.

Provide an init process, native FEX, a verified guest rootfs at /opt/qos/x86,
and Wine's guest resource directories at their native paths. Keep networking
disabled, capabilities dropped, and the normal container seccomp filter.
No externally started FEXServer is used: qOS must manage each invocation.
"""

import argparse
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
    for bits in (64, 32):
        with tempfile.TemporaryDirectory(prefix="qos-managed-fex-") as home:
            environment = os.environ | {
                "HOME": home, "XDG_RUNTIME_DIR": home,
                "WINEPREFIX": f"{home}/prefix", "WINEARCH": f"win{bits}",
                "WINEDEBUG": "-all",
            }
            marker = f"QOS_MANAGED_FEX_PASS_{bits}"
            cases = [("console", f"echo {marker}", 0), ("status", "exit /b 23", 23)]
            for name, command, expected in cases:
                try:
                    process = subprocess.run(
                        [str(helper), f"wine{bits}", "cmd.exe", "/c", command],
                        env=environment, stdin=subprocess.DEVNULL,
                        stdout=subprocess.PIPE, stderr=subprocess.STDOUT,
                        timeout=180, text=True,
                    )
                    output, status, timed_out = process.stdout, process.returncode, False
                except subprocess.TimeoutExpired as error:
                    output = (error.stdout or b"").decode(errors="replace")
                    status, timed_out = None, True
                (args.output_dir / f"wine{bits}-{name}.log").write_text(output)
                marker_ok = name != "console" or marker in [line.strip() for line in output.splitlines()]
                results.append({
                    "bits": bits, "test": name, "exit_code": status,
                    "timed_out": timed_out,
                    "passed": not timed_out and status == expected and marker_ok,
                })
                print(json.dumps(results[-1]), flush=True)
                if timed_out:
                    break  # Container teardown owns any orphaned children.
    (args.output_dir / "result.json").write_text(json.dumps(results, indent=2) + "\n")
    return 0 if len(results) == 4 and all(result["passed"] for result in results) else 1


if __name__ == "__main__":
    raise SystemExit(main())
