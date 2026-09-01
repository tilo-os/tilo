#!/usr/bin/env python3
"""Two qOS sessions, one room, and an edit made inside a **sandbox**.

`two-shell-app-collab.py` starts two real compositors and proves an edit
crosses between them — but it launches its apps as ordinary child
processes pointed at the legacy JSON socket. A real app is not launched
that way. It is launched by `qos-launcher` into a bwrap sandbox, and it
reaches the broker through one descriptor the launcher hands it. That is
a different code path end to end, and it is the one the product runs.

So this harness goes the other way round: same two compositors, same
signed invite, same room — and then two *packaged* processes, launched
by the real launcher into real sandboxes, one in each session. One
authors an Automerge document and publishes it; the other waits on its
op stream and prints what it loaded. If the text the first one wrote
comes out of the second one's log, an edit made in a sandbox on one
machine converged in a sandbox on another.

    scripts/sandboxed-app-collab.py
    scripts/sandboxed-app-collab.py --keep

Needs: cargo build -p shell -p collab-probe -p launcher, and bwrap.
No display.
"""
from __future__ import annotations

import argparse
import base64
import json
import os
import shutil
import signal
import socket
import subprocess
import sys
import time

REPO = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))

# A Unix socket address is capped at ~108 bytes and the compositor puts
# its broker at <runtime>/qos/services/collab.sock. A runtime dir under
# the repo overflows that, the broker fails to bind, and every app then
# starts with collaboration silently off. Keep these short.
RUNTIME_BASE = os.environ.get("XDG_RUNTIME_DIR", f"/run/user/{os.getuid()}")

PACKAGE_ID = "dev.qos.collab-probe"

MANIFEST = """
[package]
id       = "%s"
version  = "0.1.0"
kind     = "app"

[publisher]
identity = "ed25519:0000000000000000000000000000000000000000000000000000000000000000"
signature = ""

[sandbox]
namespaces = ["user", "pid", "uts", "ipc", "mount", "net"]
seccomp    = "qos-default"
landlock   = "strict"

# The app itself gets no network. Collaboration is the compositor's
# Iroh, reached through the broker descriptor — an app never needs an
# egress grant to collaborate, and this manifest is where that shows.
[sandbox.net]
mode = "off"

[sandbox.portals]

# required@1: an absent broker socket must fail the launch, not start
# the app with collaboration quietly switched off.
[sandbox.services]
collab = "required@1"

[entrypoint]
bin = "bin/qos-collab-probe"

[providers]
mime = []
ext  = []
""" % PACKAGE_ID

FAILURES: list[str] = []


def step(n: str, text: str) -> None:
    print(f"\n=== {n}  {text}", flush=True)


def claim(description: str, actual, expected) -> None:
    good = actual == expected
    print(f"  [{'ok' if good else 'FAIL'}] {description}"
          f"{'' if good else f'  (got {actual!r}, wanted {expected!r})'}", flush=True)
    if not good:
        FAILURES.append(description)


def note(text: str) -> None:
    print(f"       {text}", flush=True)


def call(sock_path: str, payload: dict, timeout: float = 30.0) -> dict:
    s = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
    s.settimeout(timeout)
    s.connect(sock_path)
    try:
        s.sendall(json.dumps(payload).encode() + b"\n")
        buf = b""
        while b"\n" not in buf:
            chunk = s.recv(1 << 20)
            if not chunk:
                raise RuntimeError("compositor closed the connection mid-reply")
            buf += chunk
        return json.loads(buf.split(b"\n", 1)[0])
    finally:
        s.close()


def ok(resp: dict) -> dict:
    if "err" in resp:
        raise RuntimeError(f"compositor refused: {resp['err']}")
    return resp


