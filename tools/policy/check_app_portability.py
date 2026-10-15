#!/usr/bin/env python3
"""Keep platform code out of Tilo apps and the SDK's portable crates.

Apps run on Linux, macOS and the web from one source. The rules in
``app_portability_rules.toml`` name the constructs that tie code to one
platform (target ``cfg``, ``std::os``, platform paths, OS tools, native and
internal dependencies, SDK backend features, and the ``std`` calls that
compile for the web but fail there). ``tilo-app check --lint`` reads the same
table and reports each site to the app developer; this compile-free gate
counts the sites per package and rule.

Scope:

* every package directly under ``apps/`` with a ``tilo-app.toml`` (its
  ``package.platforms`` decides which rules apply), plus ``apps/*-model``
  crates, which get the table's ``model-rules``;
* the SDK's portable crates (``[sdk]`` in the table), where target ``cfg``
  and the web run-time traps are counted.

Only production code counts: ``tests/``, ``benches/``, ``examples/``,
``build.rs``, test files, ``#[test]`` and ``#[cfg(test)]`` items are exempt.

The counts may only fall. ``app_portability_baseline.toml`` records them
keyed package → rule → count; a count above its entry fails, and so does a
package or rule without one (new apps must be clean). When counts fall the
gate prints the tightened baseline to commit, so the improvement is locked
in. ``--findings NAME`` lists the sites behind a package's counts.
"""

from __future__ import annotations

import argparse
import bisect
import fnmatch
import re
import sys
import tomllib
from collections import defaultdict
from dataclasses import dataclass, field
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
from rust_source import Literal, is_test_file, lex, line_number, matching  # noqa: E402


ROOT = Path(__file__).resolve().parents[2]
RULES = Path("tools/policy/app_portability_rules.toml")
BASELINE = Path("tools/policy/app_portability_baseline.toml")
DEFAULT_PLATFORMS = ("linux", "macos", "web")
SECTIONS = ("apps", "sdk")


@dataclass(frozen=True)
class Finding:
    section: str
    package: str
    rule: str
    path: str
    line: int
    what: str
    # A `[strict]` rule for this package: no baseline allowance.
    strict: bool = False

    def __str__(self) -> str:
        return f"{self.path}:{self.line}: {self.rule} {self.what}"


# --- Rust source ------------------------------------------------------------


def split_top_level(text: str) -> list[str]:
    parts, depth, current = [], 0, []
    for char in text:
        if char in "([{":
            depth += 1
        elif char in ")]}":
            depth -= 1
        if char == "," and depth == 0:
            parts.append("".join(current))
            current = []
        else:
            current.append(char)
    parts.append("".join(current))
    return [part.strip() for part in parts if part.strip()]


def requires_test(predicate: str) -> bool:
    """Whether a ``cfg`` predicate can only hold in a test build."""
    predicate = re.sub(r"\s+", "", predicate)
    if predicate == "test":
        return True
    match = re.fullmatch(r"(all|any)\((.*)\)", predicate)
    if not match:
        return False
    arguments = split_top_level(match.group(2))
    if match.group(1) == "all":
        return any(requires_test(argument) for argument in arguments)
    return bool(arguments) and all(requires_test(argument) for argument in arguments)


OUTER_ATTRIBUTE = re.compile(r"#\s*\[")
INNER_TEST_CFG = re.compile(r"#\s*!\s*\[\s*cfg\s*\(")
TEST_ATTRIBUTE = re.compile(r"(?:[A-Za-z_][\w]*\s*::\s*)*test")
MOD_DECLARATION = re.compile(r"(?:pub(?:\s*\([^)]*\))?\s+)?mod\s+([A-Za-z_]\w*)\s*;")
PATH_ATTRIBUTE = re.compile(r'#\s*\[\s*path\s*=\s*"([^"]*)"\s*\]')


def attribute_is_test(body: str) -> bool:
    body = body.strip()
    if TEST_ATTRIBUTE.fullmatch(body):
        return True
    match = re.fullmatch(r"cfg\s*\((.*)\)", body, re.DOTALL)
    return bool(match) and requires_test(match.group(1))


