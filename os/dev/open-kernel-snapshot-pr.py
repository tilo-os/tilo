#!/usr/bin/env python3
"""Create or find the protected-branch PR for verified native kernel pins."""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import re
import urllib.error
import urllib.request


SHA256 = re.compile(r"^[0-9a-f]{64}$")
KERNEL_REF = re.compile(
    r"^code\.podesta\.ai/qos/tilo-kernel@sha256:([0-9a-f]{64})$"
)


def snapshot_contents(x86_ref: str, arm_ref: str) -> str:
    """Return the one canonical file whose digest names the automation branch."""
    return "\n".join((
        "# Per-architecture Tilo kernel RPM repositories, signed and pinned by digest.",
        "# Updated atomically by build-kernel.yml after both native builds verify.",
        f"KERNEL_RPMS_X86_64_REF={x86_ref}",
        f"KERNEL_RPMS_AARCH64_REF={arm_ref}",
        "",
    ))


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
    parser.add_argument("--snapshot-digest", required=True)
    parser.add_argument("--x86-ref", required=True)
    parser.add_argument("--arm-ref", required=True)
    args = parser.parse_args()
    token = os.environ.get("FORGEJO_TOKEN", "")
    if not token:
        parser.error("FORGEJO_TOKEN is required")
    if not SHA256.fullmatch(args.snapshot_digest):
        parser.error("--snapshot-digest must be 64 lowercase hexadecimal characters")
    x86 = KERNEL_REF.fullmatch(args.x86_ref)
    arm = KERNEL_REF.fullmatch(args.arm_ref)
    if x86 is None or arm is None:
        parser.error("kernel refs must be immutable tilo-kernel sha256 references")
    if x86.group(1) == arm.group(1):
        parser.error("native kernel refs must have distinct digests")
    expected_digest = hashlib.sha256(
        snapshot_contents(args.x86_ref, args.arm_ref).encode()
    ).hexdigest()
    if args.snapshot_digest != expected_digest:
        parser.error("--snapshot-digest does not match the canonical kernel pins")
    expected_branch = f"automation/kernel-snapshot-{expected_digest[:16]}"
    if args.branch != expected_branch:
        parser.error("--branch does not match the verified kernel pins")

    pull = find_open(args.server, args.repository, token, args.branch)
    if pull is None:
        try:
            pull = request(args.server, args.repository, token, "/pulls", "POST", {
                "base": "main",
                "head": args.branch,
                "title": "build(os): refresh native kernel snapshots",
                "body": (
                    "Automated atomic promotion of signed, provenance-verified "
                    "native x86-64 and ARM64 kernel snapshots.\n\n"
                    f"Snapshot identity: `sha256:{expected_digest}`.\n\n"
                    "This pull request preserves protected-main required checks "
                    "and merge policy."
                ),
            })
        except urllib.error.HTTPError as error:
            # A retry can race another run after the initial lookup. Re-read the
            # open set once; fail closed for every other API error.
            pull = find_open(args.server, args.repository, token, args.branch)
            if pull is None:
                raise RuntimeError(
                    f"Forgejo refused kernel snapshot pull-request creation ({error.code})"
                ) from error
    url = pull.get("html_url")
    number = pull.get("number")
    if not isinstance(url, str) or not isinstance(number, int):
        raise RuntimeError("Forgejo returned an invalid pull-request record")
    print(f"Kernel snapshot promotion PR #{number}: {url}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
