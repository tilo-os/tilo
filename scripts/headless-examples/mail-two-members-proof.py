#!/usr/bin/env python3
"""Two members of one workspace read the same mailbox in the tilolang Mail, and either answers from it.

Ada's session hosts the workspace and Bo's joins it ([`two_people.joined`]). Ada opens the tree's
tilolang Mail, installed as a signed package, on the strip they share, and lets it reach the
network; the window comes to Bo's shell as well, his own device running it with the grant Ada
gave, so he is asked nothing. A message comes into the workspace's mailbox the way `tilo migrate`
brings one in, through Mail's own `deliver_message` action on Ada's session, and opens no window
of its own. Both windows list it: the workspace keeps one mailbox, not one per person. Each of them then opens it
and presses Reply, and their composer answers the message's sender under "Re:" its subject.

    python3 scripts/headless-examples/mail-two-members-proof.py --out DIR [--build]

Neither shell may show an error notice at any point: one fails the proof. Both shells are
written to DIR at each step (ada-STEP.png, bo-STEP.png) with the controls each
shows (ada-STEP.json, bo-STEP.json); a step that fails leaves both shells as they stood
(host-failed.png, peer-failed.png). The verdict is DIR/summary.json.
"""

from __future__ import annotations

import argparse
import json
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))

from real_apps import BUILD, REPO, Failed, Session, address, build, run, until  # noqa: E402
from two_people import joined  # noqa: E402

MAIL = "so.tilo.mail"
SUBJECT = "Quarterly invoices"
SENDER = "Grace Hopper <grace@navy.example>"
SENDER_NAME = "Grace Hopper"
MESSAGE_ID = "<invoices@navy.example>"


def mail_tapp(out: Path):
    """What both sessions install: the tilolang Mail built against the runtime the host holds."""

    def built(state: Path) -> list[Path]:
        runtime = (state / "runtimes/.version").read_text().strip()
        tilolang = next(p for p in (state / "bin/tilolang", BUILD / "bin/tilolang") if p.exists())
        run([tilolang, "build", REPO / "apps/tl/mail", "--search", REPO / "apps/tl",
             "--out", out, "--tilo-runtime", f"tilo-runtime/{runtime}"])
        return [out / "mail.tapp"]

    return built


def targets_of(session: Session) -> list[dict]:
    """The controls `session`'s shell shows, by name, what it does and rectangle."""
    return session.targets()


def names_of(session: Session) -> list[str]:
    return [target["name"] for target in targets_of(session)]


def shown(session: Session, out: Path, step: str) -> None:
    """Write what `session` shows now as `step`: its shell, and the controls on it."""
    who = "ada" if session.tag == "host" else "bo"
    session.snapshot(out / f"{who}-{step}.png")
    (out / f"{who}-{step}.json").write_text(json.dumps(
        [{"name": t["name"], "identity": t.get("identity")} for t in targets_of(session)], indent=1))


def mail_windows(session: Session) -> list[dict]:
    """The Mail windows `session`'s shell shows, named by the page each stands on."""
    return [w for w in session.windows() if w.get("app_id") == MAIL]


def title_of(session: Session) -> str:
    """The title of the one Mail window `session`'s shell shows."""
    windows = mail_windows(session)
    if len(windows) != 1:
        raise Failed(f"{session.tag}'s shell shows {len(windows)} Mail windows: {windows}")
    return windows[0].get("title") or ""


def own_mail_window(session: Session) -> dict | None:
    """The Mail window `session`'s own device runs and has mapped: not the tab of one that is
    only held for it, or still waits on a question."""
    return next((w for w in mail_windows(session)
                 if w.get("window") is not None and w.get("rect")), None)


def comes_to(session: Session, deadline: float = 180.0) -> dict:
    """The Mail window another member opened on the shared strip, run by `session`'s own device.
    The network grant it needs is the workspace's, so the device asks its person nothing: a
    question it put up before the grant arrived is taken down once it does."""
    try:
        return until(f"{session.tag}'s own Mail window on the shared strip",
                     lambda: own_mail_window(session), deadline, 0.25)
    except Failed as failed:
        state = session.state_dump()
        raise Failed(f"{failed}; its Mail windows: {mail_windows(session)}; its overlays: "
                     f"{state.get('chrome', {}).get('overlays')}; its notices: "
                     f"{state.get('notifications')}; its questions: "
                     f"{session.control({'op': 'questions'}).get('answer')}") from failed


