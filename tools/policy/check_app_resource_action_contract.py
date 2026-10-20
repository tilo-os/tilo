#!/usr/bin/env python3
"""Audit Tilo applications' resource/action model.

Every graphical app must publish its current resources from the widget tree and
describe its operations with ``ActionSpec``. The headless agent is the one
application-model consumer: it surveys peer resources and invokes their action
descriptors. A compute package (a ``service`` whose ``cli`` entrypoint a
compute worker runs for another app's action) publishes nothing either: the
action is the requesting app's. Source rules reject bespoke command buses and
applications without an OS-visible surface; the runtime ``--native-audit``
verifies the live accessibility/resource/control inventory.
"""

from __future__ import annotations

import re
import sys
import tomllib
from dataclasses import dataclass
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
from rust_source import blank_test_items, code_only, line_number  # noqa: E402


ROOT = Path(__file__).resolve().parents[2]
# Audited ceilings, one file per app (`<app>.toml`, `[ratchets.<rule>]` tables
# of `"apps/<app>/<path>" = <count>`), so changes to different apps never edit
# the same file. Rules absent from every file allow nothing; adding an entry is
# a certification exception requiring architectural review. A file whose debt
# is gone is deleted.
BASELINE = Path("tools/policy/app_resource_action_baseline")

RESOURCE_PROVIDER = re.compile(r"\bfn\s+resources\s*\(")
RESOURCE_VALUE = re.compile(r"\bResource\s*::\s*new\s*\(")
ACTION_SPEC = re.compile(r"\bActionSpec\s*::\s*(?:immediate|prompted|typed|operation)")
OPERATION_KIND = re.compile(r"\bOperationKind\s*::\s*(?:Effect|View)\b")
OPERATION_HANDLER = re.compile(r"\bfn\s+start_operation\s*\(")
OPERATION_HANDLER_REGISTRY = re.compile(
    r"\bfn\s+local_resource_operation_handlers\s*\("
)
ACTION_REGISTRY_DEFINITION = re.compile(
    r"\bfn\s+register\s*\([^)]*\bActionRegistry\b", re.DOTALL
)
LIVE_ACTION_REGISTRY_WIRING = re.compile(r"\bactions\s*::\s*register\s*\(")
TOOLKIT_ACTION_HOST = re.compile(r"\bresource_actions\s*::\s*immediate\s*\(")
TOOLKIT_RESOURCE_VALUE = re.compile(
    r"\bresource_actions\s*::\s*(?:resource|typed_resource)\s*\("
)
# A `tilo-app` framework app declares a resource type and hosts it through
# `ResourceApp`; the framework publishes that resource's actions (named
# declarations plus the typed `apply` door), builds its runtime Resource value
# and installs its replay decoder in the component registry.
FRAMEWORK_APP = re.compile(
    r"\bimpl\s+(?:[A-Za-z_][A-Za-z0-9_]*\s*::\s*)*ResourceApp\s+for\s+"
    r"[A-Za-z_][A-Za-z0-9_]*\b(?!\s*::)"
)
# The framework's launch installs the resource's replay decoder
# (`Resource::register`) into the component registry itself, whether the
# binary calls it or `ui::main!` expands to it.
FRAMEWORK_LAUNCH = re.compile(
    r"\bframework\s*::\s*(?:run|launch_spec)\s*::\s*<|\b\w+\s*::\s*main\s*!\s*\("
)
# A framework app answers its effect and view operations as a typed
# vocabulary: a surface's `ResourceSurface::effect`, a view's `cx.effects(..)`.
TYPED_EFFECT_HANDLER = re.compile(r"\bfn\s+effect\s*\(|\.\s*effects\s*\(\s*(?:move\s*)?\|")
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

# The raw operation protocol between the host and the toolkit's own widgets.
# An app answers operations as a typed effect vocabulary and, in a test that
# drives a widget without a host, delivers one through
# `ui::framework::HostDelivery`; it never
# builds, routes or answers a raw operation, in its sources or its tests.
RAW_OPERATION_PROTOCOL = re.compile(
    r"\b(?:ResourceOperation|ResourceOperationStart|OperationRequest|start_in_children|host_operation)\b"
    r"|(?:\.|::)\s*(?:start_operation|poll_resource_operation)\s*\("
)

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
    "widget_search_source": (
        re.compile(r"\bSearchSource\s+for\b"),
        "a content widget's own search provider beside the app's one search source; publish "
        "SearchEntry values from Resource::search or ResourceSurface::search_entries",
    ),
}

