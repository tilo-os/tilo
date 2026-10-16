#!/usr/bin/env python3
"""Deterministic app identities for development and continuous main releases."""
import argparse
import copy
import json
from pathlib import Path
import re
import subprocess
import tomllib


def source_identity(root, source, main_ref="refs/remotes/origin/main"):
    def git(*args):
        return subprocess.check_output(["git", "-C", str(root), *args], text=True).strip()
    if not re.fullmatch(r"[0-9a-f]{40}", source):
        raise ValueError("development source must be a full lowercase commit SHA")
    if git("rev-parse", "--is-shallow-repository") != "false":
        raise ValueError("development versions require complete main history")
    history = git("rev-list", "--first-parent", main_ref).splitlines()
    if source not in history:
        raise ValueError("development source must be on authoritative main first-parent history")
    return len(history) - history.index(source)


def development_version(declared, sequence, source):
    match = re.fullmatch(r"(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)", declared)
    if not match or not 0 < sequence < 2**64 or not re.fullmatch(r"[0-9a-f]{40}", source):
        raise ValueError("development version requires stable SemVer, positive sequence, and full source SHA")
    major, minor, patch = map(int, match.groups())
    if max(major, minor, patch + 1) >= 2**64:
        raise ValueError("development version exceeds SemVer integer range")
    return f"{major}.{minor}.{patch + 1}-dev.{sequence}+g{source}"


def release_version(declared, sequence, source):
    match = re.fullmatch(r"(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)", declared)
    if not match or not 0 < sequence < 2**64 or not re.fullmatch(r"[0-9a-f]{40}", source):
        raise ValueError("release version requires stable SemVer, positive sequence, and full source SHA")
    major, minor, patch = map(int, match.groups())
    release_patch = patch + sequence
    if max(major, minor, release_patch) >= 2**64:
        raise ValueError("release version exceeds SemVer integer range")
    return f"{major}.{minor}.{release_patch}+g{source}"


def version_plan(plan, sequence, source, *, stable=False):
    result = copy.deepcopy(plan)
    for app in result["catalog"]:
        app["declared_version"] = app["version"]
        version = release_version if stable else development_version
        app["version"] = version(app["version"], sequence, source)
    return result


def stage_manifest(path, version):
    text = path.read_text()
    # Only the signed package version changes; dependency/runtime versions do not.
    section = re.search(r"(?ms)^\[package\]\s*\n(.*?)(?=^\[|\Z)", text)
    if section is None:
        raise ValueError("manifest has no package section")
    body, count = re.subn(r'(?m)^version\s*=\s*"[^"\n]+"\s*$', f'version = "{version}"', section[1])
    if count != 1:
        raise ValueError("manifest must have exactly one package version")
    staged = text[:section.start(1)] + body + text[section.end(1):]
    if tomllib.loads(staged)["package"]["version"] != version:
        raise ValueError("staged version mismatch")
    path.write_text(staged)



def collection_plan(plan, built, prior):
    """Retain signed artifact versions for unchanged apps; never restamp reuse."""
    result = copy.deepcopy(plan)
    def members(index):
        rows = index.get("apps", [])
        mapped = {row["name"]: row for row in rows}
        if len(mapped) != len(rows):
            raise ValueError("duplicate app in artifact index")
        return mapped
    current, previous = members(built), members(prior)
    expected = {app["name"] for app in result["catalog"]}
    if not set(current) <= expected:
        raise ValueError("built app is outside the collection")
    for app in result["catalog"]:
        artifact = current.get(app["name"], previous.get(app["name"]))
        if artifact is None:
            raise ValueError(f"missing collection app: {app['name']}")
        declared = app.get("declared_version", app["version"])
        if artifact.get("declared_version", artifact["version"]) != declared:
            raise ValueError(f"declared app version changed; rebuild {app['name']}")
        for key in ("id", "package"):
            if artifact[key] != app[key]:
                raise ValueError(f"artifact identity mismatch: {app['name']} {key}")
        # A packed app names the exact runtime build it links
        # (`<release>+native.<digest>`) where its manifest declares the release.
        if artifact["runtime"].split("+native.", 1)[0] != app["runtime"]:
            raise ValueError(f"artifact identity mismatch: {app['name']} runtime")
        app["declared_version"] = declared
        app["version"] = artifact["version"]
    return result


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--root", type=Path, default=Path.cwd())
    parser.add_argument("--source")
    parser.add_argument("--sequence", type=int)
    parser.add_argument("--stable", action="store_true")
    parser.add_argument("--main-ref", default="refs/remotes/origin/main")
    parser.add_argument("--plan", type=Path)
    parser.add_argument("--built-index", type=Path)
    parser.add_argument("--prior-index", type=Path)
    parser.add_argument("--manifest", type=Path)
    parser.add_argument("--version")
    args = parser.parse_args()
    if args.manifest:
        if not args.version or not re.fullmatch(r"[0-9A-Za-z.+-]+", args.version):
            parser.error("a safe version is required")
        stage_manifest(args.manifest, args.version)
    elif args.built_index:
        plan = collection_plan(json.loads(args.plan.read_text()),
                               json.loads(args.built_index.read_text()),
                               json.loads(args.prior_index.read_text()) if args.prior_index else {})
        args.plan.write_text(json.dumps(plan, indent=2, sort_keys=True) + "\n")
    else:
        head = subprocess.check_output(["git", "-C", str(args.root), "rev-parse", "HEAD"], text=True).strip()
        if head != args.source:
            raise ValueError("development source must match the checked-out commit")
        subprocess.run(["git", "-C", str(args.root), "diff", "--exit-code", "HEAD", "--"], check=True, stdout=subprocess.DEVNULL)
        sequence = args.sequence
        if sequence is None:
            sequence = source_identity(args.root, args.source, args.main_ref)
        plan = version_plan(
            json.loads(args.plan.read_text()), sequence, args.source, stable=args.stable
        )
        args.plan.write_text(json.dumps(plan, indent=2, sort_keys=True) + "\n")


if __name__ == "__main__":
    main()
