#!/usr/bin/env python3
"""Print the base a lander re-check may narrow to, or nothing.

When the lander reruns a pull request's green check because the base branch
moved, it first posts a commit status on the head: context `lander / re-check`,
description `<old base sha> for attempt <n>`, where the old base is the base
tip the green run tested. On attempt <n> of this run, the plan may then skip
what the base's own changes since then did not reach (affected_packages.py
--recheck-from). A status from anyone but the lander, for another attempt, or
a malformed one leaves the plan complete.
"""
import json
import os
import re
import sys
from urllib.request import Request, urlopen

CONTEXT = "lander / re-check"
DESCRIPTION = re.compile(r"^([0-9a-f]{40}) for attempt ([1-9][0-9]*)$")


def recheck_base(statuses, lander, attempt):
    records = [s for s in statuses if s.get("context") == CONTEXT
               and (s.get("creator") or {}).get("login") == lander]
    if not records:
        return None
    latest = max(records, key=lambda s: s.get("id", 0))
    match = DESCRIPTION.match(latest.get("description") or "")
    if not match or int(match.group(2)) != attempt:
        return None
    return match.group(1)


def main():
    attempt = int(os.environ.get("RUN_ATTEMPT") or 1)
    lander = os.environ.get("LANDER_LOGIN", "")
    if attempt < 2 or not lander:
        return 0
    url = (f"{os.environ['GITHUB_SERVER_URL']}/api/v1/repos/{os.environ['GITHUB_REPOSITORY']}"
           f"/commits/{os.environ['HEAD_SHA']}/statuses?limit=50")
    request = Request(url, headers={"Authorization": f"token {os.environ['GITHUB_TOKEN']}",
                                    "Accept": "application/json"})
    with urlopen(request, timeout=30) as response:
        base = recheck_base(json.load(response), lander, attempt)
    if base:
        print(base)
    return 0


if __name__ == "__main__":
    sys.exit(main())
