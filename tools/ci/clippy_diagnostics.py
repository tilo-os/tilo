#!/usr/bin/env python3
"""Normalize, cache and compare Clippy diagnostics for tools/ci/clippy-diff.sh.

The diff counts one fingerprint per (lint, file, normalized message) and rejects
any fingerprint whose count grows between the comparison base and HEAD. The same
extraction code produces both sides of the comparison, whether the base comes
from a fresh base checkout or from the workspace-wide cache recorded for that
exact base commit on main.

Threshold lints such as `too_many_lines` embed the measured value in their
message ("this function has too many lines (104/100)"). Those values are
replaced by `#` before fingerprinting, so a function that was already over its
limit may change length without being reported again. A function that newly
crosses a limit still adds a fingerprint in its file and fails the diff.
"""
from __future__ import annotations

import argparse
import collections
import hashlib
import json
import os
import pathlib
import re
import subprocess
import sys

CACHE_FORMAT = 1
# Arguments shared by every Clippy invocation whose output can be compared.
CLIPPY_ARGS = ("--locked", "--all-targets", "--message-format=json", "--", "--cap-lints", "warn")
# Environment that changes which lints fire or how code is compiled. A cached
# base is only reused when these match the consuming job exactly.
FINGERPRINT_ENVIRONMENT = ("RUSTFLAGS", "CARGO_ENCODED_RUSTFLAGS", "CLIPPY_CONF_DIR", "CARGO_BUILD_TARGET")
# Lints whose message reports a measured size or count next to its limit.
THRESHOLD_LINTS = frozenset(
    "clippy::" + name
    for name in (
        "cognitive_complexity",
        "fn_params_excessive_bools",
        "large_const_arrays",
        "large_enum_variant",
        "large_futures",
        "large_include_file",
        "large_stack_arrays",
        "large_stack_frames",
        "large_types_passed_by_value",
        "result_large_err",
        "struct_excessive_bools",
        "too_many_arguments",
        "too_many_lines",
        "trivially_copy_pass_by_ref",
    )
)
_NUMBER = re.compile(r"\d+(?:\.\d+)?")


def normalized_message(code: str, message: str) -> str:
    if code in THRESHOLD_LINTS:
        return _NUMBER.sub("#", message)
    return message


def records(path):
    with open(path, encoding="utf-8") as stream:
        for line in stream:
            try:
                record = json.loads(line)
            except json.JSONDecodeError:
                continue
            if isinstance(record, dict):
                yield record


def sites(path, tree):
    """Return every Clippy diagnostic site in Cargo JSON output, tree-relative."""
    tree = pathlib.Path(tree).resolve()
    found = []
    for record in records(path):
        if record.get("reason") != "compiler-message":
            continue
        message = record.get("message") or {}
        code = (message.get("code") or {}).get("code", "")
        if not code.startswith("clippy::"):
            continue
        site = {
            "package": record.get("package_id"),
            "code": code,
            "message": message.get("message", ""),
            "rendered": message.get("rendered") or message.get("message", ""),
            "file": None,
            "line": None,
            "column": None,
        }
        spans = [span for span in message.get("spans", []) if span.get("is_primary")]
        if not spans:
            spans = message.get("spans", [])[:1]
        if spans:
            span = spans[0]
            file_path = pathlib.Path(span.get("file_name", ""))
            candidate = file_path if file_path.is_absolute() else tree / file_path
            try:
                file_name = candidate.resolve().relative_to(tree).as_posix()
            except ValueError:
                continue
            if file_name.startswith(("vendor/", "../")):
                continue
            site.update(file=file_name, line=span.get("line_start"), column=span.get("column_start"))
        found.append(site)
    return found