ITEM_KEYWORD = re.compile(
    r"(?:pub(?:\s*\([^)]*\))?\s+)?(?:(?:unsafe|async|default)\s+)*"
    r"(?:fn|mod|impl|struct|enum|union|trait|const|static|use|type|macro_rules|let|extern)\b"
)


def item_extent(code: str, start: int) -> tuple[int, int]:
    """Start and end of what follows the attributes at ``start``.

    An item or statement ends at its `;` or closing brace. A field, variant,
    match arm or argument ends at the `,` or bracket that closes it.
    """
    i = start
    while True:
        while i < len(code) and code[i].isspace():
            i += 1
        attribute = OUTER_ATTRIBUTE.match(code, i)
        if not attribute:
            break
        i = matching(code, attribute.end() - 1)
    item_start = i
    item = ITEM_KEYWORD.match(code, i) is not None
    depth = 0
    while i < len(code):
        char = code[i]
        if char in "([" or (char == "{" and depth > 0):
            depth += 1
        elif char in ")]}":
            if depth == 0:
                return item_start, i
            depth -= 1
        elif depth == 0 and char == ";":
            return item_start, i + 1
        elif depth == 0 and char == "{":
            return item_start, matching(code, i)
        elif depth == 0 and char == "," and not item:
            return item_start, i + 1
        i += 1
    return item_start, len(code)


@dataclass
class Pruned:
    code: str
    removed: list[tuple[int, int]]
    # (module, `#[path]` value) of each out-of-line test module.
    test_modules: list[tuple[str, str | None]]
    test_only: bool

    def keeps(self, offset: int) -> bool:
        index = bisect.bisect_right(self.removed, (offset, len(self.code) + 1)) - 1
        return index < 0 or not self.removed[index][0] <= offset < self.removed[index][1]


def strip_tests(text: str, code: str) -> Pruned:
    """Blank ``#[test]`` and ``#[cfg(test)]`` items in lexed ``code``."""
    inner = INNER_TEST_CFG.search(code)
    if inner:
        close = matching(code, inner.end() - 1)
        if requires_test(code[inner.end() : close - 1]):
            return Pruned("", [(0, len(code))], [], True)
    out = list(code)
    removed: list[tuple[int, int]] = []
    modules: list[tuple[str, str | None]] = []
    position = 0
    while attribute := OUTER_ATTRIBUTE.search(code, position):
        # The run of attributes on one item: any test attribute gates it all.
        first_close = matching(code, attribute.end() - 1)
        run_end, is_test = attribute.start(), False
        while run := OUTER_ATTRIBUTE.match(code, run_end):
            close = matching(code, run.end() - 1)
            is_test = is_test or attribute_is_test(code[run.end() : close - 1])
            run_end = close
            while run_end < len(code) and code[run_end].isspace():
                run_end += 1
        if not is_test:
            position = first_close
            continue
        item_start, end = item_extent(code, attribute.start())
        declaration = MOD_DECLARATION.match(code, item_start, end)
        if declaration and declaration.end() == end:
            path = PATH_ATTRIBUTE.search(text, attribute.start(), item_start)
            modules.append((declaration.group(1), path.group(1) if path else None))
        for index in range(attribute.start(), end):
            if out[index] != "\n":
                out[index] = " "
        removed.append((attribute.start(), end))
        position = end
    return Pruned("".join(out), removed, modules, False)


def path_regex(path: str, suffix: str = "") -> re.Pattern[str]:
    """``a::b`` with any whitespace; callers check what precedes a match."""
    segments = [re.escape(segment) for segment in path.split("::")]
    return re.compile(r"\s*::\s*".join(segments) + r"\b" + suffix)


def starts(code: str, pattern: re.Pattern[str], forbidden: str = "."):
    """Matches of ``pattern`` that begin an identifier and do not follow
    ``forbidden`` (a leading lookbehind would disable the regex prefix scan)."""
    for match in pattern.finditer(code):
        before = code[match.start() - 1] if match.start() else " "
        if not (before.isalnum() or before == "_" or before in forbidden):
            yield match


STD_GROUP = re.compile(r"std\s*::\s*\{")
CFG = re.compile(r"(cfg_attr|cfg)\s*(?:!\s*)?\(")
EXTERN_BLOCK = re.compile(r"extern\s*\{")


