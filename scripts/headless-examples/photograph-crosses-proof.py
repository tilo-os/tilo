#!/usr/bin/env python3
"""A photograph on the second machine's screen, not a placeholder.

A document holds a *reference* to its pictures, never the bytes: a
146 KB image inlined into an Automerge change is larger than the
transport will carry, and a change that is dropped freezes every replica
downstream of it. The reference is a few dozen bytes and crosses without
trouble. The bytes have to travel some other way, and for a while they
did not travel at all — the far screen showed a grey box reading
"image" on a document that was otherwise perfectly converged.

This is the proof that they cross now, and it is deliberately made
awkward for itself:

  * **Two real compositors**, separate homes, separate device keys,
    joined into one room by an invite minted on the first and pasted
    into the second — the way a person joins one.
  * **A real word processor in each**, a separate sandboxed process with
    a blob store of its own that neither shell can read. That is the
    boundary the bytes have to cross twice, and it is the reason the
    obvious fix (put the two on one filesystem) is not available: the
    launcher gives every app a private HOME and XDG_CACHE_HOME, on
    purpose.
  * **Only Quinten's copy has the picture.** Xaver opens a file of his
    own, on his own home, with no image in it. Both apps opening one
    file off one disk would prove nothing whatsoever — they would agree
    if the room did not exist.

So the only route from the photograph to Xaver's screen is:

    quinten's app  ──OfferBlob──▶  quinten's shell
                                        │  BlobChunk
                                        ▼
                                   xaver's shell  ──BlobArrived──▶  xaver's app

and the payoff is read off Xaver's own frame: the region of his page
where the image sits stops being paper. `pixels_changed` measures it
against the same page before the room delivered anything, so a blinking
caret cannot be mistaken for a photograph.

Usage:
  ./photograph-crosses-proof.py [--shots DIR] [--profile demo|debug]

Exits non-zero if the picture does not arrive.
"""
import argparse
import base64
import json
import os
import pathlib
import socket
import struct
import subprocess
import sys
import tempfile
import time
import zlib

from reaper import die_with_parent, track

REPO = pathlib.Path(__file__).resolve().parents[2]
PROFILES = ("demo", "debug")
APP = "dev.qos.word-processor"


def _matched_pair(profile: str | None = None):
    """The compositor and the app from **one** profile.

    They share the toolkit and the Wayland/collaboration surface, and a
    demo-profile app against a debug-profile shell dies on its first
    frame — which reads as "the app never painted", i.e. as a fault in
    the app rather than in the pairing.
    """
    if profile:
        shell = REPO / "target" / profile / "shell"
        app = REPO / "target" / profile / "word-processor"
        return (shell, app) if shell.exists() and app.exists() else (None, None)
    best = None
    for prof in PROFILES:
        shell = REPO / "target" / prof / "shell"
        app = REPO / "target" / prof / "word-processor"
        if not (shell.exists() and app.exists()):
            continue
        age = min(shell.stat().st_mtime, app.stat().st_mtime)
        if best is None or age > best[0]:
            best = (age, shell, app)
    return (best[1], best[2]) if best else (None, None)


SHELL, WORD_PROCESSOR = _matched_pair()

FAILURES: list[str] = []


def check(label: str, ok: bool, detail: str = "") -> bool:
    # `detail` explains a failure; printing it on success reads as though the
    # thing it warns about happened anyway.
    print(f"  [{'ok' if ok else 'FAIL'}] {label}" + (f" — {detail}" if detail and not ok else ""))
    if not ok:
        FAILURES.append(label)
    return ok


ANSI = __import__("re").compile(r"\x1b\[[0-9;]*m")


def note(msg: str) -> None:
    print(f"       {msg}")


def wait_for(pred, timeout=30.0, poll=0.4):
    end = time.time() + timeout
    while time.time() < end:
        try:
            v = pred()
        except Exception:
            v = None
        if v:
            return v
        time.sleep(poll)
    return None


# ---------------------------------------------------------------------------
# The photograph.
# ---------------------------------------------------------------------------

