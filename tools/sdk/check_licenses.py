"""Audit the normal/build dependency licenses of selected app configurations.

An app ships with every feature `tilo-app` declares except `test-support`, which
an app enables only from its dev-dependencies; the `all` set is exactly those
shipping features, so the audit covers what can ship and no test host.

`test-support` must also stay out of a crate a person runs: the published crates
and the binaries, and everything those reach through normal and build
dependencies. A test-only crate (unpublished, building no binary, reached only
from dev-dependencies) and what only it reaches are not part of such a program,
so what they enable reaches nobody; `test_support_from_normal_dependencies`
holds that line.
"""
import argparse
import json
import os
from pathlib import Path
import subprocess
import sys
import tomllib

ROOT = Path(__file__).resolve().parents[2]
APP_MANIFEST = ROOT / "crates/sdk/tilo-app/Cargo.toml"
TARGETS = (
    "x86_64-unknown-linux-gnu", "aarch64-unknown-linux-gnu",
    "x86_64-apple-darwin", "aarch64-apple-darwin",
    "x86_64-pc-windows-msvc", "aarch64-pc-windows-msvc",
    "wasm32-unknown-unknown",
)
HOST_ONLY = frozenset((
    "tilo-runtime-fs", "tilo-fs", "tilo-model-gateway",
    "tilo-identity", "account-credentials", "tilo-accounts", "local-model-service",
))


def findings(output, allowed):
    errors = []
    count = 0
    for line in output.splitlines():
        if not line.strip():
            continue
        package, separator, license_expression = line.partition("\t")
        if not separator:
            raise ValueError("Cargo tree output has no license field: " + line)
        count += 1
        name = package.split()[0]
        expression = license_expression.removesuffix(" (*)").strip()
        if name in HOST_ONLY:
            errors.append(f"{name}: host implementation in the app dependency graph")
        if expression not in allowed:
            errors.append(f"{name}: unapproved or missing license expression {expression!r}")
    if not count:
        raise ValueError("Cargo returned no dependencies to audit")
    return sorted(set(errors))


def audited_features(manifest):
    """Every feature an app ships with: all of `tilo-app`'s but `test-support`."""
    return sorted(name for name in manifest.get("features", {}) if name != "test-support")


def _feature(name):
    return not name.startswith("dep:") and not name.endswith("?")


def enabled_features(package, dependencies):
    """The features the graph enables on `package`: its defaults, what a dependency names (on
    any edge, so a test build's selection is covered too), and what those features include."""
    declared = package.get("features") or {}
    enabled = {name for name in declared.get("default") or () if _feature(name)}
    for dependency in dependencies:
        enabled.update(name for name in dependency["features"] if _feature(name))
    pending = list(enabled)
    while pending:
        for included in declared.get(pending.pop(), ()):
            if _feature(included) and included not in enabled:
                enabled.add(included)
                pending.append(included)
    return enabled


def builds_a_binary(package, features):
    """Whether one of `package`'s binaries has every required feature enabled, so it builds.
    A generated crate's stub target requires a feature nothing enables, so it builds nothing."""
    return any(
        "bin" in target["kind"]
        and set(target.get("required-features") or ()) <= features
        for target in package["targets"]
    )


def shipped_packages(metadata):
    """The crates a person runs, and what they reach through normal and build dependencies.

    A crate a person runs is published (`publish` names registries or anywhere) or builds a
    binary. Reachability follows normal and build edges only: a test-only crate is unpublished,
    builds no binary and is reached only from dev-dependencies, so nothing it builds or enables
    is in such a program.
    """
    packages = {package["name"]: package for package in metadata["packages"]}
    features = {
        name: enabled_features(package, [
            dependency
            for other in packages.values()
            for dependency in other["dependencies"]
            if dependency["name"] == name
        ])
        for name, package in packages.items()
    }
    pending = [
        name for name, package in packages.items()
        if package.get("publish") != [] or builds_a_binary(package, features[name])
    ]
    shipped = set()
    while pending:
        name = pending.pop()
        if name in shipped:
            continue
        shipped.add(name)
        pending += [
            dependency["name"]
            for dependency in packages[name]["dependencies"]
            if dependency["kind"] != "dev" and dependency["name"] in packages
        ]
    return shipped


def test_support_from_normal_dependencies(metadata):
    """Crates a person runs that enable `tilo-app`'s test feature themselves.

    `all` leaves `test-support` out because only dev-dependencies enable it. A shipped crate
    doing the same would put the test hosts back in the program's graph, so it fails the audit
    instead of hiding behind the exclusion. A test-only crate enabling it is where the test host
    lives, and ships nowhere.
    """
    packages = {package["name"]: package for package in metadata["packages"]}
    return sorted({
        f"{name}: enables tilo-app's test-support from a normal dependency"
        for name in shipped_packages(metadata)
        for dependency in packages[name]["dependencies"]
        if dependency["name"] == "tilo-app"
        and dependency["kind"] != "dev"
        and "test-support" in dependency["features"]
    })


def test_support_violations(cargo, offline=False):
    command = [cargo, "metadata", "--locked", "--no-deps", "--format-version", "1"]
    if offline:
        command.append("--offline")
    metadata = json.loads(subprocess.check_output(command, cwd=ROOT, text=True))
    return test_support_from_normal_dependencies(metadata)


def check(target, feature_set, features, allowed, offline=False):
    command = [
        os.environ.get("CARGO", "cargo"), "tree", "--locked", "-p", "tilo-app",
        "--target", target, "--edges", "normal,build", "--prefix", "none",
        "--format", "{p}\t{l}",
    ]
    if feature_set == "all":
        if not features:
            raise ValueError("tilo-app declares no shipping features")
        command += ["--features", ",".join(features)]
    elif feature_set == "minimal":
        command.append("--no-default-features")
    if offline:
        command.append("--offline")
    output = subprocess.check_output(command, cwd=ROOT, text=True)
    return findings(output, allowed)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--target", action="append", help="repeat to select targets; default: every supported SDK target")
    parser.add_argument("--feature-set", choices=("minimal", "default", "all"), action="append")
    parser.add_argument("--offline", action="store_true")
    args = parser.parse_args()
    policy = tomllib.loads((ROOT / "tools/sdk/license-policy.toml").read_text())
    allowed = frozenset(policy["allowed_expressions"])
    shipping = audited_features(tomllib.loads(APP_MANIFEST.read_text()))
    failed = False
    try:
        violations = test_support_violations(os.environ.get("CARGO", "cargo"), args.offline)
    except (OSError, ValueError, KeyError, subprocess.CalledProcessError) as error:
        print(f"could not audit how packages enable test-support: {error}", file=sys.stderr)
        return 1
    for violation in violations:
        print(violation, file=sys.stderr)
    failed |= bool(violations)
    for target in args.target or TARGETS:
        for features in args.feature_set or ("minimal", "default", "all"):
            try:
                errors = check(target, features, shipping, allowed, args.offline)
            except (OSError, ValueError, subprocess.CalledProcessError) as error:
                print(f"{target}/{features}: could not audit: {error}", file=sys.stderr)
                return 1
            for error in errors:
                print(f"{target}/{features}: {error}", file=sys.stderr)
            failed |= bool(errors)
            if not errors:
                print(f"{target}/{features}: app license graph OK")
    return int(failed)


if __name__ == "__main__":
    sys.exit(main())
