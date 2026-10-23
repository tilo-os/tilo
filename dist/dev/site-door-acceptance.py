#!/usr/bin/env python3
"""A Tilo computer joins a site's home workspace through its door: the
acceptance of "a workspace is its room".

Against a site that runs the way tilo.coffee runs (its entry.json naming the
home workspace and the door that admits people to it), with two nested,
headless sessions of fresh throwaway identities whose community site it is
(dist/dev/nested-session.sh --community), it checks:

  room       session A's workspace of the site is the room the site's door
             names, and the only workspace of that room (with --room, the
             room the host's operator says it is)
  web-room   the site's web view (headless Chrome, a browser person of its
             own) opens the same room
  post       a message A posts in the workspace's Chat is read on B
  web-post   the web view shows that message
  rejoin     A, started again and joined again, holds the same workspace
             (same id, same room), never a second one

Without --site it starts that site here: the room service in persistent
mode with its relay and the site's web entry serving the desktop bundle
(crates/hosting/tilo-room-service/dev/run.py, the production pieces with
plain local origins), from the nested session's build, so every run admits,
enrolls, posts and reads through the real door path with nothing stubbed.
--bundle names the desktop bundle the site serves to the web view; without
one the site serves only entry.json and the web checks fail.

With --site it joins that site instead. Never point it at the production
Lounge from CI: everything it does is real, posts included. It refuses
tilo.coffee unless --production is given.

A check that cannot observe its subject fails; nothing is skipped. Exits
non-zero when any check failed, 2 when it could not run at all.

usage: dist/dev/site-door-acceptance.py [--site URL [--room UUID]] [--out DIR]
           [--no-build] [--bundle DIR]
"""
from __future__ import annotations

import argparse
import importlib.util
import json
import os
import pathlib
import signal
import subprocess
import sys
import time
import urllib.parse

REPO = pathlib.Path(__file__).resolve().parents[2]
SESSION = REPO / "dist/dev/nested-session.sh"
RUN_SITE = REPO / "crates/hosting/tilo-room-service/dev/run.py"
WEB_VIEW = REPO / "crates/hosting/tilo-room-service/dev/site-web-view.mjs"
# How long a fresh session may take to start and join the site.
START = 900.0
# How long the local site may take to answer.
SITE_START = 300.0
PRODUCTION = {"tilo.coffee", "www.tilo.coffee"}


def pair_check():
    """dist/dev/nested-pair-check.py, whose control-socket helpers and Chat
    checks this reuses."""
    spec = importlib.util.spec_from_file_location("nested_pair_check", REPO / "dist/dev/nested-pair-check.py")
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


PAIR = pair_check()
Failed, Session, wait_for, ok = PAIR.Failed, PAIR.Session, PAIR.wait_for, PAIR.ok


def control_of(state: pathlib.Path) -> str:
    """The control socket nested-session.sh gives a state directory."""
    import hashlib
    runtime = os.environ["XDG_RUNTIME_DIR"]
    digest = hashlib.sha256(str(state).encode()).hexdigest()[:12]
    return f"{runtime}/tilo-nested-{digest}/control.sock"


def site_room(state: pathlib.Path, site: str) -> str | None:
    """The room the site's door named to the session in `state`, as it keeps
    it (peer-sync/hosted-rooms.json)."""
    path = state / "state/tilo/peer-sync/hosted-rooms.json"
    try:
        rooms = json.loads(path.read_text()).get("rooms", {})
    except (OSError, ValueError):
        return None
    for room, hosted in rooms.items():
        if hosted.get("site") == site:
            return room
    return None


class Running:
    """A nested headless session, started and stopped by this script."""

    def __init__(self, name: str, state: pathlib.Path, site: str, out: pathlib.Path):
        self.name, self.state, self.site, self.out = name, state, site, out
        self.process: subprocess.Popen | None = None

    def start(self, fresh: bool, build: bool):
        command = [str(SESSION), "--state", str(self.state), "--headless", "--community", self.site,
                   "--apps", "chat"]
        if fresh:
            command.append("--reset-identity")
        if not build:
            command.append("--no-build")
        log = open(self.out / f"{self.name}.log", "a")
        self.process = subprocess.Popen(command, stdout=log, stderr=subprocess.STDOUT,
                                        start_new_session=True, env=dict(os.environ, XKB_DEFAULT_LAYOUT="us"))

    def stop(self):
        if self.process is None or self.process.poll() is not None:
            return
        os.killpg(self.process.pid, signal.SIGTERM)
        try:
            self.process.wait(timeout=90)
        except subprocess.TimeoutExpired:
            os.killpg(self.process.pid, signal.SIGKILL)

    def joined(self) -> Session:
        """Wait until the session holds the site's workspace: its room, as the
        door named it, installed (no longer joining). The session's view."""
        control = control_of(self.state)

        def held():
            if self.process is not None and self.process.poll() is not None:
                raise Failed(f"session {self.name} exited with {self.process.returncode} "
                             f"(see {self.out / (self.name + '.log')})")
            room = site_room(self.state, self.site)
            if room is None or not os.path.exists(control):
                return None
            session = Session(self.name, control, room)
            try:
                workspace = session.lounge()
            except (Failed, OSError, ValueError):
                return None
            # The workspace is the room from the start and says Joining…
            # until the room installs; posting needs it installed.
            return session if workspace.get("strips") is not None and installed(session) else None
        return wait_for(f"session {self.name} to hold the site's workspace", held, START)


