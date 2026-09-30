#!/usr/bin/env python3
"""Drive caret focus through legacy Linux applications.

This is deliberately a process-level test.  Each case boots a fresh Tilo
headless compositor, launches an application built with a different desktop
toolkit, and uses the compositor's developer-only control socket for input.
The assertion is semantic: text typed after the pointer leaves the app must
still be selected and copied through the Wayland clipboard.
"""

from __future__ import annotations

import argparse
import json
import os
from pathlib import Path
import shutil
import signal
import socket
import subprocess
import sys
import tempfile
import time


DEADLINE = 20.0
SCREEN = (1600, 900)


class Failure(RuntimeError):
    pass


class Control:
    def __init__(self, path: Path):
        self.path = path

    def send(self, request: dict) -> dict:
        with socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as conn:
            conn.settimeout(DEADLINE)
            conn.connect(str(self.path))
            conn.sendall(json.dumps(request).encode() + b"\n")
            response = b""
            while not response.endswith(b"\n"):
                chunk = conn.recv(65536)
                if not chunk:
                    break
                response += chunk
        decoded = json.loads(response)
        if "err" in decoded:
            raise Failure(f"control request failed: {request}: {decoded}")
        return decoded

    def chord(self, key: str) -> None:
        self.send({"op": "key_down", "key": "Control"})
        try:
            self.send({"op": "key", "key": key})
        finally:
            self.send({"op": "key_up", "key": "Control"})

    def target_names(self) -> set[str]:
        response = self.send({"op": "list_targets"})
        return {entry["name"] for entry in response.get("targets", [])}


class Case:
    def __init__(self, name: str, candidates: tuple[str, ...], app_id: str,
                 click_fraction: tuple[float, float], args: tuple[str, ...] = ()):
        self.name = name
        self.candidates = candidates
        self.app_id = app_id
        self.click_fraction = click_fraction
        self.args = args

    def executable(self) -> str | None:
        for candidate in self.candidates:
            if "/" in candidate:
                matches = sorted(Path("/").glob(candidate.lstrip("/")))
                if matches:
                    return str(matches[0])
            else:
                found = shutil.which(candidate)
                if found:
                    return found
        return None


CASES = (
    Case(
        "libadwaita",
        ("gnome-text-editor",),
        "gnome-text-editor",
        # Blank document body in the real GNOME/libadwaita editor.
        (0.50, 0.50),
        ("--standalone",),
    ),
    Case(
        "qt6",
        (
            "/usr/lib64/qt6/examples/widgets/widgets/lineedits/bin/lineedits",
            "/usr/lib/qt6/examples/widgets/widgets/lineedits/bin/lineedits",
            "/usr/lib64/qt6/examples/widgets/widgets/lineedits/lineedits",
            "/usr/lib/qt6/examples/widgets/widgets/lineedits/lineedits",
            "lineedits",
        ),
        "lineedits",
        # The first ordinary QLineEdit in the official Qt example. Avoid the
        # mode combobox above it and the masked/read-only variants beside it.
        (0.25, 0.24),
    ),
    Case(
        "libreoffice",
        ("libreoffice", "soffice"),
        "libreoffice-writer",
        (0.52, 0.58),
        ("--writer", "--nologo", "--nolockcheck", "--norestore"),
    ),
)


def wait_until(description: str, predicate):
    started = time.monotonic()
    last_error = None
    while time.monotonic() - started < DEADLINE:
        try:
            value = predicate()
            if value:
                return value
        except (ConnectionError, FileNotFoundError, json.JSONDecodeError, OSError) as error:
            last_error = error
        time.sleep(0.05)
    suffix = f"; last error: {last_error}" if last_error else ""
    raise Failure(f"timed out waiting for {description}{suffix}")


# Starting a process cold is bounded by its own progress, not by DEADLINE:
# a freshly installed LibreOffice first reads hundreds of MiB of libraries,
# which on a busy disk takes longer than any fixed interaction budget. The
# wait fails once the watched process group stops using CPU and stops
# reading for STALL seconds, and in any case after STARTUP_HANG.
STALL = 20.0
STARTUP_HANG = 180.0


def group_progress(pgid: int) -> tuple[int, ...]:
    """CPU ticks, major faults and bytes read by every process in a group."""
    ticks = faults = read = 0
    for proc in Path("/proc").iterdir():
        if not proc.name.isdigit():
            continue
        try:
            stat = (proc / "stat").read_text()
            fields = stat[stat.rindex(")") + 2:].split()
            if int(fields[2]) != pgid:
                continue
            faults += int(fields[9])
            ticks += int(fields[11]) + int(fields[12])
            for line in (proc / "io").read_text().splitlines():
                if line.startswith("read_bytes:"):
                    read += int(line.split()[1])
        except (OSError, ValueError, IndexError):
            continue
    return (ticks, faults, read)


