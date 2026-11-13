//! A declaration's words: keys of the catalogue beside the crate that
//! declares them, read when the crate is built.
//!
//! An action's label and a resource's name are catalogue keys, derived by
//! rule from what they name (`<operation or kind>-<action id>`, the kind for
//! a resource) or named outright (`label = "files-open"`) where the crate
//! already says the same thing under another key. The derive reads the key's
//! text from `locales/en-US.ftl`, the source language every other language
//! translates, and publishes the word with the catalogue it is in, so it is
//! read in the person's language whenever it is published.

use std::cell::RefCell;
use std::collections::HashMap;
use std::path::PathBuf;

use proc_macro2::{Span, TokenStream as Tokens};
use quote::quote;

/// The source language, the file every crate's words are written in.
const SOURCE: &str = "en-US";

/// The default directory of a crate's catalogues.
pub(crate) const LOCALES: &str = "locales";

/// The catalogue of one crate, as its source language says it.
pub(crate) struct Catalogue {
    /// The directory, relative to the crate's manifest.
    dir: String,
    messages: HashMap<String, String>,
    locales: Vec<String>,
    /// Every declaration whose key it does not hold, so one build names
    /// every line to add.
    missing: RefCell<Option<syn::Error>>,
}

impl Catalogue {
    /// The catalogue in `dir` (relative to the crate being built); empty
    /// when there is none.
    pub(crate) fn read(dir: &str) -> Self {
        let root = std::env::var_os("CARGO_MANIFEST_DIR").map_or_else(PathBuf::new, PathBuf::from);
        let path = root.join(dir);
        let text = std::fs::read_to_string(path.join(format!("{SOURCE}.ftl"))).unwrap_or_default();
        let mut locales: Vec<String> = std::fs::read_dir(&path)
            .into_iter()
            .flatten()
            .flatten()
            .filter_map(|entry| {
                entry
                    .file_name()
                    .to_str()?
                    .strip_suffix(".ftl")
                    .map(str::to_owned)
            })
            .collect();
        locales.sort();
        Self {
            dir: dir.to_owned(),
            messages: messages(&text),
            locales,
            missing: RefCell::new(None),
        }
    }

    /// The source text of `key`, when the catalogue has it.
    pub(crate) fn text(&self, key: &str) -> Option<&str> {
        self.messages.get(key).map(String::as_str)
    }

    /// The word `key`, whose source text is `text`: a `Word` whose catalogue
    /// is this one.
    pub(crate) fn word(&self, krate: &Tokens, key: &str, text: &str) -> Tokens {
        let catalogue = self.reference(krate);
        quote! {
            #krate::label::Word {
                key: #key,
                source: #text,
                catalogue: ::core::option::Option::Some(#catalogue),
            }
        }
    }

    /// A `&'static Catalogue` of this one, as the crate is built.
    pub(crate) fn reference(&self, krate: &Tokens) -> Tokens {
        let dir = format!("/{}", self.dir);
        let source = format!("{dir}/{SOURCE}.ftl");
        let locales = &self.locales;
        quote! {
            &#krate::label::Catalogue {
                id: ::core::env!("CARGO_PKG_NAME"),
                source: ::core::include_str!(::core::concat!(::core::env!("CARGO_MANIFEST_DIR"), #source)),
                dir: ::core::concat!(::core::env!("CARGO_MANIFEST_DIR"), #dir),
                locales: &[#(#locales),*],
            }
        }
    }

    /// The word a declaration names: `named` (`label = "key"`) or the key
    /// derived for it, read from this catalogue. A declaration whose key the
    /// catalogue does not hold does not build: [`Self::missing`] names the
    /// line to add, its text the declaration's own words, `written`.
    pub(crate) fn declared(
        &self,
        krate: &Tokens,
        span: Span,
        named: Option<&str>,
        derived: &str,
        written: &str,
    ) -> syn::Result<(Tokens, String)> {
        let key = named.unwrap_or(derived);
        let Some(text) = self.text(key) else {
            let file = format!("{}/{SOURCE}.ftl", self.dir);
            let error = syn::Error::new(
                span,
                match named {
                    Some(words) if !is_key(words) => format!(
                        "a label is a key of {file}, not words: add `{derived} = {words}` there \
                         and leave `label` out"
                    ),
                    Some(_) => format!("`{key}` is no message of {file}"),
                    None => format!("`{key}` is no message of {file}: add `{key} = {written}`"),
                },
            );
            match &mut *self.missing.borrow_mut() {
                Some(all) => all.combine(error),
                none => *none = Some(error),
            }
            return Ok((self.word(krate, key, written), written.to_owned()));
        };
        if text.contains('{') {
            return Err(syn::Error::new(
                span,
                "a label is plain words: it takes no `{ $argument }`",
            ));
        }
        Ok((self.word(krate, key, text), text.to_owned()))
    }
}

impl Catalogue {
    /// Every declaration [`Self::declared`] found no key for, as one error.
    pub(crate) fn missing(&self) -> syn::Result<()> {
        self.missing.take().map_or(Ok(()), Err)
    }
}

/// Whether `text` is a catalogue key rather than words: lower case letters,
/// digits and dashes.
fn is_key(text: &str) -> bool {
    text.contains('-')
        && text
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

/// The key a declaration's word is derived under: `scope` (an operation or a
/// kind) with its separators as dashes, then `id`.
pub(crate) fn key(scope: &str, id: &str) -> String {
    let scope = scope.replace(['.', '_', ':'], "-");
    if id.is_empty() {
        scope
    } else {
        format!("{scope}-{id}")
    }
}

/// The plain messages of a Fluent catalogue, by key: a value on the key's
/// line and the indented lines after it, joined by spaces. Attributes and
/// terms are left out.
fn messages(text: &str) -> HashMap<String, String> {
    let mut out = HashMap::new();
    let mut current: Option<String> = None;
    for line in text.lines() {
        if line.starts_with(' ') && !line.trim().is_empty() {
            let trimmed = line.trim();
            if trimmed.starts_with('.') {
                current = None;
            } else if let Some(key) = &current
                && let Some(value) = out.get_mut(key)
            {
                let value: &mut String = value;
                if !value.is_empty() {
                    value.push(' ');
                }
                value.push_str(trimmed);
            }
            continue;
        }
        current = None;
        if line.starts_with('#') || line.starts_with('-') {
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let key = key.trim();
        if key.is_empty()
            || !key
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
        {
            continue;
        }
        out.insert(key.to_owned(), value.trim().to_owned());
        current = Some(key.to_owned());
    }
    out
}
