//! The address's keyword for extensions (`chrome.omnibox`): a manifest's keyword, typed in the
//! address and followed by a space, makes what follows the extension's, as in Chrome. The
//! extension hears of the person starting, each change, Enter and their leaving, and suggests
//! rows for the list under the address.
//!
//! This is the bookkeeping alone, kept beside the engine that carries it; the extension's
//! answers arrive as the engine's events.

use std::collections::HashMap;

use tilo_webview::{OmniboxInput, OmniboxSuggestion, SharedEngine, WebViewCommand, WebViewEvent};

/// Requests count up to the largest the engine carries, then start again.
const MAX_REQUEST: u32 = 0x7fff_ffff;

/// What the address is typed to: the extension whose keyword it starts with, what the extension
/// is called, and what follows the keyword.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Addressed {
    pub id: String,
    pub name: String,
    pub text: String,
}

/// One row the address offers: what it says, and what choosing it sends the extension.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Row {
    pub title: String,
    pub content: String,
}

/// The rows the address offers for the extension it addresses now: the extension it is, what
/// follows its keyword, and what Enter and the extension's suggestions do.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct State {
    pub id: String,
    pub name: String,
    pub text: String,
    pub rows: Vec<Row>,
}

struct Session {
    id: String,
    name: String,
    text: String,
    request: u32,
    suggestions: Vec<OmniboxSuggestion>,
}

/// What the person is typing after an extension's keyword, and what the extension says of it.
#[derive(Default)]
pub struct Omnibox {
    session: Option<Session>,
    request: u32,
    /// The suggestion each extension says Enter does, in its own markup.
    defaults: HashMap<String, String>,
    /// What was typed when Enter went to an extension: until it changes, no new session starts
    /// from it.
    entered: Option<String>,
}

impl Omnibox {
    /// What is typed in the address now: `addressed` when it follows an extension's keyword (the
    /// extension and what follows), `typed` being all of it. Tells the extension what changed;
    /// whether the rows did.
    pub fn typed(
        &mut self,
        typed: &str,
        addressed: Option<&Addressed>,
        engine: &SharedEngine,
    ) -> bool {
        if self
            .entered
            .as_deref()
            .is_some_and(|entered| entered != typed)
        {
            self.entered = None;
        }
        let Some(addressed) = addressed.filter(|_| self.entered.is_none()) else {
            return self.leave(engine);
        };
        let mut changed = false;
        if self
            .session
            .as_ref()
            .is_some_and(|session| session.id != addressed.id)
        {
            changed |= self.leave(engine);
        }
        let session = self.session.get_or_insert_with(|| {
            engine.send(WebViewCommand::ExtensionOmnibox {
                id: addressed.id.clone(),
                input: OmniboxInput::Started,
            });
            changed = true;
            Session {
                id: addressed.id.clone(),
                name: addressed.name.clone(),
                text: String::new(),
                request: 0,
                suggestions: Vec::new(),
            }
        });
        session.name.clone_from(&addressed.name);
        if changed || session.text != addressed.text {
            self.request = if self.request >= MAX_REQUEST {
                1
            } else {
                self.request.saturating_add(1)
            };
            session.request = self.request;
            session.text.clone_from(&addressed.text);
            engine.send(WebViewCommand::ExtensionOmnibox {
                id: addressed.id.clone(),
                input: OmniboxInput::Changed {
                    text: addressed.text.clone(),
                    request: self.request,
                },
            });
            changed = true;
        }
        changed
    }

    /// The person left the keyword without Enter: the extension hears of it. Whether there were
    /// rows.
    pub fn leave(&mut self, engine: &SharedEngine) -> bool {
        let Some(session) = self.session.take() else {
            return false;
        };
        engine.send(WebViewCommand::ExtensionOmnibox {
            id: session.id,
            input: OmniboxInput::Cancelled,
        });
        true
    }

    /// The extension the address addresses now, whose page the extension hears of Enter through.
    #[must_use]
    pub fn session_id(&self) -> Option<String> {
        self.session.as_ref().map(|session| session.id.clone())
    }

    /// The person pressed Enter, or chose a row: no session starts from what is typed until it
    /// changes.
    pub fn entered(&mut self, text: &str) {
        self.session = None;
        self.entered = Some(text.to_owned());
    }

    /// What an extension said for the address. Whether the rows changed.
    pub fn apply(&mut self, event: &WebViewEvent) -> bool {
        match event {
            WebViewEvent::ExtensionOmniboxSuggestions {
                id,
                request,
                suggestions,
            } => {
                let Some(session) = self
                    .session
                    .as_mut()
                    .filter(|session| session.id == *id && session.request == *request)
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
                    .is_some_and(|session| session.id == *id)
            }
            _ => false,
        }
    }

