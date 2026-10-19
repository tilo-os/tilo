#!/usr/bin/env python3
"""Write vendor/regex-syntax: regex-syntax 0.8.11 with its Unicode tables
laid out without pointers.

    python3 tools/regex-syntax-tables.py ~/.cargo/registry/src/*/regex-syntax-0.8.11

ucd-generate writes the name tables as slices of `(&str, &[..])` and the
simple case folding table as `(char, &[char])`: about 3,900 entries the
dynamic loader relocates in every process that maps the library holding them
(121 KB of the shared app runtime's relocated data). This rewrites them as
string literals with integer offsets, and the lookups in `src/unicode.rs` (and
the `arbitrary` impl in `src/ast/mod.rs`) to read them. Every set, name and
fold stays as ucd-generate wrote it. See vendor/regex-syntax/TILO-PATCH.md.
"""

from __future__ import annotations

import re
import shutil
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
OUT = ROOT / "vendor/regex-syntax"
DROPPED = {"Cargo.lock", ".cargo-ok", ".cargo_vcs_info.json"}
NOTE = (
    "// Tilo: laid out by tools/regex-syntax-tables.py without pointers, so the\n"
    "// loader relocates nothing here (vendor/regex-syntax/TILO-PATCH.md).\n"
)


def literal(text: str, width: int = 88) -> str:
    """`text` as a Rust string literal body of escaped, wrapped lines."""
    lines, line = [], ""
    for char in text:
        piece = char if char.isascii() and char.isprintable() and char not in '"\\' else f"\\u{{{ord(char):04X}}}"
        if line and len(line) + len(piece) > width:
            lines.append(line)
            line = ""
        line += piece
    lines.append(line)
    assert not any(part.startswith((" ", "\t")) for part in lines), "a line would lose its leading space"
    return '"\\\n' + "\\\n".join(lines) + '"'


class Blob:
    def __init__(self) -> None:
        self.text = ""
        self.at: dict[str, int] = {}

    def place(self, text: str) -> tuple[int, int]:
        if text not in self.at:
            found = self.text.find(text)
            if found < 0:
                found = len(self.text)
                self.text += text
            self.at[text] = found
        start, length = self.at[text], len(text.encode())
        start = len(self.text[:start].encode())
        assert start < 1 << 16 and length < 1 << 8, (text, start, length)
        return start, length


def by_name(source: str, path: Path) -> str:
    match = re.search(r"pub const BY_NAME: &'static \[\(&'static str, &'static \[\(char, char\)\]\)\] =\s*&\[(.*?)\];\n", source, re.S)
    if not match:
        return source
    entries = re.findall(r'\("([^"]+)",\s*([A-Z0-9_]+)\)', match.group(1))
    names = [name for name, _ in entries]
    assert names == sorted(names, key=str.encode) and len(set(names)) == len(names), path
    blob = Blob()
    spans = [blob.place(name) for name in names]
    arms = "".join(f"        {index} => {const},\n" for index, (_, const) in enumerate(entries))
    generated = f"""{NOTE}
/// The names of the sets below, back to back, in byte order.
const NAMES: &str = {literal(blob.text)};

/// Each set's name, as `(start, length)` in [`NAMES`], sorted.
const NAME_SPANS: &[(u16, u8)] = &[
{"".join(f"    ({start}, {length}),\n" for start, length in spans)}];

/// How many sets [`by_name`] knows.
#[allow(dead_code)]
pub const BY_NAME_LEN: usize = {len(entries)};

/// The set named `name` (canonically), or `None`.
#[allow(dead_code)]
pub fn by_name(name: &str) -> Option<&'static [(char, char)]> {{
    let index = NAME_SPANS
        .binary_search_by(|&(start, len)| {{
            let start = usize::from(start);
            NAMES[start..start + usize::from(len)].cmp(name)
        }})
        .ok()?;
    Some(match index {{
{arms}        _ => return None,
    }})
}}
"""
    return source[: match.start()] + generated + source[match.end() :]


