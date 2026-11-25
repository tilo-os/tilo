#!/usr/bin/env python3
"""A versioned on-disk format names the layout it reads.

Stored formats change with the code (docs/architecture/compatibility.md). A
program that writes a file in a format of its own declares it as a
``tilo_files::native::migration::Format``: its version, the oldest version it
still brings up, and one step per version in between. A layout change keeps
the step that brings up what the build before it wrote, and deletes the steps
for anything older:

``check`` (the repository policy suite) fails when a declaration's
``oldest`` is not a plain number, which the comparison below needs, and when
no loader brings the format's stored versions up: a declared chain of steps
only keeps data if the code that opens the file runs it. Each format's crate
must call ``<FORMAT>.bring_up(path, bytes, stored)`` (or ``bring_up_moved``
for a version that lived in a file of its own) with the version it read from
the file, not a number it assumed, so a file of another version is refused
rather than read as this one; and no loader calls ``migrate``, which brings
bytes up but keeps no copy of what the file held. A loader that brought up
only the first version's file and read a later one as it was refused a whole
server's state at start.

``against BASE`` (run against main before landing) fails when the golden bytes
of a version this build still declares changed, and when a format no longer
brings up the version BASE writes: every device that updates from BASE holds
files of that version, and a build that refuses them leaves the device without
what it kept (a desktop that refused its own workspaces' entries opened none and
kept nothing). A format may bring up fewer versions than BASE did, and an older
one may be gone with its step: that is a landing, not a regression.

``check --stable REF`` (``origin/stable`` by default) fails the same way
against the released build: a device updates from the release, not from main,
so no format's ``oldest`` may rise above the version the release writes, however
many main builds in between brought it up. A missing ``origin/<branch>`` is
fetched first; one that can't be fetched fails the check rather than skipping it.

Each step is tested against a file its previous version wrote, and only that
(``assert_step_reads_previous``, beside each declaration).

A version is pinned by the bytes it wrote. Postcard reads a field added,
removed or retyped without a word as long as the types line up, so a layout
that changed under the same version number goes unnoticed until a machine's
file stops reading (a device's certificate, a workspace's record and a
server's invitations each did). So ``check`` also fails a format whose crate
keeps no ``golden/<name>/v<version>.hex`` (the name lowercased, each run of
other characters a ``-``; the bytes as lowercase hex pairs, 16 to a line), or no test of the crate that reads that path: the
test hands it to ``assert_golden_bytes``, which fails when this build reads
or writes it differently. And ``against BASE`` fails when a golden file of a version this
build still declares changed: what a version wrote never changes, so a layout
change is a new version, with its own golden file. The golden file of a
version no declaration names any more goes with its step.

A format's golden bytes pin what its loader reads first, not every type written
beside it. A journal appended beside a snapshot, a frame a store keeps as a
file: each is a Rust type serialized to disk, and a field added in its middle
reads the bytes an earlier build wrote as something else (a device's journal
did, and every write after its snapshot was dropped). Such a type is listed in
``tools/policy/stored_layouts.toml`` under the format whose version covers it,
with a digest of its definition at that version. A digest covers the types the
listed definition names, recursively, resolved to the definition in its own
crate first and then to the one definition under ``crates/`` or ``apps/``: a
field added inside a type one of them reaches changes the digest of the root
that reaches it. A name with more than one definition is skipped, as is one
with none (a std or external type).

``check`` fails when a listed definition no longer matches the digest recorded
for the format's current version: a layout that changed is a new version of its
format, with the step that brings up what the version before wrote. ``pin``
writes the digest of the current version once that version is declared.
``against BASE`` fails when the digest a still-declared version had at BASE
changed.

A format is not always one ``migration::Format``. One that keeps its version
in a plain constant, and opens a state of another version by refusing it, is
declared with a ``[[format]]`` table whose ``version`` names the path and the
constant (``"crate/src/file.rs:CONST"``): its layout is pinned the same way,
and a change under the same number fails. A format with no version at all,
such as the frames a store writes without a header, is declared
``version = "none"``: any change to a listed type fails, because a later build
cannot tell the bytes apart.

The digests' meaning is written at the top of the file (``schema``). A layout
recorded under another schema is compared by the check that wrote it, not this
one: ``against BASE`` leaves it alone when BASE's schema is another.

``check_wire_enums.py`` reads the same closure: an enum a stored root reaches
is stored, so its variants move only with a new version of its format, exactly
as a listed enum's.
"""

from __future__ import annotations

import argparse
import hashlib
import os
import re
import subprocess
import sys
import tomllib
from collections import deque
from concurrent.futures import ThreadPoolExecutor
from functools import lru_cache
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
from rust_source import is_test_file, lex, split_top_level, strip_tests  # noqa: E402

ROOT = Path(__file__).resolve().parents[2]
DECLARATION = re.compile(
    r"(?<!\w)Format\s*\{\s*name:\s*\"(?P<name>[^\"]+)\"\s*,\s*version:\s*[^,]+,\s*"
    r"oldest:\s*(?P<oldest>[^,]+?)\s*,",
    re.S,
)


def declarations(texts: dict[str, str]) -> dict[str, tuple[str, str]]:
    """Format name -> (file, oldest as written)."""
    found = {}
    for path, text in texts.items():
        for match in DECLARATION.finditer(text):
            found[match.group("name")] = (path, match.group("oldest").strip())
    return found


