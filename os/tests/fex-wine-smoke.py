#!/usr/bin/env python3
"""Check both Wine architectures through FEX in a dedicated Linux container.

The caller supplies an installed native FEX runtime, a complete guest rootfs,
an init process, and container isolation. No host binfmt registration is needed.
Logs survive failures; prefixes and runtime state stay in a temporary directory.
"""

import argparse
import json
import os
from pathlib import Path
import select
import signal
import subprocess
import tempfile


def run(command, environment, timeout, log):
    process = subprocess.Popen(
        command, env=environment, stdin=subprocess.DEVNULL,
        stdout=subprocess.PIPE, stderr=subprocess.STDOUT, start_new_session=True,
    )
    timed_out = False
    try:
        output, _ = process.communicate(timeout=timeout)
    except subprocess.TimeoutExpired:
        timed_out = True
        os.killpg(process.pid, signal.SIGKILL)
        output, _ = process.communicate()
    log.write_bytes(output)
    return process.returncode, timed_out, output.decode("utf-8", errors="replace")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--rootfs", required=True, type=Path)
    parser.add_argument("--output-dir", required=True, type=Path)
    parser.add_argument("--fex", type=Path, default=Path("/usr/bin/FEX"))
    parser.add_argument("--server", type=Path, default=Path("/usr/bin/FEXServer"))
    parser.add_argument("--timeout", type=int, default=180)
    args = parser.parse_args()
    if args.timeout <= 0:
        parser.error("--timeout must be positive")
    rootfs = args.rootfs.resolve(strict=True)
    if not rootfs.is_dir() or rootfs == Path("/"):
        parser.error("--rootfs must be a separate guest filesystem directory")
    fex = args.fex.resolve(strict=True)
    server_exe = args.server.resolve(strict=True)
    args.output_dir.mkdir(parents=True, exist_ok=True)
    results = []

    with tempfile.TemporaryDirectory(prefix="tilo-fex-wine-") as temporary:
        environment = os.environ.copy()
        environment.update(
            HOME=temporary, XDG_RUNTIME_DIR=temporary, FEX_ROOTFS=str(rootfs),
            FEX_INTERPRETER_INSTALLED="0", WINEDEBUG="-all",
        )
        read_fd, write_fd = os.pipe()
        with (args.output_dir / "FEXServer.log").open("w") as server_log:
            server = subprocess.Popen(
                [str(server_exe), "--foreground", "--persistent", "300",
                 "--wait_pipe", str(write_fd)],
                env=environment, pass_fds=(write_fd,), stdin=subprocess.DEVNULL,
                stdout=server_log, stderr=subprocess.STDOUT,
            )
            os.close(write_fd)
            try:
                if not select.select([read_fd], [], [], 20)[0]:
                    raise RuntimeError("FEXServer readiness timed out")
                if os.read(read_fd, 8) or server.poll() is not None:
                    raise RuntimeError("FEXServer failed during startup")
                for bits in (64, 32):
                    guest = environment | {
                        "WINEARCH": f"win{bits}",
                        "WINEPREFIX": f"{temporary}/prefix{bits}",
                    }
                    version_status, version_timeout, _ = run(
                        [str(fex), f"/usr/bin/wine{bits}", "--version"], guest, 30,
                        args.output_dir / f"wine{bits}-version.log",
                    )
                    marker = f"TILO_WINE_CONSOLE_PASS_{bits}"
                    status, timed_out, output = run(
                        [str(fex), f"/usr/bin/wine{bits}", "cmd.exe", "/c", f"echo {marker}"],
                        guest, args.timeout, args.output_dir / f"wine{bits}-console.log",
                    )
                    # Stop this prefix's server before removing temporary state.
                    cleanup_status, cleanup_timeout, _ = run(
                        [str(fex), f"/usr/bin/wineserver{bits}", "-k"], guest, 30,
                        args.output_dir / f"wine{bits}-cleanup.log",
                    )
                    # -k returns 1 when there was no running server to signal.
                    # Independently wait for the prefix lock to be released.
                    wait_status, wait_timeout, _ = run(
                        [str(fex), f"/usr/bin/wineserver{bits}", "-w"], guest, 30,
                        args.output_dir / f"wine{bits}-server-wait.log",
                    )
                    passed = (
                        version_status == 0 and not version_timeout
                        and status == 0 and not timed_out
                        and marker in [line.strip() for line in output.splitlines()]
                        and cleanup_status in (0, 1) and not cleanup_timeout
                        and wait_status == 0 and not wait_timeout
                    )
                    results.append({
                        "bits": bits, "passed": passed, "exit_code": status,
                        "timed_out": timed_out, "version_exit_code": version_status,
                        "version_timed_out": version_timeout,
                        "cleanup_exit_code": cleanup_status,
                        "cleanup_timed_out": cleanup_timeout,
                        "server_wait_exit_code": wait_status,
                        "server_wait_timed_out": wait_timeout,
                    })
                    print(f"Wine {bits}: {'PASS' if passed else 'FAIL'}", flush=True)
            finally:
                os.close(read_fd)
                server.terminate()
                try:
                    server.wait(timeout=10)
                except subprocess.TimeoutExpired:
                    server.kill()
                    server.wait()
    report = {"rootfs": str(rootfs), "results": results}
    (args.output_dir / "result.json").write_text(json.dumps(report, indent=2) + "\n")
    return 0 if len(results) == 2 and all(result["passed"] for result in results) else 1


if __name__ == "__main__":
    raise SystemExit(main())
