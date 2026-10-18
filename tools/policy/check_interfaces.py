#!/usr/bin/env python3
"""One version number per interface, bumped whenever the interface breaks.

Tilo has four interfaces between separately updated parts: app and shell
(``shell-ui``), app and runtime (``runtime``), device and device (``peer``)
and the store's package formats (``package``). Each one has a single version
number in the code and a committed schema, ``interface.toml`` in the crate
that owns the number, written from the code the way ``tilo-app surface``
writes an app's ``surface.toml``. Two sides speak to each other only when
their numbers are equal (docs/architecture/compatibility.md); there is no
negotiation and no code for an older number.

``check`` (the repository policy suite) fails when a committed schema differs
from what the code declares, or when the version the file records is not the
one the code and the image's ``system-interfaces.toml`` carry.

``against BASE`` (the ``changes`` check, against main) fails when a schema
changed incompatibly since BASE without a higher version. An added optional
field of a JSON or TOML message, an added Cap'n Proto struct field and a new
type are compatible; everything else that changes what one side sends or
reads is not.

``write`` rewrites every ``interface.toml`` from the code, keeping the
authored header (where the version lives, the sources, the roots, the opaque
types).

The schema is read from the Rust sources themselves: every struct, enum and
type alias reachable from the interface's root types through the listed
source files, with the serde attributes that change the wire form. A type the
sources do not define must be a known standard type or be listed under
``[opaque]`` with what it is, so nothing on the wire goes unexamined by
accident. Cap'n Proto schemas are read from their ``.capnp`` files.
"""

from __future__ import annotations

import argparse
import re
import subprocess
import sys
import tomllib
from dataclasses import dataclass, field
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
FILE = "interface.toml"
SYSTEM_INTERFACES = "os/usr/share/tilo/system-interfaces.toml"
HEADER = (
    "# Written by `python3 tools/policy/check_interfaces.py write` from the code;\n"
    "# review a change here like an API change (docs/architecture/compatibility.md).\n"
    "# Everything above [schema] is authored; [schema] and `version` are written.\n\n"
)

# Types every Rust program has; they carry their arguments' schema.
STANDARD = {
    "u8", "u16", "u32", "u64", "u128", "usize", "i8", "i16", "i32", "i64", "i128",
    "isize", "f32", "f64", "bool", "char", "str", "String", "Vec", "Option", "Box",
    "Arc", "Rc", "Cow", "BTreeMap", "BTreeSet", "HashMap", "HashSet", "VecDeque",
    "Result", "PathBuf", "Path", "Self", "Duration", "SystemTime", "NonZeroU8", "NonZeroU16", "NonZeroU32",
    "NonZeroU64", "NonZeroUsize",
}
ENCODINGS = ("json", "postcard")


class InterfaceError(Exception):
    """A schema could not be read from the code."""


# ---------------------------------------------------------------------------
# Rust tokens


@dataclass(frozen=True)
class Token:
    kind: str  # ident, punct, str, lit, lifetime
    text: str


_IDENT = re.compile(r"(?:r#)?[A-Za-z_][A-Za-z0-9_]*")
_NUMBER = re.compile(r"[0-9][0-9A-Za-z_.]*")
_RAW_STR = re.compile(r'b?r(#*)"')


