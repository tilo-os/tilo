#!/usr/bin/env python3
"""Which packages' tests the ci profile's costly setup steps run for.

    setup_filters.py            # each step's filter, as .config/nextest.toml carries it
    setup_filters.py --check    # exit 1, saying what differs, when it carries another

A setup script runs once per nextest run that selects one of its filter's tests, and a suite is
well over a thousand runs (a package's test binary, or one shard of it). Two of the ci profile's
steps take work: `font-catalog` copies and hashes the 54 MB pinned font catalog, `headless-audio`
starts a PulseAudio daemon. Run for every test (`all()`), a third of the suite's jobs paid for
both and could use neither. So each runs for the packages whose code can use what it prepares,
and nextest works the set out itself, from the build's own dependency graph (`rdeps(NAME)`: NAME
and every package that depends on it, through any kind of dependency), so a package that starts
depending on one of them is served without an edit here.

What an edit here is for is the roots, and they are read from the code:

- the font catalog has one reader, the crate that owns its manifest and checks
  (`tilo-font-catalog`): a process sets text in the catalog only through it. A package outside its
  dependents that names the catalog's variables is a carrier of them (CARRIERS says why each is
  none of the catalog's readers), and any other fails the check;
- the audio endpoint's clients are the packages whose Rust sources name one (ENDPOINT): in
  library code (`src/`, `build.rs`), the package and whatever depends on it; in a package's own
  tests alone, or as a test's statement that it requires audio (REQUIRED), that package.

`tools/test-env-setup.sh runtime`, which every test gets, promises both (TILO_REQUIRE_FONTS,
TILO_REQUIRE_AUDIO), so a test that sets text or requires audio in a package a step leaves out
fails and never skips; tools/tests/test_setup_filters.py fails before that, while the filters in
.config/nextest.toml are not the ones the code gives.
"""

import argparse
import json
import os
import re
import subprocess
import sys
import tomllib
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
NEXTEST = ROOT / ".config" / "nextest.toml"

# The crate every reader of the font catalog goes through.
FONT_OWNER = "tilo-font-catalog"
# A name is a literal a source holds and, where the literal alone would say too much ("compactly"
# holds PulseAudio's `pactl`), the pattern that confirms it. The literal is looked for first: it
# is what makes reading eleven thousand sources a second's work and not ten.
Names = list[tuple[str, re.Pattern | None]]


def whole(word: str) -> tuple[str, re.Pattern]:
    return word, re.compile(rf"\b{re.escape(word)}\b")


# What names the catalog: where it is prepared, the promise of it, and its owner's API.
CATALOG: Names = [
    ("TILO_FONT_ASSET_CACHE", None), ("TILO_FONT_CATALOG", None), ("TILO_REQUIRE_FONTS", None),
    ("tilo_font_catalog", None),
]
# Packages outside the owner's dependents that name the catalog without reading it.
CARRIERS = {
    "tilo-process": "names the variables of the processes it describes (HostPaths, "
                    "RunnerDiagnostics) and reads no font",
    "tilo-test-env": "hands the variables it was given on to a program a test launches "
                     "(launch_env)",
}

# What names the private audio endpoint (tools/with-headless-audio.sh: a PulseAudio daemon) or a
# client of one: GStreamer's automatic and Pulse audio elements, PulseAudio's library, tools and
# environment, espeak's audio library, and the endpoint's own variable and wrapper.
ENDPOINT: Names = [
    ("autoaudiosink", None), ("autoaudiosrc", None), ("pulsesink", None), ("pulsesrc", None),
    ("libpulse", None), whole("pactl"), whole("pacat"), whole("paplay"), whole("parec"),
    ("pcaudiolib", None),
    ("PULSE_SERVER", None), ("PULSE_RUNTIME_PATH", None), ("PULSE_STATE_PATH", None),
    ("PULSE_COOKIE", None), ("PULSE_SINK", None), ("PULSE_SOURCE", None),
    ("TILO_HEADLESS_AUDIO_PID", None), ("with-headless-audio", None),
]
# A test's own statement that it needs audio: wherever it is written, it is its package's.
REQUIRED: Names = [
    ("require_capability!", re.compile(r"require_capability!\s*\(\s*Audio\b")),
    whole("Capability::Audio"),
]

# Directories that hold no package's sources.
PRUNED = {"target", "vendor", "node_modules", ".git"}
# Where a package's library code is: what its dependents link.
LIBRARY = {"src", "build.rs"}


def workspace() -> dict[str, dict]:
    """Every workspace package: its directory and the workspace packages it depends on, through
    any kind of dependency (as nextest's `rdeps` follows them)."""
    metadata = subprocess.run(
        ["cargo", "metadata", "--format-version", "1", "--no-deps", "--offline", "--locked"],
        cwd=ROOT, check=True, capture_output=True, text=True).stdout
    packages = json.loads(metadata)["packages"]
    names = {package["name"] for package in packages}
    return {
        package["name"]: {
            "directory": Path(package["manifest_path"]).parent,
            "dependencies": {d["name"] for d in package["dependencies"]} & names,
        }
        for package in packages
    }


def rdeps(packages: dict[str, dict], roots: set[str]) -> set[str]:
    """The roots and every package that depends on one of them."""
    dependents: dict[str, set[str]] = {}
    for name, package in packages.items():
        for dependency in package["dependencies"]:
            dependents.setdefault(dependency, set()).add(name)
    found, queue = set(roots), list(roots)
    while queue:
        for dependent in dependents.get(queue.pop(), ()):
            if dependent not in found:
                found.add(dependent)
                queue.append(dependent)
    return found