def property_names(source: str) -> str:
    match = re.search(r"pub const PROPERTY_NAMES: &'static \[\(&'static str, &'static str\)\] = &\[(.*?)\];\n", source, re.S)
    assert match
    entries = re.findall(r'\("([^"]+)", "([^"]+)"\)', match.group(1))
    aliases = [alias for alias, _ in entries]
    assert aliases == sorted(aliases, key=str.encode)
    blob = Blob()
    rows = [(*blob.place(alias), *blob.place(name)) for alias, name in entries]
    generated = f"""{NOTE}
/// Every property alias and canonical name below, back to back.
const STRINGS: &str = {literal(blob.text)};

/// Each normalized alias with its canonical property name, sorted by alias:
/// `(alias start, alias length, name start, name length)` in [`STRINGS`].
const NAMES: &[(u16, u8, u16, u8)] = &[
{"".join(f"    ({a}, {b}, {c}, {d}),\n" for a, b, c, d in rows)}];

fn string(start: u16, len: u8) -> &'static str {{
    let start = usize::from(start);
    &STRINGS[start..start + usize::from(len)]
}}

/// The canonical name of the property whose normalized name is `alias`.
pub fn canonical(alias: &str) -> Option<&'static str> {{
    let index = NAMES
        .binary_search_by(|&(start, len, _, _)| string(start, len).cmp(alias))
        .ok()?;
    let (_, _, start, len) = NAMES[index];
    Some(string(start, len))
}}

/// Every `(normalized alias, canonical name)`, in alias order.
#[allow(dead_code)]
pub fn all() -> impl Iterator<Item = (&'static str, &'static str)> {{
    NAMES.iter().map(|&(a, al, n, nl)| (string(a, al), string(n, nl)))
}}
"""
    return source[: match.start()] + generated + source[match.end() :]


def property_values(source: str) -> str:
    start = source.index("pub const PROPERTY_VALUES:")
    end = source.index("\n];\n", start) + len("\n];\n")
    body = source[start:end]
    properties = re.findall(r'\(\s*"([^"]+)",\s*&\[(.*?)\],\s*\)', body, re.S)
    names = [name for name, _ in properties]
    assert names == sorted(names, key=str.encode) and len(properties) > 5
    blob = Blob()
    values, rows = [], []
    for name, inner in properties:
        pairs = re.findall(r'\("([^"]+)", "([^"]+)"\)', inner)
        aliases = [alias for alias, _ in pairs]
        assert aliases == sorted(aliases, key=str.encode), name
        rows.append((*blob.place(name), len(values), len(pairs)))
        values += [(*blob.place(alias), *blob.place(value)) for alias, value in pairs]
    assert len(values) < 1 << 16
    generated = f"""{NOTE}
/// Every property name, value alias and canonical value below, back to back.
const STRINGS: &str = {literal(blob.text)};

/// Every property's values, property by property, each property's sorted by
/// alias: `(alias start, alias length, value start, value length)` in
/// [`STRINGS`].
const VALUES: &[(u16, u8, u16, u8)] = &[
{"".join(f"    ({a}, {b}, {c}, {d}),\n" for a, b, c, d in values)}];

/// Each property, sorted by name: `(name start, name length, its first
/// entry of [`VALUES`], how many)`.
const PROPERTIES: &[(u16, u8, u16, u16)] = &[
{"".join(f"    ({a}, {b}, {c}, {d}),\n" for a, b, c, d in rows)}];

fn string(start: u16, len: u8) -> &'static str {{
    let start = usize::from(start);
    &STRINGS[start..start + usize::from(len)]
}}

/// One property's values (a run of [`VALUES`]).
#[derive(Clone, Copy, Debug)]
pub struct Values {{
    first: u16,
    count: u16,
}}

impl Values {{
    fn entries(self) -> &'static [(u16, u8, u16, u8)] {{
        let first = usize::from(self.first);
        &VALUES[first..first + usize::from(self.count)]
    }}
}}

/// The values of the property canonically named `name`.
pub fn property_values(name: &str) -> Option<Values> {{
    let index = PROPERTIES
        .binary_search_by(|&(start, len, _, _)| string(start, len).cmp(name))
        .ok()?;
    let (_, _, first, count) = PROPERTIES[index];
    Some(Values {{ first, count }})
}}

/// The canonical value whose normalized alias is `alias`, among `values`.
pub fn canonical_value(values: Values, alias: &str) -> Option<&'static str> {{
    let entries = values.entries();
    let index = entries
        .binary_search_by(|&(start, len, _, _)| string(start, len).cmp(alias))
        .ok()?;
    let (_, _, start, len) = entries[index];
    Some(string(start, len))
}}

/// Every `(property, normalized alias, canonical value)`, property by
/// property.
#[allow(dead_code)]
pub fn all() -> impl Iterator<Item = (&'static str, &'static str, &'static str)> {{
    PROPERTIES.iter().flat_map(|&(start, len, first, count)| {{
        let property = string(start, len);
        Values {{ first, count }}
            .entries()
            .iter()
            .map(move |&(a, al, v, vl)| (property, string(a, al), string(v, vl)))
    }})
}}
"""
    return source[:start] + generated + source[end:]


def char_literal(char: str) -> str:
    return f"'\\u{{{ord(char):04X}}}'"


