#!/usr/bin/env python3
"""Two people in one workspace see the same accounts: the workspace keeps them, not each person.

Ada's session hosts the workspace and Bo's joins it ([`two_people.joined`]); Ada adds a mail
account in Settings, and Bo's Settings lists it. Both shells are written to OUT.

    python3 scripts/headless-examples/workspace-accounts-proof.py OUT [--build]
"""

from __future__ import annotations

import json
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))

from real_apps import Failed, until  # noqa: E402
from two_people import joined  # noqa: E402

ACCOUNT = "shared-mail-account"
NAME = "Team mail — Fastmail"


def accounts_shown(session) -> list[str]:
    return [t["name"] for t in session.targets() if NAME in (t.get("name") or "")]


def settings_action(session, out: Path, resource: str, action: str) -> dict:
    """Settings' `action` of `resource`, as the session lists it for agents and scripts."""
    seen: list = []

    def found():
        seen[:] = session.actions()
        for listed in seen:
            ident = str(listed.get("id", ""))
            # A bound action names its resource hex-encoded, after its window's ids.
            named = (ident.startswith(f"{resource}\u001f")
                     or f":{resource.encode().hex()}\u001f" in ident)
            if named and ident.endswith(f"\u001f{action}"):
                return listed
        return None
    try:
        return until(f"Settings' {resource} {action} to be listed on {session.tag}", found, 120)
    except Failed:
        (out / f"actions-{session.tag}.json").write_text(json.dumps(
            [a.get("id") for a in seen if "settings" in str(a.get("id")) or
             "account" in str(a.get("id"))], indent=1))
        raise


def act(session, out: Path, resource: str, action: str, args: dict) -> dict:
    listed = settings_action(session, out, resource, action)
    return session.invoke(listed["provider"], listed["id"], args)


def main() -> int:
    out = Path(sys.argv[1]).resolve()
    out.mkdir(parents=True, exist_ok=True)
    with joined(out, apps="settings", build_first="--build" in sys.argv,
                names=("Ada", "Bo")) as (host, peer, workspace, _link):
        print("workspace", workspace, file=sys.stderr)
        for session in (host, peer):
            session.launch("settings")
            session.wait_target("Accounts")
            session.click_named("Accounts")
        saved = act(host, out, "tilo.accounts", "save-account", {
            "id": ACCOUNT, "type_id": "email", "display_name": NAME,
            "config": {
                "display_name": NAME, "email": "team@example.com", "username": "team@example.com",
                "imap_host": "imap.example.com", "imap_port": 993, "imap_tls": True,
                "smtp_host": "smtp.example.com", "smtp_port": 465, "smtp_tls": True,
            },
            "enabled": True, "tint": 3,
        })
        (out / "save-account.json").write_text(json.dumps(saved, indent=2))
        until("Ada's Settings to list the account", lambda: accounts_shown(host) or None, 60, 0.5)
        host.snapshot(out / "ada-accounts.png")
        try:
            until("Bo's Settings to list Ada's account", lambda: accounts_shown(peer) or None,
                  120, 0.5)
        finally:
            peer.snapshot(out / "bo-accounts.png")
        (out / "result.json").write_text(json.dumps({
            "workspace": workspace,
            "ada": accounts_shown(host),
            "bo": accounts_shown(peer),
        }, indent=2))
    print("ok", file=sys.stderr)
    return 0


if __name__ == "__main__":
    try:
        sys.exit(main())
    except Failed as error:
        print(f"FAILED: {error}", file=sys.stderr)
        sys.exit(1)
