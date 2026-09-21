#!/usr/bin/env python3
"""Ratchet direct platform implementation out of application packages.

This is deliberately a lexical source and manifest gate.  Framework crates may
and must contain native implementations; application packages below ``apps/``
must reach them through portable contracts.  More expensive public-API and
target-resolved graph checks belong to later migration gates.
"""

from __future__ import annotations

import argparse
import json
import os
import re
import sys
import tomllib
from collections import Counter
from dataclasses import dataclass
from pathlib import Path
from typing import Iterator


ROOT = Path(__file__).resolve().parents[2]
BASELINE = Path("tools/policy/app_platform_baseline.json")
REGISTER = Path("tools/policy/app_platform_findings.json")
EXCLUDED_PARTS = {
    "target",
    ".git",
    ".hg",
    ".svn",
    ".cache",
    "__pycache__",
    "vendor",
    "node_modules",
}

NATIVE_CRATES = {
    "libc",
    "nix",
    "rustix",
    "windows_sys",
    "windows",
    "ashpd",
    "zbus",
    "xattr",
}
NATIVE_DEPENDENCIES = (NATIVE_CRATES - {"windows_sys"}) | {
    "windows-sys",
    "runtime-portal-xdg",
}
NATIVE_FEATURES = {"webview-wpe", "dmabuf-import", "mpris"}
SDK_PACKAGES = {"ui", "tilo-app"}
RAW_STRING_START = re.compile(r"(?:br|cr|r)(#{0,255})\"")


@dataclass(frozen=True)
class Violation:
    path: Path
    line: int
    rule: str
    reason: str

    def display(self, root: Path) -> str:
        try:
            path = self.path.relative_to(root).as_posix()
        except ValueError:
            path = self.path.as_posix()
        return f"{path}:{self.line}: [{self.rule}] {self.reason}"


@dataclass(frozen=True)
class Site:
    path: Path
    line: int
    rule: str
    fingerprint: str
    count: int
    reason: str

    def as_baseline_entry(self) -> dict[str, object]:
        return {
            "path": self.path.as_posix(),
            "line": self.line,
            "rule": self.rule,
            "fingerprint": self.fingerprint,
            "count": self.count,
        }


@dataclass(frozen=True)
class _Occurrence:
    path: Path
    line: int
    rule: str
    fingerprint: str
    reason: str


def _excluded(path: Path) -> bool:
    return any(part in EXCLUDED_PARTS for part in path.parts)


def _line_number(text: str, offset: int) -> int:
    return text.count("\n", 0, offset) + 1


def _source_fingerprint(code: str, offset: int) -> str:
    start = code.rfind("\n", 0, offset) + 1
    end = code.find("\n", offset)
    if end < 0:
        end = len(code)
    return "source:" + " ".join(code[start:end].split())


def _span_fingerprint(code: str, start: int, end: int) -> str:
    return "source:" + " ".join(code[start:end].split())


def _matching_brace(text: str, opening: int) -> int | None:
    depth = 1
    for index in range(opening + 1, len(text)):
        if text[index] == "{":
            depth += 1
        elif text[index] == "}":
            depth -= 1
            if depth == 0:
                return index
    return None


def _top_level_self_aliases(use_tree: str) -> Iterator[str]:
    pattern = re.compile(r"\bself\s+as\s+([A-Za-z_][A-Za-z0-9_]*)")
    for match in pattern.finditer(use_tree):
        depth = 0
        for character in use_tree[: match.start()]:
            if character == "{":
                depth += 1
            elif character == "}":
                depth -= 1
        if depth == 0:
            yield match.group(1)


def _nested_std_os_aliases(std_use_tree: str) -> Iterator[str]:
    os_group = re.compile(r"\bos\s*::\s*\{")
    for match in os_group.finditer(std_use_tree):
        prefix_depth = std_use_tree[: match.start()].count("{") - std_use_tree[
            : match.start()
        ].count("}")
        if prefix_depth != 0:
            continue
        opening = match.end() - 1
        closing = _matching_brace(std_use_tree, opening)
        if closing is not None:
            yield from _top_level_self_aliases(std_use_tree[opening + 1 : closing])


