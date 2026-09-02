#!/usr/bin/env python3
"""Keep qOS applications on the resource/action application model.

Every graphical app must publish its current resources from the widget tree and
describe its operations with ``ActionSpec``. The headless agent is the one
application-model consumer: it surveys peer resources and invokes their action
descriptors. This cheap source-boundary check makes a new bespoke command bus or
an app with no OS-visible surface fail presubmit instead of silently forking the
platform again. It is intentionally a ratchet, not a claim that source
inspection can prove full UI/action parity: known raw callback and
private-palette debt is recorded in an explicit, reduce-only baseline.
"""

from __future__ import annotations

import re
import sys
import tomllib
from dataclasses import dataclass
from pathlib import Path


ROOT = Path(__file__).resolve().parents[1]
BASELINE = "app_resource_action_baseline.toml"

RESOURCE_PROVIDER = re.compile(r"\bfn\s+resources\s*\(")
RESOURCE_VALUE = re.compile(r"\bResource\s*::\s*new\s*\(")
ACTION_SPEC = re.compile(r"\bActionSpec\s*::\s*(?:immediate|prompted|typed|operation)")
RESOURCE_CONSUMER = re.compile(r"\bActionDescriptor\b")
BROKER_CATALOGUE_CONSUMER = re.compile(r"\b(?:ActionCatalogue|BrokerActionSurface)\b")
BROKER_ACTION_INVOKER = re.compile(r"\bInvokePublishedAction\b")
BROKER_ACTION_STATUS = re.compile(r"\bPublishedInvocationStatus\b")
OPERATION_KIND = re.compile(r"\bOperationKind\s*::\s*(?:Effect|View)\b")
OPERATION_HANDLER = re.compile(r"\bfn\s+invoke_resource_operation\s*\(")
OPERATION_HANDLER_REGISTRY = re.compile(
    r"\bfn\s+local_resource_operation_handlers\s*\("
)
TOOLKIT_ACTION_HOST = re.compile(
    r"\bresource_actions\s*::\s*(?:immediate|background)\s*\("
)
TOOLKIT_RESOURCE_VALUE = re.compile(
    r"\bresource_actions\s*::\s*(?:resource|typed_resource)\s*\("
)
LEGACY_ENTRY_POINTS = {
    "legacy room-action override": re.compile(r"\bfn\s+apply_room_action\s*\("),
    "bespoke automation entry point": re.compile(
        r"\bfn\s+(?:automation|agent|mcp|control)_"
        r"(?:invoke|enumerate|dispatch|serve|listen)\b"
    ),
    "bespoke automation server": re.compile(
        r"\bstruct\s+(?:Automation|Agent|Mcp|Control)(?:Server|Socket)\b"
    ),
}

# These do not prove that a callback is wrong. They make the current, reviewed
# amount of bypass-shaped code a hard ceiling, so new work must use the shared
# action route or deliberately update the auditable baseline in review.
RATCHETS = {
    "raw_on_click_closure": (
        re.compile(r"\.on_click\s*\(\s*(?:move\s*)?\|[^|]*\|", re.DOTALL),
        "new raw on_click closure; bind the control to the resource/action route",
    ),
    "bespoke_command_entry": (
        re.compile(r"\bCommandEntry\s*::\s*new\s*\("),
        "new private command-palette entry; publish and invoke an ActionSpec instead",
    ),
}


@dataclass(frozen=True)
class Violation:
    path: Path
    line: int
    reason: str

    def display(self, root: Path) -> str:
        return f"{self.path.relative_to(root)}:{self.line}: {self.reason}"


def dependency_tables(manifest: dict) -> list[dict]:
    tables = []
    for key in ("dependencies", "dev-dependencies", "build-dependencies"):
        value = manifest.get(key, {})
        if isinstance(value, dict):
            tables.append(value)
    targets = manifest.get("target", {})
    if isinstance(targets, dict):
        for target in targets.values():
            if not isinstance(target, dict):
                continue
            for key in ("dependencies", "dev-dependencies", "build-dependencies"):
                value = target.get(key, {})
                if isinstance(value, dict):
                    tables.append(value)
    return tables


