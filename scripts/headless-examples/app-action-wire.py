#!/usr/bin/env python3
"""Reach into a running application from outside its process.

An application publishes what it has open and what may be done to it over
the socket the session hands it. This script is the other end of that,
and it is deliberately not the application: it starts a compositor,
starts the word processor as a separate process beside it, and then does
everything else through `QOS_TEST_CONTROL` — no keystroke, no click, no
pixel.

What it proves, in order, and each read back rather than assumed:

  1. the application's own operations appear in `actions_enumerate`,
     addressed by the app's own ids and by nothing positional
  2. `actions_invoke` on one of them changes the *application's*
     document, read back by a third process that asks the app for it
  3. the same invocation is refused when the caller lacks the
     capability, and the refusal is visible
  4. an application that exits stops offering anything, and the address
     that worked a moment ago is refused

    scripts/headless-examples/app-action-wire.py
    scripts/headless-examples/app-action-wire.py --keep

Build first:

    cargo build --profile demo -p shell --bin shell \\
        -p word-processor -p qdoc --bin qos-doc-text
"""
from __future__ import annotations

import argparse
import json
import os
import shutil
import socket
import subprocess
import sys
import tempfile
import threading
import time
import zlib

REPO = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
TARGET = os.environ.get("CARGO_TARGET_DIR") or os.path.join(REPO, "target")

APP_ID = "dev.qos.word-processor"
#: The provider every app-published operation is addressed under.
PROVIDER = "app"
#: The room both the compositor and the app address. Any non-nil uuid: the
#: broker rejects the nil one deliberately, because an unminted room would
#: make every workspace on every machine one shared room.
ROOM = "11111111-2222-3333-4444-555555555555"

FAIL: list[str] = []


def note(msg: str) -> None:
    print(f"  {msg}", flush=True)


def claim(ok: bool, msg: str) -> bool:
    print(f"  [{'ok ' if ok else 'FAIL'}] {msg}", flush=True)
    if not ok:
        FAIL.append(msg)
    return ok


def binary(name: str) -> str:
    for profile in ("demo", "release", "debug"):
        p = os.path.join(TARGET, profile, name)
        if os.path.exists(p):
            return p
    sys.exit(f"{name} is not built under {TARGET}; see the module docstring.")


# -- the control socket -------------------------------------------------------

def call(sock_path: str, payload: dict, timeout: float = 20.0) -> dict:
    s = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
    s.settimeout(timeout)
    s.connect(sock_path)
    try:
        s.sendall((json.dumps(payload) + "\n").encode())
        buf = b""
        while not buf.endswith(b"\n"):
            chunk = s.recv(65536)
            if not chunk:
                break
            buf += chunk
        return json.loads(buf.decode() or "{}")
    finally:
        s.close()


# -- reading a running app's document -----------------------------------------

class DocumentReader:
    """A late joiner that asks the running app for the document it holds.

    Speaks the length-prefixed JSON the toolkit speaks on the broker's
    legacy listener, announces the same `(app_id, cluster_id)` address the
    word processor announced, and sends the one frame every toolkit
    answers with its whole document. What comes back was therefore
    produced by the application, out of the document the application is
    holding — not by this script, and not by the compositor.
    """

    SCOPE = "word-processor"

    def __init__(self, sock_path: str, room: str) -> None:
        self.frames: list[dict] = []
        self.sock = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        self.sock.settimeout(10.0)
        self.sock.connect(sock_path)
        self._send({"Hello": {"app_id": APP_ID, "cluster_id": room,
                              "record": None, "handle": "readback"}})
        self._send({"Presence": {"key": {"Workspace": {"id": self.SCOPE}},
                                 "payload": "Arrived"}})
        threading.Thread(target=self._read, daemon=True).start()

    def _send(self, obj: dict) -> None:
        body = json.dumps(obj).encode()
        self.sock.sendall(len(body).to_bytes(4, "little") + body)

    def _read(self) -> None:
        try:
            f = self.sock.makefile("rb")
            while True:
                hdr = f.read(4)
                if not hdr or len(hdr) < 4:
                    return
                body = f.read(int.from_bytes(hdr, "little"))
                if not body:
                    return
                try:
                    self.frames.append(json.loads(body.decode()))
                except Exception:
                    pass
        except Exception:
            pass

    def documents(self) -> list[bytes]:
        return [bytes(m["CRDTUpdate"]["data"])
                for m in list(self.frames) if "CRDTUpdate" in m]

    def close(self) -> None:
        try:
            self.sock.close()
        except OSError:
            pass