# The one resource/action/view model (docs/architecture/README.md): an app's
# edits go through the framework `Dispatcher`, it launches as a `ResourceApp`,
# its durable state is the resource's document, and nothing beside
# `Action::apply` writes that document. These count production code only (test
# files and `#[cfg(test)]` items are exempt) and may only go down.
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
# Counted over every line of a workspace app's sources but its test files: a
# hook a widget implements anywhere in a file is the widget's.
WORKSPACE_RATCHETS = {
    "raw_operation_handler": (
        re.compile(
            r"\bfn\s+(?:start_operation|poll_resource_operation|local_resource_operation_handlers)\s*\("
        ),
        "raw resource-operation hook; answer a typed #[derive(Effect)] vocabulary in "
        "ResourceSurface::effect (or ViewCx::effects) and offer item effects bound on their rows",
    ),
}
# The app-facing APIs the model replaced (docs/architecture/resources.md §8):
# an app declares its record with `#[derive(Model)]` and `#[resource]`, its
# operations as `#[action]` verbs with typed params and `#[derive(Effect)]`
# vocabularies, and changes its document only through field edits, verbs and
# (in migrations and old-shape fixtures) the model's storage helpers. None of
# the old ways is left in an app or an example, so each is refused outright,
# in every source file of an app crate (`src`, `tests`, `examples`, `benches`)
# and of an example (`src`, `tests`): a test reaches an app the way an agent
# does.
LEGACY_API_BANS = {
    "hand_resource_impl": (
        re.compile(r"\bimpl\b(?:\s*<[^>{}]*>)?\s+(?:[A-Za-z_][A-Za-z0-9_]*\s*::\s*)*Resource\s+for\b"),
        "hand-written `impl Resource for`; derive the record with #[derive(Model)] and #[resource]",
    ),
    "action_decl_new": (
        re.compile(r"\bActionDecl\s*::\s*new\s*\("),
        "hand-built ActionDecl; declare the operation as an #[action] verb",
    ),
    "action_address_new": (
        re.compile(r"\bActionAddress\s*::\s*new\s*\("),
        "hand-built ActionAddress; name the action through its typed reference "
        "(`R::VERB.on(..)`, `effect.address_on(..)`, `invoke.address()`), take it from "
        "what is published (`resource.action_address(..)`, `door::published`), or invoke "
        "it through the door (`call_on`)",
    ),
    "untyped_action_args": (
        re.compile(r"\bargs\s*\.\s*(?:text|get|boolean|integer|number|array)\s*\("),
        "untyped argument parsing; give the verb typed params, or read the bound "
        "arguments into a serde struct",
    ),
    "action_spec": (
        re.compile(r"\bActionSpec\s*::"),
        "hand-built ActionSpec; publish the operation as an #[action] verb, an #[effect], "
        "or a registered read (`semantic_document_read::read_action`)",
    ),
    "direct_automerge_write": (
        re.compile(
            r"\.\s*(?:put|put_object|insert_object|splice_text|update_text|increment)\s*\(",
        ),
        "direct Automerge write; change the document with a field edit or a verb, and write "
        "a migration or an old-shape fixture with the model's helpers (`write_key`, `Slot`, "
        "`evolution::fixture`)",
    ),
    "json_text_field": (
        re.compile(r"\bJson\s*<"),
        "a value kept as one JSON string; give the field its model type (a struct or enum "
        "with #[derive(Model)], `Option`, `Vec`, a map), or `serde_json::Value` for JSON "
        "another party shapes, and migrate stored ones with `evolution::retype_json`",
    ),
    "value_model_in_app": (
        re.compile(r"\bvalue_model\s*!"),
        "a type the app shapes derives Model, so its fields have paths, edits and history "
        "of their own; `value_model!` is for types below the resource layer",
    ),
}
# The apps that act on other apps' published actions or publish their own to
# the broker as data, not through a resource of their own: the agent plans and
# invokes whatever the catalogue offers, Shortcuts stores and runs steps of
# it, and the web search service publishes its two actions straight to the
# broker. An address is their data, so they build one; every other old API is
# refused for them as for any app.
PROTOCOL_ENDPOINTS: dict[str, str] = {
    "agent": "plans and invokes the actions the broker's catalogue offers",
    "shortcuts": "stores and runs steps that name other apps' published actions",
    "web-search": "a headless service that publishes its actions to the broker",
}
# Which matches of a legacy rule are the old API, given the file's code and
# its raw text (same offsets): a `put` on a content store, a secret store or a
# cache is no document write, and `args.get(1)` on a binary's own command line
# is no action argument.
DOCUMENT_TYPE = (
    r"(?:[A-Za-z_]\w*\s*::\s*)*(?:AutoCommit|Automerge|Doc|Transaction)\b"
    r"|(?:impl|dyn)\s+(?:[A-Za-z_]\w*\s*::\s*)*Transactable\b"
)
TRANSACTABLE_GENERIC = re.compile(
    r"\b([A-Z]\w*)\s*:\s*(?:\?Sized\s*\+\s*)?(?:[A-Za-z_]\w*\s*::\s*)*Transactable\b"
)
DOCUMENT_LET = re.compile(
    r"\blet\s+(?:mut\s+)?([a-z_]\w*)\s*(?::[^=;]*)?=\s*(?:"
    r"(?:[A-Za-z_]\w*\s*::\s*)*(?:AutoCommit|Automerge)\s*::"
    r"|[\w.]*\s*\.\s*(?:transaction|transaction_at|fork|fork_at)\s*\()"
)
# A receiver with one of these names is the document even where its type is
# inferred (a closure's parameter).
DOCUMENT_NAMES = {"doc", "tx", "txn", "transaction"}
AUTOMERGE_FILE = re.compile(r"\bautomerge\b|\bAutoCommit\b|\bObjId\b|\bTransactable\b|\bROOT\b")
# A `put` into the document's root is a document write whatever its receiver.
AUTOMERGE_OBJECT_ARG = re.compile(r"\s*&?\s*(?:automerge\s*::\s*)?ROOT\b")
ACTION_ARGS_DECL = re.compile(
    r"\bargs\s*:\s*&?\s*(?:mut\s+)?(?:[A-Za-z_]\w*\s*::\s*)*(?:ActionArgs|Value|Map)\b"
)


def document_receivers(code: str) -> set[str]:
    """The names this file gives a document or a transaction on one."""
    types = DOCUMENT_TYPE + "".join(
        rf"|{re.escape(name)}\b" for name in sorted(set(TRANSACTABLE_GENERIC.findall(code)))
    )
    declared = re.compile(
        rf"\b([a-z_]\w*)\s*:\s*&?\s*(?:'\w+\s+)?(?:mut\s+)?(?:{types})"
    )
    names = set(declared.findall(code)) | set(DOCUMENT_LET.findall(code))
    if AUTOMERGE_FILE.search(code):
        names |= DOCUMENT_NAMES
    return names