def runtime_dependency_tables(manifest: dict) -> list[dict]:
    tables = []
    value = manifest.get("dependencies", {})
    if isinstance(value, dict):
        tables.append(value)
    targets = manifest.get("target", {})
    if isinstance(targets, dict):
        for target in targets.values():
            if isinstance(target, dict):
                value = target.get("dependencies", {})
                if isinstance(value, dict):
                    tables.append(value)
    return tables


def package_name(alias: str, specification: object) -> str:
    if isinstance(specification, dict):
        package = specification.get("package")
        if isinstance(package, str):
            return package
    return alias


def is_ui_core_dependency(alias: str, specification: object) -> bool:
    if package_name(alias, specification) == "ui":
        return True
    if isinstance(specification, dict):
        path = specification.get("path")
        return isinstance(path, str) and Path(path).name == "ui"
    return False


def load_ratchet_baseline(root: Path) -> dict[str, dict[str, int]]:
    path = root / "tools" / BASELINE
    if not path.is_file():
        return {}
    with path.open("rb") as handle:
        document = tomllib.load(handle)
    raw = document.get("ratchets", {})
    if not isinstance(raw, dict):
        return {}
    baseline: dict[str, dict[str, int]] = {}
    for rule, entries in raw.items():
        if isinstance(entries, dict):
            baseline[rule] = {
                path: limit
                for path, limit in entries.items()
                if isinstance(path, str) and isinstance(limit, int) and limit >= 0
            }
    return baseline


def audit_ratchets(
    root: Path,
    sources: list[tuple[Path, str]],
    baseline: dict[str, dict[str, int]],
    violations: list[Violation],
) -> None:
    for source, source_text in sources:
        relative = source.relative_to(root).as_posix()
        code = rust_code_only(source_text)
        for rule, (pattern, reason) in RATCHETS.items():
            matches = list(pattern.finditer(code))
            limit = baseline.get(rule, {}).get(relative, 0)
            if len(matches) <= limit:
                if len(matches) < limit:
                    violations.append(
                        Violation(
                            root / "tools" / BASELINE,
                            1,
                            f"stale {rule} ceiling for {relative}: lower {limit} to {len(matches)}",
                        )
                    )
                continue
            first_excess = matches[limit]
            violations.append(
                Violation(
                    source,
                    line_number(source_text, first_excess.start()),
                    f"{reason} ({len(matches)} found, audited ceiling {limit} in tools/{BASELINE})",
                )
            )


def line_number(text: str, offset: int) -> int:
    return text.count("\n", 0, offset) + 1


def rust_code_only(text: str) -> str:
    """Remove comments and literals so prose cannot satisfy the protocol gate."""
    out: list[str] = []
    i = 0
    block_depth = 0
    while i < len(text):
        if block_depth:
            if text.startswith("/*", i):
                block_depth += 1
                i += 2
            elif text.startswith("*/", i):
                block_depth -= 1
                i += 2
            else:
                out.append("\n" if text[i] == "\n" else " ")
                i += 1
        elif text.startswith("//", i):
            end = text.find("\n", i)
            if end < 0:
                out.extend(" " * (len(text) - i))
                break
            out.extend(" " * (end - i))
            i = end
        elif text.startswith("/*", i):
            block_depth = 1
            out.extend("  ")
            i += 2
        elif text[i] == '"':
            quote = text[i]
            out.append(" ")
            i += 1
            while i < len(text):
                char = text[i]
                out.append("\n" if char == "\n" else " ")
                i += 1
                if char == "\\" and i < len(text):
                    out.append(" ")
                    i += 1
                elif char == quote:
                    break
        else:
            out.append(text[i])
            i += 1
    return "".join(out)


