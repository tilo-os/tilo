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

What a named type is made of is followed into whichever crate declares it, the
way the compiler finds it: the path a field is written at, or the ``use`` that
brings the type in, names a dependency in the crate's ``Cargo.toml``. Before it,
only the crate that held the call was read, and a grant's ``CapOverride``,
declared in ``collab-types`` and carried in an agent's postcard frame, left its
absent ``app`` and ``granted_by`` out of the bytes unnoticed. A field written
``with`` a module of its own is written by that module, as a type with its own
impl is: what its type is made of is not followed.

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

import os
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
from rust_source import (  # noqa: E402
    dependency_tables,
    is_test_file,
    load_manifest,
    production_source,
)

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


def components(item) -> list[tuple[str, tuple[str, ...]]]:
    """The types `item` is made of — its fields', its variants' and its target's —
    each with the path it is written at.

    A field written through `with`/`serialize_with` is written by that module,
    which decides its bytes the way a type's own impl does (a frame handed on as
    JSON bytes, say): what its type is made of is not read by position here."""
    fields = list(item.fields) + [field for variant in item.variants for field in variant.fields]
    types = [field.ty for field in fields if not written_by_hand(field.serde)]
    types += [variant_ty for variant in item.variants for variant_ty in variant.types]
    types += list(item.types)
    if item.target:
        types.append(item.target)
    return [named for ty in types for named in qualified_types(ty)]


def written_by_hand(serde) -> bool:
    """Whether a field's serde attributes hand its bytes to a module of its own."""
    return any(key in serde.values for key in ("with", "serialize_with"))


def qualified_types(tokens) -> list[tuple[str, tuple[str, ...]]]:
    """`(name, path before it)` for each type `tokens` names:
    `collab_types::caps::CapOverride` is `("CapOverride", ("collab_types", "caps"))`."""
    found = []
    for i, token in enumerate(tokens):
        if token.kind != "ident":
            continue
        if i + 1 < len(tokens) and tokens[i + 1].text == "::":
            continue
        if token.text in ("dyn", "impl", "mut", "const", "as", "where", "for"):
            continue
        path, at = [], i
        while at >= 2 and tokens[at - 1].text == "::" and tokens[at - 2].kind == "ident":
            at -= 2
            path.insert(0, tokens[at].text)
        found.append((token.text, tuple(path)))
    return found


def crate_of(source: str) -> str:
    """The crate directory `source` belongs to: everything before its `src/`."""
    return source.partition("/src/")[0]


DECLARED = re.compile(r"\b(?:struct|enum|union|type)\s+([A-Za-z_][A-Za-z0-9_]*)")
USE = re.compile(r"\buse\s+([^;]*);")
LOCAL_ROOTS = ("crate", "self", "super", "Self")


