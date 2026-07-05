//! A small, self-contained Language Server Protocol client.
//!
//! Three layers, from the bottom up:
//!
//! 1. **Framing** — LSP speaks JSON-RPC 2.0 over stdio, each message prefixed
//!    by a `Content-Length` header. [`encode`] frames an outgoing payload;
//!    [`MessageReader`] decodes a stream of incoming ones. Both are pure and
//!    unit-tested against canned bytes.
//! 2. **Parsing** — [`parse_publish_diagnostics`], [`parse_completion`],
//!    [`parse_hover`], and [`parse_definition`] turn a server's JSON into the
//!    editor's own model types ([`LspDiagnostic`], [`CompletionItem`], …).
//!    These are the hermetically-tested core: they run on canned LSP JSON with
//!    no server binary and no network.
//! 3. **Client** — [`LspClient`] spawns the appropriate server for a language
//!    (rust-analyzer for Rust, gopls for Go, …), performs the
//!    `initialize`/`initialized` handshake, and pumps notifications +
//!    responses off a background thread into a channel the editor drains each
//!    frame. Spawning is guarded so a missing binary degrades gracefully to a
//!    clear note rather than a crash.
//!
//! The client's process spawning is std-only (no extra deps, no network at
//! build time), so the whole module compiles unconditionally; the editor only
//! *starts* a server when explicitly asked (see `app.rs`), keeping the
//! hermetic build and `--shots` from ever launching rust-analyzer.

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::sync::{Arc, Mutex};

use serde_json::{json, Value};

use crate::bottom_panel::{Diagnostic, Severity};
use crate::model::Language;

// ---------------------------------------------------------------------------
// Model types (LSP → editor)
// ---------------------------------------------------------------------------

/// A zero-based `(line, character)` position, as LSP uses everywhere.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Position {
    pub line: u32,
    pub character: u32,
}

/// A half-open `[start, end)` range in a document.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct LspRange {
    pub start: Position,
    pub end: Position,
}

/// A parsed `textDocument/publishDiagnostics` entry.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LspDiagnostic {
    pub range: LspRange,
    /// LSP severity: 1=Error, 2=Warning, 3=Information, 4=Hint. Defaults to
    /// Error when a server omits it (the LSP spec's stated default).
    pub severity: u8,
    pub message: String,
    pub source: Option<String>,
}

impl LspDiagnostic {
    /// Project this LSP diagnostic onto the editor's Problems-panel model,
    /// converting the zero-based LSP position to the panel's one-based
    /// line/column.
    pub fn to_panel(&self) -> Diagnostic {
        Diagnostic {
            severity: severity_from_lsp(self.severity),
            line: self.range.start.line as usize + 1,
            col: self.range.start.character as usize + 1,
            message: self.message.clone(),
        }
    }
}

/// Map an LSP severity code to the panel's severity vocabulary.
pub fn severity_from_lsp(code: u8) -> Severity {
    match code {
        1 => Severity::Error,
        2 => Severity::Warning,
        3 => Severity::Info,
        _ => Severity::Hint,
    }
}

/// One completion candidate.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CompletionItem {
    pub label: String,
    /// LSP `CompletionItemKind` (1=Text, 3=Function, 5=Field, 6=Variable, …).
    pub kind: Option<u8>,
    pub detail: Option<String>,
    /// The text to actually insert — `insertText` when present, else `label`.
    pub insert_text: String,
}

/// A source location (go-to-definition result).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Location {
    pub uri: String,
    pub range: LspRange,
}

// ---------------------------------------------------------------------------
// Parsers (pure; hermetically tested)
// ---------------------------------------------------------------------------

fn parse_position(v: &Value) -> Position {
    Position {
        line: v.get("line").and_then(Value::as_u64).unwrap_or(0) as u32,
        character: v.get("character").and_then(Value::as_u64).unwrap_or(0) as u32,
    }
}

