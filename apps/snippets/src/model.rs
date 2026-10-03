//! Pure data model for the snippet library.
//!
//! A library is a flat list of [`Snippet`]s plus a derived view of the
//! folders and tags that organise them. Folders are an explicit string
//! field on each snippet; tags are a set. Everything here is plain data
//! and pure logic — no IO, no CRDT, no rendering. The [`crate::doc`]
//! layer projects this onto an Automerge document so libraries sync
//! across devices; the widgets render it.

use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Stable identifier for one snippet. Backs every cross-reference and
/// the per-snippet CRDT object key.
pub type SnippetId = Uuid;

/// The syntax a snippet body is written in. Drives both the language
/// badge and the syntax highlighter's grammar. `PlainText` is the
/// fallback for prose snippets.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Language {
    PlainText,
    Shell,
    Rust,
    Sql,
    Git,
    Python,
    JavaScript,
    Json,
    Yaml,
    Markdown,
    Html,
    Css,
    Toml,
    Dockerfile,
}

impl Language {
    /// Every language, in the order they appear in the filter and the
    /// language picker.
    pub const ALL: [Language; 14] = [
        Language::PlainText,
        Language::Shell,
        Language::Rust,
        Language::Sql,
        Language::Git,
        Language::Python,
        Language::JavaScript,
        Language::Json,
        Language::Yaml,
        Language::Markdown,
        Language::Html,
        Language::Css,
        Language::Toml,
        Language::Dockerfile,
    ];

    /// Human-readable label shown on the badge and in the picker.
    pub fn label(self) -> &'static str {
        match self {
            Language::PlainText => "Plain text",
            Language::Shell => "Shell",
            Language::Rust => "Rust",
            Language::Sql => "SQL",
            Language::Git => "Git",
            Language::Python => "Python",
            Language::JavaScript => "JavaScript",
            Language::Json => "JSON",
            Language::Yaml => "YAML",
            Language::Markdown => "Markdown",
            Language::Html => "HTML",
            Language::Css => "CSS",
            Language::Toml => "TOML",
            Language::Dockerfile => "Dockerfile",
        }
    }

    /// Lowercase token used as the on-wire value and to match
    /// `language:foo` search filters.
    pub fn slug(self) -> &'static str {
        match self {
            Language::PlainText => "text",
            Language::Shell => "shell",
            Language::Rust => "rust",
            Language::Sql => "sql",
            Language::Git => "git",
            Language::Python => "python",
            Language::JavaScript => "javascript",
            Language::Json => "json",
            Language::Yaml => "yaml",
            Language::Markdown => "markdown",
            Language::Html => "html",
            Language::Css => "css",
            Language::Toml => "toml",
            Language::Dockerfile => "dockerfile",
        }
    }

    /// Parse a slug back into a language, defaulting to plain text.
    pub fn from_slug(s: &str) -> Language {
        Language::ALL
            .into_iter()
            .find(|l| l.slug() == s)
            .unwrap_or(Language::PlainText)
    }

    /// The short name the list-row badge shows: the language's own
    /// abbreviation where it has one, otherwise its name.
    pub fn badge(self) -> &'static str {
        match self {
            Language::PlainText => "Text",
            Language::Shell => "Shell",
            Language::Rust => "Rust",
            Language::Sql => "SQL",
            Language::Git => "Git",
            Language::Python => "Python",
            Language::JavaScript => "JS",
            Language::Json => "JSON",
            Language::Yaml => "YAML",
            Language::Markdown => "MD",
            Language::Html => "HTML",
            Language::Css => "CSS",
            Language::Toml => "TOML",
            Language::Dockerfile => "Docker",
        }
    }
}

/// One reusable snippet. The body may contain `${name}` placeholders
/// that the expand flow fills in before copying.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Snippet {
    pub id: SnippetId,
    pub title: String,
    pub language: Language,
    /// Logical folder this snippet lives in (e.g. "Deploy", "Git").
    /// Empty string == the implicit "All Snippets" root.
    pub folder: String,
    pub tags: Vec<String>,
    pub body: String,
    /// Free-text note shown above the body in the detail pane.
    pub description: String,
    /// Whether the user starred this snippet.
    pub favorite: bool,
    /// Monotonic counter bumped on each copy — drives the "Most used"
    /// smart folder and the per-row usage hint.
    pub uses: u32,
}

impl Snippet {
    /// A blank snippet with a fresh id, ready to be edited.
    pub fn blank(folder: &str) -> Self {
        Self {
            id: Uuid::new_v4(),
            title: "Untitled snippet".to_string(),
            language: Language::PlainText,
            folder: folder.to_string(),
            tags: Vec::new(),
            body: String::new(),
            description: String::new(),
            favorite: false,
            uses: 0,
        }
    }

