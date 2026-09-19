#!/usr/bin/env python3
"""Gate expensive PR jobs using their declared quoted YAML path list."""
import argparse
import json
import os
from pathlib import Path
import re
import subprocess
import sys


def declared_paths(workflow):
    """Read the deliberately restricted, quoted PR paths block; reject ambiguity."""
    lines = Path(workflow).read_text(encoding="utf-8").splitlines()
    if lines.count("  pull_request:") != 1:
        raise ValueError("missing or ambiguous pull_request event")
    start = lines.index("  pull_request:") + 1
    end = next((i for i in range(start, len(lines))
                if lines[i].strip() and not lines[i].lstrip().startswith("#")
                and len(lines[i]) - len(lines[i].lstrip()) <= 2), len(lines))
    event = lines[start:end]
    if event.count("    paths:") != 1:
        raise ValueError("missing or ambiguous PR paths")
    paths = []
    for line in event[event.index("    paths:") + 1:]:
        if not line.strip() or line.lstrip().startswith("#"):
            continue
        if not line.startswith('      - "'):
            raise ValueError("PR paths must be a block of JSON-quoted strings")
        value = json.loads(line[len("      - "):])
        if not isinstance(value, str) or not value:
            raise ValueError("invalid PR path")
        paths.append(value)
    if not paths:
        raise ValueError("empty PR paths")
    return paths


def pattern_regex(pattern):
    # Fail rather than silently mishandle future negation/brace/character classes.
    if not re.fullmatch(r"[A-Za-z0-9_./*?+-]+", pattern):
        raise ValueError(f"unsupported path pattern: {pattern!r}")
    result, i = "", 0
    while i < len(pattern):
        if pattern[i:i+3] == "**/":
            result += "(?:.*/)?"; i += 3
        elif pattern[i:i+2] == "**":
            result += ".*"; i += 2
        elif pattern[i] == "*":
            result += "[^/]*"; i += 1
        elif pattern[i] == "?":
            result += "[^/]"; i += 1
        else:
            result += re.escape(pattern[i]); i += 1
    return re.compile(result + r"\Z")


def applicable(repo, event_name, event, paths):
    patterns = [pattern_regex(p) for p in paths]
    if event_name in ("push", "workflow_dispatch"):
        return True, []
    if event_name != "pull_request":
        raise ValueError("unsupported workflow event")
    def git(*args):
        return subprocess.check_output(["git", "-C", str(repo), *args], stderr=subprocess.PIPE)
    refs = [event["pull_request"][side]["sha"] for side in ("base", "head")]
    for ref in refs:
        if not isinstance(ref, str) or not re.fullmatch(r"[0-9a-f]{40}|[0-9a-f]{64}", ref):
            raise ValueError("PR comparison requires immutable full commit IDs")
        if git("cat-file", "-t", ref).strip() != b"commit":
            raise ValueError("PR revision is not a commit")
    ancestor = git("merge-base", *refs).decode().strip()
    changed = git("diff", "--name-only", "--no-renames", "-z", ancestor, refs[1], "--").decode().split("\0")
    matched = [path for path in changed if path and any(p.fullmatch(path) for p in patterns)]
    return bool(matched), matched


def main():
    cli = argparse.ArgumentParser(description=__doc__)
    cli.add_argument("--workflow", required=True)
    args = cli.parse_args()
    try:
        paths = declared_paths(args.workflow)
        event = json.loads(Path(os.environ["GITHUB_EVENT_PATH"]).read_text())
        result, matched = applicable(Path.cwd(), os.environ["GITHUB_EVENT_NAME"], event, paths)
        print(json.dumps({"applicable": result, "matched_paths": matched}))
        with open(os.environ["GITHUB_OUTPUT"], "a") as stream:
            stream.write(f"applicable={str(result).lower()}\n")
    except (OSError, ValueError, KeyError, TypeError, subprocess.CalledProcessError) as error:
        print(f"Cannot prove workflow applicability: {type(error).__name__}: {error}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
