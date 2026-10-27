//! The snippet library's model: what is stored, and the pure queries the
//! window asks of it.
//!
//! [`Snippets`] is the record: one [`Snippet`] per item of a
//! [`Collection`], each a typed struct whose fields merge the way their types
//! say (a body merges character by character, tags per tag, uses add up).
//! `#[derive(Model)]` is the whole storage contract; nothing here touches
//! the document. The record's verbs are in [`crate::resource`].

use std::collections::BTreeMap;

use ui::tilo_resource::{self, Atom, Collection, Counter, Id, Model, Refusal, Set};

/// The id of one snippet: its item in the library's collection.
pub type SnippetId = Id;

/// The syntax a snippet body is written in. Drives both the language
/// badge and the syntax highlighter's grammar. `PlainText` is the
/// fallback for prose snippets.
#[derive(Model, Default, Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Language {
    #[default]
    #[model(rename = "text")]
    PlainText,
    Shell,
    Rust,
    Sql,
    Git,
    Python,
    #[model(rename = "javascript")]
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

    /// The language's name in the person's language: the proper names of
    /// programming languages stay as they are; plain text is translated.
    pub fn name(self) -> String {
        match self {
            Language::PlainText => crate::tr("snippets-language-plain-text"),
            other => other.label().to_string(),
        }
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
#[derive(Model, Default, Clone, Debug, PartialEq)]
#[model(check = Snippet::valid)]
pub struct Snippet {
    /// Title
    #[model(label)]
    pub title: String,
    /// Body
    pub body: String,
    /// What the snippet is for, shown above the body.
    pub description: String,
    /// Starred
    pub favorite: bool,
    /// Folder: empty for none (only "All snippets" lists it). Written
    /// whole, so two people filing it at once do not merge two names.
    pub folder: Atom,
    /// Language
    pub language: Language,
    /// Tags
    pub tags: Set<String>,
    /// How often it was copied: drives "Most used" and the row's usage
    /// hint. Only copying it changes the count.
    #[model(readonly)]
    pub uses: Counter,
}

impl Snippet {
    /// A blank snippet titled `title`, filed under `folder`.
    pub fn titled(title: impl Into<String>, folder: impl Into<String>) -> Self {
        Self {
            title: title.into(),
            folder: Atom(folder.into()),
            ..Self::default()
        }
    }

    /// Folder and tag names are what the sidebar lists and what people
    /// type to find them again: never blank, never padded.
    fn valid(&self) -> Result<(), Refusal> {
        if self.folder.0.trim() != self.folder.0 {
            return Err(Refusal::new("a folder name has no surrounding spaces"));
        }
        if self
            .tags
            .iter()
            .any(|tag| tag.trim().is_empty() || tag.trim() != tag)
        {
            return Err(Refusal::new(
                "a tag is not blank and has no surrounding spaces",
            ));
        }
        Ok(())
    }

    /// Extract the ordered, de-duplicated set of `${placeholder}` names
    /// in the body. The first occurrence wins for ordering so the expand
    /// form reads top-to-bottom.
    pub fn placeholders(&self) -> Vec<String> {
        placeholders_in(&self.body)
    }

    /// True if the body has at least one `${placeholder}`.
    pub fn has_placeholders(&self) -> bool {
        !self.placeholders().is_empty()
    }

    /// How often the snippet was copied.
    pub fn use_count(&self) -> usize {
        usize::try_from(self.uses.get()).unwrap_or_default()
    }
}

/// The snippet library: every snippet, in the order they were made.
#[derive(Model, Default, Clone, Debug, PartialEq)]
pub struct Snippets {
    pub snippets: Collection<Snippet>,
}

impl Snippets {
    pub fn get(&self, id: &SnippetId) -> Option<&Snippet> {
        self.snippets.get(id)
    }

    pub fn is_empty(&self) -> bool {
        self.snippets.is_empty()
    }

    /// Distinct folder names, sorted. Snippets in no folder are covered by
    /// the "All" smart folder.
    pub fn folders(&self) -> Vec<String> {
        let mut folders: Vec<String> = self
            .snippets
            .values()
            .map(|s| s.folder.0.clone())
            .filter(|f| !f.is_empty())
            .collect();
        folders.sort();
        folders.dedup();
        folders
    }

    /// Distinct tags across the whole library, sorted, with the per-tag
    /// snippet count.
    pub fn tags(&self) -> Vec<(String, usize)> {
        let mut counts: BTreeMap<String, usize> = BTreeMap::new();
        for tag in self.snippets.values().flat_map(|s| s.tags.iter()) {
            *counts.entry(tag.clone()).or_default() += 1;
        }
        counts.into_iter().collect()
    }

    /// Resolve a sidebar [`Selection`] into the ordered snippets the list
    /// shows. "Most used" sorts by use, everything else favorites first,
    /// then by title.
    pub fn query(&self, selection: &Selection) -> Vec<SnippetId> {
        let mut matched: Vec<(&Id, &Snippet)> = self
            .snippets
            .iter()
            .filter(|(_, s)| selection_matches(selection, s))
            .collect();
        if matches!(selection, Selection::Smart(SmartFolder::MostUsed)) {
            matched.sort_by(|(_, a), (_, b)| b.uses.cmp(&a.uses).then(a.title.cmp(&b.title)));
        } else {
            matched.sort_by(|(_, a), (_, b)| {
                b.favorite
                    .cmp(&a.favorite)
                    .then_with(|| a.title.to_lowercase().cmp(&b.title.to_lowercase()))
            });
        }
        matched.into_iter().map(|(id, _)| id.clone()).collect()
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
    /// The stable, locale-independent key of the folder: control ids and
    /// sidebar item ids are built from it.
    pub fn key(self) -> &'static str {
        match self {
            SmartFolder::All => "all",
            SmartFolder::Favorites => "favorites",
            SmartFolder::MostUsed => "most-used",
        }
    }

    /// The folder's name in the person's language.
    pub fn label(self) -> String {
        crate::tr(match self {
            SmartFolder::All => "snippets-folder-all",
            SmartFolder::Favorites => "snippets-folder-favorites",
            SmartFolder::MostUsed => "snippets-folder-most-used",
        })
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

/// Whether a [`Selection`] includes `snippet`.
fn selection_matches(selection: &Selection, snippet: &Snippet) -> bool {
    match selection {
        Selection::Smart(SmartFolder::All) => true,
        Selection::Smart(SmartFolder::Favorites) => snippet.favorite,
        Selection::Smart(SmartFolder::MostUsed) => snippet.uses.get() > 0,
        Selection::Folder(f) => &snippet.folder.0 == f,
        Selection::Tag(t) => snippet.tags.contains(t),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn library(snippets: impl IntoIterator<Item = Snippet>) -> Snippets {
        let mut library = Snippets::default();
        for (n, snippet) in snippets.into_iter().enumerate() {
            library
                .snippets
                .push(Id::from_raw(format!("s{n}")), snippet);
        }
        library
    }

    /// Placeholder ordering/dedup and expansion.
    #[test]
    fn placeholders_expand() {
        let s = Snippet {
            body: "echo ${name} then ${host} then ${name} again".into(),
            ..Snippet::default()
        };
        assert_eq!(s.placeholders(), vec!["name", "host"]);
        assert_eq!(placeholders_in("a $ b ${} c ${ok}"), vec!["ok"]);
        assert_eq!(placeholders_in("no braces here"), Vec::<String>::new());

        let mut v = HashMap::new();
        v.insert("name".to_string(), "world".to_string());
        assert_eq!(expand("hi ${name}, ${missing}", &v), "hi world, ${missing}");
    }

    /// Favourite ordering, smart favourites, most used and folder and tag
    /// filters.
    #[test]
    fn library_queries_filter_and_sort() {
        let starred = Snippet {
            favorite: true,
            ..Snippet::titled("zebra", "")
        };
        let mut used = Snippet::titled("apple", "Deploy");
        used.uses.add(3);
        used.tags.insert("k8s".to_owned());
        let lib = library([starred, used, Snippet::titled("mango", "Git")]);
        let titles = |ids: Vec<Id>| -> Vec<String> {
            ids.iter()
                .map(|id| lib.get(id).unwrap().title.clone())
                .collect()
        };
        assert_eq!(
            titles(lib.query(&Selection::default())),
            ["zebra", "apple", "mango"]
        );
        assert_eq!(
            titles(lib.query(&Selection::Smart(SmartFolder::Favorites))),
            ["zebra"]
        );
        assert_eq!(
            titles(lib.query(&Selection::Smart(SmartFolder::MostUsed))),
            ["apple"]
        );
        assert_eq!(
            titles(lib.query(&Selection::Folder("Deploy".into()))),
            ["apple"]
        );
        assert_eq!(titles(lib.query(&Selection::Tag("k8s".into()))), ["apple"]);
        assert_eq!(lib.folders(), ["Deploy", "Git"]);
        assert_eq!(lib.tags(), [("k8s".to_owned(), 1)]);
    }
}