BINDING = re.compile(
    r"\bconst\s+(?P<binding>[A-Z][A-Z0-9_]*)\s*:\s*(?:[\w:]+::)?Format\s*=\s*"
    r"(?:[\w:]+::)?Format\s*\{\s*name:\s*\"(?P<name>[^\"]+)\"",
    re.S,
)
CALL = r"\s*\.\s*(?P<call>bring_up_moved|bring_up|migrate)\s*\("


VERSIONED = re.compile(
    r"(?<!\w)Format\s*\{\s*name:\s*\"(?P<name>[^\"]+)\"\s*,\s*version:\s*(?P<version>[^,]+?)\s*,",
    re.S,
)
GOLDEN = re.compile(r"(?:^|/)golden/[^/]+/v[0-9]+\.hex$")


def slug(name: str) -> str:
    """The directory a format's golden bytes are kept in."""
    return re.sub(r"[^a-z0-9]+", "-", name.lower()).strip("-")


def crate_sources(crate: Path, root: Path, cache: dict[Path, dict[str, str]]) -> dict[str, str]:
    """Every Rust file of `crate`, by its path relative to `root`."""
    if crate not in cache:
        cache[crate] = {
            file.relative_to(root).as_posix(): file.read_text(errors="replace")
            for file in crate.glob("**/*.rs")
        }
    return cache[crate]


def version_of(expression: str, texts: dict[str, str]) -> int | None:
    """The number a declaration's `version:` is: written out, or a `u32`
    constant of its crate by its last path segment."""
    expression = expression.strip()
    if expression.isdigit():
        return int(expression)
    name = expression.rsplit("::", 1)[-1]
    if not re.fullmatch(r"[A-Z][A-Z0-9_]*", name):
        return None
    for text in texts.values():
        found = re.search(rf"\bconst\s+{name}\s*:\s*u32\s*=\s*([0-9]+)\s*;", text)
        if found:
            return int(found.group(1))
    return None


def goldens(root: Path, texts: dict[str, str]) -> list[str]:
    """Each declared format whose crate keeps no golden bytes for its
    version, or no test that reads them."""
    problems = []
    cache: dict[Path, dict[str, str]] = {}
    for path, text in texts.items():
        for match in VERSIONED.finditer(without_tests(path, text)):
            name = match.group("name")
            crate = crate_of(root, path)
            crate_texts = crate_sources(crate, root, cache)
            version = version_of(match.group("version"), crate_texts)
            if version is None:
                problems.append(
                    f"{path}: format {name!r} declares `version: {match.group('version')}`; "
                    "write the number, or a `const NAME: u32 = N;` of its crate, so its golden "
                    "bytes can be found"
                )
                continue
            kept = f"golden/{slug(name)}/v{version}.hex"
            shown = (crate / kept).relative_to(root).as_posix()
            if not (crate / kept).is_file():
                problems.append(
                    f"{path}: format {name!r} version {version} keeps no golden bytes at "
                    f"{shown}: write what this build stores there, and a test that passes it to "
                    "`assert_golden_bytes`, so a layout that changes under this number fails"
                )
            elif not any(kept in other for other in crate_texts.values()):
                problems.append(
                    f"{path}: no test of its crate reads {shown}: pass it to "
                    "`assert_golden_bytes`, which fails when this build reads or writes the "
                    f"bytes format {name!r} version {version} stored differently"
                )
    return problems


def declared_goldens(
    root: Path, texts: dict[str, str], cache: dict[Path, dict[str, str]] | None = None
) -> list[str]:
    """The golden paths the formats declared now name, by the versions they
    declare now."""
    kept = []
    cache = {} if cache is None else cache
    for path, text in texts.items():
        for match in VERSIONED.finditer(without_tests(path, text)):
            crate = crate_of(root, path)
            version = version_of(match.group("version"), crate_sources(crate, root, cache))
            if version is not None:
                kept.append(
                    (crate / f"golden/{slug(match.group('name'))}/v{version}.hex")
                    .relative_to(root)
                    .as_posix()
                )
    return kept


def without_tests(path: str, text: str) -> str:
    """`text` without its tests: nothing for a test file, else everything
    before its first inline `#[cfg(test)] mod … {`, where this tree keeps a
    file's tests."""
    name = path.rsplit("/", 1)[-1]
    if "/tests/" in path or "/benches/" in path or "/examples/" in path:
        return ""
    if name.endswith("tests.rs") or name in ("test_support.rs", "fixtures.rs"):
        return ""
    inline = re.search(r"#\[cfg\(test\)\]\s*(?:pub(?:\([\w:]+\))?\s+)?mod\s+\w+\s*\{", text)
    return text[: inline.start()] if inline else text


def arguments(text: str, start: int) -> list[str]:
    """The top-level, comma-separated arguments of the call whose opening
    parenthesis is just before `start`."""
    depth, current, found = 0, [], []
    for char in text[start:]:
        if char in "([{":
            depth += 1
        elif char in ")]}":
            if depth == 0:
                found.append("".join(current).strip())
                return [argument for argument in found if argument]
            depth -= 1
        elif char == "," and depth == 0:
            found.append("".join(current).strip())
            current = []
            continue
        current.append(char)
    return found


def crate_of(root: Path, path: str) -> Path:
    """The directory of the crate `path` (relative to `root`) belongs to, or
    the file's own directory when no manifest is above it."""
    directory = (root / path).parent
    while directory != root and not (directory / "Cargo.toml").is_file():
        directory = directory.parent
    return directory if directory != root else (root / path).parent