class Session:
    """One running compositor plus the sandboxes launched into it."""

    def __init__(self, name: str, user: str, run_dir: str, runtime_dir: str,
                 sock: str, binary: str):
        self.name = name
        self.user = user
        self.dir = run_dir
        self.runtime_dir = runtime_dir
        self.sock = sock
        self.binary = binary
        self.proc: subprocess.Popen | None = None
        self.log_path = os.path.join(run_dir, "compositor.log")
        self._log = None

    @property
    def collab_sock(self) -> str:
        return os.path.join(self.runtime_dir, "qos", "services", "collab.sock")

    def base_env(self) -> dict:
        env = dict(os.environ)
        # Nothing inherited may make the two sessions share state, and
        # nothing may point a sandbox at a broker other than its own.
        for k in ("DISPLAY", "WAYLAND_DISPLAY", "QUICK_COLLAB_SOCK",
                  "QUICK_COLLAB_CAPNP_SOCK", "QOS_SVC_COLLAB_FD"):
            env.pop(k, None)
        env.update(
            XDG_RUNTIME_DIR=self.runtime_dir,
            XDG_CONFIG_HOME=os.path.join(self.dir, "config"),
            XDG_DATA_HOME=os.path.join(self.dir, "data"),
            XDG_CACHE_HOME=os.path.join(self.dir, "cache"),
            QOS_IDENTITY_ROOT=os.path.join(self.dir, "system-identity"),
        )
        return env

    def start(self) -> None:
        for sub in ("config", "data", "cache"):
            os.makedirs(os.path.join(self.dir, sub), exist_ok=True)
        os.makedirs(self.runtime_dir, mode=0o700, exist_ok=True)
        env = self.base_env()
        env.update(
            QOS_USER_ID=self.user,
            # The network and signing brokers must use the same certified
            # device key. Distinct roots still make these two local sessions
            # independent peers.
            QOS_PEER_KEY_PATH=os.path.join(
                self.dir, "system-identity", "device", "keypair.bin"),
            QOS_FS_ACL_PATH=os.path.join(self.dir, "config", "fs-capabilities.json"),
            QOS_TEST_CONTROL=self.sock,
            NIRIOS_HEADLESS_SIZE="1280x800",
            RUST_LOG=env.get("RUST_LOG", "info"),
        )
        provisioned = subprocess.run(
            [self.binary, "--firstboot", "--provision-identity"],
            cwd=REPO,
            env=env,
            stdout=subprocess.PIPE,
            stderr=subprocess.STDOUT,
            text=True,
        )
        if provisioned.returncode != 0:
            raise RuntimeError(
                f"{self.name}: identity provisioning failed:\n{provisioned.stdout}")
        self._log = open(self.log_path, "w")
        self.proc = subprocess.Popen(
            [self.binary, "--backend", "headless", "--enable-peer-sync"],
            cwd=REPO, env=env, stdout=self._log, stderr=subprocess.STDOUT,
            preexec_fn=os.setsid,
        )

    def wait_ready(self, timeout: float = 180.0) -> None:
        deadline = time.time() + timeout
        while time.time() < deadline:
            if self.proc and self.proc.poll() is not None:
                raise RuntimeError(
                    f"{self.name} exited before binding its control socket; "
                    f"see {self.log_path}")
            if os.path.exists(self.sock):
                try:
                    self.state()
                    return
                except (OSError, RuntimeError):
                    pass
            time.sleep(0.25)
        raise RuntimeError(f"{self.name} never answered on {self.sock}")

    def stop(self) -> None:
        if self.proc and self.proc.poll() is None:
            try:
                os.killpg(os.getpgid(self.proc.pid), signal.SIGTERM)
            except Exception:
                self.proc.terminate()
            try:
                self.proc.wait(timeout=10)
            except subprocess.TimeoutExpired:
                self.proc.kill()
        if self._log:
            self._log.close()
        # The runtime dir holds the compositor's control and broker
        # sockets, and it is named after this harness's pid. Leaving it
        # behind litters /run/user with one tree per run, and the next
        # run that happens to reuse the pid inherits a stale broker
        # socket rather than binding its own.
        shutil.rmtree(self.runtime_dir, ignore_errors=True)
        try:
            os.unlink(self.sock)
        except OSError:
            pass

    # -- driving -----------------------------------------------------

    def state(self) -> dict:
        return ok(call(self.sock, {"op": "state_dump"}))["state"]

    def chrome(self) -> dict:
        return self.state()["chrome"]

    def active_workspace(self) -> dict:
        tree = self.state()["workspace_tree"]
        for ws in tree["workspaces"]:
            if ws["id"] == tree["active"]:
                return ws
        raise RuntimeError(f"{self.name}: no active workspace")

    def has_local_write_membership(self) -> bool:
        person = self.chrome()["local_user_id"]
        for capability in self.active_workspace().get("capabilities", []):
            subject = capability.get("subject", {})
            principal = subject.get("Principal", {})
            if (principal.get("Person") == person
                    and capability.get("effect") == "Grant"
                    and capability.get("verb") in ("Write", "Admin")):
                return True
        return False

    def settle(self, seconds: float = 0.6) -> None:
        time.sleep(seconds)

    def key(self, name: str) -> None:
        ok(call(self.sock, {"op": "key", "key": name}))

    def type_text(self, text: str) -> None:
        for i in range(0, len(text), 200):
            ok(call(self.sock, {"op": "type", "text": text[i:i + 200]}))

    def invoke(self, ident: str, args: dict | None = None) -> dict:
        payload = {
            "op": "actions_invoke",
            "provider": "actions",
            "id": ident,
        }
        if args is not None:
            payload["args"] = args
        return call(self.sock, payload)

    def wait_for_action(self, ident: str, timeout: float = 30.0) -> bool:
        deadline = time.time() + timeout
        while time.time() < deadline:
            response = call(self.sock, {"op": "actions_enumerate", "query": ""})
            actions = response.get("answer", {}).get("actions", [])
            if any(action.get("id") == ident for action in actions):
                return True
            time.sleep(0.25)
        return False

    # -- sandboxes ---------------------------------------------------

    def launch_sandboxed(self, launcher: str, payload_root: str, manifest: str,
                         room: str, instruction: str, log_path: str
                         ) -> subprocess.Popen:
        """Start the probe under `qos-launcher`, in a real sandbox.

        No `--no-services`: resolving the collab socket and handing its
        descriptor across the sandbox boundary is the thing under test.
        """
        env = self.base_env()
        env["RUST_LOG"] = "info"
        argv = [
            launcher, "run",
            "--manifest", manifest,
            "--payload-root", payload_root,
            "--minimal-libs",
            "--workspace-id", room,
            "--open", instruction,
        ]
        lg = open(log_path, "w")
        return subprocess.Popen(argv, cwd=REPO, env=env, stdout=lg,
                                stderr=subprocess.STDOUT, preexec_fn=os.setsid)


