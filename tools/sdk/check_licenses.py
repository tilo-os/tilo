"""Audit the normal/build dependency licenses of selected app configurations."""
import argparse
import os
from pathlib import Path
import subprocess
import sys
import tomllib

ROOT = Path(__file__).resolve().parents[2]
TARGETS = (
    "x86_64-unknown-linux-gnu", "aarch64-unknown-linux-gnu",
    "x86_64-apple-darwin", "aarch64-apple-darwin",
    "x86_64-pc-windows-msvc", "aarch64-pc-windows-msvc",
    "wasm32-unknown-unknown",
)
HOST_ONLY = frozenset((
    "runtime-fs", "runtime-identity", "tilo-fs", "tilo-model-gateway",
    "tilo-identity-host", "account-custody", "accounts", "local-model-service",
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


def check(target, feature_set, allowed, offline=False):
    command = [
        os.environ.get("CARGO", "cargo"), "tree", "--locked", "-p", "tilo-app",
        "--target", target, "--edges", "normal,build", "--prefix", "none",
        "--format", "{p}\t{l}",
    ]
    if feature_set == "all":
        command.append("--all-features")
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
    failed = False
    for target in args.target or TARGETS:
        for features in args.feature_set or ("minimal", "default", "all"):
            try:
                errors = check(target, features, allowed, args.offline)
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