def tokenize(text: str) -> list[Token]:
    """Rust tokens without comments; string contents are kept, unescaped
    only as far as attribute values need."""
    tokens: list[Token] = []
    i, n = 0, len(text)
    while i < n:
        c = text[i]
        if c.isspace():
            i += 1
            continue
        if text.startswith("//", i):
            end = text.find("\n", i)
            i = n if end < 0 else end
            continue
        if text.startswith("/*", i):
            depth, i = 1, i + 2
            while i < n and depth:
                if text.startswith("/*", i):
                    depth, i = depth + 1, i + 2
                elif text.startswith("*/", i):
                    depth, i = depth - 1, i + 2
                else:
                    i += 1
            continue
        raw = _RAW_STR.match(text, i)
        if raw:
            hashes = raw.group(1)
            start = raw.end()
            end = text.find('"' + hashes, start)
            if end < 0:
                raise InterfaceError("unterminated raw string")
            tokens.append(Token("str", text[start:end]))
            i = end + 1 + len(hashes)
            continue
        if c == '"' or text.startswith('b"', i):
            j = i + (2 if c == "b" else 1)
            out = []
            while j < n and text[j] != '"':
                if text[j] == "\\":
                    out.append(text[j + 1] if j + 1 < n else "")
                    j += 2
                else:
                    out.append(text[j])
                    j += 1
            tokens.append(Token("str", "".join(out)))
            i = j + 1
            continue
        if c == "'" or text.startswith("b'", i):
            j = i + (2 if c == "b" else 1)
            # A char literal closes within a few characters; a lifetime does not.
            close = text.find("'", j + 1)
            body = text[j:close] if close > 0 else ""
            if close > 0 and (len(body) == 1 or (body.startswith("\\") and len(body) <= 10)):
                tokens.append(Token("lit", text[i : close + 1]))
                i = close + 1
                continue
            ident = _IDENT.match(text, j)
            if ident:
                tokens.append(Token("lifetime", "'" + ident.group(0)))
                i = ident.end()
                continue
        ident = _IDENT.match(text, i)
        if ident:
            word = ident.group(0)
            tokens.append(Token("ident", word[2:] if word.startswith("r#") else word))
            i = ident.end()
            continue
        number = _NUMBER.match(text, i)
        if number:
            tokens.append(Token("lit", number.group(0)))
            i = number.end()
            continue
        for punct in ("::", "->", "=>"):
            if text.startswith(punct, i):
                tokens.append(Token("punct", punct))
                i += len(punct)
                break
        else:
            tokens.append(Token("punct", c))
            i += 1
    return tokens


OPEN = {"(": ")", "[": "]", "{": "}"}


def matching(tokens: list[Token], i: int) -> int:
    """Index of the bracket closing the one at `i`."""
    depth = 0
    for j in range(i, len(tokens)):
        t = tokens[j]
        if t.kind != "punct":
            continue
        if t.text in OPEN:
            depth += 1
        elif t.text in (")", "]", "}"):
            depth -= 1
            if depth == 0:
                return j
    raise InterfaceError("unbalanced brackets")


def split_top(tokens: list[Token], sep: str = ",") -> list[list[Token]]:
    """`tokens` split at `sep` outside any bracket or generic argument list."""
    parts: list[list[Token]] = [[]]
    depth = 0
    for i, t in enumerate(tokens):
        if t.kind == "punct":
            if t.text in OPEN:
                depth += 1
            elif t.text in (")", "]", "}"):
                depth -= 1
            elif t.text == "<":
                depth += 1
            elif t.text == ">" and not (i and tokens[i - 1].text == "-"):
                depth -= 1
            elif t.text == sep and depth == 0:
                parts.append([])
                continue
        parts[-1].append(t)
    return [part for part in parts if part]


# ---------------------------------------------------------------------------
# Attributes


@dataclass
class Serde:
    """The serde attributes of one item, field or variant."""

    flags: set[str] = field(default_factory=set)
    values: dict[str, str] = field(default_factory=dict)

    def merge(self, tokens: list[Token]) -> None:
        for part in split_top(tokens):
            key = part[0].text
            if len(part) >= 3 and part[1].text == "=":
                self.values[key] = part[2].text
            elif len(part) >= 2 and part[1].text == "(":
                # rename(serialize = "a", deserialize = "b"): the reader's name.
                inner = Serde()
                inner.merge(part[2:-1])
                chosen = inner.values.get("deserialize") or inner.values.get("serialize")
                if chosen is not None:
                    self.values[key] = chosen
                else:
                    self.flags.add(key)
            else:
                self.flags.add(key)


@dataclass
class Attrs:
    serde: Serde = field(default_factory=Serde)
    test_only: bool = False


def read_attrs(tokens: list[Token], i: int) -> tuple[Attrs, int]:
    attrs = Attrs()
    while i < len(tokens) and tokens[i].text == "#":
        inner = i + 1
        if tokens[inner].text == "!":
            inner += 1
        close = matching(tokens, inner)
        body = tokens[inner + 1 : close]
        if body and body[0].text == "serde" and len(body) > 1 and body[1].text == "(":
            attrs.serde.merge(body[2:-1])
        if body and body[0].text == "cfg":
            words = {t.text for t in body}
            if "test" in words and "not" not in words:
                attrs.test_only = True
        i = close + 1
    return attrs, i