def receiver(code: str, dot: int) -> str:
    """The name a method call at `dot` is made on: the variable or field,
    or the function whose result it is."""
    i = dot - 1
    while i >= 0 and code[i].isspace():
        i -= 1
    if i >= 0 and code[i] == ")":
        depth = 0
        while i >= 0:
            if code[i] == ")":
                depth += 1
            elif code[i] == "(":
                depth -= 1
                if depth == 0:
                    break
            i -= 1
        i -= 1
        while i >= 0 and code[i].isspace():
            i -= 1
    end = i + 1
    while i >= 0 and (code[i].isalnum() or code[i] == "_"):
        i -= 1
    return code[i + 1 : end]


def document_writes(code: str, raw: str, matches: list[re.Match]) -> list[re.Match]:
    """Writes into `ROOT`, or on a receiver this file types or names as a
    document, in a file that reaches Automerge. A content store's, a secret
    store's or a cache's `put` is no document write, nor is an edit through
    the rich-text model's own `DocMut` or an app's typed test builder."""
    if not matches or not AUTOMERGE_FILE.search(code):
        return []
    names = document_receivers(code)
    return [
        match
        for match in matches
        if AUTOMERGE_OBJECT_ARG.match(code, match.end()) is not None
        or (name := receiver(code, match.start())) in names
        or re.fullmatch(r"(?:doc|document|transaction)\w*", name) is not None
    ]


def action_arguments(code: str, raw: str, matches: list[re.Match]) -> list[re.Match]:
    """Every ActionArgs accessor; `get` only on arguments declared as
    ActionArgs or JSON, or by name in a file that reads ActionArgs: not a
    command line's flags or index, nor a stored step's bindings."""
    if not matches:
        return matches
    typed = ACTION_ARGS_DECL.search(code) is not None
    reads_action_args = re.search(r"\bActionArgs\b", code) is not None
    kept = []
    for match in matches:
        if not match.group().rstrip("( \t\n").endswith("get"):
            kept.append(match)
            continue
        after = raw[match.end() :].lstrip()
        if typed or (reads_action_args and after.startswith('"')):
            kept.append(match)
    return kept


LEGACY_API_BAN_FILTERS = {
    "direct_automerge_write": document_writes,
    "untyped_action_args": action_arguments,
}
# Ratchets that reached zero and stay there: a ceiling for one is refused.
CLOSED_RATCHETS = {"raw_operation_handler"}
RATCHET_REASONS = {
    **{rule: reason for rule, (_, reason) in RATCHETS.items()},
    **{rule: reason for rule, (_, reason) in MODEL_RATCHETS.items()},
    **{rule: reason for rule, (_, reason) in WORKSPACE_RATCHETS.items()},
}


def is_test_source(relative: str) -> bool:
    name = relative.rsplit("/", 1)[-1]
    return (
        "/tests/" in relative
        or name in {"tests.rs", "test_support.rs", "testing.rs"}
        or name.endswith("_tests.rs")
        or name.startswith("test_")
    )


def production_code(text: str) -> str:
    """Code without comments, literals or test-only items."""
    return blank_test_items(code_only(text))


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


def is_compute_package(app_dir: Path) -> bool:
    """A windowless package whose `[cli]` commands a compute worker runs.

    Its `tilo-app.toml` declares `kind = "service"`, a `cli` entrypoint and
    at least one `[cli]` command. It publishes no resource or action of its
    own: the action that runs it belongs to the app that requested the job,
    which runs the same code locally when no worker takes it.
    """
    manifest_path = app_dir / "tilo-app.toml"
    if not manifest_path.is_file():
        return False
    with manifest_path.open("rb") as handle:
        manifest = tomllib.load(handle)
    package = manifest.get("package", {})
    entrypoint = manifest.get("entrypoint", {})
    commands = manifest.get("cli", {}).get("commands", [])
    return (
        package.get("kind") == "service"
        and entrypoint.get("type") == "cli"
        and isinstance(commands, list)
        and bool(commands)
    )


def is_ui_core_dependency(alias: str, specification: object) -> bool:
    if package_name(alias, specification) == "ui":
        return True
    if isinstance(specification, dict):
        path = specification.get("path")
        return isinstance(path, str) and Path(path).name in {"ui", "ui-core"}
    return False


def baseline_file(relative: str) -> Path:
    """The per-app baseline file that holds `relative`'s ceilings."""
    parts = relative.split("/")
    if len(parts) > 2 and parts[0] == "apps":
        owner = parts[1]
    elif len(parts) > 3 and parts[:2] == ["examples", "apps"]:
        owner = f"example-{parts[2]}"
    else:
        owner = "workspace"
    return BASELINE / f"{owner}.toml"


def load_ratchet_baseline(
    root: Path, violations: list[Violation] | None = None
) -> dict[str, dict[str, int]]:
    baseline: dict[str, dict[str, int]] = {}
    for path in sorted((root / BASELINE).glob("*.toml")):
        with path.open("rb") as handle:
            raw = tomllib.load(handle).get("ratchets", {})
        if not isinstance(raw, dict):
            continue
        for rule, entries in raw.items():
            if not isinstance(entries, dict):
                continue
            for relative, limit in entries.items():
                if not (isinstance(relative, str) and isinstance(limit, int) and limit >= 0):
                    continue
                if violations is not None and rule in CLOSED_RATCHETS:
                    violations.append(
                        Violation(path, 1, f"{rule} is closed: no ceiling for `{relative}`")
                    )
                    continue
                if violations is not None and root / baseline_file(relative) != path:
                    violations.append(
                        Violation(path, 1, f"{rule} ceiling for `{relative}` belongs in {baseline_file(relative)}")
                    )
                baseline.setdefault(rule, {})[relative] = limit
    return baseline