class Crates:
    """The crates of a tree, read as far as judging a carried type needs.

    A type a compact call names, or one such a type is made of, is often
    declared in another crate: an agent's grant carries `collab_types`'
    `CapOverride`. That crate is found the way the compiler finds it — the path
    the type is written at, or the `use` that brings it in, names a dependency
    of the crate (its `Cargo.toml`) — and the type is judged in the file that
    declares it. A dependency that only re-exports the type is looked through to
    its own dependencies."""

    def __init__(self, root: Path):
        self.root = root
        self._dependencies: dict[str, dict[str, str]] = {}
        self._files: dict[str, list[tuple[str, str]]] = {}
        self._names: dict[str, dict[str, set[str]]] = {}
        self._items: dict[str, list[tuple[object, bool]]] = {}
        self._workspace: dict[str, str] | None = None

    def workspace(self) -> dict[str, str]:
        """`{dependency key: crate directory}` of the root manifest's
        `[workspace.dependencies]` that are paths in the tree."""
        if self._workspace is None:
            self._workspace = {}
            manifest = self.root / "Cargo.toml"
            if manifest.exists():
                listed = load_manifest(manifest).get("workspace", {}).get("dependencies", {})
                for key, value in listed.items():
                    if isinstance(value, dict) and "path" in value:
                        self._workspace[key] = self.relative(self.root / value["path"])
        return self._workspace

    def relative(self, path: Path) -> str:
        return Path(os.path.normpath(path)).relative_to(self.root).as_posix()

    def dependencies(self, crate: str) -> dict[str, str]:
        """`{name the code writes: crate directory}` of `crate`'s tree dependencies."""
        if crate not in self._dependencies:
            found: dict[str, str] = {}
            manifest = self.root / crate / "Cargo.toml"
            if manifest.exists():
                for _, table in dependency_tables(load_manifest(manifest)):
                    for key, value in table.items():
                        if not isinstance(value, dict):
                            continue
                        if "path" in value:
                            path = self.relative(self.root / crate / value["path"])
                        elif value.get("workspace"):
                            path = self.workspace().get(key)
                        else:
                            path = None
                        if path:
                            found[key.replace("-", "_")] = path
            self._dependencies[crate] = found
        return self._dependencies[crate]

    def files(self, crate: str) -> list[tuple[str, str]]:
        """`(source, production text)` of `crate`'s production sources."""
        if crate not in self._files:
            files = []
            for path in sorted((self.root / crate / "src").rglob("*.rs")):
                source = path.relative_to(self.root).as_posix()
                if not is_test_file(Path(source)):
                    files.append((source, path.read_text()))
            self._files[crate] = files
        return self._files[crate]

    def declared(self, crate: str, name: str) -> list[tuple[str, object, bool]]:
        """`(source, item, serde-derived)` for each declaration of `name` in `crate`.

        Only the files whose text declares `name` are tokenized: a standard type
        is looked for in every dependency, and declares nothing in any."""
        if crate not in self._names:
            self._names[crate] = {
                source: set(DECLARED.findall(text)) for source, text in self.files(crate)
            }
        found = []
        for source, text in self.files(crate):
            if name not in self._names[crate][source]:
                continue
            if source not in self._items:
                items: list = []
                read_items(tokenize(text), source, items)
                written = serde_derived(text)
                self._items[source] = [(item, item.name in written) for item in items]
            found += [(source, item, written) for item, written in self._items[source] if item.name == name]
        return found

    def resolve(
        self, crate: str, name: str, path: tuple[str, ...], source: str, text: str
    ) -> list[tuple[str, object, bool]]:
        """The declarations of the type `name`, written in `crate`'s `source`
        (whose text is `text`) at `path`: none when the tree declares no such type
        (a standard type, a generic parameter)."""
        if not path:
            here = [found for found in self.declared(crate, name) if found[0] == source]
            if here:
                return here
            path = self.use_path(name, text)
        if not path or path[0] in LOCAL_ROOTS:
            declared = self.declared(crate, name)
            if declared or path:
                return narrowed(declared, path)
            # A bare name neither declared here nor imported by a `use` naming it
            # (a glob import, say): the one dependency that declares it.
            declaring = [
                dep for dep in sorted(set(self.dependencies(crate).values()))
                if self.declared(dep, name)
            ]
            return self.declared(declaring[0], name) if len(declaring) == 1 else []
        target = self.dependencies(crate).get(path[0])
        return self.through(target, name, path, set()) if target else []

    def through(
        self, crate: str, name: str, path: tuple[str, ...], seen: set[str]
    ) -> list[tuple[str, object, bool]]:
        """`crate`'s declarations of `name`, or those of the crate a re-export of it
        names."""
        if crate in seen:
            return []
        seen.add(crate)
        declared = self.declared(crate, name)
        if declared:
            return narrowed(declared, path)
        for _, text in self.files(crate):
            if not re.search(rf"\bpub\s+use\b[^;]*\b{re.escape(name)}\b", text):
                continue
            again = self.use_path(name, text, public=True)
            target = self.dependencies(crate).get(again[0]) if again else None
            if target:
                return self.through(target, name, again, seen)
        return []

    @staticmethod
    def use_path(name: str, text: str, public: bool = False) -> tuple[str, ...]:
        """The path, before `name`, of the `use` in `text` that brings `name` in:
        `use collab_types::{caps::CapOverride, Expiry}` gives `("collab_types",
        "caps")` for `CapOverride`."""
        for match in USE.finditer(text):
            if public and not re.search(r"\bpub\s*(?:\([^)]*\))?\s*$", text[: match.start()]):
                continue
            body = re.sub(r"\s+", "", match.group(1)).lstrip(":")
            if not re.search(rf"\b{re.escape(name)}\b", body):
                continue
            return use_tree_path(body, name) or tuple(re.findall(r"^[A-Za-z_][A-Za-z0-9_]*", body))
        return ()