# ---------------------------------------------------------------------------
# Rust items


@dataclass
class Field:
    name: str
    ty: list[Token]
    serde: Serde


@dataclass
class Variant:
    name: str
    shape: str  # unit, tuple, struct
    fields: list[Field]
    types: list[list[Token]]
    serde: Serde


@dataclass
class Item:
    name: str
    kind: str  # struct, enum, alias
    path: str
    serde: Serde
    params: list[str]
    shape: str = "struct"  # struct, tuple, unit (structs)
    fields: list[Field] = field(default_factory=list)
    types: list[list[Token]] = field(default_factory=list)
    variants: list[Variant] = field(default_factory=list)
    target: list[Token] = field(default_factory=list)


def generic_params(tokens: list[Token], i: int) -> tuple[list[str], int]:
    """The type parameters of `<...>` at `i`, and the index after it."""
    if i >= len(tokens) or tokens[i].text != "<":
        return [], i
    depth, j = 0, i
    while j < len(tokens):
        if tokens[j].text == "<":
            depth += 1
        elif tokens[j].text == ">":
            depth -= 1
            if depth == 0:
                break
        j += 1
    params = []
    for part in split_top(tokens[i + 1 : j]):
        if part[0].kind == "ident" and part[0].text != "const":
            params.append(part[0].text)
    return params, j + 1


def read_fields(tokens: list[Token]) -> list[Field]:
    fields = []
    for part in split_top(tokens):
        attrs, k = read_attrs(part, 0)
        if part[k].text == "pub":
            k += 1
            if k < len(part) and part[k].text == "(":
                k = matching(part, k) + 1
        name = part[k].text
        fields.append(Field(name, part[k + 2 :], attrs.serde))
    return fields


def read_types(tokens: list[Token]) -> list[list[Token]]:
    types = []
    for part in split_top(tokens):
        _, k = read_attrs(part, 0)
        if part[k].text == "pub":
            k += 1
            if k < len(part) and part[k].text == "(":
                k = matching(part, k) + 1
        types.append(part[k:])
    return types


def skip_to_body(tokens: list[Token], i: int) -> int:
    """Past a `where` clause, to the body's `{`, `(` or `;`."""
    while tokens[i].text not in ("{", "(", ";"):
        if tokens[i].text == "<":
            _, i = generic_params(tokens, i)
            continue
        i += 1
    return i


def read_items(tokens: list[Token], path: str, items: list[Item], start: int = 0, end: int | None = None) -> None:
    i = start
    end = len(tokens) if end is None else end
    while i < end:
        attrs, i = read_attrs(tokens, i)
        if i >= end:
            break
        if tokens[i].text == "pub":
            i += 1
            if tokens[i].text == "(":
                i = matching(tokens, i) + 1
        word = tokens[i].text
        if word in ("struct", "enum", "union") and tokens[i + 1].kind == "ident":
            name = tokens[i + 1].text
            params, j = generic_params(tokens, i + 2)
            j = skip_to_body(tokens, j)
            item = Item(name, "enum" if word == "enum" else "struct", path, attrs.serde, params)
            if tokens[j].text == ";":
                item.shape = "unit"
                after = j + 1
            else:
                close = matching(tokens, j)
                body = tokens[j + 1 : close]
                if word == "enum":
                    item.variants = read_variants(body)
                elif tokens[j].text == "(":
                    item.shape = "tuple"
                    item.types = read_types(body)
                else:
                    item.fields = read_fields(body)
                after = close + 1
                if tokens[j].text == "(":
                    after = skip_to_body(tokens, after)
                    after += 1 if tokens[after].text == ";" else 0
            if not attrs.test_only:
                items.append(item)
            i = after
            continue
        if word == "type" and tokens[i + 1].kind == "ident":
            name = tokens[i + 1].text
            params, j = generic_params(tokens, i + 2)
            semi = j
            while tokens[semi].text != ";":
                semi += 1
            if not attrs.test_only and tokens[j].text == "=":
                items.append(Item(name, "alias", path, attrs.serde, params, target=tokens[j + 1 : semi]))
            i = semi + 1
            continue
        if word == "mod" and tokens[i + 2].text == "{":
            close = matching(tokens, i + 2)
            if not attrs.test_only:
                read_items(tokens, path, items, i + 3, close)
            i = close + 1
            continue
        # Anything else: to its `;` or the end of its block.
        while i < end:
            t = tokens[i]
            if t.kind == "punct" and t.text in OPEN:
                close = matching(tokens, i)
                if t.text == "{" and word not in ("const", "static", "let"):
                    i = close + 1
                    break
                i = close + 1
                continue
            i += 1
            if t.text == ";":
                break


