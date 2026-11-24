//! The code editor's language servers, as held connections of kind `lsp`.
//!
//! A language server runs as long as the project is worked on, not as a job that ends: it is the
//! app's held connection ([`tilolang_tilo::held`]), in the app's own connection process, which
//! the device holds whether or not a window of the editor is open. The app opens one per
//! language and project folder, tells it what a file holds as a person writes, and asks it for
//! completions, hovers, definitions and formatting. Everything the server says comes back as the
//! connection's one word, pushed whenever it changes: its state and progress, the problems of
//! each file, and the answer to the app's last ask.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, PoisonError, Weak};

use serde::Deserialize;
use serde_json::{Value, json};
use tilolang_tilo::held::{Connection, Kind, Opened, Say};

use crate::lang::Language;
use crate::{diagnostics, expand, lsp, read_ask};

/// The kind's name, as a package names it (`connections: ["lsp"]`).
pub const KIND: &str = "lsp";

/// Language servers for a project's folder on this device.
pub struct LanguageServers;

#[derive(Deserialize)]
struct OpenAsk {
    language: String,
    root: String,
}

/// One message from the app: what a file holds now, or what it asks of the server.
#[derive(Deserialize)]
struct Message {
    op: String,
    path: String,
    #[serde(default)]
    language: String,
    #[serde(default)]
    text: String,
    #[serde(default)]
    version: i64,
    #[serde(default)]
    line: u32,
    #[serde(default)]
    column: u32,
    #[serde(default)]
    what: String,
}

impl Kind for LanguageServers {
    fn open(&self, _key: &str, ask: &str, opened: Opened) -> Result<Box<dyn Connection>, String> {
        let ask: OpenAsk = read_ask(ask)?;
        let language = Language::from_label(&ask.language)
            .ok_or_else(|| format!("`{}` is no language this device knows", ask.language))?;
        let session = Arc::new(Mutex::new(None::<Session>));
        let heard = {
            let session: Weak<Mutex<Option<Session>>> = Arc::downgrade(&session);
            Arc::new(move || {
                if let Some(session) = session.upgrade()
                    && let Some(session) = session
                        .lock()
                        .unwrap_or_else(PoisonError::into_inner)
                        .as_mut()
                {
                    session.heard();
                }
            }) as lsp::Heard
        };
        let client = lsp::LspClient::start(language, &expand(&ask.root), heard)
            .map_err(|failure| failure.to_string())?;
        let started = Session {
            server: client.server_name().to_owned(),
            client,
            say: opened.say,
            diagnostics: BTreeMap::new(),
            asked: None,
            answer: Value::Null,
            failure: String::new(),
            progress: None,
            stopped: false,
        };
        started.say();
        *session.lock().unwrap_or_else(PoisonError::into_inner) = Some(started);
        Ok(Box::new(Server(session)))
    }
}

/// One language server, as its connection holds it.
struct Server(Arc<Mutex<Option<Session>>>);

struct Session {
    server: String,
    client: lsp::LspClient,
    say: Say,
    diagnostics: BTreeMap<String, Vec<lsp::LspDiagnostic>>,
    /// What the app last asked the server, so an answer is handed on as the ask it belongs to.
    asked: Option<String>,
    /// The answer to the app's last ask, until it asks again.
    answer: Value,
    failure: String,
    progress: Option<lsp::Progress>,
    stopped: bool,
}

impl Connection for Server {
    fn send(&mut self, message: &str) -> Result<String, String> {
        let message: Message = read_ask(message)?;
        let mut session = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        let session = session
            .as_mut()
            .ok_or_else(|| "the language server is not running".to_owned())?;
        if session.stopped {
            return Err(format!("{} stopped", session.server));
        }
        let uri = lsp::path_to_uri(&expand(&message.path));
        match message.op.as_str() {
            "open" => {
                let language =
                    Language::from_label(&message.language).unwrap_or(Language::PlainText);
                session
                    .client
                    .did_open(&uri, language.server_id(), message.version, &message.text);
            }
            "change" => session
                .client
                .did_change(&uri, message.version, &message.text),
            "saved" => session.client.did_save(&uri),
            "ask" => {
                let position = lsp::Position {
                    line: message.line,
                    character: message.column,
                };
                match message.what.as_str() {
                    "complete" => session.client.request_completion(&uri, position),
                    "hover" => session.client.request_hover(&uri, position),
                    "definition" => session.client.request_definition(&uri, position),
                    "format" => {
                        session.client.request_formatting(&uri, 4, true);
                    }
                    other => return Err(format!("`{other}` is no ask a server answers")),
                }
                session.asked = Some(message.what);
                session.answer = Value::Null;
                session.say();
            }
            other => return Err(format!("`{other}` is nothing a language server is told")),
        }
        Ok("{}".to_owned())
    }