def names(text: str, wanted: Names) -> bool:
    """Whether `text` holds one of the names."""
    return any(
        literal in text and (confirmed is None or confirmed.search(text))
        for literal, confirmed in wanted
    )


def evidence(packages: dict[str, dict]) -> list[tuple[str, bool, frozenset[str]]]:
    """What every package's Rust sources name, a file at a time: (package, whether the file is
    library code, which of "endpoint", "required" and "catalog" it names). A file is its nearest
    package's: one package's directory may hold another's."""
    marks = {"endpoint": ENDPOINT, "required": REQUIRED, "catalog": CATALOG}
    found = []
    directories = {package["directory"] for package in packages.values()}
    for name, package in sorted(packages.items()):
        top = package["directory"]
        for directory, below, files in os.walk(top):
            here = Path(directory)
            below[:] = sorted(
                entry for entry in below
                if entry not in PRUNED and here / entry not in directories
            )
            for file in sorted(files):
                if file.endswith(".rs"):
                    path = here / file
                    library = path.relative_to(top).parts[0] in LIBRARY
                    text = path.read_text(encoding="utf-8", errors="replace")
                    named = frozenset(mark for mark, wanted in marks.items() if names(text, wanted))
                    found.append((name, library, named))
    return found


def audio_clients(found) -> tuple[set[str], set[str]]:
    """From `evidence`: the packages whose library code names an audio endpoint, which their
    dependents reach through them, and the packages only whose own tests need one."""
    libraries: set[str] = set()
    alone: set[str] = set()
    for name, library, named in found:
        if "endpoint" in named:
            (libraries if library else alone).add(name)
        if "required" in named:
            alone.add(name)
    return libraries, alone - libraries


def audio_filter(libraries: set[str], alone: set[str]) -> str:
    return " or ".join(
        [f"rdeps({name})" for name in sorted(libraries)]
        + [f"package({name})" for name in sorted(alone)]
    )


def catalog_namers(found) -> set[str]:
    """From `evidence`: the packages whose sources name the font catalog."""
    return {name for name, _, named in found if "catalog" in named}


def selected(packages: dict[str, dict], filter_: str) -> set[str]:
    """The packages a filter of `rdeps(NAME)` and `package(NAME)` terms selects."""
    terms = re.findall(r"\b(rdeps|package)\(\s*([^)\s]+)\s*\)", filter_)
    if " or ".join(f"{kind}({name})" for kind, name in terms) != " ".join(filter_.split()):
        raise ValueError(f"not a list of rdeps() and package() terms: {filter_}")
    unknown = sorted({name for _, name in terms} - set(packages))
    if unknown:
        raise ValueError(f"no workspace package is named {', '.join(unknown)}: {filter_}")
    return rdeps(packages, {name for kind, name in terms if kind == "rdeps"}) | {
        name for kind, name in terms if kind == "package"
    }


def filters(found) -> dict[str, str]:
    """From `evidence`: the filter of each costly setup script, by the script's name in
    .config/nextest.toml."""
    return {
        "font-catalog": f"rdeps({FONT_OWNER})",
        "headless-audio": audio_filter(*audio_clients(found)),
    }


def configured() -> dict[str, list[str]]:
    """The filters the ci profile runs each setup script for."""
    config = tomllib.loads(NEXTEST.read_text(encoding="utf-8"))
    found: dict[str, list[str]] = {}
    for rule in config["profile"]["ci"].get("scripts", []):
        setups = rule["setup"] if isinstance(rule["setup"], list) else [rule["setup"]]
        for setup in setups:
            found.setdefault(setup, []).append(rule["filter"])
    return found


def problems(packages: dict[str, dict], named) -> list[str]:
    """What is wrong with the filters in .config/nextest.toml, or with what this file allows:
    `named` is the packages' `evidence`."""
    found = []
    rules = configured()
    for setup, wanted in filters(named).items():
        if rules.get(setup) != [wanted]:
            found.append(
                f"{NEXTEST.relative_to(ROOT)}: the ci profile runs `{setup}` for "
                f"{rules.get(setup, 'no test')}; the code gives\n    filter = \"{wanted}\""
            )
    if FONT_OWNER not in packages:
        return [*found, f"no workspace package is named {FONT_OWNER}, the font catalog's reader"]
    outside = catalog_namers(named) - rdeps(packages, {FONT_OWNER})
    for name in sorted(outside - set(CARRIERS)):
        found.append(
            f"{name} names the font catalog and does not depend on {FONT_OWNER}: its tests get "
            f"no catalog. Read the catalog through {FONT_OWNER}, or say in CARRIERS "
            f"({Path(__file__).relative_to(ROOT)}) why it reads none"
        )
    for name in sorted(set(CARRIERS) - outside):
        found.append(
            f"CARRIERS excuses {name}, which no longer names the font catalog from outside "
            f"{FONT_OWNER}'s dependents: drop it"
        )
    return found


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    parser.add_argument("--check", action="store_true")
    args = parser.parse_args()
    packages = workspace()
    named = evidence(packages)
    if args.check:
        found = problems(packages, named)
        for problem in found:
            print(problem, file=sys.stderr)
        sys.exit(1 if found else 0)
    for setup, wanted in filters(named).items():
        print(f"{setup}: {wanted}")


if __name__ == "__main__":
    main()
