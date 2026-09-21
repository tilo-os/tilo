#!/usr/bin/env python3
"""Create or find the protected-branch PR for a verified RPM snapshot pin."""

from __future__ import annotations

import argparse
import json
import os
import re
import urllib.error
import urllib.request


SHA256 = re.compile(r"^[0-9a-f]{64}$")


def request(server: str, repository: str, token: str, path: str,
            method: str = "GET", payload: dict | None = None):
    url = f"{server.rstrip('/')}/api/v1/repos/{repository}{path}"
    data = None if payload is None else json.dumps(payload).encode()
    headers = {"Authorization": f"token {token}", "Accept": "application/json"}
    if data is not None:
        headers["Content-Type"] = "application/json"
    call = urllib.request.Request(url, data=data, headers=headers, method=method)
    with urllib.request.urlopen(call, timeout=30) as response:
        return json.load(response)


def find_open(server: str, repository: str, token: str, branch: str):
    page = 1
    while True:
        pulls = request(server, repository, token, f"/pulls?state=open&limit=50&page={page}")
        if not isinstance(pulls, list):
            raise RuntimeError("Forgejo returned an invalid pull-request listing")
        for pull in pulls:
            if (pull.get("head", {}).get("ref") == branch
                    and pull.get("head", {}).get("repo", {}).get("full_name") == repository
                    and pull.get("base", {}).get("ref") == "main"):
                return pull
        if len(pulls) < 50:
            return None
        page += 1


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--server", required=True)
    parser.add_argument("--repository", required=True)
    parser.add_argument("--branch", required=True)
    parser.add_argument("--digest", required=True)
    args = parser.parse_args()
    token = os.environ.get("FORGEJO_TOKEN", "")
    if not token:
        parser.error("FORGEJO_TOKEN is required")
    if not SHA256.fullmatch(args.digest):
        parser.error("--digest must be 64 lowercase hexadecimal characters")
    expected_branch = f"automation/rpm-snapshot-{args.digest[:16]}"
    if args.branch != expected_branch:
        parser.error("--branch does not match the verified digest")

    pull = find_open(args.server, args.repository, token, args.branch)
    if pull is None:
        try:
            pull = request(args.server, args.repository, token, "/pulls", "POST", {
                "base": "main",
                "head": args.branch,
                "title": "build(os): refresh native RPM snapshot index",
                "body": (
                    "Automated promotion of a checksum-pinned, signed, and "
                    f"provenance-verified native RPM index (`sha256:{args.digest}`).\n\n"
                    "This pull request is used because `main` is protected; its "
                    "normal required checks and merge policy remain authoritative."
                ),
            })
        except urllib.error.HTTPError as error:
            # A retry can race another run after the initial lookup. Re-read the
            # open set once; fail closed for every other API error.
            pull = find_open(args.server, args.repository, token, args.branch)
            if pull is None:
                raise RuntimeError(
                    f"Forgejo refused snapshot pull-request creation ({error.code})"
                ) from error
    url = pull.get("html_url")
    number = pull.get("number")
    if not isinstance(url, str) or not isinstance(number, int):
        raise RuntimeError("Forgejo returned an invalid pull-request record")
    print(f"Snapshot promotion PR #{number}: {url}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
