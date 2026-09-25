#!/usr/bin/env python3
"""Conservatively compare frozen RPM inputs without building either architecture."""
from __future__ import annotations
import argparse
import importlib.util
import json
import os
from pathlib import Path
import re
import subprocess
import tempfile

ROOT = Path(__file__).resolve().parents[2]
spec = importlib.util.spec_from_file_location("rpm_packages", ROOT / "os/dev/derive-rpm-package-set.py")
parser = importlib.util.module_from_spec(spec)
spec.loader.exec_module(parser)

# Everything that can change what snapshot-rpms.yml publishes. Its push paths
# mirror this list. The pin is included so an in-flight publication becomes
# stale when another pin lands, and so landing a pin rechecks for drift.
# build-kernel.yml reads the same list, so both workflows agree on when a
# snapshot is refreshed.
PIN = "os/rpms-snapshot.env"
INPUTS = (
    "os/Containerfile",
    "os/base-snapshot.env",
    "os/dev/rpm-snapshot-inputs.py",
    ".forgejo/actions/install-cosign",
    "os/Containerfile.rpms",
    "os/dev/verify-rpm-snapshot-signatures.sh",
    "os/external-assets/github-assets.tsv",
    "os/dev/cache-github-assets.sh",
    "os/dev/derive-rpm-package-set.py",
    "keys/cosign.pub",
    "os/dev/promote-native-index.py",
    "os/dev/open-snapshot-pr.py",
    "os/dev/install-cached-cosign.sh",
    "os/dev/configure-cosign-kms.sh",
    "os/Containerfile.dev",
    "os/Containerfile.host",
    "os/dev/composefs/Containerfile",
    "os/containers/tilo-relay/Containerfile",
    "os/kernel/Containerfile",
    ".forgejo/workflows/snapshot-rpms.yml",
    PIN,
)
# The published pin records the commit its snapshot was built from.
SOURCE_REVISION = re.compile(r"^RPMS_SOURCE_REVISION=([0-9a-f]{40})$", re.MULTILINE)


def git(repo, *args):
    return subprocess.check_output(["git", "-C", str(repo), *args], stderr=subprocess.PIPE)


def commit(repo, revision):
    """Resolve a commit, fetching it alone into a shallow checkout if absent."""
    try:
        return git(repo, "rev-parse", "--verify", revision + "^{commit}").decode().strip()
    except subprocess.CalledProcessError:
        git(repo, "fetch", "--no-tags", "--depth=1", "origin", revision)
        return git(repo, "rev-parse", "--verify", revision + "^{commit}").decode().strip()


def pinned_source(repo, head):
    """Return the commit the snapshot pinned at head was built from."""
    revisions = SOURCE_REVISION.findall(git(repo, "show", f"{head}:{PIN}").decode())
    if len(revisions) != 1:
        raise ValueError("the pinned snapshot records no single source revision")
    return revisions[0]


def newer_than_pin(repo, source, head):
    """Return whether a snapshot built from source supersedes the pin at head.

    A pin that records its source is superseded by any strictly later commit.
    An older pin without a source is superseded only while it is still the pin
    source itself carried, so an unrecorded newer pin is never replaced.
    """
    source = commit(repo, source)
    head = commit(repo, head)
    pinned = git(repo, "show", f"{head}:{PIN}").decode()
    revisions = SOURCE_REVISION.findall(pinned)
    if len(revisions) == 1:
        if revisions[0] == source:
            return False, "the pin on main already comes from this source"
        ancestor = subprocess.run(
            ["git", "-C", str(repo), "merge-base", "--is-ancestor", revisions[0], source],
            stderr=subprocess.PIPE).returncode == 0
        return ancestor, ("the pin on main comes from an earlier commit" if ancestor
                          else "the pin on main does not come from an ancestor of this source")
    if revisions:
        raise ValueError("the pinned snapshot records several source revisions")
    same = git(repo, "show", f"{source}:{PIN}").decode() == pinned
    return same, ("the pin on main is the one this source replaced" if same
                  else "a pin without a recorded source landed after this source")