def a_real_png(width: int = 480, height: int = 300) -> bytes:
    """A picture with real content, encoded as a real PNG.

    Not a 1x1 fixture and not noise. It has to survive being looked at:
    a sky that darkens upward, a sun, and a dark ridge across the lower
    third, so that "is the photograph there" is answerable by a person
    opening the frame as well as by counting changed pixels. It also has
    to be *large* — the whole difficulty was a payload that does not fit
    in one message, and a small image would cross by accident.
    """
    rows = bytearray()
    cx, cy, r = width * 0.68, height * 0.30, height * 0.11
    horizon = int(height * 0.63)
    # Deterministic grain. A flat gradient compresses to a few KB, and a
    # few KB would cross in a single message — which is precisely the case
    # that never broke. Photographs carry sensor noise; this carries the
    # equivalent, so the payload is the size a photograph is and has to be
    # split to travel at all.
    seed = 0x2545F4914F6CDD1D
    for y in range(height):
        rows.append(0)  # PNG filter byte: none
        for x in range(width):
            seed ^= (seed << 13) & 0xFFFFFFFFFFFFFFFF
            seed ^= seed >> 7
            seed ^= (seed << 17) & 0xFFFFFFFFFFFFFFFF
            grain = ((seed >> 33) & 0x1F) - 15
            if y < horizon:
                t = y / max(1, horizon)
                r8, g8, b8 = (
                    int(60 + 150 * t),
                    int(90 + 120 * t),
                    int(170 + 60 * t),
                )
                if (x - cx) ** 2 + (y - cy) ** 2 < r * r:
                    r8, g8, b8 = 255, 236, 170
            else:
                t = (y - horizon) / max(1, height - horizon)
                ridge = 1.0 if ((x // 37) % 2 == 0) else 0.88
                r8 = int((40 + 60 * t) * ridge)
                g8 = int((55 + 70 * t) * ridge)
                b8 = int((45 + 55 * t) * ridge)
            rows += bytes((
                min(255, max(0, r8 + grain)),
                min(255, max(0, g8 + grain)),
                min(255, max(0, b8 + grain)),
            ))

    def chunk(tag: bytes, body: bytes) -> bytes:
        return (struct.pack(">I", len(body)) + tag + body
                + struct.pack(">I", zlib.crc32(tag + body) & 0xFFFFFFFF))

    return (b"\x89PNG\r\n\x1a\n"
            + chunk(b"IHDR", struct.pack(">IIBBBBB", width, height, 8, 2, 0, 0, 0))
            + chunk(b"IDAT", zlib.compress(bytes(rows), 6))
            + chunk(b"IEND", b""))


def html_with_image(png: bytes) -> str:
    """A document whose import path stores the image as a blob.

    The HTML codec turns a `data:` URI into a content-addressed blob
    (`crates/doc/qdoc/src/codecs/html.rs`), which is exactly the call the
    drop-a-file-in gesture makes. Going in through a file rather than
    through synthetic drag means the app does the storing, in its own
    process, which is the half of the journey this proof exists for.
    """
    uri = "data:image/png;base64," + base64.b64encode(png).decode()
    return (
        "<html><body>"
        "<h1>Harbour, Tuesday</h1>"
        "<p>The photograph below has to reach the other machine.</p>"
        f'<img src="{uri}"/>'
        "<p>And the typing after it has to keep arriving.</p>"
        "</body></html>"
    )


def html_without_image() -> str:
    """What the second app opens: nothing.

    Empty rather than "the same document minus the picture", and the
    reason is worth writing down. Two apps that each *import* a file
    build two separately-created Automerge documents; merging those
    leaves one side's blocks addressed by a root key the other side's
    version of that key wins, so the second app sits looking at its own
    copy and the first one's page never arrives. An app that imported
    nothing has nothing to win with, so the whole document it is handed
    is the document it shows — which is the state a peer joining a room
    is actually in.
    """
    return "<html><body></body></html>"


# ---------------------------------------------------------------------------
# Reading a frame back.
# ---------------------------------------------------------------------------

def load_png(path: str):
    """(width, height, rgb bytes) for a PNG written by the compositor.

    Hand-rolled rather than pillow: this repo's harness scripts carry no
    third-party imaging dependency, and the compositor writes plain
    non-interlaced RGB/RGBA.
    """
    data = open(path, "rb").read()
    if not data.startswith(b"\x89PNG\r\n\x1a\n"):
        return None
    pos, idat, w, h, chans = 8, bytearray(), 0, 0, 3
    while pos + 8 <= len(data):
        ln = struct.unpack(">I", data[pos:pos + 4])[0]
        tag = data[pos + 4:pos + 8]
        body = data[pos + 8:pos + 8 + ln]
        if tag == b"IHDR":
            w, h, depth, colour = struct.unpack(">IIBB", body[:10])
            if depth != 8 or colour not in (2, 6):
                return None
            chans = 3 if colour == 2 else 4
        elif tag == b"IDAT":
            idat += body
        elif tag == b"IEND":
            break
        pos += 12 + ln
    if not w or not h:
        return None
    raw = zlib.decompress(bytes(idat))
    stride = w * chans
    out = bytearray()
    prev = bytearray(stride)
    p = 0
    for _ in range(h):
        ftype = raw[p]; p += 1
        line = bytearray(raw[p:p + stride]); p += stride
        if ftype == 1:
            for i in range(chans, stride):
                line[i] = (line[i] + line[i - chans]) & 0xFF
        elif ftype == 2:
            for i in range(stride):
                line[i] = (line[i] + prev[i]) & 0xFF
        elif ftype == 3:
            for i in range(stride):
                left = line[i - chans] if i >= chans else 0
                line[i] = (line[i] + ((left + prev[i]) >> 1)) & 0xFF
        elif ftype == 4:
            for i in range(stride):
                a = line[i - chans] if i >= chans else 0
                b = prev[i]
                c = prev[i - chans] if i >= chans else 0
                pa, pb, pc = abs(b - c), abs(a - c), abs(a + b - 2 * c)
                pred = a if (pa <= pb and pa <= pc) else (b if pb <= pc else c)
                line[i] = (line[i] + pred) & 0xFF
        prev = line
        if chans == 3:
            out += line
        else:
            for i in range(0, stride, 4):
                out += line[i:i + 3]
    return w, h, bytes(out)


def pixels_changed(before: str, after: str) -> int | None:
    """How many pixels differ between two frames, beyond noise.

    A threshold rather than an exact compare: a caret blinks, and a
    photograph appearing is four orders of magnitude larger than that.
    """
    a, b = load_png(before), load_png(after)
    if not a or not b or a[0] != b[0] or a[1] != b[1]:
        return None
    pa, pb = a[2], b[2]
    n = 0
    for i in range(0, len(pa) - 2, 3):
        if (abs(pa[i] - pb[i]) + abs(pa[i + 1] - pb[i + 1])
                + abs(pa[i + 2] - pb[i + 2])) > 24:
            n += 1
    return n


# ---------------------------------------------------------------------------
# Sessions.
# ---------------------------------------------------------------------------

class Session:
    """One real compositor, isolated from the machine and from its peer."""

    def __init__(self, name: str, root: str):
        self.name = name
        self.dir = os.path.join(root, name)
        self.home = os.path.join(self.dir, "home")
        os.makedirs(self.home, exist_ok=True)
        self.socket = os.path.join(self.dir, "control.sock")
        self.runtime = os.path.join(self.dir, "run")
        os.makedirs(self.runtime, mode=0o700, exist_ok=True)
        machine_home = os.path.expanduser("~")
        env = {
            **os.environ,
            "QOS_TEST_CONTROL": self.socket,
            "QOS_STATE_PATH": os.path.join(self.dir, "config", "state.json"),
            "XDG_DATA_HOME": os.path.join(self.dir, "data"),
            "XDG_CACHE_HOME": os.path.join(self.dir, "cache"),
            "XDG_STATE_HOME": os.path.join(self.dir, "state"),
            "XDG_CONFIG_HOME": os.path.join(self.dir, "config"),
            "XDG_RUNTIME_DIR": self.runtime,
            # Machine state the isolation must not take with it.
            "QOS_APPS_DATA_DIR": os.path.join(machine_home, ".local/share/qos-apps"),
            "QOS_RUNTIME_BUNDLE_DIR": os.path.join(machine_home, ".local/share/qos-runtime"),
            # Without its own device key a session's peer-sync turns
            # itself off, and the only symptom is a missing peer.* action.
            "QOS_PEER_KEY_PATH": os.path.join(self.dir, "keys", "peer-key"),
            "NIRIOS_HEADLESS_SIZE": "1600x1200",
            # The shell says what it did with a document's content at `info`,
            # and those lines are the account of the hop this script is about.
            # Without them a run that moved the bytes and one that dropped them
            # produce identical logs, which is the condition the whole defect
            # lived in.
            "RUST_LOG": "warn,shell::shell::network=info,shell::shell::collab_broker=info",
            "LC_ALL": "en_US.UTF-8",
            "LANG": "en_US.UTF-8",
            "LANGUAGE": "en_US",
        }
        env.pop("DISPLAY", None)
        env.pop("WAYLAND_DISPLAY", None)
        self.env = env
        self.log_path = os.path.join(self.dir, "shell.log")
        os.makedirs(self.dir, exist_ok=True)
        self.log = open(self.log_path, "w")
        self.proc = track(subprocess.Popen(
            [str(SHELL), "--backend", "headless", "--enable-peer-sync"],
            env=env, stdout=self.log, stderr=subprocess.STDOUT,
            preexec_fn=die_with_parent(),
        ))

    @property
    def broker(self) -> str:
        return os.path.join(self.runtime, "qos", "services", "collab-json.sock")

    def wait_up(self, timeout=90.0) -> bool:
        end = time.time() + timeout
        while time.time() < end:
            if os.path.exists(self.socket):
                time.sleep(1.0)
                return True
            if self.proc.poll() is not None:
                return False
            time.sleep(0.3)
        return False

    def call(self, payload: dict, timeout: float = 60.0) -> dict:
        s = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        s.settimeout(timeout)
        s.connect(self.socket)
        try:
            s.sendall((json.dumps(payload) + "\n").encode())
            buf = b""
            while not buf.endswith(b"\n"):
                chunk = s.recv(65536)
                if not chunk:
                    break
                buf += chunk
        finally:
            s.close()
        return json.loads(buf.decode())

    def chrome(self) -> dict:
        return self.call({"op": "state_dump"})["state"]["chrome"]

    def invoke(self, provider: str, ident: str) -> dict:
        return self.call({"op": "actions_invoke", "provider": provider, "id": ident})

    def enumerate(self, query: str = "") -> list:
        r = self.call({"op": "actions_enumerate", "query": query})
        return r.get("answer", {}).get("actions", []) or []

    def frames(self) -> list[dict]:
        r = self.call({"op": "foreign_frames"})
        if "foreign_frames" in r:
            return r["foreign_frames"].get("frames", [])
        return r.get("answer", r.get("frames", [])) or []

    def snapshot(self, path: str) -> bool:
        self.call({"op": "snapshot", "path": path})
        for _ in range(40):
            if os.path.exists(path) and os.path.getsize(path) > 0:
                return True
            time.sleep(0.25)
        return False

    def log_text(self) -> str:
        try:
            return open(self.log_path, errors="replace").read()
        except OSError:
            return ""

    def stop(self):
        for fn in (self.proc.terminate, self.proc.kill):
            try:
                fn()
                self.proc.wait(timeout=8)
                break
            except Exception:
                continue
        try:
            self.log.close()
        except Exception:
            pass


UNPAINTED = object()


def launch_word_processor(session: Session, room: str, doc: str,
                          timeout: float = 150.0):
    """Start the real word processor on `doc` as a client of `session`."""
    display_file = os.path.join(session.runtime, "qos-session", "wayland-display")
    if not wait_for(lambda: os.path.exists(display_file), timeout=45):
        note(f"{session.name}: no wayland display at {display_file}")
        return None
    display = open(display_file).read().strip()
    if not wait_for(lambda: os.path.exists(session.broker), timeout=45):
        note(f"{session.name}: no collaboration broker at {session.broker}")
        return None

    session.call({"op": "place_next_window", "app_id": APP})
    env = dict(session.env)
    env.update({
        "WAYLAND_DISPLAY": display,
        "QUICK_COLLAB_SOCK": session.broker,
        "QOS_PACKAGE_ID": APP,
        "QOS_WORKSPACE_ID": room,
        "QUICK_APP_ID": session.name,
        "QOS_USER_ID": session.name,
        "RUST_LOG": "warn,ui=info,qdoc=info",
        "QUICK_COLLAB_DEBUG": "1",
    })
    log_path = os.path.join(session.dir, "word-processor.log")
    before = {f.get("surface_id") for f in session.frames()}
    proc = track(subprocess.Popen(
        [str(WORD_PROCESSOR), doc], env=env,
        stdout=subprocess.DEVNULL, stderr=open(log_path, "w"),
        preexec_fn=die_with_parent(),
    ))
    end = time.time() + timeout
    while time.time() < end:
        if proc.poll() is not None:
            note(f"{session.name}: the word processor exited; see {log_path}")
            return None
        for f in session.frames():
            if f.get("surface_id") not in before and f.get("frame_seq", 0) > 0:
                return f
        time.sleep(1.0)
    note(f"{session.name}: the word processor never painted; see {log_path}")
    return None


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--shots", help="where to write frames")
    ap.add_argument("--profile", choices=PROFILES)
    args = ap.parse_args()

    global SHELL, WORD_PROCESSOR
    if args.profile:
        SHELL, WORD_PROCESSOR = _matched_pair(args.profile)
    if not SHELL or not WORD_PROCESSOR:
        print("missing shell / word-processor in target/demo or target/debug")
        print("build them:  cargo build -p shell --bin shell -p word-processor")
        return 2
    print(f"  shell:          {SHELL}")
    print(f"  word processor: {WORD_PROCESSOR}")

    # AF_UNIX paths cap near 108 bytes and a session binds several
    # sockets under its root, so a long prefix fails the bind and reads
    # as "the session never came up".
    root = tempfile.mkdtemp(prefix="/tmp/qp-")
    shots = args.shots or os.path.join(root, "shots")
    os.makedirs(shots, exist_ok=True)
    print(f"session root: {root}")

    png = a_real_png()
    print(f"the photograph: {len(png)} bytes — several transport chunks, "
          f"which is the whole difficulty")

    sessions: list[Session] = []
    try:
        print("\n[1] two real sessions")
        quinten = Session("quinten", root); sessions.append(quinten)
        xaver = Session("xaver", root); sessions.append(xaver)
        for s in sessions:
            if not check(f"{s.name} came up", s.wait_up(), f"see {s.log_path}"):
                return 1

        print("\n[2] one room, formed the way a person forms one")
        exported = {a["id"] for a in quinten.enumerate("")}
        if not check("peer-sync is on", "peer.invite" in exported):
            note(f"check {quinten.log_path} for 'device key load failed'")
            return 1
        quinten.invoke("actions", "peer.invite")
        token = wait_for(lambda: quinten.chrome()["peer_sync"]["invite_token"], timeout=30)
        if not check("quinten minted an invite", bool(token)):
            return 1
        quinten.call({"op": "key", "key": "Escape"})
        room = json.loads(base64.urlsafe_b64decode(
            token + "=" * (-len(token) % 4)))["room_id"]

        xaver.invoke("actions", "peer.connect")
        wait_for(lambda: xaver.chrome()["peer_sync"]["connect_input"] is not None,
                 timeout=25)
        xaver.call({"op": "type", "text": token})
        xaver.call({"op": "key", "key": "Enter"})
        joined = wait_for(
            lambda: (xaver.chrome()["peer_sync"]["status"] or "").startswith("Joined"),
            timeout=40)
        if not check("xaver joined quinten's room", bool(joined)):
            return 1
        xaver.call({"op": "key", "key": "Escape"})
        note(f"room = {room}")

        print("\n[3] the picture is on one machine only")
        # One path, because an app announces its record as `file:<path>` and
        # two different paths are two different documents that never bridge.
        # The *content* at that path differs between the two launches: the
        # picture is written in for Quinten's app to import, then taken out
        # again before Xaver's app opens it. An app reads the file once, at
        # startup, so after that the disk has no further say.
        doc = os.path.join(root, "harbour.html")
        with open(doc, "w") as fh:
            fh.write(html_with_image(png))
        win = launch_word_processor(quinten, room, doc)
        if not check("quinten's word processor painted", win is not None,
                     f"see {os.path.join(quinten.dir, 'word-processor.log')}"):
            return 1

        with open(doc, "w") as fh:
            fh.write(html_without_image())
        check("the file on disk no longer holds the photograph",
              b"data:image/png" not in open(doc, "rb").read())
        win = launch_word_processor(xaver, room, doc)
        if not check("xaver's word processor painted", win is not None,
                     f"see {os.path.join(xaver.dir, 'word-processor.log')}"):
            return 1
        # The disk is the control from here on. Anything of the photograph
        # that shows up on Xaver's page came through the room, because there
        # is nowhere else it could have come from.
        note("both apps opened the same record; only quinten's read a picture")

        # Both apps must have announced themselves before anything is
        # expected to cross, or a slow attach reads as a lost transfer.
        time.sleep(8.0)
        before = os.path.join(shots, "1-xaver-before.png")
        check("xaver's page photographed before anything crossed",
              xaver.snapshot(before))

        print("\n[4] the bytes cross")
        # Nothing is typed and nothing is clicked. Quinten's app stored the
        # image when it imported the file; from there the whole journey is
        # the room's.
        #
        # Four separate things had to happen, and the defect could have been
        # any one of them: never offered, never asked for, never delivered,
        # or delivered somewhere the reader does not look. Each is checked
        # on its own so a failure names which.
        def both_logs():
            return quinten.log_text(), xaver.log_text()

        wait_for(lambda: "content arrived and verified" in xaver.log_text(),
                 timeout=90, poll=2.0)
        q_log, x_log = both_logs()
        check("quinten's shell took the content from its own app and offered it",
              "offered a local app's content to the room" in q_log,
              "the app's bytes never reached the shell, so there was nothing to send")
        check("the content arrived on xaver's machine and verified against its hash",
              "content arrived and verified" in x_log,
              "the bytes did not cross")
        check("xaver's shell handed it down to the apps in the room",
              "handing arrived content to the apps in this room" in x_log,
              "the shell has the picture and the app that draws it does not")

        # The request is the *recovery* path, and whether it runs depends on
        # timing this script does not control: when the unsolicited offer
        # lands, nobody needs to ask. Requiring it here would fail a run in
        # which everything worked first time. Its own coverage is the unit
        # test, which drops the offer deliberately and then insists on the
        # question being asked.
        asked = "asking the room for it" in x_log
        answered = "sent on request from a peer" in q_log
        note("the recovery path ran too: xaver asked and quinten answered"
             if asked and answered else
             "the offer landed first time, so nothing had to be asked for")

        for name, log in (("quinten", q_log), ("xaver", x_log)):
            for line in ANSI.sub("", log).splitlines():
                if "shell_network:" in line and ("content" in line or "asking" in line):
                    said = line.split("shell_network: ")[-1].strip()
                    note(f"{name}: {said.split(' room=')[0][:110]}")

        print("\n[5] and the photograph is on xaver's page")
        landed, moved, waited = False, 0, 0.0
        start = time.time()
        after = os.path.join(shots, "2-xaver-after.png")
        while time.time() - start < 60.0:
            if xaver.snapshot(after):
                d = pixels_changed(before, after)
                if d is not None:
                    moved = max(moved, d)
                    if d > 8000:
                        landed = True
                        waited = time.time() - start
                        break
            time.sleep(2.0)

        check("the photograph is on xaver's screen", landed,
              f"largest change seen: {moved} pixels")
        note(f"before: {before}")
        note(f"after:  {after}")
        if landed:
            note(f"it took {waited:.1f}s from both apps being up — a "
                 f"{len(png)}-byte picture across a chunked transport")
        elif "content arrived and verified" in x_log:
            # Worth separating, because these are two different faults and
            # only one of them is about moving bytes. The content is on the
            # machine and in the app's own store; if the page is still blank
            # then what has not arrived is the *document* — two apps that
            # each imported a file hold separately-created Automerge
            # documents, and merging those leaves one side's blocks behind
            # whichever root key wins.
            note("the bytes are on xaver's machine and were handed to his app, "
                 "so this is not the transport: his document did not adopt "
                 "quinten's blocks, and there is no image block to draw")

        return 1 if FAILURES else 0
    finally:
        for s in sessions:
            s.stop()
        print(f"\nframes and logs under {root}")
        if FAILURES:
            print("failed: " + "; ".join(FAILURES))


if __name__ == "__main__":
    sys.exit(main())