def count(site_list, path_map=None):
    """Count normalized fingerprints, de-duplicating identical source sites."""
    path_map = path_map or {}
    seen = set()
    fingerprints = collections.Counter()
    examples = collections.defaultdict(list)
    for site in site_list:
        message = normalized_message(site["code"], site["message"])
        if site["file"] is None:
            fingerprint = (site["code"], "<no-span>", message, "")
            key = fingerprint
        else:
            # Count a diagnostic kind/message per file rather than treating
            # edited source text as a new lint site. Line/column remain in the
            # key for de-duplication, while the Counter still detects an
            # increased number of equivalent warnings in that file.
            fingerprint = (site["code"], path_map.get(site["file"], site["file"]), message, "")
            key = fingerprint + (site["line"], site["column"])
        if key in seen:
            continue
        seen.add(key)
        fingerprints[fingerprint] += 1
        examples[fingerprint].append(site.get("rendered") or site["message"])
    return fingerprints, examples


def completed_packages(path):
    return {
        record["package_id"]
        for record in records(path)
        if record.get("reason") == "compiler-artifact" and record.get("package_id")
    }


def has_compiler_errors(path):
    return any(
        record.get("reason") == "compiler-message" and (record.get("message") or {}).get("level") == "error"
        for record in records(path)
    )


def read_changes(path):
    changed = set()
    renames = {}
    for line in pathlib.Path(path).read_text(encoding="utf-8").splitlines():
        fields = line.split("\t")
        status = fields[0]
        if status.startswith("R") and len(fields) == 3:
            old_path, new_path = fields[1:]
            renames[old_path] = new_path
            changed.add(new_path)
        elif len(fields) >= 2:
            changed.add(fields[-1])
    return changed, renames


def lint_closures(metadata):
    """Map each workspace member to the members Clippy lints for `-p member`.

    `cargo clippy -p X --all-targets` also lints every workspace member in X's
    build graph: X's direct dependencies of every kind (dev-dependencies are
    built for its tests) and, transitively, their normal and build dependencies.
    """
    members = set(metadata["workspace_members"])
    nodes = {node["id"]: node for node in (metadata.get("resolve") or {}).get("nodes", [])}

    def dependencies(package, kinds):
        for dependency in nodes.get(package, {}).get("deps", []):
            if dependency["pkg"] in members and any(
                kind.get("kind") in kinds for kind in dependency.get("dep_kinds", [])
            ):
                yield dependency["pkg"]

    closures = {}
    for member in sorted(members):
        closure = {member}
        pending = [
            package for package in dependencies(member, {None, "build", "dev"}) if package not in closure
        ]
        while pending:
            package = pending.pop()
            if package in closure:
                continue
            closure.add(package)
            pending.extend(dependencies(package, {None, "build"}))
        closures[member] = sorted(closure)
    return closures


def fingerprint_inputs(image):
    """Return the tool and environment identity a cached base is bound to."""
    root = pathlib.Path(__file__).resolve().parents[2]
    parts = [
        f"format={CACHE_FORMAT}",
        "rustc=" + subprocess.check_output(["rustc", "-vV"], text=True),
        "clippy=" + subprocess.check_output(["cargo", "clippy", "-V"], text=True),
        "image=" + image,
        "args=" + " ".join(CLIPPY_ARGS),
    ]
    parts.extend(f"env:{name}={os.environ.get(name, '')}" for name in FINGERPRINT_ENVIRONMENT)
    for helper in ("tools/ci/clippy_diagnostics.py", "tools/ci/clippy-diff.sh"):
        parts.append(f"{helper}=" + hashlib.sha256((root / helper).read_bytes()).hexdigest())
    return parts


def cache_key(base, image):
    digest = hashlib.sha256()
    for part in fingerprint_inputs(image):
        encoded = part.encode("utf-8")
        digest.update(len(encoded).to_bytes(8, "big") + encoded)
    return f"clippy-base-v{CACHE_FORMAT}-{base}-{digest.hexdigest()[:32]}"


