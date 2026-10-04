#!/usr/bin/env python3
"""Audit Tilo applications' resource/action model.

Every graphical app must publish its current resources from the widget tree and
describe its operations with ``ActionSpec``. The headless agent is the one
application-model consumer: it surveys peer resources and invokes their action
descriptors. Source rules reject bespoke command buses and applications without
an OS-visible surface; the runtime ``--native-audit`` verifies the live
accessibility/resource/control inventory.
"""

from __future__ import annotations

import re
import sys
import tomllib
from dataclasses import dataclass
from pathlib import Path


ROOT = Path(__file__).resolve().parents[2]
BASELINE = Path("tools/policy/app_resource_action_baseline.toml")

RESOURCE_PROVIDER = re.compile(r"\bfn\s+resources\s*\(")
RESOURCE_VALUE = re.compile(r"\bResource\s*::\s*new\s*\(")
ACTION_SPEC = re.compile(r"\bActionSpec\s*::\s*(?:immediate|prompted|typed|operation)")
RESOURCE_CONSUMER = re.compile(r"\bActionDescriptor\b")
# The agent reaches published actions through the remote-action vocabulary,
# the same messages peers and local SDK callers use.
BROKER_CATALOGUE_CONSUMER = re.compile(r"\bRemoteActionMessage\s*::\s*CatalogueRequest\b")
BROKER_ACTION_INVOKER = re.compile(r"\bRemoteActionMessage\s*::\s*Invoke\b")
BROKER_ACTION_STATUS = re.compile(r"\bRemoteActionMessage\s*::\s*(?:Observe|Update)\b")
OPERATION_KIND = re.compile(r"\bOperationKind\s*::\s*(?:Effect|View)\b")
OPERATION_HANDLER = re.compile(r"\bfn\s+start_operation\s*\(")
OPERATION_HANDLER_REGISTRY = re.compile(
    r"\bfn\s+local_resource_operation_handlers\s*\("
)
ACTION_REGISTRY_DEFINITION = re.compile(
    r"\bfn\s+register\s*\([^)]*\bActionRegistry\b", re.DOTALL
)
LIVE_ACTION_REGISTRY_WIRING = re.compile(r"\bactions\s*::\s*register\s*\(")
TOOLKIT_ACTION_HOST = re.compile(
    r"\bresource_actions\s*::\s*(?:immediate|background)\s*\("
)
TOOLKIT_RESOURCE_VALUE = re.compile(
    r"\bresource_actions\s*::\s*(?:resource|typed_resource)\s*\("
)
# A `tilo-app` framework app declares a resource type and hosts it through
# `ResourceApp`; the framework publishes that resource's actions (named
# declarations plus the typed `apply` door), builds its runtime Resource value
# and installs its replay decoder in the component registry.
FRAMEWORK_APP = re.compile(r"\bimpl\s+(?:[\w:]+::)?ResourceApp\s+for\b")
# The framework's launch installs the resource's replay decoder
# (`Resource::register`) into the component registry itself.
FRAMEWORK_LAUNCH = re.compile(
    r"\bframework\s*::\s*(?:run|launch_spec)\s*::\s*<|\bui\s*::\s*main\s*!\s*\("
)
FRAMEWORK_RESOURCE = re.compile(
    r"#\[\s*resource\s*\(|\bimpl\s+(?:[\w:]+::)?Resource\s+for\b"
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

SEARCH_PROVIDER_IMPL = re.compile(
    r"\bimpl(?:\s*<[^>{}]*>)?\s+SearchProvider\s+for\s+([A-Za-z_][A-Za-z0-9_]*)"
)
# `ui::search::CatalogSearchProvider` derives every query method from one
# retained `SearchCatalog`, so its budgets hold by construction. What remains
# for an implementation to get wrong is `catalog()` itself: building a
# catalogue there re-indexes the whole corpus on every query.
CATALOG_SEARCH_PROVIDER_IMPL = re.compile(
    r"\bimpl(?:\s*<[^>{}]*>)?\s+(?:[A-Za-z_][A-Za-z0-9_]*\s*::\s*)*CatalogSearchProvider"
    r"\s+for\s+([A-Za-z_][A-Za-z0-9_]*)"
)
PER_QUERY_CATALOG_BUILD = re.compile(
    r"\bSearchCatalog\s*::\s*new\s*\(|\.\s*(?:replace_all|upsert)\s*\("
)
FRAMEWORK_RESOURCE_SEARCH_BUILD = re.compile(
    r"crate\s*::\s*search\s*::\s*OpenResourceSearch\s*::\s*new\s*\(\s*&resources\s*\)"
)
FRAMEWORK_RESOURCE_SEARCH_INSTALL = re.compile(
    r"providers\s*\.\s*push\s*\(\s*Box\s*::\s*new\s*\(\s*open_resources\s*\)"
)
# Exception keys are `apps/<app>/src/<file>.rs::ProviderType`. Keep this empty
# by default: adding one records that the corpus is bounded and the unbounded
# fallback is safe for that provider.
SEARCH_LIMIT_EXEMPTIONS: dict[str, str] = {}

# Reviewed exceptions for providers that discard a durable tail before querying
# it. New entries require an explicit review; migrated providers are removed.
TRUNCATING_SEARCH_REMEDIATION: dict[str, str] = {
}
TRUNCATING_SEARCH = re.compile(
    r"\.take\s*\(\s*DEFAULT_SEARCH_INDEX_LIMIT(?:\s*\.\s*saturating_sub\s*\([^)]*\))?\s*\)"
)

# These limits record the reviewed amount of bypass-shaped code. New work must
# use the shared action route or update the auditable baseline.
RATCHETS = {
    "raw_on_click_closure": (
        re.compile(r"\.on_click\s*\(\s*(?:move\s*)?\|[^|]*\|", re.DOTALL),
        "new raw on_click closure; bind the control to the resource/action route",
    ),
    "bespoke_command_entry": (
        re.compile(r"\bCommandEntry\s*::\s*new\s*\("),
        "new private command-palette entry; publish and invoke an ActionSpec instead",
    ),
    "private_palette_argument": (
        re.compile(r"\.open_argument\s*\("),
        "private command-palette argument route; use open_action_argument with a published ActionSpec",
    ),
    "private_palette_outcome": (
        re.compile(r"\bCommandOutcome\b"),
        "private command-palette outcome; drain the shared ControlInvocation route instead",
    ),
    "opaque_search_activation": (
        re.compile(r"\bfn\s+activate_search_result\s*\("),
        "opaque search activation bypass; return SearchResult::for_action with a published resource action",
    ),
    "surface_document_actions": (
        re.compile(r"\bfn\s+publishes_document_actions\s*\("),
        "a surface publishing document actions of its own; bind edits to its view state with "
        "ResourceSurface::bound_declarations and publish live rows with ResourceSurface::live_rows",
    ),
}

# The one resource/action/view model (docs/architecture/README.md): an app's
# edits go through the framework `Dispatcher`, it launches as a `ResourceApp`,
# its durable state is the resource's document, and nothing beside
# `Action::apply` writes that document. These count production code only (test
# files and the trailing `#[cfg(test)]` module are exempt) and may only go down.
MODEL_RATCHETS = {
    "app_action_host": (
        re.compile(r"\bActionHost\b"),
        "app-owned ActionHost; submit edits through the framework Dispatcher",
    ),
    "standalone_launch": (
        re.compile(
            r"\bLaunchSpec\s*::\s*new\b|\bLegacyLaunchInputs\b|\bLegacyRecordPolicy\b"
            r"|\.\s*as_definition\s*\(|\.\s*shot_root\s*\("
        ),
        "standalone widget launch; launch the ResourceApp and stage shots with ResourceApp::scenario",
    ),
    "direct_document_api": (
        re.compile(r"\bDocMut\b"),
        "direct document write; change the document through the resource's Action",
    ),
    "empty_action_vocabulary": (
        re.compile(r"\benum\s+[A-Za-z_][A-Za-z0-9_]*\s*(?:<[^>{}]*>)?\s*\{\s*\}"),
        "empty action vocabulary; the app's durable state belongs in its resource document",
    ),
    "widget_owned_document": (
        re.compile(r"\bAutoCommit\s*::\s*(?:new|load|load_with\w*)\s*\("),
        "app-owned document; the framework owns the resource document",
    ),
}
RATCHET_REASONS = {
    **{rule: reason for rule, (_, reason) in RATCHETS.items()},
    **{rule: reason for rule, (_, reason) in MODEL_RATCHETS.items()},
}
TEST_MODULE_START = re.compile(r"^\s*#\[cfg\((?:test|all\(test)", re.MULTILINE)


def is_test_source(relative: str) -> bool:
    name = relative.rsplit("/", 1)[-1]
    return (
        "/tests/" in relative
        or name in {"tests.rs", "test_support.rs", "testing.rs"}
        or name.endswith("_tests.rs")
        or name.startswith("test_")
    )


def production_code(text: str) -> str:
    """Code up to the conventional trailing test module, without comments or literals."""
    found = TEST_MODULE_START.search(text)
    if found:
        text = text[: found.start()]
    return rust_code_only(text)


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
        return isinstance(path, str) and Path(path).name in {"ui", "ui-core"}
    return False


def load_ratchet_baseline(root: Path) -> dict[str, dict[str, int]]:
    path = root / BASELINE
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
        rules = [(rule, pattern, reason, code) for rule, (pattern, reason) in RATCHETS.items()]
        # A Tilo workspace invariant, like the app host check in `audit`; the
        # miniature SDK apps the policy tests build are not held to it.
        if root.resolve() == ROOT.resolve():
            production = "" if is_test_source(relative) else production_code(source_text)
            rules += [
                (rule, pattern, reason, production)
                for rule, (pattern, reason) in MODEL_RATCHETS.items()
            ]
        for rule, pattern, reason, code in rules:
            matches = list(pattern.finditer(code))
            limit = baseline.get(rule, {}).get(relative, 0)
            if len(matches) <= limit:
                if len(matches) < limit:
                    violations.append(
                        Violation(
                            root / BASELINE,
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
                    f"{reason} ({len(matches)} found, audited ceiling {limit} in {BASELINE})",
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
    pending = ["start_operation"]
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


def live_launch_registration_code(text: str) -> str:
    """Return production `main` plus named LaunchSpec configuration callbacks.

    Scanning the entire file lets a dead helper or `#[cfg(test)]` regression
    impersonate production wiring. Inline closures already live in `main`;
    named `.configure_app(configure)` callbacks are the only extra bodies the
    launch builder can reach without executing arbitrary app code here.
    """
    bodies = function_bodies(text)
    pending = ["main"]
    seen: set[str] = set()
    reached: list[str] = []
    while pending:
        name = pending.pop()
        if name in seen:
            continue
        seen.add(name)
        for body in bodies.get(name, []):
            reached.append(body)
            code = rust_code_only(body)
            pending.extend(
                callback
                for callback in re.findall(
                    r"\.(?:configure_app|live_handler)\s*\(\s*([A-Za-z_][A-Za-z0-9_]*)\s*\)",
                    code,
                )
                if callback in bodies and callback not in seen
            )
            pending.extend(
                called
                for called in re.findall(r"\b([a-z_][A-Za-z0-9_]*)\s*\(", code)
                if called in bodies and called not in seen
            )
    return "\n".join(reached)


def prepared_launch_registry_wired(text: str) -> bool:
    """Recognize the shared preparer's returned registry, not a discarded one.

    This deliberately accepts a bounded form: registration immediately followed
    by a returned LegacyLaunchInputs constructor, optionally inside a final
    block, and simple forwarding callbacks (including map_err). Merely reaching
    a helper that registers an unrelated registry must not satisfy the contract.
    """
    code = rust_code_only(text)
    bodies = function_bodies(code)
    pending = re.findall(
        r"\.prepare_legacy_with_args\s*\(\s*([A-Za-z_][A-Za-z0-9_]*)\s*\)",
        rust_code_only(live_launch_registration_code(text)),
    )
    seen: set[str] = set()
    returned_registry = re.compile(
        r"\bactions\s*::\s*register\s*\(\s*&mut\s+(?P<registry>[A-Za-z_][A-Za-z0-9_]*)\s*\)\s*;"
        r"\s*Ok\s*\(\s*(?:[A-Za-z_][A-Za-z0-9_]*\s*::\s*)*LegacyLaunchInputs\s*::\s*new"
        r"\s*\([^();{}]*\)\s*(?:\.with_records\s*\([^();{}]*\)\s*)?"
        r"\.with_registry\s*\(\s*(?P=registry)\s*\)\s*"
        r"(?:\.with_widget_owned_persistence\s*\(\s*\)\s*)?,?\s*\)\s*}*\s*$"
    )
    while pending:
        name = pending.pop()
        if name in seen:
            continue
        seen.add(name)
        for body in bodies.get(name, []):
            if returned_registry.search(body):
                return True
            forwarded = re.fullmatch(
                r"\s*([a-z_][A-Za-z0-9_]*)\s*\([^();{}]*\)"
                r"(?:\s*\.map_err\s*\([^;{}]*\))?\s*",
                body,
            )
            if forwarded:
                pending.append(forwarded.group(1))
    return False


def component_launch_registry_wired(main_text: str, crate_text: str) -> bool:
    """Recognize a registry installed by a canonical component factory.

    ``ApplicationDefinition`` owns the production factory in the component
    architecture, while ``LaunchSpec`` only supplies host inputs. Follow the
    definition's named create callback and require that the same registry
    passed to ``actions::register`` reaches ``AppComponent::new``. This keeps
    test-only or disconnected registration helpers from satisfying the gate.
    """
    main = rust_code_only(live_launch_registration_code(main_text))
    if not (
        re.search(r"(?:::|\.)from_definition\s*\(", main)
        and re.search(r"\.component_host\s*\(", main)
    ):
        return False

    code = rust_code_only(crate_text)
    bodies = function_bodies(code)
    pending = re.findall(
        r"\bcreate\s*:\s*\|[^|]*\|\s*([a-z_][A-Za-z0-9_]*)\s*\(", code
    )
    seen: set[str] = set()
    while pending:
        name = pending.pop()
        if name in seen:
            continue
        seen.add(name)
        for body in bodies.get(name, []):
            for registration in re.finditer(
                r"\bactions\s*::\s*register\s*\(\s*&mut\s+"
                r"(?P<registry>[A-Za-z_][A-Za-z0-9_]*)\s*\)\s*;",
                body,
            ):
                registry = re.escape(registration.group("registry"))
                tail = body[registration.end() :]
                if re.search(
                    rf"\bAppComponent\s*::\s*new\s*\([^;{{}}]*\b{registry}\b\s*\)",
                    tail,
                ):
                    return True
            pending.extend(
                called
                for called in re.findall(r"\b([a-z_][A-Za-z0-9_]*)\s*\(", body)
                if called in bodies and called not in seen
            )
    return False


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


def audit_search_provider_limits(
    root: Path, sources: list[tuple[Path, str]], violations: list[Violation]
) -> set[str]:
    """Require every app provider to own paging, work, and materialisation budgets."""
    found: set[str] = set()
    truncating_paths: set[str] = set()
    for source, raw in sources:
        code = rust_code_only(raw)
        relative = source.relative_to(root).as_posix()
        if TRUNCATING_SEARCH.search(code):
            truncating_paths.add(relative)
            if relative not in TRUNCATING_SEARCH_REMEDIATION:
                violations.append(
                    Violation(
                        source,
                        line_number(raw, TRUNCATING_SEARCH.search(code).start()),
                        "durable SearchProvider truncates its corpus before querying; "
                        "use the shared incremental SearchCatalog so late resources remain findable",
                    )
                )
        for match in SEARCH_PROVIDER_IMPL.finditer(code):
            provider = match.group(1)
            key = f"{relative}::{provider}"
            found.add(key)
            body = brace_body(code, match.start()) or ""
            limited = re.search(r"\bfn\s+search_limited\s*\(", body)
            if limited:
                limited_body = brace_body(body, limited.start()) or ""
                if re.search(r"\bself\s*\.\s*search\s*\(", limited_body):
                    violations.append(
                        Violation(
                            source,
                            line_number(raw, match.start() + limited.start()),
                            "search_limited calls unbounded search() before applying its budget",
                        )
                    )
            elif key not in SEARCH_LIMIT_EXEMPTIONS:
                violations.append(
                    Violation(
                        source,
                        line_number(raw, match.start()),
                        "app SearchProvider inherits the unbounded search_limited fallback; "
                        "implement CatalogSearchProvider for a catalogue-backed provider, or "
                        "override it and stop scanning/materialising at the caller budget",
                    )
                )

            paged = re.search(r"\bfn\s+search_page\s*\(", body)
            if paged:
                paged_body = brace_body(body, paged.start()) or ""
                if re.search(r"\bself\s*\.\s*(?:search|search_limited)\s*\(", paged_body):
                    violations.append(
                        Violation(
                            source,
                            line_number(raw, match.start() + paged.start()),
                            "search_page calls a materialising search fallback; query a retained "
                            "catalogue with the cursor, work budget, and result limit instead",
                        )
                    )
            elif key not in SEARCH_LIMIT_EXEMPTIONS:
                violations.append(
                    Violation(
                        source,
                        line_number(raw, match.start()),
                        "app SearchProvider inherits the non-paged search_page fallback; override "
                        "it so broad corpora continue under a hard per-page work budget",
                    )
                )
        for match in CATALOG_SEARCH_PROVIDER_IMPL.finditer(code):
            found.add(f"{relative}::{match.group(1)}")
            body = brace_body(code, match.start()) or ""
            catalog = re.search(r"\bfn\s+catalog\s*\(", body)
            catalog_body = brace_body(body, catalog.start()) if catalog else None
            if catalog_body and PER_QUERY_CATALOG_BUILD.search(catalog_body):
                violations.append(
                    Violation(
                        source,
                        line_number(raw, match.start() + catalog.start()),
                        "CatalogSearchProvider::catalog builds a catalogue per query; return "
                        "the retained catalogue and update it incrementally as the corpus changes",
                    )
                )
    if root.resolve() == ROOT.resolve():
        app_prefixes = {
            "/".join(source.relative_to(root).as_posix().split("/")[:2]) + "/"
            for source, _ in sources
        }
        scoped_remediation = {
            relative
            for relative in TRUNCATING_SEARCH_REMEDIATION
            if any(relative.startswith(prefix) for prefix in app_prefixes)
        }
        for relative in sorted(scoped_remediation - truncating_paths):
            violations.append(
                Violation(
                    root / relative,
                    1,
                    f"stale truncating-search remediation `{relative}`; remove the inventory entry",
                )
            )
    return found


def audit(root: Path = ROOT) -> tuple[list[Violation], int]:
    violations: list[Violation] = []
    # This is a Tilo framework invariant, not a requirement on the isolated
    # miniature app repositories used below and by third-party SDK consumers.
    # Only the authoritative workspace owns the App host implementation. Keep
    # this invariant spanning its focused implementation modules so moving
    # action/resource code out of app.rs cannot disable the audit.
    if root.resolve() == ROOT.resolve():
        app_host_path = root / "crates" / "sdk" / "ui-core" / "src" / "app.rs"
        app_action_path = app_host_path.parent / "app" / "actions.rs"
        app_host = rust_code_only(app_host_path.read_text(encoding="utf-8"))
        if app_action_path.is_file():
            app_host += "\n" + rust_code_only(app_action_path.read_text(encoding="utf-8"))
        if not FRAMEWORK_RESOURCE_SEARCH_BUILD.search(
            app_host
        ) or not FRAMEWORK_RESOURCE_SEARCH_INSTALL.search(app_host):
            violations.append(
                Violation(
                    app_host_path,
                    1,
                    "app host must install bounded canonical search for every live resource tree",
                )
            )
    ratchet_baseline = load_ratchet_baseline(root)
    baseline_path = root / BASELINE
    for rule, entries in ratchet_baseline.items():
        if rule not in RATCHET_REASONS:
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
            not (isinstance(facade, dict) and facade.get("package") == "tilo-app")
            for facade in facades
        ):
            violations.append(
                Violation(
                    manifest_path,
                    1,
                    "app must depend on the `tilo-app` SDK facade under the `ui` crate name",
                )
            )
        for alias, specification in dependencies:
            if is_ui_core_dependency(alias, specification):
                violations.append(
                    Violation(
                        manifest_path,
                        1,
                        f"direct ui-core dependency `{alias}` bypasses the `tilo-app` SDK facade",
                    )
                )

        sources: list[tuple[Path, str]] = []
        for source in sorted((manifest_path.parent / "src").rglob("*.rs")):
            sources.append((source, source.read_text(encoding="utf-8")))
        combined_raw = "\n".join(text for _, text in sources)
        combined = rust_code_only(combined_raw)
        main_path = manifest_path.parent / "src" / "main.rs"
        main_raw = main_path.read_text(encoding="utf-8") if main_path.is_file() else ""
        main_code = rust_code_only(live_launch_registration_code(main_raw))
        audit_ratchets(root, sources, ratchet_baseline, violations)
        found_providers = audit_search_provider_limits(root, sources, violations)
        for key in SEARCH_LIMIT_EXEMPTIONS:
            if key.startswith(f"apps/{manifest_path.parent.name}/") and key not in found_providers:
                violations.append(
                    Violation(
                        manifest_path,
                        1,
                        f"stale SearchProvider limit exemption `{key}`",
                    )
                )
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
                        "headless agent must consume the generic action catalogue through remote-action messages",
                    )
                )
            if not BROKER_ACTION_INVOKER.search(combined) or not BROKER_ACTION_STATUS.search(combined):
                violations.append(
                    Violation(
                        manifest_path,
                        1,
                        "headless agent must invoke and observe actions through remote-action messages",
                    )
                )
        else:
            framework_host = (
                FRAMEWORK_APP.search(combined) is not None
                and FRAMEWORK_RESOURCE.search(combined) is not None
            )
            toolkit_host = TOOLKIT_ACTION_HOST.search(combined) is not None
            # A host with several operations builds its descriptor directly;
            # the toolkit helpers each describe a single operation.
            toolkit_resource = TOOLKIT_RESOURCE_VALUE.search(combined) is not None or (
                RESOURCE_VALUE.search(combined) is not None
                and ACTION_SPEC.search(combined) is not None
            )
            if toolkit_host and not toolkit_resource:
                violations.append(
                    Violation(
                        manifest_path,
                        1,
                        "toolkit ResourceActionHost has no toolkit resource descriptor",
                    )
                )
            if not toolkit_host and not framework_host and not RESOURCE_PROVIDER.search(combined):
                violations.append(
                    Violation(
                        manifest_path,
                        1,
                        "app does not implement Widget::resources",
                    )
                )
            if not toolkit_host and not framework_host and not ACTION_SPEC.search(combined):
                violations.append(
                    Violation(
                        manifest_path,
                        1,
                        "app exposes no ActionSpec operations",
                    )
                )
            if not toolkit_host and not framework_host and not RESOURCE_VALUE.search(combined):
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
                        "effect/view actions require the canonical Widget::start_operation handler",
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
            # A document ActionSpec is only valid at runtime when its decoder
            # is installed into the App's ActionRegistry. Looking for a
            # register function anywhere in the crate used to let test-only
            # calls satisfy this gate while the production LaunchSpec silently
            # withdrew the complete resource catalogue as invalid.
            if (
                ACTION_REGISTRY_DEFINITION.search(combined)
                and not LIVE_ACTION_REGISTRY_WIRING.search(main_code)
                and not prepared_launch_registry_wired(main_raw)
                and not component_launch_registry_wired(main_raw, combined_raw)
                and not (framework_host and FRAMEWORK_LAUNCH.search(combined))
            ):
                violations.append(
                    Violation(
                        main_path if main_path.is_file() else manifest_path,
                        1,
                        "document action registry is not wired into the production app launch",
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