    /// What the address offers now, when it addresses an extension: the extension it is, what
    /// follows its keyword, and its rows — first what Enter does (its default suggestion with
    /// `%s` for what is typed, else what is typed), then its suggestions.
    #[must_use]
    pub fn state(&self) -> Option<State> {
        let session = self.session.as_ref()?;
        let text = &session.text;
        let title = match self.defaults.get(&session.id) {
            // As in Chrome: the description is read without its markup first, then its first
            // `%s` stands for what is typed, so typed `<` is not taken for markup.
            Some(description) => plain_description(description).replacen("%s", text, 1),
            None => text.clone(),
        };
        let mut rows = vec![Row {
            title: if title.is_empty() {
                session.name.clone()
            } else {
                title
            },
            content: text.clone(),
        }];
        for suggestion in &session.suggestions {
            if rows.iter().any(|row| row.content == suggestion.content) {
                continue;
            }
            rows.push(Row {
                title: plain_description(&suggestion.description),
                content: suggestion.content.clone(),
            });
        }
        Some(State {
            id: session.id.clone(),
            name: session.name.clone(),
            text: text.clone(),
            rows,
        })
    }
}

/// What an extension's description says without Chrome's markup: the text of `<match>`, `<dim>`
/// and `<url>`, entities read, on one line.
#[must_use]
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

#[cfg(test)]
mod tests {
    use super::*;
    use tilo_webview::test_engine::TestEngine;

    fn addressed(id: &str, name: &str, text: &str) -> Addressed {
        Addressed {
            id: id.into(),
            name: name.into(),
            text: text.into(),
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

    /// The extension hears each change once, and of the person leaving it.
    #[test]
    fn a_keyword_takes_what_follows_it_and_the_extension_hears_each_change_once() {
        let engine = TestEngine::new();
        let shared: SharedEngine = engine.clone();
        let mut omnibox = Omnibox::default();
        let (kw, other) = (
            addressed(&"a".repeat(32), "kw finder", ""),
            addressed(&"b".repeat(32), "wk finder", "d"),
        );

        assert!(omnibox.typed("kw ", Some(&kw), &shared));
        assert!(omnibox.typed("kw c", Some(&addressed(&kw.id, "kw finder", "c")), &shared));
        assert!(!omnibox.typed("kw c", Some(&addressed(&kw.id, "kw finder", "c")), &shared));
        assert!(omnibox.typed("wk d", Some(&other), &shared));
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

    /// The rows are what Enter does, then the extension's suggestions for what is typed; a
    /// suggestion for another request is not the address's.
    #[test]
    fn rows_are_what_enter_does_then_the_suggestions_for_what_is_typed() {
        let engine = TestEngine::new();
        let shared: SharedEngine = engine.clone();
        let mut omnibox = Omnibox::default();
        let kw = addressed(&"a".repeat(32), "kw finder", "");
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
        let rows = |omnibox: &Omnibox| {
            omnibox.state().map(|state| {
                state
                    .rows
                    .into_iter()
                    .map(|row| (row.title, row.content))
                    .collect::<Vec<_>>()
            })
        };
        let pair = |title: &str, content: &str| (title.to_owned(), content.to_owned());
        assert_eq!(rows(&omnibox), None);
        omnibox.typed("kw ", Some(&kw), &shared);
        assert_eq!(rows(&omnibox), Some(vec![pair("kw finder", "")]));
        omnibox.typed("kw ca", Some(&addressed(&kw.id, "kw finder", "ca")), &shared);
        assert!(!omnibox.apply(&suggestions(1, &["old"])));
        assert!(omnibox.apply(&suggestions(2, &["cat", "ca"])));
        assert!(omnibox.apply(&WebViewEvent::ExtensionOmniboxDefault {
            id: kw.id.clone(),
            description: Some("Look up <match>%s</match>".into()),
        }));
        assert_eq!(
            rows(&omnibox),
            Some(vec![pair("Look up ca", "ca"), pair("cat & more", "cat")])
        );
        assert!(!omnibox.apply(&WebViewEvent::ExtensionOmniboxSuggestions {
            id: "b".repeat(32),
            request: 2,
            suggestions: Vec::new(),
        }));
        omnibox.entered("kw ca");
        assert_eq!(rows(&omnibox), None);
        let before = told(&engine).len();
        assert!(!omnibox.typed("kw ca", Some(&addressed(&kw.id, "kw finder", "ca")), &shared));
        assert_eq!(told(&engine).len(), before);
        assert!(omnibox.typed("kw cat", Some(&addressed(&kw.id, "kw finder", "cat")), &shared));
        assert_eq!(
            told(&engine)[before],
            (kw.id.clone(), OmniboxInput::Started)
        );
        assert_eq!(
            told(&engine).last(),
            Some(&(
                kw.id.clone(),
                OmniboxInput::Changed {
                    text: "cat".into(),
                    request: 3
                }
            ))
        );
    }

    /// What is typed stands in the default suggestion word for word.
    #[test]
    fn what_is_typed_stands_in_the_default_suggestion_word_for_word() {
        let shared: SharedEngine = TestEngine::new();
        let mut omnibox = Omnibox::default();
        let kw = addressed(&"a".repeat(32), "kw finder", "");
        let typed = "a<b &amp;  c";
        let kw = addressed(&kw.id, "kw finder", typed);
        omnibox.typed(&format!("kw {typed}"), Some(&kw), &shared);
        assert!(omnibox.apply(&WebViewEvent::ExtensionOmniboxDefault {
            id: kw.id,
            description: Some("Look up <match>%s</match> not %s".into()),
        }));
        let state = omnibox.state().unwrap();
        assert_eq!(
            state.rows[0],
            Row {
                title: "Look up a<b &amp;  c not %s".into(),
                content: typed.into(),
            }
        );
    }

    /// A description is read without its markup.
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