def read_variants(tokens: list[Token]) -> list[Variant]:
    variants = []
    for part in split_top(tokens):
        attrs, k = read_attrs(part, 0)
        if k >= len(part):
            continue
        name = part[k].text
        rest = part[k + 1 :]
        if rest and rest[0].text == "{":
            close = matching(rest, 0)
            variants.append(Variant(name, "struct", read_fields(rest[1:close]), [], attrs.serde))
        elif rest and rest[0].text == "(":
            close = matching(rest, 0)
            variants.append(Variant(name, "tuple", [], read_types(rest[1:close]), attrs.serde))
        else:
            variants.append(Variant(name, "unit", [], [], attrs.serde))
    return variants


# ---------------------------------------------------------------------------
# Wire names


def renamed(name: str, rule: str | None, variant: bool = False) -> str:
    """`name` under serde's `rename_all` `rule`, exactly as serde_derive
    applies it: a variant's name is PascalCase, a field's snake_case."""
    if not rule:
        return name
    if variant:
        if rule == "lowercase":
            return name.lower()
        if rule == "UPPERCASE":
            return name.upper()
        if rule == "PascalCase":
            return name
        if rule == "camelCase":
            return name[:1].lower() + name[1:]
        snake = "".join(
            ("_" if i and c.isupper() else "") + c.lower() for i, c in enumerate(name)
        )
        if rule == "snake_case":
            return snake
        if rule == "SCREAMING_SNAKE_CASE":
            return snake.upper()
        if rule == "kebab-case":
            return snake.replace("_", "-")
        if rule == "SCREAMING-KEBAB-CASE":
            return snake.upper().replace("_", "-")
    else:
        if rule in ("lowercase", "snake_case"):
            return name
        if rule in ("UPPERCASE", "SCREAMING_SNAKE_CASE"):
            return name.upper()
        if rule in ("PascalCase", "camelCase"):
            pascal = "".join(part[:1].upper() + part[1:] for part in name.split("_"))
            return pascal if rule == "PascalCase" else pascal[:1].lower() + pascal[1:]
        if rule == "kebab-case":
            return name.replace("_", "-")
        if rule == "SCREAMING-KEBAB-CASE":
            return name.upper().replace("_", "-")
    raise InterfaceError(f"unknown rename_all rule {rule!r}")


# ---------------------------------------------------------------------------
# Schema


def type_text(tokens: list[Token]) -> str:
    """A type as one string: paths shortened to their last segment,
    lifetimes dropped."""
    out: list[str] = []
    i = 0
    while i < len(tokens):
        t = tokens[i]
        if t.kind == "lifetime":
            i += 2 if i + 1 < len(tokens) and tokens[i + 1].text == "," else 1
            continue
        if t.kind == "ident" and i + 1 < len(tokens) and tokens[i + 1].text == "::":
            i += 2
            continue
        if t.text == "::":
            i += 1
            continue
        out.append(t.text)
        i += 1
    text = ""
    for piece in out:
        if text and (text[-1].isalnum() or text[-1] == "_") and (piece[0].isalnum() or piece[0] == "_"):
            text += " "
        text += piece
        if piece == ",":
            text += " "
        if piece == ";":
            text += " "
    return text.replace("<>", "")


def named_types(tokens: list[Token]) -> list[str]:
    names = []
    for i, t in enumerate(tokens):
        if t.kind != "ident":
            continue
        if i + 1 < len(tokens) and tokens[i + 1].text == "::":
            continue
        if t.text in ("dyn", "impl", "mut", "const", "as", "where", "for"):
            continue
        names.append(t.text)
    return names