def loaders(root: Path, texts: dict[str, str]) -> list[str]:
    """Each declared format whose crate opens its file without bringing every
    stored version up through `bring_up`, or brings one up with `migrate`."""
    problems = []
    crates: dict[Path, dict[str, str]] = {}
    for path, text in texts.items():
        for match in BINDING.finditer(text):
            binding, name = match.group("binding"), match.group("name")
            crate = crate_of(root, path)
            if crate not in crates:
                crates[crate] = {
                    file.relative_to(root).as_posix(): file.read_text(errors="replace")
                    for file in crate.glob("**/*.rs")
                }
            stem = path.rsplit("/", 1)[-1].removesuffix(".rs")
            named = []
            for other, other_text in crates[crate].items():
                code = without_tests(other, other_text)
                bare = other == path or re.search(rf"\buse\b[^;]*\b{stem}::{binding}\b", code)
                prefix = rf"(?<![\w:]){binding}" if bare else rf"\b{stem}::{binding}"
                for call in re.finditer(prefix + CALL, code):
                    named.append((other, call.group("call"), arguments(code, call.end())))
            for other, call, args in named:
                if call == "migrate":
                    problems.append(
                        f"{other}: format {name!r} is brought up with `migrate`, which keeps no "
                        "copy of what the file held; a loader calls `bring_up` (or "
                        "`bring_up_moved` for a version kept in a file of its own)"
                    )
            if not any(call != "migrate" and args and not args[-1].isdigit() for _, call, args in named):
                problems.append(
                    f"{path}: format {name!r} declares the versions it brings up, but no code "
                    f"of its crate passes the version it read from the file to "
                    f"`{binding}.bring_up`: a file an older build wrote would be read as it is, or "
                    "refused. Its loader reads the stored version and brings it up before it reads."
                )
    return problems


def sources(root: Path) -> dict[str, str]:
    texts = {}
    for path in root.glob("crates/**/*.rs"):
        if "/target/" in path.as_posix():
            continue
        text = path.read_text(errors="replace")
        if "migration::Format" in text:
            texts[path.relative_to(root).as_posix()] = text
    for path in root.glob("apps/**/*.rs"):
        text = path.read_text(errors="replace")
        if "migration::Format" in text:
            texts[path.relative_to(root).as_posix()] = text
    return texts


def sources_at(root: Path, revision: str) -> dict[str, str]:
    listed = subprocess.run(
        ["git", "grep", "-l", "migration::Format", revision, "--", "*.rs"],
        cwd=root,
        capture_output=True,
        text=True,
    )
    texts = {}
    for line in listed.stdout.splitlines():
        _, _, path = line.partition(":")
        shown = subprocess.run(
            ["git", "show", f"{revision}:{path}"], cwd=root, capture_output=True, text=True
        )
        if shown.returncode == 0:
            texts[path] = shown.stdout
    return texts


def sources_since(root: Path, base: str, before: dict[str, str]) -> dict[str, str]:
    """What declares a format now: `before` (the sources at `base`) with the
    files changed since, and the new ones git doesn't track yet, read again.
    Only those: reading every file of the tree took most of the check on a
    node whose disk is busy."""
    def listed(*arguments: str) -> list[str]:
        out = subprocess.run(
            ["git", *arguments, "--", "*.rs"], cwd=root, capture_output=True, text=True, check=True
        ).stdout
        return [path for path in out.split("\0") if path]

    texts = dict(before)
    changed = listed("diff", "--name-only", "-z", "--no-renames", base)
    for path in changed + listed("ls-files", "-z", "--others", "--exclude-standard"):
        texts.pop(path, None)
        file = root / path
        if file.is_file():
            text = file.read_text(errors="replace")
            if "migration::Format" in text:
                texts[path] = text
    return texts


LAYOUTS = Path("tools/policy/stored_layouts.toml")

# The items a stored layout may name: a type written to disk, a function whose
# output is kept there (a derived key), a constant it is derived with, or a
# type alias a listed definition names.
ITEM = re.compile(
    r"(?m)^(?P<attrs>(?:[ \t]*#\[[^\n]*\]\s*\n)*)[ \t]*"
    r"(?:pub(?:\([^)]*\))?\s+)?"
    r"(?:(?:const|async|unsafe|default|extern(?:\s+\"[^\"]*\")?)\s+)*"
    r"(?P<kind>struct|enum|union|trait|fn|const|static|type)\s+"
    r"(?:mut\s+)?(?P<name>[A-Za-z_][A-Za-z0-9_]*)\b"
)
# A name a stored definition may reach: a type is written in CamelCase.
TYPE_NAME = re.compile(r"\b[A-Z][A-Za-z0-9_]*\b")


@lru_cache(maxsize=512)
def definition_region(text: str) -> tuple[str, str]:
    """`text` and the same text, both without their test items: the first with
    comments blanked and literals kept (what is stored), the second with both
    blanked (where its brackets are)."""
    blanked, literals = lex(text)
    pruned = strip_tests(text, blanked)
    kept = list(blanked)
    for literal in literals:
        kept[literal.start : literal.end] = text[literal.start : literal.end]
    for start, end in pruned.removed:
        for at in range(start, end):
            if kept[at] != "\n":
                kept[at] = " "
    return "".join(kept), pruned.code


