"""Audit that the client side an SDK or the command line builds on stays MIT.

`check_licenses.py` guards what an app links. This guards the other graph: the
crates a program links to act on a person's workspaces from outside Tilo — the
session client, the iroh transport, identity and device certificates, keys, and
the action vocabulary. LICENSING.md licenses that side MIT, so everything it
reaches must be permissively licensed, and a Tilo crate may not join it without
being decided on in `license-policy.toml`.
"""
import argparse
import json
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


def parse(output):
    """The (package, license expression) pairs of a `cargo tree` output."""
    found = []
    for line in output.splitlines():
        if not line.strip():
            continue
        package, separator, license_expression = line.partition("\t")
        if not separator:
            raise ValueError("Cargo tree output has no license field: " + line)
        found.append((package.split()[0], license_expression.removesuffix(" (*)").strip()))
    if not found:
        raise ValueError("Cargo returned no dependencies to audit")
    return found


def findings(dependencies, allowed):
    """Every expression a platform's client graph reaches is approved."""
    return sorted({f"{name}: unapproved license expression {expression!r}"
                   for name, expression in dependencies if expression not in allowed})


def boundary(reached, client_side, workspace):
    """The Tilo crates every platform's client graph reaches are exactly the
    ones `client_side` names."""
    errors = [
        f"{name}: a Tilo crate the client side now reaches; license it MIT and "
        f"add it to client_side in tools/sdk/license-policy.toml"
        for name in sorted(reached & workspace - client_side)
    ]
    errors += [
        f"{name}: named as the client side but no longer reached"
        for name in sorted(client_side - reached)
    ]
    return errors


def tree(cargo, roots, target, offline):
    command = [
        cargo, "tree", "--locked", "--target", target,
        "--edges", "normal,build", "--prefix", "none", "--format", "{p}\t{l}",
    ]
    for root in roots:
        command += ["-p", root]
    if offline:
        command.append("--offline")
    return subprocess.check_output(command, cwd=ROOT, text=True)


def workspace_crates(cargo, offline):
    command = [cargo, "metadata", "--locked", "--no-deps", "--format-version", "1"]
    if offline:
        command.append("--offline")
    metadata = json.loads(subprocess.check_output(command, cwd=ROOT, text=True))
    return {package["name"] for package in metadata["packages"]}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--target", action="append", help="repeat to select targets; default: every supported target")
    parser.add_argument("--offline", action="store_true")
    args = parser.parse_args()
    policy = tomllib.loads((ROOT / "tools/sdk/license-policy.toml").read_text())
    allowed = frozenset(policy["allowed_expressions"])
    client_roots = policy["client_roots"]
    client_side = frozenset(policy["client_side"])
    cargo = os.environ.get("CARGO", "cargo")
    reached = set()
    failed = False
    try:
        workspace = workspace_crates(cargo, args.offline)
        for target in args.target or TARGETS:
            dependencies = parse(tree(cargo, client_roots, target, args.offline))
            reached |= {name for name, _ in dependencies}
            errors = findings(dependencies, allowed)
            for error in errors:
                print(f"{target}: {error}", file=sys.stderr)
            failed |= bool(errors)
            if not errors:
                print(f"{target}: client license graph OK")
    except (OSError, ValueError, KeyError, subprocess.CalledProcessError) as error:
        print(f"could not audit the client license graph: {error}", file=sys.stderr)
        return 1
    # Every platform the client builds for shares one boundary: a crate only the
    # secret store's platform reaches still belongs to the list.
    for error in boundary(reached, client_side, workspace):
        print(error, file=sys.stderr)
        failed = True
    return int(failed)


if __name__ == "__main__":
    sys.exit(main())