def _char_literal_end(text: str, quote: int) -> int | None:
    """Return the end of a Rust character literal, or None for a lifetime."""

    cursor = quote + 1
    if cursor >= len(text) or text[cursor] in "'\n\r":
        return None
    if text[cursor] != "\\":
        cursor += 1
    elif text.startswith("\\u{", cursor):
        close = text.find("}", cursor + 3)
        if close < 0:
            return None
        cursor = close + 1
    else:
        cursor += 2
    if cursor < len(text) and text[cursor] == "'":
        return cursor + 1
    return None


def rust_code_only(text: str) -> str:
    """Blank Rust comments and literal contents while retaining positions.

    Newlines, quote delimiters and literal widths are retained so diagnostics
    still point at the original line.  Rust raw strings and nested block
    comments need explicit handling; a regular expression masker is not enough.
    """

    out = list(text)
    i = 0
    block_depth = 0
    length = len(text)

    def blank(start: int, end: int) -> None:
        for index in range(start, end):
            if out[index] != "\n":
                out[index] = " "

    while i < length:
        if block_depth:
            if text.startswith("/*", i):
                blank(i, i + 2)
                block_depth += 1
                i += 2
            elif text.startswith("*/", i):
                blank(i, i + 2)
                block_depth -= 1
                i += 2
            else:
                blank(i, i + 1)
                i += 1
            continue

        if text.startswith("//", i):
            end = text.find("\n", i)
            if end < 0:
                end = length
            blank(i, end)
            i = end
            continue
        if text.startswith("/*", i):
            blank(i, i + 2)
            block_depth = 1
            i += 2
            continue

        raw = RAW_STRING_START.match(text, i)
        if raw:
            hashes = raw.group(1)
            content_start = raw.end()
            terminator = '"' + hashes
            end = text.find(terminator, content_start)
            end = length if end < 0 else end + len(terminator)
            # Keep the delimiters syntactically visible, but erase all content.
            blank(content_start, end - len(terminator) if end < length else end)
            i = end
            continue

        char_quote = i + 1 if text.startswith("b'", i) else i
        if text[char_quote : char_quote + 1] == "'":
            char_end = _char_literal_end(text, char_quote)
            if char_end is not None:
                blank(i, char_end)
                i = char_end
                continue

        prefix = 1 if text.startswith(('b"', 'c"'), i) else 0
        if text[i + prefix : i + prefix + 1] == '"':
            quote = i + prefix
            cursor = quote + 1
            escaped = False
            while cursor < length:
                char = text[cursor]
                if char == '"' and not escaped:
                    break
                escaped = char == "\\" and not escaped
                if char != "\\":
                    escaped = False
                cursor += 1
            blank(quote + 1, min(cursor, length))
            i = min(cursor + 1, length)
            continue
        i += 1
    return "".join(out)


def _walk_files(directory: Path, filename: str | None = None, suffix: str | None = None) -> Iterator[Path]:
    for current, directories, files in os.walk(directory):
        directories[:] = sorted(name for name in directories if name not in EXCLUDED_PARTS)
        for name in sorted(files):
            if filename is not None and name != filename:
                continue
            if suffix is not None and not name.endswith(suffix):
                continue
            yield Path(current) / name