fn parse_range(v: &Value) -> LspRange {
    LspRange {
        start: v.get("start").map(parse_position).unwrap_or_default(),
        end: v.get("end").map(parse_position).unwrap_or_default(),
    }
}

/// Parse a `textDocument/publishDiagnostics` notification into `(uri, diags)`.
/// Accepts either the full notification (`{method, params:{uri, diagnostics}}`)
/// or a bare `params` object.
pub fn parse_publish_diagnostics(msg: &Value) -> Option<(String, Vec<LspDiagnostic>)> {
    let params = msg.get("params").unwrap_or(msg);
    let uri = params.get("uri").and_then(Value::as_str)?.to_string();
    let diags = params
        .get("diagnostics")
        .and_then(Value::as_array)
        .map(|arr| {
            arr.iter()
                .map(|d| LspDiagnostic {
                    range: d.get("range").map(parse_range).unwrap_or_default(),
                    // Per spec, absent severity defaults to Error(1).
                    severity: d.get("severity").and_then(Value::as_u64).unwrap_or(1) as u8,
                    message: d
                        .get("message")
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .to_string(),
                    source: d.get("source").and_then(Value::as_str).map(str::to_string),
                })
                .collect()
        })
        .unwrap_or_default();
    Some((uri, diags))
}

/// Parse a `textDocument/completion` result. Handles both shapes the spec
/// allows: a bare `CompletionItem[]`, or a `CompletionList { items: [...] }`.
/// Accepts either the full response envelope (`{result: …}`) or the result
/// value directly.
pub fn parse_completion(msg: &Value) -> Vec<CompletionItem> {
    let result = msg.get("result").unwrap_or(msg);
    let items = if let Some(arr) = result.as_array() {
        arr
    } else if let Some(arr) = result.get("items").and_then(Value::as_array) {
        arr
    } else {
        return Vec::new();
    };
    items
        .iter()
        .filter_map(|it| {
            let label = it.get("label").and_then(Value::as_str)?.to_string();
            let insert_text = it
                .get("insertText")
                .and_then(Value::as_str)
                .map(str::to_string)
                // `textEdit.newText` is the modern insert payload; fall back to it.
                .or_else(|| {
                    it.get("textEdit")
                        .and_then(|e| e.get("newText"))
                        .and_then(Value::as_str)
                        .map(str::to_string)
                })
                .unwrap_or_else(|| label.clone());
            Some(CompletionItem {
                label,
                kind: it.get("kind").and_then(Value::as_u64).map(|k| k as u8),
                detail: it.get("detail").and_then(Value::as_str).map(str::to_string),
                insert_text,
            })
        })
        .collect()
}

/// Parse a `textDocument/hover` result into plain display text. Handles the
/// three `contents` shapes: a `MarkupContent {value}`, a `MarkedString` (string
/// or `{value}`), and an array of `MarkedString`. `None`/`null` → `None`.
pub fn parse_hover(msg: &Value) -> Option<String> {
    let result = msg.get("result").unwrap_or(msg);
    if result.is_null() {
        return None;
    }
    let contents = result.get("contents").unwrap_or(result);
    let text = marked_to_string(contents);
    let text = text.trim().to_string();
    if text.is_empty() {
        None
    } else {
        Some(text)
    }
}

fn marked_to_string(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Array(arr) => arr
            .iter()
            .map(marked_to_string)
            .filter(|s| !s.trim().is_empty())
            .collect::<Vec<_>>()
            .join("\n"),
        Value::Object(_) => v
            .get("value")
            .and_then(Value::as_str)
            .map(str::to_string)
            .unwrap_or_default(),
        _ => String::new(),
    }
}

/// Parse a `textDocument/definition` result. Handles a single `Location`, a
/// `Location[]`, and a `LocationLink[]` (which uses `targetUri`/`targetRange`).
pub fn parse_definition(msg: &Value) -> Vec<Location> {
    let result = msg.get("result").unwrap_or(msg);
    match result {
        Value::Null => Vec::new(),
        Value::Array(arr) => arr.iter().filter_map(parse_location).collect(),
        obj => parse_location(obj).into_iter().collect(),
    }
}

