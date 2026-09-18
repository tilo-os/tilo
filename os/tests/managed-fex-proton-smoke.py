#!/usr/bin/env python3
"""Run offline UMU/Proton through the shipped ARM manifests and Tilo supervisor.

Use a disposable native ARM container with an init process, networking disabled,
and capabilities dropped. Mount the verified guest root at /opt/tilo/x86, the UMU
zipapp at /usr/libexec/umu_run.py, and the shipped manifests under
/usr/share/tilo/windows. Mount the Steam runtime read-only except for a private
writable file over its platform files/.ref. This is a runtime integration test;
it does not exercise the outer Tilo capsule or Landlock policy.
"""

import argparse
import json
import os
from pathlib import Path
import platform
import shutil
import subprocess
import tempfile


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--helper", required=True, type=Path)
    parser.add_argument("--runtime", required=True, type=Path)
    parser.add_argument("--proton", required=True, type=Path)
    parser.add_argument("--output-dir", required=True, type=Path)
    parser.add_argument("--timeout", type=int, default=180)
    args = parser.parse_args()
    if platform.machine() != "aarch64" or args.timeout <= 0:
        parser.error("requires native AArch64 and a positive timeout")
    helper, runtime, proton = (
        path.resolve(strict=True) for path in (args.helper, args.runtime, args.proton)
    )
    args.output_dir.mkdir(parents=True, exist_ok=True)
    if any(args.output_dir.iterdir()):
        parser.error("output directory must be empty")
    with tempfile.TemporaryDirectory(prefix="tilo-managed-umu-") as temporary:
        home = Path(temporary)
        local = home / "data/umu/steamrt3"
        local.mkdir(parents=True)
        for item in runtime.iterdir():
            if item.is_dir():
                (local / item.name).symlink_to(item, target_is_directory=True)
            else:
                shutil.copy2(item, local / item.name)
        (local / "umu").symlink_to("_v2-entry-point")
        environment = os.environ | {
            "HOME": temporary, "XDG_RUNTIME_DIR": temporary,
            "UMU_FOLDERS_PATH": str(home / "data"), "UMU_RUNTIME_UPDATE": "0",
            "UMU_LOG": "debug", "PROTONPATH": str(proton),
            "WINEPREFIX": str(home / "prefix"), "GAMEID": "umu-0",
            "WINEDEBUG": "-all",
            "STEAM_COMPAT_EMULATOR": "/usr/share/tilo/windows/fex-emulator.json",
            "STEAM_COMPAT_GRAPHICS_PROVIDER": "/usr/share/tilo/windows/fex-graphics-provider.json",
            "STEAM_COMPAT_MACHINE_ARCHITECTURE": "aarch64-linux-gnu",
            "PRESSURE_VESSEL_COPY_RUNTIME": "1",
            "PRESSURE_VESSEL_VARIABLE_DIR": str(home / "var"),
            "PRESSURE_VESSEL_GENERATE_LOCALES": "0",
            "PRESSURE_VESSEL_SYSTEMD_SCOPE": "0",
            "PRESSURE_VESSEL_GRAPHICS_PROVIDER_aarch64_linux_gnu": "/",
            "PRESSURE_VESSEL_GRAPHICS_PROVIDER_x86_64_linux_gnu": "/opt/tilo/x86",
            "PRESSURE_VESSEL_GRAPHICS_PROVIDER_i386_linux_gnu": "/opt/tilo/x86",
        }
        log = args.output_dir / "umu.log"
        with log.open("w") as output:
            try:
                process = subprocess.run(
                    [str(helper), "proton", "cmd.exe", "/c",
                     r"echo TILO_MANAGED_UMU_PASS>C:\tilo-console.txt"],
                    env=environment, stdin=subprocess.DEVNULL, stdout=output,
                    stderr=subprocess.STDOUT, timeout=args.timeout,
                )
                status, timed_out = process.returncode, False
            except subprocess.TimeoutExpired:
                # The disposable container's init and teardown own descendants.
                status, timed_out = None, True
        marker = home / "prefix/drive_c/tilo-console.txt"
        actual = marker.read_text().strip() if marker.is_file() else None
        output = log.read_text(errors="replace")
        offline = all(text in output for text in (
            "Network is unreachable", ": mtree is OK",
            "steamrt3 updates disabled, skipping",
        ))
        result = {
            "exit_code": status, "timed_out": timed_out,
            "console_marker": actual, "offline_runtime_verified": offline,
            "passed": not timed_out and status == 0
                      and actual == "TILO_MANAGED_UMU_PASS" and offline,
        }
        (args.output_dir / "result.json").write_text(json.dumps(result, indent=2) + "\n")
        print(json.dumps(result), flush=True)
        return 0 if result["passed"] else 1


if __name__ == "__main__":
    raise SystemExit(main())