def equivalent(repo, base, head, inputs):
    # Resolve commits before reading paths. Missing/zero/truncated history is unknown.
    base = commit(repo, base)
    head = git(repo, "rev-parse", "--verify", head + "^{commit}").decode().strip()
    changed = set(git(repo, "diff", "--name-only", base, head, "--", *inputs).decode().splitlines())
    other = changed - set(parser.SOURCES)
    if other:
        return False, "snapshot implementation or pinned inputs changed: " + ", ".join(sorted(other))
    with tempfile.TemporaryDirectory() as temporary:
        roots = [Path(temporary) / "before", Path(temporary) / "after"]
        for revision, root in zip((base, head), roots):
            for source in parser.SOURCES:
                path = root / source
                path.parent.mkdir(parents=True, exist_ok=True)
                path.write_bytes(git(repo, "show", f"{revision}:{source}"))
        # The extractor intentionally accepts only a subset of shell. Never let a
        # new/unrecognized package command become an empty or unchanged-set proof.
        def commands(root):
            return [line.strip() for source in parser.SOURCES
                    for line in parser.logical_lines((root / source).read_text())
                    if re.search(r"\b(?:dnf|microdnf|rpm)\b|tilo-rpm-snapshot-package", line)]
        if commands(roots[0]) != commands(roots[1]):
            return False, "package command or explicit architecture package input changed"
        for arch in ("x86_64", "aarch64"):
            sets = [{pkg for source in parser.SOURCES for pkg in parser.packages(root / source, arch)} for root in roots]
            if not all(sets):
                raise ValueError(f"empty {arch} package set")
            if sets[0] != sets[1]:
                return False, f"{arch} package set changed"
    return True, "both native package sets and all other snapshot inputs are unchanged"


def main():
    cli = argparse.ArgumentParser(description=__doc__)
    bases = cli.add_mutually_exclusive_group(required=True)
    bases.add_argument("--base")
    # Compare with the source of the snapshot pinned at head, ignoring the pin:
    # a change that landed while an earlier publication was refused as stale is
    # still seen by every later push.
    bases.add_argument("--since-pin", action="store_true")
    cli.add_argument("--head", required=True)
    cli.add_argument("--event", default="push")
    cli.add_argument("--require-equivalent", action="store_true")
    # Promotion: refuse only a snapshot that is not newer than the pin at head.
    cli.add_argument("--require-newer-than-pin", action="store_true")
    args = cli.parse_args()
    if args.require_newer_than_pin:
        try:
            newer, reason = newer_than_pin(ROOT, args.base, args.head)
        except (OSError, ValueError, subprocess.CalledProcessError, TypeError) as error:
            newer, reason = False, f"cannot order against the pin: {type(error).__name__}"
        print(json.dumps({"newer": newer, "reason": reason}))
        return int(not newer)
    try:
        inputs = list(INPUTS)
        if not inputs or not set(parser.SOURCES).issubset(inputs):
            raise ValueError("incomplete snapshot input manifest")
        if args.event == "workflow_dispatch" and not args.require_equivalent:
            same, reason = False, "manual snapshot refresh is forced"
        elif args.since_pin:
            base = pinned_source(ROOT, commit(ROOT, args.head))
            same, reason = equivalent(ROOT, base, args.head, [i for i in inputs if i != PIN])
        else:
            same, reason = equivalent(ROOT, args.base, args.head, inputs)
    except (OSError, ValueError, subprocess.CalledProcessError, SystemExit) as error:
        same, reason = False, f"cannot prove unchanged inputs: {type(error).__name__}"
    changed = not same
    print(json.dumps({"changed": changed, "reason": reason}))
    if output := os.environ.get("GITHUB_OUTPUT"):
        with open(output, "a") as stream:
            stream.write(f"changed={str(changed).lower()}\n")
    return int(args.require_equivalent and changed)


if __name__ == "__main__":
    raise SystemExit(main())
