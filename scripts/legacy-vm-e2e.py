#!/usr/bin/env python3
"""End-to-end proof of a legacy VM against a real headless Tilo shell.

Boots the Linux legacy guest disk under KVM through `tilo-legacy` (an
unprivileged libvirt session), with a headless `shell` as the compositor, and
checks, in order:

1. CLI stdio and exit status relay: `run -- sh -c 'echo hi; exit 3'`.
2. A granted host directory is visible in the guest at the same path.
3. A GUI app (foot) launched in the guest maps as the foreign toplevel
   `legacy.linux.foot`, and its first frames are not flat.
4. Text typed through the compositor reaches the app in the guest.
5. With a short idle limit the VM is saved to disk, and the next launch
   restores it (the guest's boot id is unchanged).

Run as an ordinary user with /dev/kvm and /dev/vhost-vsock accessible.
"""

from __future__ import annotations

import argparse
import json
import os
import signal
import socket
import struct
import subprocess
import tempfile
import time
import zlib
from pathlib import Path

SCREEN = (1600, 900)
DEADLINE = 30.0
IDLE_SECONDS = 20
APP_ID = "legacy.linux.foot"
TYPED = "hello-legacy"


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

    def target_names(self) -> set[str]:
        response = self.send({"op": "list_targets"})
        return {entry["name"] for entry in response.get("targets", [])}


def wait_until(description: str, predicate, deadline: float = DEADLINE):
    started = time.monotonic()
    last_error = None
    while time.monotonic() - started < deadline:
        try:
            value = predicate()
            if value:
                return value
        except (ConnectionError, FileNotFoundError, json.JSONDecodeError, OSError) as error:
            last_error = error
        time.sleep(0.2)
    suffix = f"; last error: {last_error}" if last_error else ""
    raise Failure(f"timed out waiting for {description}{suffix}")


def complete_onboarding(control: Control) -> None:
    """Create a throwaway identity through the same UI a first boot uses."""
    names = wait_until("onboarding or desktop layout", control.target_names)
    if "onboarding.begin.button" not in names:
        return
    control.send({"op": "click_named", "name": "onboarding.begin.button"})
    wait_until("onboarding acknowledgement",
               lambda: "onboarding.create.button" in control.target_names())
    control.send({"op": "click_at", "x": 480, "y": 618})
    control.send({"op": "click_named", "name": "onboarding.create.button"})
    wait_until("onboarding completion action",
               lambda: "onboarding.finish.button" in control.target_names())
    control.send({"op": "click_named", "name": "onboarding.finish.button"})
    wait_until("desktop after onboarding",
               lambda: "onboarding.finish.button" not in control.target_names())


def frame_for(control: Control, app_id: str) -> dict | None:
    envelope = control.send({"op": "foreign_frames"})
    for frame in envelope.get("foreign_frames", {}).get("frames", []):
        if frame.get("app_id") == app_id and frame.get("frame_seq", 0) > 0:
            rect = frame.get("rect") or {}
            if frame.get("presented") and rect.get("w", 0) > 100 and rect.get("h", 0) > 100:
                return frame
    return None


def read_png(path: Path) -> tuple[int, int, int, bytes]:
    """(width, height, bytes per pixel, unfiltered pixels) of an 8-bit RGB(A) PNG."""
    data = path.read_bytes()
    if data[:8] != b"\x89PNG\r\n\x1a\n":
        raise Failure(f"{path} is not a PNG")
    pos, idat = 8, b""
    width = height = bpp = 0
    while pos < len(data):
        length, kind = struct.unpack(">I4s", data[pos:pos + 8])
        body = data[pos + 8:pos + 8 + length]
        pos += 12 + length
        if kind == b"IHDR":
            width, height, depth, color = struct.unpack(">IIBB", body[:10])
            if depth != 8 or color not in (2, 6):
                raise Failure(f"unsupported PNG format depth={depth} color={color}")
            bpp = 4 if color == 6 else 3
        elif kind == b"IDAT":
            idat += body
        elif kind == b"IEND":
            break
    raw = zlib.decompress(idat)
    stride = width * bpp
    out = bytearray(height * stride)
    previous = bytearray(stride)
    for row in range(height):
        start = row * (stride + 1)
        kind = raw[start]
        line = bytearray(raw[start + 1:start + 1 + stride])
        for i in range(stride):
            left = line[i - bpp] if i >= bpp else 0
            up = previous[i]
            up_left = previous[i - bpp] if i >= bpp else 0
            if kind == 1:
                line[i] = (line[i] + left) & 0xFF
            elif kind == 2:
                line[i] = (line[i] + up) & 0xFF
            elif kind == 3:
                line[i] = (line[i] + ((left + up) >> 1)) & 0xFF
            elif kind == 4:
                p = left + up - up_left
                pa, pb, pc = abs(p - left), abs(p - up), abs(p - up_left)
                pred = left if pa <= pb and pa <= pc else (up if pb <= pc else up_left)
                line[i] = (line[i] + pred) & 0xFF
        out[row * stride:(row + 1) * stride] = line
        previous = line
    return width, height, bpp, bytes(out)