def stage_payload(root: str, probe_bin: str) -> tuple[str, str]:
    """Lay the probe out as an unpacked package the launcher can run."""
    payload = os.path.join(root, "payload")
    os.makedirs(os.path.join(payload, "bin"), exist_ok=True)
    shutil.copy2(probe_bin, os.path.join(payload, "bin", "qos-collab-probe"))
    manifest = os.path.join(root, "manifest.toml")
    with open(manifest, "w") as f:
        f.write(MANIFEST)
    return payload, manifest


def read_log(path: str) -> str:
    try:
        with open(path) as f:
            return f.read()
    except OSError:
        return ""


def peer_id_of(token: str) -> str | None:
    """Read the device id from this session's freshly minted invitation."""
    try:
        padding = "=" * (-len(token) % 4)
        payload = json.loads(base64.urlsafe_b64decode(token + padding).decode())
        return payload.get("owner_peer_id") or None
    except (ValueError, UnicodeDecodeError, json.JSONDecodeError):
        return None


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--out", default=None)
    ap.add_argument("--keep", action="store_true")
    ap.add_argument("--wait", type=int, default=45,
                    help="seconds the reader waits for the edit to arrive")
    ap.add_argument("--shell-bin", default=os.path.join(REPO, "target/debug/shell"))
    ap.add_argument("--probe-bin",
                    default=os.path.join(REPO, "target/debug/qos-collab-probe"))
    ap.add_argument("--launcher-bin",
                    default=os.path.join(REPO, "target/debug/qos-launcher"))
    args = ap.parse_args()

    for path, how in ((args.shell_bin, "cargo build -p shell"),
                      (args.probe_bin, "cargo build -p collab-probe"),
                      (args.launcher_bin, "cargo build -p launcher")):
        if not os.path.exists(path):
            print(f"missing {path}; run: {how}", file=sys.stderr)
            return 2
    if not shutil.which("bwrap"):
        print("bwrap is not on PATH; the sandbox cannot be built", file=sys.stderr)
        return 2

    out = args.out or os.path.join("/tmp", f"qos-sandbox-collab-{os.getpid()}")
    os.makedirs(out, exist_ok=True)
    print(f"artefacts: {out}")

    pid = os.getpid()
    host = Session("host", "alice", os.path.join(out, "host"),
                   os.path.join(RUNTIME_BASE, f"qs-h{pid}"),
                   os.path.join(RUNTIME_BASE, f"qs-h{pid}.sock"), args.shell_bin)
    peer = Session("peer", "bob", os.path.join(out, "peer"),
                   os.path.join(RUNTIME_BASE, f"qs-p{pid}"),
                   os.path.join(RUNTIME_BASE, f"qs-p{pid}.sock"), args.shell_bin)
    sessions = [host, peer]
    marker = f"crossed-the-sandbox-{pid}"
    launched: list[subprocess.Popen] = []

    try:
        step("1", "start the sessions")
        for s in sessions:
            s.start()
        for s in sessions:
            s.wait_ready()
            c = s.chrome()["collab"]
            claim(f"{s.name}'s broker is available", c["available"], True)
            claim(f"{s.name}'s broker is the one the launcher dials",
                  c["socket"], s.collab_sock)
            claim(f"{s.name}'s peer-sync actions are ready",
                  s.wait_for_action("peer.invite"), True)

        step("2", "join the peer to the host's workspace")
        # Certified collaboration admits an identity, not merely a network
        # endpoint. The peer's own signed invite states its endpoint id; the
        # host then creates a named invitation bound to that exact device.
        response = peer.invoke("peer.invite")
        if "err" in response:
            raise RuntimeError(f"peer identity announcement was refused: {response['err']}")
        peer_token = None
        deadline = time.time() + 30
        while time.time() < deadline:
            peer_token = peer.chrome().get("peer_sync", {}).get("invite_token")
            if peer_token:
                break
            time.sleep(0.5)
        peer_id = peer_id_of(peer_token or "")
        claim("the peer announced its certified endpoint", bool(peer_id), True)
        if not peer_id:
            status = peer.chrome().get("peer_sync", {}).get("status")
            raise RuntimeError(
                "the peer's own invite carried no endpoint id "
                f"({status or 'no status'})")
        peer.key("Escape")

        response = host.invoke(
            "peer.invite.named", {"peer": peer_id, "name": peer.user})
        if "err" in response:
            raise RuntimeError(f"named invite was refused: {response['err']}")
        token = None
        deadline = time.time() + 30
        while time.time() < deadline:
            token = host.chrome().get("peer_sync", {}).get("invite_token")
            if token:
                break
            time.sleep(0.5)
        claim("the host minted an invite", bool(token), True)
        if not token:
            status = host.chrome().get("peer_sync", {}).get("status")
            raise RuntimeError(
                f"no invite token ({status or 'no status'}); see {host.log_path}")
        response = peer.invoke("peer.connect")
        if "err" in response:
            raise RuntimeError(f"connect prompt was refused: {response['err']}")
        deadline = time.time() + 15
        while time.time() < deadline:
            if peer.chrome().get("peer_sync", {}).get("connect_input") is not None:
                break
            time.sleep(0.25)
        peer.type_text(token)
        peer.key("Enter")
        deadline = time.time() + 60
        joined = False
        while time.time() < deadline:
            if (peer.chrome().get("peer_sync", {}).get("status") or "").startswith("Joined"):
                joined = True
                break
            time.sleep(0.5)
        claim("the peer joined", joined, True)
        room = host.active_workspace()["room"]
        claim("both sessions are in one room", peer.active_workspace()["room"], room)
        note(f"room = {room}")
        for s in sessions:
            s.key("Escape")

        # The peers exchange device certificates when their link comes up.
        # The host then admits the certified person and room sync carries that
        # grant back to the peer. Joining the transport room alone is not
        # permission to author document operations.
        deadline = time.time() + 30
        while time.time() < deadline and not peer.has_local_write_membership():
            time.sleep(0.25)
        claim("the peer's signed identity received room write membership",
              peer.has_local_write_membership(), True)

        step("3", "stage the probe as a package in each session")
        staged = {}
        for s in sessions:
            staged[s.name] = stage_payload(os.path.join(s.dir, "pkg"), args.probe_bin)
            note(f"{s.name}: {staged[s.name][1]}")

        step("4", "launch a sandboxed reader in the peer session")
        reader_log = os.path.join(out, "peer-reader.log")
        payload, manifest = staged[peer.name]
        reader = peer.launch_sandboxed(args.launcher_bin, payload, manifest,
                                       room, f"read:{args.wait}", reader_log)
        launched.append(reader)
        # The reader has to be subscribed before the writer publishes:
        # the broker fans an op to whoever is listening now, and a late
        # subscriber is indistinguishable from a broken one.
        subscribed = False
        deadline = time.time() + 60
        while time.time() < deadline:
            if "probe: subscribed" in read_log(reader_log):
                subscribed = True
                break
            if reader.poll() is not None:
                break
            time.sleep(0.5)
        claim("the sandboxed reader reached its broker and subscribed",
              subscribed, True)
        if not subscribed:
            note("reader log:")
            for line in read_log(reader_log).splitlines()[-25:]:
                note("  " + line)

        step("5", "launch a sandboxed writer in the host session")
        writer_log = os.path.join(out, "host-writer.log")
        payload, manifest = staged[host.name]
        writer = host.launch_sandboxed(args.launcher_bin, payload, manifest,
                                       room, f"write:{marker}", writer_log)
        launched.append(writer)
        writer_rc = writer.wait(timeout=90)
        claim("the sandboxed writer published its edit", writer_rc, 0)
        if writer_rc != 0:
            note("writer log:")
            for line in read_log(writer_log).splitlines()[-25:]:
                note("  " + line)

        step("6", "read it back out of the other machine's sandbox")
        try:
            reader_rc = reader.wait(timeout=args.wait + 30)
        except subprocess.TimeoutExpired:
            reader_rc = None
        blob = read_log(reader_log)
        claim("the reader exited having received an edit", reader_rc, 0)
        claim("the edit authored in the host sandbox arrived in the peer sandbox",
              marker in blob, True)
        note("reader log:")
        for line in blob.splitlines()[-20:]:
            note("  " + line)

    except Exception as e:
        print(f"\nharness error: {e}", file=sys.stderr)
        FAILURES.append(f"harness error: {e}")
    finally:
        for p in launched:
            if p.poll() is None:
                try:
                    os.killpg(os.getpgid(p.pid), signal.SIGTERM)
                except Exception:
                    p.terminate()
        if args.keep:
            print(f"\n--keep: sessions left running; artefacts in {out}")
        else:
            for s in sessions:
                s.stop()
            if not FAILURES:
                shutil.rmtree(out, ignore_errors=True)
            else:
                print(f"\nfailed-run artefacts retained in {out}")

    print()
    if FAILURES:
        print(f"FAILED ({len(FAILURES)}):")
        for f in FAILURES:
            print(f"  - {f}")
        return 1
    print("all claims verified")
    return 0


if __name__ == "__main__":
    sys.exit(main())