    /// Extract the ordered, de-duplicated set of `${placeholder}` names
    /// in the body. Placeholders are `${` + identifier-ish chars + `}`.
    /// The first occurrence wins for ordering so the expand form reads
    /// top-to-bottom.
    pub fn placeholders(&self) -> Vec<String> {
        placeholders_in(&self.body)
    }

    /// True if the body has at least one `${placeholder}`.
    pub fn has_placeholders(&self) -> bool {
        !self.placeholders().is_empty()
    }
}

/// Pull the ordered, de-duplicated `${name}` placeholders out of `body`.
/// A name runs from `${` to the next `}`; empty or whitespace-only names
/// are ignored. Standalone `$` and unmatched `${` are left as literal
/// text.
pub fn placeholders_in(body: &str) -> Vec<String> {
    let bytes = body.as_bytes();
    let mut out: Vec<String> = Vec::new();
    let mut i = 0;
    while i + 1 < bytes.len() {
        // Find the closing brace.
        if bytes.get(i).copied() == Some(b'$')
            && bytes.get(i + 1).copied() == Some(b'{')
            && let Some(rest) = body.get(i + 2..)
            && let Some(rel) = rest.find('}')
            && let Some(name) = body.get(i + 2..i + 2 + rel)
        {
            let name = name.trim().to_string();
            if !name.is_empty() && !out.contains(&name) {
                out.push(name);
            }
            i = i + 2 + rel + 1;
            continue;
        }
        i += 1;
    }
    out
}

/// Substitute `${name}` occurrences in `body` with the matching value
/// from `values`. A placeholder with no value — either absent from the
/// map or mapped to an empty string — is left untouched so a partial fill
/// stays legible and the preview keeps showing which slots remain.
pub fn expand<S: std::hash::BuildHasher>(
    body: &str,
    values: &std::collections::HashMap<String, String, S>,
) -> String {
    let mut out = String::with_capacity(body.len());
    let mut i = 0;
    while let Some(rest) = body.get(i..) {
        let Some(c) = rest.chars().next() else {
            break;
        };
        let c_len = c.len_utf8();
        if c == '$'
            && let Some(after_dollar) = body.get(i + c_len..)
            && after_dollar.starts_with('{')
            && let Some(rel) = after_dollar.get(1..).and_then(|r| r.find('}'))
        {
            let brace_start = i + c_len + 1;
            let end = brace_start + rel;
            if let Some(name) = body.get(brace_start..end) {
                let name = name.trim();
                match values.get(name) {
                    Some(v) if !v.is_empty() => out.push_str(v),
                    // Absent or empty: keep the placeholder visible.
                    _ => {
                        if let Some(slice) = body.get(i..=end) {
                            out.push_str(slice);
                        }
                    }
                }
            }
            i = end + 1;
            continue;
        }
        // Push the whole character, not its individual bytes — a raw
        // `bytes[i] as char` here would mangle any multi-byte UTF-8
        // character in the body into mojibake.
        out.push(c);
        i += c_len;
    }
    out
}

/// A built-in smart folder that selects snippets by a rule rather than
/// by an explicit `folder` field.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SmartFolder {
    All,
    Favorites,
    MostUsed,
}

impl SmartFolder {
    pub fn label(self) -> &'static str {
        match self {
            SmartFolder::All => "All snippets",
            SmartFolder::Favorites => "Favorites",
            SmartFolder::MostUsed => "Most used",
        }
    }
}

/// Which collection of snippets the sidebar selection resolves to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Selection {
    Smart(SmartFolder),
    Folder(String),
    Tag(String),
}

impl Default for Selection {
    fn default() -> Self {
        Selection::Smart(SmartFolder::All)
    }
}

/// In-memory snippet library with the query/mutation ops the UI needs.
#[derive(Clone, Debug, Default)]
pub struct Library {
    pub snippets: Vec<Snippet>,
}

impl Library {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn get(&self, id: SnippetId) -> Option<&Snippet> {
        self.snippets.iter().find(|s| s.id == id)
    }

    pub fn get_mut(&mut self, id: SnippetId) -> Option<&mut Snippet> {
        self.snippets.iter_mut().find(|s| s.id == id)
    }

    pub fn insert(&mut self, snippet: Snippet) {
        self.snippets.push(snippet);
    }

    pub fn remove(&mut self, id: SnippetId) {
        self.snippets.retain(|s| s.id != id);
    }

    /// Distinct folder names in stable, sorted order. The empty-string
    /// root folder is never returned (it's covered by the "All" smart
    /// folder).
    pub fn folders(&self) -> Vec<String> {
        let mut set: Vec<String> = self
            .snippets
            .iter()
            .map(|s| s.folder.clone())
            .filter(|f| !f.is_empty())
            .collect();
        set.sort();
        set.dedup();
        set
    }