def distinct_colours(png: Path, rect: dict) -> int:
    width, height, bpp, pixels = read_png(png)
    colours = set()
    x0, y0 = max(0, int(rect["x"])), max(0, int(rect["y"]))
    x1 = min(width, int(rect["x"] + rect["w"]))
    y1 = min(height, int(rect["y"] + rect["h"]))
    for y in range(y0, y1, 2):
        for x in range(x0, x1, 2):
            offset = (y * width + x) * bpp
            colours.add(pixels[offset:offset + 3])
    return len(colours)


class Legacy:
    """`tilo-legacy` with this run's image and idle limit."""

    def __init__(self, binary: Path, image: Path, env: dict, log: Path):
        self.base = [str(binary), "--image", str(image), "--idle-suspend", str(IDLE_SECONDS)]
        self.env = env
        self.log = log

    def run(self, *args: str, timeout: float = 600) -> subprocess.CompletedProcess:
        result = subprocess.run(
            [*self.base, *args], env=self.env, capture_output=True, text=True, timeout=timeout,
        )
        with self.log.open("a") as log:
            log.write(f"$ tilo-legacy {' '.join(args)}\n-> {result.returncode}\n"
                      f"stdout: {result.stdout}\nstderr: {result.stderr}\n")
        return result

    def guest(self, script: str, timeout: float = 600) -> subprocess.CompletedProcess:
        return self.run("run", "--no-window", "--", "sh", "-c", script, timeout=timeout)

    def popen(self, *args: str, log) -> subprocess.Popen:
        return subprocess.Popen(
            [*self.base, *args], env=self.env, stdin=subprocess.DEVNULL,
            stdout=log, stderr=subprocess.STDOUT, start_new_session=True,
        )


def terminate(process: subprocess.Popen | None) -> None:
    if process is None or process.poll() is not None:
        return
    try:
        os.killpg(process.pid, signal.SIGTERM)
        process.wait(timeout=5)
    except (ProcessLookupError, subprocess.TimeoutExpired):
        try:
            os.killpg(process.pid, signal.SIGKILL)
        except ProcessLookupError:
            pass


def check(condition: bool, message: str) -> None:
    if not condition:
        raise Failure(message)
    print(f"PASS {message}", flush=True)


