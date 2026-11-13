#!/usr/bin/env python3
"""A type a compact format carries writes every field it declares.

postcard and bincode name a struct's field by its place in the bytes and an
enum's variant by its index: nothing in them says which field is which. A serde
attribute that changes which fields a value writes (``skip``,
``skip_serializing``, ``skip_serializing_if``, ``skip_deserializing``,
``flatten``) or how a reader finds its variant (``untagged``, ``tag = …``,
``content = …``) therefore makes the reader take another field's bytes, or the
frame not decode at all. A roster row that left out its absent role shifted its
name, avatar, colour and join time, and every app with someone in its roster
lost its collaboration at start (docs/policies.md).

What it judges is what a compact format carries. A socket is declared: every
type reachable from the roots of an ``interface.toml`` whose ``encoding`` is
``postcard`` or ``bincode`` (``check_wire_enums.carried_types``, read with
``check_interfaces.py``, so "wire type" has one definition in the tree). A type
only a JSON interface carries is exempt, because that file says
``encoding = "json"``: those bytes name their fields, so leaving one out is what
``skip_serializing_if`` is for. A type both kinds of interface carry is judged,
since the positional reader is the one that breaks. A declaration this cannot
read is named and left out: ``check_interfaces.py check`` reports it, and a tree
whose declarations do not read cannot land.

A stored record declares no interface, so the call that reads it says what it
is: ``postcard::from_bytes::<T>`` and its like name `T` in the code, and the
types a `T` is made of are read by position too (``stored_types``). That is
what keeps a durable record, a sealed payload and a frame handed on inside
another one judged, since none of them appears among an interface's roots.
Before it, the offer a device answers with — decoded from an action's bytes —
was no part of any interface, and its rows could have left their absent parts
out unnoticed. A call with no type written at it names nothing to read, and the
tree is judged by its declarations for those.

``#[serde(default)]`` is not refused: it says what an absent field reads as and
never changes what is written, and a positional format only omits a field at
the end of a frame. Neither are ``rename``/``rename_all`` (a positional format
writes no names), ``with``/``serialize_with`` (which write the field the same
way on both sides), or ``transparent``.

A type that only builds what a frame holds is not judged: a roster's `Person` is
turned into its `Member` row before it reaches the frame, and no byte of it is
read by position. A type that becomes a field of a carried type is judged from
then on.

This is an absolute judgement of a tree, not of a change: it runs over the whole
tree in the pre-commit hook and in the repository policy suite, so a commit made
with ``--no-verify`` is judged by the suite on the tree that would land.
"""

from __future__ import annotations

import re
import subprocess
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
from check_interfaces import (  # noqa: E402
    matching,
    named_types,
    read_items,
    tokenize,
)
from check_wire_enums import carried_types  # noqa: E402
from rust_source import is_test_file, production_source  # noqa: E402

ROOT = Path(__file__).resolve().parents[2]

REFUSED_FLAGS = ("flatten", "skip", "skip_deserializing", "skip_serializing", "untagged")
REFUSED_VALUES = ("content", "skip_serializing_if", "tag")

# The calls that write or read a compact value at a type written in the call:
# `postcard::from_bytes::<T>`, `postcard::take_from_bytes::<T>`,
# `postcard::to_stdvec::<T>`, `bincode::deserialize::<T>`, and every other one. A
# call that lets the type be inferred names nothing that can be read here.
AT_TYPE = re.compile(r"\b(?:postcard|bincode)\s*::\s*[A-Za-z_][A-Za-z0-9_:]*\s*::\s*<")
# The two formats `check_wire_enums.POSITIONAL` names, as a call writes them.
COMPACT = ("postcard", "bincode")

# What serde derives write a value's fields; a type without them writes its own
# bytes (a JSON text in a binary format, say), and serde reads no field of it by
# position.
DERIVED = {"Serialize", "Deserialize"}

WHY = (
    "postcard and bincode read a struct's fields by position and an enum's variant by its "
    "index, so the reader takes another field's bytes and the frame does not decode; write "
    "the field whole (absent ones as `None`), or carry the shape in a type of its own"
)


def refused(serde) -> list[str]:
    """The serde attributes of `serde` that leave part of a value out of the bytes."""
    return [flag for flag in REFUSED_FLAGS if flag in serde.flags] + [
        name for name in REFUSED_VALUES if name in serde.values
    ]


def placed(item) -> list[tuple[str, object]]:
    """`(where, serde attributes)` for `item` itself, its fields and its variants' fields."""
    places = [(item.name, item.serde)]
    places += [(f"{item.name}.{field.name}", field.serde) for field in item.fields]
    for variant in item.variants:
        places.append((f"{item.name}::{variant.name}", variant.serde))
        places += [
            (f"{item.name}::{variant.name}.{field.name}", field.serde)
            for field in variant.fields
        ]
    return places


def omitted(source: str, text: str, names: set[str]) -> list[str]:
    """`text`'s declared types among `names` that leave part of a value out of the bytes."""
    found = []
    items = []
    read_items(tokenize(text), source, items)
    written = serde_derived(text)
    for item in items:
        if item.name not in names or item.name not in written:
            continue
        for where, serde in placed(item):
            names_refused = refused(serde)
            if names_refused:
                found.append(
                    f"{source}: {where}: #[serde({', '.join(names_refused)})] leaves part "
                    f"of the value out of the bytes; {WHY}"
                )
    return found


