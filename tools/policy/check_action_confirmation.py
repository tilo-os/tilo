#!/usr/bin/env python3
"""Undo is offered for what it can take back, and nothing else is asked twice.

Undo and History are system services: an action Undo takes back runs at once
and offers Undo, with no confirmation at all. A confirmation is for what Undo
cannot take back, and a typed one (`ConfirmationPolicy::Phrase`) names what is
lost, so typing it means the person read it. This check refuses, in production
Rust:

- a typed phrase on an action declared undoable: in the same builder chain or
  contract literal as a compensated undo or `ActionKind::Undoable`;
- a typed phrase that is a generic word ("CONFIRM", "YES", "DELETE", ...),
  which says nothing about what is lost;
- a `"CONFIRM"` literal anywhere: no person is ever asked to type it;
- a deletion (`.destructive()` in a production chain) that Undo cannot take
  back and that says neither why — `undo:irreversible` in the same chain, which
  also makes every door ask before it runs — nor in `undoable-deletions.toml`,
  whose entries carry the reason Undo cannot bring back what the deletion
  destroys. An entry names the one site that builds the action (`file`) or the
  resource whose actions hold it (`resource`), so the same id elsewhere is not
  covered by it, and a deletion whose reason is the world's is written down
  once, where a reviewer reads it beside the action.

`ActionContract::validate_declaration` refuses the same at runtime; this check
catches it before anything runs.
"""

from __future__ import annotations

import fnmatch
import re
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
from rust_source import is_test_file, line_number, production_source  # noqa: E402

ROOT = Path(__file__).resolve().parents[2]
TREES = ("apps", "crates", "examples")
LEDGER = "tools/policy/undoable-deletions.toml"

# Kept in step with `GENERIC_CONFIRMATION_WORDS` in
# crates/collab/actions/src/contract.rs.
GENERIC_WORDS = {
    "confirm", "confirmed", "yes", "ok", "okay", "sure", "agree", "i agree", "continue",
    "proceed", "accept", "delete", "remove", "erase", "wipe", "reset", "destroy",
}

PHRASE = re.compile(r"ConfirmationPolicy\s*::\s*Phrase\s*\{")
PHRASE_LITERAL = re.compile(r'phrase\s*:\s*"([^"]*)"')
CONFIRM_LITERAL = re.compile(r'"CONFIRM"')
ASSIGNED = re.compile(r"\b([A-Za-z_][A-Za-z0-9_]*)\s*\.\s*contract\s*\.\s*confirmation\s*=")
UNDOABLE = re.compile(
    r"\bkind\s*:\s*ActionKind\s*::\s*Undoable\b"
    r"|\bof_kind\s*\(\s*ActionKind\s*::\s*Undoable\s*\)"
    r"|\bCOMPENSATED_UNDO_TAG\b"
)
# `undo:irreversible`, as `IRREVERSIBLE_TAG` or the tag's own words: what
# says a deletion loses something the document never held.
IRREVERSIBLE = re.compile(r"\bIRREVERSIBLE_TAG\b|\"undo:irreversible\"")
# Settling the confirmation once the tags are in place: what a builder generic
# over the declaration does for every verb a surface publishes, so the undo a
# declaration declares (`undo:compensated`) reaches the published row.
SETTLED = re.compile(r"\.\s*settle_confirmation\s*\(\s*\)")
# The tags a declaration carries, put on the spec a builder hands back.
TAGGED = re.compile(r"\btags\s*\.\s*push\s*\(")
# The deletion builders: the one `ActionSpec` carries, and the declaration of
# a verb (`declaration.describe(label).destructive()`), which reaches the spec
# it binds to (`Declaration::destructive` in tilo-resource).
DESTRUCTIVE = re.compile(r"\.\s*destructive\s*\(\s*\)")
# What the word says on something that is not an action: a `ChangeKind` is how
# a change is made, `is_destructive()` asks a built spec whether it is one, and
# a toolbar button (a `ToolbarAction`, or a window bar's entry its
# `bar_entries` builds) is painted as one. Each carries its own object, named before
# the call, and none of them builds a deletion.
NOT_A_BUILDER = re.compile(
    r"(?:\b(?:kind|change|ChangeKind\s*::\s*\w+)\s*\.\s*destructive\s*\(\s*\)"
    r"|\bis_destructive\s*\(\s*\)"
    r"|\bToolbarAction"
    r"|\bbar_entries\s*\.\s*entry\s*\("
    r"|\bActionKind\s*::\s*\w+\.\s*destructive\s*\(\s*\))"
)
# The action a `.destructive()` at the end of a chain continues is built by
# the constant the chain names (`Calendar::INVITE`, `Workspace::DELETE`), whose
# id is the constant's own (`tilo_resource!` names each verb by its variant).
ACTION_CONSTANT = re.compile(r"\b([A-Z][A-Za-z0-9_]*)::([A-Z][A-Z0-9_]*)")
# The declaration a chain continues, by the effect or verb it is built for.
DECLARATION_OF = re.compile(
    r"\.\s*([a-z_][A-Za-z0-9_]*)\s*\(\s*([A-Z][A-Za-z0-9_]*)::([A-Z][A-Z0-9_]*)"
)