def _source_occurrences(root: Path) -> Iterator[_Occurrence]:
    apps = root / "apps"
    if not apps.is_dir():
        return
    native_path = re.compile(
        r"\b(" + "|".join(re.escape(name) for name in sorted(NATIVE_CRATES, key=len, reverse=True))
        + r")\s*::"
    )
    alias_import = re.compile(
        r"\buse\s+("
        + "|".join(re.escape(name) for name in sorted(NATIVE_CRATES, key=len, reverse=True))
        + r")\s+as\s+([A-Za-z_][A-Za-z0-9_]*)\s*;"
    )
    grouped_alias_import = re.compile(
        r"\buse\s+("
        + "|".join(re.escape(name) for name in sorted(NATIVE_CRATES, key=len, reverse=True))
        + r")\s*::\s*\{[^}\n]*\bself\s+as\s+([A-Za-z_][A-Za-z0-9_]*)"
    )
    std_os_alias_import = re.compile(
        r"\buse\s+std\s*::\s*os\s+as\s+([A-Za-z_][A-Za-z0-9_]*)\s*;"
    )
    grouped_std_os_import = re.compile(
        r"\buse\s+std\s*::\s*os\s*::\s*\{([^;]*)\}\s*;", re.DOTALL
    )
    grouped_std_import = re.compile(r"\buse\s+std\s*::\s*\{([^;]*)\}\s*;", re.DOTALL)
    grouped_std_alias = re.compile(
        r"\bos\s+as\s+([A-Za-z_][A-Za-z0-9_]*)"
    )
    ffi = re.compile(r"\b(?:unsafe\s+)?extern(?:\s+\"[^\"]*\")?\s*\{")
    cfg_start = re.compile(r"\b(?:cfg|cfg_attr)\s*!?\s*\(")
    native_cfg_term = re.compile(r"\b(?:target_os|target_family|unix|windows)\b")

    for path in _walk_files(apps, suffix=".rs"):
        relative = path.relative_to(root)
        if _excluded(relative):
            continue
        text = path.read_text(encoding="utf-8")
        code = rust_code_only(text)
        aliases: dict[str, str] = {}
        for match in alias_import.finditer(code):
            aliases[match.group(2)] = match.group(1)
            yield _Occurrence(
                relative,
                _line_number(text, match.start()),
                "native-api",
                _source_fingerprint(code, match.start()),
                f"application imports native crate {match.group(1)} as {match.group(2)}; move this implementation into the framework",
            )
        for match in grouped_alias_import.finditer(code):
            aliases[match.group(2)] = match.group(1)
            yield _Occurrence(
                relative,
                _line_number(text, match.start()),
                "native-api",
                _source_fingerprint(code, match.start()),
                f"application imports native crate {match.group(1)} as {match.group(2)}; move this implementation into the framework",
            )
        for match in std_os_alias_import.finditer(code):
            alias = match.group(1)
            aliases[alias] = "std::os"
            yield _Occurrence(
                relative,
                _line_number(text, match.start()),
                "native-api",
                _source_fingerprint(code, match.start()),
                f"application imports std::os as {alias}; move this implementation into a framework host service",
            )
        for match in grouped_std_os_import.finditer(code):
            for alias in _top_level_self_aliases(match.group(1)):
                aliases[alias] = "std::os"
        for match in grouped_std_import.finditer(code):
            body = match.group(1)
            os_path = re.search(r"\bos\s*::", body)
            os_alias = grouped_std_alias.search(body)
            if os_path or os_alias:
                yield _Occurrence(
                    relative,
                    _line_number(text, match.start()),
                    "native-api",
                    _span_fingerprint(code, match.start(), match.end()),
                    "application imports an OS-specific std API through a grouped use; move the operation behind a framework host service",
                )
            if os_alias:
                aliases[os_alias.group(1)] = "std::os"
            for alias in _nested_std_os_aliases(body):
                aliases[alias] = "std::os"

        for match in re.finditer(r"\bstd\s*::\s*os\s*::", code):
            yield _Occurrence(
                relative,
                _line_number(text, match.start()),
                "native-api",
                _source_fingerprint(code, match.start()),
                "application imports an OS-specific std API; move the operation behind a framework host service",
            )
        for match in native_path.finditer(code):
            crate = match.group(1)
            yield _Occurrence(
                relative,
                _line_number(text, match.start()),
                "native-api",
                _source_fingerprint(code, match.start()),
                f"application directly uses native crate path {crate}::; expose a semantic framework operation instead",
            )
        for alias, crate in sorted(aliases.items()):
            pattern = re.compile(rf"\b{re.escape(alias)}\s*::")
            for match in pattern.finditer(code):
                yield _Occurrence(
                    relative,
                    _line_number(text, match.start()),
                    "native-api",
                    _source_fingerprint(code, match.start()),
                    f"application uses renamed native crate {alias} ({crate}); move this implementation into the framework",
                )
        for match in ffi.finditer(code):
            yield _Occurrence(
                relative,
                _line_number(text, match.start()),
                "native-ffi",
                _source_fingerprint(code, match.start()),
                "application declares a native FFI block; place the binding and safety contract in a framework backend",
            )
        for match in cfg_start.finditer(code):
            cursor = match.end()
            depth = 1
            while cursor < len(code) and depth:
                if code[cursor] == "(":
                    depth += 1
                elif code[cursor] == ")":
                    depth -= 1
                cursor += 1
            if depth or not native_cfg_term.search(code, match.end(), cursor - 1):
                continue
            yield _Occurrence(
                relative,
                _line_number(text, match.start()),
                "platform-cfg",
                _span_fingerprint(code, match.start(), cursor),
                "application selects an OS/family implementation; select the backend in framework composition",
            )