def case_folding(source: str) -> str:
    start = source.index("pub const CASE_FOLDING_SIMPLE:")
    end = source.index("\n];\n", start) + len("\n];\n")
    body = source[start:end]
    # The file writes literal chars; read them back one entry at a time.
    entries = re.findall(r"\('((?:\\.|[^'\\])+)', &\[(.*?)\]\)", body, re.S)

    def decode(text: str) -> str:
        if text.startswith("\\u{"):
            return chr(int(text[3:-1], 16))
        escapes = {"\\'": "'", "\\\\": "\\", "\\n": "\n", "\\t": "\t", "\\r": "\r", "\\0": "\0"}
        return escapes.get(text, text)

    folds, rows = [], []
    for key, inner in entries:
        values = [decode(value) for value in re.findall(r"'((?:\\.|[^'\\])+)'", inner)]
        assert len(values) < 256
        rows.append((decode(key), len(folds), len(values)))
        folds += values
    keys = [key for key, _, _ in rows]
    assert keys == sorted(keys) and len(rows) > 2000 and len(folds) < 1 << 16
    generated = f"""{NOTE}
/// Each scalar value with a non-trivial simple case fold, sorted: the scalar,
/// and its equivalence class (without it) as `(start, length)` in
/// [`CASE_FOLDING_VALUES`].
pub const CASE_FOLDING_SIMPLE: &'static [(char, u16, u8)] = &[
{"".join(f"    ({char_literal(key)}, {first}, {count}),\n" for key, first, count in rows)}];

/// The equivalence classes of [`CASE_FOLDING_SIMPLE`], back to back.
pub const CASE_FOLDING_VALUES: &'static [char] = &[
{"".join(f"    {char_literal(value)},\n" for value in folds)}];
"""
    return source[:start] + generated + source[end:]


def replace(path: Path, old: str, new: str, count: int = 1) -> None:
    text = path.read_text()
    assert text.count(old) == count, (path, text.count(old), old[:70])
    path.write_text(text.replace(old, new))


def patch_unicode(path: Path) -> None:
    replace(path, "    table: &'static [(char, &'static [char])],", "    table: &'static [(char, u16, u8)],")
    replace(path, """        let (k, v) = self.table[self.next];
        if k == c {
            self.next += 1;
            return v;
        }""", """        let (k, start, len) = self.table[self.next];
        if k == c {
            self.next += 1;
            return fold_values(start, len);
        }""")
    replace(path, """                self.next = i + 1;
                self.table[i].1""", """                self.next = i + 1;
                let (_, start, len) = self.table[i];
                fold_values(start, len)""")
    replace(path, ".binary_search_by(|&(c, _)| {", ".binary_search_by(|&(c, _, _)| {")
    replace(path, "self.table.binary_search_by_key(&c, |&(c1, _)| c1)", "self.table.binary_search_by_key(&c, |&(c1, _, _)| c1)")
    replace(path, """/// A query for finding a character class defined by Unicode. This supports""", """/// The equivalence class at `start..start + len` of the case folding table's
/// values (Tilo: the table holds offsets, not slices).
#[cfg(feature = "unicode-case")]
fn fold_values(start: u16, len: u8) -> &'static [char] {
    let start = usize::from(start);
    &crate::unicode_tables::case_folding_simple::CASE_FOLDING_VALUES
        [start..start + usize::from(len)]
}

#[cfg(not(feature = "unicode-case"))]
fn fold_values(_: u16, _: u8) -> &'static [char] {
    &[]
}

/// A query for finding a character class defined by Unicode. This supports""")
    features = """    feature = "unicode-age",
    feature = "unicode-bool",
    feature = "unicode-gencat",
    feature = "unicode-perl",
    feature = "unicode-script",
    feature = "unicode-segment","""
    replace(path, """type PropertyValues = &'static [(&'static str, &'static str)];""", f"""#[cfg(any(
{features}
))]
type PropertyValues = crate::unicode_tables::property_values::Values;

/// Without the tables there are no property values to hold.
#[cfg(not(any(
{features}
)))]
type PropertyValues = core::convert::Infallible;""")
    replace(path, """        use crate::unicode_tables::property_names::PROPERTY_NAMES;

        Ok(PROPERTY_NAMES
            .binary_search_by_key(&name, |&(n, _)| n)
            .ok()
            .map(|i| PROPERTY_NAMES[i].1))""", """        Ok(crate::unicode_tables::property_names::canonical(name))""")
    replace(path, """    vals.binary_search_by_key(&normalized_value, |&(n, _)| n)
        .ok()
        .map(|i| vals[i].1)""", f"""    #[cfg(any(
    {features.replace(chr(10), chr(10) + "    ")}
    ))]
    return crate::unicode_tables::property_values::canonical_value(
        vals,
        normalized_value,
    );
    #[cfg(not(any(
    {features.replace(chr(10), chr(10) + "    ")}
    )))]
    match vals {{}}""")
    replace(path, """        use crate::unicode_tables::property_values::PROPERTY_VALUES;

        Ok(PROPERTY_VALUES
            .binary_search_by_key(&name, |&(n, _)| n)
            .ok()
            .map(|i| PROPERTY_VALUES[i].1))""", """        Ok(crate::unicode_tables::property_values::property_values(name))""")
    replace(path, """fn property_set(
    name_map: &'static [(&'static str, Range)],
    canonical: &'static str,
) -> Option<Range> {
    name_map
        .binary_search_by_key(&canonical, |x| x.0)
        .ok()
        .map(|i| name_map[i].1)
}""", """fn property_set(
    by_name: fn(&str) -> Option<Range>,
    canonical: &'static str,
) -> Option<Range> {
    by_name(canonical)
}""")
    for table in ["general_category", "script", "script_extension", "property_bool",
                  "grapheme_cluster_break", "word_break", "sentence_break"]:
        replace(path, f"use crate::unicode_tables::{table}::BY_NAME;", f"use crate::unicode_tables::{table}::by_name;")
    replace(path, "property_set(BY_NAME, name)", "property_set(by_name, name)", count=7)
    replace(path, 'assert_eq!(AGES.len(), age::BY_NAME.len(), "ages are out of sync");',
            'assert_eq!(AGES.len(), age::BY_NAME_LEN, "ages are out of sync");')