def one_window_each(sessions: dict[str, Session]) -> dict[str, int]:
    """Each shell shows the one Mail window on the shared strip, and nothing a delivery opened."""
    counts = {who: len(mail_windows(session)) for who, session in sessions.items()}
    if any(count != 1 for count in counts.values()):
        raise Failed(f"one delivery left these Mail windows: {counts}; "
                     + "; ".join(f"{who}: {mail_windows(session)}"
                                 for who, session in sessions.items()))
    return counts


def open_mail(session: Session, deadline: float = 180.0) -> dict:
    """Open the tilolang Mail through the desktop's own launcher. The app asks for the network it
    reaches its accounts' servers over, which the shell puts to the workspace's people: the window
    opens once one of them allows it."""
    session.open(MAIL)

    def opened():
        window = next((w for w in session.windows() if w.get("app_id") == MAIL), None)
        if window is not None:
            return window
        if any(target["name"] == "Allow" for target in targets_of(session)):
            print(f"==> {session.tag} allows {MAIL} the network it names", file=sys.stderr)
            session.click_named("Allow")
        return None

    return until(f"the window of {MAIL}", opened, deadline, 0.25)


def click_row(session: Session, wanted, what: str, deadline: float = 180.0) -> None:
    """Press the first control on `session`'s shell whose name `wanted` accepts."""
    target = until(f"{what} on {session.tag}'s shell",
                   lambda: next((t for t in targets_of(session) if wanted(t["name"])), None),
                   deadline, 0.5)
    session.click_named(target["name"])


def deliver_action(session: Session) -> dict:
    """Mail's `deliver_message` action, as the session lists it for agents and scripts."""
    def found():
        for row in session.actions(""):
            if row.get("app_id") == MAIL and (address(row) or ("",))[-1] == "deliver_message":
                return row
        return None
    return until("Mail's deliver_message action to be listed", found, 180)


def deliver(session: Session) -> dict:
    """A message for the workspace's mailbox, brought in as an import brings each one."""
    action = deliver_action(session)
    answer = session.invoke_row(action, {
        "from": SENDER,
        "to": "Office <office@company.example>",
        "subject": SUBJECT,
        "body": "Please confirm the totals by Friday.",
        "date": "2026-11-12T09:30:00Z",
        "folder": "inbox",
        "message_id": MESSAGE_ID,
    })
    if isinstance(answer, dict) and "Failed" in answer:
        raise Failed(f"the delivery was not taken: {json.dumps(answer)[:1500]}")
    return answer


def list_it(session: Session, out: Path) -> dict:
    """`session`'s window lists the message, and says the page it stands on."""
    until(f"{session.tag}'s Mail to list the message from {SENDER_NAME}",
          lambda: any(SENDER_NAME in name for name in names_of(session)), 180, 0.5)
    shown(session, out, "listed")
    return {"title": title_of(session), "rows": [name for name in names_of(session)
                                                 if SENDER_NAME in name]}


def open_it(session: Session, out: Path) -> dict:
    """`session`'s person opens the message: the window stands on the conversation's page."""
    before = title_of(session)
    click_row(session, lambda name: SENDER_NAME in name, f"the message from {SENDER_NAME}")
    names = until(f"the conversation open on {session.tag}'s Mail",
                  lambda: (now if title_of(session) == SUBJECT else None)
                  if (now := names_of(session)) else None,
                  60, 0.25)
    shown(session, out, "reading")
    return {"title": title_of(session), "was": before,
            "controls": [name for name in names if name in ("Reply", "Reply all", "Forward")]}