def _load_toml(path: Path) -> tuple[dict, str]:
    text = path.read_text(encoding="utf-8")
    return tomllib.loads(text), text


def _package_name(alias: str, specification: object) -> str:
    if isinstance(specification, dict) and isinstance(specification.get("package"), str):
        return specification["package"]
    return alias


def _merged_workspace_spec(
    alias: str, specification: object, workspace_dependencies: dict
) -> object:
    if not isinstance(specification, dict) or specification.get("workspace") is not True:
        return specification
    inherited = workspace_dependencies.get(alias, {})
    if not isinstance(inherited, dict):
        return inherited
    merged = dict(inherited)
    local = {key: value for key, value in specification.items() if key != "workspace"}
    inherited_features = inherited.get("features", [])
    local_features = local.pop("features", [])
    merged.update(local)
    if isinstance(inherited_features, list) or isinstance(local_features, list):
        merged["features"] = list(
            dict.fromkeys(
                [
                    feature
                    for features in (inherited_features, local_features)
                    if isinstance(features, list)
                    for feature in features
                ]
            )
        )
    return merged


def _dependency_tables(manifest: dict) -> Iterator[tuple[str, dict]]:
    for name in ("dependencies", "dev-dependencies", "build-dependencies"):
        table = manifest.get(name)
        if isinstance(table, dict):
            yield name, table
    targets = manifest.get("target")
    if isinstance(targets, dict):
        for target_name, target in targets.items():
            if not isinstance(target, dict):
                continue
            for name in ("dependencies", "dev-dependencies", "build-dependencies"):
                table = target.get(name)
                if isinstance(table, dict):
                    yield f"target.{target_name}.{name}", table


def _table_ranges(text: str) -> list[tuple[str, int, int]]:
    headers = list(re.finditer(r"(?m)^\s*\[([^]\n]+)]\s*$", text))
    ranges: list[tuple[str, int, int]] = []
    for index, match in enumerate(headers):
        end = headers[index + 1].start() if index + 1 < len(headers) else len(text)
        ranges.append((match.group(1).replace("'", "").replace('"', ""), match.end(), end))
    return ranges