def without_variant_names(body: str) -> str:
    """An enum's body without each variant's name: its payloads and fields are
    left, and their types are what the enum names."""
    opening = body.find("{")
    closing = body.rfind("}")
    if opening < 0 or closing < opening:
        return body
    parts = []
    for part in split_top_level(body[opening + 1 : closing]):
        part = re.sub(r"^(?:\s*#\[[^\]]*\]\s*)*", "", part)
        name = re.match(r"[A-Za-z_][A-Za-z0-9_]*", part)
        parts.append(part[name.end() :] if name else part)
    return body[: opening + 1] + ",".join(parts) + body[closing:]


def item_references(body: str, kind: str) -> list[str]:
    """The type names a definition's body uses: its fields' and parameters'
    types, its return type, the types of its variants' payloads and the types
    it constructs. Field names, variant names and calls are no types."""
    text = re.sub(r"#\[[^\]]*\]", " ", body)
    text = re.sub(r"\b[A-Za-z_][A-Za-z0-9_]*\s*:", " ", text)
    text = re.sub(r"\b[A-Za-z_][A-Za-z0-9_]*\s*\(", " ", text)
    if kind == "enum":
        text = without_variant_names(text)
    return TYPE_NAME.findall(text)


def item(text: str, name: str) -> tuple[str | None, list[str]]:
    """The definition of item `name` in `text` (a struct, enum, union, trait,
    function, constant, static or type alias), with the serde attributes on it
    and its fields, without comments or doc text, its whitespace collapsed:
    what decides the bytes it is stored as, or the value it derives; and the
    type names its body uses."""
    code, scan = definition_region(text)
    found = None
    for match in ITEM.finditer(code):
        if match.group("name") == name:
            found = match
            break
    if found is None:
        return None, []
    # A body ends at its closing brace; a declaration at its `;`. Parens and
    # brackets only nest: a function's parameters close before its body, and a
    # tuple body's `)` before its `;`. Angles are no brackets either:
    # `-> Result<Vec<u8>, _>` would unbalance a count of them.
    depth, end = 0, None
    for at in range(found.end(), len(scan)):
        char = scan[at]
        if char in "([{":
            depth += 1
        elif char in ")]":
            depth = max(0, depth - 1)
        elif char == "}":
            depth -= 1
            if depth == 0:
                end = at + 1
                break
        elif char == ";" and depth == 0:
            end = at + 1
            break
    if end is None:
        return None, []
    attrs = " ".join(
        attr for attr in re.findall(r"#\[[^\n]*\]", found.group("attrs")) if "serde" in attr
    )
    body = code[found.start("kind") : end]
    definition = re.sub(r"\s+", " ", f"{attrs} {body}").strip()
    return definition, item_references(scan[found.start("kind") : end], found.group("kind"))


def item_definition(text: str, name: str) -> str | None:
    """The definition of item `name` in `text`, as [`item`] reads it."""
    return item(text, name)[0]


def crate_path(path: str) -> str:
    """The crate directory a tree-relative source path belongs to."""
    head, separator, _ = path.partition("/src/")
    return head if separator else path.rsplit("/", 1)[0]


class Definitions:
    """Every definition under `crates/` and `apps/`, by the names its files
    declare: what a stored definition reaches, resolved the same way each time.

    A name is resolved to the one definition in the referencing file's crate,
    else to the one definition in the tree. A name with more than one in its
    crate, or none in its crate and more than one in the tree, is ambiguous
    and skipped; one with none anywhere is std or external and skipped.
    Test sources are no definitions: a fixture's `Entry` is not the one stored.
    """

    def __init__(self, root: Path):
        self.root = root
        self._names: dict[str, list[str]] | None = None
        self._texts: dict[str, str | None] = {}
        self._items: dict[tuple[str, str], tuple[str | None, list[str]]] = {}
        self._verified: dict[str, list[str]] = {}

    def text(self, path: str) -> str | None:
        if path not in self._texts:
            file = self.root / path
            try:
                self._texts[path] = file.read_text(errors="replace") if file.is_file() else None
            except OSError:
                self._texts[path] = None
        return self._texts[path]

    def _scan(self) -> dict[str, list[str]]:
        if self._names is not None:
            return self._names
        files = []
        for base in ("crates", "tools", "apps"):
            directory = self.root / base
            if not directory.is_dir():
                continue
            for file in sorted(directory.rglob("*.rs")):
                relative = file.relative_to(self.root)
                if "target" in relative.parts or is_test_file(file):
                    continue
                files.append(relative.as_posix())

        def read(path: str) -> tuple[str, str | None]:
            try:
                return path, (self.root / path).read_text(errors="replace")
            except OSError:
                return path, None

        workers = min(16, max(1, (os.cpu_count() or 1) * 2))
        with ThreadPoolExecutor(max_workers=workers) as pool:
            for path, text in pool.map(read, files):
                self._texts[path] = text
        names: dict[str, list[str]] = {}
        for path in files:
            text = self._texts[path]
            if text is None:
                continue
            for match in ITEM.finditer(text):
                names.setdefault(match.group("name"), []).append(path)
        self._names = names
        return names

    def _item(self, path: str, name: str) -> tuple[str | None, list[str]]:
        key = (path, name)
        if key not in self._items:
            text = self.text(path)
            self._items[key] = item(text, name) if text is not None else (None, [])
        return self._items[key]

    def definition(self, path: str, name: str) -> str | None:
        return self._item(path, name)[0]

    def references(self, path: str, name: str) -> list[str]:
        return self._item(path, name)[1]

    def verified(self, name: str) -> list[str]:
        """The files that define `name` in production code. A match in a test
        module is none."""
        if name not in self._verified:
            found = []
            for path in self._scan().get(name, []):
                if self.definition(path, name) is not None:
                    found.append(path)
            self._verified[name] = found
        return self._verified[name]

    def resolve(self, name: str, from_path: str) -> str | None:
        """The file whose definition `name` means when named in `from_path`:
        the one in its crate first, else the one in the tree."""
        found = self.verified(name)
        if not found:
            return None
        crate = crate_path(from_path)
        local = [path for path in found if crate_path(path) == crate]
        if len(local) == 1:
            return local[0]
        if local:
            return None
        return found[0] if len(found) == 1 else None