# A spec built from the declaration it is published for, rather than one that
# names a deletion of its own (`ActionSpec::typed(decl.id, …)`).
DECLARED_ID = re.compile(r"\bActionSpec\s*::\s*typed\s*\(\s*decl\s*\.\s*id\b")
# What such a builder is called in a finding: every declaration's own action,
# none of which the ledger can name.
DECLARED_ACTION = "a declaration's own action"
NAME_ASSIGNED = re.compile(r"\b([A-Za-z_][A-Za-z0-9_]*)\s*\.\s*contract\s*\.")
KIND_ASSIGNED = re.compile(
    r"\b([A-Za-z_][A-Za-z0-9_]*)(?:\s*\.\s*contract)?\s*\.\s*kind\s*=\s*"
    r"ActionKind\s*::\s*(Undoable|Outside|Read)\b"
)


def names_undoable(statement: str, code: str, start: int) -> bool:
    """Whether `statement` names an action Undo takes back: its own contract
    says so, or it is a binding of one that does, reached through the builder
    chain (`spec.kind = ActionKind::Undoable`) or a contract field
    (`spec.contract.kind = ActionKind::Undoable`). A binding that goes on to
    name another kind says what it is as well: one that reaches outside is
    not undoable, whatever it started as."""
    # The kind the chain names last is what it is: one a builder set later
    # (the arm of a conditional) overrides one set before it.
    kinds = list(KIND_ASSIGNED.finditer(statement))
    if kinds:
        return kinds[-1].group(2) == "Undoable"
    named = NAME_ASSIGNED.search(statement)
    if named is None:
        # A contract literal, wherever it sits.
        return bool(UNDOABLE.search(statement))
    # The nearest binding before the statement that changes it, and what
    # that binding is: never what this binding's own chain says.
    name = re.escape(named.group(1))
    binding = None
    for binding in re.finditer(
        rf"\b(?:let|const|static)\s+(?:mut\s+)?{name}\b", code[:start]
    ):
        pass
    built_start = statement_bounds(code, binding.end())[0] if binding else start
    return names_undoable(code[built_start:start], code, built_start)


def statement_bounds(code: str, at: int) -> tuple[int, int]:
    """The statement around offset `at`: back to the previous `;`, `{` or `}`
    at the same depth, forward to the `;` (or close) that ends it."""
    depth = 0
    start = at
    while start > 0:
        char = code[start - 1]
        if char in ")]}":
            depth += 1
        elif char in "([{":
            if depth == 0:
                break
            depth -= 1
        elif char == ";" and depth == 0:
            break
        start -= 1
    depth = 0
    end = at
    while end < len(code):
        char = code[end]
        if char in "([{":
            depth += 1
        elif char in ")]}":
            if depth == 0:
                break
            depth -= 1
        elif char == ";" and depth == 0:
            break
        end += 1
    return start, end


