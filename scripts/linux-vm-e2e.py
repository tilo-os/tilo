#!/usr/bin/env python3
"""End-to-end proof of a Linux VM against a real headless Tilo session.

Boots the Linux guest disk under KVM through `tilo-linux-vm` (an
unprivileged libvirt session), with a headless `tilo-session` as the compositor,
and checks, in order:

1. CLI stdio and exit status relay: `run -- sh -c 'echo hi; exit 3'`.
2. A granted host directory is visible in the guest at the same path.
3. A GUI app (foot) launched in the guest maps as the window
   `vm.linux.foot`, and its first frames are not flat.
4. Text typed through the compositor reaches the app in the guest.
5. With a short idle limit the idle VM goes to sleep and the next launch
   wakes it. With kernel vhost-vsock the VM is saved to disk and restored
   (the guest's boot id is unchanged); on a host that only has the
   user-space vsock daemon it is shut down and booted again.

Run as an ordinary user with /dev/kvm accessible.
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
import uuid
import zlib
from pathlib import Path

SCREEN = (1600, 900)
DEADLINE = 30.0
IDLE_SECONDS = 20
APP_ID = "vm.linux.foot"
TYPED = "hello-linux-vm"


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

    def read(self, resource: str) -> dict:
        """The session's own `resource`, as the walk reads it."""
        answer = self.send({"version": 3, "message": {"Invoke": {
            "invocation": uuid.uuid4().hex,
            "target": {"provider": "so.tilo.session",
                       "address": {"resource": resource, "action": "read"}},
            "argument": "None"}}})
        return answer["message"]["Receipt"]["status"]["Completed"]["output"]["value"]

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


def start_session(session: Path, env: dict, log) -> subprocess.Popen:
    return subprocess.Popen(
        [str(session), "--backend", "headless", "--size", f"{SCREEN[0]}x{SCREEN[1]}"],
        env=env, stdout=log, stderr=subprocess.STDOUT, start_new_session=True,
    )


def mint_identity(session: Path, env: dict, control: Control, log: Path) -> None:
    """Give the person a throwaway identity, as a development session mints one, so the session
    under test opens on the desktop rather than on onboarding, which covers every window."""
    with log.open("wb") as output:
        minting = start_session(session, env, output)
        try:
            wait_until("the minting session's test-control socket", control.path.exists, 120)
            wait_until("the minting session", lambda: control.read("state"), 120)
            minted = control.send({"op": "provision_test_identity"})
            check(bool(minted.get("answer", {}).get("throwaway")),
                  f"a throwaway identity is minted ({json.dumps(minted)[:200]})")
        finally:
            terminate(minting)
    control.path.unlink(missing_ok=True)


def on_the_desktop(control: Control) -> bool:
    """The session shows its desktop: controls, none of them onboarding's."""
    names = control.target_names()
    return bool(names) and not any(name.startswith("onboarding.") for name in names)


def wayland_display(log: Path) -> str | None:
    """The display the session bound: the `WAYLAND_DISPLAY=wayland-N` line it prints."""
    if not log.exists():
        return None
    for line in log.read_text(errors="replace").splitlines():
        if line.startswith("WAYLAND_DISPLAY="):
            return line.split("=", 1)[1].strip()
    return None


def windows(control: Control) -> dict:
    """The shell's open windows, each with `rect` (`[x, y, width, height]`, null while no window
    of the tab is mapped) and `frames`, the frames its client committed."""
    return control.read("windows")


def frame_for(control: Control, app_id: str) -> dict | None:
    for row in windows(control).get("answer", {}).get("windows", []):
        rect = row.get("rect")
        if (
            row.get("app_id") == app_id
            and row.get("frames", 0) > 0
            and rect is not None
            and rect[2] > 100
            and rect[3] > 100
        ):
            return row
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


def distinct_colours(png: Path, rect: list) -> int:
    width, height, bpp, pixels = read_png(png)
    colours = set()
    x0, y0 = max(0, int(rect[0])), max(0, int(rect[1]))
    x1 = min(width, int(rect[0] + rect[2]))
    y1 = min(height, int(rect[1] + rect[3]))
    for y in range(y0, y1, 2):
        for x in range(x0, x1, 2):
            offset = (y * width + x) * bpp
            colours.add(pixels[offset:offset + 3])
    return len(colours)


