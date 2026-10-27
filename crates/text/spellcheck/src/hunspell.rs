//! Hunspell dictionaries, checked in-process.
//!
//! The system's Hunspell dictionaries (`.aff` + `.dic`, as distributions ship
//! them under `/usr/share/hunspell`) are parsed and queried in this process by
//! `spellbook`, so checking works the same inside an app sandbox — which can
//! read the dictionary directory but cannot start helper programs — as
//! outside it. Each language loads once, on first use or on
//! [`warm_up`](super::warm_up_language), and is shared by every field.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};

/// Where distributions install Hunspell dictionaries. The app sandbox's
/// system read profile exposes the same directories.
pub const DICTIONARY_DIRS: &[&str] = &["/usr/share/hunspell", "/usr/share/myspell"];

type Slot = Arc<OnceLock<Option<spellbook::Dictionary>>>;

fn slot(dictionary: &str) -> Option<Slot> {
    static LOADED: OnceLock<Mutex<HashMap<String, Slot>>> = OnceLock::new();
    let loaded = LOADED.get_or_init(|| Mutex::new(HashMap::new()));
    let mut loaded = loaded.lock().ok()?;
    Some(Arc::clone(loaded.entry(dictionary.to_owned()).or_default()))
}

/// The dictionary for `language`, loading it on first use.
fn dictionary(language: &str) -> Option<Slot> {
    let name = super::normalize_language(language)?;
    let slot = slot(&name)?;
    slot.get_or_init(|| {
        let dirs: Vec<PathBuf> = DICTIONARY_DIRS.iter().map(PathBuf::from).collect();
        load_from(&name, &dirs)
    });
    slot.get()?.is_some().then_some(slot)
}

/// Load `<name>.aff` / `<name>.dic` from the first directory holding them.
/// A bare language (`de`) takes the first regional dictionary (`de_DE`).
pub(super) fn load_from(name: &str, dirs: &[PathBuf]) -> Option<spellbook::Dictionary> {
    let (aff, dic) = dirs.iter().find_map(|dir| locate(dir, name))?;
    let aff = reencode_aff(&std::fs::read(aff).ok()?);
    let dic = decode(&std::fs::read(dic).ok()?, declared_encoding(&aff));
    spellbook::Dictionary::new(&aff, &dic).ok()
}

fn locate(dir: &Path, name: &str) -> Option<(PathBuf, PathBuf)> {
    let pair = |stem: &str| {
        let aff = dir.join(format!("{stem}.aff"));
        let dic = dir.join(format!("{stem}.dic"));
        (aff.is_file() && dic.is_file()).then_some((aff, dic))
    };
    if let Some(found) = pair(name) {
        return Some(found);
    }
    if name.contains('_') {
        return None;
    }
    let prefix = format!("{name}_");
    let mut regional: Vec<String> = std::fs::read_dir(dir)
        .ok()?
        .filter_map(|entry| {
            let file = entry.ok()?.file_name().into_string().ok()?;
            let stem = file.strip_suffix(".dic")?;
            stem.starts_with(&prefix).then(|| stem.to_owned())
        })
        .collect();
    regional.sort();
    regional.iter().find_map(|stem| pair(stem))
}

/// The `SET` encoding an `.aff` file declares; it governs both files.
fn declared_encoding(aff: &str) -> Option<&'static encoding_rs::Encoding> {
    aff.lines()
        .find_map(|line| line.trim().strip_prefix("SET "))
        .and_then(|label| encoding_rs::Encoding::for_label(label.trim().as_bytes()))
}

/// Decode a dictionary file with `encoding`, or as UTF-8 falling back to
/// Latin-1 when none is declared.
fn decode(bytes: &[u8], encoding: Option<&'static encoding_rs::Encoding>) -> String {
    match encoding {
        Some(encoding) => encoding.decode_without_bom_handling(bytes).0.into_owned(),
        None => match std::str::from_utf8(bytes) {
            Ok(text) => text.to_owned(),
            Err(_) => encoding_rs::WINDOWS_1252
                .decode_without_bom_handling(bytes)
                .0
                .into_owned(),
        },
    }
}

/// Decode an `.aff`: first as UTF-8-or-Latin-1 to find its ASCII `SET` line,
/// then with the encoding it declares.
fn reencode_aff(bytes: &[u8]) -> String {
    let first = decode(bytes, None);
    match declared_encoding(&first) {
        Some(encoding) => decode(bytes, Some(encoding)),
        None => first,
    }
}

/// Start loading `language` so the first keystroke does not pay for it.
/// Whether the system has a dictionary for it.
pub(super) fn warm_up(language: &str) -> bool {
    dictionary(language).is_some()
}

/// Whether each word is spelled correctly; `None` without a dictionary.
pub(super) fn results(language: &str, words: &[&str]) -> Option<Vec<bool>> {
    let slot = dictionary(language)?;
    let dictionary = slot.get()?.as_ref()?;
    Some(words.iter().map(|word| dictionary.check(word)).collect())
}

/// Corrections for `word`, best first; `None` without a dictionary.
pub(super) fn suggestions(language: &str, word: &str) -> Option<Vec<String>> {
    let slot = dictionary(language)?;
    let dictionary = slot.get()?.as_ref()?;
    let mut out = Vec::new();
    dictionary.suggest(word, &mut out);
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A tiny Latin-1 dictionary with one suffix rule, written the way many
    /// distribution dictionaries are encoded.
    fn latin1_dictionary(dir: &Path, stem: &str) {
        std::fs::write(
            dir.join(format!("{stem}.aff")),
            b"SET ISO8859-1\nTRY esianrtolcdugmphbyfvkwz\nSFX S Y 1\nSFX S 0 e .\n",
        )
        .unwrap();
        // "stra\xdfe" is "straße" and "gr\xfcn" is "grün" in ISO 8859-1.
        std::fs::write(
            dir.join(format!("{stem}.dic")),
            b"3\nstra\xdfe\nhaus/S\ngr\xfcn\n",
        )
        .unwrap();
    }

    #[test]
    fn system_dictionaries_load_in_process_with_their_declared_encoding() {
        let dir = tempfile::tempdir().unwrap();
        latin1_dictionary(dir.path(), "xx_YY");
        let dirs = vec![PathBuf::from("/nonexistent"), dir.path().to_path_buf()];

        let dictionary = load_from("xx_YY", &dirs).expect("dictionary loads");
        assert!(dictionary.check("straße"), "Latin-1 bytes decode");
        assert!(dictionary.check("grün"));
        assert!(dictionary.check("hause"), "affix rules apply");
        assert!(!dictionary.check("hausx"));
        let mut suggestions = Vec::new();
        dictionary.suggest("hasu", &mut suggestions);
        assert!(
            suggestions.iter().any(|word| word == "haus"),
            "{suggestions:?}"
        );

        // A bare language finds its regional dictionary; a missing one is
        // simply absent.
        assert!(load_from("xx", &dirs).is_some());
        assert!(load_from("zz_ZZ", &dirs).is_none());
    }
}