@dataclass
class Spec:
    name: str
    version: int
    version_const: str
    encoding: str
    sources: list[str]
    roots: list[str]
    capnp: list[str]
    opaque: dict[str, str]
    serves: bool
    path: Path


def load_spec(path: Path) -> tuple[Spec, dict[str, str]]:
    data = tomllib.loads(path.read_text())
    spec = Spec(
        name=data["name"],
        version=int(data["version"]),
        version_const=data["version_const"],
        encoding=data.get("encoding", "json"),
        sources=list(data.get("sources", [])),
        roots=list(data.get("roots", [])),
        capnp=list(data.get("capnp", [])),
        opaque=dict(data.get("opaque", {})),
        serves=bool(data.get("served_to_apps", False)),
        path=path,
    )
    if spec.encoding not in ENCODINGS:
        raise InterfaceError(f"{path}: encoding must be one of {', '.join(ENCODINGS)}")
    return spec, dict(data.get("schema", {}))


def code_version(spec: Spec, root: Path = ROOT) -> int:
    file, _, const = spec.version_const.partition(":")
    text = (root / file).read_text()
    found = re.search(rf"\bconst\s+{re.escape(const)}\s*:\s*\w+\s*=\s*([0-9_]+)\s*;", text)
    if not found:
        raise InterfaceError(f"{file} declares no `const {const}` with a number")
    return int(found.group(1).replace("_", ""))


