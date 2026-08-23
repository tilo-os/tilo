#!/usr/bin/env python3
"""Record a qOS boot as video, from firmware to the desktop.

vm.sh takes stills. This takes a film: it starts the VM and then dumps the
framebuffer in a loop, timestamping every frame, and encodes the result at the
rate the frames were actually captured. The video therefore runs at the speed
the machine really booted at -- a 40 second boot is 40 seconds of footage. No
frame is duplicated to pad a gap and none is dropped to shorten one.

    ./os/dev/record-boot.py --out boot.mp4
    ./os/dev/record-boot.py --mode gl --login --seconds 90

DISPLAY MODE is the one real decision here, and it is forced on us by QEMU:
`screendump` reads the 2D framebuffer, so under --mode gl (virtio-vga-gl, which
is what lets the compositor paint at all) every dump of the pre-compositor
console answers "no surface". Under --mode 2d the console and greeter capture
perfectly and the compositor falls back to llvmpipe. Neither mode sees
everything, so the script reports per-phase how many frames it actually got
rather than leaving a silent gap in the middle of the film:

    captured 412 frames, 38 empty ("no surface")

An empty stretch in that report is the honest shape of the capture, not a
failure to be smoothed over. Frames that returned no surface are omitted from
the video and their duration is carried onto the previous frame, so the clock
stays true.

Frames are normalised to one canvas before encoding because the resolution
genuinely changes during a boot (firmware, then the kernel console, then the
compositor's mode) and neither the concat demuxer nor image2 accepts a size
change mid-stream.
"""

from __future__ import annotations

import argparse
import os
import shutil
import socket
import threading
import subprocess
import sys
import time
from pathlib import Path

HERE = Path(__file__).resolve().parent
REPO = HERE.parent.parent
VM = HERE / "vm.sh"


def log(msg: str) -> None:
    print(f"\033[1;36m[rec]\033[0m {msg}", file=sys.stderr, flush=True)


def warn(msg: str) -> None:
    print(f"\033[1;33m[rec] WARN:\033[0m {msg}", file=sys.stderr, flush=True)


def die(msg: str) -> "NoReturn":  # type: ignore[valid-type]
    print(f"\033[1;31m[rec] ERROR:\033[0m {msg}", file=sys.stderr, flush=True)
    raise SystemExit(1)


class Monitor:
    """One long-lived connection to QEMU's HMP socket.

    vm.sh spawns a socat per command, which costs more than the screendump
    itself and puts a floor under the frame interval. The socket lives on a
    path shared with the container qemu runs in, so it can be opened directly.
    """

    def __init__(self, path: str, timeout: float = 10.0):
        self.sock = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        self.sock.settimeout(timeout)
        self.sock.connect(path)
        self.lock = threading.Lock()
        time.sleep(0.3)
        self._drain()

    # HMP prints "(qemu)" when it is ready for the next command, so a reply is
    # read up to that rather than by waiting out a timeout. Waiting instead put
    # a hard floor under the frame interval -- a 0.25s drain is 4 fps however
    # fast the screendump itself was.
    def _drain(self, deadline: float = 3.0) -> str:
        out = b""
        end = time.monotonic() + deadline
        self.sock.settimeout(0.05)
        try:
            while time.monotonic() < end:
                try:
                    chunk = self.sock.recv(65536)
                except (socket.timeout, TimeoutError):
                    if out.rstrip().endswith(b"(qemu)"):
                        break
                    continue
                if not chunk:
                    break
                out += chunk
                if out.rstrip().endswith(b"(qemu)"):
                    break
        finally:
            self.sock.settimeout(10.0)
        return out.decode("utf-8", "replace")

    def cmd(self, line: str) -> str:
        with self.lock:
            self.sock.sendall((line + "\n").encode())
            return self._drain()

    def close(self) -> None:
        try:
            self.sock.close()
        except OSError:
            pass


def greeter_up(serial: Path) -> bool:
    """Has greetd reached the point where it will accept a login?

    The same evidence vm.sh waits on. Typing at a fixed offset instead means
    the keystrokes land wherever the boot happened to be that run -- which on a
    slow boot is the kernel console, and the greeter then sits untouched for the
    rest of the take.
    """
    try:
        with open(serial, "rb") as fh:
            fh.seek(0, os.SEEK_END)
            fh.seek(max(0, fh.tell() - 131072))
            tail = fh.read().replace(b"\x00", b"")
    except OSError:
        return False
    return b"Started greetd.service" in tail or b"session-c1.scope" in tail