def brace_body(text: str, start: int) -> str | None:
    """Return the balanced body following ``start`` (sufficient for Rust fn bodies)."""
    opening = text.find("{", start)
    if opening < 0:
        return None
    depth = 0
    for offset in range(opening, len(text)):
        if text[offset] == "{":
            depth += 1
        elif text[offset] == "}":
            depth -= 1
            if depth == 0:
                return text[opening + 1 : offset]
    return None


def function_bodies(text: str) -> dict[str, list[str]]:
    bodies: dict[str, list[str]] = {}
    for match in re.finditer(r"\bfn\s+([A-Za-z_][A-Za-z0-9_]*)\b", text):
        body = brace_body(text, match.start())
        if body is not None:
            bodies.setdefault(match.group(1), []).append(body)
    return bodies


def reachable_handler_code(text: str) -> str:
    """The canonical handler plus local helper functions it directly reaches."""
    bodies = function_bodies(text)
    pending = ["invoke_resource_operation", "invoke_resource_operation_v2", "start_resource_operation"]
    seen: set[str] = set()
    reached: list[str] = []
    while pending:
        name = pending.pop()
        if name in seen:
            continue
        seen.add(name)
        for body in bodies.get(name, []):
            reached.append(body)
            for called in re.findall(
                r"(?:(?:crate|self|super)::(?:[A-Za-z_][A-Za-z0-9_]*::)*)?"
                r"([a-z_][A-Za-z0-9_]*)\s*\(",
                rust_code_only(body),
            ):
                if called in bodies and called not in seen:
                    pending.append(called)
    return "\n".join(reached)


def unattached_handler_entries(text: str) -> list[str]:
    """Registry claims with no evidence on the reachable dispatch path."""
    registry = OPERATION_HANDLER_REGISTRY.search(text)
    if registry is None:
        return []
    body = brace_body(text, registry.start()) or ""
    entries = [("literal", value) for value in re.findall(r'"([A-Za-z0-9_.-]+)"', body)]
    entries.extend(
        ("constant", value)
        for value in re.findall(r"\b([A-Z][A-Z0-9_]+)\b", rust_code_only(body))
    )
    reachable = reachable_handler_code(text)
    missing: list[str] = []
    for kind, value in entries:
        present = (
            f'"{value}"' in reachable
            if kind == "literal"
            else re.search(rf"\b{re.escape(value)}\b", rust_code_only(reachable)) is not None
        )
        if not present:
            missing.append(value)
    return sorted(set(missing))