    /// Distinct tags across the whole library, sorted, with the per-tag
    /// snippet count.
    pub fn tags(&self) -> Vec<(String, usize)> {
        use std::collections::BTreeMap;
        let mut counts: BTreeMap<String, usize> = BTreeMap::new();
        for s in &self.snippets {
            for t in &s.tags {
                *counts.entry(t.clone()).or_default() += 1;
            }
        }
        counts.into_iter().collect()
    }

    /// Resolve a sidebar [`Selection`] into the ordered list of snippet ids
    /// to show in the list pane. Favorites sort first, then by title.
    pub fn query(&self, selection: &Selection) -> Vec<SnippetId> {
        let mut matched: Vec<&Snippet> = self
            .snippets
            .iter()
            .filter(|s| selection_matches(selection, s))
            .collect();
        // Smart "Most used" sorts by usage desc; everything else sorts
        // favorites-first then alphabetical.
        if matches!(selection, Selection::Smart(SmartFolder::MostUsed)) {
            matched.sort_by(|a, b| b.uses.cmp(&a.uses).then(a.title.cmp(&b.title)));
        } else {
            matched.sort_by(|a, b| {
                b.favorite
                    .cmp(&a.favorite)
                    .then_with(|| a.title.to_lowercase().cmp(&b.title.to_lowercase()))
            });
        }
        matched.into_iter().map(|s| s.id).collect()
    }
}

/// Whether a [`Selection`] includes `snippet`.
fn selection_matches(selection: &Selection, snippet: &Snippet) -> bool {
    match selection {
        Selection::Smart(SmartFolder::All) => true,
        Selection::Smart(SmartFolder::Favorites) => snippet.favorite,
        Selection::Smart(SmartFolder::MostUsed) => snippet.uses > 0,
        Selection::Folder(f) => &snippet.folder == f,
        Selection::Tag(t) => snippet.tags.iter().any(|x| x == t),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    /// Placeholder ordering/dedup, expansion and language slugs.
    #[test]
    fn placeholders_expand_and_language_slugs_round_trip() {
        // Case `placeholders_are_ordered_and_deduped`.
        {
            let s = Snippet {
                body: "echo ${name} then ${host} then ${name} again".into(),
                ..Snippet::blank("")
            };
            assert_eq!(s.placeholders(), vec!["name", "host"]);
        }

        // Case `placeholders_ignore_unmatched_and_empty`.
        {
            assert_eq!(placeholders_in("a $ b ${} c ${ok}"), vec!["ok"]);
            assert_eq!(placeholders_in("no braces here"), Vec::<String>::new());
        }

        // Case `expand_substitutes_known_keeps_unknown`.
        {
            let mut v = HashMap::new();
            v.insert("name".to_string(), "world".to_string());
            let out = expand("hi ${name}, ${missing}", &v);
            assert_eq!(out, "hi world, ${missing}");
        }

        // Case `language_slug_round_trips`.
        {
            for l in Language::ALL {
                assert_eq!(Language::from_slug(l.slug()), l);
            }
        }
    }

    /// Favourite ordering, smart favourites and folder filters.
    #[test]
    fn library_queries_filter_and_sort() {
        // Case `favorites_sort_before_others`.
        {
            let mut lib = Library::new();
            lib.insert(Snippet {
                title: "zebra".into(),
                favorite: true,
                ..Snippet::blank("")
            });
            lib.insert(Snippet {
                title: "apple".into(),
                favorite: false,
                ..Snippet::blank("")
            });
            let ids = lib.query(&Selection::default());
            assert_eq!(lib.get(ids[0]).unwrap().title, "zebra");
        }

        // Case `smart_favorites_only_includes_starred`.
        {
            let mut lib = Library::new();
            lib.insert(Snippet {
                title: "a".into(),
                favorite: true,
                ..Snippet::blank("")
            });
            lib.insert(Snippet {
                title: "b".into(),
                favorite: false,
                ..Snippet::blank("")
            });
            let ids = lib.query(&Selection::Smart(SmartFolder::Favorites));
            assert_eq!(ids.len(), 1);
            assert!(lib.get(ids[0]).unwrap().favorite);
        }

        // Case `folder_selection_filters_by_folder`.
        {
            let mut lib = Library::new();
            lib.insert(Snippet {
                title: "a".into(),
                ..Snippet::blank("Deploy")
            });
            lib.insert(Snippet {
                title: "b".into(),
                ..Snippet::blank("Git")
            });
            let ids = lib.query(&Selection::Folder("Deploy".into()));
            assert_eq!(ids.len(), 1);
            assert_eq!(lib.get(ids[0]).unwrap().folder, "Deploy");
        }
    }
}