def use_tree_path(tree: str, name: str, prefix: tuple[str, ...] = ()) -> tuple[str, ...] | None:
    """The path before `name` in the `use` tree `tree` (whitespace removed)."""
    depth, start, parts = 0, 0, []
    for at, char in enumerate(tree):
        if char == "{":
            depth += 1
        elif char == "}":
            depth -= 1
        elif char == "," and depth == 0:
            parts.append(tree[start:at])
            start = at + 1
    parts.append(tree[start:])
    for part in parts:
        if "{" in part:
            head, _, rest = part.partition("{")
            segments = tuple(segment for segment in head.split("::") if segment)
            found = use_tree_path(rest[: rest.rfind("}")], name, prefix + segments)
            if found is not None:
                return found
            continue
        segments = [segment for segment in part.split("::") if segment]
        if segments and re.fullmatch(rf"{re.escape(name)}(?:as\w+)?", segments[-1]):
            return prefix + tuple(segments[:-1])
    return None


def narrowed(declared: list, path: tuple[str, ...]) -> list:
    """Of several declarations of one name in a crate, those in the module `path`
    names (`crate::workspaces::door` is `src/workspaces/door.rs`); all of them when
    the path names no module that tells them apart."""
    if len({source for source, _, _ in declared}) <= 1:
        return declared
    modules = [segment for segment in path if segment not in LOCAL_ROOTS][1 if path and path[0] not in LOCAL_ROOTS else 0 :]
    if not modules:
        return declared
    suffix = "/".join(modules)
    matching = [
        found for found in declared
        if re.sub(r"(?:/mod|/lib)?\.rs$", "", found[0]).endswith("/" + suffix)
    ]
    return matching or declared


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

    A file is read when it writes or reads a compact value at a type written in
    the call (`postcard::from_bytes::<T>` and its like); every serde-derived type
    a `T` is made of is included, transitively, in whichever crate of the tree
    declares it (`Crates`). `T` and `Self` name nothing at all, and neither does a
    name no crate of the tree declares (a standard type).
    """
    crates = Crates(root)
    pending: list[tuple[str, str, tuple[str, ...], str, str]] = []
    for source in compact_sources(root):
        text = (root / source).read_text()
        if not any(format in text for format in COMPACT):
            continue
        for name in named_call_types(production_source(text)):
            pending.append((crate_of(source), name, (), source, text))
    found: dict[str, set[str]] = {}
    seen: set[tuple[str, str]] = set()
    while pending:
        crate, name, path, source, text = pending.pop()
        for declared_in, item, written in crates.resolve(crate, name, path, source, text):
            if (declared_in, name) in seen or (not written and item.kind != "alias"):
                continue
            seen.add((declared_in, name))
            if written:
                found.setdefault(declared_in, set()).add(name)
            declaring = crate_of(declared_in)
            declared_text = dict(crates.files(declaring))[declared_in]
            for part, part_path in components(item):
                pending.append((declaring, part, part_path, declared_in, declared_text))
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