def definitions_for(root: Path) -> Definitions:
    """The definitions `root` holds now: scanned once per call, so a check
    that reads a tree being edited sees its files as they are."""
    return Definitions(Path(root))


def reached_definitions(
    entries: list[str], definitions: Definitions
) -> tuple[dict[tuple[str, str], str], list[str]]:
    """The definition of each `path:Name` in `entries`, then of every type it
    names, recursively; with the entries that name no definition."""
    found: dict[tuple[str, str], str] = {}
    missing: list[str] = []
    queue = deque()
    for entry in entries:
        path, _, name = entry.partition(":")
        queue.append((path, name))
    while queue:
        path, name = queue.popleft()
        key = (path, name)
        if key in found:
            continue
        definition = definitions.definition(path, name)
        if definition is None:
            missing.append(f"{path}:{name}")
            continue
        found[key] = definition
        if len(found) > 4096:
            continue
        for reference in definitions.references(path, name):
            target = definitions.resolve(reference, path)
            if target is not None:
                target_key = (target, reference)
                if target_key not in found:
                    queue.append(target_key)
    return found, missing


def layout_digest(entries: list[str], definitions: Definitions) -> tuple[str | None, list[str]]:
    """The digest of `entries` (each `path:Name`) and what they reach, or
    `None` with what was not found."""
    found, missing = reached_definitions(entries, definitions)
    if missing:
        return None, missing
    # The definitions alone, not the files that hold them: moving a type to another file or
    # crate changes no byte it writes.
    parts = sorted(f"{name}={definition}" for (_path, name), definition in found.items())
    return hashlib.sha256("\n".join(parts).encode()).hexdigest()[:16], []


def layouts(text: str) -> list[dict]:
    return tomllib.loads(text).get("layout", [])


def formats(text: str) -> list[dict]:
    return tomllib.loads(text).get("format", [])


def schema(text: str) -> int:
    """The meaning of the digests in a `stored_layouts.toml`."""
    return int(tomllib.loads(text).get("schema", 1))


def constant_version(root: Path, written: str) -> int | None:
    """The number a `[[format]]` `version = "<path>:<CONST>"` names, if that
    constant is a `u32` of the file."""
    path, _, constant = written.partition(":")
    if not path or not constant:
        return None
    file = root / path
    if not file.is_file():
        return None
    found = re.search(
        rf"\bconst\s+{re.escape(constant)}\s*:\s*u32\s*=\s*([0-9]+)\s*;",
        file.read_text(errors="replace"),
    )
    return int(found.group(1)) if found else None


def format_tables(root: Path) -> dict[str, str]:
    """Each `[[format]]` of `stored_layouts.toml`, by name, as written."""
    file = root / LAYOUTS
    if not file.is_file():
        return {}
    return {table["name"]: str(table.get("version", "")).strip() for table in formats(file.read_text())}


def format_problems(root: Path) -> list[str]:
    """Each `[[format]]` whose `version` names neither `none` nor a `u32`
    constant that exists."""
    problems = []
    for name, written in sorted(format_tables(root).items()):
        if written == "none" or constant_version(root, written) is not None:
            continue
        if written.isdigit():
            continue
        problems.append(
            f"{LAYOUTS}: format {name!r} declares `version = \"{written}\"`; name "
            "`<path>:<CONST>` of a `const NAME: u32 = N;`, or `none` for a format "
            "written with no version at all"
        )
    return problems


def declared_format_versions(root: Path) -> tuple[dict[str, int], set[str]]:
    """The versions `[[format]]` tables declare, by name; and the names of
    those that declare no version."""
    versions: dict[str, int] = {}
    unversioned: set[str] = set()
    for name, written in format_tables(root).items():
        if written == "none":
            unversioned.add(name)
            continue
        if written.isdigit():
            versions[name] = int(written)
            continue
        number = constant_version(root, written)
        if number is not None:
            versions[name] = number
    return versions, unversioned


def current_versions(root: Path, texts: dict[str, str]) -> dict[str, int]:
    """Each declared format's version now, by name: a `migration::Format`'s, or
    one a `[[format]]` table names."""
    found: dict[str, int] = {}
    cache: dict[Path, dict[str, str]] = {}
    for path, text in texts.items():
        for match in VERSIONED.finditer(without_tests(path, text)):
            crate = crate_of(root, path)
            version = version_of(match.group("version"), crate_sources(crate, root, cache))
            if version is not None:
                found[match.group("name")] = version
    versions, _ = declared_format_versions(root)
    found.update(versions)
    return found