def serde_derived(text: str) -> set[str]:
    """The types `text` derives `Serialize` or `Deserialize` for.

    Only those write their fields through serde, so only those have a field a
    `#[serde(...)]` attribute could leave out. A type with an impl of its own
    decides what its bytes are, and serde reads none of its fields by position."""
    tokens = tokenize(text)
    names: set[str] = set()
    derived = False
    i = 0
    while i < len(tokens):
        token = tokens[i]
        if token.text == "#" and i + 1 < len(tokens) and tokens[i + 1].text == "[":
            close = matching(tokens, i + 1)
            body = tokens[i + 2 : close]
            if body and body[0].text == "derive" and any(t.text in DERIVED for t in body):
                derived = True
            i = close + 1
            continue
        if token.text in ("struct", "enum") and i + 1 < len(tokens) and derived:
            names.add(tokens[i + 1].text)
        if token.text in ("struct", "enum", ";", "{", "}"):
            derived = False
        i += 1
    return names


def named_call_types(text: str) -> set[str]:
    """The types a `postcard::…::<T>` or `bincode::…::<T>` call in `text` writes or
    reads as."""
    found = set()
    for match in AT_TYPE.finditer(text):
        at, depth = match.end(), 1
        while at < len(text) and depth:
            depth += {"<": 1, ">": -1}.get(text[at], 0)
            at += 1
        found.update(named_types(tokenize(text[match.end() : at - 1])))
    return found


def components(item) -> set[str]:
    """The types `item` is made of: its fields', its variants' and its target's."""
    types = [field.ty for field in item.fields]
    types += [variant_ty for variant in item.variants for variant_ty in variant.types]
    types += [field.ty for variant in item.variants for field in variant.fields]
    if item.target:
        types.append(item.target)
    return {name for ty in types for name in named_types(ty)}


def crate_of(source: str) -> str:
    """The crate directory `source` belongs to: everything before its `src/`."""
    return source.partition("/src/")[0]


def compact_sources(root: Path = ROOT) -> list[str]:
    """The production sources of `root` that mention a compact format, relative to it.

    Read through git where `root` is a checkout, so judging every commit does not
    read the tree's ten thousand sources one at a time."""
    if (root / ".git").exists():
        found = set()
        for format in COMPACT:
            listed = subprocess.run(
                ["git", "grep", "-l", "-F", format, "--", "crates", "apps"],
                cwd=root,
                capture_output=True,
                text=True,
            )
            if listed.returncode not in (0, 1):
                break
            found.update(listed.stdout.splitlines())
        else:
            return [
                source
                for source in sorted(found)
                if source.endswith(".rs")
                and "/src/" in source
                and not is_test_file(Path(source))
            ]
    sources = []
    for directory in ("crates", "apps"):
        for path in sorted((root / directory).rglob("*.rs")):
            source = path.relative_to(root).as_posix()
            if "/src/" in source and not is_test_file(Path(source)):
                sources.append(source)
    return sources


def stored_types(root: Path = ROOT) -> dict[str, set[str]]:
    """`{file: type names}` of what the compact calls of `root` name, and of what
    those types are made of.

    A crate is read when one of its production files writes or reads a compact
    value at a type written in the call (`postcard::from_bytes::<T>` and its
    like); every type of that crate a `T` is made of is included, transitively. A
    name the crate does not define is another crate's type and is judged there, or
    by the interfaces; `T` and `Self` name nothing at all.
    """
    per_crate: dict[str, list[tuple[str, str, set[str]]]] = {}
    for source in compact_sources(root):
        text = (root / source).read_text()
        if not any(format in text for format in COMPACT):
            continue
        roots = named_call_types(production_source(text))
        if roots:
            per_crate.setdefault(crate_of(source), []).append((source, text, roots))
    found: dict[str, set[str]] = {}
    for _, files in sorted(per_crate.items()):
        items: dict[str, list[tuple[str, object]]] = {}
        for source, text, _ in files:
            read: list = []
            read_items(tokenize(text), source, read)
            written = serde_derived(text)
            for item in read:
                if item.name in written:
                    items.setdefault(item.name, []).append((source, item))
        pending = {name for _, _, roots in files for name in roots if name in items}
        seen: set[str] = set()
        while pending:
            name = pending.pop()
            if name in seen:
                continue
            seen.add(name)
            for source, item in items[name]:
                found.setdefault(source, set()).add(name)
                pending.update(components(item) & items.keys() - seen)
    return found


def problems(root: Path = ROOT, unreadable: list[str] | None = None) -> list[str]:
    """What the types `root`'s compact formats carry or store get wrong, one line each.

    A declaration this cannot read is named in `unreadable` and that wire is left
    out: the declaration is `check_interfaces.py check`'s to report, and a tree
    whose declarations do not read cannot land either way."""
    by_source: dict[str, set[str]] = {}
    for table in (carried_types(root, unreadable), stored_types(root)):
        for source, names in table.items():
            by_source.setdefault(source, set()).update(names)
    found = []
    for source, names in sorted(by_source.items()):
        found.extend(omitted(source, (root / source).read_text(), names))
    return found


def main(argv: list[str] | None = None) -> int:
    """`check_wire_attributes.py [ROOT]`, failing on what the lines above name."""
    arguments = list(sys.argv[1:] if argv is None else argv)
    root = Path(arguments[0]).resolve() if arguments else ROOT
    unreadable: list[str] = []
    found = problems(root, unreadable)
    for error in unreadable:
        print(
            f"note: {error}; the wire types it declares are left unjudged until "
            "`python3 tools/policy/check_interfaces.py check` passes",
            file=sys.stderr,
        )
    for problem in found:
        print(f"::error::{problem}", file=sys.stderr)
    if found:
        print(
            f"{len(found)} serde attribute(s) omit part of a value a compact format carries",
            file=sys.stderr,
        )
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