def audit(root: Path = ROOT) -> tuple[list[Violation], int]:
    violations: list[Violation] = []
    ratchet_baseline = load_ratchet_baseline(root)
    baseline_path = root / "tools" / BASELINE
    for rule, entries in ratchet_baseline.items():
        if rule not in RATCHETS:
            violations.append(Violation(baseline_path, 1, f"unknown ratchet rule `{rule}`"))
        for relative in entries:
            if not (root / relative).is_file():
                violations.append(
                    Violation(
                        baseline_path,
                        1,
                        f"stale {rule} baseline path `{relative}` does not exist",
                    )
                )
    manifests = sorted((root / "apps").glob("*/Cargo.toml"))
    for manifest_path in manifests:
        with manifest_path.open("rb") as handle:
            manifest = tomllib.load(handle)
        dependencies = [
            (alias, specification)
            for table in dependency_tables(manifest)
            for alias, specification in table.items()
        ]
        runtime_dependencies = [
            (alias, specification)
            for table in runtime_dependency_tables(manifest)
            for alias, specification in table.items()
        ]
        facades = [
            specification for alias, specification in runtime_dependencies if alias == "ui"
        ]
        if not facades or any(
            not (isinstance(facade, dict) and facade.get("package") == "qos-app")
            for facade in facades
        ):
            violations.append(
                Violation(
                    manifest_path,
                    1,
                    "app must depend on the `qos-app` SDK facade under the `ui` crate name",
                )
            )
        for alias, specification in dependencies:
            if is_ui_core_dependency(alias, specification):
                violations.append(
                    Violation(
                        manifest_path,
                        1,
                        f"direct ui-core dependency `{alias}` bypasses the `qos-app` SDK facade",
                    )
                )

        sources: list[tuple[Path, str]] = []
        for source in sorted((manifest_path.parent / "src").rglob("*.rs")):
            sources.append((source, source.read_text(encoding="utf-8")))
        combined_raw = "\n".join(text for _, text in sources)
        combined = rust_code_only(combined_raw)
        audit_ratchets(root, sources, ratchet_baseline, violations)
        app_name = manifest_path.parent.name
        if app_name == "agent":
            if not RESOURCE_PROVIDER.search(combined) or not RESOURCE_CONSUMER.search(combined):
                violations.append(
                    Violation(
                        manifest_path,
                        1,
                        "headless agent must survey resources and invoke ActionDescriptor entries",
                    )
                )
            if not BROKER_CATALOGUE_CONSUMER.search(combined):
                violations.append(
                    Violation(
                        manifest_path,
                        1,
                        "headless agent must consume the broker's generic action catalogue",
                    )
                )
            if not BROKER_ACTION_INVOKER.search(combined) or not BROKER_ACTION_STATUS.search(combined):
                violations.append(
                    Violation(
                        manifest_path,
                        1,
                        "headless agent must invoke and observe actions through the broker protocol",
                    )
                )
        else:
            toolkit_host = TOOLKIT_ACTION_HOST.search(combined) is not None
            toolkit_resource = TOOLKIT_RESOURCE_VALUE.search(combined) is not None
            if toolkit_host and not toolkit_resource:
                violations.append(
                    Violation(
                        manifest_path,
                        1,
                        "toolkit ResourceActionHost has no toolkit resource descriptor",
                    )
                )
            if not toolkit_host and not RESOURCE_PROVIDER.search(combined):
                violations.append(
                    Violation(
                        manifest_path,
                        1,
                        "app does not implement Widget::resources",
                    )
                )
            if not toolkit_host and not ACTION_SPEC.search(combined):
                violations.append(
                    Violation(
                        manifest_path,
                        1,
                        "app exposes no ActionSpec operations",
                    )
                )
            if not toolkit_host and not RESOURCE_VALUE.search(combined):
                violations.append(
                    Violation(
                        manifest_path,
                        1,
                        "app builds no runtime Resource value (an empty provider is not a protocol)",
                    )
                )
            if (
                not toolkit_host
                and OPERATION_KIND.search(combined)
                and not OPERATION_HANDLER.search(combined)
            ):
                violations.append(
                    Violation(
                        manifest_path,
                        1,
                        "effect/view actions require the canonical invoke_resource_operation handler",
                    )
                )
            if (
                not toolkit_host
                and OPERATION_KIND.search(combined)
                and not OPERATION_HANDLER_REGISTRY.search(combined)
            ):
                violations.append(
                    Violation(
                        manifest_path,
                        1,
                        "effect/view actions require an explicit runtime handler registry",
                    )
                )
            if not toolkit_host and OPERATION_KIND.search(combined):
                for entry in unattached_handler_entries(combined_raw):
                    violations.append(
                        Violation(
                            manifest_path,
                            1,
                            f"declared operation handler `{entry}` is not reachable from the canonical handler",
                        )
                    )

        for source, text in sources:
            for reason, pattern in LEGACY_ENTRY_POINTS.items():
                for match in pattern.finditer(text):
                    violations.append(
                        Violation(source, line_number(text, match.start()), reason)
                    )

    return violations, len(manifests)


def main() -> int:
    violations, app_count = audit()
    if violations:
        print("application resource/action contract FAILED", file=sys.stderr)
        for violation in violations:
            print(f"  {violation.display(ROOT)}", file=sys.stderr)
        return 1
    print(
        "application resource/action contract OK "
        f"({app_count} app crates; one resource/action protocol)"
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
