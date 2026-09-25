#!/usr/bin/env python3
"""Create or find the protected-branch PR that lands a verified snapshot pin.

Snapshot workflows cannot push to protected main. They commit the canonical pin
files to an automation branch named after the files' SHA-256 and call this
script, which checks that branch, digest and references agree before opening
(or re-finding) the pull request.

Every pin lands on its own: the kernel builds on its own signed buildroot, so
the frozen RPM index and the kernels never have to move together.

FORGEJO_TOKEN must be able to read and create pull requests on the repository;
the workflows pass secrets.RELEASE_TOKEN. The token that pushes the branch
(REGISTRY_TOKEN or the job token) is not enough: Forgejo answers its
pull-request API calls with 404.
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
KERNEL_REF = re.compile(r"^code\.podesta\.ai/qos/tilo-kernel@sha256:([0-9a-f]{64})$")
RPMS_REF = re.compile(r"^code\.podesta\.ai/qos/rpms@sha256:[0-9a-f]{64}$")
REVISION = re.compile(r"^[0-9a-f]{40}$")
WPE_REF = re.compile(r"^code\.podesta\.ai/qos/tilo-wpe-buildroot@sha256:([0-9a-f]{64})$")
KERNEL_BUILDROOT_REF = re.compile(
    r"^code\.podesta\.ai/qos/tilo-kernel-buildroot@sha256:([0-9a-f]{64})$")
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
        "# Updated atomically by build-kernel.yml after both native builds verify.",
        f"KERNEL_RPMS_X86_64_REF={x86_ref}",
        f"KERNEL_RPMS_AARCH64_REF={arm_ref}",
        "",
    ))


def rpms_contents(rpms_ref: str, source_revision: str) -> str:
    """Return the canonical os/rpms-snapshot.env for the frozen RPM index."""
    return "\n".join((
        "# Digest-pinned frozen RPM repository used by os/Containerfile.",
        "# Updated by the snapshot-rpms workflow when the package set changes.",
        f"RPMS_REF={rpms_ref}",
        f"RPMS_SOURCE_REVISION={source_revision}",
        "",
    ))


def wpe_contents(x86_ref: str, arm_ref: str) -> str:
    """Return the canonical os/wpe-buildroot-snapshot.env for both buildroots."""
    return "\n".join((
        "# Per-architecture WPE buildroots, signed and pinned by digest.",
        "# Updated atomically by snapshot-wpe-buildroot.yml after both native builds verify.",
        f"WPE_BUILDROOT_X86_64_REF={x86_ref}",
        f"WPE_BUILDROOT_AARCH64_REF={arm_ref}",
        "",
    ))


def kernel_buildroot_contents(x86_ref: str, arm_ref: str) -> str:
    """Return the canonical os/kernel-buildroot-snapshot.env for both buildroots."""
    return "\n".join((
        "# Per-architecture Tilo kernel buildroots, signed and pinned by digest.",
        "# Updated atomically by snapshot-kernel-buildroot.yml after both native builds verify.",
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


def rpms_snapshot(args, parser) -> tuple[str, dict]:
    if RPMS_REF.fullmatch(args.rpms_ref) is None:
        parser.error("--rpms-ref must be an immutable qos/rpms sha256 reference")
    if REVISION.fullmatch(args.source_revision or "") is None:
        parser.error("--source-revision must be the full commit the index was built from")
    return rpms_contents(args.rpms_ref, args.source_revision), {
        "label": "RPM",
        "branch_prefix": "automation/rpm-snapshot-",
        "title": "build(os): refresh native RPM snapshot index",
        "summary": ("Automated promotion of the signed, provenance-verified "
                    "two-architecture frozen RPM index. The kernel builds on "
                    "its own signed buildroot, so this pin does not invalidate "
                    "the pinned kernels."),
    }


def wpe_snapshot(args, parser) -> tuple[str, dict]:
    x86 = WPE_REF.fullmatch(args.x86_ref)
    arm = WPE_REF.fullmatch(args.arm_ref)
    if x86 is None or arm is None:
        parser.error("WPE refs must be immutable tilo-wpe-buildroot sha256 references")
    if x86.group(1) == arm.group(1):
        parser.error("native WPE buildroot refs must have distinct digests")
    return wpe_contents(args.x86_ref, args.arm_ref), {
        "label": "WPE buildroot",
        "branch_prefix": "automation/wpe-buildroot-snapshot-",
        "title": "build(os): refresh native WPE buildroot snapshots",
        "summary": ("Automated atomic promotion of the signed native x86-64 and "
                    "ARM64 WPE buildroots, each audited for its architecture "
                    "marker and the absence of Fedora repositories."),
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
                    "After this merges, build-kernel.yml rebuilds both native "
                    "kernels on these buildroots."),
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


def promote() -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--server", required=True)
    parser.add_argument("--repository", required=True)
    parser.add_argument("--branch", required=True)
    parser.add_argument("--snapshot-digest", required=True)
    parser.add_argument("--supersede", action="append", default=[], metavar="BRANCH",
                        help="close the open pin pull request from this older automation branch")
    kinds = parser.add_subparsers(dest="kind", required=True)
    kernel = kinds.add_parser("kernel", help="both native kernel RPM repositories")
    kernel.add_argument("--x86-ref", required=True)
    kernel.add_argument("--arm-ref", required=True)
    kernel.set_defaults(snapshot=kernel_snapshot)
    rpms = kinds.add_parser("rpms", help="the two-architecture frozen RPM index")
    rpms.add_argument("--rpms-ref", required=True)
    rpms.add_argument("--source-revision", required=True,
                      help="the commit the index was built from")
    rpms.set_defaults(snapshot=rpms_snapshot)
    wpe = kinds.add_parser("wpe", help="both native WPE buildroots")
    wpe.add_argument("--x86-ref", required=True)
    wpe.add_argument("--arm-ref", required=True)
    wpe.set_defaults(snapshot=wpe_snapshot)
    buildroot = kinds.add_parser("kernel-buildroot", help="both native kernel buildroots")
    buildroot.add_argument("--x86-ref", required=True)
    buildroot.add_argument("--arm-ref", required=True)
    buildroot.set_defaults(snapshot=kernel_buildroot_snapshot)
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
    try:
        return promote()
    except PromotionError as error:
        print(f"::error::{error}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
