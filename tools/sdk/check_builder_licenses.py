"""Audit that what people build on from crates.io stays MIT, end to end.

The builder crates (`builder_roots` in `license-policy.toml`: the toolkit, the
app SDK and tilolang) are published, so each one's whole graph ships to whoever
depends on it. LICENSING.md licenses them MIT, so:

- every Tilo crate their manifests reach is licensed with an approved
  expression; crates.io needs every dependency a manifest names, optional ones
  included, so the graph is read from the manifests, not from one feature
  selection;
- every third-party crate they reach on any supported target, with every
  feature a builder crate ships (all but `test-support`), is approved too.

Every Tilo crate they reach is published with them, so it must be publishable too: `publish`
not false, a `tilo-` or `tilolang` package name, and a version on every path dependency its
published manifest keeps (crates.io takes no path).

`--manifests` checks only what the manifests say (`cargo metadata` and a walk of the graph, in
seconds): what every push to main runs, and the pre-push hook. The full check adds the
third-party graph on every target.

A crate's own license field is the decision: shell, session, host and server
code stays AGPL, so a builder crate reaching one fails here, and the dependency
is what gets fixed. Which crates a group may link is the crate layout's
(tools/policy/crate_layout.toml), not this check's.

The toolkit (`tilo-ui`) is held to more than its license: it must not depend on Tilo, so
`--manifests` also fails when its manifests reach an in-repo crate that `toolkit_reaches` and
`toolkit_reaches_to_cut` in `license-policy.toml` do not list, naming the dependency that
reaches it, and when a listed crate is no longer reached, so the lists only shrink.
"""
import argparse
import importlib.util
import json
import os
from pathlib import Path
import subprocess
import sys
import tomllib

ROOT = Path(__file__).resolve().parents[2]
_SPEC = importlib.util.spec_from_file_location("client_licenses", ROOT / "tools/sdk/check_client_licenses.py")
client_licenses = importlib.util.module_from_spec(_SPEC)
_SPEC.loader.exec_module(client_licenses)
TARGETS = client_licenses.TARGETS
# Features a builder crate declares that no shipped program turns on: an app enables
# them from its dev-dependencies.
UNSHIPPED_FEATURES = frozenset(("test-support",))


def published(dependency):
    """Whether a manifest dependency stays in the published manifest: normal and build ones do,
    and so does a dev-dependency that carries a version; Cargo drops one without."""
    return dependency["kind"] != "dev" or dependency["req"] != "*"


def manifest_closure(packages, roots):
    """The workspace crates `roots` reach through what their published manifests name on every
    target: normal and build dependencies, optional ones included, and dev-dependencies that
    carry a version."""
    local = {package["name"]: package for package in packages if package["source"] is None}
    missing = sorted(set(roots) - set(local))
    if missing:
        raise ValueError(f"builder roots not in the workspace: {', '.join(missing)}")
    seen = set()
    pending = list(roots)
    while pending:
        name = pending.pop()
        if name in seen:
            continue
        seen.add(name)
        for dependency in local[name]["dependencies"]:
            if published(dependency) and dependency.get("path") and dependency["name"] in local:
                pending.append(dependency["name"])
    return {name: local[name] for name in seen}


def toolkit_findings(packages, root, reaches, to_cut):
    """What the toolkit `root` reaches beyond the in-repo crates it may (`reaches`, and
    `to_cut`, the ones it reaches only until its app layer moves out), each named by the
    dependencies that bring it in; and every listed crate it no longer reaches, whose line goes."""
    local = {package["name"]: package for package in packages if package["source"] is None}
    reached = manifest_closure(packages, [root])
    findings = [
        f"{name}: listed in both toolkit_reaches and toolkit_reaches_to_cut"
        for name in sorted(set(reaches) & set(to_cut))
    ]
    listed = set(reaches) | set(to_cut)
    for name in sorted(set(reached) - listed - {root}):
        edges = sorted(
            f"{source} -> {name}"
            for source in reached
            for dependency in local[source]["dependencies"]
            if dependency["name"] == name and published(dependency) and dependency.get("path")
        )
        findings.append(
            f"{', '.join(edges)}: {root} must not depend on Tilo, and it reaches {name}, which "
            "toolkit_reaches in tools/sdk/license-policy.toml does not list"
        )
    for key, names in (("toolkit_reaches", reaches), ("toolkit_reaches_to_cut", to_cut)):
        findings.extend(
            f"{name}: {root} no longer reaches it; delete its line from {key} in "
            "tools/sdk/license-policy.toml"
            for name in sorted(set(names) - set(reached))
        )
    return findings


