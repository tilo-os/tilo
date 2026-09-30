#!/usr/bin/env python3
"""Every capability an app's code uses is declared in its tilo-app.toml.

``app_capability_usage.toml`` maps code evidence (production dependencies,
feature selections, source paths and method calls) to the capabilities of
``crates/sdk/tilo-caps``. An app that uses one without listing it in
``[capabilities] required`` or ``optional`` fails, with the line to add: on
macOS an undeclared prompted service has no usage string and the system ends
the process on first use, and hosts grant only what the manifest lists.
Unknown capability names fail too. ``tilo-app check`` and ``tilo-app build``
read the same table and additionally refuse a required capability a declared
target cannot provide (the availability table lives in tilo-caps).

``--report`` prints each app's declaration next to what its code uses.
"""

from __future__ import annotations

import argparse
import fnmatch
import re
import sys
import tomllib
from dataclasses import dataclass, field
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import check_app_portability as portability  # noqa: E402

ROOT = portability.ROOT
TABLE = Path("tools/policy/app_capability_usage.toml")
RULES = portability.RULES

# The capability vocabulary (crates/sdk/tilo-caps/src/lib.rs, `capabilities!`).
TILO_CAPS = Path("crates/sdk/tilo-caps/src/lib.rs")
VOCABULARY = re.compile(r'^\s*[A-Z][A-Za-z]*\s*=\s*"([a-z.\-]+)"\s*:', re.M)


@dataclass
class Usage:
    capability: str
    path: str
    line: int
    what: str

    def __str__(self) -> str:
        return f"{self.path}:{self.line}: {self.what}"


@dataclass
class AppCapabilities:
    name: str
    directory: Path
    required: list[str] = field(default_factory=list)
    optional: list[str] = field(default_factory=list)
    uses: dict[str, list[Usage]] = field(default_factory=dict)

    @property
    def declared(self) -> set[str]:
        return {*self.required, *self.optional}

    def undeclared(self) -> dict[str, list[Usage]]:
        return {cap: sites for cap, sites in sorted(self.uses.items()) if cap not in self.declared}


def vocabulary(root: Path) -> set[str]:
    return set(VOCABULARY.findall((root / TILO_CAPS).read_text(encoding="utf-8")))


def group_members(code: str, open_index: int) -> list[str]:
    close = portability.matching(code, open_index)
    body = re.sub(r"\{[^{}]*\}", " ", code[open_index + 1 : close - 1])
    return [part.strip().split("::")[0].split(" as ")[0].strip() for part in body.split(",")]


def path_hits(code: str, pattern: str) -> list[int]:
    """Offsets where ``pattern`` (``a::b``) appears as a path, or as
    ``a::{.., b, ..}``."""
    hits = [match.start() for match in portability.starts(code, portability.path_regex(pattern))]
    segments = pattern.split("::")
    if len(segments) >= 2:
        head = r"\s*::\s*".join(re.escape(segment) for segment in segments[:-1])
        for match in portability.starts(code, re.compile(head + r"\s*::\s*\{")):
            if segments[-1] in group_members(code, match.end() - 1):
                hits.append(match.start())
    return hits


def source_usage(table: dict, source: portability.Source, relative: str) -> list[Usage]:
    found = []
    code = source.code
    for capability, spec in table["uses"].items():
        for pattern in spec.get("paths", []):
            for offset in path_hits(code, pattern):
                line = portability.line_number(code, offset)
                found.append(Usage(capability, relative, line, f"`{pattern}`"))
        for method in spec.get("methods", []):
            for match in re.finditer(rf"\.\s*{re.escape(method)}\s*\(", code):
                line = portability.line_number(code, match.start())
                found.append(Usage(capability, relative, line, f"`.{method}(`"))
    return found


