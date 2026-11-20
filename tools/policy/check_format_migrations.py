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
with a digest of its definition at that version. ``check`` fails when a listed
type's definition no longer matches the digest recorded for the format's
current version: a layout that changed is a new version of its format, with
the step that brings up what the version before wrote. ``pin`` writes the
digest of the current version once that version is declared. ``against BASE``
fails when the digest a still-declared version had at BASE changed.
"""

from __future__ import annotations

import argparse
import hashlib
import re
import subprocess
import sys
import tomllib
from pathlib import Path

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


def item_definition(text: str, name: str) -> str | None:
    """The definition of struct or enum `name` in `text`, with the serde
    attributes on it and its fields, without comments or doc text, its
    whitespace collapsed: what decides the bytes it is stored as."""
    code = without_tests("lib.rs", text)
    code = re.sub(r"/\*.*?\*/", " ", code, flags=re.S)
    code = re.sub(r"(?m)//[^\n]*$", "", code)
    found = re.search(
        rf"(?m)^(?P<attrs>(?:[ \t]*#\[[^\n]*\]\s*\n)*)[ \t]*(?:pub(?:\([^)]*\))?\s+)?"
        rf"(?P<kind>struct|enum)\s+{re.escape(name)}\b",
        code,
    )
    if not found:
        return None
    depth, end = 0, None
    for at in range(found.end(), len(code)):
        char = code[at]
        if char in "<({[":
            depth += 1
        elif char in ">)}]":
            depth -= 1
            if depth == 0 and char in ")}":
                end = at + 1
                break
        elif char == ";" and depth == 0:
            end = at + 1
            break
    if end is None:
        return None
    attrs = " ".join(
        attr for attr in re.findall(r"#\[[^\n]*\]", found.group("attrs")) if "serde" in attr
    )
    body = code[found.start("kind") : end]
    return re.sub(r"\s+", " ", f"{attrs} {body}").strip()


def layout_digest(types: list[str], read) -> tuple[str | None, list[str]]:
    """The digest of `types` (each `path:Type`) as `read(path)` holds them, or
    `None` with what was not found."""
    parts, missing = [], []
    for entry in types:
        path, _, name = entry.partition(":")
        text = read(path)
        definition = item_definition(text, name) if text is not None else None
        if definition is None:
            missing.append(entry)
            continue
        parts.append(f"{entry}={definition}")
    if missing:
        return None, missing
    return hashlib.sha256("\n".join(parts).encode()).hexdigest()[:16], []


def layouts(text: str) -> list[dict]:
    return tomllib.loads(text).get("layout", [])


def current_versions(root: Path, texts: dict[str, str]) -> dict[str, int]:
    """Each declared format's version now, by name."""
    found: dict[str, int] = {}
    cache: dict[Path, dict[str, str]] = {}
    for path, text in texts.items():
        for match in VERSIONED.finditer(without_tests(path, text)):
            crate = crate_of(root, path)
            version = version_of(match.group("version"), crate_sources(crate, root, cache))
            if version is not None:
                found[match.group("name")] = version
    return found


def read_file(root: Path):
    def read(path: str) -> str | None:
        file = root / path
        return file.read_text(errors="replace") if file.is_file() else None

    return read


def stored_layouts(root: Path, texts: dict[str, str]) -> list[str]:
    """Each listed stored layout whose definition changed under its format's
    current version, or that names a format or type there is not."""
    file = root / LAYOUTS
    if not file.is_file():
        return []
    problems = []
    versions = current_versions(root, texts)
    newest: dict[str, int] = {}
    for layout in layouts(file.read_text()):
        name, version = layout["format"], int(layout["version"])
        newest[name] = max(newest.get(name, 0), version)
        if name not in versions:
            problems.append(
                f"{LAYOUTS}: names format {name!r}, which nothing declares: list a stored "
                "type under the `migration::Format` whose version covers it"
            )
            continue
        if version > versions[name]:
            problems.append(
                f"{LAYOUTS}: lists format {name!r} version {version}, newer than the "
                f"{versions[name]} it declares"
            )
        if version != versions[name]:
            continue
        digest, missing = layout_digest(layout["types"], read_file(root))
        if missing:
            problems.append(
                f"{LAYOUTS}: format {name!r} version {version} lists {', '.join(missing)}, "
                "which its file does not define"
            )
        elif digest != layout["digest"]:
            problems.append(
                f"{LAYOUTS}: a type stored under format {name!r} changed while the format is "
                f"still version {version} ({', '.join(layout['types'])}). Bytes an earlier "
                "build wrote would read as something else: give the format a new version with "
                "a step that brings up what this one wrote, then pin the new layout with "
                "`python3 tools/policy/check_format_migrations.py pin`"
            )
    for name, version in sorted(versions.items()):
        if name in newest and newest[name] < version:
            problems.append(
                f"{LAYOUTS}: format {name!r} is version {version} and its stored types are "
                f"pinned only up to version {newest[name]}: pin it with "
                "`python3 tools/policy/check_format_migrations.py pin`"
            )
    return problems