def manifest_findings(reached, allowed):
    return sorted(
        f"{name}: a builder crate reaches it, and its license {package.get('license')!r} is not approved"
        for name, package in reached.items()
        if package.get("license") not in allowed
    )


# What a published crate's package name starts with: whose crate it is on crates.io.
PUBLISHED_PREFIXES = ("tilo-", "tilolang")


def publishable_findings(reached):
    """Why each crate of `reached` (a builder crate or what one reaches) could not be published
    with them."""
    findings = []
    for name, package in sorted(reached.items()):
        if package.get("publish") == []:
            findings.append(f"{name}: a builder crate reaches it, and it has publish = false")
        if not name.startswith(PUBLISHED_PREFIXES):
            findings.append(f"{name}: a builder crate reaches it, and its name has no tilo- or "
                            "tilolang prefix")
        for dependency in package["dependencies"]:
            if published(dependency) and dependency.get("path") and dependency["req"] == "*":
                findings.append(f"{name}: a builder crate reaches it, and its path dependency "
                                f"{dependency['name']} has no version")
    return findings


def shipped_features(package):
    return sorted(name for name in package.get("features", {}) if name not in UNSHIPPED_FEATURES)


def tree(cargo, root, features, target, offline):
    command = [
        cargo, "tree", "--locked", "-p", root, "--target", target,
        "--edges", "normal,build", "--prefix", "none", "--format", "{p}\t{l}",
    ]
    if features:
        command += ["--features", ",".join(features)]
    if offline:
        command.append("--offline")
    return subprocess.check_output(command, cwd=ROOT, text=True)


def main():
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--target", action="append", help="repeat to select targets; default: every supported target")
    parser.add_argument("--offline", action="store_true")
    parser.add_argument("--manifests", action="store_true",
                        help="check only what the manifests say, in seconds; not the third-party graph")
    args = parser.parse_args()
    policy = tomllib.loads((ROOT / "tools/sdk/license-policy.toml").read_text())
    allowed = frozenset(policy["allowed_expressions"])
    roots = policy["builder_roots"]
    cargo = os.environ.get("CARGO", "cargo")
    command = [cargo, "metadata", "--locked", "--format-version", "1"]
    if args.offline:
        command.append("--offline")
    failed = False
    try:
        metadata = json.loads(subprocess.check_output(command, cwd=ROOT, text=True))
        reached = manifest_closure(metadata["packages"], roots)
        errors = manifest_findings(reached, allowed) + publishable_findings(reached)
        errors += toolkit_findings(metadata["packages"], "tilo-ui", policy["toolkit_reaches"],
                                   policy["toolkit_reaches_to_cut"])
        for error in errors:
            print(error, file=sys.stderr)
        failed |= bool(errors)
        if not errors:
            print(f"builder manifests: {len(reached)} Tilo crates, all approved and publishable")
        for target in [] if args.manifests else args.target or TARGETS:
            target_errors = set()
            for root in roots:
                output = tree(cargo, root, shipped_features(reached[root]), target, args.offline)
                target_errors.update(client_licenses.findings(client_licenses.parse(output), allowed))
            for error in sorted(target_errors):
                print(f"{target}: {error}", file=sys.stderr)
            failed |= bool(target_errors)
            if not target_errors:
                print(f"{target}: builder license graph OK")
    except (OSError, ValueError, KeyError, subprocess.CalledProcessError) as error:
        print(f"could not audit the builder license graph: {error}", file=sys.stderr)
        return 1
    return int(failed)


if __name__ == "__main__":
    sys.exit(main())
