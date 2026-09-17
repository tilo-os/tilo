#!/usr/bin/env python3
"""Check Proton console execution through native ARM pressure-vessel and FEX.

Run inside a disposable, offline container with an init process and no granted
capabilities. Bind the verified runtime and guest rootfs read-only, and bind a
private writable file over the runtime platform's files/.ref. The caller owns
image/archive verification; this test does not download or register binfmt.
"""

import argparse
import json
import os
from pathlib import Path
import platform
import select
import shutil
import signal
import subprocess
import tempfile


def run(command, environment, timeout, log):
    with log.open("w") as output:
        process = subprocess.Popen(
            command, env=environment, stdin=subprocess.DEVNULL,
            stdout=output, stderr=subprocess.STDOUT, start_new_session=True,
        )
        try:
            return process.wait(timeout=timeout), False
        except subprocess.TimeoutExpired:
            os.killpg(process.pid, signal.SIGKILL)
            process.wait()
            return process.returncode, True


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--runtime", type=Path, required=True)
    parser.add_argument("--rootfs", type=Path, required=True)
    parser.add_argument("--proton", type=Path, required=True)
    parser.add_argument("--umu", type=Path, help="test the pinned UMU zipapp entry point offline")
    parser.add_argument("--output-dir", type=Path, required=True)
    parser.add_argument("--timeout", type=int, default=180)
    args = parser.parse_args()
    if platform.machine() != "aarch64" or args.timeout <= 0:
        parser.error("requires native AArch64 and a positive timeout")
    runtime, rootfs, proton = (
        path.resolve(strict=True) for path in (args.runtime, args.rootfs, args.proton)
    )
    umu = args.umu.resolve(strict=True) if args.umu else None
    if umu is not None and not umu.is_file():
        parser.error("--umu must point to the launcher zipapp")
    for path in (runtime, rootfs, proton):
        if not path.is_dir() or path == Path("/"):
            parser.error("runtime, rootfs, and proton must be separate directories")
    platforms = list(runtime.glob("sniper_platform_*/files"))
    if len(platforms) != 1:
        parser.error("runtime must contain exactly one sniper platform")
    pressure_vessel = runtime / "pressure-vessel-arm64/bin/pressure-vessel-wrap"
    for executable in (pressure_vessel, proton / "proton", Path("/usr/bin/FEX"),
                       Path("/usr/bin/FEXServer")):
        if not executable.is_file() or not os.access(executable, os.X_OK):
            parser.error(f"missing executable: {executable}")
    args.output_dir.mkdir(parents=True, exist_ok=True)
    if any(args.output_dir.iterdir()):
        parser.error("output directory must be empty")
    results = []
    with tempfile.TemporaryDirectory(prefix="tilo-proton-") as temporary:
        home = Path(temporary)
        for directory in ("compat", "steam"):
            (home / directory).mkdir()
        emulator = {"emulator_v0": {
            "argv": ["/usr/bin/FEX"],
            "environment": {"FEX_ROOTFS": str(rootfs), "FEX_INTERPRETER_INSTALLED": "0"},
            "container_environment": {"FEX_ROOTFS": ""},
            "emulated_architectures": ["x86_64-linux-gnu", "i386-linux-gnu"],
            "required_architectures": ["aarch64-linux-gnu"],
            "required_libraries": ["libc.so.6", "libstdc++.so.6", "libm.so.6", "libgcc_s.so.1"],
        }}
        manifest = home / "emulator.json"
        manifest.write_text(json.dumps(emulator))
        environment = os.environ | {
            "HOME": temporary, "XDG_RUNTIME_DIR": temporary,
            "FEX_ROOTFS": str(rootfs), "FEX_INTERPRETER_INSTALLED": "0",
            "STEAM_COMPAT_EMULATOR": str(manifest),
            "PRESSURE_VESSEL_ARCHITECTURES": "x86_64-linux-gnu:i386-linux-gnu",
            "STEAM_COMPAT_DATA_PATH": str(home / "compat"),
            "STEAM_COMPAT_CLIENT_INSTALL_PATH": str(home / "steam"),
            "UMU_ID": "umu-0", "SteamAppId": "0", "SteamGameId": "0",
            "WINEDEBUG": "-all",
        }
        if umu is not None:
            # UMU owns this per-user directory and atomically replaces its
            # install marker. Keep large runtime directories shared read-only;
            # the caller still supplies the separate writable .ref binding.
            local = home / "data/umu/steamrt3"
            local.mkdir(parents=True)
            for item in runtime.iterdir():
                if item.is_dir():
                    (local / item.name).symlink_to(item, target_is_directory=True)
                else:
                    shutil.copy2(item, local / item.name)
            (local / "umu").symlink_to("_v2-entry-point")
            graphics = home / "graphics.json"
            graphics.write_text(json.dumps({"graphics_provider_v0": {
                "root": str(rootfs),
                "architectures": ["x86_64-linux-gnu", "i386-linux-gnu"],
            }}))
            environment.update(
                UMU_FOLDERS_PATH=str(home / "data"), UMU_RUNTIME_UPDATE="0",
                UMU_LOG="debug", PROTONPATH=str(proton),
                WINEPREFIX=str(home / "compat"), GAMEID="umu-0",
                STEAM_COMPAT_GRAPHICS_PROVIDER=str(graphics),
                STEAM_COMPAT_MACHINE_ARCHITECTURE="aarch64-linux-gnu",
                PRESSURE_VESSEL_COPY_RUNTIME="1",
                PRESSURE_VESSEL_VARIABLE_DIR=str(home / "var"),
                PRESSURE_VESSEL_GENERATE_LOCALES="0",
                PRESSURE_VESSEL_SYSTEMD_SCOPE="0",
                PRESSURE_VESSEL_GRAPHICS_PROVIDER_aarch64_linux_gnu="/",
                PRESSURE_VESSEL_GRAPHICS_PROVIDER_x86_64_linux_gnu=str(rootfs),
                PRESSURE_VESSEL_GRAPHICS_PROVIDER_i386_linux_gnu=str(rootfs),
            )
        read_fd, write_fd = os.pipe()
        with (args.output_dir / "FEXServer.log").open("w") as server_log:
            server = subprocess.Popen(
                ["/usr/bin/FEXServer", "--foreground", "--persistent", "300",
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
                wrapper = [
                    str(pressure_vessel), "--batch", "--copy-runtime",
                    "--no-generate-locales", "--no-systemd-scope",
                    "--variable-dir", str(home / "var"),
                    "--runtime", str(platforms[0].parent),
                    "--graphics-provider=aarch64-linux-gnu=/",
                    f"--graphics-provider=x86_64-linux-gnu={rootfs}",
                    f"--graphics-provider=i386-linux-gnu={rootfs}",
                    f"--filesystem={proton}:ro", "--",
                ]
                marker = "TILO_PROTON_CONSOLE_PASS"
                command = wrapper + [
                    "/usr/bin/python3", str(proton / "proton"), "run", "cmd.exe",
                    "/c", rf"echo {marker}>C:\tilo-console.txt",
                ]
                if umu is not None:
                    command = [
                        "/usr/bin/FEX", "/usr/bin/python3", str(umu), "cmd.exe",
                        "/c", rf"echo {marker}>C:\tilo-console.txt",
                    ]
                log = args.output_dir / "proton.log"
                status, timed_out = run(
                    command, environment, args.timeout, log,
                )
                prefix = home / ("compat" if umu is not None else "compat/pfx")
                marker_path = prefix / "drive_c/tilo-console.txt"
                actual = marker_path.read_text().strip() if marker_path.is_file() else None
                alive = server.poll() is None
                offline_verified = None
                if umu is not None:
                    output = log.read_text(errors="replace")
                    offline_verified = all(message in output for message in (
                        "Network is unreachable", f"{platforms[0].parent.name}: mtree is OK",
                        "steamrt3 updates disabled, skipping",
                    ))
                results.append({
                    "test": "umu-offline-console" if umu is not None else "proton-console",
                    "exit_code": status,
                    "timed_out": timed_out, "server_running": alive,
                    "console_marker": actual,
                    "offline_runtime_verified": offline_verified,
                    "passed": (status == 0 and not timed_out and alive
                               and actual == marker and offline_verified is not False),
                })
            finally:
                os.close(read_fd)
                if server.poll() is None:
                    server.terminate()
                    try:
                        server.wait(timeout=10)
                    except subprocess.TimeoutExpired:
                        server.kill()
                        server.wait()
    (args.output_dir / "result.json").write_text(json.dumps(results, indent=2) + "\n")
    print(json.dumps(results, indent=2))
    return 0 if results and all(result["passed"] for result in results) else 1


if __name__ == "__main__":
    raise SystemExit(main())
