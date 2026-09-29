#!/usr/bin/env python3
"""Choose whether builder qualification needs new native images."""
import argparse
import os
from pathlib import Path
import re
import subprocess


REBUILD_PATHS = {
    "dist/ci/install-build-deps.sh",
    "os/Containerfile.builder",
    "os/external-assets/github-assets.tsv",
    "os/dev/cache-github-assets.sh",
    "os/dev/install-cached-cosign.sh",
    "os/dev/install-aws-cli.sh",
    "os/dev/promote-native-index.py",
    "rust-toolchain.toml",
    "tools/ci/check-build-env.sh",
    "tools/ci/install-android-ndk.sh",
    "tools/ci/install-pdfium.sh",
    "tools/ci/native-mold-linker.sh",
    "tools/ci/verify-native-mold.sh",
}
REBUILD_PREFIXES = (".forgejo/actions/install-cosign/", ".forgejo/actions/cache-github-assets/")
WORKFLOW = ".forgejo/workflows/build-ci-builder.yml"


def job(text, name):
    lines = text.splitlines()
    marker = f"  {name}:"
    if lines.count(marker) != 1:
        raise ValueError(f"missing or ambiguous {name} job")
    start = lines.index(marker)
    end = next((i for i in range(start + 1, len(lines))
                if lines[i].startswith("  ") and not lines[i].startswith("    ")
                and lines[i].strip()), len(lines))
    return lines[start:end]


def build_contract(text):
    """Exclude only admission wiring; every native build step stays authoritative."""
    result = []
    skipping_if = False
    for line in job(text, "build"):
        stripped = line.strip()
        if line.startswith("    needs:"):
            continue
        if line.startswith("    if:"):
            skipping_if = stripped.endswith(">-") or stripped.endswith("|")
            continue
        if skipping_if:
            if line.startswith("      "):
                continue
            skipping_if = False
        result.append(line)
    return "\n".join(result)


def choose(event, changed, base_workflow=None, head_workflow=None):
    if event in ("schedule", "workflow_dispatch"):
        return "build", f"{event} refresh"
    if event != "push":
        return "build", "unsupported event"
    for path in changed:
        if path in REBUILD_PATHS or path.startswith(REBUILD_PREFIXES):
            return "build", f"builder input changed: {path}"
    if WORKFLOW in changed:
        try:
            if build_contract(base_workflow) != build_contract(head_workflow):
                return "build", "native build contract changed"
        except (AttributeError, ValueError):
            return "build", "native build contract could not be compared"
    return "reuse", "qualification inputs only"


def git(root, *args):
    return subprocess.check_output(["git", "-C", str(root), *args], stderr=subprocess.PIPE)


def plan(root, event, before, head):
    if event != "push":
        return choose(event, [])
    full = re.compile(r"[0-9a-f]{40}")
    if not full.fullmatch(before or "") or not full.fullmatch(head or "") or set(before) == {"0"}:
        return "build", "comparison revisions unavailable"
    try:
        for revision in (before, head):
            if git(root, "cat-file", "-t", revision).strip() != b"commit":
                return "build", "comparison revision is not a commit"
        changed = [value.decode("utf-8", "surrogateescape") for value in
                   git(root, "diff", "--name-only", "--no-renames", "-z", before, head, "--").split(b"\0")
                   if value]
        base_workflow = git(root, "show", f"{before}:{WORKFLOW}").decode()
        head_workflow = git(root, "show", f"{head}:{WORKFLOW}").decode()
    except (OSError, UnicodeError, subprocess.CalledProcessError):
        return "build", "comparison failed"
    return choose(event, changed, base_workflow, head_workflow)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--root", type=Path, default=Path.cwd())
    args = parser.parse_args()
    mode, reason = plan(args.root, os.environ.get("GITHUB_EVENT_NAME", ""),
                        os.environ.get("GITHUB_EVENT_BEFORE", ""), os.environ.get("GITHUB_SHA", ""))
    if mode not in ("build", "reuse"):
        raise ValueError("invalid builder plan")
    with Path(os.environ["GITHUB_OUTPUT"]).open("a", encoding="utf-8") as output:
        output.write(f"mode={mode}\nreason={reason}\n")
    print(f"Builder plan: {mode} ({reason})")


if __name__ == "__main__":
    main()