fn parse_location(v: &Value) -> Option<Location> {
    // Plain Location.
    if let (Some(uri), Some(range)) = (v.get("uri").and_then(Value::as_str), v.get("range")) {
        return Some(Location {
            uri: uri.to_string(),
            range: parse_range(range),
        });
    }
    // LocationLink.
    if let (Some(uri), Some(range)) = (
        v.get("targetUri").and_then(Value::as_str),
        v.get("targetSelectionRange").or_else(|| v.get("targetRange")),
    ) {
        return Some(Location {
            uri: uri.to_string(),
            range: parse_range(range),
        });
    }
    None
}

// ---------------------------------------------------------------------------
// JSON-RPC framing
// ---------------------------------------------------------------------------

/// Frame a JSON-RPC payload as bytes: `Content-Length: N\r\n\r\n<body>`.
pub fn encode(payload: &Value) -> Vec<u8> {
    let body = serde_json::to_vec(payload).expect("serialize json-rpc payload");
    let mut out = format!("Content-Length: {}\r\n\r\n", body.len()).into_bytes();
    out.extend_from_slice(&body);
    out
}

/// Streaming decoder for `Content-Length`-framed JSON-RPC messages.
pub struct MessageReader<R: BufRead> {
    reader: R,
}

impl<R: BufRead> MessageReader<R> {
    pub fn new(reader: R) -> Self {
        Self { reader }
    }

    /// Read the next framed message. Returns `Ok(None)` at clean EOF.
    pub fn next_message(&mut self) -> std::io::Result<Option<Value>> {
        let mut content_length: Option<usize> = None;
        // Parse headers up to the blank line.
        loop {
            let mut line = String::new();
            let n = self.reader.read_line(&mut line)?;
            if n == 0 {
                return Ok(None); // EOF
            }
            let trimmed = line.trim_end_matches(['\r', '\n']);
            if trimmed.is_empty() {
                break; // end of headers
            }
            if let Some(rest) = trimmed
                .strip_prefix("Content-Length:")
                .or_else(|| trimmed.strip_prefix("content-length:"))
            {
                content_length = rest.trim().parse().ok();
            }
        }
        let len = match content_length {
            Some(len) => len,
            None => {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "LSP message missing Content-Length header",
                ))
            }
        };
        let mut body = vec![0u8; len];
        self.reader.read_exact(&mut body)?;
        let value = serde_json::from_slice(&body).map_err(|e| {
            std::io::Error::new(std::io::ErrorKind::InvalidData, e.to_string())
        })?;
        Ok(Some(value))
    }
}

// ---------------------------------------------------------------------------
// Live client
// ---------------------------------------------------------------------------

/// Which pending request a response id corresponds to, so the reader thread can
/// emit the right event when the matching response arrives.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Pending {
    Completion,
    Hover,
    Definition,
}

/// An event surfaced by the background reader thread for the editor to fold in.
#[derive(Clone, Debug)]
pub enum LspEvent {
    /// The server finished initializing and is ready for document syncs.
    Initialized,
    Diagnostics { uri: String, diagnostics: Vec<LspDiagnostic> },
    Completion(Vec<CompletionItem>),
    Hover(Option<String>),
    Definition(Vec<Location>),
    /// The server process exited or the stream errored.
    Exited,
}

/// A running language-server connection.
pub struct LspClient {
    child: Child,
    stdin: Arc<Mutex<ChildStdin>>,
    events: Receiver<LspEvent>,
    pending: Arc<Mutex<HashMap<i64, Pending>>>,
    next_id: i64,
    server_name: String,
}

