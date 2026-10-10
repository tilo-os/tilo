"""Plan default source swaps against the selected disk's source identity."""

from __future__ import annotations

import re
import subprocess
import sys
from pathlib import Path


def git(*args: str, repo: Path | None = None) -> str:
    return subprocess.run(["git", *args], cwd=repo, check=True,
                          capture_output=True, text=True).stdout


def commit(identity: str, role: str, repo: Path | None) -> str:
    if not re.fullmatch(r"[0-9a-f]{40}", identity):
        raise ValueError(f"{role} needs an exact source commit; refusing an unknown baseline")
    try:
        resolved = git("rev-parse", "--verify", f"{identity}^{{commit}}", repo=repo).strip()
    except subprocess.CalledProcessError as exc:
        raise ValueError(f"{role} source commit is unavailable; refusing to guess") from exc
    if resolved != identity:
        raise ValueError(f"{role} identity does not name its source commit")
    return resolved


def plan(disk: str, target: str, repo: Path | None = None) -> str:
    disk = commit(disk, "selected disk", repo)
    target = commit(target, "target", repo)
    ancestor = subprocess.run(["git", "merge-base", "--is-ancestor", disk, target],
                              cwd=repo, capture_output=True)
    if ancestor.returncode:
        raise ValueError("selected disk is not an ancestor of the target; refusing incompatible source")
    changed = git("diff", "--name-only", disk, target, "--", repo=repo).splitlines()
    if not changed:
        return ""
    # Keep a changed app with its exact SDK through the normal signed cohort
    # transfer. One cohort also avoids rebuilding the shared graph per app.
    apps = sorted({path.split("/")[1] for path in changed
                   if path.startswith("apps/") and len(path.split("/")) > 2})
    for app in apps:
        present = subprocess.run(["git", "cat-file", "-e", f"{target}:apps/{app}/Cargo.toml"],
                                 cwd=repo, capture_output=True)
        if not present.returncode:
            return "shell,apps+runtime"
        if present.returncode != 128:
            raise ValueError("cannot inspect the target app graph; refusing incomplete source")
    return "shell"


if __name__ == "__main__":
    if len(sys.argv) != 3:
        sys.exit("usage: plan_swaps.py DISK_COMMIT TARGET_COMMIT")
    try:
        print(plan(sys.argv[1], sys.argv[2]))
    except (ValueError, subprocess.CalledProcessError) as error:
        sys.exit(f"default swap plan: {error}")