def changed_layout_message(name: str, version: int, entries: list[str], constant: str | None) -> str:
    """What a stored layout that changed under its version means, for a format
    that keeps its version in a `migration::Format` or in a plain constant."""
    listed = ", ".join(entries)
    if constant is None:
        return (
            f"{LAYOUTS}: a type stored under format {name!r} changed while the format is "
            f"still version {version} ({listed}). Bytes an earlier build wrote would read as "
            "something else: give the format a new version with a step that brings up what "
            "this one wrote, then pin the new layout with "
            "`python3 tools/policy/check_format_migrations.py pin`"
        )
    return (
        f"{LAYOUTS}: a type stored under format {name!r} changed while the format is still "
        f"version {version} ({listed}). That format refuses a state of another version by "
        "name, so a new version needs a step that brings up what this one wrote before the "
        f"number in `{constant}` moves: without that step the bytes an earlier build wrote are "
        "refused, not read. Then pin the new layout with "
        "`python3 tools/policy/check_format_migrations.py pin`"
    )


def stored_layouts(root: Path, texts: dict[str, str]) -> list[str]:
    """Each listed stored layout whose definition changed under its format's
    current version, or that names a format or type there is not."""
    file = root / LAYOUTS
    if not file.is_file():
        return []
    data = tomllib.loads(file.read_text())
    problems = format_problems(root)
    versions = current_versions(root, texts)
    constant_versions, unversioned = declared_format_versions(root)
    definitions = definitions_for(root)
    newest: dict[str, int] = {}
    for layout in data.get("layout", []):
        name = layout["format"]
        if name in unversioned:
            digest, missing = layout_digest(layout["types"], definitions)
            if missing:
                problems.append(
                    f"{LAYOUTS}: format {name!r} lists {', '.join(missing)}, "
                    "which its file does not define"
                )
            elif digest != layout["digest"]:
                problems.append(
                    f"{LAYOUTS}: a type stored in format {name!r} changed, and that format "
                    f"has no version ({', '.join(layout['types'])}): the bytes an earlier "
                    "build wrote read as something else, and a later build cannot tell them "
                    "from these. Give the format a version in its `[[format]]` table and a "
                    "step that brings up what was written (a reader that converts it), then "
                    "pin the new layout with "
                    "`python3 tools/policy/check_format_migrations.py pin`"
                )
            continue
        if "version" not in layout:
            problems.append(
                f"{LAYOUTS}: a layout of format {name!r} names no version: a format with "
                "versions writes the version its layout is"
            )
            continue
        version = int(layout["version"])
        newest[name] = max(newest.get(name, 0), version)
        if name not in versions:
            problems.append(
                f"{LAYOUTS}: names format {name!r}, which nothing declares: list a stored "
                "type under the `migration::Format` whose version covers it, or declare the "
                "format in a `[[format]]` table"
            )
            continue
        if version > versions[name]:
            problems.append(
                f"{LAYOUTS}: lists format {name!r} version {version}, newer than the "
                f"{versions[name]} it declares"
            )
        if version != versions[name]:
            continue
        digest, missing = layout_digest(layout["types"], definitions)
        if missing:
            problems.append(
                f"{LAYOUTS}: format {name!r} version {version} lists {', '.join(missing)}, "
                "which its file does not define"
            )
        elif digest != layout["digest"]:
            problems.append(
                changed_layout_message(
                    name,
                    version,
                    layout["types"],
                    format_tables(root).get(name) if name in constant_versions else None,
                )
            )
    for name, version in sorted(versions.items()):
        if name in newest and newest[name] < version:
            problems.append(
                f"{LAYOUTS}: format {name!r} is version {version} and its stored types are "
                f"pinned only up to version {newest[name]}: pin it with "
                "`python3 tools/policy/check_format_migrations.py pin`"
            )
    return problems


DIGEST_BLOCK = re.compile(r"(?ms)^\[\[layout\]\]\n.*?(?=^\[\[layout\]\]\n|\Z)")


def rewrite_digests(text: str, wanted: dict[tuple[str, int | None], str]) -> str:
    """`text` with each `[[layout]]` block's digest replaced by the one `wanted`
    names for its (format, version). A block besides those is as it was."""

    def fix(block: re.Match[str]) -> str:
        chunk = block.group(0)
        name = re.search(r'(?m)^format\s*=\s*"([^"]*)"', chunk)
        version = re.search(r"(?m)^version\s*=\s*([0-9]+)", chunk)
        if name is None:
            return chunk
        key = (name.group(1), int(version.group(1)) if version else None)
        if key not in wanted:
            return chunk
        return re.sub(
            r'(?m)^digest\s*=\s*"[^"]*"', f'digest = "{wanted[key]}"', chunk, count=1
        )

    return DIGEST_BLOCK.sub(fix, text)


