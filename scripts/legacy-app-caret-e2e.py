#!/usr/bin/env python3
"""Drive caret focus through real legacy Linux applications.

This is deliberately a process-level test.  Each case boots a fresh qOS
headless compositor, launches an application built with a different desktop
toolkit, and uses the compositor's developer-only control socket for input.
The assertion is semantic: text typed after the pointer leaves the app must
still be selected and copied by that app through the real Wayland clipboard.
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


def terminate(process: subprocess.Popen | None) -> None:
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
        process.wait(timeout=3)


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


def run_case(shell: Path, case: Case, artifacts: Path) -> None:
    executable = case.executable()
    if executable is None:
        raise Failure(f"{case.name}: no real application executable found: {case.candidates}")

    case_artifacts = artifacts / case.name
    case_artifacts.mkdir(parents=True, exist_ok=True)
    # Keep Unix socket paths comfortably below SUN_LEN. TemporaryDirectory
    # removes this tree on success and failure; durable diagnostics live in
    # ``artifacts`` instead.
    with tempfile.TemporaryDirectory(prefix="qos-legacy-") as raw_root:
        root = Path(raw_root)
        runtime = root / "runtime"
        runtime.mkdir(mode=0o700)
        control_path = root / "control.sock"
        compositor_log = (case_artifacts / "compositor.log").open("wb")
        app_log = (case_artifacts / "app.log").open("wb")

        env = os.environ.copy()
        env.update({
            "XDG_RUNTIME_DIR": str(runtime),
            "XDG_CONFIG_HOME": str(root / "config"),
            "XDG_DATA_HOME": str(root / "data"),
            "XDG_STATE_HOME": str(root / "state"),
            "XDG_CACHE_HOME": str(root / "cache"),
            "QOS_STATE_PATH": str(root / "state.json"),
            "QOS_IDENTITY_ROOT": str(root / "identity"),
            "QOS_WORKSPACE_ROOTS": "",
            "QOS_TEST_CONTROL": str(control_path),
            "NIRIOS_HEADLESS_SIZE": f"{SCREEN[0]}x{SCREEN[1]}",
            "RUST_LOG": "warn,shell::compositor::client_input=debug",
        })
        env.pop("WAYLAND_DISPLAY", None)
        env.pop("DISPLAY", None)

        compositor = None
        app = None
        try:
            compositor = subprocess.Popen(
                [str(shell), "--mode", "native", "--backend", "headless"],
                env=env,
                stdout=compositor_log,
                stderr=subprocess.STDOUT,
                start_new_session=True,
            )
            wait_until("test-control socket", control_path.exists)
            control = Control(control_path)
            wait_until("initial workspace", lambda: control.send({"op": "state_dump"}))
            complete_onboarding(control)
            display_file = runtime / "qos-session" / "wayland-display"
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

            frame = wait_until(
                f"{case.name} to render a real frame",
                lambda: frame_for(control, case.app_id),
            )
            rect = frame["rect"]
            click_x = rect["x"] + rect["w"] * case.click_fraction[0]
            click_y = rect["y"] + rect["h"] * case.click_fraction[1]
            prefix = f"qos{case.name}before"
            suffix = "after"
            expected = prefix + suffix

            # Clear any toolkit-provided sample value, then type on both sides
            # of a genuine pointer-leave transition.
            control.send({"op": "click_at", "x": click_x, "y": click_y})
            frame = wait_for_frame_after(control, case.app_id, frame["frame_seq"])
            control.chord("a")
            control.send({"op": "key", "key": "Backspace"})
            time.sleep(0.1)
            frame = frame_for(control, case.app_id) or frame
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

            def copied_text():
                response = control.send({"op": "clipboard_paste", "primary": False})
                return response.get("clipboard", {}).get("text")

            copied = wait_until(
                f"{case.name} clipboard to contain the typed payload",
                lambda: (value := copied_text()) and value,
            )
            if copied.strip("\r\n") != expected:
                raise Failure(
                    f"{case.name}: caret/input mismatch after pointer leave; "
                    f"expected {expected!r}, copied {copied!r}"
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
            terminate(app)
            terminate(compositor)
            compositor_log.close()
            app_log.close()


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