def manifest_usage(table: dict, root: Path, directory: Path) -> list[Usage]:
    path = directory / "Cargo.toml"
    text = path.read_text(encoding="utf-8")
    manifest = tomllib.loads(text)
    workspace_manifest = root / "Cargo.toml"
    workspace = (
        portability.load_manifest(workspace_manifest).get("workspace", {}).get("dependencies", {})
        if workspace_manifest.is_file()
        else {}
    )
    relative = path.relative_to(root).as_posix()
    packages: dict[str, str] = {}
    selections: list[tuple[str, int]] = []
    found = []
    for _, dependencies in portability.dependency_tables(manifest):
        for alias, spec in dependencies.items():
            spec = spec if isinstance(spec, dict) else {"version": spec}
            inherited = workspace.get(alias) if spec.get("workspace") is True else None
            inherited = inherited if isinstance(inherited, dict) else {}
            name = spec.get("package") or inherited.get("package") or alias
            packages[alias] = name
            line = portability.key_line(text, alias)
            for capability, rule in table["uses"].items():
                if portability.matches_any(name, rule.get("crates", [])):
                    found.append(Usage(capability, relative, line, f"depends on `{name}`"))
            for feature in [*spec.get("features", []), *inherited.get("features", [])]:
                selections += [(f"{alias}/{feature}", line), (f"{name}/{feature}", line)]
    for feature, selected in (manifest.get("features", {}) or {}).items():
        line = portability.key_line(text, feature)
        for selection in selected if isinstance(selected, list) else []:
            alias, _, chosen = selection.partition("/")
            alias = alias.removeprefix("dep:").rstrip("?")
            if chosen:
                selections += [(f"{alias}/{chosen}", line),
                               (f"{packages.get(alias, alias)}/{chosen}", line)]
    for selection, line in dict.fromkeys(selections):
        for capability, rule in table["uses"].items():
            if selection in rule.get("features", []):
                found.append(Usage(capability, relative, line, f"selects `{selection}`"))
    return found


def audit(root: Path = ROOT, table_path: Path | None = None) -> list[AppCapabilities]:
    table = tomllib.loads((table_path or root / TABLE).read_text(encoding="utf-8"))
    rules = tomllib.loads((root / RULES).read_text(encoding="utf-8"))
    exempt_dirs = rules.get("scope", {}).get("exempt-dirs", ["tests", "benches", "examples"])
    apps = []
    for directory in sorted((root / "apps").iterdir()) if (root / "apps").is_dir() else []:
        manifest_path = directory / "tilo-app.toml"
        if not (manifest_path.is_file() and (directory / "Cargo.toml").is_file()):
            continue
        declared = portability.load_manifest(manifest_path).get("capabilities", {})
        app = AppCapabilities(
            directory.name,
            directory,
            list(declared.get("required", [])),
            list(declared.get("optional", [])),
        )
        package = portability.Package("apps", directory.name, directory, set())
        usages = manifest_usage(table, root, directory)
        for source in portability.production_sources(package, exempt_dirs, [], root):
            usages += source_usage(table, source, source.path.relative_to(root).as_posix())
        for usage in sorted(usages, key=lambda u: (u.path, u.line)):
            app.uses.setdefault(usage.capability, []).append(usage)
        apps.append(app)
    return apps


def problems(apps: list[AppCapabilities], known: set[str], table: dict) -> list[str]:
    lines = []
    unknown_in_table = sorted(set(table["uses"]) - known)
    for name in unknown_in_table:
        lines.append(f"{TABLE}: `{name}` is not a tilo-caps capability")
    for app in apps:
        manifest = (app.directory / "tilo-app.toml").relative_to(app.directory.parents[1])
        for name in sorted(app.declared - known):
            lines.append(f"{manifest}: `{name}` is not a Tilo capability")
        for capability, sites in app.undeclared().items():
            lines.append(
                f"{manifest}: uses `{capability}` without declaring it ({sites[0]}"
                + (f", and {len(sites) - 1} more" if len(sites) > 1 else "")
                + f")\n    fix: add \"{capability}\" to [capabilities] optional (or required, if "
                "the app cannot work without it)"
            )
    return lines


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    parser.add_argument("--report", action="store_true",
                        help="print each app's declaration and what its code uses")
    args = parser.parse_args(argv)
    table = tomllib.loads((ROOT / TABLE).read_text(encoding="utf-8"))
    apps = audit()
    if args.report:
        for app in apps:
            used = ", ".join(sorted(app.uses)) or "-"
            print(f"{app.name}: required={app.required} optional={app.optional} uses={used}")
        return 0
    found = problems(apps, vocabulary(ROOT), table)
    if found:
        print("app capabilities FAILED: code uses capabilities the manifest does not declare",
              file=sys.stderr)
        for line in found:
            print(f"  {line}", file=sys.stderr)
        return 1
    print(f"app capabilities OK ({len(apps)} app(s); every used capability is declared)")
    return 0


if __name__ == "__main__":
    sys.exit(main())
