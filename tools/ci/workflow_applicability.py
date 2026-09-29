#!/usr/bin/env python3
"""Gate expensive PR jobs using their declared quoted YAML path list."""
import argparse
import json
import os
from pathlib import Path
import re
import subprocess
import sys
import tomllib


APPLICABILITY_INPUT = "tools/ci/workflow_applicability.py"
HISTORY_INPUT = "tools/ci/prepare_pr_history.py"
APPLICABILITY_GUARD = (
    "(github.event_name != 'pull_request' || "
    "(needs.applicability.result == 'success' && "
    "needs.applicability.outputs.applicable == 'true')) && "
)


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


def applicability_wiring(source):
    """Return jobs with complete gate wiring; reject partial wiring."""
    lines = source.splitlines()
    if lines.count("jobs:") != 1:
        raise ValueError("workflow must have one jobs section")
    jobs_start = lines.index("jobs:")
    starts = [i for i in range(jobs_start + 1, len(lines))
              if re.fullmatch(r"  [A-Za-z0-9_-]+:", lines[i])]
    wired = set()
    for index, start in enumerate(starts):
        name = lines[start].strip()[:-1]
        if name == "applicability":
            continue
        end = starts[index + 1] if index + 1 < len(starts) else len(lines)
        block = lines[start + 1:end]
        needs_gate = False
        guard_gate = False
        i = 0
        while i < len(block):
            line = block[i]
            if line.startswith("    needs:"):
                raw = line.split(":", 1)[1].strip()
                needs_gate = bool(re.search(r"(?:^|[\[, ]\s*)applicability(?:\s*[] ,]|$)", raw))
            elif line.startswith("    if:"):
                raw = line.split(":", 1)[1].strip()
                if raw in (">", ">-", "|", "|-"):
                    parts = []
                    i += 1
                    while i < len(block) and (not block[i].strip() or block[i].startswith("      ")):
                        if block[i].strip():
                            parts.append(block[i].strip())
                        i += 1
                    raw = " ".join(parts)
                    i -= 1
                guard_gate = APPLICABILITY_GUARD in re.sub(r"\s+", " ", raw).strip()
            i += 1
        if needs_gate != guard_gate:
            raise ValueError(f"partial applicability wiring for job {name}")
        if needs_gate:
            wired.add(name)
    if "  applicability:" in lines and not wired:
        raise ValueError("applicability job does not guard any job")
    return wired


def workflow_contract(source):
    """Project a workflow onto everything that can change build/publication."""
    applicability_wiring(source)
    lines = source.splitlines()
    if lines.count("on:") != 1 or lines.count("jobs:") != 1:
        raise ValueError("workflow must have one on section and one jobs section")
    on_start = lines.index("on:")
    jobs_start = lines.index("jobs:")
    if on_start >= jobs_start:
        raise ValueError("workflow sections are out of order")
    on_end = next((i for i in range(on_start + 1, len(lines))
                   if lines[i] and not lines[i].startswith((" ", "#"))), len(lines))

    projected = []
    applicability_jobs = 0
    i = 0
    while i < len(lines):
        line = lines[i]
        if on_start < i < on_end and line in (f'      - "{APPLICABILITY_INPUT}"', f'      - "{HISTORY_INPUT}"'):
            i += 1
            continue

        if i > jobs_start and line == "  applicability:":
            applicability_jobs += 1
            i += 1
            while i < len(lines) and not re.fullmatch(r"  [A-Za-z0-9_-]+:", lines[i]):
                i += 1
            continue

        if i > jobs_start and line.startswith("    needs:"):
            raw = line.split(":", 1)[1].strip()
            if raw.startswith("[") and raw.endswith("]"):
                needs = [item.strip() for item in raw[1:-1].split(",") if item.strip()]
            elif re.fullmatch(r"[A-Za-z0-9_-]+", raw):
                needs = [raw]
            else:
                raise ValueError("unsupported job needs syntax")
            needs = [item for item in needs if item != "applicability"]
            if needs:
                projected.append("    needs: " + ",".join(needs))
            i += 1
            continue

        if i > jobs_start and line.startswith("    if:"):
            raw = line.split(":", 1)[1].strip()
            if raw in (">", ">-", "|", "|-"):
                parts = []
                i += 1
                while i < len(lines) and (not lines[i].strip() or lines[i].startswith("      ")):
                    if lines[i].strip():
                        parts.append(lines[i].strip())
                    i += 1
                raw = " ".join(parts)
            else:
                i += 1
            expression = re.sub(r"\s+", " ", raw).strip()
            expression = expression.replace(APPLICABILITY_GUARD, "")
            if "needs.applicability" in expression:
                raise ValueError("unsupported applicability guard")
            if expression != "${{ always() && true }}":
                projected.append("    if: " + expression)
            continue

        if line.strip():
            projected.append(line)
        i += 1

    if applicability_jobs > 1:
        raise ValueError("ambiguous applicability job")
    return "\n".join(projected) + "\n"