def wait_while_progressing(description: str, predicate, process: subprocess.Popen):
    """Wait for `predicate` while `process`'s group keeps making progress."""
    started = time.monotonic()
    last_progress_at = started
    last = group_progress(process.pid)
    last_error = None
    while True:
        try:
            value = predicate()
            if value:
                return value
        except (ConnectionError, FileNotFoundError, json.JSONDecodeError, OSError) as error:
            last_error = error
        now = time.monotonic()
        current = group_progress(process.pid)
        if current != last:
            last, last_progress_at = current, now
        stalled = now - last_progress_at >= STALL
        if stalled or now - started >= STARTUP_HANG:
            why = (
                f"{process.args[0]} made no CPU or read progress for {STALL:.0f}s"
                if stalled
                else f"still starting after {STARTUP_HANG:.0f}s"
            )
            suffix = f"; last error: {last_error}" if last_error else ""
            raise Failure(f"timed out waiting for {description}: {why}{suffix}")
        time.sleep(0.05)


# A SIGKILLed process is only reaped once it leaves the kernel: one blocked
# in a driver or filesystem call, or being dumped, outlives the signal.
KILLED_REAP_SECONDS = 30.0


def terminate(process: subprocess.Popen | None, record: Path | None = None) -> None:
    if process is None or process.poll() is not None:
        return
    try:
        os.killpg(process.pid, signal.SIGTERM)
        process.wait(timeout=3)
    except (ProcessLookupError, subprocess.TimeoutExpired):
        try:
            os.killpg(process.pid, signal.SIGKILL)
        except ProcessLookupError:
            pass
        try:
            process.wait(timeout=KILLED_REAP_SECONDS)
        except subprocess.TimeoutExpired:
            if record is not None:
                record_process_group(process.pid, record)
            raise Failure(
                f"{process.args[0]} survived SIGKILL for {KILLED_REAP_SECONDS:.0f}s"
                + (f"; where it is blocked: {record}" if record is not None else "")
            ) from None


def record_process_group(pgid: int, destination: Path) -> None:
    """Write where every thread of a still-running process group is blocked.

    A wait that runs out on a process that never answered is only
    diagnosable from what it was doing: its state, the kernel function each
    thread sleeps in, its open files, and its kernel stack where readable.
    """
    lines: list[str] = []
    for proc in sorted(Path("/proc").iterdir(), key=lambda p: p.name):
        if not proc.name.isdigit():
            continue
        try:
            stat = (proc / "stat").read_text()
            # comm may contain spaces; the fields after its closing paren are fixed.
            fields = stat[stat.rindex(")") + 2:].split()
            if int(fields[2]) != pgid:
                continue
            cmdline = (proc / "cmdline").read_bytes().replace(b"\0", b" ").decode(errors="replace")
            lines.append(f"pid {proc.name} state {fields[0]} ppid {fields[1]}: {cmdline.strip()}")
            for task in sorted((proc / "task").iterdir(), key=lambda p: int(p.name)):
                comm = (task / "comm").read_text().strip()
                state = (task / "stat").read_text().rsplit(")", 1)[1].split()[0]
                wchan = (task / "wchan").read_text().strip() or "-"
                lines.append(f"  tid {task.name} {comm} state {state} wchan {wchan}")
                try:
                    stack = (task / "stack").read_text().strip()
                except OSError:
                    stack = ""
                lines.extend(f"    {frame}" for frame in stack.splitlines())
            for fd in sorted((proc / "fd").iterdir(), key=lambda p: int(p.name)):
                try:
                    lines.append(f"  fd {fd.name} -> {os.readlink(fd)}")
                except OSError:
                    pass
        except (OSError, ValueError, IndexError):
            continue
    destination.write_text("\n".join(lines) + "\n" if lines else f"no live process in group {pgid}\n")


def frame_for(control: Control, app_id: str) -> dict | None:
    envelope = control.send({"op": "foreign_frames"})
    frames = envelope.get("foreign_frames", {}).get("frames", [])
    for frame in frames:
        if frame.get("app_id") == app_id and frame.get("frame_seq", 0) > 0:
            # Do not type into a client hidden by the shell's opening card.
            # CSD buffers can include shadows beyond the tab rect, in which
            # case the shell deliberately waits for its bounded presentation
            # fallback before accepting this as visually ready.
            if not frame.get("presented"):
                continue
            rect = frame.get("rect")
            if rect and rect.get("w", 0) > 100 and rect.get("h", 0) > 100:
                return frame
    return None