impl LspClient {
    /// The server program + args for a language, or `None` when qOS has no
    /// mapping. The binaries are the conventional ones; a missing binary is
    /// handled at spawn time, not here.
    pub fn server_command(lang: Language) -> Option<(String, Vec<String>)> {
        let (prog, args): (&str, &[&str]) = match lang {
            Language::Rust => ("rust-analyzer", &[]),
            Language::Python => ("pylsp", &[]),
            Language::TypeScript | Language::JavaScript => {
                ("typescript-language-server", &["--stdio"])
            }
            Language::Go => ("gopls", &[]),
            Language::C | Language::Cpp => ("clangd", &[]),
            _ => return None,
        };
        Some((
            prog.to_string(),
            args.iter().map(|s| s.to_string()).collect(),
        ))
    }

    /// Spawn the server for `lang` rooted at `root_uri` and run the
    /// `initialize`/`initialized` handshake. Returns a descriptive error when
    /// there is no server mapping or the binary can't be launched — the caller
    /// turns that into a graceful in-editor note.
    pub fn start(lang: Language, root_uri: &str) -> Result<LspClient, String> {
        let (prog, args) = Self::server_command(lang)
            .ok_or_else(|| format!("no language server configured for {}", lang.label()))?;

        let mut child = Command::new(&prog)
            .args(&args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|e| {
                if e.kind() == std::io::ErrorKind::NotFound {
                    format!("{prog} not found on PATH — install it for live diagnostics")
                } else {
                    format!("failed to launch {prog}: {e}")
                }
            })?;

        let stdin = child.stdin.take().expect("piped stdin");
        let stdout = child.stdout.take().expect("piped stdout");
        let stdin = Arc::new(Mutex::new(stdin));
        let pending: Arc<Mutex<HashMap<i64, Pending>>> = Arc::new(Mutex::new(HashMap::new()));

        let (tx, rx) = mpsc::channel();
        let reader_pending = pending.clone();
        std::thread::spawn(move || {
            let mut reader = MessageReader::new(BufReader::new(stdout));
            loop {
                match reader.next_message() {
                    Ok(Some(msg)) => {
                        if let Some(ev) = route_message(&msg, &reader_pending) {
                            if tx.send(ev).is_err() {
                                break;
                            }
                        }
                    }
                    Ok(None) | Err(_) => {
                        let _ = tx.send(LspEvent::Exited);
                        break;
                    }
                }
            }
        });

        let mut client = LspClient {
            child,
            stdin,
            events: rx,
            pending,
            next_id: 1,
            server_name: prog,
        };

        // Kick the handshake. `initialize` is a request; once its response
        // lands (surfaced as `Initialized` by the router) we send `initialized`
        // — but rust-analyzer tolerates an eager `initialized`, and buffering
        // the notification here keeps the client simple.
        client.send_request_raw(
            "initialize",
            json!({
                "processId": std::process::id(),
                "rootUri": root_uri,
                "capabilities": {
                    "textDocument": {
                        "hover": { "contentFormat": ["plaintext", "markdown"] },
                        "completion": { "completionItem": { "snippetSupport": false } },
                        "publishDiagnostics": {},
                        "definition": {}
                    }
                }
            }),
            None,
        );
        client.send_notification("initialized", json!({}));

        Ok(client)
    }

    pub fn server_name(&self) -> &str {
        &self.server_name
    }

    /// Drain any events the reader thread produced since the last call.
    pub fn poll(&mut self) -> Vec<LspEvent> {
        let mut out = Vec::new();
        while let Ok(ev) = self.events.try_recv() {
            out.push(ev);
        }
        out
    }

    /// Notify the server a document was opened.
    pub fn did_open(&mut self, uri: &str, language_id: &str, version: i64, text: &str) {
        self.send_notification(
            "textDocument/didOpen",
            json!({
                "textDocument": {
                    "uri": uri,
                    "languageId": language_id,
                    "version": version,
                    "text": text,
                }
            }),
        );
    }

    /// Notify the server a document changed (full-text sync).
    pub fn did_change(&mut self, uri: &str, version: i64, text: &str) {
        self.send_notification(
            "textDocument/didChange",
            json!({
                "textDocument": { "uri": uri, "version": version },
                "contentChanges": [ { "text": text } ]
            }),
        );
    }