def _manifest_key_line(text: str, table: str, key: str) -> int:
    normalized_table = table.replace("'", "").replace('"', "")
    key_pattern = re.compile(
        rf"(?m)^\s*(?:{re.escape(key)}|[\"']{re.escape(key)}[\"'])"
        r"(?:\.[A-Za-z_][A-Za-z0-9_-]*)?\s*="
    )
    for name, start, end in _table_ranges(text):
        if name == f"{normalized_table}.{key}":
            return _line_number(text, start)
        if name == normalized_table:
            match = key_pattern.search(text, start, end)
            if match:
                return _line_number(text, match.start())
    match = key_pattern.search(text)
    return _line_number(text, match.start()) if match else 1


def _is_sdk(alias: str, specification: object) -> bool:
    package = _package_name(alias, specification)
    if package in SDK_PACKAGES:
        return True
    if isinstance(specification, dict) and isinstance(specification.get("path"), str):
        return Path(specification["path"]).name in SDK_PACKAGES
    return False


def _manifest_occurrences(root: Path) -> tuple[list[_Occurrence], int, list[Violation]]:
    apps = root / "apps"
    if not apps.is_dir():
        return [], 0, [
            Violation(
                apps,
                1,
                "manifest-config",
                "no application packages can be observed because apps/ is missing",
            )
        ]
    workspace_dependencies: dict = {}
    workspace_path = root / "Cargo.toml"
    configuration: list[Violation] = []
    if workspace_path.is_file():
        try:
            workspace, _ = _load_toml(workspace_path)
            raw = workspace.get("workspace", {}).get("dependencies", {})
            if isinstance(raw, dict):
                workspace_dependencies = raw
        except (OSError, UnicodeError, tomllib.TOMLDecodeError) as error:
            configuration.append(
                Violation(workspace_path, 1, "manifest-config", f"cannot parse workspace manifest: {error}")
            )

    occurrences: list[_Occurrence] = []
    checked_apps = 0
    for path in _walk_files(apps, filename="Cargo.toml"):
        relative = path.relative_to(root)
        if _excluded(relative):
            continue
        try:
            manifest, text = _load_toml(path)
        except (OSError, UnicodeError, tomllib.TOMLDecodeError) as error:
            configuration.append(
                Violation(path, 1, "manifest-config", f"cannot parse application manifest: {error}")
            )
            continue
        if not isinstance(manifest.get("package"), dict):
            continue
        checked_apps += 1
        sdk_aliases: set[str] = set()
        for table_name, table in _dependency_tables(manifest):
            for alias, local_spec in table.items():
                specification = _merged_workspace_spec(alias, local_spec, workspace_dependencies)
                package = _package_name(alias, specification)
                line = _manifest_key_line(text, table_name, alias)
                if package in NATIVE_DEPENDENCIES:
                    occurrences.append(
                        _Occurrence(
                            relative,
                            line,
                            "native-dependency",
                            f"{table_name}:{alias}->{package}",
                            f"application directly depends on native mechanism {package} as {alias}; depend on a semantic framework service",
                        )
                    )
                if _is_sdk(alias, specification):
                    sdk_aliases.add(alias)
                    if isinstance(specification, dict):
                        features = specification.get("features", [])
                        if isinstance(features, list):
                            for feature in features:
                                if feature in NATIVE_FEATURES:
                                    occurrences.append(
                                        _Occurrence(
                                            relative,
                                            line,
                                            "native-feature",
                                            f"{table_name}:{alias}/feature:{feature}",
                                            f"application selects backend feature {alias}/{feature}; select a semantic SDK capability instead",
                                        )
                                    )

        features = manifest.get("features", {})
        if isinstance(features, dict):
            for feature_name, selections in features.items():
                if not isinstance(selections, list):
                    continue
                line = _manifest_key_line(text, "features", feature_name)
                for selection in selections:
                    if not isinstance(selection, str) or "/" not in selection:
                        continue
                    alias, selected = selection.split("/", 1)
                    alias = alias.removesuffix("?")
                    if alias in sdk_aliases and selected in NATIVE_FEATURES:
                        occurrences.append(
                            _Occurrence(
                                relative,
                                line,
                                "native-feature",
                                f"feature:{feature_name}:{alias}/{selected}",
                                f"application feature {feature_name} selects backend feature {alias}/{selected}; expose a semantic feature",
                            )
                        )
    if checked_apps == 0:
        configuration.append(
            Violation(
                apps,
                1,
                "manifest-config",
                "no application packages were found below apps/",
            )
        )
    return occurrences, checked_apps, configuration