def installed(session: Session) -> bool:
    """Whether the session's workspace of its room has its replica: Chat's
    actions in it are offered only then."""
    return any(row.get("application") == PAIR.CHAT and row.get("room") == session.room
               for row in session.catalogue())


def workspaces_of(session: Session) -> list[dict]:
    tree = session.state()["workspace_tree"]
    return [w for w in tree.get("workspaces", []) if str(w.get("room")) == session.room]


def free_port_base(span: int = 80) -> int:
    """A base port with `span` free ports above it on this machine: a t node
    runs other checks beside this one."""
    import random
    import socket
    for _ in range(200):
        base = random.randrange(20000, 60000 - span)
        try:
            for port in range(base, base + span):
                with socket.socket() as probe:
                    probe.bind(("127.0.0.1", port))
        except OSError:
            continue
        return base
    raise Failed("no free ports for the local site")


class LocalSite:
    """The site this script starts when given none: run.py's room service,
    relay and web entry, from the nested session's build."""

    def __init__(self, out: pathlib.Path, bundle: pathlib.Path | None):
        self.dir, self.bundle = out / "site", bundle
        self.process: subprocess.Popen | None = None
        self.site = self.room = ""

    def start(self):
        build = pathlib.Path(os.environ.get("TILO_NESTED_SESSION_BUILD", REPO / "target/nested-session"))
        self.dir.mkdir(parents=True, exist_ok=True)
        ready = self.dir / "ready.json"
        ready.unlink(missing_ok=True)
        base = free_port_base()
        command = [sys.executable, "-I", str(RUN_SITE), "--scratch", str(self.dir), "--fresh",
                   "--name", "lounge", "--shown-name", "Lounge", "--home-app", "so.tilo.chat",
                   "--relay-bin", str(build / "bin/tilo-relay"),
                   "--room-service-bin", str(build / "bin/tilo-room-service"),
                   "--web-port", str(base), "--api-port", str(base + 1), "--port-base", str(base + 10),
                   "--ready-file", str(ready)]
        command += ["--bundle", str(self.bundle)] if self.bundle else ["--no-bundle"]
        # Both sessions and the web view join from one address: the door's
        # per-address guest limits are the operator's setting here.
        env = dict(os.environ, TILO_ROOMS_GUEST_JOINS_PER_10_MIN="1000", TILO_ROOMS_GUEST_JOINS_PER_DAY="100000")
        log = open(self.dir / "site.log", "a")
        self.process = subprocess.Popen(command, stdout=log, stderr=subprocess.STDOUT,
                                        start_new_session=True, env=env)

        def answered():
            if self.process.poll() is not None:
                raise Failed(f"the local site exited with {self.process.returncode} (see {self.dir / 'site.log'})")
            try:
                return json.loads(ready.read_text())
            except (OSError, ValueError):
                return None
        up = wait_for("the local site to answer", answered, SITE_START)
        self.site, self.room = up["site"], up["room"]
        print(f"the local site is up at {self.site} (room {self.room})")

    def stop(self):
        if self.process is None or self.process.poll() is not None:
            return
        os.killpg(self.process.pid, signal.SIGTERM)
        try:
            self.process.wait(timeout=60)
        except subprocess.TimeoutExpired:
            os.killpg(self.process.pid, signal.SIGKILL)


def web_view(site: str, args, out: pathlib.Path, text: str | None) -> dict:
    """Open the site in headless Chrome as a person of its own and report
    the room it opened and, with `text`, whether its Chat shows it."""
    command = ["node", str(WEB_VIEW), "--site", site, "--out", str(out / "web")]
    if text is not None:
        command += ["--expect", text]
    done = subprocess.run(command, capture_output=True, text=True, timeout=600)
    (out / "web.log").write_text(done.stdout + done.stderr)
    try:
        return json.loads(done.stdout.strip().splitlines()[-1])
    except (IndexError, ValueError):
        raise Failed(f"the web view did not report (exit {done.returncode}; see {out / 'web.log'})")