class LinuxVm:
    """`tilo-linux-vm` with this run's image and idle limit."""

    def __init__(self, binary: Path, image: Path, env: dict, log: Path):
        self.base = [str(binary), "--image", str(image), "--idle-suspend", str(IDLE_SECONDS)]
        self.env = env
        self.log = log

    def run(self, *args: str, timeout: float = 600) -> subprocess.CompletedProcess:
        result = subprocess.run(
            [*self.base, *args], env=self.env, capture_output=True, text=True, timeout=timeout,
        )
        with self.log.open("a") as log:
            log.write(f"$ tilo-linux-vm {' '.join(args)}\n-> {result.returncode}\n"
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
    root = Path(tempfile.mkdtemp(prefix="tilo-linux-vm-e2e-", dir="/var/tmp"))
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
        "RUST_LOG": "warn",
    })
    (root / "home").mkdir()
    env.pop("WAYLAND_DISPLAY", None)
    env.pop("DISPLAY", None)
    vm = LinuxVm(args.tilo_linux_vm, args.image.resolve(), env, artifacts / "tilo-linux-vm.log")
    compositor = foot = None
    compositor_log = (artifacts / "compositor.log").open("wb")
    foot_log = (artifacts / "foot.log").open("wb")
    control = Control(control_path)
    try:
        mint_identity(args.session, env, control, artifacts / "identity.log")
        compositor = start_session(args.session, env, compositor_log)
        wait_until("test-control socket", control_path.exists, 120)
        wait_until("initial workspace", lambda: control.read("state"), 120)
        wait_until("the desktop, past onboarding", lambda: on_the_desktop(control), 120)
        env["WAYLAND_DISPLAY"] = wait_until(
            "published Wayland display", lambda: wayland_display(args.artifacts / "compositor.log"))

        # 1. stdio and exit status, which also wakes (creates and boots) the VM.
        started = time.monotonic()
        result = vm.guest("echo hi; exit 3", timeout=900)
        print(f"first launch (create + boot) took {time.monotonic() - started:.0f}s", flush=True)
        check(result.stdout == "hi\n" and result.returncode == 3,
              f"stdout and exit status relay (got {result.stdout!r}, {result.returncode}; "
              f"stderr {result.stderr[-800:]!r})")

        # 2. a granted directory at the same path.
        shared = root / "shared"
        shared.mkdir()
        (shared / "marker.txt").write_text("from the host\n")
        granted = vm.run("grant", str(shared))
        check(granted.returncode == 0, f"grant {shared} ({granted.stderr[-400:]!r})")
        seen = vm.guest(f"cat '{shared}/marker.txt'")
        check(seen.stdout == "from the host\n",
              f"granted directory visible in the guest at {shared} "
              f"(got {seen.stdout!r}; stderr {seen.stderr[-800:]!r})")
        boot_before = vm.guest("cat /proc/sys/kernel/random/boot_id").stdout.strip()
        check(len(boot_before) == 36, f"guest boot id {boot_before!r}")
        status = vm.run("status", timeout=60).stdout
        snapshots = "vsock:   kernel" in status
        print(f"vsock transport: {'kernel vhost-vsock' if snapshots else 'vhost-user vsock'}",
              flush=True)

        # 3. a GUI app's window arrives as a a Linux VM's window.
        foot = vm.popen(
            "run", "--", "foot", "--app-id", "foot", "sh", "-c",
            f"echo linux VM window ready; read line; echo \"$line\" > /tmp/typed",
            log=foot_log,
        )
        frame = wait_until(f"{APP_ID} to map and present", lambda: frame_for(control, APP_ID), 180)
        dump = json.dumps(control.read("state"))
        check(APP_ID in dump, f"state dump lists the foreign tab {APP_ID}")
        shot = artifacts / "vm-window.png"
        control.send({"op": "snapshot", "path": str(shot)})
        wait_until("snapshot", shot.exists)
        time.sleep(0.5)
        colours = distinct_colours(shot, frame["rect"])
        check(colours >= 4, f"the VM window's frame is not flat ({colours} colours)")

        # 4. typed text reaches the app in the guest.
        rect = frame["rect"]
        control.send({"op": "click_at", "x": rect[0] + rect[2] / 2, "y": rect[1] + rect[3] / 2})
        time.sleep(0.5)
        for character in TYPED:
            control.send({"op": "type", "text": character})
            time.sleep(0.05)
        control.send({"op": "key", "key": "Enter"})
        wait_until("foot to exit after its line", lambda: foot.poll() is not None, 60)
        typed = vm.guest("cat /tmp/typed").stdout.strip()
        check(typed == TYPED, f"typed text arrived in the guest (got {typed!r})")

        # 5. idle suspend, then resume on launch.
        asleep = "phase:   suspended" if snapshots else "phase:   off"

        def sleeping() -> bool:
            return asleep in vm.run("status", timeout=60).stdout
        wait_until(f"the idle VM to reach {asleep!r}", sleeping, IDLE_SECONDS + 240)
        check(True, f"the idle VM went to sleep ({asleep.split()[-1]})")
        started = time.monotonic()
        back = vm.guest("echo back; cat /proc/sys/kernel/random/boot_id")
        lines = back.stdout.split()
        check(lines[:1] == ["back"], f"launch after suspend runs (got {back.stdout!r})")
        if snapshots:
            check(lines[1:2] == [boot_before],
                  f"the VM resumed from its snapshot rather than rebooting "
                  f"({time.monotonic() - started:.0f}s)")
        else:
            check(len(lines) == 2 and lines[1] != boot_before,
                  f"the VM booted again on launch ({time.monotonic() - started:.0f}s)")
    except Exception:
        try:
            (artifacts / "windows.json").write_text(
                json.dumps(windows(control), indent=2) + "\n")
            control.send({"op": "snapshot", "path": str(artifacts / "failed.png")})
        except Exception:
            pass
        raise
    finally:
        terminate(foot)
        subprocess.run([*vm.base, "stop"], env=env, capture_output=True, timeout=120)
        for log in (runtime / "tilo-linux-vm" / "linux" / "supervisor.log",
                    root / "data" / "tilo" / "linux-vm" / "linux" / "console.log"):
            if log.exists():
                (artifacts / log.name).write_bytes(log.read_bytes())
        for qemu_log in (root / "cache").glob("libvirt/qemu/log/*.log"):
            (artifacts / qemu_log.name).write_bytes(qemu_log.read_bytes())
        terminate(compositor)
        compositor_log.close()
        foot_log.close()


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--session", type=Path, required=True)
    parser.add_argument("--tilo-linux-vm", type=Path, required=True)
    parser.add_argument("--image", type=Path, required=True)
    parser.add_argument("--artifacts", type=Path, required=True)
    try:
        run(parser.parse_args())
    except Failure as failure:
        print(f"FAIL {failure}", flush=True)
        return 1
    print("Linux VM end-to-end: all checks passed", flush=True)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