def frame_or_process_exit(
    control: Control, case: Case, process: subprocess.Popen
) -> dict | None:
    """Return the first real frame, or fail immediately if its app died.

    A dead toolkit process cannot become a compositor timeout. Keeping these
    failure modes distinct is important when the matrix catches a broken host
    package before the application ever connects to Wayland.
    """
    frame = frame_for(control, case.app_id)
    if frame is not None:
        return frame
    status = process.poll()
    if status is not None:
        raise Failure(
            f"{case.name}: real application exited with status {status} "
            "before publishing a Wayland frame; see app.log"
        )
    return None


def wait_for_frame_after(control: Control, app_id: str, sequence: int) -> dict:
    return wait_until(
        f"{app_id} to render after input frame {sequence}",
        lambda: (
            current
            if (current := frame_for(control, app_id))
            and current["frame_seq"] > sequence
            else None
        ),
    )


def type_with_frame_barriers(control: Control, app_id: str, frame: dict, text: str) -> dict:
    """Type through slow toolkit queues without treating an intermediate repaint as completion."""
    for character in text:
        control.send({"op": "type", "text": character})
        frame = wait_for_frame_after(control, app_id, frame["frame_seq"])
    return frame


def clipboard_text(control: Control) -> str | None:
    response = control.send({"op": "clipboard_paste", "primary": False})
    return response.get("clipboard", {}).get("text")


def wait_for_clipboard_text(
    control: Control, description: str, expected: str, *, after_sample: bool = False
) -> str:
    """Wait for the clipboard to hold `expected`, or with `after_sample` to end with it."""
    last_value: str | None = None

    def matching_text() -> str | None:
        nonlocal last_value
        value = clipboard_text(control)
        last_value = value
        if value is None:
            return None
        text = value.strip("\r\n")
        if text == expected or (after_sample and text.endswith(expected)):
            return value
        return None

    try:
        return wait_until(description, matching_text)
    except Failure as error:
        raise Failure(
            f"{error}; expected {expected!r}, last copied {last_value!r}"
        ) from error


def verify_focus_and_select_probe(control: Control, app_id: str, frame: dict) -> dict:
    """Prove keyboard focus and leave known document contents selected.

    No keystroke may follow a Ctrl+A until something acknowledges the
    selection. LibreOffice runs Select All from its dispatcher, after an
    unrelated repaint has already satisfied the frame barrier. A key typed in
    that gap is inserted first and then selected, so the next key replaces it.
    That is how "tilofocusprobe" became "ilofocusprobe". Type the probe after
    any sample content, then select everything and copy it. Copy is dispatched
    after Select All, so the clipboard acknowledges both. Leaving it selected
    lets the payload's first character replace the whole document atomically.
    """
    probe = "tilofocusprobe"
    frame = type_with_frame_barriers(control, app_id, frame, probe)
    control.chord("a")
    frame = wait_for_frame_after(control, app_id, frame["frame_seq"])
    control.chord("c")
    wait_for_clipboard_text(
        control, f"{app_id} focus probe selection", probe, after_sample=True
    )
    return frame_for(control, app_id) or frame


def snapshot(control: Control, destination: Path) -> None:
    control.send({"op": "snapshot", "path": str(destination)})
    wait_until(f"snapshot {destination}", lambda: destination.exists())


def complete_onboarding(control: Control) -> None:
    """Create a throwaway identity through the same UI a first boot uses."""
    names = wait_until("onboarding or desktop layout", control.target_names)
    if "onboarding.begin.button" not in names:
        return
    control.send({"op": "click_named", "name": "onboarding.begin.button"})
    wait_until(
        "onboarding acknowledgement",
        lambda: "onboarding.create.button" in control.target_names(),
    )
    # Checkbox currently exposes its action key rather than its test id in the
    # widget registry. Its full-system layout is fixed by SCREEN above; the
    # following named Create button is still the guard that proves the click
    # actually enabled the irreversible step.
    control.send({"op": "click_at", "x": 480, "y": 618})
    control.send({"op": "click_named", "name": "onboarding.create.button"})
    wait_until(
        "onboarding completion action",
        lambda: "onboarding.finish.button" in control.target_names(),
    )
    control.send({"op": "click_named", "name": "onboarding.finish.button"})
    wait_until(
        "desktop after onboarding",
        lambda: "onboarding.finish.button" not in control.target_names(),
    )