def rust_schema(spec: Spec, root: Path = ROOT) -> dict[str, str]:
    items: dict[str, list[Item]] = {}
    for source in spec.sources:
        found: list[Item] = []
        read_items(tokenize((root / source).read_text()), source, found)
        for item in found:
            items.setdefault(item.name, []).append(item)
    schema: dict[str, str] = {}
    seen: set[str] = set()
    pending = [(name, None) for name in spec.roots]
    missing: dict[str, str] = {}
    ordered = spec.encoding == "postcard"

    def want(tokens: list[Token], params: list[str], at: str, from_path: str) -> None:
        for name in named_types(tokens):
            if name in params or name in STANDARD or name in spec.opaque:
                continue
            if name in items:
                pending.append((name, from_path))
            elif name[:1].isupper():
                missing.setdefault(name, at)

    def resolve(name: str, from_path: str | None) -> Item:
        candidates = items[name]
        if len(candidates) > 1:
            local = [c for c in candidates if c.path == from_path]
            if len(local) == 1:
                return local[0]
            places = ", ".join(sorted({c.path for c in candidates}))
            raise InterfaceError(f"`{name}` is defined more than once ({places})")
        return candidates[0]

    def field_line(
        f: Field, index: int, rule: str | None, item: Item, owner: str
    ) -> tuple[str, str] | None:
        params, path = item.params, item.path
        defaulted = "default" in item.serde.flags or "default" in item.serde.values
        if "skip" in f.serde.flags or "skip_deserializing" in f.serde.flags:
            return None
        wire = f.serde.values.get("rename", renamed(f.name, rule))
        ty = type_text(f.ty)
        notes = []
        if ordered:
            notes.append(f"#{index}")
        notes.append(ty)
        optional = defaulted or "default" in f.serde.flags or "default" in f.serde.values
        if not ordered and (optional or ty.startswith("Option<")):
            notes.append("optional")
        if "flatten" in f.serde.flags:
            notes.append("flatten")
        for key in ("with", "deserialize_with"):
            if key in f.serde.values:
                notes.append(f"{key}={f.serde.values[key]}")
        want(f.ty, params, f"{owner}.{wire}", path)
        return f"{owner}.{wire}", " ".join(notes)

    while pending:
        name, from_path = pending.pop(0)
        if name in seen:
            continue
        seen.add(name)
        if name not in items:
            missing.setdefault(name, "roots")
            continue
        item = resolve(name, from_path)
        serde = item.serde
        container = []
        for key in ("tag", "content", "from", "try_from", "into"):
            if key in serde.values:
                container.append(f"{key}={serde.values[key]}")
                if key in ("from", "try_from"):
                    want(tokenize(serde.values[key]), item.params, name, item.path)
        for flag in ("untagged", "transparent", "deny_unknown_fields"):
            if flag in serde.flags:
                container.append(flag)
        if item.kind == "alias":
            schema[name] = f"alias {type_text(item.target)}"
            want(item.target, item.params, name, item.path)
            continue
        if item.kind == "struct":
            if item.shape == "unit":
                schema[name] = " ".join(["unit", *container])
                continue
            if item.shape == "tuple":
                inner = ", ".join(type_text(t) for t in item.types)
                schema[name] = " ".join([f"tuple({inner})", *container])
                for t in item.types:
                    want(t, item.params, name, item.path)
                continue
            schema[name] = " ".join(["struct", *container])
            rule = serde.values.get("rename_all")
            for index, f in enumerate(item.fields):
                line = field_line(f, index, rule, item, name)
                if line:
                    schema[line[0]] = line[1]
            continue
        style = "external"
        if "untagged" in serde.flags:
            style = "untagged"
        elif "tag" in serde.values:
            style = "internal"
        schema[name] = " ".join(["enum", style, *[c for c in container if c != "untagged"]])
        rule = serde.values.get("rename_all")
        field_rule = serde.values.get("rename_all_fields")
        index = 0
        for variant in item.variants:
            if "skip" in variant.serde.flags or "skip_deserializing" in variant.serde.flags:
                continue
            wire = variant.serde.values.get("rename", renamed(variant.name, rule, variant=True))
            owner = f"{name}::{wire}"
            notes = [f"#{index}"] if ordered else []
            index += 1
            if variant.shape == "unit":
                notes.append("unit")
            elif variant.shape == "tuple":
                notes.append(f"tuple({', '.join(type_text(t) for t in variant.types)})")
                for t in variant.types:
                    want(t, item.params, owner, item.path)
            else:
                notes.append("struct")
            if "other" in variant.serde.flags:
                notes.append("other")
            for key in ("with", "deserialize_with"):
                if key in variant.serde.values:
                    notes.append(f"{key}={variant.serde.values[key]}")
            schema[owner] = " ".join(notes)
            vrule = variant.serde.values.get("rename_all", field_rule)
            for findex, f in enumerate(variant.fields):
                line = field_line(f, findex, vrule, item, owner)
                if line:
                    schema[line[0]] = line[1]
    if missing:
        listed = ", ".join(f"`{name}` (in {at})" for name, at in sorted(missing.items()))
        raise InterfaceError(
            f"{spec.path.relative_to(root) if spec.path.is_relative_to(root) else spec.path}: "
            f"types the sources do not define: {listed}; add the file that defines each to "
            "`sources`, or list it under [opaque] with what it is"
        )
    return schema


# ---------------------------------------------------------------------------
# Cap'n Proto


_CAPNP_TOKEN = re.compile(r'"(?:[^"\\]|\\.)*"|@0x[0-9a-fA-F]+|@[0-9]+|0x[0-9a-fA-F]+|[A-Za-z_][A-Za-z0-9_.]*|->|\S')


def capnp_tokens(text: str) -> list[str]:
    text = re.sub(r"#[^\n]*", "", text)
    return _CAPNP_TOKEN.findall(text)