def std_module_hits(code: str, modules: list[str]) -> list[tuple[int, str]]:
    hits = []
    names = "|".join(re.escape(module) for module in modules)
    for match in starts(code, re.compile(rf"std\s*::\s*({names})\b"), ".:"):
        hits.append((match.start(), f"`std::{match.group(1)}`"))
    for match in starts(code, STD_GROUP, ".:"):
        close = matching(code, match.end() - 1)
        group = code[match.end() : close - 1]
        tokens = re.findall(r"::|\w+|\S", re.sub(r"\{[^{}]*\}", " ", group))
        for index, token in enumerate(tokens):
            if token in modules and (index == 0 or tokens[index - 1] not in ("::", ".")):
                hits.append((match.start(), f"`std::{token}`"))
                break
    return hits


@dataclass(frozen=True)
class Source:
    path: Path
    # Production code: comments, literals and test items blanked.
    code: str
    # String literals outside test items.
    literals: list[Literal]


def parse_source(path: Path, text: str) -> tuple[Source, list[tuple[str, str | None]], bool]:
    code, literals = lex(text)
    pruned = strip_tests(text, code)
    live = [literal for literal in literals if pruned.keeps(literal.start)]
    return Source(path, pruned.code, live), pruned.test_modules, pruned.test_only


class SourceScanner:
    """Finds rule sites in one production source file."""

    def __init__(self, rules: dict, active: set[str], native_aliases: set[str]):
        self.rules = rules
        self.active = active
        r1 = rules.get("R1", {})
        predicates = "|".join(re.escape(p) for p in r1.get("predicates", []))
        self.r1_predicate = re.compile(rf"\b(?:{predicates})\b") if predicates else None
        self.r1_exempt = r1.get("exempt-cfg-attr", [])
        self.path_patterns = {
            rule: [(path, path_regex(path)) for path in rules[rule].get("paths", [])]
            for rule in rules
        }
        self.native = sorted(alias.replace("-", "_") for alias in native_aliases)

    def scan(self, source: Source) -> list[tuple[str, int, str]]:
        code, live = source.code, source.literals
        hits: list[tuple[str, int, str]] = []

        def add(rule: str, offset: int, what: str) -> None:
            if rule in self.active:
                hits.append((rule, line_number(code, offset), what))

        if "R1" in self.active and self.r1_predicate:
            for match in starts(code, CFG, ".:"):
                close = matching(code, match.end() - 1)
                predicate = code[match.end() : close - 1]
                if not self.r1_predicate.search(predicate):
                    continue
                if match.group(1) == "cfg_attr" and any(
                    re.search(rf"\b{re.escape(name)}\b", predicate) for name in self.r1_exempt
                ):
                    continue
                what = f"`{match.group(1)}({compact(predicate)})` selects code by target"
                add("R1", match.start(), what)
        for rule, spec in self.rules.items():
            if rule not in self.active:
                continue
            modules = spec.get("std-modules", [])
            if modules:
                for offset, what in std_module_hits(code, modules):
                    add(rule, offset, what)
            for path, pattern in self.path_patterns.get(rule, []):
                for match in starts(code, pattern):
                    add(rule, match.start(), f"`{path}`")
        if "R2" in self.active:
            if self.rules["R2"].get("extern-blocks"):
                for match in starts(code, EXTERN_BLOCK, ".:"):
                    add("R2", match.start(), "`extern` block")
            for alias in self.native:
                for match in starts(code, re.compile(rf"{re.escape(alias)}\s*::"), ".:"):
                    add("R2", match.start(), f"`{alias}::` native binding crate")
        strings = [literal for literal in live if not literal.byte]
        string_starts = [literal.start for literal in strings]

        def argument(offset: int) -> Literal | None:
            """The string literal that is the first argument after ``offset``."""
            index = bisect.bisect_left(string_starts, offset)
            if index < len(strings) and not code[offset : strings[index].start].strip():
                return strings[index]
            return None

        if "R3" in self.active:
            spec = self.rules["R3"]
            prefixes = tuple(spec.get("literal-prefixes", []))
            contains = spec.get("literal-contains", [])
            exempt = tuple(spec.get("exempt-literal-prefixes", []))
            for literal in strings:
                value = literal.value
                if exempt and value.startswith(exempt):
                    continue
                if value.startswith(prefixes) or any(part in value for part in contains):
                    add("R3", literal.start, f"platform path {literal.value[:60]!r}")
            names = set(spec.get("env-names", []))
            env_prefixes = tuple(spec.get("env-prefixes", []))
            for function in spec.get("env-functions", []):
                for match in starts(code, path_regex(function, r"\s*\(")):
                    literal = argument(match.end())
                    if literal and (
                        literal.value in names or literal.value.startswith(env_prefixes)
                    ):
                        add("R3", match.start(), f"`{function}({literal.value!r})`")
        if "R4" in self.active:
            for constructor in self.rules["R4"].get("constructors", []):
                for match in starts(code, path_regex(constructor, r"\s*\(")):
                    literal = argument(match.end())
                    if literal:
                        add("R4", match.start(), f"`{constructor}({literal.value!r})`")
        return hits