def preflight_toolkit_runtime(
    case: Case, executable: str, env: dict[str, str], root: Path, log
) -> None:
    """Distinguish a broken toolkit installation from a Wayland failure."""
    if case.name not in {"qt6", "libreoffice"}:
        return

    preflight_env = env.copy()
    preflight_env.pop("WAYLAND_DISPLAY", None)
    preflight_env.pop("DISPLAY", None)
    if case.name == "qt6":
        # The copied plugin tree is needed only for the Wayland run on hosts
        # whose checkout is virtiofs.  Loading the offscreen backend from that
        # tmpfs copy aborts in Qt before it can produce diagnostics, so let the
        # compositor-independent health check use the installed plugin tree.
        preflight_env.pop("QT_PLUGIN_PATH", None)
        preflight_env["QT_QPA_PLATFORM"] = "offscreen"
        command = [executable, *case.args]
    else:
        preflight_env["SAL_USE_VCLPLUGIN"] = "svp"
        command = [
            executable,
            *case.args,
            "--headless",
            f"-env:UserInstallation=file://{root / 'libreoffice-preflight-profile'}",
        ]

    process = subprocess.Popen(
        command,
        env=preflight_env,
        stdout=log,
        stderr=subprocess.STDOUT,
        start_new_session=True,
    )
    try:
        status = None
        deadline = time.monotonic() + 3.0
        while status is None and time.monotonic() < deadline:
            time.sleep(0.1)
            status = process.poll()
        if status is not None and status != 0:
            raise Failure(
                f"{case.name}: installed application exits with status {status} "
                "on its toolkit's compositor-independent backend; this host "
                "runtime cannot exercise Tilo (see preflight.log)"
            )
    finally:
        terminate(process)