_KEYMAP = {
    " ": "spc", "-": "minus", ".": "dot", "/": "slash", "_": "shift-minus",
    ":": "shift-semicolon", ";": "semicolon", "@": "shift-2", "=": "equal",
    "+": "shift-equal", "|": "shift-backslash", "\\": "backslash",
    ">": "shift-dot", "<": "shift-comma", ",": "comma", "~": "shift-grave_accent",
    "`": "grave_accent", "#": "shift-3", "*": "shift-8", "&": "shift-7",
    "(": "shift-9", ")": "shift-0", "!": "shift-1", '"': "shift-apostrophe",
    "'": "apostrophe", "?": "shift-slash", "$": "shift-4", "%": "shift-5",
    "^": "shift-6", "{": "shift-bracket_left", "}": "shift-bracket_right",
    "[": "bracket_left", "]": "bracket_right",
}


def key_for(ch: str) -> str:
    if ch in _KEYMAP:
        return _KEYMAP[ch]
    if ch.isupper():
        return f"shift-{ch.lower()}"
    return ch


def _ppm_header(path: Path):
    """Open a binary PPM and return (file, width, height, pixel_offset)."""
    try:
        fh = open(path, "rb")
    except OSError:
        return None
    try:
        if fh.read(2) != b"P6":
            fh.close(); return None
        vals: list[int] = []
        while len(vals) < 3:                       # width, height, maxval
            tok = b""
            while True:
                c = fh.read(1)
                if not c:
                    fh.close(); return None
                if c.isspace():
                    if tok:
                        break
                    continue
                tok += c
            vals.append(int(tok))
        return fh, vals[0], vals[1], fh.tell()
    except (OSError, ValueError):
        fh.close(); return None


def _region_mean(path: Path, x0: int, y0: int, x1: int, y1: int) -> float | None:
    """Mean brightness of a rectangle of a binary PPM."""
    hdr = _ppm_header(path)
    if hdr is None:
        return None
    fh, w, h, base = hdr
    with fh:
        x1 = min(x1, w); y1 = min(y1, h)
        if x0 >= x1 or y0 >= y1:
            return None
        total = n = 0
        for y in range(y0, y1, 2):
            fh.seek(base + (y * w + x0) * 3)
            row = fh.read((x1 - x0) * 3)
            total += sum(row); n += len(row)
        return (total / n) if n else None


class GreeterWatch:
    """Decide when the greeter is up AND painted, from the framebuffer itself.

    Two events look alike and are half a minute apart under software rendering:
    the greeter taking the VT (which clears the console) and the greeter
    actually drawing its form. Typing at the first one puts the credentials
    nowhere -- the take ends on an empty login box, which is exactly what
    happened before this looked for the form instead.

    So it waits for the form: the greeter is a lighter panel centred on a dark
    field, which nothing else on the way through a boot looks like. Measured on
    real frames from this image, panel-minus-corner is +14.8 for the greeter,
    -3.2 for the desktop and -11.5 for the console.
    """

    PANEL_DELTA = 8.0

    def __init__(self) -> None:
        self.ready_at: float | None = None

    def feed(self, path: Path, elapsed: float) -> bool:
        if self.ready_at is not None:
            return True
        panel = _region_mean(path, 500, 200, 780, 560)
        corner = _region_mean(path, 0, 600, 300, 780)
        if panel is None or corner is None:
            return False
        if panel - corner > self.PANEL_DELTA:
            self.ready_at = elapsed
            return True
        return False


def run_vm(*args: str, check: bool = True) -> subprocess.CompletedProcess:
    return subprocess.run([str(VM), *args], cwd=REPO, check=check)


