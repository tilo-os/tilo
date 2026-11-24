#!/usr/bin/env python3
"""Two people in one workspace read each other by name in a tilolang app.

Ada's session hosts the workspace and Bo's joins it ([`two_people.joined`]); both open the
tilolang Chat this tree builds, installed as a signed package. Ada makes a channel and writes in
it. Bo's window, read as a screen reader reads it (AT-SPI, on his session's own bus), says
"Ada is typing…" while she types, names her as the author of what she sent ("Ada", never
"Unknown member"), and says no one is typing once she sent. Ada's own window names her
as the author too, never as a guest.

    python3 scripts/headless-examples/hosted-names-proof.py --out DIR [--build]

Each step's AT-SPI names and both shells are written to DIR on the way; a step that fails
leaves both shells as they stood (host-failed.png, peer-failed.png).
"""

from __future__ import annotations

import argparse
import json
import re
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))

from real_apps import BUILD, REPO, Failed, Session, build, run, until  # noqa: E402
from two_people import joined  # noqa: E402

sys.path.insert(0, str(REPO / "tools"))
from atspi_external_client import (  # noqa: E402
    APP_TREE_NODE_LIMIT, ROOT_PATH, AtspiClient, CheckError, bus_pids, command,
)

CHAT = "so.tilo.chat"
CHANNEL = "new-channel"
MESSAGE = "hello Bo"
TYPING = "Ada is typing…"
UNNAMED = "Unknown member"
GUEST = "Guest"


def chat_tapp(out: Path):
    """What both sessions install: Chat built against the runtime the host's state holds."""

    def built(state: Path) -> list[Path]:
        runtime = (state / "runtimes/.version").read_text().strip()
        tilolang = next(p for p in (state / "bin/tilolang", BUILD / "bin/tilolang") if p.exists())
        run([tilolang, "build", "--locked", REPO / "apps/tl/chat", "--search", REPO / "apps/tl",
             "--out", out, "--tilo-runtime", f"tilo-runtime/{runtime}"])
        return [out / "chat.tapp"]

    return built


def session_bus(session: Session) -> str:
    """The private D-Bus session bus `session` runs on (dist/dev/session-bus.sh)."""
    buses = sorted(Path(session.runtime_directory()).glob("session-bus.*/bus"))
    if len(buses) != 1:
        raise Failed(f"{session.tag} runs on {len(buses)} session buses, not one")
    return f"unix:path={buses[0]}"


def accessibility_bus(session: Session) -> str:
    """Turn assistive technology on for `session`, as starting a screen reader does, and answer
    the AT-SPI bus its windows export their trees on."""
    bus = session_bus(session)
    command("gdbus", "call", "--address", bus, "--dest", "org.a11y.Bus",
            "--object-path", "/org/a11y/bus", "--method", "org.freedesktop.DBus.Properties.Set",
            "org.a11y.Status", "IsEnabled", "<true>")
    reply = command("gdbus", "call", "--address", bus, "--dest", "org.a11y.Bus",
                    "--object-path", "/org/a11y/bus", "--method", "org.a11y.Bus.GetAddress")
    match = re.search(r"'([^']+)'", reply)
    if not match:
        raise Failed(f"{session.tag}'s session has no AT-SPI bus: {reply.strip()}")
    return match.group(1)


def chat_says(address: str, seen: dict) -> list[str]:
    """Every name the tilolang windows on the AT-SPI bus `address` export, as a screen reader
    reads them: one list for all of them, each walked whole. `seen` gets each window's names."""
    said: list[str] = []
    seen.clear()
    for name, pid in bus_pids(address).items():
        client = AtspiClient(address, name)
        try:
            # Only a tilolang app's window is walked: the shell's own tree is the session's. An
            # application node is named after the executable, and a tilolang app's window is its
            # process's (`tilolang-process`), never the runtime's.
            if "tilolang" not in client.property(ROOT_PATH, "Name"):
                continue
            tree = client.tree(APP_TREE_NODE_LIMIT)
        except CheckError:
            # Not an exporting application, or a subtree replaced while it was walked: the next
            # read walks it again.
            continue
        names = []
        for value in tree.values():
            match = re.search(r"'((?:[^'\\]|\\.)*)'", value)
            if match and match.group(1).strip():
                names.append(match.group(1))
        seen[f"{name} {pid}"] = names
        said += names
    return said