def run_case(shell: Path, case: Case, artifacts: Path) -> None:
    executable = case.executable()
    if executable is None:
        raise Failure(f"{case.name}: no real application executable found: {case.candidates}")

    case_artifacts = artifacts / case.name
    case_artifacts.mkdir(parents=True, exist_ok=True)
    # Keep Unix socket paths comfortably below SUN_LEN. TemporaryDirectory
    # removes this tree on success and failure; durable diagnostics live in
    # ``artifacts`` instead.
    temp_parent = (
        "/dev/shm"
        if case.name in {"qt6", "libreoffice"} and os.access("/dev/shm", os.W_OK)
        else None
    )
    with tempfile.TemporaryDirectory(prefix="tilo-legacy-", dir=temp_parent) as raw_root:
        root = Path(raw_root)
        runtime = root / "runtime"
        runtime.mkdir(mode=0o700)
        control_path = root / "control.sock"
        compositor_log = (case_artifacts / "compositor.log").open("wb")
        app_log = (case_artifacts / "app.log").open("wb")
        preflight_log = (case_artifacts / "preflight.log").open("wb")

        env = os.environ.copy()
        env.update({
            "XDG_RUNTIME_DIR": str(runtime),
            "XDG_CONFIG_HOME": str(root / "config"),
            "XDG_DATA_HOME": str(root / "data"),
            "XDG_STATE_HOME": str(root / "state"),
            "XDG_CACHE_HOME": str(root / "cache"),
            "TILO_STATE_PATH": str(root / "state.json"),
            "TILO_IDENTITY_ROOT": str(root / "identity"),
            "TILO_WORKSPACE_ROOTS": "",
            "TILO_TEST_CONTROL": str(control_path),
            "TILO_HEADLESS_SIZE": f"{SCREEN[0]}x{SCREEN[1]}",
            "RUST_LOG": "warn,shell::compositor::client_input=debug",
        })
        env.pop("WAYLAND_DISPLAY", None)
        env.pop("DISPLAY", None)
        compositor = None
        app = None
        try:
            preflight_toolkit_runtime(case, executable, env, root, preflight_log)
            compositor = subprocess.Popen(
                [str(shell), "--mode", "native", "--backend", "headless"],
                env=env,
                stdout=compositor_log,
                stderr=subprocess.STDOUT,
                start_new_session=True,
            )
            wait_while_progressing("test-control socket", control_path.exists, compositor)
            control = Control(control_path)
            wait_while_progressing(
                "initial workspace", lambda: control.send({"op": "state_dump"}), compositor
            )
            complete_onboarding(control)
            display_file = runtime / "tilo-session" / "wayland-display"
            display = wait_until(
                "published Wayland display",
                lambda: display_file.read_text().strip() if display_file.exists() else None,
            )

            app_env = env.copy()
            app_env.update({
                "WAYLAND_DISPLAY": display,
                "GDK_BACKEND": "wayland",
                "QT_QPA_PLATFORM": "wayland",
                "SAL_USE_VCLPLUGIN": "gtk3",
                "NO_AT_BRIDGE": "1",
            })
            command = [executable, *case.args]
            if case.name == "libreoffice":
                command.append(f"-env:UserInstallation=file://{root / 'libreoffice-profile'}")
            app = subprocess.Popen(
                command,
                env=app_env,
                stdout=app_log,
                stderr=subprocess.STDOUT,
                start_new_session=True,
            )

            frame = wait_while_progressing(
                f"{case.name} to render a real frame",
                lambda: frame_or_process_exit(control, case, app),
                app,
            )
            rect = frame["rect"]
            click_x = rect["x"] + rect["w"] * case.click_fraction[0]
            click_y = rect["y"] + rect["h"] * case.click_fraction[1]
            prefix = f"tilo{case.name}before"
            suffix = "after"
            expected = prefix + suffix

            # Clear any toolkit-provided sample value, then type on both sides
            # of a genuine pointer-leave transition.
            control.send({"op": "click_at", "x": click_x, "y": click_y})
            frame = wait_for_frame_after(control, case.app_id, frame["frame_seq"])
            # Some toolkits use the first click to activate a newly mapped
            # window and only the next click to place the caret. Repeat the
            # click after the activation repaint so the first typed character
            # is never racing that focus handshake (notably LibreOffice).
            control.send({"op": "click_at", "x": click_x, "y": click_y})
            frame = wait_for_frame_after(control, case.app_id, frame["frame_seq"])
            frame = verify_focus_and_select_probe(control, case.app_id, frame)
            frame = type_with_frame_barriers(control, case.app_id, frame, prefix)
            control.send({"op": "pointer_move", "x": 10, "y": SCREEN[1] - 10})
            # Let the pointer-leave commit settle before taking the sequence
            # barrier for the suffix. Otherwise that redraw could satisfy the
            # wait while the application's last key is still queued.
            time.sleep(0.1)
            frame = frame_for(control, case.app_id) or frame
            frame = type_with_frame_barriers(control, case.app_id, frame, suffix)
            control.chord("a")
            control.chord("c")

            copied = wait_for_clipboard_text(
                control,
                f"{case.name} clipboard to contain the typed payload",
                expected,
            )
            snapshot(control, case_artifacts / "passed.png")
            (case_artifacts / "result.json").write_text(json.dumps({
                "app_id": case.app_id,
                "executable": executable,
                "surface_id": frame["surface_id"],
                "frame_seq": frame["frame_seq"],
                "copied": copied,
            }, indent=2) + "\n")
            print(f"PASS {case.name}: {expected}")
        except Exception:
            for name, process in (("app", app), ("compositor", compositor)):
                if process is not None and process.poll() is None:
                    record_process_group(process.pid, case_artifacts / f"{name}-processes.txt")
            if control_path.exists():
                try:
                    failed_control = Control(control_path)
                    diagnostics = failed_control.send({"op": "foreign_frames"})
                    (case_artifacts / "foreign-frames.json").write_text(
                        json.dumps(diagnostics, indent=2) + "\n"
                    )
                    snapshot(failed_control, case_artifacts / "failed.png")
                except Exception:
                    pass
            raise
        finally:
            terminate(app, case_artifacts / "app-unreaped.txt")
            terminate(compositor, case_artifacts / "compositor-unreaped.txt")
            compositor_log.close()
            app_log.close()
            preflight_log.close()


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser()
    parser.add_argument("--shell", type=Path, default=Path("target/debug/shell"))
    parser.add_argument("--artifacts", type=Path, default=Path("target/legacy-app-e2e"))
    parser.add_argument(
        "--case", choices=("all", *(case.name for case in CASES)), default="all"
    )
    return parser.parse_args()


def main() -> int:
    args = parse_args()
    shell = args.shell.resolve()
    if not shell.is_file():
        raise Failure(f"shell binary does not exist: {shell}")
    args.artifacts.mkdir(parents=True, exist_ok=True)
    selected = CASES if args.case == "all" else tuple(
        case for case in CASES if case.name == args.case
    )
    for case in selected:
        run_case(shell, case, args.artifacts.resolve())
    return 0


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except Failure as error:
        print(f"FAIL: {error}", file=sys.stderr)
        raise SystemExit(1)