def doc_strings(blob: bytes) -> list[str]:
    """Every string the document holds, as `<path>\\t<text>` lines.

    Delegated to `qos-doc-text`, which walks the object graph: Automerge
    DEFLATE-compresses its columns, so a phrase present in a document is
    absent from its bytes and grepping the blob answers "empty" for every
    document. A missing decoder raises rather than answering "absent" —
    unreadable and absent are different facts and only one is about the
    product.
    """
    out = subprocess.run([binary("qos-doc-text"), "-"], input=blob,
                         capture_output=True, timeout=30)
    if out.returncode not in (0, 1):
        raise RuntimeError(f"qos-doc-text could not read the document: "
                           f"{out.stderr.decode()[:200]}")
    return sorted(out.stdout.decode(errors="replace").splitlines())


def app_document(json_sock: str, room: str, tries: int = 25) -> list[bytes]:
    """Ask the app for its document, giving it a few beats to answer."""
    reader = DocumentReader(json_sock, room)
    try:
        for _ in range(tries):
            time.sleep(0.2)
            docs = reader.documents()
            if docs:
                return docs
        return []
    finally:
        reader.close()


def spawn_app(wp_bin: str, app_env: dict, collab_sock: str,
              wayland: str, log) -> subprocess.Popen:
    """Start the application the way the launcher would.

    The launcher dials the broker, hands the child the connected fd by number,
    and then **closes its own copy**. That last step is not tidiness: a socket
    is open while anyone holds it, so a parent that kept its copy would leave
    the RPC session alive after the application had exited -- and the session
    would go on offering operations for a process that is gone. Which is
    exactly what this harness did until it closed the fd here.
    """
    conn = dial(collab_sock)
    env = dict(app_env)
    env["QOS_SVC_COLLAB_FD"] = str(conn.fileno())
    env["WAYLAND_DISPLAY"] = wayland
    app = subprocess.Popen([wp_bin], env=env, stdout=log,
                           stderr=subprocess.STDOUT, pass_fds=(conn.fileno(),))
    conn.close()
    return app


def png_has_content(path: str) -> bool | None:
    """Whether a PNG holds more than one colour. `None` if it cannot be read.

    A compositor whose GL cannot open a screen -- `failed to create dri2
    screen`, which is this machine's state and several CI machines' -- still
    answers a snapshot, and answers it with a uniform frame. A check that took
    the file's existence for evidence would pass on every one of them and
    report a rendered palette nobody rendered. So the pixels are looked at, and
    a blank one is said out loud.
    """
    try:
        raw = open(path, "rb").read()
    except OSError:
        return None
    if not raw.startswith(b"\x89PNG\r\n\x1a\n"):
        return None
    idat, i = b"", 8
    while i + 8 <= len(raw):
        n = int.from_bytes(raw[i:i + 4], "big")
        kind = raw[i + 4:i + 8]
        if kind == b"IDAT":
            idat += raw[i + 8:i + 8 + n]
        elif kind == b"IEND":
            break
        i += 12 + n
    if not idat:
        return None
    try:
        flat = zlib.decompress(idat)
    except zlib.error:
        return None
    # Sampled rather than counted in full: a 1920x1080 frame is 8 MB of
    # scanline and the question only needs two different values.
    seen = set(flat[::997])
    return len(seen) > 2


def dial(path: str, tries: int = 100) -> socket.socket:
    """A connected socket to the session broker, inheritable by a child.

    The one thing the launcher would otherwise do for a packaged app: hand it
    an already-connected fd. Retried, because a compositor that has just been
    restarted binds this a moment after its control socket appears, and a
    connect that raced it would look exactly like a broker that is not there.
    """
    last: Exception | None = None
    for _ in range(tries):
        s = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        try:
            s.connect(path)
            os.set_inheritable(s.fileno(), True)
            return s
        except OSError as e:
            last = e
            s.close()
            time.sleep(0.1)
    raise RuntimeError(f"the session broker at {path} never accepted: {last}")


# -- the run ------------------------------------------------------------------