def export(args):
    tree = pathlib.Path(args.tree).resolve()
    if has_compiler_errors(args.messages):
        print("Workspace Clippy reported compiler errors; not recording a base cache.", file=sys.stderr)
        return 1
    metadata = json.load(open(args.metadata, encoding="utf-8"))
    members = set(metadata["workspace_members"])
    packages = []
    for package in metadata["packages"]:
        if package["id"] not in members:
            continue
        manifest = pathlib.Path(package["manifest_path"]).resolve().relative_to(tree).as_posix()
        packages.append({"name": package["name"], "id": package["id"], "manifest": manifest})
    cache = {
        "format": CACHE_FORMAT,
        "commit": args.commit,
        "key": args.key,
        "packages": sorted(packages, key=lambda package: package["name"]),
        "lint_closures": lint_closures(metadata),
        "sites": [
            {name: site[name] for name in ("package", "code", "message", "file", "line", "column")}
            for site in sites(args.messages, tree)
        ],
    }
    pathlib.Path(args.output).parent.mkdir(parents=True, exist_ok=True)
    with open(args.output, "w", encoding="utf-8") as stream:
        json.dump(cache, stream, separators=(",", ":"), sort_keys=True)
    print(f"Recorded {len(cache['sites'])} Clippy diagnostic sites for {args.commit} under {args.key}")
    return 0


def load_cache(path, commit, key):
    """Return the cached base, or None with a reason when it must not be used."""
    try:
        cache = json.load(open(path, encoding="utf-8"))
    except (OSError, ValueError) as error:
        return None, f"unreadable ({error.__class__.__name__})"
    if not isinstance(cache, dict) or cache.get("format") != CACHE_FORMAT:
        return None, "unsupported format"
    if cache.get("commit") != commit:
        return None, f"recorded for {cache.get('commit')}, not {commit}"
    if cache.get("key") != key:
        return None, "toolchain or configuration fingerprint differs"
    if not all(isinstance(cache.get(field), list) for field in ("packages", "sites")) or not isinstance(
        cache.get("lint_closures"), dict
    ):
        return None, "incomplete"
    return cache, None


def cache_check(args):
    cache, reason = load_cache(args.cache, args.commit, args.key)
    if cache is None:
        print(reason)
        return 1
    return 0


def cache_metadata(args):
    """Emit `cargo metadata --no-deps`-shaped package data for the cached base."""
    cache, reason = load_cache(args.cache, args.commit, args.key)
    if cache is None:
        print(reason, file=sys.stderr)
        return 1
    tree = pathlib.Path(args.tree)
    packages = [
        {"name": package["name"], "id": package["id"], "manifest_path": str(tree / package["manifest"])}
        for package in cache["packages"]
    ]
    json.dump({"packages": packages, "workspace_members": [package["id"] for package in packages]}, sys.stdout)
    return 0


def cached_base_sites(cache, names):
    ids = {package["name"]: package["id"] for package in cache["packages"]}
    linted = set()
    for name in names:
        if name not in ids or ids[name] not in cache["lint_closures"]:
            raise SystemExit(f"cached base does not describe package {name}")
        linted.update(cache["lint_closures"][ids[name]])
    return [site for site in cache["sites"] if site.get("package") in linted]