def compact(text: str) -> str:
    text = re.sub(r"\s+", " ", text).strip()
    return text if len(text) <= 60 else text[:60] + "…"


# --- Packages ---------------------------------------------------------------


def matches_any(value: str, patterns: list[str]) -> bool:
    return any(fnmatch.fnmatchcase(value, pattern) for pattern in patterns)


def dependency_tables(manifest: dict) -> list[tuple[str, dict]]:
    """Dependency tables that reach shipped code (not dev-dependencies)."""
    tables = []
    for key in ("dependencies", "build-dependencies"):
        if isinstance(manifest.get(key), dict):
            tables.append((key, manifest[key]))
    targets = manifest.get("target", {})
    if isinstance(targets, dict):
        for target, value in targets.items():
            if not isinstance(value, dict):
                continue
            for key in ("dependencies", "build-dependencies"):
                if isinstance(value.get(key), dict):
                    tables.append((f"target.{target}.{key}", value[key]))
    return tables


def key_line(text: str, key: str) -> int:
    for index, line in enumerate(text.splitlines(), start=1):
        stripped = line.strip()
        if re.match(rf"^{re.escape(key)}\s*[=.]", stripped) or re.match(
            rf'^"{re.escape(key)}"\s*[=.]', stripped
        ):
            return index
    for index, line in enumerate(text.splitlines(), start=1):
        if key in line:
            return index
    return 1


@dataclass
class Package:
    section: str
    name: str
    directory: Path
    # The rules that apply, given the package's platforms.
    rules: set[str]
    # Rules with no baseline allowance (``[strict]``, for apps on every platform).
    strict: set[str] = field(default_factory=set)


def load_manifest(path: Path) -> dict:
    with path.open("rb") as handle:
        return tomllib.load(handle)


def active_rules(rules: dict, candidates: list[str], platforms: tuple[str, ...]) -> set[str]:
    active = set()
    for rule in candidates:
        applies = rules[rule].get("applies", "always")
        if applies == "not-tilo-os" and "tilo-os" in platforms:
            continue
        if applies == "web" and "web" not in platforms:
            continue
        active.add(rule)
    return active


def discover(root: Path, table: dict) -> list[Package]:
    rules = table["rules"]
    packages: list[Package] = []
    apps = root / "apps"
    model_pattern = table.get("scope", {}).get("model-crates", "apps/*-model")
    model_rules = table.get("scope", {}).get("model-rules", [])
    strict_spec = table.get("strict", {})
    if apps.is_dir():
        for directory in sorted(apps.iterdir()):
            if not (directory / "Cargo.toml").is_file():
                continue
            relative = directory.relative_to(root).as_posix()
            manifest_path = directory / "tilo-app.toml"
            strict: set[str] = set()
            if manifest_path.is_file():
                package = load_manifest(manifest_path).get("package", {})
                platforms = tuple(package.get("platforms", DEFAULT_PLATFORMS))
                candidates = list(rules)
                if set(strict_spec.get("platforms", [])) <= set(platforms):
                    strict = set(strict_spec.get("rules", []))
            elif fnmatch.fnmatchcase(relative, model_pattern):
                platforms = DEFAULT_PLATFORMS
                candidates = [rule for rule in model_rules if rule in rules]
            else:
                continue
            active = active_rules(rules, candidates, platforms)
            packages.append(Package("apps", directory.name, directory, active, active & strict))
    sdk = table.get("sdk", {})
    for crate in sdk.get("crates", []):
        directory = root / crate
        if not (directory / "Cargo.toml").is_file():
            continue
        package = load_manifest(directory / "Cargo.toml").get("package", {})
        name = package.get("name", directory.name)
        sdk_rules = {rule for rule in sdk.get("rules", []) if rule in rules}
        packages.append(Package("sdk", name, directory, sdk_rules))
    return packages