def capnp_schema(path: str, root: Path = ROOT) -> dict[str, str]:
    """`Scope.member` lines for every struct field, union and group member,
    interface method and enumerant in a `.capnp` file."""
    tokens = capnp_tokens((root / path).read_text())
    schema: dict[str, str] = {}
    scopes: list[tuple[str, str]] = []  # (name, kind)
    i = 0

    def scope_name() -> str:
        return ".".join(name for name, _ in scopes if name)

    while i < len(tokens):
        t = tokens[i]
        if t in ("struct", "interface", "enum") and i + 1 < len(tokens):
            name = tokens[i + 1]
            j = i + 2
            while tokens[j] != "{":
                j += 1
            scopes.append((name, t))
            schema[scope_name()] = t
            i = j + 1
            continue
        if t == "union" and tokens[i + 1] == "{":
            scopes.append(("", "union"))
            i += 2
            continue
        if t == "}":
            if scopes:
                scopes.pop()
            i += 1
            continue
        if t in ("using", "const", "annotation") or t.startswith("@0x"):
            while i < len(tokens) and tokens[i] != ";":
                i += 1
            i += 1
            continue
        if scopes and i + 1 < len(tokens) and tokens[i + 1].startswith("@"):
            name, ordinal = t, tokens[i + 1]
            kind = next((k for _, k in reversed(scopes) if k != "union"), "struct")
            j = i + 2
            body: list[str] = []
            while j < len(tokens) and tokens[j] not in (";", "{"):
                body.append(tokens[j])
                j += 1
            key = f"{scope_name()}.{name}"
            if j < len(tokens) and tokens[j] == "{":
                # A named group or union member.
                schema[key] = f"{ordinal} {' '.join(body)}".strip()
                scopes.append((name, "struct"))
                i = j + 1
                continue
            text = re.sub(r"\s*([:(),\[\]<>=])\s*", r"\1", " ".join(body))
            schema[key] = f"{ordinal} {text}".strip() if kind != "enum" else ordinal
            i = j + 1
            continue
        if scopes and i + 2 < len(tokens) and tokens[i + 1] == ":" and tokens[i + 2] in ("group", "union"):
            name = t
            j = i + 3
            schema[f"{scope_name()}.{name}"] = tokens[i + 2]
            scopes.append((name, "struct"))
            i = j + 1
            continue
        i += 1
    return {f"capnp:{Path(path).name}:{key}": value for key, value in schema.items()}


def schema_of(spec: Spec, root: Path = ROOT) -> dict[str, str]:
    schema = rust_schema(spec, root) if spec.roots else {}
    for path in spec.capnp:
        schema.update(capnp_schema(path, root))
    return dict(sorted(schema.items()))


# ---------------------------------------------------------------------------
# Compatibility


def owner_of(key: str) -> str:
    """The type a schema line belongs to."""
    if key.startswith("capnp:"):
        prefix, _, rest = key.partition(":")[2].partition(":")
        return f"capnp:{prefix}:{rest.rsplit('.', 1)[0] if '.' in rest else rest}"
    head = key.split(".", 1)[0]
    return head.split("::", 1)[0]


def top_type(key: str) -> str:
    if key.startswith("capnp:"):
        _, file, rest = key.split(":", 2)
        return f"capnp:{file}:{rest.split('.', 1)[0]}"
    return key.split(".", 1)[0].split("::", 1)[0]


def incompatible(before: dict[str, str], now: dict[str, str], encoding: str) -> list[str]:
    """Why `now` cannot talk to `before`, one reason per line; empty when it
    can."""
    reasons = []
    old_types = {top_type(key) for key in before}
    for key, value in before.items():
        if key not in now:
            reasons.append(f"`{key}` is gone")
        elif now[key] != value:
            reasons.append(f"`{key}` is now `{now[key]}`, was `{value}`")
    for key, value in now.items():
        if key in before:
            continue
        if top_type(key) not in old_types:
            continue  # a new type: judged where it is used
        if key.startswith("capnp:"):
            parent = key.rsplit(".", 1)[0]
            if before.get(parent) in ("struct", "union", "group"):
                continue
            reasons.append(f"`{key}` is new ({value})")
            continue
        parent = key.rsplit(".", 1)[0] if "." in key else None
        added_field = parent is not None and parent in before
        if (
            encoding == "json"
            and added_field
            and "optional" in value.split()
            and "deny_unknown_fields" not in before.get(owner_of(key), "")
        ):
            continue
        reasons.append(f"`{key}` is new ({value})")
    return reasons


# ---------------------------------------------------------------------------
# Files


def interface_files(root: Path = ROOT) -> list[Path]:
    """Every committed interface: one per owning crate, at its root."""
    return sorted(root.glob(f"crates/*/*/{FILE}"))


def toml_string(value: str) -> str:
    return '"' + value.replace("\\", "\\\\").replace('"', '\\"') + '"'