def _group_sites(occurrences: list[_Occurrence]) -> list[Site]:
    counts = Counter((item.path, item.rule, item.fingerprint) for item in occurrences)
    lines: dict[tuple[Path, str, str], int] = {}
    reasons = {
        (item.path, item.rule, item.fingerprint): item.reason for item in occurrences
    }
    for item in occurrences:
        key = (item.path, item.rule, item.fingerprint)
        lines[key] = min(lines.get(key, item.line), item.line)
    return [
        Site(path, lines[(path, rule, fingerprint)], rule, fingerprint, count, reasons[(path, rule, fingerprint)])
        for (path, rule, fingerprint), count in sorted(
            counts.items(), key=lambda item: (item[0][0].as_posix(), item[0][1], item[0][2])
        )
    ]


def _collect(root: Path) -> tuple[list[Site], int, list[Violation]]:
    manifest_occurrences, checked_apps, configuration = _manifest_occurrences(root)
    occurrences = list(_source_occurrences(root)) + manifest_occurrences
    return _group_sites(occurrences), checked_apps, configuration


def inventory(root: Path) -> tuple[list[Site], int]:
    """Return every detected app-side native site without applying exceptions."""

    sites, checked_apps, configuration = _collect(root)
    if configuration:
        # The CLI reports configuration failures.  Direct callers get synthetic
        # sites so inventory can never silently claim an unreadable tree is clean.
        sites.extend(
            Site(
                violation.path.relative_to(root),
                violation.line,
                violation.rule,
                "configuration-error",
                1,
                violation.reason,
            )
            for violation in configuration
        )
    return sites, checked_apps


def _config_violation(root: Path, path: Path, reason: str) -> Violation:
    return Violation(root / path, 1, "baseline-config", reason)


def _load_baseline(root: Path) -> tuple[list[dict], list[Violation]]:
    path = root / BASELINE
    if not path.is_file():
        return [], []
    try:
        document = json.loads(path.read_text(encoding="utf-8"))
    except (OSError, UnicodeError, json.JSONDecodeError) as error:
        return [], [_config_violation(root, BASELINE, f"cannot parse baseline JSON: {error}")]
    if not isinstance(document, dict) or document.get("schema_version") != 1:
        return [], [_config_violation(root, BASELINE, "baseline must be an object with schema_version 1")]
    entries = document.get("entries")
    if not isinstance(entries, list):
        return [], [_config_violation(root, BASELINE, "baseline entries must be a list")]

    required = {
        "path": str,
        "line": int,
        "rule": str,
        "fingerprint": str,
        "count": int,
        "finding_id": str,
        "removal_task": str,
        "rationale": str,
    }
    valid: list[dict] = []
    violations: list[Violation] = []
    seen: set[tuple] = set()
    for index, entry in enumerate(entries):
        malformed = not isinstance(entry, dict) or any(
            key not in entry or not isinstance(entry[key], expected)
            for key, expected in required.items()
        )
        if not malformed and (
            entry["line"] < 1
            or entry["count"] < 1
            or not entry["path"].startswith("apps/")
            or not entry["rationale"].strip()
        ):
            malformed = True
        if malformed:
            violations.append(
                _config_violation(root, BASELINE, f"malformed baseline entry at index {index}")
            )
            continue
        key = (entry["path"], entry["rule"], entry["fingerprint"])
        if key in seen:
            violations.append(
                _config_violation(root, BASELINE, f"duplicate baseline entry for {entry['path']}:{entry['line']} {entry['rule']} {entry['fingerprint']}")
            )
            continue
        seen.add(key)
        valid.append(entry)
    return valid, violations


