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
# mirror this list minus the pin itself: a pin change alone never republishes,
# but a newer pin on main makes an in-flight publication stale. build-kernel.yml
# reads the same list, so both workflows agree on when a snapshot is refreshed.
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
    "os/rpms-snapshot.env",
)


def git(repo, *args):
    return subprocess.check_output(["git", "-C", str(repo), *args], stderr=subprocess.PIPE)


def equivalent(repo, base, head, inputs):
    # Resolve commits before reading paths. Missing/zero/truncated history is unknown.
    base = git(repo, "rev-parse", "--verify", base + "^{commit}").decode().strip()
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
    cli.add_argument("--base", required=True)
    cli.add_argument("--head", required=True)
    cli.add_argument("--event", default="push")
    cli.add_argument("--require-equivalent", action="store_true")
    args = cli.parse_args()
    try:
        inputs = list(INPUTS)
        if not inputs or not set(parser.SOURCES).issubset(inputs):
            raise ValueError("incomplete snapshot input manifest")
        if args.event == "workflow_dispatch" and not args.require_equivalent:
            same, reason = False, "manual snapshot refresh is forced"
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
