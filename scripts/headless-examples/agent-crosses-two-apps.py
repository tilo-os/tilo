#!/usr/bin/env python3
"""One sentence, two applications: "make this square and put it in the open document".

The question this answers is not whether the pieces exist. They do, and each
has its own proof: an application publishes its operations over the socket the
session hands it (`app-action-wire.py`), the photo editor exports a real crop
(`apps/photos/src/actions.rs`), and an agent can be told to open an application
it does not hold (`apps/agent/tests/room_surface.rs`). The question is whether
a model, handed the room as it actually stands, *composes* them — and that has
never been run end to end outside a unit test.

So nothing here is staged past the point of the question:

  * The photo editor is **not running**. If it ends up running, something
    asked the session to start it, and the session's own log says who.
  * The instruction names no operation, no application and no file. It is the
    sentence a person would say, and "this" is left to be resolved out of what
    the room exports.
  * The agent is launched exactly as the film launches it -- `--app ""` so its
    route is the room rather than one document, and one `--grant` per
    application it is allowed to cross into.

What is read back, in order:

  1. what the agent decided, off its own log -- the ids it chose, in order
  2. whether the photo editor was asked for, and by whom, off the compositor's
  3. whether a photo-editor process is now running
  4. whether the crop landed, out of the photo editor's own document
  5. whether the picture arrived in the word processor's document, asked of
     the word processor rather than of the file it might later save

A refusal is a result, not a failure of this script: it prints what the agent
said and why, and exits 1 only when the run could not be *performed* --
nothing built, no key, a compositor that never came up.

    scripts/headless-examples/agent-crosses-two-apps.py [--keep] [--no-model]

Build first:

    cargo build --profile demo --bin shell --bin word-processor --bin photos \\
                --bin file-explorer --bin qos-agent --bin qos-doc-text
    scripts/demo-apps-live.sh     # so the session can *launch* the photo editor

Every binary is named as its own `--bin`: the selector is global rather than
scoped to the `-p` before it, so a `-p photos` with another package's `--bin`
after it compiles the library and leaves whatever binary was already on disk.
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

REPO = os.path.dirname(os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
TARGET = os.environ.get("CARGO_TARGET_DIR") or os.path.join(REPO, "target")


def _film():
    """`scripts/yc-demo.py`, for the few things this must not restate.

    Loading a 7000-line module to read three constants out of it looks
    like a lot, and the alternative is worse: this script and the film
    have to be asking for *the same thing*, or a green run here says
    nothing about the take. They disagreed once — a crop here, a run
    sheet in the film, a table in the live rig — and each read correctly
    on its own. Importing costs about a quarter of a second and nothing
    else; that module does no work at import.
    """
    import importlib.util
    path = os.path.join(REPO, "scripts", "yc-demo.py")
    spec = importlib.util.spec_from_file_location("yc_demo_film", path)
    mod = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(mod)
    return mod


film = _film()

WP_APP = film.WORD_PROCESSOR
PHOTOS_APP = film.IMAGE_EDITOR
FILES_APP = film.FILES_APP
ROOM = "11111111-2222-3333-4444-555555555555"
PHOTO = film.PHOTO_NAME

#: The agent's name: what it is called in the room, the prefix an instruction
#: is addressed with, and the subject a grant has to name.
AGENT_NAME = "Claude"

#: The sentence, taken from the film rather than written again here. It
#: names no application, no operation and no file.
INSTRUCTION = film.CROP_INSTRUCTION

FAIL: list[str] = []


def note(msg: str) -> None:
    print(f"  {msg}", flush=True)


def claim(ok: bool, msg: str) -> bool:
    print(f"  [{'ok ' if ok else '--'}] {msg}", flush=True)
    return ok


def hard(ok: bool, msg: str) -> bool:
    """A claim whose failure means the run could not be performed."""
    print(f"  [{'ok ' if ok else 'FAIL'}] {msg}", flush=True)
    if not ok:
        FAIL.append(msg)
    return ok


def _inputs_newer_than(path: str) -> list[tuple[float, str]] | None:
    """Sources this binary was built from that have changed since it was.

    Read out of cargo's depfile, which for a binary spans the workspace
    crates it links. `None` means no depfile: the question cannot be asked.
    """
    dep = path + ".d"
    try:
        text = open(dep).read()
    except OSError:
        return None
    built = os.path.getmtime(path)
    newer: list[tuple[float, str]] = []
    for line in text.splitlines():
        if ":" not in line:
            continue
        for dep_path in line.split(":", 1)[1].split():
            if not dep_path.startswith("/"):
                continue
            try:
                m = os.path.getmtime(dep_path)
            except OSError:
                continue
            if m > built:
                newer.append((m, dep_path))
    newer.sort(reverse=True)
    return newer


def binary(name: str) -> str:
    """The built binary, and only if it was built from the tree as it stands."""
    for profile in ("demo", "release", "debug"):
        p = os.path.join(TARGET, profile, name)
        if not os.path.exists(p):
            continue
        newer = _inputs_newer_than(p)
        if newer is None:
            sys.exit(f"{p} has no depfile beside it ({name}.d), so whether it "
                     f"matches this tree cannot be established. Rebuild it.")
        if newer:
            when = time.strftime("%H:%M:%S", time.localtime(os.path.getmtime(p)))
            listed = "\n".join(
                f"    {os.path.relpath(f, REPO) if f.startswith(REPO + os.sep) else f}"
                for _, f in newer[:5])
            sys.exit(f"{p} was built at {when} from sources that have changed "
                     f"since -- {len(newer)} of them:\n{listed}\nRebuild it.")
        return p
    sys.exit(f"{name} is not built under {TARGET}; see the module docstring.")


# -- the control socket -------------------------------------------------------

def call(sock_path: str, payload: dict, timeout: float = 25.0) -> dict:
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
    """A late joiner that asks a running app for the document it holds.

    Speaks the length-prefixed JSON the toolkit speaks on the broker's legacy
    listener and announces the same `(app_id, cluster_id)` address the app
    announced. What comes back was produced by the application out of the
    document the application is holding.
    """

    def __init__(self, sock_path: str, room: str, app_id: str,
                 scope: str) -> None:
        self.frames: list[dict] = []
        self.sock = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        self.sock.settimeout(10.0)
        self.sock.connect(sock_path)
        self._send({"Hello": {"app_id": app_id, "cluster_id": room,
                              "record": None, "handle": "readback"}})
        self._send({"Presence": {"key": {"Workspace": {"id": scope}},
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
    """Every string a document holds, as `<path>\\t<text>` lines.

    Delegated to `qos-doc-text`: Automerge DEFLATE-compresses its columns, so
    grepping the blob answers "absent" for every document.
    """
    out = subprocess.run([binary("qos-doc-text"), "-"], input=blob,
                         capture_output=True, timeout=60)
    if out.returncode not in (0, 1):
        raise RuntimeError(f"qos-doc-text could not read it: "
                           f"{out.stderr.decode()[:200]}")
    return sorted(out.stdout.decode(errors="replace").splitlines())


def app_document(json_sock: str, room: str, app_id: str, scope: str,
                 tries: int = 30) -> list[bytes]:
    reader = DocumentReader(json_sock, room, app_id, scope)
    try:
        for _ in range(tries):
            time.sleep(0.2)
            docs = reader.documents()
            if docs:
                return docs
        return []
    finally:
        reader.close()


# -- a photograph -------------------------------------------------------------

def a_photograph(width: int = 512, height: int = 320) -> bytes:
    """The film's own photograph, so this proof and the take use one picture.

    Drawn rather than checked in — a binary blob in the tree is one nobody
    can review — and drawn *there* rather than here, because the aspect
    ratio is the whole point of the instruction and two generators drifting
    apart would mean this measured a crop of a different picture from the
    one on camera.
    """
    return film.a_real_png(width, height)


def png_size(raw: bytes) -> tuple[int, int] | None:
    if not raw.startswith(b"\x89PNG\r\n\x1a\n"):
        return None
    import struct
    return struct.unpack(">II", raw[16:24])


def png_has_content(path: str) -> bool | None:
    """Whether a PNG holds more than one colour. `None` if it cannot be read."""
    import zlib
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
    return len(set(flat[::997])) > 2


def blob_bytes(hash_hex: str, roots: list[str]) -> bytes | None:
    """The content a document names, out of whichever blob store holds it.

    A document carries a hash and a descriptor; the bytes live in a store
    addressed by content (`crates/doc/qdoc/src/blobstore.rs`), one per HOME.
    The agent, the applications and this script do not share a HOME here, so
    the hash is looked for in each of them rather than in one guessed root.
    """
    if not hash_hex or not all(c in "0123456789abcdef" for c in hash_hex):
        return None
    for root in roots:
        path = os.path.join(root, "qos", "blobs", hash_hex[:2], hash_hex)
        try:
            return open(path, "rb").read()
        except OSError:
            continue
    return None


# -- starting things ----------------------------------------------------------

def dial(path: str, tries: int = 120) -> socket.socket:
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


def spawn_app(app_bin: str, app_id: str, env: dict, collab_sock: str,
              wayland: str, home: str, log, argv: list[str] | None = None
              ) -> subprocess.Popen:
    """Start an application the way the launcher would.

    The launcher dials the broker, hands the child the connected fd by number
    and closes its own copy -- a socket is open while anyone holds it, so a
    parent that kept its copy would leave the RPC session alive after the
    application had exited.
    """
    conn = dial(collab_sock)
    e = dict(env)
    e["QOS_SVC_COLLAB_FD"] = str(conn.fileno())
    e["WAYLAND_DISPLAY"] = wayland
    e["QOS_PACKAGE_ID"] = app_id
    e["HOME"] = home
    os.makedirs(home, exist_ok=True)
    p = subprocess.Popen([app_bin] + (argv or []), env=e, stdout=log,
                         stderr=subprocess.STDOUT, pass_fds=(conn.fileno(),))
    conn.close()
    return p


def running_photo_editors(payload_bin: str) -> list[int]:
    """Pids of photo-editor processes, matched on the executable behind /proc.

    Read off `/proc/<pid>/exe` rather than a command-line pattern: a pattern
    broad enough to catch the app also catches this script and whatever is
    reading the log, and the answer then depends on who is watching.
    """
    pids = []
    for entry in os.listdir("/proc"):
        if not entry.isdigit():
            continue
        try:
            if os.path.realpath(f"/proc/{entry}/exe") == os.path.realpath(payload_bin):
                pids.append(int(entry))
        except OSError:
            continue
    return pids


# -- the run ------------------------------------------------------------------

def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--keep", action="store_true",
                    help="leave the run directory behind")
    ap.add_argument("--no-model", action="store_true",
                    help="decide by the agent's keyword vocabulary instead of "
                         "asking a model. Reproducible, and says so.")
    ap.add_argument("--instruction", default=INSTRUCTION)
    ap.add_argument("--wait", type=float, default=180.0,
                    help="seconds to give the agent before reading back")
    args = ap.parse_args()

    shell_bin = binary("shell")
    wp_bin = binary("word-processor")
    files_bin = binary("file-explorer")
    agent_bin = binary("qos-agent")

    # The store the session launches out of. The photo editor has to be
    # *installed* rather than merely built: `agent_opens::decide` looks it up
    # in `launcher::gui_apps()`, and an absent package is answered as "no
    # application called that is installed here" rather than as a refusal.
    live_apps = os.environ.get("QOS_APPS_LIVE_DIR") or \
        os.path.join(TARGET, "qos-apps-live")
    photos_payload = os.path.join(live_apps, "payloads", PHOTOS_APP,
                                  "bin", "photos")
    # Reassigned below to the copy the session actually launches out of.
    if not os.path.exists(photos_payload):
        sys.exit(f"no installed photo editor at {photos_payload}. Run "
                 f"scripts/demo-apps-live.sh -- without it the session has "
                 f"nothing to open and the question cannot be asked.")

    run_dir = tempfile.mkdtemp(prefix="qos-crossapp-")
    runtime_dir = os.path.join(run_dir, "rt")
    os.makedirs(runtime_dir, mode=0o700, exist_ok=True)
    home = os.path.join(run_dir, "home")
    os.makedirs(os.path.join(home, "Pictures"), exist_ok=True)
    control = os.path.join(run_dir, "control.sock")
    collab_sock = os.path.join(runtime_dir, "qos", "services", "collab.sock")
    json_sock = os.path.join(runtime_dir, "qos", "services", "collab-json.sock")
    apps_dir = os.path.join(run_dir, "apps")
    shutil.copytree(live_apps, apps_dir)
    # The binary the session will run is the one in *its* store, and a
    # process is recognised by the executable behind /proc/<pid>/exe — so
    # the path asked about has to be that copy and not the tree it came
    # from, or a running app reads as no app at all.
    photos_payload = os.path.join(apps_dir, "payloads", PHOTOS_APP,
                                  "bin", "photos")

    photo = a_photograph()
    for where in (os.path.join(home, PHOTO),
                  os.path.join(home, "Pictures", PHOTO)):
        with open(where, "wb") as fh:
            fh.write(photo)
    original = png_size(photo)
    note(f"the photograph is {original[0]}x{original[1]} -- not square, "
         f"which is what the instruction is about")

    env = dict(os.environ)
    for k in ("DISPLAY", "WAYLAND_DISPLAY", "QUICK_COLLAB_SOCK"):
        env.pop(k, None)
    # Keep every process this run starts off the sleeping discrete GPU.
    #
    # The film's own function, not a copy of it: this machine has an NVIDIA
    # card in D3Cold, creating a wgpu instance loads every Vulkan ICD, and
    # enumerating through that one reaches across PCIe to a powered-down
    # device. `yc-demo.py` applies this to everything it starts; a harness
    # driving the same binaries from outside it inherits the problem, which
    # shows up as `failed to create dri2 screen` and a compositor that renders
    # nothing a snapshot can read back.
    env = film.pin_gpu(env)
    env.update({
        "XDG_RUNTIME_DIR": runtime_dir,
        "HOME": home,
        "QOS_USER_HOME": home,
        "XDG_CONFIG_HOME": os.path.join(run_dir, "config"),
        "XDG_DATA_HOME": os.path.join(run_dir, "data"),
        "QOS_TEST_CONTROL": control,
        "QOS_WORKSPACE_ID": ROOM,
        "QOS_USER_ID": "quinten",
        "QOS_STATE_PATH": os.path.join(run_dir, "config", "qos", "state.json"),
        # The store the launch reads, and the runtime every packaged app pins.
        "QOS_APPS_DATA_DIR": apps_dir,
        "QOS_RUNTIME_BUNDLE_DIR": os.path.expanduser("~/.local/share/qos-runtime"),
        "QOS_STORE_CONFIG_DIR": os.path.join(run_dir, "store-config"),
        "QOS_STORE_CACHE_DIR": os.path.join(run_dir, "store-cache"),
        # English, pinned: this machine is German, and an app's own catalogue
        # resolved in another language is a survey the agent reads differently
        # from the one this script asserts on.
        "LC_ALL": "en_GB.UTF-8",
        "LANG": "en_GB.UTF-8",
        "LC_MESSAGES": "en_GB.UTF-8",
        "RUST_LOG": "info,shell::shell::agent_opens=info,"
                    "shell::shell::app_action_wire=info",
    })
    env.pop("LANGUAGE", None)
    for d in ("store-config", "store-cache"):
        os.makedirs(os.path.join(run_dir, d), exist_ok=True)

    shell_log_path = os.path.join(run_dir, "compositor.log")
    shell_log = open(shell_log_path, "wb")
    app_log = open(os.path.join(run_dir, "apps.log"), "wb")
    agent_log_path = os.path.join(run_dir, "agent.log")

    comp = subprocess.Popen([shell_bin, "--backend", "headless"], env=env,
                            stdout=shell_log, stderr=subprocess.STDOUT)
    started: list[subprocess.Popen] = []
    agent: subprocess.Popen | None = None
    try:
        print("\n1. a session, with a document open and no photo editor")
        for _ in range(300):
            if os.path.exists(control) and os.path.exists(collab_sock):
                break
            if comp.poll() is not None:
                return fail_out(run_dir, "the compositor exited before it "
                                         "bound its sockets")
            time.sleep(0.1)
        if not hard(os.path.exists(collab_sock), f"the broker is at {collab_sock}"):
            return fail_out(run_dir, "no broker socket")

        display_file = os.path.join(runtime_dir, "qos-session", "wayland-display")
        for _ in range(150):
            if os.path.exists(display_file):
                break
            time.sleep(0.1)
        wayland = open(display_file).read().strip() if os.path.exists(display_file) else ""
        if not hard(bool(wayland), "the compositor published a wayland socket"):
            return fail_out(run_dir, "no wayland socket")

        # The room the *session* is in, read off its own tree rather than
        # pinned from out here.
        #
        # This is the difference between four participants in one room and
        # four in two. An app this script starts takes its room from
        # `QOS_WORKSPACE_ID`, so pinning one made the apps and the readback
        # agree with each other and with nothing else. The photo editor is
        # not started from here — the *session* starts it, through the
        # ordinary launcher, and hands it the room its own active workspace
        # actually has. Pinned, that app joined `62bc…` while the agent sat
        # in `1111…`, and the agent's report — that it asked for the photo
        # editor and nothing came back — was true and about a room the
        # photo editor was never in.
        # `{"state": {...}}`, not `{"answer": ...}`: the response envelope is
        # externally tagged and a state dump is its own variant, so the
        # payload's keys sit one level in under `state`.
        dump = call(control, {"op": "state_dump"})
        tree = ((dump.get("state") or dump.get("answer") or {})
                .get("workspace_tree") or {})
        active = tree.get("active")
        room = next((w.get("room") for w in tree.get("workspaces") or []
                     if w.get("id") == active), None)
        if not hard(bool(room), "the session names the room its workspace is in"):
            return fail_out(run_dir, "no active workspace in the state dump")
        note(f"the session's own room is {room}")
        env["QOS_WORKSPACE_ID"] = room

        # The two applications a person would already have open. The photo
        # editor is deliberately not among them.
        #
        # The file manager first and the word processor second, because the
        # instruction below is *typed*: keystrokes go to whatever holds the
        # keyboard, which is the window that opened last. Started the other
        # way round, the sentence lands in the file manager's search field and
        # the document the agent is waiting on never says anything -- which is
        # a harness fault that reads exactly like an agent ignoring its name.
        files = spawn_app(files_bin, FILES_APP, env, collab_sock, wayland,
                          os.path.join(run_dir, "home-files"), app_log)
        started.append(files)
        time.sleep(3.0)
        wp = spawn_app(wp_bin, WP_APP, env, collab_sock, wayland,
                       os.path.join(run_dir, "home-wp"), app_log)
        started.append(wp)
        note(f"file manager pid {files.pid}, word processor pid {wp.pid}")

        # Wait until the word processor is publishing, so the instruction is
        # typed into a document that exists rather than into a starting app.
        offered: list[dict] = []
        for _ in range(160):
            time.sleep(0.25)
            for p, what in ((wp, "word processor"), (files, "file manager")):
                if p.poll() is not None:
                    return fail_out(run_dir, f"the {what} exited "
                                             f"(code {p.returncode})")
            resp = call(control, {"op": "actions_enumerate", "query": ""})
            offered = [a for a in (resp.get("answer") or {}).get("actions") or []
                       if a.get("provider") == "app"]
            if any(a["id"].startswith("word-processor.") for a in offered):
                break
        hard(bool(offered), f"the running applications publish their "
                            f"operations ({len(offered)} of them)")
        before_ids = {a["id"] for a in offered}
        note("no photo-editor operations are on offer, because it is not open: "
             + str(sorted(i for i in before_ids if i.startswith("photos."))))
        hard(not running_photo_editors(photos_payload),
             "no photo-editor process is running yet")

        # What the room grants the agent. One per application it may cross
        # into -- `Scope::App` is leave to open the application as well as to
        # act in it, which is the grant the instruction needs.
        print("\n2. what the room grants the agent")
        # `agent:Claude`, with the capital, because the subject is matched
        # against the name the connection announced on its `Hello` and matched
        # exactly: `agent_opens::decide` builds `Actor::agent(handle)` out of
        # that string, so a grant written `agent:claude` for an agent running
        # as `--name Claude` names a different principal and the open is
        # refused with "has not been given leave".
        subject = f"agent:{AGENT_NAME}"
        for app in (WP_APP, PHOTOS_APP, FILES_APP, "dev.qos.chat"):
            r = call(control, {"op": "grant", "subject": subject,
                               "scope": f"app:{app}", "verb": "write"})
            claim("err" not in r, f"{subject} may write in {app}: "
                                  f"{json.dumps(r)[:120]}")

        # 3. the instruction ------------------------------------------------
        print("\n3. the instruction, written into the open document")
        call(control, {"op": "type", "text": args.instruction})
        time.sleep(1.5)
        # Read back what *landed*, never what was sent: `type` emits US evdev
        # keycodes and the client resolves them through its own keymap, so a
        # colon can arrive as something else entirely and the agent's
        # addressing test then never fires.
        docs = app_document(json_sock, room, WP_APP, "word-processor")
        landed = doc_strings(docs[-1]) if docs else []
        typed = [s for s in landed if "laude" in s or "quare" in s]
        for s in typed[:4]:
            note(f"in the document: {s}")
        addressed = any(s.split("\t")[-1].strip().lower().startswith("claude:")
                        for s in landed)
        if not addressed:
            note("nothing in the document starts with `claude:`. What it "
                 "actually holds:")
            for s in landed[:40]:
                note(f"    {s}")
        hard(addressed,
             "the line in the document is addressed to the agent -- it starts "
             "with the literal prefix `claude:` that `Instruction::addressed` "
             "matches, rather than something a keymap turned it into")

        # 4. the agent ------------------------------------------------------
        print("\n4. the agent, pointed at the room rather than at one document")
        argv = [agent_bin, "--workspace", room, "--name", AGENT_NAME,
                "--app", "",
                "--grant", "word-processor", "--grant", "files",
                "--grant", "chat", "--grant", "photos",
                "--socket", json_sock,
                "--directory", home,
                "--once", "--pace-ms", "250",
                "--await-document-ms", "60000"]
        if args.no_model:
            argv.append("--no-model")
        agent_env = dict(env)
        # The key is read from a file the agent opens itself. It is never put
        # on a command line, in this environment, or in any log this writes.
        agent_env["HOME"] = os.path.expanduser("~")
        agent_env["ANTHROPIC_ENV_FILE"] = os.path.expanduser("~/tupo/birke/.env")
        with open(agent_log_path, "wb") as alog:
            agent = subprocess.Popen(argv, env=agent_env, stdout=alog,
                                     stderr=subprocess.STDOUT, cwd=REPO)
        note(f"qos-agent pid {agent.pid}")

        deadline = time.time() + args.wait
        opened_seen = False
        while time.time() < deadline and agent.poll() is None:
            time.sleep(1.0)
            if not opened_seen and running_photo_editors(photos_payload):
                opened_seen = True
                note("a photo-editor process appeared")
        if agent.poll() is None:
            note(f"the agent is still working after {args.wait:.0f}s; "
                 f"reading back what it has done so far")
        else:
            note(f"the agent exited {agent.returncode}")

        # 5. what happened --------------------------------------------------
        print("\n5. what the agent decided, off its own log")
        agent_text = open(agent_log_path, errors="replace").read()
        for line in agent_text.splitlines():
            if any(k in line for k in (
                    "action choice", "chose", "step begins", "step ",
                    "asked this session to open", "it opened", "refus",
                    "applied", "answered", "crossing", "resolved against")):
                note(line.strip()[:200])

        print("\n6. whether the photo editor was opened, and by whom")
        comp_text = open(shell_log_path, errors="replace").read()
        asked = [l for l in comp_text.splitlines()
                 if "because a participant asked" in l
                 or "refused to open an application" in l]
        for l in asked[:6]:
            note(l.strip()[:200])
        claim(bool(asked), "the session recorded an application being asked for")
        pids = running_photo_editors(photos_payload)
        claim(bool(pids) or opened_seen,
              f"a photo-editor process is running: {pids}")

        print("\n7. the photograph, out of the photo editor's own document")
        photos_docs = app_document(json_sock, room, PHOTOS_APP, "photos",
                                   tries=15)
        if photos_docs:
            for s in doc_strings(photos_docs[-1])[:12]:
                note(f"photos: {s}")
        claim(bool(photos_docs),
              "the photo editor answered with the document it holds")

        print("\n8. the open document, asked of the word processor")
        after = app_document(json_sock, room, WP_APP, "word-processor")
        after_strings = doc_strings(after[-1]) if after else []
        added = [s for s in after_strings if s not in landed]
        for s in added[:10]:
            note(f"+ {s}")
        image = [s for s in after_strings
                 if "image" in s.lower() or "blob" in s.lower()
                 or "hash" in s.lower()]
        for s in image[:6]:
            note(f"image reference: {s}")
        claim(bool(image),
              "the word processor's document holds an image reference")

        # And whether the picture that arrived is the one that went in.
        #
        # "Make this square" is the whole instruction, and an image reference
        # arriving proves only that the two applications were crossed. Whether
        # the crossing carried out the request is a different question, and
        # this is the form of it this rig can actually answer.
        #
        # The session writes the content it opens the photo editor onto into
        # `rt/qos-opened/<hash>.png` -- named by its own content hash. The
        # document names the hash of what was placed. Content addressing does
        # the rest: identical hashes mean the bytes that reached the page are
        # the bytes that went into the editor, and nothing in between changed
        # the photograph. Different hashes mean something did.
        #
        # Read off the two documents and the session's own file, never off the
        # agent's account of itself: an agent reporting "crop" in its log is
        # the claim under test, not evidence for it.
        placed_hash = next(
            (s.split("\t")[-1].strip() for s in after_strings
             if s.split("\t")[0].endswith("blob")), "")
        opened_dir = os.path.join(runtime_dir, "qos-opened")
        opened = [f[:-4] for f in sorted(os.listdir(opened_dir))
                  if f.endswith(".png")] if os.path.isdir(opened_dir) else []
        if not opened:
            note("the session never wrote the content it opened an application "
                 "onto, so what went in cannot be compared with what came out")
        else:
            note(f"the editor was opened onto {opened[0][:12]}…; the document "
                 f"names {placed_hash[:12] or 'nothing'}")
        claim(bool(placed_hash) and bool(opened)
              and placed_hash not in opened,
              "the picture in the document is not the one the editor was "
              "opened onto -- something between them changed the photograph")

        shot = os.path.join(run_dir, "after.png")
        call(control, {"op": "snapshot", "path": shot})
        drawn = png_has_content(shot)
        if drawn:
            claim(True, f"a frame of the session: {shot}")
        else:
            note(f"GAP: the snapshot at {shot} came back blank -- this "
                 f"compositor's GL cannot open a screen here, so the pixels "
                 f"could not be looked at. Everything above was read off the "
                 f"documents and the processes instead.")
    finally:
        for p in [agent] + started:
            if p is not None and p.poll() is None:
                p.terminate()
                try:
                    p.wait(timeout=10)
                except subprocess.TimeoutExpired:
                    p.kill()
        # Anything the *session* started is the session's to reap, and it is
        # gone when the compositor is -- but a leaked compositor holds GPU
        # file descriptors for every session on the machine, so this is not
        # left to chance.
        for pid in running_photo_editors(photos_payload):
            try:
                os.kill(pid, 15)
            except OSError:
                pass
        if comp.poll() is None:
            comp.terminate()
            try:
                comp.wait(timeout=15)
            except subprocess.TimeoutExpired:
                comp.kill()
        shell_log.close()
        app_log.close()

    print(f"\nlogs, documents and the frame are in {run_dir}")
    if FAIL:
        print(f"{len(FAIL)} thing(s) the run needed were missing")
        return 1
    if not args.keep:
        print("(kept anyway: this run is evidence, not a gate)")
    return 0


def fail_out(run_dir: str, why: str) -> int:
    print(f"\nCOULD NOT RUN: {why}\n  logs in {run_dir}")
    return 1


if __name__ == "__main__":
    sys.exit(main())