def pin(root: Path = ROOT) -> list[str]:
    """Pin each listed format's current version: the digest of the definition
    its layout lists, as the tree holds it now. A version not recorded yet is
    added, from the types the newest recorded layout lists; a recorded one has
    its digest written again (a re-pin is visible to `against BASE`)."""
    file = root / LAYOUTS
    text = file.read_text()
    problems = format_problems(root)
    if problems:
        return problems
    versions = current_versions(root, sources(root))
    table_versions, unversioned = declared_format_versions(root)
    definitions = definitions_for(root)
    listed = layouts(text)
    wanted: dict[tuple[str, int | None], str] = {}
    added: list[str] = []
    names = {layout["format"] for layout in listed} | set(table_versions) | unversioned
    for name in sorted(names):
        mine = [layout for layout in listed if layout["format"] == name]
        if name in unversioned:
            if not mine:
                return [f"{name}: no layout lists the types a format with no version stores"]
            digest, missing = layout_digest(mine[0]["types"], definitions)
            if missing:
                return [f"{name}: {', '.join(missing)} not found"]
            wanted[(name, None)] = digest
            continue
        version = versions.get(name)
        if version is None:
            continue
        current = [layout for layout in mine if int(layout.get("version", -1)) == version]
        if current:
            digest, missing = layout_digest(current[0]["types"], definitions)
            if missing:
                return [f"{name}: {', '.join(missing)} not found"]
            wanted[(name, version)] = digest
            continue
        if not mine:
            return [f"{name}: no layout lists the types version {version} stores"]
        newest = max(mine, key=lambda layout: int(layout["version"]))
        digest, missing = layout_digest(newest["types"], definitions)
        if missing:
            return [f"{name}: {', '.join(missing)} not found"]
        types = "".join(f'  "{entry}",\n' for entry in newest["types"])
        added.append(
            f'\n[[layout]]\nformat = "{name}"\nversion = {version}\ndigest = "{digest}"\n'
            f'what = """{newest["what"]}"""\ntypes = [\n{types}]\n'
        )
        wanted[(name, version)] = digest
    text = rewrite_digests(text, wanted)
    if added:
        text = text.rstrip("\n") + "\n" + "".join(added)
    file.write_text(text)
    return []


def changed_layouts(root: Path, base: str, now_texts: dict[str, str]) -> list[str]:
    """Each digest `base` recorded for a version still declared that is
    another now: what a version wrote never changes. Digests BASE recorded
    under another `schema` were meant another way and are left to the check
    that wrote them."""
    try:
        then_text = subprocess.check_output(
            ["git", "show", f"{base}:{LAYOUTS.as_posix()}"],
            cwd=root,
            text=True,
            stderr=subprocess.DEVNULL,
        )
    except subprocess.CalledProcessError:
        return []
    file = root / LAYOUTS
    if not file.is_file():
        return []
    now_text = file.read_text()
    if schema(then_text) != schema(now_text):
        return []
    versions = current_versions(root, now_texts)
    constant_versions, unversioned = declared_format_versions(root)
    now = {
        (layout["format"], int(layout["version"]) if "version" in layout else None): layout
        for layout in layouts(now_text)
    }
    declared_now = declarations(now_texts)
    problems = []
    for layout in layouts(then_text):
        key = (layout["format"], int(layout["version"]) if "version" in layout else None)
        name, version = key
        if name in unversioned:
            still = True
        elif name in constant_versions:
            # This format refuses a version other than the one it is: only the
            # version it declares now can still be read.
            still = version == constant_versions[name]
        else:
            oldest = declared_now.get(name)
            still = oldest is not None and oldest[1].isdigit() and int(oldest[1]) <= (version or 0)
        if not still:
            continue
        if version is not None and name in versions and version > versions[name]:
            continue
        if key not in now or now[key]["digest"] != layout["digest"]:
            problems.append(
                f"{LAYOUTS}: the layout format {name!r} version {version} stored changed; "
                "what a version wrote never changes, so a layout change is a new version"
            )
    return problems


def check(root: Path = ROOT) -> list[str]:
    problems = format_problems(root)
    texts = sources(root)
    for name, (path, oldest) in declarations(texts).items():
        if not oldest.isdigit():
            problems.append(
                f"{path}: format {name!r} declares `oldest: {oldest}`; write the number, so "
                "that it can only ever fall"
            )
    problems.extend(loaders(root, texts))
    problems.extend(goldens(root, texts))
    problems.extend(stored_layouts(root, texts))
    return problems


def kept_goldens(root: Path, revision: str) -> list[str]:
    listed = subprocess.run(
        ["git", "ls-tree", "-r", "--name-only", revision, "--", "crates", "tools", "apps"],
        cwd=root,
        capture_output=True,
        text=True,
        check=True,
    ).stdout
    return [path for path in listed.splitlines() if GOLDEN.search(path)]


def changed_goldens(base: str, root: Path, declared: list[str]) -> list[str]:
    """Each golden file `base` kept that holds other bytes now. The bytes of a
    version this build no longer declares may be gone with its step."""
    problems = []
    for path in kept_goldens(root, base):
        file = root / path
        if not file.is_file():
            if path in declared:
                problems.append(
                    f"{path}: the bytes a version of a stored format wrote are gone; this "
                    "build still declares the version, so keep the file and the test that "
                    "reads it"
                )
            continue
        then = subprocess.run(
            ["git", "rev-parse", f"{base}:{path}"], cwd=root, capture_output=True, text=True
        ).stdout.strip()
        now = subprocess.run(
            ["git", "hash-object", "--", path], cwd=root, capture_output=True, text=True
        ).stdout.strip()
        if then != now:
            problems.append(
                f"{path}: golden bytes changed. What a version of a format wrote never changes: "
                "a layout change is a new version, with its own golden file and the step that "
                "brings these bytes up"
            )
    return problems


def version_at(root: Path, base: str, path: str, expression: str) -> int | None:
    """The number a declaration's `version:` was at `base`: written out, or a
    `u32` constant of the crate `path` belongs to."""
    expression = expression.strip()
    if expression.isdigit():
        return int(expression)
    name = expression.rsplit("::", 1)[-1]
    if not re.fullmatch(r"[A-Z][A-Z0-9_]*", name):
        return None
    crate = crate_at(root, base, path)
    found = subprocess.run(
        ["git", "grep", "-h", "-E", rf"\bconst\s+{name}\s*:\s*u32\s*=\s*[0-9]+", base, "--",
         f"{crate}/*.rs"],
        cwd=root,
        capture_output=True,
        text=True,
    ).stdout
    number = re.search(r"=\s*([0-9]+)", found)
    return int(number.group(1)) if number else None