    fn close(&mut self) {
        // The client's end kills the server and waits for it.
        self.0.lock().unwrap_or_else(PoisonError::into_inner).take();
    }
}

impl Session {
    /// Take in what the server said since, and say the connection's word again.
    fn heard(&mut self) {
        for event in self.client.poll() {
            match event {
                lsp::LspEvent::Diagnostics { uri, diagnostics } => {
                    self.diagnostics.insert(uri, diagnostics);
                }
                lsp::LspEvent::Progress(progress) => {
                    self.progress = self.client.note_progress(&progress);
                }
                lsp::LspEvent::Initialized => self.progress = None,
                lsp::LspEvent::Completion(items) => self.answered(
                    "complete",
                    json!({
                        "items": items
                            .into_iter()
                            .map(|item| json!({
                                "label": item.label,
                                "detail": item.detail.unwrap_or_default(),
                                "insert": item.insert_text,
                            }))
                            .collect::<Vec<_>>(),
                    }),
                ),
                lsp::LspEvent::Hover(text) => {
                    self.answered("hover", json!({ "text": text.unwrap_or_default() }));
                }
                lsp::LspEvent::Definition(locations) => self.answered(
                    "definition",
                    json!({
                        "locations": locations
                            .into_iter()
                            .map(|location| json!({
                                "path": lsp::uri_to_path(&location.uri),
                                "line": location.range.start.line,
                                "column": location.range.start.character,
                            }))
                            .collect::<Vec<_>>(),
                    }),
                ),
                lsp::LspEvent::Formatted(edits) => self.answered(
                    "format",
                    match edits {
                        Ok(edits) => json!({
                            "edits": edits
                                .into_iter()
                                .map(|edit| json!({
                                    "start": edit.range.start.line,
                                    "start_column": edit.range.start.character,
                                    "end": edit.range.end.line,
                                    "end_column": edit.range.end.character,
                                    "text": edit.new_text,
                                }))
                                .collect::<Vec<_>>(),
                        }),
                        Err(why) => json!({ "error": why }),
                    },
                ),
                lsp::LspEvent::Exited => {
                    self.failure = match self.client.exit_detail() {
                        Some(detail) => format!("{} stopped ({detail})", self.server),
                        None => format!("{} stopped", self.server),
                    };
                    self.stopped = true;
                }
            }
        }
        self.say();
    }

    /// The answer to the ask `what`, when it is the one the app asked last.
    fn answered(&mut self, what: &str, mut answer: Value) {
        if self.asked.as_deref() == Some(what) {
            if let Some(answer) = answer.as_object_mut() {
                answer.insert("what".to_owned(), json!(what));
            }
            self.answer = answer;
            self.asked = None;
        }
    }

    /// Say the connection's word: everything the app shows of the server.
    fn say(&self) {
        let progress = self.progress.as_ref().map(|progress| {
            json!({
                "title": progress.title,
                "detail": progress.message.clone().unwrap_or_default(),
                "percent": progress.percentage,
            })
        });
        let state = if self.stopped {
            "stopped"
        } else if progress.is_some() {
            "busy"
        } else {
            "ready"
        };
        let found: serde_json::Map<String, Value> = self
            .diagnostics
            .iter()
            .map(|(uri, found)| (lsp::uri_to_path(uri), json!(diagnostics(found))))
            .collect();
        let answer = if self.answer.is_null() {
            String::new()
        } else {
            self.answer.to_string()
        };
        (self.say)(
            &json!({
                "state": state,
                "server": self.server,
                "failure": self.failure,
                "progress": progress,
                "diagnostics": found,
                "answer": answer,
            })
            .to_string(),
        );
    }
}
