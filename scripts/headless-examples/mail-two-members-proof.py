#!/usr/bin/env python3
"""Two members of one workspace read the same mailbox in Mail, and either answers from it.

Ada's session hosts the workspace and Bo's joins it ([`two_people.joined`]); both open the tree's
Mail. A message comes into the workspace's mailbox the way `tilo migrate` brings one in, through
Mail's own `deliver-message` action on Ada's session. Both windows list it: the workspace keeps one
mailbox, not one per person. Each of them then opens it and presses Reply, and their composer
answers the message's sender under "Re:" its subject.

    python3 scripts/headless-examples/mail-two-members-proof.py --out DIR [--build]

Both shells are written to DIR at each step (ada-STEP.png, bo-STEP.png) with the controls each
shows (ada-STEP.json, bo-STEP.json); a step that fails leaves both shells as they stood
(host-failed.png, peer-failed.png). The verdict is DIR/summary.json.
"""

from __future__ import annotations

import argparse
import json
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
from real_apps import Failed, Session, build, until  # noqa: E402
from two_people import joined  # noqa: E402

MAIL = "so.tilo.mail"
SUBJECT = "Quarterly invoices"
SENDER = "Grace Hopper <grace@navy.example>"
REPLY_SUBJECT = f"Re: {SUBJECT}"


def targets_of(session: Session) -> list[dict]:
    """The controls `session`'s shell shows, by name and rectangle."""
    return session.targets()


def names_of(session: Session) -> list[str]:
    return [target["name"] for target in targets_of(session)]


def shown(session: Session, out: Path, step: str) -> None:
    """Write what `session` shows now as `step`: its shell, and the controls on it."""
    who = "ada" if session.tag == "host" else "bo"
    session.snapshot(out / f"{who}-{step}.png")
    (out / f"{who}-{step}.json").write_text(json.dumps(names_of(session), indent=1))


def click_on(session: Session, wanted, what: str, deadline: float = 60.0) -> None:
    """Click the middle of the first control on `session`'s shell whose name `wanted` accepts."""
    target = until(f"{what} on {session.tag}'s shell",
                   lambda: next((t for t in targets_of(session) if wanted(t["name"])), None),
                   deadline)
    x, y, width, height = target["rect"]
    session.click_at(x + width / 2, y + height / 2)


def deliver_action(session: Session) -> dict:
    """Mail's `deliver-message` action, as the session lists it for agents and scripts."""
    def found():
        for action in session.actions("deliver"):
            if str(action.get("id", "")).endswith("deliver-message"):
                return action
        return None
    return until("Mail's deliver-message action to be listed", found, 120)


def deliver(session: Session) -> dict:
    """A message for the workspace's mailbox, brought in as an import brings each one."""
    action = deliver_action(session)
    answer = session.invoke(action["provider"], action["id"], {
        "from": SENDER,
        "to": "Office <office@company.example>",
        "subject": SUBJECT,
        "body": "Please confirm the totals by Friday.",
        "date": "2026-11-12T09:30:00Z",
        "folder": "inbox",
        "message_id": "<invoices@navy.example>",
    })
    receipt = answer.get("receipt") or {}
    if receipt.get("status") not in (None, "ok", "done", "settled") and not receipt.get("update"):
        raise Failed(f"the delivery was not taken: {json.dumps(answer)[:1500]}")
    return answer


def answers(session: Session, out: Path) -> dict:
    """`session`'s person opens the message and presses Reply; what their composer then holds."""
    who = "ada" if session.tag == "host" else "bo"
    click_on(session, lambda name: SUBJECT in name and not name.startswith("Re:"),
             f"the message {SUBJECT!r}")
    until(f"the message open on {who}'s Mail",
          lambda: any(name == "Reply" or name.startswith("Reply") for name in names_of(session)),
          60)
    shown(session, out, "reading")
    click_on(session, lambda name: name == "Reply", "Reply")
    def replying():
        now = names_of(session)
        return now if any(REPLY_SUBJECT in name for name in now) else None
    names = until(f"{who}'s reply composer to answer the sender", replying, 60)
    shown(session, out, "replying")
    return {"composer": [name for name in names if REPLY_SUBJECT in name or "grace" in name.lower()]}


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__,
                                     formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--out", type=Path, required=True)
    parser.add_argument("--build", action="store_true", help="build the nested session first")
    args = parser.parse_args()
    out = args.out.resolve()
    out.mkdir(parents=True, exist_ok=True)
    if args.build:
        build(apps="mail")
    summary: dict = {"proof": "mail-two-members", "passed": False}
    try:
        with joined(out, apps="mail", names=("Ada", "Bo")) as (ada, bo, workspace, _link):
            summary["workspace"] = workspace.get("id")
            ada.launch(MAIL)
            bo.launch(MAIL)
            for session in (ada, bo):
                shown(session, out, "opened")
            summary["delivered"] = deliver(ada)
            # One mailbox for the workspace: the message reaches Bo's window too.
            for session in (ada, bo):
                until(f"{session.tag}'s Mail to list {SUBJECT!r}",
                      lambda session=session: any(SUBJECT in name for name in names_of(session)),
                      180, 0.5)
                shown(session, out, "listed")
            summary["ada"] = answers(ada, out)
            summary["bo"] = answers(bo, out)
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