def module_children(file: Path) -> Path:
    if file.name in ("lib.rs", "main.rs", "mod.rs") or file.parent.name == "bin":
        return file.parent
    return file.parent / file.stem


def production_sources(
    package: Package, exempt_dirs: list[str], allowed: list[str], root: Path
) -> list[Source]:
    directory = package.directory
    nested = {
        path.parent for path in directory.rglob("Cargo.toml") if path.parent != directory
    }
    candidates = []
    for path in sorted(directory.rglob("*.rs")):
        relative = path.relative_to(directory)
        if relative.parts[0] in (*exempt_dirs, "target") or relative == Path("build.rs"):
            continue
        if any(parent in nested for parent in path.parents):
            continue
        if is_test_file(relative) or matches_any(path.relative_to(root).as_posix(), allowed):
            continue
        candidates.append(path)
    parsed = []
    excluded: set[Path] = set()
    for path in candidates:
        source, test_modules, test_only = parse_source(
            path, path.read_text(encoding="utf-8", errors="replace")
        )
        if not test_only:
            parsed.append(source)
        for module, path_attribute in test_modules:
            if path_attribute:
                excluded.add((path.parent / path_attribute).resolve())
                continue
            children = module_children(path)
            excluded.add((children / f"{module}.rs").resolve())
            excluded.add((children / module).resolve())
    return [
        source
        for source in parsed
        if not any(
            item == source.path.resolve() or item in source.path.resolve().parents
            for item in excluded
        )
    ]


def header_line(text: str, target: str) -> int:
    for index, line in enumerate(text.splitlines(), start=1):
        if line.lstrip().startswith("[") and target in line:
            return index
    return 1


def dependency_path(package: Package, root: Path, spec: dict, inherited: dict) -> str | None:
    """Repository-relative directory of a path dependency."""
    if isinstance(spec.get("path"), str):
        resolved = (package.directory / spec["path"]).resolve()
    elif isinstance(inherited.get("path"), str):
        resolved = (root / inherited["path"]).resolve()
    else:
        return None
    try:
        return resolved.relative_to(root.resolve()).as_posix()
    except ValueError:
        return resolved.as_posix()