def element_start(code: str, at: int) -> int | None:
    """Where the array element holding `at` starts, when `at` is one: the
    element after the `[` that opens the array or after the comma that ends
    the one before it. A list of actions is a single statement — its elements
    are separated by commas, not semicolons — so a deletion written as one
    element is read as that element, never as the `const` the list is bound
    to. `None` when no array literal encloses `at`."""
    depth = 0
    index = at
    opened = None
    while index > 0:
        index -= 1
        char = code[index]
        if char in ")]}":
            depth += 1
        elif char in "([{":
            if depth:
                depth -= 1
            elif char == "[":
                opened = index
                break
            else:
                # A brace or a call the element sits in is not an array: the
                # chain reaches back no further than the block it is in.
                return None
    if opened is None:
        return None
    depth = 0
    start = opened + 1
    for index in range(opened + 1, at):
        char = code[index]
        if char in "([{":
            depth += 1
        elif char in ")]}":
            depth -= 1
        elif char == "," and depth == 0:
            start = index + 1
    return start


def chain_bounds(code: str, at: int) -> tuple[int, int]:
    """The expression around the method call at `at`: back to the `let`,
    `const` or `static` that opens it (or the statement's own start), forward
    to the statement's end. A chain that is one arm of a conditional is read
    as the arm it is, and one written as an element of a list as the element
    it is."""
    start = statement_bounds(code, at)[0]
    opening = None
    for opening in re.finditer(r"\b(?:let|const|static)\s", code[:start]):
        pass
    if opening is not None and not re.search(r"[;{}]", code[opening.end() : start]):
        start = opening.start()
    element = element_start(code, at)
    if element is not None and element > start:
        start = element
    return start, statement_bounds(code, at)[1]


def binding(line: str) -> str | None:
    """The name `line` starts by assigning or binding (`spec = spec.destructive()`,
    `let mut spec = …`), which is the name a chain continued on the next line
    belongs to."""
    found = re.match(
        r"^[ \t]*(?:let\s+(?:mut\s+)?)?([a-z_][A-Za-z0-9_]*)[ \t]*=", line.strip("\n")
    )
    return found.group(1) if found else None


def assignment(code: str, name: str, at: int) -> int:
    """Where the binding of `name` the deletion at `at` continues starts: the
    nearest one above it a statement opens with. A line that is itself a
    binding (`spec = spec.destructive()`) continues that name, so the search
    starts before its own line."""
    head = code.rfind("\n", 0, at) + 1
    found = [
        bound
        for bound in re.finditer(
            rf"(?m)^[ \t]*(?:let\s+(?:mut\s+)?)?{re.escape(name)}[ \t]*=", code
        )
        if bound.start() < at
    ]
    return found[-1].start() if found else -1


def built_action(code: str, at: int, name: str) -> str | None:
    """The action the binding of `name` above `at` builds, followed back
    through every binding it continues (`let spec = …` then
    `spec = spec.below_field(…)`)."""
    seen: set[int] = set()
    while True:
        bound = assignment(code, name, at)
        if bound < 0 or bound in seen:
            return None
        seen.add(bound)
        built = code[code.index("=", bound) + 1 : statement_bounds(code, bound)[1]]
        action = declared_action(built, code)
        if action is not None:
            return action
        # The binding builds what another binding holds, so that one names it:
        # the name the assignment starts with (`spec.with_description(…)`) is
        # the binding the deletion continues. Its own line is read only when
        # the name opens that line, never the one above.
        continued = re.match(r"\s*([a-z_][A-Za-z0-9_]*)(?![A-Za-z0-9_])", built)
        if continued is None:
            return None
        opened = code[code.rfind("\n", 0, bound) + 1 : bound].strip() == ""
        at, name = (bound, continued.group(1)) if opened else (bound + 1, continued.group(1))


def names_deletion(chain: str, code: str, start: int) -> str | None:
    """The action a deletion `chain`, built at `start`, deletes: the one it
    names itself, or the one the binding it continues names (`spec` bound to
    an action built with `operation(…)`). A chain that names neither is a
    caller's own action, built where the ledger cannot see it."""
    direct = declared_action(chain, code)
    if direct is not None:
        return direct
    name = binding(chain)
    return None if name is None else continued_binding(code, start, name)


def continued_binding(code: str, start: int, name: str) -> str | None:
    """The action the binding `name` builds, for a deletion the binding
    continues (`spec = spec.destructive()`, `let spec = …` then
    `spec = spec.with_…()`)."""
    if name == "destructive":
        return None
    # The deletion's own line continues the binding it opens, so the search
    # starts within it.
    opening = code.rfind("\n", 0, start) + 1
    moved = opening if code[opening:start].strip() == "" else start + 1
    return built_action(code, moved, name)



