//! How much a person uses each category of file: the documents they opened
//! recently, through Tilo or any other app, as `recently-used.xbel` lists
//! them (the freedesktop list the shell publishes every open to).
//!
//! Settings orders its default-app categories by it, most used first, and
//! shows the types of files the person has even where no app claims them.

use std::collections::{BTreeMap, BTreeSet};

use crate::defaults::TypeCategory;

/// The most of the list read: past this it is not a bookmark list.
#[cfg(all(feature = "host-storage", not(target_arch = "wasm32")))]
const MAX_BYTES: u64 = 8 * 1024 * 1024;

/// The longest extension counted.
const MAX_EXTENSION: usize = 16;

/// The documents opened recently, by category.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TypeUsage {
    /// How many recently opened documents each category holds.
    pub opened: BTreeMap<TypeCategory, u32>,
    /// The extensions of those documents, by category.
    pub extensions: BTreeMap<TypeCategory, BTreeSet<String>>,
}

impl TypeUsage {
    /// How many recently opened documents `category` holds.
    #[must_use]
    pub fn count(&self, category: TypeCategory) -> u32 {
        self.opened.get(&category).copied().unwrap_or_default()
    }

    /// Count one opened document with extension `extension`.
    pub fn note(&mut self, extension: &str) {
        let extension = extension.to_ascii_lowercase();
        let category = TypeCategory::of_extension(&extension);
        let count = self.opened.entry(category).or_default();
        *count = count.saturating_add(1);
        self.extensions
            .entry(category)
            .or_default()
            .insert(extension);
    }
}

/// The person's recently opened documents, from
/// `$XDG_DATA_HOME/recently-used.xbel`; none when there is no such list.
#[cfg(all(feature = "host-storage", not(target_arch = "wasm32")))]
#[must_use]
pub fn recent_type_usage() -> TypeUsage {
    use std::io::Read as _;
    let Some(path) = tilo_paths::xdg::data_home().map(|data| data.join("recently-used.xbel"))
    else {
        return TypeUsage::default();
    };
    let mut document = String::new();
    let read = std::fs::File::open(&path)
        .and_then(|file| file.take(MAX_BYTES).read_to_string(&mut document));
    match read {
        Ok(_) => parse_usage(&document),
        Err(_) => TypeUsage::default(),
    }
}

/// The documents an XBEL list names, by category: each `<bookmark>`'s
/// local file, by its extension.
#[must_use]
pub fn parse_usage(document: &str) -> TypeUsage {
    let mut usage = TypeUsage::default();
    for bookmark in document.split("<bookmark").skip(1) {
        let Some(extension) = href(bookmark).and_then(extension_of_uri) else {
            continue;
        };
        usage.note(extension);
    }
    usage
}

/// The `href` attribute at the head of a bookmark element.
fn href(element: &str) -> Option<&str> {
    let tag = element.split('>').next()?;
    let start = tag.find("href=")?.checked_add("href=".len())?;
    let rest = tag.get(start..)?;
    let quote = rest.chars().next().filter(|q| matches!(q, '"' | '\''))?;
    let body = rest.get(quote.len_utf8()..)?;
    body.split(quote).next()
}

/// The extension of a local file's URI: `file:///home/a/Thesis.typ` is
/// `typ`. Anything that is not a plain extension is none.
fn extension_of_uri(uri: &str) -> Option<&str> {
    let path = uri.strip_prefix("file://")?;
    let path = path.split(['?', '#']).next()?;
    let name = path.rsplit('/').next()?;
    let (stem, extension) = name.rsplit_once('.')?;
    (!stem.is_empty()
        && !extension.is_empty()
        && extension.len() <= MAX_EXTENSION
        && extension.bytes().all(|b| b.is_ascii_alphanumeric()))
    .then_some(extension)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recent_documents_count_toward_their_category() {
        let document = r#"<?xml version="1.0" encoding="UTF-8"?>
<xbel version="1.0">
  <bookmark href="file:///home/ada/src/main.rs" added="2026-10-20T10:00:00Z" modified="2026-10-20T10:00:00Z" visited="2026-10-20T10:00:00Z">
  </bookmark>
  <bookmark href="file:///home/ada/src/Cargo.TOML" modified="2026-10-20T10:00:00Z"></bookmark>
  <bookmark href='file:///home/ada/Letters/Dear%20Bea.odt' modified="2026-10-20T10:00:00Z"></bookmark>
  <bookmark href="file:///home/ada/.bashrc" modified="2026-10-20T10:00:00Z"></bookmark>
  <bookmark href="https://example.org/page.html" modified="2026-10-20T10:00:00Z"></bookmark>
  <bookmark href="file:///home/ada/notes" modified="2026-10-20T10:00:00Z"></bookmark>
</xbel>"#;
        let usage = parse_usage(document);
        assert_eq!(usage.count(TypeCategory::Code), 2);
        assert_eq!(usage.count(TypeCategory::Documents), 1);
        assert_eq!(usage.count(TypeCategory::Web), 0, "only local files count");
        assert_eq!(usage.count(TypeCategory::Other), 0, "a dotfile has no type");
        assert_eq!(
            usage.extensions.get(&TypeCategory::Code),
            Some(&["rs".to_owned(), "toml".to_owned()].into())
        );
    }
}