def manifest_findings(
    package: Package, root: Path, rules: dict, allowed: list[str]
) -> tuple[list[tuple[str, int, str]], set[str]]:
    """Findings in the package's Cargo.toml, and its native-crate aliases."""
    path = package.directory / "Cargo.toml"
    text = path.read_text(encoding="utf-8")
    manifest = tomllib.loads(text)
    workspace_manifest = root / "Cargo.toml"
    workspace = (
        load_manifest(workspace_manifest).get("workspace", {}).get("dependencies", {})
        if workspace_manifest.is_file()
        else {}
    )
    active = package.rules
    hits: list[tuple[str, int, str]] = []
    native_aliases: set[str] = set()

    r1 = rules.get("R1", {})
    if (
        "R1" in active
        and r1.get("cargo-target-sections")
        and not matches_any(path.relative_to(root).as_posix(), allowed)
    ):
        predicates = r1.get("predicates", [])
        build_only = set(r1.get("build-only-target-dependencies", []))
        for target, tables in (manifest.get("target", {}) or {}).items():
            if target.startswith("cfg(") and not any(
                re.search(rf"\b{re.escape(p)}\b", target) for p in predicates
            ):
                continue
            named = {
                alias
                for kind in ("dependencies", "build-dependencies")
                for alias in (tables or {}).get(kind, {})
            }
            # Target-specific dev-dependencies only reach tests, which are exempt.
            if not named and (tables or {}).get("dev-dependencies"):
                continue
            if named and named <= build_only:
                continue
            hits.append(("R1", header_line(text, target), f"`[target.{target}]` section"))

    d1, d2, f1 = rules.get("D1", {}), rules.get("D2", {}), rules.get("F1", {})
    backend_features = f1.get("features", [])
    sdk_aliases: set[str] = set()
    for _, dependencies in dependency_tables(manifest):
        for alias, spec in dependencies.items():
            spec = spec if isinstance(spec, dict) else {"version": spec}
            inherited = workspace.get(alias) if spec.get("workspace") is True else None
            inherited = inherited if isinstance(inherited, dict) else {}
            name = spec.get("package") or inherited.get("package") or alias
            line = key_line(text, alias)
            if name in rules.get("R2", {}).get("native-crates", []):
                native_aliases.add(alias)
            if "D1" in active and matches_any(name, d1.get("crates", [])):
                hits.append(("D1", line, f"depends on `{name}`"))
            where = dependency_path(package, root, spec, inherited)
            if (
                "D2" in active
                and where is not None
                and name not in d2.get("allowed-packages", [])
                and not matches_any(where, d2.get("allowed-paths", []))
            ):
                hits.append(("D2", line, f"depends on the Tilo crate `{name}` ({where})"))
            if name in f1.get("sdk-packages", []):
                sdk_aliases.add(alias)
                features = [*spec.get("features", []), *inherited.get("features", [])]
                for feature in features:
                    if "F1" in active and feature in backend_features:
                        hits.append(("F1", line, f"selects `{alias}/{feature}`"))
    if "F1" in active:
        for feature, selections in (manifest.get("features", {}) or {}).items():
            line = key_line(text, feature)
            if feature in f1.get("app-features", []):
                hits.append(("F1", line, f"declares the backend feature `{feature}`"))
            for selection in selections if isinstance(selections, list) else []:
                alias, _, selected = selection.partition("/")
                if alias.rstrip("?") in sdk_aliases and selected in backend_features:
                    hits.append(("F1", line, f"selects `{selection}`"))
    return hits, native_aliases


def audit(root: Path = ROOT, rules_path: Path | None = None) -> list[Finding]:
    table = tomllib.loads((rules_path or root / RULES).read_text(encoding="utf-8"))
    rules = table["rules"]
    exempt_dirs = table.get("scope", {}).get("exempt-dirs", ["tests", "benches", "examples"])
    allowed = table.get("target-cfg", {}).get("allowed", [])
    findings: list[Finding] = []
    for package in discover(root, table):
        hits, native_aliases = manifest_findings(package, root, rules, allowed)
        manifest = (package.directory / "Cargo.toml").relative_to(root).as_posix()
        located = [(manifest, *hit) for hit in hits]
        scanner = SourceScanner(rules, package.rules, native_aliases)
        for source in production_sources(package, exempt_dirs, allowed, root):
            relative = source.path.relative_to(root).as_posix()
            located += [(relative, *hit) for hit in scanner.scan(source)]
        seen = set()
        for path, rule, line, what in sorted(located):
            if (path, line, rule) in seen:
                continue
            seen.add((path, line, rule))
            findings.append(
                Finding(package.section, package.name, rule, path, line, what,
                        rule in package.strict)
            )
    return findings


# --- Baseline ---------------------------------------------------------------

Counts = dict[str, dict[str, dict[str, int]]]


def count(findings: list[Finding]) -> Counts:
    counts: Counts = {section: defaultdict(lambda: defaultdict(int)) for section in SECTIONS}
    for finding in findings:
        counts[finding.section][finding.package][finding.rule] += 1
    return {
        section: {name: dict(rules) for name, rules in packages.items()}
        for section, packages in counts.items()
    }


def load_baseline(path: Path) -> Counts:
    if not path.is_file():
        return {section: {} for section in SECTIONS}
    document = tomllib.loads(path.read_text(encoding="utf-8"))
    return {
        section: {
            name: {rule: value for rule, value in rules.items() if isinstance(value, int)}
            for name, rules in document.get(section, {}).items()
            if isinstance(rules, dict)
        }
        for section in SECTIONS
    }