# A string constant a crate declares, by name: the id an `Action` literal
# names its action with.
STRING_CONSTANT = re.compile(
    r"\bconst\s+([A-Z][A-Z0-9_]*)\s*:\s*&(?:'static\s+)?str\s*=\s*\"([^\"]*)\""
)
_STRING_CONSTANTS: dict[str, str] = {}


def string_constant(name: str) -> str | None:
    """The value of the string constant `name` the tree declares: an action's
    id is named by a constant in the module that owns the action, not at the
    file that lists it. `None` when no crate declares one under that name."""
    if not _STRING_CONSTANTS:
        for tree in TREES:
            for path in sorted((ROOT / tree).rglob("*.rs")):
                relative = path.relative_to(ROOT)
                if "target" in relative.parts:
                    continue
                text = path.read_text(encoding="utf-8", errors="replace")
                for found in STRING_CONSTANT.finditer(text):
                    _STRING_CONSTANTS.setdefault(found.group(1), found.group(2))
    return _STRING_CONSTANTS.get(name)


def declared_action(chain: str, code: str = "") -> str | None:
    """The action id `chain` builds: the literal it names (`operation("remove", …)`),
    the `id` its `Action` literal carries, or the constant it continues
    (`Calendar::INVITE`). `None` when the word closes a chain that names none
    of them, which is a caller's own action built somewhere the ledger cannot
    see."""
    literal = re.search(r"\b(?:operation|typed_operation)\s*\(\s*\"([^\"]+)\"", chain)
    if literal is not None:
        return literal.group(1)
    # An `Action` literal names the id it is declared with: written out, or
    # the constant the crate declares the action under (`APP_UNINSTALL_ID`),
    # which is a string constant elsewhere in the tree.
    named = re.search(
        r"\bid\s*:\s*(?:\"([^\"]+)\""
        r"|([a-z_:]*[A-Z][A-Z0-9_]*(?:::[A-Za-z_][A-Za-z0-9_]*)?))",
        chain,
    )
    if named is not None:
        if named.group(1) is not None:
            return named.group(1)
        return string_constant(named.group(2).rsplit("::", 1)[-1])
    # A spec built from the declaration itself (`ActionSpec::typed(decl.id, …)`)
    # is each declaration's own action, offered with its item bound: the
    # declaration says what it destroys and whether it is tagged irreversible.
    if DECLARED_ID.search(chain):
        return DECLARED_ACTION
    # The one the chain is named for: `Calendar::INVITE.declaration(...)`, or
    # the effect a declaration is looked up by (`Calendar::INVITE.bind(…)`) —
    # a constant is the argument, a method is not.
    declared = DECLARATION_OF.findall(chain)
    if declared:
        return declared[-1][2].lower().replace("_", "-")
    # A constant named for an action is spelled in capitals with a body
    # (`Workspace::DELETE_OFFLINE_AREA`): a type's own name (`Workspace`) is
    # never the action, and neither is a constructor's (`ToolbarAction::new`).
    found = ACTION_CONSTANT.search(chain)
    if found is None or not found.group(2)[1:].isupper():
        return None
    return found.group(2).lower().replace("_", "-")


def statement_line(code: str, at: int) -> str:
    """The text from the start of `at`'s clause: what is said about the call
    without reaching into the one before it."""
    opened = statement_bounds(code, at)[0]
    start = opened
    for clause in (";", "{", "}"):
        closer = code.rfind(clause, 0, at)
        if closer >= opened:
            start = closer + 1
    # A chain continued on its own line closes the call it belongs to two
    # lines up (`ToolbarAction::new(…)` then `.destructive()`).
    if code[start:opened].strip() == "" and start > opened:
        previous = code.rfind("\n", 0, start)
        if previous >= 0:
            start = code.rfind("\n", 0, previous) + 1
    return code[start:statement_bounds(code, at)[1]]