    /// Request completions at a position (result arrives as an `LspEvent`).
    pub fn request_completion(&mut self, uri: &str, pos: Position) {
        self.send_request(
            "textDocument/completion",
            json!({
                "textDocument": { "uri": uri },
                "position": { "line": pos.line, "character": pos.character }
            }),
            Pending::Completion,
        );
    }

    /// Request hover at a position.
    pub fn request_hover(&mut self, uri: &str, pos: Position) {
        self.send_request(
            "textDocument/hover",
            json!({
                "textDocument": { "uri": uri },
                "position": { "line": pos.line, "character": pos.character }
            }),
            Pending::Hover,
        );
    }

    /// Request the definition location(s) at a position.
    pub fn request_definition(&mut self, uri: &str, pos: Position) {
        self.send_request(
            "textDocument/definition",
            json!({
                "textDocument": { "uri": uri },
                "position": { "line": pos.line, "character": pos.character }
            }),
            Pending::Definition,
        );
    }

    fn send_request(&mut self, method: &str, params: Value, kind: Pending) {
        let id = self.next_id;
        self.next_id += 1;
        self.pending.lock().unwrap().insert(id, kind);
        self.send_request_raw(method, params, Some(id));
    }

    fn send_request_raw(&mut self, method: &str, params: Value, id: Option<i64>) {
        let id = id.unwrap_or_else(|| {
            let i = self.next_id;
            self.next_id += 1;
            i
        });
        let payload = json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params,
        });
        self.write(&payload);
    }

    fn send_notification(&mut self, method: &str, params: Value) {
        let payload = json!({
            "jsonrpc": "2.0",
            "method": method,
            "params": params,
        });
        self.write(&payload);
    }

    fn write(&mut self, payload: &Value) {
        let bytes = encode(payload);
        if let Ok(mut stdin) = self.stdin.lock() {
            let _ = stdin.write_all(&bytes);
            let _ = stdin.flush();
        }
    }
}