def audit_ratchets(
    root: Path,
    sources: list[tuple[Path, str]],
    baseline: dict[str, dict[str, int]],
    violations: list[Violation],
    *,
    workspace: bool | None = None,
) -> None:
    if workspace is None:
        workspace = root.resolve() == ROOT.resolve()
    for source, source_text in sources:
        relative = source.relative_to(root).as_posix()
        code = code_only(source_text)
        rules = [(rule, pattern, reason, code) for rule, (pattern, reason) in RATCHETS.items()]
        # A Tilo workspace invariant, like the app host check in `audit`; the
        # miniature SDK apps the policy tests build are not held to it.
        if workspace:
            production = "" if is_test_source(relative) else production_code(source_text)
            rules += [
                (rule, pattern, reason, production)
                for rule, (pattern, reason) in MODEL_RATCHETS.items()
            ]
            shipped = "" if is_test_source(relative) else code
            rules += [
                (rule, pattern, reason, shipped)
                for rule, (pattern, reason) in WORKSPACE_RATCHETS.items()
            ]
        parts = relative.split("/")
        if len(parts) > 2 and parts[0] == "apps" and dynamic_resource_host(root / "apps" / parts[1]):
            rules = [entry for entry in rules if entry[0] not in DYNAMIC_HOST_RULES]
        for rule, pattern, reason, code in rules:
            matches = list(pattern.finditer(code))
            limit = baseline.get(rule, {}).get(relative, 0)
            if len(matches) <= limit:
                if len(matches) < limit:
                    violations.append(
                        Violation(
                            root / baseline_file(relative),
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
                    f"{reason} ({len(matches)} found, audited ceiling {limit} in {baseline_file(relative)})",
                )
            )


def legacy_api_sources(owner: Path, directories: tuple[str, ...]) -> list[tuple[Path, str]]:
    """Every Rust source under `directories` of the crate at `owner`."""
    sources = []
    for directory in directories:
        for source in sorted((owner / directory).rglob("*.rs")):
            sources.append((source, source.read_text(encoding="utf-8")))
    return sources