def pin(root: Path = ROOT) -> list[str]:
    """Pin the digest of each listed format's current version: a new
    entry, from the types its newest one lists, for a version not recorded
    yet. A version already recorded keeps its digest."""
    file = root / LAYOUTS
    text = file.read_text()
    versions = current_versions(root, sources(root))
    listed = layouts(text)
    added = []
    for name in sorted({layout["format"] for layout in listed}):
        mine = [layout for layout in listed if layout["format"] == name]
        newest = max(mine, key=lambda layout: int(layout["version"]))
        version = versions.get(name)
        if version is None or any(int(layout["version"]) == version for layout in mine):
            continue
        digest, missing = layout_digest(newest["types"], read_file(root))
        if missing:
            return [f"{name}: {', '.join(missing)} not found"]
        types = "".join(f'  "{entry}",\n' for entry in newest["types"])
        added.append(
            f'\n[[layout]]\nformat = "{name}"\nversion = {version}\ndigest = "{digest}"\n'
            f'what = """{newest["what"]}"""\ntypes = [\n{types}]\n'
        )
    if added:
        file.write_text(text.rstrip("\n") + "\n" + "".join(added))
    return []


def changed_layouts(root: Path, base: str, now_texts: dict[str, str]) -> list[str]:
    """Each digest `base` recorded for a version still declared that is
    another now: what a version wrote never changes."""
    try:
        then = subprocess.check_output(
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
    versions = current_versions(root, now_texts)
    now = {(layout["format"], int(layout["version"])): layout for layout in layouts(file.read_text())}
    problems = []
    for layout in layouts(then):
        key = (layout["format"], int(layout["version"]))
        oldest = declarations(now_texts).get(layout["format"])
        still = oldest is not None and oldest[1].isdigit() and int(oldest[1]) <= key[1]
        if not still or key[1] > versions.get(key[0], 0):
            continue
        if key not in now or now[key]["digest"] != layout["digest"]:
            problems.append(
                f"{LAYOUTS}: the layout format {key[0]!r} version {key[1]} stored changed; "
                "what a version wrote never changes, so a layout change is a new version"
            )
    return problems


def check(root: Path = ROOT) -> list[str]:
    problems = []
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
        ["git", "ls-tree", "-r", "--name-only", revision, "--", "crates", "apps"],
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
    crate = crate_of(root, path).relative_to(root).as_posix()
    found = subprocess.run(
        ["git", "grep", "-h", "-E", rf"\bconst\s+{name}\s*:\s*u32\s*=\s*[0-9]+", base, "--",
         f"{crate}/*.rs"],
        cwd=root,
        capture_output=True,
        text=True,
    ).stdout
    number = re.search(r"=\s*([0-9]+)", found)
    return int(number.group(1)) if number else None


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
                f"{base} writes: every device that updates holds that version, and would refuse "
                "what it kept. Keep the step that brings it up (`oldest` at most "
                f"{written}), tested with `assert_step_reads_previous`"
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


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__.split("\n", 1)[0])
    sub = parser.add_subparsers(dest="command", required=True)
    sub.add_parser("check")
    sub.add_parser("pin")
    base = sub.add_parser("against")
    base.add_argument("base")
    args = parser.parse_args(argv)
    if args.command == "pin":
        problems = pin()
        for problem in problems:
            print(problem)
    elif args.command == "check":
        problems = check()
        for problem in problems:
            print(problem)
    else:
        problems = against(args.base)
        for problem in problems:
            print(f"::error::{problem}", file=sys.stderr)
    return 1 if problems else 0


if __name__ == "__main__":
    sys.exit(main())
