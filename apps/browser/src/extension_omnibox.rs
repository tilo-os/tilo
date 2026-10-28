//! The address's keyword for extensions (`chrome.omnibox`). An extension's
//! manifest names a keyword; typed in the Browser's address and followed by
//! a space, what comes after it is the extension's, as in Chrome. The
//! extension hears of the person starting, of each change, of Enter and of
//! their leaving, and suggests rows for the list under the address.
//!
//! This is the bookkeeping alone: what to tell the engine as the typed text
//! changes, which suggestions still answer what is typed, and the rows they
//! make. Nothing here waits: the extension's answers arrive as engine events.

use std::collections::HashMap;

use ui::webview::{OmniboxInput, OmniboxSuggestion, SharedEngine, WebViewCommand, WebViewEvent};

/// The longest keyword taken from a manifest.
const MAX_KEYWORD_CHARS: usize = 64;
/// Requests count up to the largest the engine carries, then start again.
const MAX_REQUEST: u32 = 0x7fff_ffff;

/// An extension the address's keyword reaches.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Keyword {
    pub id: String,
    pub name: String,
    pub keyword: String,
}

/// The manifest's `omnibox.keyword`, when it is one a person can type: one
/// word, without spaces.
pub fn manifest_keyword(manifest: &serde_json::Value) -> Option<String> {
    let keyword = manifest.get("omnibox")?.get("keyword")?.as_str()?;
    (!keyword.is_empty()
        && keyword.chars().count() <= MAX_KEYWORD_CHARS
        && !keyword.chars().any(|c| c.is_whitespace() || c.is_control()))
    .then(|| keyword.to_owned())
}

/// `typed` addressed to the extension whose keyword it starts with, then a
/// space: the keyword's index in `keywords` and what follows, without the
/// spaces before it, as Chrome takes it. A keyword matches whatever its
/// case.
pub fn split<'a, 'k>(
    typed: &'a str,
    keywords: impl IntoIterator<Item = &'k str>,
) -> Option<(usize, &'a str)> {
    let typed = typed.trim_start();
    let (first, rest) = typed.split_once(char::is_whitespace)?;
    let index = keywords.into_iter().position(|keyword| {
        keyword
            .chars()
            .flat_map(char::to_lowercase)
            .eq(first.chars().flat_map(char::to_lowercase))
    })?;
    Some((index, rest.trim_start()))
}

/// A suggestion as the address lists it: what it says, plain, and what
/// choosing it enters.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OmniboxRow {
    pub title: String,
    pub content: String,
}