def _validate_register(root: Path, entries: list[dict]) -> list[Violation]:
    if not entries:
        return []
    path = root / REGISTER
    try:
        document = json.loads(path.read_text(encoding="utf-8"))
    except (OSError, UnicodeError, json.JSONDecodeError) as error:
        return [_config_violation(root, REGISTER, f"cannot parse finding register: {error}")]
    if not isinstance(document, dict):
        return [_config_violation(root, REGISTER, "finding register must be a JSON object")]
    findings = {
        item.get("id") for item in document.get("findings", []) if isinstance(item, dict)
    }
    tasks = {item.get("id") for item in document.get("tasks", []) if isinstance(item, dict)}
    violations: list[Violation] = []
    for entry in entries:
        if entry["finding_id"] not in findings:
            violations.append(
                _config_violation(
                    root,
                    BASELINE,
                    f"finding ID {entry['finding_id']} does not exist in {REGISTER.name}",
                )
            )
        if entry["removal_task"] not in tasks:
            violations.append(
                _config_violation(
                    root,
                    BASELINE,
                    f"removal task {entry['removal_task']} does not exist in {REGISTER.name}",
                )
            )
    return violations


def audit(root: Path) -> tuple[list[Violation], int]:
    """Audit application sources/manifests and apply exact migration exceptions."""

    root = root.resolve()
    sites, checked_apps, violations = _collect(root)
    entries, baseline_violations = _load_baseline(root)
    violations.extend(baseline_violations)
    violations.extend(_validate_register(root, entries))

    baseline_by_key = {
        (entry["path"], entry["rule"], entry["fingerprint"]): entry
        for entry in entries
    }
    sites_by_key = {
        (site.path.as_posix(), site.rule, site.fingerprint): site for site in sites
    }

    for key, site in sites_by_key.items():
        entry = baseline_by_key.get(key)
        allowed = entry["count"] if entry else 0
        if site.count > allowed:
            violations.append(
                Violation(
                    root / site.path,
                    site.line,
                    site.rule,
                    f"{site.reason}; {site.count - allowed} new occurrence(s) at this exact site ({site.count} detected, {allowed} excepted)",
                )
            )

    for key, entry in baseline_by_key.items():
        actual = sites_by_key.get(key)
        count = actual.count if actual else 0
        if count < entry["count"]:
            detail = (
                "site is gone"
                if count == 0
                else f"occurrence count decreased from {entry['count']} to {count}"
            )
            violations.append(
                Violation(
                    root / entry["path"],
                    entry["line"],
                    "stale-exception",
                    f"stale exception for {entry['rule']} ({detail}); remove or shrink the attributed baseline entry",
                )
            )
    violations.sort(key=lambda item: (item.path.as_posix(), item.line, item.rule, item.reason))
    return violations, checked_apps


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--root", type=Path, default=ROOT, help="repository root")
    parser.add_argument(
        "--inventory",
        action="store_true",
        help="print detected sites as JSON without applying or modifying the baseline",
    )
    args = parser.parse_args(argv)
    root = args.root.resolve()
    if args.inventory:
        sites, checked_apps, configuration = _collect(root)
        if configuration:
            for violation in configuration:
                print(violation.display(root), file=sys.stderr)
            return 2
        print(
            json.dumps(
                {
                    "schema_version": 1,
                    "checked_apps": checked_apps,
                    "entries": [site.as_baseline_entry() for site in sites],
                },
                indent=2,
            )
        )
        return 0

    violations, checked_apps = audit(root)
    for violation in violations:
        print(violation.display(root))
    if violations:
        print(f"app platform contract failed: {len(violations)} violation(s), {checked_apps} app package(s) checked")
        return 1
    print(f"app platform contract passed: {checked_apps} app package(s) checked")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