def bound_action(code: str, at: int) -> str | None:
    """The action a binding of the block around `at` builds: `name` is bound to
    `Type::ACTION.spec()` or `.operation("id", …)` somewhere above the
    deletion, and the deletion continues that binding by name."""
    statement = statement_line(code, at)
    # The name the chain continues is at its head, or the first one it passes
    # as an argument (`Some(declaration.describe(…))`).
    heads = re.findall(r"\b([a-z_][A-Za-z0-9_]*)\b", statement)
    names = heads[:1] if binding(statement) else heads
    for name in names:
        action = built_action(code, at, name)
        if action is not None:
            return action
    return None

def ledger_entries(entries: list[dict], errors: list[str]) -> list[dict]:
    """The ledger's entries, each one that names an action and says why Undo
    cannot bring back what it destroys."""
    kept = []
    for entry in entries:
        action = entry.get("action")
        if not isinstance(action, str) or not action:
            errors.append(f"{LEDGER}: an entry names no action; add `action = \"<id>\"`")
            continue
        reason = entry.get("reason")
        if not isinstance(reason, str) or len(reason.split()) < 8:
            errors.append(
                f"{LEDGER}: {action}: the entry gives no reason; say what the deletion "
                "loses that Undo cannot bring back"
            )
            continue
        unknown = set(entry) - {"action", "file", "resource", "effect", "reason"}
        if unknown:
            errors.append(
                f"{LEDGER}: {action}: an entry names {', '.join(sorted(unknown))}, which "
                "nothing reads"
            )
            continue
        # Where the deletion is built: the `file` that builds it, optionally
        # with the `#[effect(destructive)]` it declares for it, or the
        # `resource` whose own builder publishes every action it declares.
        file = entry.get("file")
        effect = entry.get("effect")
        resource = entry.get("resource")
        if file is None and resource is None:
            errors.append(
                f"{LEDGER}: {action}: an entry pins the site that builds the deletion with "
                "`file`, or the resource whose builder publishes it with `resource`"
            )
            continue
        if not isinstance(file, str) and file is not None:
            errors.append(f"{LEDGER}: {action}: `file` names the path that builds it")
            continue
        if resource is not None:
            if not isinstance(resource, str) or not resource or effect is not None:
                errors.append(
                    f"{LEDGER}: {action}: `resource` names one kind, and pins nothing else"
                )
                continue
        elif effect is not None and (not isinstance(effect, str) or not effect):
            errors.append(
                f"{LEDGER}: {action}: `effect` names the variant the declaration is "
                'written as (`effect = "Format"` for `#[effect(job, destructive)]`)'
            )
            continue
        kept.append(entry)
    return kept


def pins(entry: dict, relative: str, action: str, effects: set[tuple[str, str]]) -> bool:
    """Whether `entry` is about the deletion `action` at `relative`: the file
    it names builds it (a `*` covers the files that directory holds), or the
    `#[effect(destructive)]` the file declares it as, or every action of the
    site is the resource's own (`resource`), which names the deletion in the
    one builder that publishes them."""
    site = entry.get("file")
    effect = entry.get("effect")
    if isinstance(effect, str):
        # The entry names the effect, and the action the effect is published
        # as: the effect's variant and the action id are not the same word.
        return effect in effects and action in (entry["action"], effect)
    if isinstance(site, str):
        return fnmatch.fnmatchcase(relative, site) and entry["action"] == action
    return isinstance(entry.get("resource"), str) and entry["action"] == action


def ledger_effects(
    entries: list[dict], deletions: list[tuple[str, str, set[str]]]
) -> list[tuple[str, str, set[str]]]:
    """`deletions` with what a ledger entry naming an `effect` covers: an
    `#[effect(destructive)]` declares a deletion its surface's builder
    publishes, so the entry names the effect and its action together, and the
    effect is read where the entry's `file` declares it."""
    found = list(deletions)
    for entry in entries:
        if not isinstance(entry.get("effect"), str):
            continue
        site = entry.get("file")
        if not isinstance(site, str):
            continue
        path = ROOT / site
        if not path.is_file():
            continue
        effects = declared_effects(path.read_text(encoding="utf-8", errors="replace"))
        if entry["effect"] in effects:
            found.append((site, entry["effect"], effects))
    return found