def render(spec: Spec, schema: dict[str, str], authored: str) -> str:
    """The file: the authored header with `version` rewritten, then the
    schema."""
    head = re.sub(r"(?m)^version\s*=.*$", f"version = {spec.version}", authored, count=1)
    lines = [head.rstrip() + "\n", "\n[schema]\n"]
    for key, value in schema.items():
        lines.append(f"{toml_string(key)} = {toml_string(value)}\n")
    return "".join(lines)


def authored_part(text: str) -> str:
    before, _, _ = text.partition("\n[schema]")
    if not before.startswith(HEADER):
        before = HEADER + before.lstrip()
    return before


def served_versions(root: Path = ROOT) -> dict[str, int]:
    data = tomllib.loads((root / SYSTEM_INTERFACES).read_text())
    return dict(data.get("interfaces", {}))


def check(root: Path = ROOT) -> list[str]:
    problems = []
    served = served_versions(root)
    files = interface_files(root)
    if not files:
        problems.append(f"no {FILE} is committed")
    for path in files:
        rel = path.relative_to(root)
        try:
            spec, committed = load_spec(path)
            declared = code_version(spec, root)
            schema = schema_of(spec, root)
        except (InterfaceError, KeyError, OSError, tomllib.TOMLDecodeError) as error:
            problems.append(f"{rel}: {error}")
            continue
        if declared != spec.version:
            problems.append(
                f"{rel}: records version {spec.version}, the code declares {declared} "
                f"({spec.version_const}); run `python3 tools/policy/check_interfaces.py write`"
            )
        if schema != committed:
            changed = sorted(set(schema.items()) ^ set(committed.items()))
            sample = ", ".join(f"`{key}`" for key, _ in changed[:5])
            problems.append(
                f"{rel}: the code's {spec.name} interface differs from the committed schema "
                f"({sample}{', ...' if len(changed) > 5 else ''}); review it and run "
                "`python3 tools/policy/check_interfaces.py write`"
            )
        if spec.serves and served.get(spec.name) != declared:
            problems.append(
                f"{SYSTEM_INTERFACES}: serves {spec.name} = {served.get(spec.name)}, "
                f"the code declares {declared}"
            )
    return problems


def write(root: Path = ROOT) -> list[str]:
    problems = []
    for path in interface_files(root):
        try:
            spec, _ = load_spec(path)
            spec.version = code_version(spec, root)
            schema = schema_of(spec, root)
        except (InterfaceError, KeyError, OSError) as error:
            problems.append(f"{path.relative_to(root)}: {error}")
            continue
        path.write_text(render(spec, schema, authored_part(path.read_text())))
    return problems


def at_revision(root: Path, revision: str, rel: str) -> str | None:
    """`rel` as `revision` has it; `None` where it has no such file."""
    shown = subprocess.run(
        ["git", "show", f"{revision}:{rel}"],
        cwd=root,
        capture_output=True,
    )
    return shown.stdout.decode() if shown.returncode == 0 else None


def against(base: str, root: Path = ROOT) -> list[str]:
    problems = []
    for path in interface_files(root):
        rel = path.relative_to(root).as_posix()
        old_text = at_revision(root, base, rel)
        if old_text is None:
            continue
        old = tomllib.loads(old_text)
        new = tomllib.loads(path.read_text())
        reasons = incompatible(old.get("schema", {}), new.get("schema", {}), new.get("encoding", "json"))
        if reasons and int(new["version"]) <= int(old["version"]):
            shown = "; ".join(reasons[:8]) + ("; ..." if len(reasons) > 8 else "")
            problems.append(
                f"{rel}: the {new['name']} interface changed incompatibly ({shown}) without a new "
                f"version; bump {new['version_const']} past {old['version']}, run "
                "`python3 tools/policy/check_interfaces.py write`, and update the other side"
            )
    return problems


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__.split("\n", 1)[0])
    sub = parser.add_subparsers(dest="command", required=True)
    sub.add_parser("check")
    sub.add_parser("write")
    base = sub.add_parser("against")
    base.add_argument("base")
    args = parser.parse_args(argv)
    if args.command == "check":
        problems = check()
    elif args.command == "write":
        problems = write()
    else:
        problems = against(args.base)
    for problem in problems:
        print(f"::error::{problem}", file=sys.stderr)
    return 1 if problems else 0


if __name__ == "__main__":
    sys.exit(main())