def targets_of(session: Session) -> list[str]:
    """The names of the controls `session`'s shell shows."""
    return [target["name"] for target in session.targets()]


def shown_on(session: Session) -> list:
    """The controls `session`'s shell shows where it draws them, less the shell's own layers:
    what a failed step says the person was looking at."""
    return [(t["name"], t["rect"]) for t in session.targets()
            if not t["name"].startswith(("BackgroundLayer", "DesktopChrome"))]


def click_on(session: Session, name: str) -> None:
    """Click where `session`'s shell draws the control `name`, as a person points at it: the one
    window that shows it, whichever other windows are on the shell."""
    try:
        target = until(f"{name!r} on {session.tag}'s shell", lambda: next(
            (t for t in session.targets() if t["name"] == name), None), 60)
    except Failed as error:
        raise Failed(f"{error}; it shows {shown_on(session)}") from None
    x, y, width, height = target["rect"]
    session.click_at(x + width / 2, y + height / 2)


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__,
                                     formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--out", type=Path, required=True)
    parser.add_argument("--build", action="store_true", help="build the nested session first")
    args = parser.parse_args()
    out = args.out.resolve()
    if args.build:
        build(apps="")
    with joined(out, apps="", tapps=chat_tapp(out / "tapps"), names=("Ada", "Bo")) as (
            ada, bo, _workspace, _link):
        reads, her_reads = accessibility_bus(bo), accessibility_bus(ada)
        last: dict[str, list[str]] = {}

        def bo_reads(step: str, wanted, deadline: float = 60.0) -> list[str]:
            """What Bo's window says once `wanted` holds of it, written down as `step`."""
            def read():
                said = chat_says(reads, last)
                return said if wanted(said) else None
            try:
                return until(f"Bo's window to read {step}", read, deadline, 0.1)
            finally:
                (out / f"bo-{step}.json").write_text(json.dumps(last, indent=1))
                bo.snapshot(out / f"bo-{step}.png")
                ada.snapshot(out / f"ada-{step}.png")

        ada.launch(CHAT)
        bo.launch(CHAT)
        bo_reads("opened", lambda said: "Ada" in said)
        # Ada makes a channel and stands in it; Bo opens it once it reached him.
        click_on(ada, "New channel")
        until("Ada's channel open in her window",
              lambda: f"Message #{CHANNEL}" in targets_of(ada), 60)
        until("Ada's channel listed in Bo's window", lambda: CHANNEL in targets_of(bo), 120)
        click_on(bo, CHANNEL)
        try:
            until("the channel open in Bo's window",
                  lambda: f"Message #{CHANNEL}" in targets_of(bo), 60)
        except Failed as error:
            raise Failed(f"{error}; Bo's windows are {bo.windows()} and his shell shows "
                         f"{shown_on(bo)}") from None
        # Ada types, and Bo reads that she is typing, by her name.
        click_on(ada, f"Message #{CHANNEL}")
        ada.type(MESSAGE)
        typing = bo_reads("typing", lambda said: TYPING in said)
        # She sends: Bo reads what she said under her name (one "Ada" more than the sidebar's
        # conversation with her), and no one typing any more.
        click_on(ada, "Send")
        said = bo_reads("sent", lambda said: MESSAGE in said and TYPING not in said)
        if UNNAMED in said or said.count("Ada") <= typing.count("Ada"):
            raise Failed(f"Bo's window does not name Ada as the author: {said}")
        # Her own window names her too, by the name she gave, never as a guest.
        hers = until("Ada's window to name her as the author",
                     lambda: (mine if MESSAGE in (mine := chat_says(her_reads, {}))
                              and "Ada" in mine else None), 60, 0.1)
        if GUEST in hers or UNNAMED in hers:
            raise Failed(f"Ada's window does not name her: {hers}")
        print(json.dumps({"bo_reads": said}, indent=1))
    return 0


if __name__ == "__main__":
    try:
        sys.exit(main())
    except Failed as error:
        print(f"FAILED: {error}", file=sys.stderr)
        sys.exit(1)