def render_baseline(counts: Counts) -> str:
    lines = [
        "# Platform code that apps and the SDK's portable crates still contain",
        "# (tools/policy/check_app_portability.py, rules in app_portability_rules.toml).",
        "# Keyed package -> rule -> count. Counts may only fall: lower an entry when you",
        "# remove sites (the check prints the tightened table); never add one. A new app",
        "# or rule starts at zero.",
    ]
    for section in SECTIONS:
        for name in sorted(counts.get(section, {})):
            rules = {rule: n for rule, n in counts[section][name].items() if n > 0}
            if not rules:
                continue
            key = name if re.fullmatch(r"[A-Za-z0-9_-]+", name) else f'"{name}"'
            lines += ["", f"[{section}.{key}]"]
            lines += [f"{rule} = {rules[rule]}" for rule in sorted(rules)]
    return "\n".join(lines) + "\n"


def compare(current: Counts, baseline: Counts) -> tuple[list[str], list[str]]:
    """Return (regressions, stale entries)."""
    regressions, stale = [], []
    for section in SECTIONS:
        for name, rules in sorted(current.get(section, {}).items()):
            allowed_rules = baseline.get(section, {}).get(name)
            for rule, found in sorted(rules.items()):
                limit = (allowed_rules or {}).get(rule, 0)
                if found <= limit:
                    continue
                if allowed_rules is None:
                    regressions.append(
                        f"{section}.{name}: {found} {rule} finding(s); a package without a "
                        "baseline entry must be clean"
                    )
                else:
                    regressions.append(
                        f"{section}.{name}: {found} {rule} finding(s), baseline allows {limit}"
                    )
        for name, rules in sorted(baseline.get(section, {}).items()):
            for rule, limit in sorted(rules.items()):
                found = current.get(section, {}).get(name, {}).get(rule, 0)
                if found < limit:
                    stale.append(f"{section}.{name}: {rule} fell from {limit} to {found}")
    return regressions, stale


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    parser.add_argument("--findings", nargs="*", metavar="NAME",
                        help="list the sites behind the counts (all packages, or the named ones)")
    parser.add_argument("--print-baseline", action="store_true",
                        help="print the baseline for the current tree")
    args = parser.parse_args(argv)

    findings = audit()
    current = count(findings)
    if args.print_baseline:
        print(render_baseline(current), end="")
        return 0
    if args.findings is not None:
        for finding in findings:
            if not args.findings or finding.package in args.findings:
                print(f"{finding.section}.{finding.package}: {finding}")
        return 0

    strict = [finding for finding in findings if finding.strict]
    if strict:
        print("app portability FAILED: platform code in an app that runs everywhere",
              file=sys.stderr)
        for finding in strict:
            print(f"  {finding.package}: {finding}", file=sys.stderr)
        print(
            "\n  An app whose platforms are linux, macos and web holds no platform code:\n"
            "  move it into a platform crate (crates/platform/*) or an SDK API. Run\n"
            "  `tilo-app check --lint <app>` for the remedies.",
            file=sys.stderr,
        )
        return 1
    regressions, stale = compare(current, load_baseline(ROOT / BASELINE))
    if regressions:
        print("app portability FAILED: platform code grew", file=sys.stderr)
        for line in regressions:
            print(f"  {line}", file=sys.stderr)
        print(
            "\n  Apps call portable APIs; platform code lives in platform crates. Run\n"
            "  `python3 tools/policy/check_app_portability.py --findings <name>` for the sites\n"
            "  and `tilo-app check --lint <app>` for the remedies. An app whose purpose is\n"
            "  platform-specific narrows `[package] platforms` with a `platform-reason`.",
            file=sys.stderr,
        )
        return 1
    if stale:
        print("app portability: counts fell; lock the improvement in", file=sys.stderr)
        for line in stale:
            print(f"  {line}", file=sys.stderr)
        print(f"\n  Replace {BASELINE} with:\n", file=sys.stderr)
        print(render_baseline(current), file=sys.stderr, end="")
        return 1
    apps = current["apps"]
    print(
        f"app portability OK ({sum(sum(r.values()) for r in apps.values())} baselined app "
        f"site(s) in {len(apps)} app(s); "
        f"{sum(sum(r.values()) for r in current['sdk'].values())} in the SDK's portable crates)"
    )
    return 0


if __name__ == "__main__":
    sys.exit(main())
