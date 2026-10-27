#!/usr/bin/env python3
"""The canonical snapshot pin files, and the protected-branch PR that can land one.

`open-snapshot-pr.py pins KIND ...` prints the canonical pin file; t's pin-kernel,
pin-kernel-buildroot and pin-legacy-guest checks write it with this and push it to
main. Without `pins`, it lands a pin committed to an automation branch named after
the file's SHA-256 through a pull request instead, checking that branch, digest and
references agree before opening (or re-finding) it.

Every pin lands on its own. Frozen RPM snapshots resolve by package-set
fingerprint and are not promoted through this script.

For a pull request, FORGEJO_TOKEN must be able to read and create pull requests on
the repository. A token that can only push the branch is not enough: Forgejo
answers its pull-request API calls with 404.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import re
import sys
import urllib.error
import urllib.request


SHA256 = re.compile(r"^[0-9a-f]{64}$")
KERNEL_REF = re.compile(r"^registry\.tilo\.so/tilo/tilo-kernel@sha256:([0-9a-f]{64})$")
REVISION = re.compile(r"^[0-9a-f]{40}$")
KERNEL_BUILDROOT_REF = re.compile(
    r"^registry\.tilo\.so/tilo/tilo-kernel-buildroot@sha256:([0-9a-f]{64})$")
LEGACY_GUEST_REF = re.compile(r"^registry\.tilo\.so/tilo/legacy-guest@sha256:([0-9a-f]{64})$")
# Forgejo reports a repository the token cannot see as 404, not 403.
ACCESS_DENIED = (401, 403, 404)
TOKEN_HINT = ("set the workflow's FORGEJO_TOKEN to secrets.RELEASE_TOKEN, a token "
              "with read and pull-request write access to this repository")


class PromotionError(RuntimeError):
    """A failure the workflow reports as one clear error line."""


def kernel_contents(x86_ref: str, arm_ref: str) -> str:
    """Return the canonical os/kernel-snapshot.env for both native kernels."""
    return "\n".join((
        "# Per-architecture Tilo kernel RPM repositories, signed and pinned by digest.",
        "# Updated atomically by the pin-kernel check after both native builds verify.",
        f"KERNEL_RPMS_X86_64_REF={x86_ref}",
        f"KERNEL_RPMS_AARCH64_REF={arm_ref}",
        "",
    ))


def kernel_buildroot_contents(x86_ref: str, arm_ref: str) -> str:
    """Return the canonical os/kernel-buildroot-snapshot.env for both buildroots."""
    return "\n".join((
        "# Per-architecture Tilo kernel buildroots, signed and pinned by digest.",
        "# Updated atomically by the pin-kernel-buildroot check after both native builds verify.",
        f"KERNEL_BUILDROOT_X86_64_REF={x86_ref}",
        f"KERNEL_BUILDROOT_AARCH64_REF={arm_ref}",
        "",
    ))


def kernel_snapshot(args, parser) -> tuple[str, dict]:
    x86 = KERNEL_REF.fullmatch(args.x86_ref)
    arm = KERNEL_REF.fullmatch(args.arm_ref)
    if x86 is None or arm is None:
        parser.error("kernel refs must be immutable tilo-kernel sha256 references")
    if x86.group(1) == arm.group(1):
        parser.error("native kernel refs must have distinct digests")
    return kernel_contents(args.x86_ref, args.arm_ref), {
        "label": "Kernel",
        "branch_prefix": "automation/kernel-snapshot-",
        "title": "build(os): refresh native kernel snapshots",
        "summary": ("Automated atomic promotion of signed, provenance-verified "
                    "native x86-64 and ARM64 kernel snapshots, each bound to its "
                    "signed native kernel buildroot."),
    }



def kernel_buildroot_snapshot(args, parser) -> tuple[str, dict]:
    x86 = KERNEL_BUILDROOT_REF.fullmatch(args.x86_ref)
    arm = KERNEL_BUILDROOT_REF.fullmatch(args.arm_ref)
    if x86 is None or arm is None:
        parser.error("kernel buildroot refs must be immutable tilo-kernel-buildroot sha256 references")
    if x86.group(1) == arm.group(1):
        parser.error("native kernel buildroot refs must have distinct digests")
    return kernel_buildroot_contents(args.x86_ref, args.arm_ref), {
        "label": "Kernel buildroot",
        "branch_prefix": "automation/kernel-buildroot-snapshot-",
        "title": "build(os): refresh native kernel buildroot snapshots",
        "summary": ("Automated atomic promotion of the signed native x86-64 and "
                    "ARM64 kernel buildroots, each audited for its architecture, "
                    "base, signature-checked kernel SRPM and the absence of "
                    "Fedora repositories.\n\n"
                    "After this merges, image-kernel-<arch> rebuilds both native "
                    "kernels on these buildroots."),
    }


def legacy_guest_contents(x86_ref: str) -> str:
    """Return the canonical os/legacy-guest-snapshot.env."""
    return "\n".join((
        "# The Linux legacy VM guest image, pinned by digest and signed with the",
        "# release key. Written by the pin-legacy-guest check; `tilo-legacy` compiles",
        "# it in and fetches nothing else.",
        f"LEGACY_GUEST_X86_64_REF={x86_ref}",
        "",
    ))


def legacy_guest_snapshot(args, parser) -> tuple[str, dict]:
    if LEGACY_GUEST_REF.fullmatch(args.x86_ref) is None:
        parser.error("legacy guest ref must be an immutable legacy-guest sha256 reference")
    return legacy_guest_contents(args.x86_ref), {
        "label": "Legacy guest",
        "branch_prefix": "automation/legacy-guest-snapshot-",
        "title": "build(os): pin the Linux legacy VM guest image",
        "summary": ("Automated promotion of the signed Linux legacy VM guest disk, "
                    "booted once in CI until its agent served vsock.\n\n"
                    "After this merges, `tilo-legacy` creates new VMs from it."),
    }


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
        try:
            pulls = request(server, repository, token, f"/pulls?state=open&limit=50&page={page}")
        except urllib.error.HTTPError as error:
            if error.code in ACCESS_DENIED:
                raise PromotionError(
                    f"token cannot read pull requests on {repository} "
                    f"(HTTP {error.code}); {TOKEN_HINT}"
                ) from error
            raise PromotionError(
                f"Forgejo failed to list pull requests on {repository} (HTTP {error.code})"
            ) from error
        if not isinstance(pulls, list):
            raise PromotionError("Forgejo returned an invalid pull-request listing")
        for pull in pulls:
            if (pull.get("head", {}).get("ref") == branch
                    and pull.get("head", {}).get("repo", {}).get("full_name") == repository
                    and pull.get("base", {}).get("ref") == "main"):
                return pull
        if len(pulls) < 50:
            return None
        page += 1


def add_kinds(parser) -> None:
    kinds = parser.add_subparsers(dest="kind", required=True)
    for name, snapshot, help in (
            ("kernel", kernel_snapshot, "both native kernel RPM repositories"),
            ("kernel-buildroot", kernel_buildroot_snapshot, "both native kernel buildroots")):
        kind = kinds.add_parser(name, help=help)
        kind.add_argument("--x86-ref", required=True)
        kind.add_argument("--arm-ref", required=True)
        kind.set_defaults(snapshot=snapshot)
    # The legacy VM guest is published for x86-64 only.
    kind = kinds.add_parser("legacy-guest", help="the x86-64 Linux legacy VM guest disk")
    kind.add_argument("--x86-ref", required=True)
    kind.set_defaults(snapshot=legacy_guest_snapshot)


def pins(argv: list[str]) -> int:
    """Print the canonical pin file for verified references."""
    parser = argparse.ArgumentParser(prog="open-snapshot-pr.py pins")
    add_kinds(parser)
    args = parser.parse_args(argv)
    contents, _ = args.snapshot(args, parser)
    sys.stdout.write(contents)
    return 0


def promote() -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--server", required=True)
    parser.add_argument("--repository", required=True)
    parser.add_argument("--branch", required=True)
    parser.add_argument("--snapshot-digest", required=True)
    parser.add_argument("--supersede", action="append", default=[], metavar="BRANCH",
                        help="close the open pin pull request from this older automation branch")
    add_kinds(parser)
    args = parser.parse_args()

    token = os.environ.get("FORGEJO_TOKEN", "")
    if not token:
        parser.error(f"FORGEJO_TOKEN is empty; {TOKEN_HINT}")
    if not SHA256.fullmatch(args.snapshot_digest):
        parser.error("--snapshot-digest must be 64 lowercase hexadecimal characters")
    contents, kind = args.snapshot(args, parser)
    expected_digest = hashlib.sha256(contents.encode()).hexdigest()
    if args.snapshot_digest != expected_digest:
        parser.error(f"--snapshot-digest does not match the canonical {args.kind} pins")
    if args.branch != kind["branch_prefix"] + expected_digest[:16]:
        parser.error(f"--branch does not match the verified {args.kind} pins")

    pull = find_open(args.server, args.repository, token, args.branch)
    if pull is None:
        try:
            pull = request(args.server, args.repository, token, "/pulls", "POST", {
                "base": "main",
                "head": args.branch,
                "title": kind["title"],
                "body": (
                    f"{kind['summary']}\n\n"
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
                hint = f"; {TOKEN_HINT}" if error.code in ACCESS_DENIED else ""
                raise PromotionError(
                    f"Forgejo refused {args.kind} snapshot pull-request creation "
                    f"(HTTP {error.code}){hint}"
                ) from error
    url = pull.get("html_url")
    number = pull.get("number")
    if not isinstance(url, str) or not isinstance(number, int):
        raise PromotionError("Forgejo returned an invalid pull-request record")
    print(f"{kind['label']} snapshot promotion PR #{number}: {url}")
    for branch in args.supersede:
        close_superseded(args.server, args.repository, token, branch, number)
    return 0


def close_superseded(server: str, repository: str, token: str, branch: str, keep: int) -> None:
    """Close an open pin pull request that the new one replaces.

    Best effort: the new pull request already exists, and a superseded one
    left open only conflicts with it once either merges.
    """
    if not branch.startswith("automation/"):
        raise PromotionError(f"refusing to supersede non-automation branch {branch}")
    try:
        pull = find_open(server, repository, token, branch)
        if pull is None or pull.get("number") == keep:
            return
        request(server, repository, token, f"/pulls/{pull['number']}", "PATCH",
                {"state": "closed"})
    except (PromotionError, urllib.error.HTTPError, KeyError) as error:
        print(f"::warning::could not close superseded pin PR for {branch}: {error}",
              file=sys.stderr)
        return
    print(f"Closed superseded snapshot PR #{pull['number']} ({branch})")


def main() -> int:
    if sys.argv[1:2] == ["pins"]:
        return pins(sys.argv[2:])
    try:
        return promote()
    except PromotionError as error:
        print(f"::error::{error}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