def capture(mon: Monitor, frames_dir: Path, fps: float, seconds: float,
            login_at: float | None, auto_login: bool, serial: Path,
            after_login: float | None, user: str, password: str,
            settle: float) -> list[tuple[float, Path | None]]:
    """Dump frames until the time budget runs out. Returns (timestamp, path|None)."""
    interval = 1.0 / fps
    watch = GreeterWatch()

    def do_login() -> None:
        # Typed over the monitor connection this recorder already holds. Going
        # through `vm.sh keys` spawns a container per keystroke, so clearing the
        # field and typing two words took minutes -- the take ended on an empty
        # login box while the keys were still being delivered.
        time.sleep(settle)          # let it finish painting before typing
        def send(k: str) -> None:
            mon.cmd(f"sendkey {k}")
            time.sleep(0.03)
        for _ in range(24):         # clear whatever username is remembered
            send("backspace")
        for ch in user:
            send(key_for(ch))
        send("tab")                 # Return here submits; the form wants Tab
        for ch in password:
            send(key_for(ch))
        send("ret")
        log("login typed")
    t0 = time.monotonic()
    shots: list[tuple[float, Path | None]] = []
    n = 0
    logged_in = False
    login_t = None

    while True:
        now = time.monotonic()
        elapsed = now - t0
        if elapsed >= seconds:
            break
        if login_t is not None and after_login is not None and elapsed - login_t >= after_login:
            log(f"t+{elapsed:.1f}s: {after_login:.0f}s past the login, stopping")
            break

        if not logged_in and (
                (login_at is not None and elapsed >= login_at)
                or (auto_login and greeter_up(serial) and watch.ready_at is not None)):
            log(f"t+{elapsed:.1f}s: greeter is up, typing the login")
            # `vm.sh login` submits the username with Return, which this greeter
            # reads as "log in" and answers with "Enter a password" while the
            # password lands nowhere. Its form is two fields: clear the
            # remembered username, type it, Tab to the password, and submit.
            threading.Thread(target=do_login, daemon=True).start()
            logged_in = True
            login_t = elapsed

        path = frames_dir / f"f{n:06d}.ppm"
        try:
            reply = mon.cmd(f"screendump {path}")
        except (OSError, socket.timeout) as exc:
            warn(f"monitor went away at t+{elapsed:.1f}s: {exc}")
            break

        if "no surface" in reply.lower():
            shots.append((elapsed, None))
        else:
            # screendump returns before the file is fully on disk.
            for _ in range(40):
                if path.exists() and path.stat().st_size > 0:
                    break
                time.sleep(0.005)
            shots.append((elapsed, path if path.exists() else None))
            if auto_login and not logged_in and path.exists():
                watch.feed(path, elapsed)
        n += 1

        if n % 50 == 0:
            got = sum(1 for _, p in shots if p)
            log(f"t+{elapsed:.1f}s  {got}/{len(shots)} frames")

        slack = interval - (time.monotonic() - now)
        if slack > 0:
            time.sleep(slack)

    return shots


def normalise(shots, out_dir: Path, width: int, height: int) -> list[tuple[float, Path]]:
    """Put every frame on one canvas so the encoder accepts the sequence."""
    out_dir.mkdir(parents=True, exist_ok=True)
    kept = [(t, p) for t, p in shots if p is not None]
    if not kept:
        die("no frame was ever captured -- nothing to encode")

    magick = shutil.which("magick") or shutil.which("convert")
    if not magick:
        die("ImageMagick (magick/convert) not found on the host")

    log(f"normalising {len(kept)} frames to {width}x{height}")
    done: list[tuple[float, Path]] = []
    for i, (t, src) in enumerate(kept):
        dst = out_dir / f"n{i:06d}.png"
        done.append((t, dst))

    # One process per frame is the bulk of the wall time here; feed them in
    # chunks instead.
    chunk = 64
    for i in range(0, len(done), chunk):
        procs = []
        for (t, dst), (_, src) in zip(done[i:i + chunk], kept[i:i + chunk]):
            procs.append(subprocess.Popen(
                [magick, str(src), "-resize", f"{width}x{height}",
                 "-background", "black", "-gravity", "center",
                 "-extent", f"{width}x{height}", str(dst)],
                stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL))
        for p in procs:
            p.wait()
    missing = [d for _, d in done if not d.exists()]
    if missing:
        die(f"{len(missing)} frames failed to convert (first: {missing[0]})")
    return done


