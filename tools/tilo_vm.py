#!/usr/bin/env python3
"""Fast, receipt-driven control of local Tilo ARM64 QEMU runs."""

from __future__ import annotations

import argparse
import ctypes
import datetime as dt
import hashlib
import json
import os
from pathlib import Path
import re
import shutil
import socket
import subprocess
import sys
import time
import uuid
from typing import Any, Iterable


SCHEMA = "tilo.vm-run/v1"
SENSITIVE_ARGUMENT = re.compile(r"password|passphrase|secret|token|credential", re.I)
DEFAULT_TARGET_GIB = 64


def utc_now() -> str:
    return dt.datetime.now(dt.timezone.utc).isoformat().replace("+00:00", "Z")


def json_dump(value: Any) -> str:
    return json.dumps(value, indent=2, sort_keys=True) + "\n"


def write_new(path: Path, value: str) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    with path.open("x", encoding="utf-8", newline="\n") as stream:
        stream.write(value)


def append_event(run_dir: Path, event: str, **details: Any) -> None:
    record = {"at": utc_now(), "event": event, **details}
    with (run_dir / "events.jsonl").open("a", encoding="utf-8", newline="\n") as stream:
        stream.write(json.dumps(record, sort_keys=True) + "\n")


def sha256(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as stream:
        for block in iter(lambda: stream.read(1024 * 1024), b""):
            digest.update(block)
    return digest.hexdigest()


def file_receipt(path: Path, *, with_hash: bool = False) -> dict[str, Any]:
    resolved = path.resolve()
    result: dict[str, Any] = {
        "path": str(resolved),
        "bytes": resolved.stat().st_size,
        "modified_ns": resolved.stat().st_mtime_ns,
    }
    if with_hash:
        result["sha256"] = sha256(resolved)
    return result


def path_location(path: Path) -> str:
    value = str(path)
    if re.match(r"^\\\\wsl(?:\.localhost)?\\", value, re.I):
        return "wsl-unc"
    if value.startswith("\\\\"):
        return "network-unc"
    return "local"


def windows_allocation(path: Path) -> int:
    high = ctypes.c_ulong()
    low = ctypes.windll.kernel32.GetCompressedFileSizeW(str(path), ctypes.byref(high))
    if low == 0xFFFFFFFF and ctypes.get_last_error():
        raise ctypes.WinError(ctypes.get_last_error())
    return (high.value << 32) | low


def allocated_bytes(path: Path) -> int:
    if os.name == "nt":
        return windows_allocation(path)
    stat = path.stat()
    return int(getattr(stat, "st_blocks", 0) * 512)


def windows_filesystem(path: Path) -> str:
    if os.name != "nt":
        return "test-local"
    volume = ctypes.create_unicode_buffer(261)
    if not ctypes.windll.kernel32.GetVolumePathNameW(str(path), volume, len(volume)):
        raise ctypes.WinError(ctypes.get_last_error())
    filesystem = ctypes.create_unicode_buffer(261)
    if not ctypes.windll.kernel32.GetVolumeInformationW(
        volume.value, None, 0, None, None, None, filesystem, len(filesystem)
    ):
        raise ctypes.WinError(ctypes.get_last_error())
    return filesystem.value


def create_sparse_target(path: Path, size_gib: int, owned_root: Path) -> dict[str, Any]:
    root = owned_root.resolve()
    target = path.resolve()
    if path_location(target) != "local":
        raise ValueError("automatic disposable targets require a local path; pass --target to preserve an explicit WSL or network path")
    try:
        target.relative_to(root)
    except ValueError as error:
        raise ValueError(f"automatic disposable target escapes its owned directory: {target}") from error
    if target.exists() or target.is_symlink():
        raise FileExistsError(f"refusing to replace disposable target: {target}")
    target.parent.mkdir(parents=True, exist_ok=True)
    filesystem = windows_filesystem(target.parent)
    if os.name == "nt" and filesystem.upper() != "NTFS":
        raise ValueError(f"automatic disposable targets require NTFS, found {filesystem}: {target.parent}")
    logical = size_gib * 1024**3
    try:
        with target.open("xb+") as stream:
            if os.name == "nt":
                import msvcrt

                handle = msvcrt.get_osfhandle(stream.fileno())
                returned = ctypes.c_ulong()
                if not ctypes.windll.kernel32.DeviceIoControl(
                    ctypes.c_void_p(handle), 0x900C4,
                    None, 0, None, 0, ctypes.byref(returned), None,
                ):
                    raise ctypes.WinError(ctypes.get_last_error())
                if not ctypes.windll.kernel32.SetFilePointerEx(
                    ctypes.c_void_p(handle), ctypes.c_longlong(logical), None, 0
                ):
                    raise ctypes.WinError(ctypes.get_last_error())
                if not ctypes.windll.kernel32.SetEndOfFile(ctypes.c_void_p(handle)):
                    raise ctypes.WinError(ctypes.get_last_error())
            else:
                stream.truncate(logical)
            stream.flush()
            os.fsync(stream.fileno())
        allocation = allocated_bytes(target)
        if allocation >= logical:
            raise RuntimeError(f"disposable target is dense ({allocation} allocated of {logical} bytes): {target}")
        return {
            "path": str(target),
            "location": "local-ntfs" if os.name == "nt" else "local-test",
            "filesystem": filesystem,
            "logical_bytes": logical,
            "allocated_bytes": allocation,
            "sparse": True,
        }
    except Exception:
        target.unlink(missing_ok=True)
        raise


def read_tail(path: Path, maximum_bytes: int) -> bytes:
    if maximum_bytes < 1:
        raise ValueError("serial tail size must be positive")
    with path.open("rb") as stream:
        stream.seek(0, os.SEEK_END)
        stream.seek(max(0, stream.tell() - maximum_bytes), os.SEEK_SET)
        return stream.read(maximum_bytes)


def available_port() -> int:
    with socket.socket(socket.AF_INET, socket.SOCK_STREAM) as probe:
        probe.bind(("127.0.0.1", 0))
        return int(probe.getsockname()[1])


def parse_launcher_arguments(values: Iterable[str]) -> list[tuple[str, str | None]]:
    parsed: list[tuple[str, str | None]] = []
    for value in values:
        if "=" in value:
            name, argument = value.split("=", 1)
        else:
            name, argument = value, None
        if not re.fullmatch(r"[A-Za-z][A-Za-z0-9]*", name):
            raise ValueError(f"invalid PowerShell parameter name: {name!r}")
        parsed.append((name, argument))
    return parsed


def redacted_arguments(arguments: list[tuple[str, str | None]]) -> dict[str, Any]:
    return {
        name: "<redacted>" if SENSITIVE_ARGUMENT.search(name) else value if value is not None else True
        for name, value in arguments
    }


def powershell() -> str:
    found = shutil.which("pwsh") or shutil.which("powershell.exe") or shutil.which("powershell")
    if not found:
        raise RuntimeError("PowerShell was not found in PATH")
    return found


def launcher_command(
    launcher: Path,
    run_dir: Path,
    qmp_port: int,
    serial_port: int,
    arguments: list[tuple[str, str | None]],
    *,
    check_only: bool,
    state_parameter: str | None,
    qmp_parameter: str | None,
    serial_parameter: str | None,
) -> list[str]:
    command = [powershell(), "-NoLogo", "-NoProfile", "-NonInteractive", "-File", str(launcher.resolve())]
    standard = (
        (state_parameter, str(run_dir)),
        (qmp_parameter, str(qmp_port)),
        (serial_parameter, str(serial_port)),
    )
    for name, value in standard:
        if name:
            command.extend((f"-{name}", value))
    for name, value in arguments:
        command.append(f"-{name}")
        if value is not None:
            command.append(value)
    if check_only:
        command.append("-CheckOnly")
    return command


def run_launcher(command: list[str], run_dir: Path, stem: str) -> subprocess.CompletedProcess[str]:
    started = time.monotonic()
    process = subprocess.run(command, text=True, encoding="utf-8", errors="replace", capture_output=True)
    write_new(run_dir / f"{stem}.stdout.log", process.stdout)
    write_new(run_dir / f"{stem}.stderr.log", process.stderr)
    append_event(
        run_dir,
        f"launcher-{stem}",
        exit_code=process.returncode,
        duration_seconds=round(time.monotonic() - started, 3),
    )
    return process


class QMP:
    def __init__(self, port: int, timeout: float = 3.0):
        self.socket = socket.create_connection(("127.0.0.1", port), timeout=timeout)
        self.stream = self.socket.makefile("rwb", buffering=0)
        self._read_reply()
        self.command("qmp_capabilities")

    def close(self) -> None:
        self.stream.close()
        self.socket.close()

    def __enter__(self) -> "QMP":
        return self

    def __exit__(self, *_: object) -> None:
        self.close()

    def _read_reply(self) -> dict[str, Any]:
        while True:
            line = self.stream.readline()
            if not line:
                raise RuntimeError("QMP closed the connection")
            reply = json.loads(line)
            if "event" not in reply:
                return reply

    def command(self, execute: str, arguments: dict[str, Any] | None = None) -> Any:
        request: dict[str, Any] = {"execute": execute}
        if arguments is not None:
            request["arguments"] = arguments
        self.stream.write(json.dumps(request).encode("utf-8") + b"\r\n")
        reply = self._read_reply()
        if "error" in reply:
            raise RuntimeError(f"QMP {execute} failed: {reply['error']}")
        return reply.get("return")


def read_run(reference: str, runs_dir: Path) -> tuple[Path, dict[str, Any]]:
    candidate = Path(reference)
    run_dir = candidate if candidate.is_dir() else runs_dir / reference
    receipt_path = run_dir / "run.json"
    if not receipt_path.is_file():
        raise RuntimeError(f"run receipt is missing: {receipt_path}")
    return run_dir.resolve(), json.loads(receipt_path.read_text(encoding="utf-8-sig"))


def process_status(pid: int | None) -> str:
    if not pid:
        return "unknown"
    if os.name == "nt":
        result = subprocess.run(
            ["powershell.exe", "-NoLogo", "-NoProfile", "-Command", f"if (Get-Process -Id {pid} -ErrorAction SilentlyContinue) {{ exit 0 }} else {{ exit 1 }}"],
            capture_output=True,
        )
        return "running" if result.returncode == 0 else "exited"
    try:
        os.kill(pid, 0)
        return "running"
    except OSError:
        return "exited"


def discover_pid(run_dir: Path, stdout: str) -> int | None:
    for candidate in (run_dir / "launch.json", run_dir / "qemu.pid"):
        if not candidate.is_file():
            continue
        try:
            if candidate.suffix == ".json":
                value = json.loads(candidate.read_text(encoding="utf-8-sig")).get("pid")
            else:
                value = candidate.read_text(encoding="ascii").strip()
            return int(value)
        except (ValueError, TypeError, json.JSONDecodeError):
            pass
    matches = re.findall(r'"pid"\s*:\s*(\d+)', stdout)
    return int(matches[-1]) if matches else None


def new_run_dir(runs_dir: Path, requested: str | None) -> tuple[str, Path]:
    if requested:
        if not re.fullmatch(r"[A-Za-z0-9][A-Za-z0-9._-]{0,79}", requested):
            raise ValueError("run id must be 1-80 safe filename characters")
        run_id = requested
    else:
        timestamp = dt.datetime.now(dt.timezone.utc).strftime("%Y%m%dT%H%M%SZ")
        run_id = f"{timestamp}-{uuid.uuid4().hex[:8]}"
    run_dir = (runs_dir / run_id).resolve()
    run_dir.mkdir(parents=True, exist_ok=False)
    return run_id, run_dir


def normalized_inputs(args: argparse.Namespace, launcher_args: list[tuple[str, str | None]]) -> dict[str, Any]:
    result: dict[str, Any] = {
        "launcher": file_receipt(args.launcher, with_hash=True),
        "launcher_arguments": redacted_arguments(launcher_args),
    }
    for label in ("disk", "target", "kernel", "initrd", "source_overlay"):
        path = getattr(args, label, None)
        if path:
            if label == "target" and args.disposable_target and not path.exists():
                continue
            result[label] = file_receipt(path)
            if label == "target":
                result[label]["location"] = path_location(path)
                result[label]["allocated_bytes"] = allocated_bytes(path)
    return result


def materialize_arguments(args: argparse.Namespace) -> list[tuple[str, str | None]]:
    values = parse_launcher_arguments(args.launcher_argument)
    mapped = (
        ("DiskPath", args.disk),
        ("InstallTargetDisk", args.target),
        ("KernelPath", args.kernel),
        ("InitrdPath", args.initrd),
        ("SourceOverlayPath", args.source_overlay),
    )
    existing = {name.lower() for name, _ in values}
    for name, path in mapped:
        if path and name.lower() not in existing:
            values.append((name, str(path.resolve())))
    if args.disposable_target:
        if "createinstalltarget" not in existing:
            values.append(("CreateInstallTarget", None))
        if "installtargetgib" not in existing:
            values.append(("InstallTargetGiB", str(args.target_size_gib)))
    return values


def preflight_or_start(args: argparse.Namespace, *, start: bool) -> int:
    launcher = args.launcher.resolve()
    if not launcher.is_file() or launcher.suffix.lower() != ".ps1":
        raise RuntimeError(f"PowerShell launcher is missing: {launcher}")
    if args.disposable_target and args.target:
        raise ValueError("--disposable-target and --target are mutually exclusive")
    if not 34 <= args.target_size_gib <= 4096:
        raise ValueError("--target-size-gib must be between 34 and 4096")
    run_id, run_dir = new_run_dir(args.runs_dir.resolve(), args.run_id)
    target_storage = None
    if args.disposable_target:
        target_root = args.runs_dir.resolve().parent / "targets"
        args.target = target_root / f"{run_id}.raw"
        if start:
            target_storage = create_sparse_target(args.target, args.target_size_gib, target_root)
            write_new(run_dir / "target.json", json_dump(target_storage))
            append_event(run_dir, "target-created", **target_storage)
    launcher_args = materialize_arguments(args)
    qmp_port, serial_port = available_port(), available_port()
    while serial_port == qmp_port:
        serial_port = available_port()
    created_at = utc_now()
    common = {
        "schema": SCHEMA,
        "run_id": run_id,
        "kind": "vm" if start else "preflight",
        "created_at": created_at,
        "run_directory": str(run_dir),
        "ports": {"qmp": qmp_port, "serial": serial_port},
        "inputs": normalized_inputs(args, launcher_args),
        "launcher_parameters": {
            "state": args.state_parameter,
            "qmp": args.qmp_parameter,
            "serial": args.serial_parameter,
        },
        "target_storage": target_storage or ({
            "path": str(args.target.resolve()),
            "location": "planned-local-ntfs" if os.name == "nt" else "planned-local-test",
            "logical_bytes": args.target_size_gib * 1024**3,
            "sparse": True,
        } if args.disposable_target else None),
    }
    append_event(run_dir, "run-created", kind=common["kind"])
    check = launcher_command(
        launcher, run_dir, qmp_port, serial_port, launcher_args,
        check_only=True, state_parameter=args.state_parameter,
        qmp_parameter=args.qmp_parameter, serial_parameter=args.serial_parameter,
    )
    checked = run_launcher(check, run_dir, "preflight")
    if checked.returncode:
        write_new(run_dir / "run.json", json_dump({**common, "preflight_exit_code": checked.returncode}))
        print(json_dump({"run_id": run_id, "run_directory": str(run_dir), "preflight": "failed"}), end="")
        return checked.returncode
    if not start:
        receipt = {**common, "preflight_exit_code": 0}
        write_new(run_dir / "run.json", json_dump(receipt))
        print(json_dump(receipt), end="")
        return 0
    command = launcher_command(
        launcher, run_dir, qmp_port, serial_port, launcher_args,
        check_only=False, state_parameter=args.state_parameter,
        qmp_parameter=args.qmp_parameter, serial_parameter=args.serial_parameter,
    )
    launched = run_launcher(command, run_dir, "start")
    pid = discover_pid(run_dir, launched.stdout)
    receipt = {**common, "preflight_exit_code": 0, "launcher_exit_code": launched.returncode, "pid": pid}
    write_new(run_dir / "run.json", json_dump(receipt))
    append_event(run_dir, "run-started" if launched.returncode == 0 else "run-start-failed", pid=pid)
    print(json_dump(receipt), end="")
    return launched.returncode


def command_status(args: argparse.Namespace) -> int:
    run_dir, receipt = read_run(args.run, args.runs_dir.resolve())
    result: dict[str, Any] = {
        "run_id": receipt["run_id"],
        "run_directory": str(run_dir),
        "process": process_status(receipt.get("pid")),
        "qmp": "unavailable",
    }
    try:
        with QMP(int(receipt["ports"]["qmp"])) as qmp:
            result["qmp"] = qmp.command("query-status")
    except (OSError, RuntimeError, KeyError, ValueError) as error:
        result["qmp_error"] = str(error)
    print(json_dump(result), end="")
    return 0


def command_capture(args: argparse.Namespace) -> int:
    run_dir, receipt = read_run(args.run, args.runs_dir.resolve())
    capture_id = dt.datetime.now(dt.timezone.utc).strftime("%Y%m%dT%H%M%S.%fZ")
    capture_dir = run_dir / "captures" / capture_id
    capture_dir.mkdir(parents=True, exist_ok=False)
    screenshot = capture_dir / "screen.ppm"
    with QMP(int(receipt["ports"]["qmp"])) as qmp:
        qmp.command("screendump", {"filename": str(screenshot)})
        qmp_status = qmp.command("query-status")
    serial_candidates = (run_dir / "serial.log", run_dir / "qemu.serial.log")
    serial = next((path for path in serial_candidates if path.is_file()), None)
    serial_tail = None
    if serial:
        data = read_tail(serial, args.serial_bytes)
        serial_tail = capture_dir / "serial-tail.log"
        write_new(serial_tail, data.decode("utf-8", errors="replace"))
    metadata = {
        "schema": "tilo.vm-capture/v1",
        "run_id": receipt["run_id"],
        "captured_at": utc_now(),
        "qmp_status": qmp_status,
        "screenshot": file_receipt(screenshot, with_hash=True),
        "serial_tail": file_receipt(serial_tail, with_hash=True) if serial_tail else None,
    }
    write_new(capture_dir / "capture.json", json_dump(metadata))
    append_event(run_dir, "capture-created", capture_id=capture_id)
    print(json_dump(metadata), end="")
    return 0


def command_stop(args: argparse.Namespace) -> int:
    run_dir, receipt = read_run(args.run, args.runs_dir.resolve())
    method = "qmp"
    try:
        with QMP(int(receipt["ports"]["qmp"])) as qmp:
            qmp.command("quit")
    except (OSError, RuntimeError, KeyError, ValueError) as error:
        if not args.force or not receipt.get("pid"):
            raise RuntimeError(f"QMP stop failed ({error}); use --force for the receipt's PID") from error
        method = "process"
        pid = int(receipt["pid"])
        if os.name == "nt":
            stopped = subprocess.run(
                ["powershell.exe", "-NoLogo", "-NoProfile", "-Command", f"Stop-Process -Id {pid} -ErrorAction Stop"],
                capture_output=True,
            )
            if stopped.returncode:
                raise RuntimeError(stopped.stderr.decode(errors="replace"))
        else:
            os.kill(pid, 15)
    append_event(run_dir, "stop-requested", method=method)
    print(json_dump({"run_id": receipt["run_id"], "stop_requested": True, "method": method}), end="")
    return 0


def common_start_arguments(parser: argparse.ArgumentParser) -> None:
    parser.add_argument("--launcher", type=Path, required=True, help="existing .ps1 launcher to wrap")
    parser.add_argument("--run-id", help="optional stable label; must be unused")
    parser.add_argument("--disk", type=Path)
    parser.add_argument("--target", type=Path)
    parser.add_argument("--disposable-target", action="store_true",
                        help="create a fresh sparse target under the local target store")
    parser.add_argument("--target-size-gib", type=int, default=DEFAULT_TARGET_GIB)
    parser.add_argument("--kernel", type=Path)
    parser.add_argument("--initrd", type=Path)
    parser.add_argument("--source-overlay", type=Path)
    parser.add_argument("--launcher-argument", action="append", default=[], metavar="NAME[=VALUE]")
    parser.add_argument("--state-parameter", default="StateDirectory")
    parser.add_argument("--qmp-parameter", default="QmpPort")
    parser.add_argument("--serial-parameter", default="SerialPort")


def parser() -> argparse.ArgumentParser:
    result = argparse.ArgumentParser(description=__doc__)
    default_runs = Path(os.environ.get("TILO_VM_RUNS", r"C:\tilo-build\runs"))
    result.add_argument("--runs-dir", type=Path, default=default_runs)
    commands = result.add_subparsers(dest="command", required=True)
    preflight = commands.add_parser("preflight", help="validate a launcher and its exact inputs")
    common_start_arguments(preflight)
    preflight.set_defaults(handler=lambda args: preflight_or_start(args, start=False))
    start = commands.add_parser("start", help="preflight and start a new immutable run")
    common_start_arguments(start)
    start.set_defaults(handler=lambda args: preflight_or_start(args, start=True))
    status = commands.add_parser("status", help="query the process and QMP state")
    status.add_argument("run")
    status.set_defaults(handler=command_status)
    capture = commands.add_parser("capture", help="save a screenshot and bounded serial tail")
    capture.add_argument("run")
    capture.add_argument("--serial-bytes", type=int, default=1024 * 1024)
    capture.set_defaults(handler=command_capture)
    stop = commands.add_parser("stop", help="request a clean QMP shutdown")
    stop.add_argument("run")
    stop.add_argument("--force", action="store_true", help="kill only the PID recorded by this run if QMP is unavailable")
    stop.set_defaults(handler=command_stop)
    return result


def main(argv: list[str] | None = None) -> int:
    args = parser().parse_args(argv)
    try:
        return int(args.handler(args))
    except (OSError, RuntimeError, ValueError) as error:
        print(f"tilo-vm: {error}", file=sys.stderr)
        return 2


if __name__ == "__main__":
    raise SystemExit(main())