def unbuilt(entries: list[dict], deletions: list[tuple[str, str, set[str]]]) -> list[str]:
    """The ledger's entries that cover no deletion the tree builds: an entry
    outlives the change that removed its deletion, and one that covers nothing
    reads as if it still did.

    `deletions` is every deletion in the tree as `(file, action, the
    `#[effect(destructive)]` declarations at that site)`."""
    errors = []
    for entry in entries:
        action = entry["action"]
        found = any(
            pins(entry, file, name, effects) for file, name, effects in deletions
        )
        if not found:
            where = entry.get("file") or entry.get("effect") or entry.get("resource")
            errors.append(
                f"{LEDGER}: {action}: no deletion named `{action}` is built at {where}; "
                "remove the entry, or name what builds it"
            )
    return errors


def listed(
    entries: list[dict], relative: str, action: str, effects: set[tuple[str, str]]
) -> bool:
    """Whether the ledger holds the deletion `action` at `relative`."""
    return any(pins(entry, relative, action, effects) for entry in entries)


def declared_effects(text: str) -> set[str]:
    """The variants `text` declares destructive with `#[effect(destructive)]`,
    as the effect means them (`Format`)."""
    return {
        found.group("variant")
        for found in re.finditer(
            r"^[ \t]*#\[effect\((?P<flags>[^)]*)\)\][ \t]*\n[ \t]*(?:pub\s+)?(?P<variant>[A-Z][A-Za-z0-9_]*)",
            text,
            re.M,
        )
        if re.search(r"(?:^|,)\s*destructive\s*(?:,|$)", found.group("flags"))
    }


def check_source(relative: str, text: str, entries: list[dict] | None = None) -> list[str]:
    entries = entries or []
    effects = declared_effects(text)
    code = production_source(text)
    errors: list[str] = []
    for found in CONFIRM_LITERAL.finditer(code):
        errors.append(
            f'{relative}:{line_number(code, found.start())}: "CONFIRM" is never a word a '
            "person types: an undoable action asks nothing, an irreversible one names what it "
            "destroys"
        )
    for found in PHRASE.finditer(code):
        line = line_number(code, found.start())
        start, end = statement_bounds(code, found.start())
        statement = code[start:end]
        literal = PHRASE_LITERAL.search(code, found.end(), min(end, found.end() + 400))
        if literal and literal.group(1).strip().lower() in GENERIC_WORDS:
            errors.append(
                f'{relative}:{line}: typed confirmation "{literal.group(1)}" says nothing '
                "about what is lost; make the phrase the name of what it destroys"
            )
        # A phrase set on a spec built earlier is read with what that
        # binding already says it is: `names_undoable` reaches the binding
        # from the kind the statement sets, never from its own text.
        if names_undoable(statement, code, start):
            errors.append(
                f"{relative}:{line}: a typed confirmation on an action Undo takes back; it "
                "runs at once and offers Undo instead"
            )
    errors.extend(check_deletions(relative, code, entries, effects))
    return errors


def opening_brace(code: str, at: int) -> int:
    """The nearest brace above `at` that opens the block it is in."""
    depth = 0
    index = at
    while index > 0:
        index -= 1
        char = code[index]
        if char == "}":
            depth += 1
        elif char == "{":
            if depth == 0:
                return index
            depth -= 1
    return -1


def is_fn_body(code: str, brace: int) -> bool:
    """Whether the block `brace` opens is a function's body: the signature
    above it starts at an `fn` and holds no other brace, so an `if` or a
    `match` inside another function's body is not one."""
    head = code[max(0, brace - 400) : brace]
    found = list(re.finditer(r"\bfn\b", head))
    return bool(found) and not re.search(r"[{};]", head[found[-1].end() :])


def enclosing_body(code: str, at: int) -> str:
    """The body of the function that contains `at`: from its opening brace to
    the brace that closes it, so a deletion reads what settles the spec it
    returns rather than what settles some other function's."""
    opened = -1
    while True:
        opened = opening_brace(code, at)
        if opened < 0:
            return ""
        # An `if` or `match` arm is no function's body: the deletion reads
        # what settles the spec outside the branch it is built in.
        if is_fn_body(code, opened):
            break
        at = opened
    depth = 0
    for index in range(opened, len(code)):
        char = code[index]
        if char == "{":
            depth += 1
        elif char == "}":
            depth -= 1
            if depth == 0:
                return code[opened : index + 1]
    return code[opened:]