def workflow_contract_changed(repo, base, head, workflow):
    """Return true on a contract difference or when comparison is unprovable."""
    path = Path(workflow).as_posix()
    if Path(path).is_absolute() or path.startswith("../"):
        return True
    try:
        def show(ref):
            return subprocess.check_output(
                ["git", "-C", str(repo), "show", f"{ref}:{path}"],
                stderr=subprocess.PIPE,
            ).decode()
        base_source, head_source = show(base), show(head)
        if not applicability_wiring(base_source).issubset(applicability_wiring(head_source)):
            return True
        return workflow_contract(base_source) != workflow_contract(head_source)
    except (OSError, UnicodeError, ValueError, subprocess.CalledProcessError):
        return True


ROOT_CARGO_FILES = ("Cargo.lock", "Cargo.toml")


def cargo_closure_changed(repo, base, head, packages):
    """Whether the root Cargo files changed anything the packages build with.

    Compares every Cargo.lock entry reachable from the packages, and the root
    manifest with workspace membership, metadata and the dependency, profile
    and patch entries of crates outside that closure left out. Changes to the
    packages' own sources and manifests are path triggers of their own.
    """
    def show(ref, path):
        return tomllib.loads(subprocess.check_output(
            ["git", "-C", str(repo), "show", f"{ref}:{path}"], stderr=subprocess.PIPE).decode())

    def closure(lock):
        by_name = {}
        for entry in lock.get("package", []):
            by_name.setdefault(entry["name"], []).append(entry)
        seen = {}
        stack = [entry for name in packages for entry in by_name.get(name, [])
                 if "source" not in entry]
        while stack:
            entry = stack.pop()
            key = (entry["name"], entry["version"], entry.get("source", ""))
            if key in seen:
                continue
            seen[key] = (entry.get("checksum", ""), tuple(sorted(entry.get("dependencies", []))))
            for spec in entry.get("dependencies", []):
                name, *version = spec.split(" ")
                matches = by_name.get(name, [])
                if version:
                    matches = [m for m in matches if m["version"] == version[0]] or matches
                stack.extend(matches)
        return seen

    def manifest(root, names):
        root = dict(root)
        workspace = {key: value for key, value in root.get("workspace", {}).items()
                     if key not in ("members", "exclude", "default-members", "metadata")}
        workspace["dependencies"] = {name: value for name, value in
                                     workspace.get("dependencies", {}).items() if name in names}
        root["workspace"] = workspace
        profiles = {}
        for name, profile in root.get("profile", {}).items():
            profile = dict(profile)
            overrides = {crate: value for crate, value in profile.pop("package", {}).items()
                         if crate in names}
            if overrides:
                profile["package"] = overrides
            if profile:
                profiles[name] = profile
        root["profile"] = profiles
        root["patch"] = {source: kept for source, crates in root.get("patch", {}).items()
                         if (kept := {crate: value for crate, value in crates.items()
                                      if crate in names})}
        return root

    before, after = closure(show(base, "Cargo.lock")), closure(show(head, "Cargo.lock"))
    if not before or before != after:
        return True
    names = {name for name, _, _ in before}
    return manifest(show(base, "Cargo.toml"), names) != manifest(show(head, "Cargo.toml"), names)


def applicable(repo, event_name, event, paths, workflow=None, cargo_packages=()):
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
    if workflow is not None:
        workflow = Path(workflow).as_posix()
        contract_inputs = {APPLICABILITY_INPUT, HISTORY_INPUT, workflow}
        direct = [path for path in matched if path not in contract_inputs]
        if (cargo_packages and set(direct) & set(ROOT_CARGO_FILES)
                and not cargo_closure_changed(repo, ancestor, refs[1], cargo_packages)):
            direct = [path for path in direct if path not in ROOT_CARGO_FILES]
        if direct:
            return True, matched
        if matched and workflow_contract_changed(repo, ancestor, refs[1], workflow):
            return True, [workflow]
        return False, []
    return bool(matched), matched


def main():
    cli = argparse.ArgumentParser(description=__doc__)
    cli.add_argument("--workflow", required=True)
    cli.add_argument("--event", help="event name when run from a called workflow")
    cli.add_argument("--cargo-package", action="append", default=[],
                     help="count root Cargo.lock/Cargo.toml changes only when they change "
                          "what this package builds with")
    args = cli.parse_args()
    try:
        paths = declared_paths(args.workflow)
        event = json.loads(Path(os.environ["GITHUB_EVENT_PATH"]).read_text())
        result, matched = applicable(
            Path.cwd(), args.event or os.environ["GITHUB_EVENT_NAME"], event, paths, args.workflow,
            args.cargo_package
        )
        print(json.dumps({"applicable": result, "matched_paths": matched}))
        with open(os.environ["GITHUB_OUTPUT"], "a") as stream:
            stream.write(f"applicable={str(result).lower()}\n")
    except (OSError, ValueError, KeyError, TypeError, subprocess.CalledProcessError) as error:
        print(f"Cannot prove workflow applicability: {type(error).__name__}: {error}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