def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--keep", action="store_true",
                    help="leave the run directory behind")
    args = ap.parse_args()

    shell_bin = binary("shell")
    wp_bin = binary("word-processor")
    run_dir = tempfile.mkdtemp(prefix="qos-actionwire-")
    runtime_dir = os.path.join(run_dir, "rt")
    os.makedirs(runtime_dir, mode=0o700, exist_ok=True)
    control = os.path.join(run_dir, "control.sock")
    collab_sock = os.path.join(runtime_dir, "qos", "services", "collab.sock")
    json_sock = os.path.join(runtime_dir, "qos", "services", "collab-json.sock")

    env = dict(os.environ)
    for k in ("DISPLAY", "WAYLAND_DISPLAY", "QUICK_COLLAB_SOCK"):
        env.pop(k, None)
    env.update({
        "XDG_RUNTIME_DIR": runtime_dir,
        "HOME": run_dir,
        "XDG_CONFIG_HOME": os.path.join(run_dir, "config"),
        "XDG_DATA_HOME": os.path.join(run_dir, "data"),
        "QOS_TEST_CONTROL": control,
        "QOS_WORKSPACE_ID": ROOM,
        # Who is sitting at this session. Named rather than inherited so the
        # workspace has a known owner, which is what step 4 needs somebody
        # else to not be.
        "QOS_USER_ID": "quinten",
        # Named explicitly: `dirs::config_dir()` ignores a relative
        # XDG_CONFIG_HOME, and a harness that isolates itself badly writes
        # into the real session's state.
        "QOS_STATE_PATH": os.path.join(run_dir, "config", "qos", "state.json"),
        # `shell::shell::app_action_wire` is where the session records an
        # application's surface arriving; step 4 reads that back, so the filter
        # has to select it by the target it is actually logged under.
        "RUST_LOG": "shell::shell::app_action_wire=info,warn",
    })

    shell_log = open(os.path.join(run_dir, "compositor.log"), "wb")
    app_log = open(os.path.join(run_dir, "app.log"), "wb")
    comp = subprocess.Popen([shell_bin, "--backend", "headless"], env=env,
                            stdout=shell_log, stderr=subprocess.STDOUT)
    app: subprocess.Popen | None = None
    try:
        print("\n1. a compositor, and the word processor beside it")
        for _ in range(200):
            if os.path.exists(control) and os.path.exists(collab_sock):
                break
            if comp.poll() is not None:
                return fail_out(run_dir, "the compositor exited before it bound "
                                         "its sockets")
            time.sleep(0.1)
        if not os.path.exists(collab_sock):
            return fail_out(run_dir, f"no broker socket at {collab_sock}")
        note(f"compositor up; broker at {collab_sock}")

        display_file = os.path.join(runtime_dir, "qos-session", "wayland-display")
        for _ in range(100):
            if os.path.exists(display_file):
                break
            time.sleep(0.1)
        wayland = open(display_file).read().strip() if os.path.exists(display_file) else ""
        if not wayland:
            return fail_out(run_dir, "the compositor published no wayland socket")

        # The one thing the launcher would otherwise do: hand the app a
        # *connected* fd to the broker. Everything downstream of this is the
        # ordinary path a packaged app takes.
        app_env = dict(env)
        app_env.update({
            "QOS_PACKAGE_ID": APP_ID,
            "HOME": os.path.join(run_dir, "app-home"),
        })
        os.makedirs(app_env["HOME"], exist_ok=True)
        app = spawn_app(wp_bin, app_env, collab_sock, wayland, app_log)
        note(f"word processor started as pid {app.pid}, "
             f"a separate process on {wayland}")

        # 2. what it offers, from outside it -------------------------------
        print("\n2. what the application says can be done to it")
        def published_now() -> list[dict]:
            resp = call(control, {"op": "actions_enumerate", "query": ""})
            return [a for a in (resp.get("answer") or {}).get("actions") or []
                    if a.get("provider") == PROVIDER]

        offered: list[dict] = []
        for _ in range(120):
            time.sleep(0.25)
            if app.poll() is not None:
                return fail_out(run_dir, f"the app exited (code {app.returncode}); "
                                         f"see {run_dir}/app.log")
            offered = published_now()
            if offered:
                break
        if not offered:
            # The word processor anchors most of its operations on the caret,
            # and a document nothing has ever been typed into may not have one
            # yet. One keystroke gives it one. Printed, because "it published
            # on its own" and "it published once it had a caret" are different
            # claims and only one of them was true.
            note("nothing published yet; giving the document a caret with one "
                 "keystroke, which is the app's own precondition rather than "
                 "this wire's")
            call(control, {"op": "type", "text": "x"})
            for _ in range(60):
                time.sleep(0.25)
                offered = published_now()
                if offered:
                    break
        claim(bool(offered),
              f"actions_enumerate returns operations from the running "
              f"application ({len(offered)} of them)")
        if not offered:
            return fail_out(run_dir, "nothing was published")
        for a in offered[:6]:
            note(f"({a['provider']}, {a['id']})  {a['title']}")
        ids = {a["id"] for a in offered}
        claim(all(i.startswith("word-processor.") for i in ids),
              "every address is the app's own resource id, and none is a "
              "screen position")

        target = "word-processor.document#insert-table"
        claim(target in ids, f"the address to invoke is offered: {target}")

        # And the same rows a person sees. The catalogue enumeration above and
        # the command palette are one registry, so this is not a second
        # rendering of the same list -- it is the evidence that they are the
        # same list, in a picture somebody can check.
        shot = os.path.join(run_dir, "palette-with-app-actions.png")
        call(control, {"op": "modifiers", "logo": True})
        call(control, {"op": "key", "key": "k"})
        call(control, {"op": "modifiers"})
        time.sleep(0.6)
        call(control, {"op": "type", "text": "table"})
        time.sleep(0.8)
        call(control, {"op": "snapshot", "path": shot})
        drawn = png_has_content(shot)
        if drawn:
            claim(True,
                  f"the palette, with the application's operations in it: {shot}")
        else:
            # Not a failure about the product: enumeration above already read
            # the same rows out of the same registry. It is a failure to be
            # able to look, and those are different things.
            note(f"GAP: the snapshot at {shot} came back blank, so the palette "
                 f"could not be looked at here. This compositor's GL reports "
                 f"`failed to create dri2 screen`; `shots --mode offscreen` "
                 f"renders on a different path and is unaffected.")
        call(control, {"op": "key", "key": "Escape"})
        time.sleep(0.4)

        # 3. invoking it changes the application's document ----------------
        print("\n3. invoking one, and reading the result out of the app")
        before = app_document(json_sock, ROOM)
        claim(bool(before), "the application answered with the document it holds")
        before_strings = doc_strings(before[-1]) if before else []
        note(f"before: {len(before_strings)} strings in the app's document")

        resp = call(control, {"op": "actions_invoke",
                              "provider": PROVIDER, "id": target})
        claim("err" not in resp,
              f"actions_invoke was accepted: {json.dumps(resp)[:180]}")
        time.sleep(1.5)

        after = app_document(json_sock, ROOM)
        after_strings = doc_strings(after[-1]) if after else []
        note(f"after:  {len(after_strings)} strings in the app's document")
        added = [s for s in after_strings if s not in before_strings]
        claim(bool(added),
              f"the application's own document changed "
              f"({len(added)} new strings)")
        for s in added[:6]:
            note(f"+ {s}")

        # And the app's *published surface* changed with it: a table it did
        # not have is a resource it did not offer.
        grew = []
        for _ in range(20):
            time.sleep(0.25)
            resp = call(control, {"op": "actions_enumerate", "query": ""})
            now = {a["id"] for a in (resp.get("answer") or {}).get("actions") or []
                   if a.get("provider") == PROVIDER}
            grew = sorted(now - ids)
            if grew:
                break
        claim(bool(grew),
              "the application republished a surface that grew by the thing "
              "the invocation made")
        for g in grew[:4]:
            note(f"+ ({PROVIDER}, {g})")

        # 4. the gate ------------------------------------------------------
        print("\n4. the same address, to a caller who does not own the room")
        # A second session over the *same* persisted workspace, sitting at it
        # as somebody else. The room is still owned by the first user, so this
        # one holds no write capability in it -- which is the state the gate
        # exists for, and the only honest way to reach it is to be that
        # caller rather than to ask the gate to pretend.
        app.terminate()
        app.wait(timeout=10)
        app = None
        comp.terminate()
        comp.wait(timeout=15)
        stranger_control = os.path.join(run_dir, "control2.sock")
        senv = dict(env)
        senv["QOS_USER_ID"] = "stranger"
        senv["QOS_TEST_CONTROL"] = stranger_control
        comp = subprocess.Popen([shell_bin, "--backend", "headless"], env=senv,
                                stdout=shell_log, stderr=subprocess.STDOUT)
        for _ in range(200):
            if os.path.exists(stranger_control) and os.path.exists(collab_sock):
                break
            time.sleep(0.1)
        for _ in range(100):
            if os.path.exists(display_file):
                break
            time.sleep(0.1)
        wayland = open(display_file).read().strip()
        owner = ((call(stranger_control, {"op": "state_dump"}).get("answer") or {})
                 .get("workspace_tree") or {})
        note(f"this session is sitting at the same desktop as `stranger`")

        app = spawn_app(wp_bin, app_env, collab_sock, wayland, app_log)
        note(f"the word processor is running again, pid {app.pid}, and "
             f"publishing to this session")
        # Give it as long as the successful run took to publish, so an
        # empty answer here is a refusal rather than a race.
        answering = False
        for _ in range(80):
            time.sleep(0.25)
            j = call(stranger_control, {"op": "state_dump"})
            # A state dump answers `{"state": {...}}` -- the response envelope
            # is externally tagged, so the payload's own keys are one level in.
            if "state" in j or "answer" in j:
                answering = True
                break
        claim(answering, "the second session is answering")
        # As long as the first session took to be published to, so an empty
        # answer below is a refusal rather than a race.
        time.sleep(8.0)

        resp = call(stranger_control, {"op": "actions_enumerate", "query": ""})
        rows = [a for a in (resp.get("answer") or {}).get("actions") or []
                if a.get("provider") == PROVIDER]
        claim(not rows,
              "the application's operations are absent from this caller's "
              "enumeration -- not present and disabled")
        resp = call(stranger_control, {"op": "actions_invoke",
                                       "provider": PROVIDER, "id": target})
        code = (resp.get("err") or {}).get("code")
        claim(code == "unknown_action",
              f"and invoking the address is refused, in the same words an "
              f"address that does not exist gets: {json.dumps(resp.get('err'))}")

        # The app really is publishing to this session -- the refusal is the
        # gate, not an empty catalogue. Read that off the compositor's own log
        # rather than inferring it.
        log = open(os.path.join(run_dir, "compositor.log"), "rb").read().decode(
            errors="replace")
        claim(log.count("published what can be done to it") >= 2,
              "the compositor recorded this session receiving the app's "
              "surface, so the refusal above is the gate and not an empty "
              "catalogue")

        # 5. lifecycle -----------------------------------------------------
        print("\n5. the application exits")
        # Back to the owner, who can see the surface -- "it stopped being
        # offered" is only a claim where it was offered.
        app.terminate()
        app.wait(timeout=10)
        comp.terminate()
        comp.wait(timeout=15)
        comp = subprocess.Popen([shell_bin, "--backend", "headless"], env=env,
                                stdout=shell_log, stderr=subprocess.STDOUT)
        for _ in range(200):
            if os.path.exists(control) and os.path.exists(collab_sock):
                break
            time.sleep(0.1)
        for _ in range(100):
            if os.path.exists(display_file):
                break
            time.sleep(0.1)
        app = spawn_app(wp_bin, app_env, collab_sock,
                        open(display_file).read().strip(), app_log)
        back = False
        for _ in range(80):
            time.sleep(0.25)
            resp = call(control, {"op": "actions_enumerate", "query": ""})
            if [a for a in (resp.get("answer") or {}).get("actions") or []
                    if a.get("provider") == PROVIDER]:
                back = True
                break
        claim(back, "the owner is offered the application's operations again")
        app.terminate()
        try:
            app.wait(timeout=10)
        except subprocess.TimeoutExpired:
            app.kill()
            app.wait(timeout=5)
        gone = False
        for _ in range(40):
            time.sleep(0.25)
            resp = call(control, {"op": "actions_enumerate", "query": ""})
            left = [a for a in (resp.get("answer") or {}).get("actions") or []
                    if a.get("provider") == PROVIDER]
            if not left:
                gone = True
                break
        claim(gone, "an application that has exited offers nothing")
        resp = call(control, {"op": "actions_invoke",
                              "provider": PROVIDER, "id": target})
        claim((resp.get("err") or {}).get("code") == "unknown_action",
              f"and the address that worked a moment ago is refused: "
              f"{json.dumps(resp.get('err'))}")
        app = None
    finally:
        for p in (app, comp):
            if p is not None and p.poll() is None:
                p.terminate()
                try:
                    p.wait(timeout=10)
                except subprocess.TimeoutExpired:
                    p.kill()
        shell_log.close()
        app_log.close()

    if FAIL:
        print(f"\n{len(FAIL)} claim(s) failed; logs in {run_dir}")
        return 1
    print(f"\nevery claim above was read back. logs and the palette \
snapshot are in {run_dir}")
    if not args.keep:
        shutil.rmtree(run_dir, ignore_errors=True)
    return 0


def fail_out(run_dir: str, why: str) -> int:
    print(f"\nFAILED: {why}\n  logs in {run_dir}")
    return 1


if __name__ == "__main__":
    sys.exit(main())
