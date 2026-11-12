"""The plans file a VM host admits by, as the provider writes it.

A host that sells its VMs reads this file for every admission and every page
action (docs/hosted-browser.md): an account it does not name has no card on
file, so its first Linux command is refused and its Browser runs only the
period's free page actions. The provider adds the account once a card is on
file, and the next admission and the next page action are served without a
restart, because the file is read again each time.

The checks play the provider: they write the file the way the provider's own
billing would, so the refusal and what follows it are the product's and not
the check's. Written in place, never renamed over: the file is mounted into
the host's container, and a replacement would leave it reading the old one.

    plans.py ACCOUNT FILE     put `ACCOUNT` on file with a card, and say so

The entry written is the one an account on the stack's one plan gets: a card,
and the limits that plan carries.
"""

from __future__ import annotations

import json
import sys

# The plan the local stack's plans file sells (os/containers/tilo-sh/vm-host.sh),
# and what an account on it may run.
PLAN = {
    "plan": "local",
    "max_running": 2,
    "max_vms": 5,
    "sizes": ["small", "default"],
    "card": {"brand": "visa", "last4": "4242"},
}


def add_account(path: str, account: str) -> None:
    """`account` on file, with a card, in the plans file at `path`."""
    with open(path) as plans:
        document = json.load(plans)
    document.setdefault("accounts", {})[account] = dict(PLAN)
    with open(path, "w") as plans:
        json.dump(document, plans, indent=2)


def named(path: str, account: str) -> bool:
    """Whether the file at `path` has `account` on file (a card, to the host)."""
    try:
        with open(path) as plans:
            return account in (json.load(plans).get("accounts") or {})
    except (OSError, ValueError):
        return False


if __name__ == "__main__":
    if len(sys.argv) != 3:
        raise SystemExit("usage: plans.py ACCOUNT FILE")
    add_account(sys.argv[2], sys.argv[1])
    print(json.dumps({"account": sys.argv[1], "plans": sys.argv[2], "on_file": True}))