def audit_legacy_apis(
    sources: list[tuple[Path, str]],
    violations: list[Violation],
    *,
    endpoint: bool = False,
) -> None:
    """Refuse every use of an API the model replaced in `sources`; an
    address in a protocol endpoint's (`endpoint`) is its data."""
    for source, source_text in sources:
        code = code_only(source_text)
        for rule, (pattern, reason) in LEGACY_API_BANS.items():
            if endpoint and rule == "action_address_new":
                continue
            matches = list(pattern.finditer(code))
            if rule in LEGACY_API_BAN_FILTERS:
                matches = LEGACY_API_BAN_FILTERS[rule](code, source_text, matches)
            for match in matches:
                violations.append(
                    Violation(source, line_number(source_text, match.start()), reason)
                )


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
                code_only(body),
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
            code = code_only(body)
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
    code = code_only(text)
    bodies = function_bodies(code)
    pending = re.findall(
        r"\.prepare_legacy_with_args\s*\(\s*([A-Za-z_][A-Za-z0-9_]*)\s*\)",
        code_only(live_launch_registration_code(text)),
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
    main = code_only(live_launch_registration_code(main_text))
    if not (
        re.search(r"(?:::|\.)from_definition\s*\(", main)
        and re.search(r"\.component_host\s*\(", main)
    ):
        return False

    code = code_only(crate_text)
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


SHARED_REEXPORT = re.compile(r"\bpub\s+use\s+([A-Za-z_][A-Za-z0-9_]*)::([^;]+);")
PUBLIC_USE = re.compile(r"\bpub\s+use\s+([^;]+);")
TOP_LEVEL_DEFINITION = re.compile(r"\bpub\s+fn\s+definition\s*\(\s*\)")
DEFINITION_BUILT_IN = re.compile(
    r"(?:(?:crate|self|super|[A-Za-z_][A-Za-z0-9_]*)\s*::\s*)*"
    r"built_in_definition\s*!\s*\(\s*"
    r"(?P<app>(?:[A-Za-z_][A-Za-z0-9_]*\s*::\s*)*[A-Za-z_][A-Za-z0-9_]*)\s*\)"
)
DEFINITION_FRAMEWORK = re.compile(
    r"(?:(?:crate|self|super|[A-Za-z_][A-Za-z0-9_]*)\s*::\s*)*"
    r"framework\s*::\s*definition\s*::\s*<\s*"
    r"(?P<app>(?:[A-Za-z_][A-Za-z0-9_]*\s*::\s*)*[A-Za-z_][A-Za-z0-9_]*)\s*>\s*\(\s*\)"
)
DEFINITION_DELEGATE = re.compile(
    r"(?P<alias>[A-Za-z_][A-Za-z0-9_]*)\s*::\s*definition\s*\(\s*\)"
)
RESOURCE_APP_IMPL = re.compile(
    r"\bimpl(?:\s*<[^>{}]*>)?\s+(?:[A-Za-z_][A-Za-z0-9_]*\s*::\s*)*ResourceApp\s+for\s+"
    r"((?:[A-Za-z_][A-Za-z0-9_]*\s*::\s*)*[A-Za-z_][A-Za-z0-9_]*)"
)
ASSOCIATED_RESOURCE = re.compile(
    r"\btype\s+Resource\s*=\s*"
    r"((?:[A-Za-z_][A-Za-z0-9_]*\s*::\s*)*[A-Za-z_][A-Za-z0-9_]*)\s*;"
)
RESOURCE_ATTRIBUTE = re.compile(
    r"#\[\s*(?:[A-Za-z_][A-Za-z0-9_]*\s*::\s*)*resource\s*\([^\]]*\)\s*\]"
    r"(?:\s*#\[[^\]]*\])*"
    r"\s*(?:pub(?:\s*\([^)]*\))?\s+)?(?:struct|enum|impl)\s+"
    r"([A-Za-z_][A-Za-z0-9_]*)\b(?!\s*::)(?!\s+for\b)"
)
RESOURCE_IMPL = re.compile(
    r"\bimpl(?:\s*<[^>{}]*>)?\s+(?:[A-Za-z_][A-Za-z0-9_]*\s*::\s*)*Resource\s+for\s+"
    r"([A-Za-z_][A-Za-z0-9_]*)\b(?!\s*::)"
)


def rust_type_name(path: str) -> str:
    """The last segment of a Rust type path, without whitespace."""
    return re.sub(r"\s+", "", path).rsplit("::", 1)[-1]


def app_reexports(code: str) -> dict[str, tuple[set[str], bool]]:
    """Items the app re-exports from each direct dependency alias."""
    exported: dict[str, tuple[set[str], bool]] = {}
    for match in SHARED_REEXPORT.finditer(code):
        alias, rest = match.group(1), " ".join(match.group(2).split())
        names, glob = exported.get(alias, (set(), False))
        items = rest[1:-1].split(",") if rest.startswith("{") and rest.endswith("}") else [rest]
        for item in items:
            item = item.split(" as ", 1)[0].strip()
            if item == "*":
                glob = True
            elif item and item != "self":
                names.add(rust_type_name(item))
        exported[alias] = (names, glob)
    return exported


MODULE_DECLARATION = re.compile(
    r"(?P<attributes>(?:\s*#!?\[[^\]]*\]\s*)*)"
    r"\b(?:pub(?:\s*\([^)]*\))?\s+)?mod\s+(?P<name>[A-Za-z_][A-Za-z0-9_]*)\s*;"
)
PATH_ATTRIBUTE = re.compile(r'#\[\s*path\s*=\s*"([^"]+)"\s*\]')
TEST_MODULE_ATTRIBUTE = re.compile(r"cfg\s*\(\s*(?:test|all\s*\(\s*test\b)")


def test_only_module_files(dependency: Path) -> set[Path]:
    """Files a dependency declares only behind `#[cfg(test)] mod ...;`."""
    excluded: set[Path] = set()
    for source in sorted((dependency / "src").rglob("*.rs")):
        raw = source.read_text(encoding="utf-8")
        for match in MODULE_DECLARATION.finditer(raw):
            attributes = match.group("attributes")
            if not TEST_MODULE_ATTRIBUTE.search(attributes):
                continue
            name = match.group("name")
            path = PATH_ATTRIBUTE.search(attributes)
            if path is not None:
                excluded.add((source.parent / path.group(1)).resolve())
                continue
            stem = source.stem
            parent = source.parent if stem in {"lib", "main", "mod"} else source.parent / stem
            excluded.add((parent / f"{name}.rs").resolve())
            excluded.add((parent / name / "mod.rs").resolve())
    return excluded


def definition_tail(body: str) -> str:
    """The expression a `definition()` returns, without a trailing semicolon."""
    tail = body.strip()
    if tail.endswith(";"):
        tail = tail[:-1].rstrip()
    return tail


def definition_app_path(body: str) -> str | None:
    """The `ResourceApp` path a canonical production `definition()` builds."""
    tail = definition_tail(body)
    for pattern in (DEFINITION_BUILT_IN, DEFINITION_FRAMEWORK):
        match = pattern.fullmatch(tail)
        if match is not None:
            return re.sub(r"\s+", "", match.group("app"))
    return None


def definition_delegate(body: str) -> str | None:
    """The crate alias a canonical production `definition()` delegates to."""
    match = DEFINITION_DELEGATE.fullmatch(definition_tail(body))
    return match.group("alias") if match is not None else None


def brace_depth(code: str, offset: int) -> int:
    """The brace nesting depth at `offset` in lexed Rust code."""
    depth = 0
    for char in code[:offset]:
        if char == "{":
            depth += 1
        elif char == "}":
            depth -= 1
    return depth


def top_level_definition_body(code: str) -> str | None:
    """The body of the crate root's one `pub fn definition()`."""
    matches = [
        match
        for match in TOP_LEVEL_DEFINITION.finditer(code)
        if brace_depth(code, match.start()) == 0
    ]
    if len(matches) != 1:
        return None
    return brace_body(code, matches[0].start())


def reexported_definition_module(items: str) -> list[str] | None:
    """The module a `pub use` re-exports `definition` from, if it does."""
    match = re.fullmatch(
        r"(?P<path>(?:[A-Za-z_][A-Za-z0-9_]*\s*::\s*)*[A-Za-z_][A-Za-z0-9_]*)"
        r"\s*::\s*(?P<items>\{[^}]*\}|[A-Za-z_][A-Za-z0-9_]*)",
        " ".join(items.split()),
    )
    if match is None:
        return None
    item = match.group("items")
    names = (
        [part.strip() for part in item[1:-1].split(",")]
        if item.startswith("{")
        else [item]
    )
    if "definition" not in names:
        return None
    path = [part for part in re.split(r"\s*::\s*", match.group("path")) if part]
    if path and path[0] in {"crate", "self"}:
        path = path[1:]
    if any(part == "super" for part in path):
        return None
    return path


def module_source(crate_dir: Path, segments: list[str]) -> Path | None:
    """The one source file a module path names, if it exists unambiguously."""
    source = crate_dir / "src"
    if not segments:
        root = source / "lib.rs"
        return root if root.is_file() else None
    base = source.joinpath(*segments)
    candidates = [
        candidate
        for candidate in (base.with_suffix(".rs"), base / "mod.rs")
        if candidate.is_file()
    ]
    return candidates[0] if len(candidates) == 1 else None


def module_segments(source: Path, crate_dir: Path) -> list[str] | None:
    """The module path of `source` from the crate root."""
    try:
        relative = source.resolve().relative_to((crate_dir / "src").resolve())
    except ValueError:
        return None
    if relative.name in {"lib.rs", "main.rs"}:
        return [] if relative.parent == Path() else None
    if relative.name == "mod.rs":
        return list(relative.parent.parts)
    if relative.suffix != ".rs":
        return None
    return list(relative.with_suffix("").parts)


def split_path(path: str) -> list[str]:
    """The identifiers in a Rust path, without whitespace."""
    return [part for part in re.split(r"\s*::\s*", path.strip()) if part]


def module_path_source(
    crate_dir: Path, source: Path, segments: list[str]
) -> Path | None:
    """The module file a path written in `source` names.

    A path is qualified from the crate root (`crate`), the current module
    (`self`), its parents (`super`) or, for the plain form, either a child of
    the current module or the crate root. Two different resolutions are
    ambiguous: the caller gets nothing rather than one of them.
    """
    if not segments:
        return source
    base = module_segments(source, crate_dir)
    if base is None:
        return None
    head = segments[0]
    if head == "crate":
        return module_source(crate_dir, segments[1:])
    if head == "self":
        return module_source(crate_dir, base + segments[1:])
    if head == "super":
        rest = list(segments)
        while rest and rest[0] == "super":
            if not base:
                return None
            base = base[:-1]
            rest = rest[1:]
        return module_source(crate_dir, base + rest)
    local = module_source(crate_dir, base + segments)
    absolute = module_source(crate_dir, segments)
    if local is not None and absolute is not None:
        if local.resolve() != absolute.resolve():
            return None
    return local or absolute


USE_DECLARATION = re.compile(
    r"(?m)^[ \t]*(?:pub(?:\s*\([^)]*\))?\s+)?use\s+([^;]+);"
)


def use_origins(code: str) -> dict[str, list[list[str]]]:
    """The target paths a module's top-level `use` declarations give each name."""
    origins: dict[str, list[list[str]]] = {}
    for match in USE_DECLARATION.finditer(code):
        if brace_depth(code, match.start()) != 0:
            continue
        expression = " ".join(match.group(1).split()).lstrip(":")
        prefix: list[str] = []
        if "{" in expression:
            if not expression.endswith("}"):
                continue
            head, group = expression.split("{", 1)
            prefix = split_path(head.rstrip(":"))
            items = group[:-1].split(",")
        else:
            items = [expression]
        for item in items:
            item = item.strip()
            if not item or item in {"self", "*"}:
                continue
            local = None
            if " as " in item:
                item, local = [part.strip() for part in item.split(" as ", 1)]
            target = split_path(item)
            if not target:
                continue
            origins.setdefault(local or target[-1], []).append(prefix + target)
    return origins


def imported_type(
    crate_dir: Path, source: Path, name: str
) -> tuple[Path, str] | None:
    """The one shipping type a top-level import names, in its own module."""
    code = production_code(source.read_text(encoding="utf-8"))
    origins = use_origins(code).get(name, [])
    if len(origins) != 1 or not origins[0]:
        return None
    target = origins[0]
    module = module_path_source(crate_dir, source, target[:-1])
    return (module, target[-1]) if module is not None else None


def definition_host_source(
    crate_dir: Path, definition_source: Path, app_path: str
) -> tuple[Path, str] | None:
    """The module and type a definition's built host names, if unambiguous."""
    segments = split_path(app_path)
    if not segments:
        return None
    if len(segments) == 1:
        imported = imported_type(crate_dir, definition_source, segments[0])
        return imported or (definition_source, segments[0])
    module = module_path_source(crate_dir, definition_source, segments[:-1])
    return (module, segments[-1]) if module is not None else None


def resource_provider_source(
    crate_dir: Path, host_source: Path, resource_path: str
) -> tuple[Path, str] | None:
    """The module and type an associated resource names, if unambiguous."""
    segments = split_path(resource_path)
    if not segments:
        return None
    if len(segments) == 1:
        imported = imported_type(crate_dir, host_source, segments[0])
        return imported or (host_source, segments[0])
    module = module_path_source(crate_dir, host_source, segments[:-1])
    return (module, segments[-1]) if module is not None else None


def dependency_exports_host(
    crate_dir: Path,
    names: set[str],
    host_source: Path,
    host_type: str,
) -> bool | None:
    """Whether a dependency root's explicit re-export names the built host.

    `None` means none of `names` is re-exported, so the host may be declared
    in the root itself. `False` means every explicit re-export with one of
    those names resolves to another type.
    """
    root = crate_dir / "src" / "lib.rs"
    if not root.is_file():
        return False
    origins = use_origins(production_code(root.read_text(encoding="utf-8")))
    explicit = [name for name in names if name in origins]
    if not explicit:
        return None
    for name in explicit:
        for target in origins[name]:
            if not target:
                continue
            module = module_path_source(crate_dir, root, target[:-1])
            if (
                module is not None
                and module.resolve() == host_source.resolve()
                and target[-1] == host_type
            ):
                return True
    return False


def shipping_source(source: Path, dependency: Path, excluded: set[Path]) -> bool:
    """Whether `source` is production code of `dependency`."""
    try:
        relative = source.resolve().relative_to(dependency.resolve()).as_posix()
    except ValueError:
        return False
    return not is_test_source(relative) and source.resolve() not in excluded


def top_level_resource_app_impls(code: str, app_type: str) -> list[re.Match]:
    """The `ResourceApp` impls whose target is exactly the local `app_type`.

    A qualified target names another type, even when its last segment matches,
    so it does not certify this module's type.
    """
    return [
        match
        for match in RESOURCE_APP_IMPL.finditer(code)
        if brace_depth(code, match.start()) == 0
        and split_path(match.group(1)) == [app_type]
    ]


def declared_resource_names(code: str) -> set[str]:
    """Plain target types this module itself declares as a framework resource.

    A qualified target is not a canonical local declaration, so it certifies
    no type in this module.
    """
    names: set[str] = set()
    for pattern in (RESOURCE_IMPL, RESOURCE_ATTRIBUTE):
        for match in pattern.finditer(code):
            if brace_depth(code, match.start()) != 0:
                continue
            target = split_path(match.group(1))
            if len(target) == 1:
                names.add(target[0])
    return names


def exported_definition(crate_dir: Path) -> tuple[Path, str] | None:
    """The source and body of the crate root's exported `definition()`."""
    root = crate_dir / "src" / "lib.rs"
    if not root.is_file():
        return None
    code = production_code(root.read_text(encoding="utf-8"))
    bodies: list[tuple[Path, str]] = []
    test_only = test_only_module_files(crate_dir)
    direct = top_level_definition_body(code)
    if direct is not None:
        bodies.append((root, direct))
    for match in PUBLIC_USE.finditer(code):
        if brace_depth(code, match.start()) != 0:
            continue
        segments = reexported_definition_module(match.group(1))
        if segments is None:
            continue
        source = module_source(crate_dir, segments)
        if source is None:
            continue
        if (
            source.resolve() in test_only
            or is_test_source(source.relative_to(crate_dir).as_posix())
        ):
            continue
        body = top_level_definition_body(production_code(source.read_text(encoding="utf-8")))
        if body is not None:
            bodies.append((source, body))
    return bodies[0] if len(bodies) == 1 else None


def shared_framework_host(
    root: Path, app_dir: Path, manifest: dict, code: str
) -> bool:
    """Whether the app hosts a framework app whose resource a re-exported
    workspace crate declares.

    A portable app can re-export a shared `ResourceApp` and return its
    `definition()`; the resource, its verbs and its surface live in that
    crate. The app then publishes through the shared app exactly as an app
    that declares the same items locally. Follow only a normal (runtime)
    path dependency the app re-exports. The app root's exported
    `definition()` must return exactly one delegation, and the dependency
    root's exported `definition()` must return exactly one canonical
    `built_in_definition!` or `framework::definition::<..>()` tail that
    builds the re-exported host.

    The built host's module is resolved from that tail, and only that module
    may declare its `ResourceApp` impl and the associated resource's
    declaration. A same-named host or resource in another module, an
    ambiguous path or import, or a test-only module publishes nothing. The
    app root must re-export exactly the host the definition builds.
    """
    definition = exported_definition(app_dir)
    if definition is None:
        return False
    _, body = definition
    delegated = definition_delegate(body)
    if delegated is None:
        return False
    reexports = app_reexports(code)
    for table in runtime_dependency_tables(manifest):
        for alias, specification in table.items():
            rust_alias = alias.replace("-", "_")
            if rust_alias != delegated or not isinstance(specification, dict):
                continue
            names, glob = reexports.get(rust_alias, (set(), False))
            path = specification.get("path")
            if not isinstance(path, str):
                continue
            dependency = (app_dir / path).resolve()
            try:
                dependency.relative_to(root.resolve())
            except ValueError:
                continue
            source = dependency / "src"
            if not source.is_dir():
                continue
            definition = exported_definition(dependency)
            if definition is None:
                continue
            definition_source, definition_body = definition
            app_path = definition_app_path(definition_body)
            if app_path is None:
                continue
            app_type = rust_type_name(app_path)
            if not glob and app_type not in names:
                continue
            host = definition_host_source(dependency, definition_source, app_path)
            if host is None:
                continue
            host_source, host_type = host
            excluded = test_only_module_files(dependency)
            if not shipping_source(host_source, dependency, excluded):
                continue
            if not glob and dependency_exports_host(
                dependency, names, host_source, host_type
            ) is False:
                continue
            host_code = production_code(host_source.read_text(encoding="utf-8"))
            impls = top_level_resource_app_impls(host_code, host_type)
            if len(impls) != 1:
                continue
            resource = ASSOCIATED_RESOURCE.search(
                brace_body(host_code, impls[0].start()) or ""
            )
            if resource is None:
                continue
            provider = resource_provider_source(
                dependency, host_source, resource.group(1)
            )
            if provider is None:
                continue
            resource_source, resource_type = provider
            if not shipping_source(resource_source, dependency, excluded):
                continue
            resource_code = production_code(resource_source.read_text(encoding="utf-8"))
            if resource_type not in declared_resource_names(resource_code):
                continue
            return True
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
        for value in re.findall(r"\b([A-Z][A-Z0-9_]+)\b", code_only(body))
    )
    reachable = reachable_handler_code(text)
    missing: list[str] = []
    for kind, value in entries:
        present = (
            f'"{value}"' in reachable
            if kind == "literal"
            else re.search(rf"\b{re.escape(value)}\b", code_only(reachable)) is not None
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
        code = code_only(raw)
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


# An app that hosts other apps' resources, which exist only at run time (the tilolang host runs
# packages written in the app language). Its resources and actions come from the packages'
# metadata through the platform crate it depends on, so its own source holds none of the
# `Resource`/`ActionSpec` values this audit looks for. The dependency is checked, so the
# exemption cannot outlive the crate that publishes them.
DYNAMIC_RESOURCE_HOSTS = {"tilolang": "tilolang-tilo"}


def dynamic_resource_host(app_dir: Path) -> bool:
    """Whether the app publishes its resources through the platform crate that describes
    them from package metadata at run time."""
    crate = DYNAMIC_RESOURCE_HOSTS.get(app_dir.name)
    manifest = app_dir / "Cargo.toml"
    return (
        crate is not None
        and manifest.is_file()
        and re.search(rf"^{re.escape(crate)}\s*=", manifest.read_text(encoding="utf-8"), re.M)
        is not None
    )


# What a dynamic resource host (above) has beside the typed framework apps: the dev workbench holds
# one document per replica it runs, and search is a catalogue over resources that are only known at
# run time, so there is no typed `Resource::search` to publish from.
DYNAMIC_HOST_RULES = frozenset({"widget_owned_document", "widget_search_source"})

PUBLISH_ACTIONS = re.compile(r"\bCollabRequest\s*::\s*PublishActions\b")


def headless_action_service(app_dir: Path, code: str) -> bool:
    """Whether the app is a headless service (`kind = "service"` with a `cli`
    entrypoint in its `tilo-app.toml`) that publishes its actions to the
    broker itself, so it has no widget resources to audit."""
    manifest = app_dir / "tilo-app.toml"
    if not manifest.is_file() or PUBLISH_ACTIONS.search(code) is None:
        return False
    with manifest.open("rb") as handle:
        data = tomllib.load(handle)
    return (
        data.get("package", {}).get("kind") == "service"
        and data.get("entrypoint", {}).get("type") == "cli"
    )


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
        app_host = code_only(app_host_path.read_text(encoding="utf-8"))
        if app_action_path.is_file():
            app_host += "\n" + code_only(app_action_path.read_text(encoding="utf-8"))
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
    ratchet_baseline = load_ratchet_baseline(root, violations)
    for rule, entries in ratchet_baseline.items():
        for relative in entries:
            baseline_path = root / baseline_file(relative)
            if rule not in RATCHET_REASONS:
                violations.append(Violation(baseline_path, 1, f"unknown ratchet rule `{rule}`"))
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
        # An app model crate (`apps/*-model`) is data shared with an app and
        # may have no UI, nor may a compute package; any `ui` either does
        # depend on is still the facade.
        compute_package = is_compute_package(manifest_path.parent)
        facade_required = not manifest_path.parent.name.endswith("-model") and not compute_package
        if (facade_required and not facades) or any(
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
        combined = code_only(combined_raw)
        # Hand-built effect/view specs are judged by what ships: a test may
        # assert the kind of an operation the framework publishes.
        shipped = "\n".join(
            production_code(text)
            for source, text in sources
            if not is_test_source(source.relative_to(root).as_posix())
        )
        main_path = manifest_path.parent / "src" / "main.rs"
        main_raw = main_path.read_text(encoding="utf-8") if main_path.is_file() else ""
        main_code = code_only(live_launch_registration_code(main_raw))
        audit_ratchets(root, sources, ratchet_baseline, violations)
        if root.resolve() == ROOT.resolve():
            audit_legacy_apis(
                legacy_api_sources(manifest_path.parent, ("src", "tests", "examples", "benches")),
                violations,
                endpoint=manifest_path.parent.name in PROTOCOL_ENDPOINTS,
            )
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
        # The headless agent publishes nothing of its own: it acts only
        # through the broker's task-bound action surfaces. An `apps/*-model`
        # crate is a record shape apps embed (the portability rules let apps
        # depend on it), not an app: the apps embedding it publish its actions.
        # A compute package runs for the action of the app that requested it.
        # A headless service has no widget tree to publish from; it publishes
        # its action descriptors to the broker directly.
        if (
            app_name != "agent"
            and not app_name.endswith("-model")
            and not compute_package
            and not dynamic_resource_host(manifest_path.parent)
            and not headless_action_service(manifest_path.parent, combined)
        ):
            framework_host = (
                FRAMEWORK_APP.search(combined) is not None
                and bool(declared_resource_names(combined))
            ) or shared_framework_host(root, manifest_path.parent, manifest, shipped)
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
            typed_effects = framework_host and TYPED_EFFECT_HANDLER.search(combined) is not None
            if (
                not toolkit_host
                and not typed_effects
                and OPERATION_KIND.search(shipped)
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
                and not typed_effects
                and OPERATION_KIND.search(shipped)
                and not OPERATION_HANDLER_REGISTRY.search(combined)
            ):
                violations.append(
                    Violation(
                        manifest_path,
                        1,
                        "effect/view actions require an explicit runtime handler registry",
                    )
                )
            if not toolkit_host and OPERATION_KIND.search(shipped):
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

        app_dir = manifest_path.parent
        for source in sorted(
            [*(app_dir / "src").rglob("*.rs"), *(app_dir / "tests").rglob("*.rs")]
        ):
            raw = source.read_text(encoding="utf-8")
            for match in RAW_OPERATION_PROTOCOL.finditer(code_only(raw)):
                violations.append(
                    Violation(
                        source,
                        line_number(raw, match.start()),
                        "the host's raw operation protocol is the toolkit's, not an app's: "
                        "answer a typed effect vocabulary, and in a test deliver an "
                        "operation through ui::framework::HostDelivery",
                    )
                )

        for source, text in sources:
            for reason, pattern in LEGACY_ENTRY_POINTS.items():
                for match in pattern.finditer(text):
                    violations.append(
                        Violation(source, line_number(text, match.start()), reason)
                    )

    # The examples teach the app model: none of the APIs it replaced is in
    # one.
    if root.resolve() == ROOT.resolve():
        for manifest_path in sorted((root / "examples" / "apps").glob("*/Cargo.toml")):
            audit_legacy_apis(legacy_api_sources(manifest_path.parent, ("src", "tests")), violations)

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