def check_deletions(
    relative: str, code: str, entries: list[dict], effects: set[str]
) -> list[str]:
    """Each deletion `code` builds that Undo cannot take back and that neither
    tags itself irreversible nor is written down in the ledger."""
    errors: list[str] = []
    seen: set[int] = set()
    for found in DESTRUCTIVE.finditer(code):
        if NOT_A_BUILDER.search(statement_line(code, found.start())):
            continue
        start = chain_bounds(code, found.start())[0]
        if start in seen:
            continue
        seen.add(start)
        # The chain is read to the end of its statement: a deletion tagged
        # irreversibly is tagged when it is built, and one built elsewhere
        # carries the tag where it is built.
        around = code[start : statement_bounds(code, start)[1]]
        if IRREVERSIBLE.search(around):
            continue
        # A builder that settles the spec's confirmation once its kind and
        # tags are in place, in the same body, lets the kind its declaration
        # gives it decide: settling reads what the spec carries.
        if SETTLED.search(around) or SETTLED.search(enclosing_body(code, start)):
            continue
        # A builder that puts the declaration's tags on the spec before
        # handing it back has read them: `destructive` settles from whatever
        # the spec carries, so the undo the declaration declares decides.
        if TAGGED.search(enclosing_body(code, start)):
            continue
        action = named_deletion(code, found.start())
        if action is not None and listed(entries, relative, action, effects):
            continue
        line = line_number(code, found.start())
        # What was found and why it is refused; what to do about it is the
        # ledger's own header, read by the person who writes the entry.
        if action is None:
            named = "a deletion the code does not name"
        else:
            named = f"`{action}`, a deletion"
        errors.append(
            f"{relative}:{line}: {named} Undo cannot take back and says neither "
            f"`undo:irreversible` nor why in {LEDGER}"
        )
    return errors


def deletions_built(relative: str, text: str) -> set[str]:
    """The action each deletion in `text` names, as [`check_deletions`] reads
    it: what a ledger entry has to be built by."""
    code = production_source(text)
    return {
        action
        for found in DESTRUCTIVE.finditer(code)
        if not NOT_A_BUILDER.search(statement_line(code, found.start()))
        for action in [named_deletion(code, found.start())]
        if action is not None
    }


def named_deletion(code: str, at: int) -> str | None:
    """The action the deletion at `at` deletes, by the chain it closes or the
    binding it continues; `None` when it names neither."""
    start, end = chain_bounds(code, at)
    chain = code[start:end]
    statement = statement_bounds(code, start)
    action = names_deletion(chain, code, start) or names_deletion(
        code[statement[0] : statement[1]], code, statement[0]
    )
    return bound_action(code, start) if action is None else action


def load_ledger(errors: list[str]) -> list[dict]:
    import tomllib

    path = ROOT / LEDGER
    if not path.exists():
        return []
    try:
        document = tomllib.loads(path.read_text(encoding="utf-8"))
    except tomllib.TOMLDecodeError as error:
        errors.append(f"{LEDGER}: {error}")
        return []
    return ledger_entries(document.get("deletion", []), errors)


def main() -> int:
    errors: list[str] = []
    entries = load_ledger(errors)
    deletions: list[tuple[str, str, set[str]]] = []
    for tree in TREES:
        for path in sorted((ROOT / tree).rglob("*.rs")):
            relative = path.relative_to(ROOT)
            if "target" in relative.parts or is_test_file(relative):
                continue
            text = path.read_text(encoding="utf-8", errors="replace")
            if (
                "Phrase" not in text
                and "CONFIRM" not in text
                and "destructive()" not in text
            ):
                continue
            errors.extend(check_source(relative.as_posix(), text, entries))
            deletions.extend(
                (relative.as_posix(), action, declared_effects(text))
                for action in deletions_built(relative.as_posix(), text)
            )
    errors.extend(unbuilt(entries, ledger_effects(entries, deletions)))
    for error in errors:
        print(error)
    if errors:
        print(
            f"{len(errors)} confirmation(s) break the rule: undoable actions run at once with "
            "Undo; an irreversible one names what it loses and is written down",
            file=sys.stderr,
        )
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