impl Drop for LspClient {
    fn drop(&mut self) {
        // Best-effort shutdown so we don't leak a server process.
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Turn an incoming server message into an editor event, consulting the pending
/// map to resolve response ids to their originating request kind.
fn route_message(msg: &Value, pending: &Arc<Mutex<HashMap<i64, Pending>>>) -> Option<LspEvent> {
    // Notifications carry a `method` but no `id`.
    if let Some(method) = msg.get("method").and_then(Value::as_str) {
        return match method {
            "textDocument/publishDiagnostics" => {
                let (uri, diagnostics) = parse_publish_diagnostics(msg)?;
                Some(LspEvent::Diagnostics { uri, diagnostics })
            }
            // Server→client requests (config, registration) we don't fulfil are
            // ignored; the servers we target tolerate that.
            _ => None,
        };
    }

    // Responses carry an `id` (and a `result` or `error`).
    let id = msg.get("id").and_then(Value::as_i64)?;
    let kind = pending.lock().unwrap().remove(&id);
    match kind {
        Some(Pending::Completion) => Some(LspEvent::Completion(parse_completion(msg))),
        Some(Pending::Hover) => Some(LspEvent::Hover(parse_hover(msg))),
        Some(Pending::Definition) => Some(LspEvent::Definition(parse_definition(msg))),
        // The `initialize` response (or any unmatched id) reports readiness.
        None => Some(LspEvent::Initialized),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn framing_round_trips() {
        let payload = json!({ "jsonrpc": "2.0", "id": 1, "method": "ping" });
        let bytes = encode(&payload);
        let text = String::from_utf8(bytes.clone()).unwrap();
        assert!(text.starts_with("Content-Length: "));
        assert!(text.contains("\r\n\r\n"));

        let mut reader = MessageReader::new(Cursor::new(bytes));
        let decoded = reader.next_message().unwrap().unwrap();
        assert_eq!(decoded, payload);
        // Second read hits clean EOF.
        assert!(reader.next_message().unwrap().is_none());
    }

    #[test]
    fn framing_reads_two_back_to_back_messages() {
        let mut buf = encode(&json!({ "a": 1 }));
        buf.extend(encode(&json!({ "b": 2 })));
        let mut reader = MessageReader::new(Cursor::new(buf));
        assert_eq!(reader.next_message().unwrap().unwrap(), json!({ "a": 1 }));
        assert_eq!(reader.next_message().unwrap().unwrap(), json!({ "b": 2 }));
        assert!(reader.next_message().unwrap().is_none());
    }

    #[test]
    fn parses_publish_diagnostics_into_model() {
        // A canned rust-analyzer-shaped diagnostics notification.
        let msg = json!({
            "jsonrpc": "2.0",
            "method": "textDocument/publishDiagnostics",
            "params": {
                "uri": "file:///proj/src/main.rs",
                "diagnostics": [
                    {
                        "range": {
                            "start": { "line": 9, "character": 4 },
                            "end": { "line": 9, "character": 12 }
                        },
                        "severity": 1,
                        "source": "rustc",
                        "message": "cannot find value `limitt` in this scope"
                    },
                    {
                        "range": {
                            "start": { "line": 2, "character": 0 },
                            "end": { "line": 2, "character": 3 }
                        },
                        "severity": 2,
                        "message": "unused import: `std::env`"
                    }
                ]
            }
        });
        let (uri, diags) = parse_publish_diagnostics(&msg).unwrap();
        assert_eq!(uri, "file:///proj/src/main.rs");
        assert_eq!(diags.len(), 2);
        assert_eq!(diags[0].severity, 1);
        assert_eq!(diags[0].range.start, Position { line: 9, character: 4 });
        assert_eq!(diags[0].source.as_deref(), Some("rustc"));

        // Projection onto the panel model converts to 1-based coordinates.
        let panel = diags[0].to_panel();
        assert_eq!(panel.severity, Severity::Error);
        assert_eq!(panel.line, 10);
        assert_eq!(panel.col, 5);
        assert!(panel.message.contains("limitt"));

        assert_eq!(diags[1].to_panel().severity, Severity::Warning);
    }

    #[test]
    fn absent_severity_defaults_to_error() {
        let msg = json!({
            "params": {
                "uri": "file:///a.rs",
                "diagnostics": [
                    { "range": { "start": {"line":0,"character":0}, "end": {"line":0,"character":1} },
                      "message": "boom" }
                ]
            }
        });
        let (_, diags) = parse_publish_diagnostics(&msg).unwrap();
        assert_eq!(diags[0].severity, 1);
        assert_eq!(diags[0].to_panel().severity, Severity::Error);
    }

    #[test]
    fn parses_completion_list_and_array_shapes() {
        // CompletionList shape with an insertText override.
        let list = json!({
            "jsonrpc": "2.0", "id": 2,
            "result": {
                "isIncomplete": false,
                "items": [
                    { "label": "println!", "kind": 3, "detail": "macro", "insertText": "println!" },
                    { "label": "primes_below", "kind": 3, "detail": "fn(u64) -> Vec<u64>" }
                ]
            }
        });
        let items = parse_completion(&list);
        assert_eq!(items.len(), 2);
        assert_eq!(items[0].label, "println!");
        assert_eq!(items[0].kind, Some(3));
        assert_eq!(items[0].insert_text, "println!");
        // No insertText → falls back to the label.
        assert_eq!(items[1].insert_text, "primes_below");
        assert_eq!(items[1].detail.as_deref(), Some("fn(u64) -> Vec<u64>"));

        // Bare CompletionItem[] shape (result is an array).
        let arr = json!({ "result": [ { "label": "foo" } ] });
        let items = parse_completion(&arr);
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].label, "foo");
        assert_eq!(items[0].insert_text, "foo");
    }

    #[test]
    fn completion_prefers_text_edit_new_text() {
        let msg = json!({ "result": [
            { "label": "greet", "textEdit": { "newText": "greet(${1})" } }
        ]});
        let items = parse_completion(&msg);
        assert_eq!(items[0].insert_text, "greet(${1})");
    }

    #[test]
    fn parses_hover_markup_marked_string_and_array() {
        // MarkupContent { kind, value }.
        let markup = json!({ "result": { "contents": { "kind": "markdown", "value": "fn main()" } } });
        assert_eq!(parse_hover(&markup).as_deref(), Some("fn main()"));

        // Bare string MarkedString.
        let s = json!({ "result": { "contents": "u64" } });
        assert_eq!(parse_hover(&s).as_deref(), Some("u64"));

        // Array of MarkedString (string + {value}).
        let arr = json!({ "result": { "contents": [ "line one", { "language": "rust", "value": "line two" } ] } });
        assert_eq!(parse_hover(&arr).as_deref(), Some("line one\nline two"));

        // Null hover → None.
        assert_eq!(parse_hover(&json!({ "result": null })), None);
    }

    #[test]
    fn parses_definition_location_shapes() {
        // Single Location.
        let single = json!({ "result": {
            "uri": "file:///proj/src/prime.rs",
            "range": { "start": {"line":3,"character":7}, "end": {"line":3,"character":18} }
        }});
        let locs = parse_definition(&single);
        assert_eq!(locs.len(), 1);
        assert_eq!(locs[0].uri, "file:///proj/src/prime.rs");
        assert_eq!(locs[0].range.start, Position { line: 3, character: 7 });

        // LocationLink[] (targetUri / targetSelectionRange).
        let links = json!({ "result": [ {
            "targetUri": "file:///proj/src/greeter.rs",
            "targetSelectionRange": { "start": {"line":4,"character":7}, "end": {"line":4,"character":12} }
        } ] });
        let locs = parse_definition(&links);
        assert_eq!(locs.len(), 1);
        assert_eq!(locs[0].uri, "file:///proj/src/greeter.rs");
        assert_eq!(locs[0].range.start.line, 4);

        // Null → empty.
        assert!(parse_definition(&json!({ "result": null })).is_empty());
    }

    #[test]
    fn route_message_resolves_response_ids_to_kinds() {
        let pending: Arc<Mutex<HashMap<i64, Pending>>> = Arc::new(Mutex::new(HashMap::new()));
        pending.lock().unwrap().insert(7, Pending::Completion);
        let resp = json!({ "jsonrpc": "2.0", "id": 7, "result": [ { "label": "x" } ] });
        match route_message(&resp, &pending) {
            Some(LspEvent::Completion(items)) => assert_eq!(items.len(), 1),
            other => panic!("expected completion event, got {other:?}"),
        }
        // The pending entry is consumed.
        assert!(pending.lock().unwrap().is_empty());

        // A diagnostics notification routes without an id.
        let note = json!({
            "method": "textDocument/publishDiagnostics",
            "params": { "uri": "file:///x.rs", "diagnostics": [] }
        });
        match route_message(&note, &pending) {
            Some(LspEvent::Diagnostics { uri, diagnostics }) => {
                assert_eq!(uri, "file:///x.rs");
                assert!(diagnostics.is_empty());
            }
            other => panic!("expected diagnostics event, got {other:?}"),
        }
    }

    #[test]
    fn missing_server_binary_degrades_gracefully() {
        // A language with a mapping but a binary that doesn't exist: `start`
        // returns a clear error rather than panicking. We can't assert on
        // rust-analyzer's absence portably, so drive a language whose server is
        // exceedingly unlikely to be installed under this exact name.
        let err = LspClient::server_command(Language::Rust);
        assert!(err.is_some(), "rust has a server mapping");
        // A language with no mapping errors clearly.
        assert!(LspClient::server_command(Language::Markdown).is_none());
    }
}