def patch_ast(path: Path) -> None:
    replace(path, """            use super::unicode_tables::{
                property_names::PROPERTY_NAMES,
                property_values::PROPERTY_VALUES,
            };""", """            use super::unicode_tables::{property_names, property_values};""")
    replace(path, """                    let all = PROPERTY_VALUES
                        .iter()
                        .flat_map(|e| e.1.iter())
                        .filter(|(name, _)| name.len() == 1)
                        .count();
                    let idx = u.choose_index(all)?;
                    let value = PROPERTY_VALUES
                        .iter()
                        .flat_map(|e| e.1.iter())
                        .take(idx + 1)
                        .last()
                        .unwrap()
                        .0""", """                    let all = property_values::all()
                        .filter(|(_, name, _)| name.len() == 1)
                        .count();
                    let idx = u.choose_index(all)?;
                    let value = property_values::all()
                        .take(idx + 1)
                        .last()
                        .unwrap()
                        .1""")
    replace(path, """                    let all = PROPERTY_VALUES
                        .iter()
                        .map(|e| e.1.len())
                        .sum::<usize>()
                        + PROPERTY_NAMES.len();
                    let idx = u.choose_index(all)?;
                    let name = PROPERTY_VALUES
                        .iter()
                        .flat_map(|e| e.1.iter())
                        .chain(PROPERTY_NAMES)
                        .map(|(_, e)| e)""", """                    let all =
                        property_values::all().count() + property_names::all().count();
                    let idx = u.choose_index(all)?;
                    let name = property_values::all()
                        .map(|(_, _, e)| e)
                        .chain(property_names::all().map(|(_, e)| e))""")
    replace(path, """                    let all = PROPERTY_VALUES
                        .iter()
                        .map(|e| e.1.len())
                        .sum::<usize>();
                    let idx = u.choose_index(all)?;
                    let (prop, value) = PROPERTY_VALUES
                        .iter()
                        .flat_map(|e| {
                            e.1.iter().map(|(_, value)| (e.0, value))
                        })""", """                    let all = property_values::all().count();
                    let idx = u.choose_index(all)?;
                    let (prop, value) = property_values::all()
                        .map(|(prop, _, value)| (prop, value))""")


def main() -> int:
    if len(sys.argv) != 2:
        print(__doc__, file=sys.stderr)
        return 2
    source = Path(sys.argv[1])
    if OUT.exists():
        shutil.rmtree(OUT)
    shutil.copytree(source, OUT, ignore=lambda _dir, names: [n for n in names if n in DROPPED])
    for path in OUT.rglob("*"):
        if path.is_file():
            path.chmod(0o644)
    tables = OUT / "src/unicode_tables"
    for path in sorted(tables.glob("*.rs")):
        text = path.read_text()
        new = by_name(text, path)
        if path.name == "property_names.rs":
            new = property_names(new)
        if path.name == "property_values.rs":
            new = property_values(new)
        if path.name == "case_folding_simple.rs":
            new = case_folding(new)
        if "pub const BY_NAME:" in new:
            print(f"{path}: BY_NAME left", file=sys.stderr)
            return 1
        path.write_text(new)
    patch_unicode(OUT / "src/unicode.rs")
    patch_ast(OUT / "src/ast/mod.rs")
    print(f"wrote {OUT}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
