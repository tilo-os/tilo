"""Hold the toolkit's built graph to the in-repo crates it may compile.

`tilo-ui` is a UI toolkit a Rust program can draw windows with and know nothing about Tilo.
Tilo's model (its records, CRDT sync, settings store and identity) reaches it only through the
`model` feature, which `tilo-app` turns on. This check resolves what Cargo actually builds for
`tilo-ui` and fails when:

- with every feature off, it compiles an in-repo crate `allowed` in `toolkit_closure.toml` does
  not list (naming the path that reaches it), or no longer compiles one the list keeps, so the
  list only shrinks;
- with its default features, it compiles a crate listed in `never`.

`check_builder_licenses.py --manifests` reads the manifests, optional dependencies included,
because crates.io needs each of them; this one reads what a build of the toolkit compiles.
It runs `cargo tree` on the lockfile, in seconds, with no build.
"""
import argparse
from pathlib import Path
import subprocess
import sys
import tomllib

ROOT = Path(__file__).resolve().parents[2]
POLICY = ROOT / "tools/sdk/toolkit_closure.toml"
TOOLKIT = "tilo-ui"


def local_crates(tree_output):
    """The in-repo crates in `cargo tree --prefix none` output: the lines whose package has a
    path source, without the root."""
    crates = set()
    for line in tree_output.splitlines():
        if " (/" not in line:
            continue
        crates.add(line.split(" ", 1)[0])
    return crates


def cargo_tree(*arguments):
    command = [
        "cargo", "tree", "--locked", "-p", TOOLKIT, "-e", "normal,build", "--target", "all",
        *arguments,
    ]
    return subprocess.run(command, cwd=ROOT, check=True, capture_output=True, text=True).stdout


def built(*features):
    return local_crates(cargo_tree("--prefix", "none", *features)) - {TOOLKIT}


def reach_path(crate, *features):
    """The chain from the toolkit to `crate`, read off `cargo tree -i`."""
    output = cargo_tree("-i", crate, "--prefix", "depth", *features)
    chain = {}
    for line in output.splitlines():
        digits = len(line) - len(line.lstrip("0123456789"))
        if not digits:
            continue
        depth = int(line[:digits])
        chain[depth] = line[digits:].split(" ", 1)[0]
        if chain[depth] == TOOLKIT:
            return " <- ".join(chain[level] for level in range(depth + 1))
    return crate


def findings(featureless, defaults, allowed, never, explain=lambda crate, *features: crate):
    problems = []
    for crate in sorted(featureless - allowed):
        problems.append(
            f"{TOOLKIT} with every feature off compiles {crate}, which {POLICY.name} `allowed` "
            f"does not list: {explain(crate, '--no-default-features')}. Put what needs it behind "
            "the `model` feature, or list it if it is a UI or platform crate the toolkit may use."
        )
    for crate in sorted(allowed - featureless):
        problems.append(
            f"{TOOLKIT} no longer compiles {crate} with every feature off: delete it from "
            f"{POLICY.name} `allowed`."
        )
    for crate in sorted(defaults & never):
        problems.append(
            f"{TOOLKIT} with its default features compiles {crate}, which is Tilo's model and "
            f"comes only with the `model` feature: {explain(crate)}."
        )
    return problems


def main():
    parser = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    parser.parse_args()
    policy = tomllib.loads(POLICY.read_text())
    allowed = set(policy["allowed"])
    never = set(policy["never"])
    problems = findings(
        built("--no-default-features"), built(), allowed, never, explain=reach_path
    )
    for problem in problems:
        print(problem, file=sys.stderr)
    if problems:
        return 1
    print(f"{TOOLKIT} compiles {len(allowed)} in-repo crates with every feature off, none of Tilo's model")
    return 0


if __name__ == "__main__":
    sys.exit(main())
