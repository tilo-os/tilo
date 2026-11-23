#!/usr/bin/env python3
"""The core and the UI library link only what their groups allow.

docs/architecture/core.md groups the crates by what they are. Four of those
groups promise something about what they link:

  * `crates/platform/` holds the OS capabilities, one API per capability on
    every target. They know nothing about Tilo, so a platform crate depends on
    no workspace crate outside `crates/platform/`.
  * `crates/core/` is the foundation and the device daemon. A core crate
    depends only on `crates/core/` and `crates/platform/`, so everything else
    (the desktop, the OS, VMs, integrations, engines, importers) stays optional
    and the core builds and runs without it.
  * `crates/services/` holds the device services (speaker groups, nearby
    send, casting, clipboard and notification sync) that the daemon binary
    mounts as providers. A service depends only on `crates/services/`,
    `crates/core/` and `crates/platform/`; the core library never links a
    service, which the core rule above already says.
  * `crates/ui/` is the UI library, which knows nothing about Tilo. A UI crate
    depends only on `crates/ui/` and `crates/platform/`.

A dev-dependency on a `tools/` crate (a test fake, the test environment) is
allowed in all four: it is not linked into what ships.

The dependencies that break this today are listed in
tools/policy/crate_layout.toml, each one a dependency to cut. The list only
shrinks: a dependency missing from it fails, and so does a listed one that no
longer exists, so cutting one means deleting its line.
"""

from __future__ import annotations

import argparse
import sys
import tomllib
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
EXCEPTIONS = "tools/policy/crate_layout.toml"

# The groups docs/architecture/core.md's code layout names, and the workspace-hack beside them.
GROUPS = {
    "core", "platform", "ui", "sdk", "lang", "services", "desktop", "os", "vm", "compat",
    "integrations", "engines", "import", "workspace-hack",
}
# Groups the layout no longer names whose crates are still being moved out; each leaves this
# list when it is empty.
LEAVING = {"session", "system", "packaging"}

# What each group may link besides itself.
ALLOWED = {
    "platform": {"platform"},
    "core": {"core", "platform"},
    "services": {"services", "core", "platform"},
    "ui": {"ui", "platform"},
}

KINDS = (("dependencies", "normal"), ("build-dependencies", "build"), ("dev-dependencies", "dev"))


def group(path: Path) -> str:
    """`core` for crates/core/x, `workspace-hack` for crates/workspace-hack, `tools` for tools/x."""
    parts = path.parts
    if parts[0] == "crates":
        return parts[1]
    return parts[0]


def members(root: Path) -> dict[Path, dict]:
    workspace = tomllib.loads((root / "Cargo.toml").read_text())["workspace"]
    found: dict[Path, dict] = {}
    for pattern in workspace["members"]:
        for path in sorted(root.glob(pattern)):
            manifest = path / "Cargo.toml"
            if manifest.is_file():
                found[path.relative_to(root)] = tomllib.loads(manifest.read_text())
    return found


def edges(root: Path = ROOT) -> list[tuple[str, str, str, str, str]]:
    """Every (crate, its group, dependency, the dependency's group, kind) between members."""
    workspace = tomllib.loads((root / "Cargo.toml").read_text())["workspace"]
    shared = {
        name: Path(spec["path"])
        for name, spec in workspace.get("dependencies", {}).items()
        if isinstance(spec, dict) and "path" in spec
    }
    crates = members(root)
    name_of = {path: manifest["package"]["name"] for path, manifest in crates.items()}
    out = []
    for path, manifest in sorted(crates.items()):
        tables = [manifest]
        tables += list(manifest.get("target", {}).values())
        for table in tables:
            for key, kind in KINDS:
                for name, spec in (table.get(key) or {}).items():
                    if not isinstance(spec, dict):
                        continue
                    if "path" in spec:
                        target = Path(*(path / spec["path"]).parts)
                        target = Path((root / target).resolve().relative_to(root.resolve()))
                    elif spec.get("workspace") and spec.get("package", name) in shared:
                        target = shared[spec.get("package", name)]
                    else:
                        continue
                    if target not in name_of:
                        continue
                    out.append((name_of[path], group(path), name_of[target], group(target), kind))
    return sorted(set(out))


def violations(found: list[tuple[str, str, str, str, str]]) -> set[str]:
    bad = set()
    for crate, home, dependency, there, kind in found:
        if home not in ALLOWED or crate == dependency:
            continue
        if kind == "dev" and there == "tools":
            continue
        if there not in ALLOWED[home]:
            bad.add(f"{crate} -> {dependency}")
    return bad


def listed(root: Path = ROOT) -> dict[str, set[str]]:
    document = tomllib.loads((root / EXCEPTIONS).read_text())
    return {key: set(document.get(key, [])) for key in ALLOWED}


def misplaced(root: Path = ROOT) -> list[str]:
    """Member crates under crates/ in a group the layout does not name."""
    return [
        f"{path}: crates/{group(path)}/ is not a group of docs/architecture/core.md's code layout;\n"
        f"  move the crate to the group that says what it is"
        for path in members(root)
        if path.parts[0] == "crates" and group(path) not in GROUPS | LEAVING
    ]


def check(root: Path = ROOT) -> list[str]:
    found = edges(root)
    home = {crate: g for crate, g, *_ in found}
    bad = violations(found)
    allowed = listed(root)
    problems = misplaced(root)
    for edge in sorted(bad):
        crate = edge.split(" -> ")[0]
        if edge not in allowed[home[crate]]:
            reach = " and ".join(f"crates/{g}/" for g in sorted(ALLOWED[home[crate]]))
            problems.append(
                f"{edge}: a crates/{home[crate]}/ crate depends on a crate outside {reach}.\n"
                f"  why: docs/architecture/core.md keeps the platform crates and the UI library free of\n"
                f"  Tilo and the core free of everything optional; the list in {EXCEPTIONS} only shrinks.\n"
                f"  fix: depend the other way round, move the shared part into an allowed group, or move\n"
                f"  the crate to the group it belongs in."
            )
    for key, entries in allowed.items():
        for edge in sorted(entries - bad):
            problems.append(f"{EXCEPTIONS}: `{edge}` under [{key}] no longer exists: delete the line")
    return problems


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--list", action="store_true",
                        help="print every dependency that breaks the layout, listed or not")
    args = parser.parse_args(argv)
    if args.list:
        found = edges()
        home = {crate: g for crate, g, *_ in found}
        for edge in sorted(violations(found)):
            print(f"[{home[edge.split(' -> ')[0]]}] {edge}")
        return 0
    problems = check()
    for problem in problems:
        print(problem)
    if problems:
        print(f"crate layout: {len(problems)} problem(s)")
        return 1
    allowed = listed()
    left = ", ".join(f"{len(allowed[key])} {key}" for key in ALLOWED)
    print(f"crate layout OK (platform, core, services and ui link what they may; dependencies left to cut: {left})")
    return 0


if __name__ == "__main__":
    sys.exit(main())