/// What an extension's description says without Chrome's markup: the text
/// of `<match>`, `<dim>` and `<url>`, entities read, on one line.
pub fn plain_description(description: &str) -> String {
    let mut text = String::with_capacity(description.len());
    let mut chars = description.chars();
    while let Some(c) = chars.next() {
        match c {
            // A tag: left out, with what it says inside its brackets.
            '<' => {
                for inside in chars.by_ref() {
                    if inside == '>' {
                        break;
                    }
                }
            }
            '&' => {
                // An entity's name, up to its `;`, read where it is one.
                let mut name = String::new();
                let mut lookahead = chars.clone();
                let mut closed = false;
                for next in lookahead.by_ref().take(10) {
                    if next == ';' {
                        closed = true;
                        break;
                    }
                    name.push(next);
                }
                match entity(&name).filter(|_| closed) {
                    Some(character) => {
                        text.push(character);
                        chars = lookahead;
                    }
                    None => text.push('&'),
                }
            }
            c => text.push(c),
        }
    }
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn entity(name: &str) -> Option<char> {
    Some(match name {
        "amp" => '&',
        "lt" => '<',
        "gt" => '>',
        "quot" => '"',
        "apos" => '\'',
        _ => {
            let number = name.strip_prefix('#')?;
            let code = match number.strip_prefix(['x', 'X']) {
                Some(hex) => u32::from_str_radix(hex, 16).ok()?,
                None => number.parse().ok()?,
            };
            char::from_u32(code).filter(|c| !c.is_control())?
        }
    })
}

struct Session {
    keyword: Keyword,
    /// What follows the keyword, as last told.
    text: String,
    /// The request the extension's suggestions must answer.
    request: u32,
    suggestions: Vec<OmniboxSuggestion>,
}

/// The extension the person is typing to, if any, and what it said.
#[derive(Default)]
pub struct ExtensionOmnibox {
    session: Option<Session>,
    request: u32,
    /// Each extension's default suggestion, as it last told it.
    defaults: HashMap<String, String>,
    /// What was typed when Enter went to an extension: until it changes, no
    /// new keyword session starts from it.
    entered: Option<String>,
}

impl ExtensionOmnibox {
    /// What is typed in the address now: `addressed` when it follows an
    /// extension's keyword (the extension and what follows), `typed` being
    /// all of it. Tells the extension what changed; returns whether the rows
    /// did.
    pub fn typed(
        &mut self,
        typed: &str,
        addressed: Option<(&Keyword, &str)>,
        engine: &SharedEngine,
    ) -> bool {
        if self
            .entered
            .as_deref()
            .is_some_and(|entered| entered != typed)
        {
            self.entered = None;
        }
        let Some((keyword, text)) = addressed.filter(|_| self.entered.is_none()) else {
            return self.leave(engine);
        };
        let mut changed = false;
        if self
            .session
            .as_ref()
            .is_some_and(|session| session.keyword.id != keyword.id)
        {
            changed |= self.leave(engine);
        }
        let session = self.session.get_or_insert_with(|| {
            engine.send(WebViewCommand::ExtensionOmnibox {
                id: keyword.id.clone(),
                input: OmniboxInput::Started,
            });
            changed = true;
            Session {
                keyword: keyword.clone(),
                text: String::new(),
                request: 0,
                suggestions: Vec::new(),
            }
        });
        if changed || session.text != text {
            // Within the range the engine carries.
            self.request = if self.request >= MAX_REQUEST {
                1
            } else {
                self.request + 1
            };
            session.request = self.request;
            text.clone_into(&mut session.text);
            engine.send(WebViewCommand::ExtensionOmnibox {
                id: keyword.id.clone(),
                input: OmniboxInput::Changed {
                    text: text.to_owned(),
                    request: self.request,
                },
            });
            changed = true;
        }
        changed
    }

    /// The person left the keyword without Enter: the extension hears of it.
    /// Returns whether there were rows.
    pub fn leave(&mut self, engine: &SharedEngine) -> bool {
        let Some(session) = self.session.take() else {
            return false;
        };
        engine.send(WebViewCommand::ExtensionOmnibox {
            id: session.keyword.id,
            input: OmniboxInput::Cancelled,
        });
        true
    }

    /// Enter went to an extension with `typed` in the address: its session
    /// ends there, without being left.
    pub fn entered(&mut self, typed: &str) {
        self.session = None;
        self.entered = Some(typed.to_owned());
    }

    /// What an extension said for the address. Returns whether the rows
    /// changed.
    pub fn on_engine_event(&mut self, event: &WebViewEvent) -> bool {
        match event {
            WebViewEvent::ExtensionOmniboxSuggestions {
                id,
                request,
                suggestions,
            } => {
                let Some(session) = self
                    .session
                    .as_mut()
                    .filter(|session| session.keyword.id == *id && session.request == *request)
                else {
                    return false;
                };
                session.suggestions.clone_from(suggestions);
                true
            }
            WebViewEvent::ExtensionOmniboxDefault { id, description } => {
                match description {
                    Some(description) => self.defaults.insert(id.clone(), description.clone()),
                    None => self.defaults.remove(id),
                };
                self.session
                    .as_ref()
                    .is_some_and(|session| session.keyword.id == *id)
            }
            _ => false,
        }
    }

    /// The extension typed to and its rows: first what Enter does (its
    /// default suggestion with `%s` for what is typed, else what is typed),
    /// then its suggestions for what is typed.
    pub fn rows(&self) -> Option<(&Keyword, Vec<OmniboxRow>)> {
        let session = self.session.as_ref()?;
        let text = &session.text;
        let title = match self.defaults.get(&session.keyword.id) {
            // As in Chrome: the description is read without its markup
            // first, then its first `%s` stands for what is typed, word for
            // word, so typed `<` or `&amp;` is not taken for markup.
            Some(description) => plain_description(description).replacen("%s", text, 1),
            None => text.clone(),
        };
        let mut rows = vec![OmniboxRow {
            title: if title.is_empty() {
                session.keyword.name.clone()
            } else {
                title
            },
            content: text.clone(),
        }];
        for suggestion in &session.suggestions {
            if rows.iter().any(|row| row.content == suggestion.content) {
                continue;
            }
            rows.push(OmniboxRow {
                title: plain_description(&suggestion.description),
                content: suggestion.content.clone(),
            });
        }
        Some((&session.keyword, rows))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ui::webview::test_engine::TestEngine;

    fn keyword(id: &str, keyword: &str) -> Keyword {
        Keyword {
            id: id.into(),
            name: format!("{keyword} finder"),
            keyword: keyword.into(),
        }
    }

    fn told(engine: &TestEngine) -> Vec<(String, OmniboxInput)> {
        engine
            .commands()
            .into_iter()
            .filter_map(|command| match command {
                WebViewCommand::ExtensionOmnibox { id, input } => Some((id, input)),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn a_keyword_and_a_space_address_what_follows_to_the_extension() {
        let keywords = ["kw", "Wiki"];
        assert_eq!(split("kw cats", keywords), Some((0, "cats")));
        assert_eq!(
            split("  KW   cats and dogs", keywords),
            Some((0, "cats and dogs"))
        );
        assert_eq!(split("wiki ", keywords), Some((1, "")));
        // The keyword alone, a longer word, or the keyword inside the text.
        for typed in ["kw", "kws cats", "cats kw", ""] {
            assert_eq!(split(typed, keywords), None, "{typed}");
        }
        let manifest = |omnibox: serde_json::Value| serde_json::json!({ "omnibox": omnibox });
        assert_eq!(
            manifest_keyword(&manifest(serde_json::json!({"keyword": "kw"}))),
            Some("kw".into())
        );
        for bad in [
            serde_json::json!({"keyword": ""}),
            serde_json::json!({"keyword": "two words"}),
            serde_json::json!({"keyword": 7}),
            serde_json::json!({"keyword": "k".repeat(65)}),
        ] {
            assert_eq!(manifest_keyword(&manifest(bad.clone())), None, "{bad}");
        }
    }

    #[test]
    fn the_extension_hears_each_change_once_and_of_the_person_leaving() {
        let engine = TestEngine::new();
        let shared: SharedEngine = engine.clone();
        let mut omnibox = ExtensionOmnibox::default();
        let (kw, other) = (
            keyword(&"a".repeat(32), "kw"),
            keyword(&"b".repeat(32), "wk"),
        );

        assert!(omnibox.typed("kw ", Some((&kw, "")), &shared));
        assert!(omnibox.typed("kw c", Some((&kw, "c")), &shared));
        // The same text again tells nothing.
        assert!(!omnibox.typed("kw c", Some((&kw, "c")), &shared));
        // Another extension's keyword: the first is left.
        assert!(omnibox.typed("wk d", Some((&other, "d")), &shared));
        assert!(omnibox.typed("w", None, &shared));
        assert!(!omnibox.typed("", None, &shared));
        assert_eq!(
            told(&engine),
            [
                (kw.id.clone(), OmniboxInput::Started),
                (
                    kw.id.clone(),
                    OmniboxInput::Changed {
                        text: String::new(),
                        request: 1
                    }
                ),
                (
                    kw.id.clone(),
                    OmniboxInput::Changed {
                        text: "c".into(),
                        request: 2
                    }
                ),
                (kw.id.clone(), OmniboxInput::Cancelled),
                (other.id.clone(), OmniboxInput::Started),
                (
                    other.id.clone(),
                    OmniboxInput::Changed {
                        text: "d".into(),
                        request: 3
                    }
                ),
                (other.id.clone(), OmniboxInput::Cancelled),
            ]
        );
    }

    #[test]
    fn rows_are_what_enter_does_then_the_suggestions_for_what_is_typed() {
        let engine = TestEngine::new();
        let shared: SharedEngine = engine.clone();
        let mut omnibox = ExtensionOmnibox::default();
        let kw = keyword(&"a".repeat(32), "kw");
        let suggestions = |request, contents: &[&str]| WebViewEvent::ExtensionOmniboxSuggestions {
            id: kw.id.clone(),
            request,
            suggestions: contents
                .iter()
                .map(|content| OmniboxSuggestion {
                    content: (*content).into(),
                    description: format!("<match>{content}</match> &amp; more"),
                })
                .collect(),
        };
        let rows = |omnibox: &ExtensionOmnibox| {
            omnibox.rows().map(|(_, rows)| {
                rows.into_iter()
                    .map(|row| (row.title, row.content))
                    .collect::<Vec<_>>()
            })
        };
        let pair = |title: &str, content: &str| (title.to_owned(), content.to_owned());
        assert_eq!(rows(&omnibox), None);
        omnibox.typed("kw ", Some((&kw, "")), &shared);
        // Nothing typed and no default: the extension's name.
        assert_eq!(rows(&omnibox), Some(vec![pair("kw finder", "")]));
        omnibox.typed("kw ca", Some((&kw, "ca")), &shared);
        // An answer to a request since overtaken is not shown.
        assert!(!omnibox.on_engine_event(&suggestions(1, &["old"])));
        assert!(omnibox.on_engine_event(&suggestions(2, &["cat", "ca"])));
        assert!(
            omnibox.on_engine_event(&WebViewEvent::ExtensionOmniboxDefault {
                id: kw.id.clone(),
                description: Some("Look up <match>%s</match>".into()),
            })
        );
        assert_eq!(
            rows(&omnibox),
            Some(vec![pair("Look up ca", "ca"), pair("cat & more", "cat"),])
        );
        // Another extension's answer changes nothing here.
        assert!(
            !omnibox.on_engine_event(&WebViewEvent::ExtensionOmniboxSuggestions {
                id: "b".repeat(32),
                request: 2,
                suggestions: Vec::new(),
            })
        );
        // Enter ends it without leaving it, and the same text starts no
        // session again; new typing does.
        omnibox.entered("kw ca");
        assert_eq!(rows(&omnibox), None);
        let before = told(&engine).len();
        assert!(!omnibox.typed("kw ca", Some((&kw, "ca")), &shared));
        assert_eq!(told(&engine).len(), before);
        assert!(omnibox.typed("kw cat", Some((&kw, "cat")), &shared));
        assert_eq!(
            told(&engine)[before],
            (kw.id.clone(), OmniboxInput::Started)
        );
    }

    #[test]
    fn what_is_typed_stands_in_the_default_suggestion_word_for_word() {
        let shared: SharedEngine = TestEngine::new();
        let mut omnibox = ExtensionOmnibox::default();
        let kw = keyword(&"a".repeat(32), "kw");
        let typed = "a<b &amp;  c";
        omnibox.typed(&format!("kw {typed}"), Some((&kw, typed)), &shared);
        assert!(
            omnibox.on_engine_event(&WebViewEvent::ExtensionOmniboxDefault {
                id: kw.id.clone(),
                description: Some("Look up <match>%s</match> not %s".into()),
            })
        );
        let (_, rows) = omnibox.rows().unwrap();
        assert_eq!(
            rows[0],
            OmniboxRow {
                title: "Look up a<b &amp;  c not %s".into(),
                content: typed.into(),
            }
        );
    }

    #[test]
    fn descriptions_are_read_without_their_markup() {
        assert_eq!(
            plain_description(
                "Search <match>tilo</match> on <url>example.org</url> <dim>(beta)</dim>"
            ),
            "Search tilo on example.org (beta)"
        );
        assert_eq!(
            plain_description("a &lt;b&gt; &amp;&#65;&#x42; &unknown; &"),
            "a <b> &AB &unknown; &"
        );
        assert_eq!(plain_description("two\nlines <broken"), "two lines");
    }
}
