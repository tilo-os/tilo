//! What the start module is made of: its function bodies' bytes by the part
//! of the program each comes from (`crate::module`, read from the v0-mangled
//! names the splitter keeps), so the size budget can name the part that grew
//! (`tools/web/wasm_size_ratchet.py`).

use std::collections::BTreeMap;

use anyhow::Result;

/// The parts of the program the start module holds, largest first in
/// spirit: `crate::module` (or just `crate`) to bytes of function bodies.
/// Only parts of at least `floor` bytes are listed; the rest is `other`.
pub fn start_parts(bytes: &[u8], floor: u64) -> Result<BTreeMap<String, u64>> {
    let mut imported = 0_u32;
    let mut sizes: Vec<u64> = Vec::new();
    let mut names: BTreeMap<u32, String> = BTreeMap::new();
    for payload in wasmparser::Parser::new(0).parse_all(bytes) {
        match payload? {
            wasmparser::Payload::ImportSection(imports) => {
                for import in imports {
                    if matches!(import?.ty, wasmparser::TypeRef::Func(_)) {
                        imported += 1;
                    }
                }
            }
            wasmparser::Payload::CodeSectionEntry(body) => {
                sizes.push(body.range().len() as u64);
            }
            wasmparser::Payload::CustomSection(section) => {
                if let wasmparser::KnownCustom::Name(reader) = section.as_known() {
                    for name in reader {
                        if let wasmparser::Name::Function(map) = name? {
                            for naming in map {
                                let naming = naming?;
                                names.insert(naming.index, naming.name.to_owned());
                            }
                        }
                    }
                }
            }
            _ => {}
        }
    }
    let mut parts: BTreeMap<String, u64> = BTreeMap::new();
    for (local, size) in sizes.iter().enumerate() {
        let index = imported + u32::try_from(local)?;
        let part = names
            .get(&index)
            .map_or_else(|| "other".to_owned(), |name| part_of(name));
        *parts.entry(part).or_default() += size;
    }
    let mut listed = BTreeMap::new();
    let mut other = 0;
    for (part, size) in parts {
        if size >= floor && part != "other" {
            listed.insert(part, size);
        } else {
            other += size;
        }
    }
    listed.insert("other".to_owned(), other);
    Ok(listed)
}

/// The standard library's crates: a generic of theirs instantiated for a
/// type of the program (`core::ptr::drop_glue::<tilo_ui::Button>`) counts
/// toward that type's part.
const STANDARD: [&str; 3] = ["core", "alloc", "std"];

/// `crate::module` of a v0-mangled name (`_RNvNtCs1a_7tilo_ui8headless4item`
/// is `tilo_ui::headless`): the first crate root it names outside the
/// standard library (else its first) and the identifier right after it.
/// `other` for a name that is not v0-mangled.
fn part_of(name: &str) -> String {
    let bytes = name.as_bytes();
    if !name.starts_with("_R") {
        return "other".to_owned();
    }
    let mut first = None;
    let mut at = 2;
    while at < bytes.len() {
        let root = if bytes[at] == b'C' {
            crate_root(bytes, at + 1)
        } else {
            None
        };
        if let Some((krate, after)) = root {
            let part = match identifier(bytes, after) {
                Some((module, _)) => format!("{krate}::{module}"),
                None => krate.to_owned(),
            };
            if !STANDARD.contains(&krate) {
                return part;
            }
            first.get_or_insert(part);
            at = after;
            continue;
        }
        at += 1;
    }
    first.unwrap_or_else(|| "other".to_owned())
}

/// The crate name of a crate root whose tag precedes `at`: an optional
/// disambiguator (`s` base-62 digits `_`), then the name; and where it ends.
fn crate_root(bytes: &[u8], at: usize) -> Option<(&str, usize)> {
    let mut cursor = at;
    if bytes.get(cursor) == Some(&b's') {
        cursor += 1;
        while cursor < bytes.len() && bytes[cursor].is_ascii_alphanumeric() {
            cursor += 1;
        }
        if bytes.get(cursor) != Some(&b'_') {
            return None;
        }
        cursor += 1;
    }
    identifier(bytes, cursor)
}

/// A length-prefixed identifier at `at`, and where it ends.
fn identifier(bytes: &[u8], at: usize) -> Option<(&str, usize)> {
    let digits = bytes[at.min(bytes.len())..]
        .iter()
        .take_while(|byte| byte.is_ascii_digit())
        .count();
    if digits == 0 || bytes.get(at) == Some(&b'0') {
        return None;
    }
    let length: usize = std::str::from_utf8(&bytes[at..at + digits])
        .ok()?
        .parse()
        .ok()?;
    let start = at + digits;
    // A `_` separates an identifier that starts with a digit or `_`.
    let start = if bytes.get(start) == Some(&b'_') {
        start + 1
    } else {
        start
    };
    let end = start.checked_add(length)?;
    let ident = std::str::from_utf8(bytes.get(start..end)?).ok()?;
    ident
        .bytes()
        .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
        .then_some((ident, end))
}

#[cfg(test)]
mod tests {
    use super::part_of;

    #[test]
    fn a_v0_name_reads_as_its_crate_and_module() {
        assert_eq!(
            part_of("_RNvNtCs4OKXud0Q8dV_7tilo_ui8headless4item"),
            "tilo_ui::headless"
        );
        assert_eq!(
            part_of(
                "_RNvMs1_NtCsbpKMap6vRW1_20tilo_browser_runtime5shellNtB5_12BrowserShell5chord"
            ),
            "tilo_browser_runtime::shell"
        );
        assert_eq!(part_of("_RNvCs1a_4core3fmt"), "core::fmt");
        assert_eq!(
            part_of(
                "_RINvNtCsdkdt1aaAg1T_4core3ptr9drop_glueNtNtNtCs4OKXud0Q8dV_7tilo_ui6widget15command_palette14CommandPaletteE"
            ),
            "tilo_ui::widget"
        );
        assert_eq!(part_of("memcpy"), "other");
    }
}