def tail(path: pathlib.Path, lines: int = 80, matching: tuple[str, ...] = ()):
    """The end of `path`, printed under its name, if it exists: its last
    `lines`, or its last `lines` that mention any of `matching`."""
    try:
        text = path.read_text(errors="replace").splitlines()
    except OSError:
        print(f"--- {path}: missing")
        return
    if matching:
        text = [line for line in text if any(word in line.lower() for word in matching)]
    print(f"--- {path} (last {min(lines, len(text))} of {len(text)} lines)")
    for line in text[-lines:]:
        print(f"    {line}")


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--site", default=os.environ.get("TILO_ACCEPTANCE_SITE"),
                        help="the site whose home workspace is joined (TILO_ACCEPTANCE_SITE); "
                             "without one, a local site this script starts")
    parser.add_argument("--room", default=os.environ.get("TILO_ACCEPTANCE_ROOM"),
                        help="the room the site's home workspace is, as its host's operator knows it")
    parser.add_argument("--production", action="store_true", help="allow tilo.coffee itself (never from CI)")
    parser.add_argument("--no-build", action="store_true", help="start the sessions from the last build")
    parser.add_argument("--bundle", type=pathlib.Path,
                        help="the desktop bundle the local site serves to the web view")
    parser.add_argument("--out", type=pathlib.Path, help="where logs, state dumps and snapshots go")
    args = parser.parse_args()
    if args.site:
        host = urllib.parse.urlparse(args.site).hostname or ""
        if host in PRODUCTION and not args.production:
            print(f"{host} is the production Lounge: this posts into it. Use a test site, or --production by hand.",
                  file=sys.stderr)
            return 2
    elif args.room:
        print("--room names a room of a --site; the local site says its own", file=sys.stderr)
        return 2
    # Under target/, never /tmp: two sessions keep their whole state there.
    out = (args.out or REPO / "target/site-door-acceptance" / time.strftime("%Y%m%d-%H%M%S")).resolve()
    out.mkdir(parents=True, exist_ok=True)
    local = None if args.site else LocalSite(out, args.bundle)
    if local is not None:
        try:
            local.start()
        except Failed as failure:
            print(f"FAIL site: {failure}")
            tail(local.dir / "site.log")
            local.stop()
            return 2
        args.room = local.room
    site = args.site or local.site
    site = site if site.endswith("/") else site + "/"
    a = Running("a", out / "a", site, out)
    b = Running("b", out / "b", site, out)
    failed: list[str] = []

    def check(name: str, passed: bool, detail: str):
        print(f"{'ok  ' if passed else 'FAIL'} {name}: {detail}")
        if not passed:
            failed.append(name)

    try:
        a.start(fresh=True, build=not args.no_build)
        session_a = a.joined()
        b.start(fresh=True, build=False)
        session_b = b.joined()

        held = workspaces_of(session_a)
        expected = args.room or session_a.room
        check("room", len(held) == 1 and str(held[0]["room"]) == expected == session_b.room,
              f"A holds {[(w['id'], w['room']) for w in held]}, the door named {session_a.room}, "
              f"B {session_b.room}, expected {expected}")
        before = held[0]["id"] if held else None

        web = web_view(site, args, out, None)
        check("web-room", web.get("entry") == "open" and web.get("room") == session_a.room,
              f"the web view opened {web}")

        text = f"Door acceptance {time.strftime('%Y-%m-%d %H.%M.%S')}"
        _, channel = PAIR.channel_reader(session_a)
        session_a.chat("post", {"channel": channel, "text": text})
        try:
            wait_for("A's message to be read on B", lambda: PAIR.reads_message(session_b, channel, text))
            check("post", True, f"B reads {text!r} in {channel}")
        except Failed as missing:
            check("post", False, str(missing))

        web = web_view(site, args, out, text)
        channels = web.pop("channels", None)
        check("web-post", bool(web.get("shows")), f"the web view {web}")
        if channels:
            print("    the web view's channels:")
            for line in str(channels).splitlines():
                print(f"      {line}")

        a.stop()
        a.start(fresh=False, build=False)
        session_a = a.joined()
        again = workspaces_of(session_a)
        check("rejoin", [w["id"] for w in again] == [before] and session_a.room == expected,
              f"after joining again A holds {[(w['id'], w['room']) for w in again]}")
        for session in (session_a, session_b):
            session.snapshot(out, "end")
    except (Failed, OSError) as failure:
        # A session that went away takes its control socket with it: an
        # OSError here is a session that stopped, said as a failure.
        failed.append("setup")
        print(f"FAIL setup: {failure!r}")
        for running in (a, b):
            if running.process is not None and running.process.poll() is not None:
                print(f"session {running.name} exited with {running.process.returncode}")
        # A failed check keeps no outputs: say here what the sessions said.
        for running in (a, b):
            tail(out / f"{running.name}.log")
            tail(running.state / "state/tilo/peer-sync/hosted-rooms.json")
            tail(running.state / "logs/shell.log", lines=40)
            tail(running.state / "logs/shell.log", lines=150, matching=(
                "community", "site", "door", "join", "enrol", "room", "error", "warn", "panic", "refus"))
    finally:
        a.stop()
        b.stop()
        if failed:
            tail(out / "web.log", lines=60)
        if local is not None:
            if failed:
                tail(local.dir / "site.log", lines=60)
            local.stop()
    print(("all door checks passed" if not failed else f"failed: {', '.join(failed)}") + f" ({out})")
    return 1 if failed else 0


if __name__ == "__main__":
    sys.exit(main())