def encode(frames: list[tuple[float, Path]], out: Path, tail: float) -> float:
    """Encode at real time: each frame lasts until the next one was captured."""
    ffmpeg = shutil.which("ffmpeg")
    if not ffmpeg:
        die("ffmpeg not found on the host")

    listing = out.parent / "frames.ffconcat"
    lines = ["ffconcat version 1.0"]
    for i, (t, path) in enumerate(frames):
        nxt = frames[i + 1][0] if i + 1 < len(frames) else t + tail
        dur = max(nxt - t, 0.001)
        lines.append(f"file '{path}'")
        lines.append(f"duration {dur:.4f}")
    # The concat demuxer drops the final entry's duration unless the file is
    # named once more after it.
    lines.append(f"file '{frames[-1][1]}'")
    listing.write_text("\n".join(lines) + "\n")

    span = frames[-1][0] - frames[0][0] + tail
    out.parent.mkdir(parents=True, exist_ok=True)
    log(f"encoding {len(frames)} frames spanning {span:.1f}s -> {out}")
    subprocess.run(
        [ffmpeg, "-y", "-loglevel", "error",
         "-f", "concat", "-safe", "0", "-i", str(listing),
         "-fps_mode", "cfr", "-r", "30",
         "-c:v", "libx264", "-preset", "medium", "-crf", "20",
         "-pix_fmt", "yuv420p", str(out)],
        check=True)
    return span


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__,
                                 formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--out", default=".claude/demo-artifacts/qos-boot.mp4")
    ap.add_argument("--mode", choices=["2d", "gl"], default="2d",
                    help="2d captures console+greeter; gl captures what the compositor paints")
    ap.add_argument("--fps", type=float, default=10.0)
    ap.add_argument("--seconds", type=float, default=75.0)
    ap.add_argument("--login-at", type=float, default=None,
                    help="seconds into the boot to type the greeter login")
    ap.add_argument("--login", action="store_true",
                    help="type the login as soon as greetd reports itself up")
    ap.add_argument("--user", default=os.environ.get("QOS_USER", "quickuser"))
    ap.add_argument("--password", default=os.environ.get("QOS_PASS", "quickdesktop"))
    ap.add_argument("--login-settle", type=float, default=6.0,
                    help="seconds to let the greeter paint before typing")
    ap.add_argument("--after-login", type=float, default=None,
                    help="stop this many seconds after the login (keeps the clip tight)")
    ap.add_argument("--disk", default=None)
    ap.add_argument("--width", type=int, default=1280)
    ap.add_argument("--height", type=int, default=800)
    ap.add_argument("--keep-frames", action="store_true")
    ap.add_argument("--no-boot", action="store_true",
                    help="attach to a VM that is already running")
    args = ap.parse_args()

    run_dir = Path(os.environ.get("QOS_RUN", "/tmp/qos-vm"))
    mon_path = run_dir / "mon.sock"

    out = Path(args.out)
    if not out.is_absolute():
        out = REPO / out
    work = out.parent / (out.stem + "-frames")
    if work.exists():
        shutil.rmtree(work)
    raw = work / "raw"
    raw.mkdir(parents=True, exist_ok=True)

    if not args.no_boot:
        subprocess.run([str(VM), "stop"], cwd=REPO,
                       stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, check=False)
        boot = ["boot", f"--{args.mode}"]
        if args.disk:
            boot += ["--disk", args.disk]
        log(f"booting ({args.mode})")
        run_vm(*boot)

    for _ in range(100):
        if mon_path.exists():
            break
        time.sleep(0.1)
    if not mon_path.exists():
        die(f"no monitor socket at {mon_path}")

    mon = Monitor(str(mon_path))
    log(f"recording {args.seconds:.0f}s at {args.fps:g} fps")
    try:
        shots = capture(mon, raw, args.fps, args.seconds, args.login_at,
                        args.login, run_dir / "serial.log", args.after_login,
                        args.user, args.password, args.login_settle)
    finally:
        mon.close()

    got = sum(1 for _, p in shots if p)
    empty = len(shots) - got
    log(f"captured {got} frames, {empty} empty (\"no surface\")")
    if empty:
        first_empty = next(t for t, p in shots if p is None)
        last_empty = max(t for t, p in shots if p is None)
        warn(f"the framebuffer was unreadable from t+{first_empty:.1f}s to "
             f"t+{last_empty:.1f}s -- that stretch is missing from the video")

    frames = normalise(shots, work / "norm", args.width, args.height)
    span = encode(frames, out, tail=1.0 / args.fps)

    if not args.keep_frames:
        shutil.rmtree(work, ignore_errors=True)
        (out.parent / "frames.ffconcat").unlink(missing_ok=True)

    size = out.stat().st_size
    log(f"wrote {out} ({size/1e6:.1f} MB, {span:.1f}s of real boot time)")
    print(out)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