def crate_at(root: Path, revision: str, path: str) -> str:
    """The directory of the crate `path` belonged to at `revision`, or the
    file's own directory when no manifest was above it: a crate may have moved
    since (the stable release keeps crates where main no longer does)."""
    directory = Path(path).parent
    while directory != Path("."):
        manifest = (directory / "Cargo.toml").as_posix()
        if subprocess.run(["git", "cat-file", "-e", f"{revision}:{manifest}"], cwd=root,
                          capture_output=True).returncode == 0:
            return directory.as_posix()
        directory = directory.parent
    return Path(path).parent.as_posix()


def dropped_versions(root: Path, base: str, then: dict[str, str], now: dict[str, str]) -> list[str]:
    """Each format that no longer brings up the version `base` writes."""
    problems = []
    declared_now = declarations({path: without_tests(path, text) for path, text in now.items()})
    for path, text in then.items():
        for match in VERSIONED.finditer(without_tests(path, text)):
            name = match.group("name")
            if name not in declared_now:
                continue
            written = version_at(root, base, path, match.group("version"))
            now_path, oldest = declared_now[name]
            if written is None or not oldest.isdigit() or int(oldest) <= written:
                continue
            problems.append(
                f"{now_path}: format {name!r} no longer brings up version {written}, which "
                f"{base} writes: its new floor is `oldest: {oldest}`. Every device that updates "
                "from it holds that version, and would refuse what it kept. Keep the step that "
                f"brings it up (`oldest` at most {written}), tested with "
                "`assert_step_reads_previous`"
            )
    return problems


def against(base: str, root: Path = ROOT) -> list[str]:
    """What a change may not do to a stored layout: change the bytes of a
    version this build still declares, or stop bringing up the version `base`
    writes. A format may bring up fewer older versions, gone with their steps."""
    then = sources_at(root, base)
    now = sources_since(root, base, then)
    return (
        changed_goldens(base, root, declared_goldens(root, now))
        + dropped_versions(root, base, then, now)
        + changed_layouts(root, base, now)
    )


# Where `check` fetches the released build's branch from when its ref is missing (a t job's Git
# view holds only the commit it checks and main).
STABLE_REMOTE = "https://code.tilo.so/tilo/tilo.git"


def below_stable(root: Path, stable: str, remote: str = STABLE_REMOTE) -> list[str]:
    """Each format whose oldest version is above the one the released build
    (`stable`) writes. A device updates from the release, not from main: a
    floor above what the release writes refuses every file such a device kept,
    however many main builds in between brought that version up. A ref that is
    missing is fetched (`origin/<branch>` from `remote`); one that can't be is
    a failure, never a skip."""
    def resolves() -> bool:
        return subprocess.run(["git", "rev-parse", "--verify", "--quiet", f"{stable}^{{commit}}"],
                              cwd=root, capture_output=True).returncode == 0

    if not resolves() and stable.startswith("origin/"):
        branch = stable.removeprefix("origin/")
        try:
            fetched = subprocess.run(
                ["git", "fetch", "--quiet", "--no-tags", "--depth=1", remote,
                 f"+refs/heads/{branch}:refs/remotes/origin/{branch}"],
                cwd=root, capture_output=True, text=True, timeout=120,
                env={**os.environ, "GIT_TERMINAL_PROMPT": "0"},
            )
            reason = fetched.stderr.strip()
        except subprocess.TimeoutExpired:
            reason = "no answer within 120 s"
        if not resolves():
            return [
                f"the released build's branch {stable!r} is not here and fetching it from "
                f"{remote} failed ({reason or 'no such branch'}): without it no format's floor "
                "can be checked against what released devices hold"
            ]
    if not resolves():
        return [
            f"the released build's ref {stable!r} does not name a commit: without it no "
            "format's floor can be checked against what released devices hold"
        ]
    return dropped_versions(root, stable, sources_at(root, stable), sources(root))


def stored_closure(root: Path = ROOT) -> set[tuple[str, str]]:
    """(file, name) pairs of every definition `stored_layouts.toml` lists or
    one of its listed definitions reaches: the types whose bytes a later build
    reads back."""
    file = root / LAYOUTS
    if not file.is_file():
        return set()
    entries = [
        entry
        for layout in layouts(file.read_text())
        for entry in layout.get("types", [])
    ]
    if not entries:
        return set()
    found, _ = reached_definitions(entries, definitions_for(root))
    return set(found)


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__.split("\n", 1)[0])
    sub = parser.add_subparsers(dest="command", required=True)
    checked = sub.add_parser("check")
    checked.add_argument("--stable", default="origin/stable",
                         help="the released build's ref, whose versions every floor stays at or below")
    sub.add_parser("pin")
    base = sub.add_parser("against")
    base.add_argument("base")
    args = parser.parse_args(argv)
    if args.command == "pin":
        problems = pin()
        for problem in problems:
            print(problem)
    elif args.command == "check":
        problems = check() + below_stable(ROOT, args.stable)
        for problem in problems:
            print(problem)
    else:
        problems = against(args.base)
        for problem in problems:
            print(f"::error::{problem}", file=sys.stderr)
    return 1 if problems else 0


if __name__ == "__main__":
    sys.exit(main())
