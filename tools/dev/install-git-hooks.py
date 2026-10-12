#!/usr/bin/env python3
"""Install the repository hooks without replacing unrelated hook configuration."""
from pathlib import Path
import subprocess
import sys


def git(root, *args):
    return subprocess.run(["git", "-C", str(root), *args], capture_output=True, text=True, check=False)


def install(root):
    common_result = git(root, "rev-parse", "--git-common-dir")
    if common_result.returncode:
        raise ValueError("Cannot inspect the common Git directory.")
    common = Path(common_result.stdout.strip())
    if not common.is_absolute():
        common = root / common
    managed = {(root / ".githooks").resolve(), (common.resolve().parent / ".githooks").resolve()}
    configured_result = git(root, "config", "--get", "core.hooksPath")
    if configured_result.returncode not in (0, 1):
        raise ValueError("Cannot inspect existing hook configuration.")
    configured = configured_result.stdout.strip()
    if configured:
        existing = Path(configured).expanduser()
        if not existing.is_absolute():
            existing = root / existing
        if existing.resolve() not in managed:
            raise ValueError("core.hooksPath points to unrelated hooks at " + str(existing) + "; configuration was preserved. Arrange explicit chaining before installation.")
    else:
        hooks = common / "hooks"
        custom = [p.name for p in hooks.glob("*") if (p.is_file() or p.is_symlink()) and not p.name.endswith(".sample")]
        if custom:
            raise ValueError("Unrelated default hooks exist: " + ", ".join(custom) + "; no hooks or settings were replaced. Arrange explicit chaining first.")
    for name in ("pre-commit", "commit-msg"):
        path = root / ".githooks" / name
        if not path.is_file() or not path.stat().st_mode & 0o111:
            raise ValueError("Missing or nonexecutable managed hook: " + str(path))
    scope = "--worktree" if git(root, "config", "--bool", "extensions.worktreeConfig").stdout.strip() == "true" else "--local"
    result = git(root, "config", scope, "core.hooksPath", ".githooks")
    if result.returncode:
        raise ValueError(result.stderr.strip())
    if git(root, "config", "--get", "core.hooksPath").stdout.strip() != ".githooks":
        raise ValueError("Hook installation could not be verified.")
    return scope


def main():
    result = git(Path.cwd(), "rev-parse", "--show-toplevel")
    if result.returncode:
        print("Run this installer inside a Tilo checkout.", file=sys.stderr)
        return 1
    root = Path(result.stdout.strip())
    try:
        scope = install(root)
    except ValueError as error:
        print("Hook installation refused: " + str(error), file=sys.stderr)
        return 1
    print("Installed and verified pre-commit and commit-msg via " + scope + " core.hooksPath=.githooks")
    return 0


if __name__ == "__main__":
    sys.exit(main())