def compare(args):
    changed, renames = read_changes(args.changed)
    head, examples = count(sites(args.head_messages, args.head_tree))
    if args.base_cache:
        cache, reason = load_cache(args.base_cache, args.base_commit, args.base_key)
        if cache is None:
            raise SystemExit(f"cached base unusable: {reason}")
        # Map base paths to their HEAD location so moving a file does not reset
        # its lint budget. New duplicate sites still increase the counter.
        base, _ = count(cached_base_sites(cache, args.base_package), renames)
        source = f"cached base {args.base_commit[:12]}"
    else:
        base, _ = count(sites(args.base_messages, args.base_tree), renames)
        source = "base"
    introduced = head - base
    if not args.base_cache and args.base_compiled != "true":
        # An early compiler error can prevent Clippy from visiting entire
        # dependent packages. A missing diagnostic in one of those packages is
        # not evidence that HEAD introduced it. Keep enforcing the diff for
        # changed files in packages that produced a completed compiler artifact,
        # and for new packages that have no base baseline to preserve.
        metadata = json.load(open(args.base_metadata, encoding="utf-8"))
        compiled = completed_packages(args.base_messages)
        base_tree = pathlib.Path(args.base_tree).resolve()
        package_roots = []
        for package in metadata["packages"]:
            manifest = pathlib.Path(package["manifest_path"]).resolve()
            relative_manifest = manifest.relative_to(base_tree).as_posix()
            head_manifest = renames.get(relative_manifest, relative_manifest)
            package_roots.append((pathlib.PurePosixPath(head_manifest).parent, package["id"]))
        package_roots.sort(key=lambda entry: len(entry[0].parts), reverse=True)

        def base_package_for(file_name):
            path = pathlib.PurePosixPath(file_name)
            for root, package in package_roots:
                if path == root or root in path.parents:
                    return package
            return None

        introduced = collections.Counter(
            {
                fingerprint: number
                for fingerprint, number in introduced.items()
                if fingerprint[1] == "<no-span>"
                or (
                    fingerprint[1] in changed
                    and (
                        base_package_for(fingerprint[1]) is None
                        or base_package_for(fingerprint[1]) in compiled
                    )
                )
            }
        )
    if not introduced:
        print(f"Clippy diagnostic diff clean ({sum(head.values())} current, {sum(base.values())} at {source})")
        return 0

    print(f"introduced {sum(introduced.values())} Clippy diagnostic site(s):", file=sys.stderr)
    for fingerprint, number in introduced.items():
        if base[fingerprint]:
            # Equivalent sites cannot be told apart; show every current one.
            print(f"{number} more `{fingerprint[0]}` site(s) in {fingerprint[1]} than at {source}, among:",
                  file=sys.stderr)
            shown = examples[fingerprint]
        else:
            shown = examples[fingerprint][:number]
        for rendered in shown:
            print(rendered.rstrip(), file=sys.stderr)
    return 1


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    commands = parser.add_subparsers(dest="command", required=True)

    key = commands.add_parser("cache-key", help="print the cache key for a base commit")
    key.add_argument("--base", required=True)
    key.add_argument("--image", required=True)

    record = commands.add_parser("export", help="record a workspace Clippy run as a base cache")
    record.add_argument("--messages", required=True)
    record.add_argument("--tree", required=True)
    record.add_argument("--metadata", required=True, help="cargo metadata including resolve")
    record.add_argument("--commit", required=True)
    record.add_argument("--key", required=True)
    record.add_argument("--output", required=True)

    for name in ("cache-check", "cache-metadata"):
        command = commands.add_parser(name)
        command.add_argument("--cache", required=True)
        command.add_argument("--commit", required=True)
        command.add_argument("--key", required=True)
        if name == "cache-metadata":
            command.add_argument("--tree", required=True)

    diff = commands.add_parser("compare", help="compare HEAD diagnostics with a base")
    diff.add_argument("--head-messages", required=True)
    diff.add_argument("--head-tree", required=True)
    diff.add_argument("--changed", required=True)
    diff.add_argument("--base-messages")
    diff.add_argument("--base-tree")
    diff.add_argument("--base-compiled", default="true")
    diff.add_argument("--base-metadata")
    diff.add_argument("--base-cache")
    diff.add_argument("--base-commit")
    diff.add_argument("--base-key")
    diff.add_argument("--base-package", action="append", default=[])

    args = parser.parse_args(argv)
    if args.command == "cache-key":
        print(cache_key(args.base, args.image))
        return 0
    if args.command == "compare" and not args.base_cache and not (args.base_messages and args.base_tree):
        parser.error("compare needs --base-cache or --base-messages with --base-tree")
    return {"export": export, "cache-check": cache_check, "cache-metadata": cache_metadata, "compare": compare}[
        args.command
    ](args)


if __name__ == "__main__":
    raise SystemExit(main())