def replied_through_api(session: Session) -> str:
    """What Mail's `reply` answers `session`'s person through the API, as an agent or a script
    asks it: whether it was taken, or why not."""
    try:
        action = next((row for row in session.actions("")
                       if row.get("app_id") == MAIL and (address(row) or ("",))[-1] == "reply"),
                      None)
        if action is None:
            return "nothing: the session lists no reply"
        return json.dumps(session.invoke_row(action))[:1500]
    except Failed as error:
        return f"a refusal: {error}"


def reply(session: Session, out: Path) -> dict:
    """`session`'s person presses Reply, and their composer answers the sender under "Re:"."""
    target = until("the conversation's own Reply, as its action names it",
                   lambda: next((t for t in targets_of(session)
                                 if t["name"] == "Reply"
                                 and (t.get("identity") or "").endswith("\x1freply")), None), 60)
    session.click_doing(target["identity"])
    try:
        names = until(f"{session.tag}'s reply composer to open",
                      lambda: (now if all(what in now for what in ("Send", "Subject", "To"))
                               else None) if (now := names_of(session)) else None,
                      60, 0.25)
    except Failed as error:
        raise Failed(f"{error}; {session.tag}'s Mail window is {title_of(session)!r}, its Reply "
                     f"is {target}, the same Reply through the API answers "
                     f"{replied_through_api(session)} and the window shows "
                     f"{names_of(session)}") from None
    wanted = f"Re: {SUBJECT}"
    title = title_of(session)
    if title != wanted:
        raise Failed(f"{session.tag}'s composer is titled {title!r}, not {wanted!r}")
    shown(session, out, "replying")
    return {"title": title,
            "composer": [name for name in names if name in ("Send", "Cc", "Subject", "To")]}


def no_error_notices(sessions: dict[str, Session]) -> None:
    """Fail on any error notice either shell shows: an error the person reads is a failure of
    the flow, whichever step it came up in."""
    told = [
        f"{who}: " + " — ".join(part for part in (notice.get("title"), notice.get("body")) if part)
        for who, session in sessions.items()
        for notice in session.state_dump().get("notifications", [])
        if notice.get("severity") == "error"
    ]
    if told:
        raise Failed("an error notice came up: " + "; ".join(told))


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__,
                                     formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--out", type=Path, required=True)
    parser.add_argument("--build", action="store_true", help="build the nested session first")
    args = parser.parse_args()
    out = args.out.resolve()
    out.mkdir(parents=True, exist_ok=True)
    if args.build:
        build(apps="")
    summary: dict = {"proof": "mail-two-members", "passed": False}
    try:
        with joined(out, apps="", tapps=mail_tapp(out / "tapps"), names=("Ada", "Bo")) as (
                ada, bo, workspace, _link):
            summary["workspace"] = workspace
            # Ada opens Mail on the strip they share and lets it reach the servers it names;
            # the grant is the workspace's, so Bo's device runs the window without a sheet.
            open_mail(ada)
            comes_to(bo)
            no_error_notices({"Ada": ada, "Bo": bo})
            for session in (ada, bo):
                shown(session, out, "opened")
            summary["delivered"] = deliver(ada)
            summary["windows"] = one_window_each({"Ada": ada, "Bo": bo})
            # One mailbox for the workspace: the message reaches Bo's window too.
            summary["ada"] = list_it(ada, out)
            summary["bo"] = list_it(bo, out)
            summary["same_mailbox"] = (summary["ada"]["title"] == summary["bo"]["title"]
                                       and summary["ada"]["rows"] == summary["bo"]["rows"])
            if not summary["same_mailbox"]:
                raise Failed(f"the two shells read different mailboxes: {summary['ada']} and "
                             f"{summary['bo']}")
            summary["ada_opened"] = open_it(ada, out)
            summary["bo_opened"] = open_it(bo, out)
            summary["ada_reply"] = reply(ada, out)
            summary["bo_reply"] = reply(bo, out)
            no_error_notices({"Ada": ada, "Bo": bo})
            summary["passed"] = True
    finally:
        (out / "summary.json").write_text(json.dumps(summary, indent=1, default=str))
    print(json.dumps(summary, indent=1, default=str))
    return 0


if __name__ == "__main__":
    try:
        sys.exit(main())
    except Failed as error:
        print(f"FAILED: {error}", file=sys.stderr)
        sys.exit(1)