def run(args: argparse.Namespace) -> None:
    artifacts = args.artifacts.resolve()
    artifacts.mkdir(parents=True, exist_ok=True)
    root = Path(tempfile.mkdtemp(prefix="tilo-legacy-e2e-", dir="/var/tmp"))
    runtime = root / "runtime"
    runtime.mkdir(mode=0o700)
    control_path = runtime / "control.sock"
    env = os.environ.copy()
    env.update({
        "XDG_RUNTIME_DIR": str(runtime),
        "XDG_CONFIG_HOME": str(root / "config"),
        "XDG_DATA_HOME": str(root / "data"),
        "XDG_STATE_HOME": str(root / "state"),
        "XDG_CACHE_HOME": str(root / "cache"),
        "HOME": str(root / "home"),
        "TILO_STATE_PATH": str(root / "state.json"),
        "TILO_IDENTITY_ROOT": str(root / "identity"),
        "TILO_WORKSPACE_ROOTS": "",
        "TILO_TEST_CONTROL": str(control_path),
        "TILO_HEADLESS_SIZE": f"{SCREEN[0]}x{SCREEN[1]}",
        "RUST_LOG": "warn",
    })
    (root / "home").mkdir()
    env.pop("WAYLAND_DISPLAY", None)
    env.pop("DISPLAY", None)
    legacy = Legacy(args.tilo_legacy, args.image.resolve(), env, artifacts / "tilo-legacy.log")
    compositor = foot = None
    compositor_log = (artifacts / "compositor.log").open("wb")
    foot_log = (artifacts / "foot.log").open("wb")
    control = Control(control_path)
    try:
        compositor = subprocess.Popen(
            [str(args.shell), "--mode", "native", "--backend", "headless"],
            env=env, stdout=compositor_log, stderr=subprocess.STDOUT, start_new_session=True,
        )
        wait_until("test-control socket", control_path.exists, 120)
        wait_until("initial workspace", lambda: control.send({"op": "state_dump"}), 120)
        complete_onboarding(control)
        display_file = runtime / "tilo-session" / "wayland-display"
        wait_until("published Wayland display",
                   lambda: display_file.exists() and display_file.read_text().strip())

        # 1. stdio and exit status, which also wakes (creates and boots) the VM.
        started = time.monotonic()
        result = legacy.guest("echo hi; exit 3", timeout=900)
        print(f"first launch (create + boot) took {time.monotonic() - started:.0f}s", flush=True)
        check(result.stdout == "hi\n" and result.returncode == 3,
              f"stdout and exit status relay (got {result.stdout!r}, {result.returncode})")

        # 2. a granted directory at the same path.
        shared = root / "shared"
        shared.mkdir()
        (shared / "marker.txt").write_text("from the host\n")
        granted = legacy.run("grant", str(shared))
        check(granted.returncode == 0, f"grant {shared}")
        seen = legacy.guest(f"cat '{shared}/marker.txt'")
        check(seen.stdout == "from the host\n",
              f"granted directory visible in the guest at {shared} (got {seen.stdout!r})")
        boot_before = legacy.guest("cat /proc/sys/kernel/random/boot_id").stdout.strip()
        check(len(boot_before) == 36, f"guest boot id {boot_before!r}")

        # 3. a GUI app's window arrives as a legacy foreign toplevel.
        foot = legacy.popen(
            "run", "--", "foot", "--app-id", "foot", "sh", "-c",
            f"echo legacy window ready; read line; echo \"$line\" > /tmp/typed",
            log=foot_log,
        )
        frame = wait_until(f"{APP_ID} to map and present", lambda: frame_for(control, APP_ID), 180)
        dump = json.dumps(control.send({"op": "state_dump"}))
        check(APP_ID in dump, f"state dump lists the foreign tab {APP_ID}")
        shot = artifacts / "legacy-window.png"
        control.send({"op": "snapshot", "path": str(shot)})
        wait_until("snapshot", shot.exists)
        time.sleep(0.5)
        colours = distinct_colours(shot, frame["rect"])
        check(colours >= 4, f"the legacy window's frame is not flat ({colours} colours)")

        # 4. typed text reaches the app in the guest.
        rect = frame["rect"]
        control.send({"op": "click_at", "x": rect["x"] + rect["w"] / 2, "y": rect["y"] + rect["h"] / 2})
        time.sleep(0.5)
        for character in TYPED:
            control.send({"op": "type", "text": character})
            time.sleep(0.05)
        control.send({"op": "key", "key": "Enter"})
        wait_until("foot to exit after its line", lambda: foot.poll() is not None, 60)
        typed = legacy.guest("cat /tmp/typed").stdout.strip()
        check(typed == TYPED, f"typed text arrived in the guest (got {typed!r})")

        # 5. idle suspend, then resume on launch.
        def suspended() -> bool:
            status = legacy.run("status", timeout=60).stdout
            return "suspended" in status.lower()
        wait_until("the idle VM to be saved to disk", suspended, IDLE_SECONDS + 240)
        check(True, "the idle VM was saved to disk")
        started = time.monotonic()
        back = legacy.guest("echo back; cat /proc/sys/kernel/random/boot_id")
        lines = back.stdout.split()
        check(lines[:1] == ["back"], f"launch after suspend runs (got {back.stdout!r})")
        check(lines[1:2] == [boot_before],
              f"the VM resumed from its snapshot rather than rebooting "
              f"({time.monotonic() - started:.0f}s)")
    except Exception:
        try:
            (artifacts / "foreign-frames.json").write_text(
                json.dumps(control.send({"op": "foreign_frames"}), indent=2) + "\n")
            control.send({"op": "snapshot", "path": str(artifacts / "failed.png")})
        except Exception:
            pass
        raise
    finally:
        terminate(foot)
        subprocess.run([*legacy.base, "stop"], env=env, capture_output=True, timeout=120)
        supervisor_log = runtime / "tilo-legacy" / "linux" / "supervisor.log"
        if supervisor_log.exists():
            (artifacts / "supervisor.log").write_bytes(supervisor_log.read_bytes())
        for qemu_log in (root / "cache").glob("libvirt/qemu/log/*.log"):
            (artifacts / qemu_log.name).write_bytes(qemu_log.read_bytes())
        terminate(compositor)
        compositor_log.close()
        foot_log.close()


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--shell", type=Path, required=True)
    parser.add_argument("--tilo-legacy", type=Path, required=True)
    parser.add_argument("--image", type=Path, required=True)
    parser.add_argument("--artifacts", type=Path, required=True)
    try:
        run(parser.parse_args())
    except Failure as failure:
        print(f"FAIL {failure}", flush=True)
        return 1
    print("legacy VM end-to-end: all checks passed", flush=True)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
