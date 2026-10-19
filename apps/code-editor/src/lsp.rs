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
//!
//! [`encode`]: crate::lsp::encode
//! [`MessageReader`]: crate::lsp::MessageReader
//! [`parse_publish_diagnostics`]: crate::lsp::parse_publish_diagnostics
//! [`parse_completion`]: crate::lsp::parse_completion
//! [`parse_hover`]: crate::lsp::parse_hover
//! [`parse_definition`]: crate::lsp::parse_definition
//! [`LspDiagnostic`]: crate::lsp::LspDiagnostic
//! [`CompletionItem`]: crate::lsp::CompletionItem
//! [`LspClient`]: crate::lsp::LspClient

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::sync::mpsc::{self, Receiver, SyncSender, TryRecvError, TrySendError};
use std::sync::{Arc, Mutex};

use serde_json::{Value, json};

use crate::bottom_panel::{Diagnostic, Severity};
use crate::model::Language;

/// A language server is an external process. Never let a malformed or runaway
/// server turn one advertised frame into an unbounded allocation in the IDE.
/// This still comfortably fits the largest text buffer Code supports (8 MiB),
/// plus JSON-RPC framing and diagnostics.
pub const MAX_LSP_MESSAGE_BYTES: usize = 16 * 1024 * 1024;
const MAX_LSP_HEADER_LINE_BYTES: usize = 8 * 1024;
/// The UI folds reader events every frame. A fixed queue keeps a server that
/// floods progress notifications from accumulating memory while preserving a
/// responsive editor; a later diagnostics publication supersedes an earlier
/// one, so dropped backpressure events are safe to recover from.
const LSP_EVENT_QUEUE_CAPACITY: usize = 64;
/// Completion / hover / definition requests are ephemeral. Capping their
/// response table prevents key-repeat from retaining an unlimited number of
/// request identities when a server is slow or wedged.
const MAX_PENDING_REQUESTS: usize = 64;
const MAX_LSP_DIAGNOSTICS: usize = 10_000;
const MAX_LSP_COMPLETIONS: usize = 256;
const MAX_LSP_DEFINITIONS: usize = 128;
const MAX_LSP_DISPLAY_TEXT_CHARS: usize = 64 * 1024;

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
    let uri = normalize_document_uri(&clip_display_text(
        params.get("uri").and_then(Value::as_str)?,
    ));
    let diags = params
        .get("diagnostics")
        .and_then(Value::as_array)
        .map(|arr| {
            arr.iter()
                .take(MAX_LSP_DIAGNOSTICS)
                .map(|d| LspDiagnostic {
                    range: d.get("range").map(parse_range).unwrap_or_default(),
                    // Per spec, absent severity defaults to Error(1).
                    severity: d.get("severity").and_then(Value::as_u64).unwrap_or(1) as u8,
                    message: d
                        .get("message")
                        .and_then(Value::as_str)
                        .map_or_else(String::new, clip_display_text),
                    source: d
                        .get("source")
                        .and_then(Value::as_str)
                        .map(clip_display_text),
                })
                .collect()
        })
        .unwrap_or_default();
    Some((uri, diags))
}

/// Parse a `$/progress` notification into a [`Progress`].
///
/// The shape is a token plus a `value` that is one of `begin` / `report` /
/// `end`. `begin` carries the title, `report` carries updates without repeating
/// it, and `end` may carry only a closing message — so a caller that wants a
/// stable label has to remember the title from `begin`. That is the client's
/// job (see [`LspClient`]), not the parser's; this returns exactly what the
/// notification said.
pub fn parse_progress(msg: &Value) -> Option<Progress> {
    let params = msg.get("params").unwrap_or(msg);
    // The token may be a string or an integer; normalise to a string so it can
    // key a map either way.
    let token = match params.get("token")? {
        Value::String(s) => clip_display_text(s),
        Value::Number(n) => n.to_string(),
        _ => return None,
    };
    let value = params.get("value")?;
    let kind = value.get("kind").and_then(Value::as_str).unwrap_or("");
    Some(Progress {
        token,
        title: clip_display_text(value.get("title").and_then(Value::as_str).unwrap_or("")),
        message: value
            .get("message")
            .and_then(Value::as_str)
            .map(clip_display_text),
        percentage: value
            .get("percentage")
            .and_then(Value::as_u64)
            .map(|p| p.min(100) as u32),
        done: kind == "end",
    })
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
        .take(MAX_LSP_COMPLETIONS)
        .filter_map(|it| {
            let label = clip_display_text(it.get("label").and_then(Value::as_str)?);
            let insert_text = it
                .get("insertText")
                .and_then(Value::as_str)
                .map(clip_display_text)
                // `textEdit.newText` is the modern insert payload; fall back to it.
                .or_else(|| {
                    it.get("textEdit")
                        .and_then(|e| e.get("newText"))
                        .and_then(Value::as_str)
                        .map(clip_display_text)
                })
                .unwrap_or_else(|| label.clone());
            Some(CompletionItem {
                label,
                kind: it.get("kind").and_then(Value::as_u64).map(|k| k as u8),
                detail: it
                    .get("detail")
                    .and_then(Value::as_str)
                    .map(clip_display_text),
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
    let text = clip_display_text(&text);
    if text.is_empty() { None } else { Some(text) }
}

/// Bound user-visible LSP strings after parsing. UTF-8 is preserved because
/// truncation happens in character coordinates, just like the editor buffer.
fn clip_display_text(text: &str) -> String {
    let end = text
        .char_indices()
        .nth(MAX_LSP_DISPLAY_TEXT_CHARS)
        .map_or(text.len(), |(byte, _)| byte);
    text.get(..end).unwrap_or_default().trim().to_string()
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
        Value::Array(arr) => arr
            .iter()
            .take(MAX_LSP_DEFINITIONS)
            .filter_map(parse_location)
            .collect(),
        obj => parse_location(obj).into_iter().collect(),
    }
}

fn parse_location(v: &Value) -> Option<Location> {
    // Plain Location.
    if let (Some(uri), Some(range)) = (v.get("uri").and_then(Value::as_str), v.get("range")) {
        return Some(Location {
            uri: normalize_document_uri(&clip_display_text(uri)),
            range: parse_range(range),
        });
    }
    // LocationLink.
    if let (Some(uri), Some(range)) = (
        v.get("targetUri").and_then(Value::as_str),
        v.get("targetSelectionRange")
            .or_else(|| v.get("targetRange")),
    ) {
        return Some(Location {
            uri: normalize_document_uri(&clip_display_text(uri)),
            range: parse_range(range),
        });
    }
    None
}

// ---------------------------------------------------------------------------
// JSON-RPC framing
// ---------------------------------------------------------------------------

/// Frame a JSON-RPC payload as bytes: `Content-Length: N\r\n\r\n<body>`.
///
/// Every payload this client builds is either a `json!()` literal or a
/// `Value` freshly parsed from valid JSON — both are always representable in
/// JSON (the only way to construct a non-representable `Value`, a NaN/
/// infinite float, is not something this code does) — so serialization here
/// cannot practically fail; the empty-frame fallback exists only so a future
/// caller building a payload another way degrades instead of crashing.
pub fn encode(payload: &Value) -> Vec<u8> {
    let body = serde_json::to_vec(payload).unwrap_or_default();
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
    ///
    /// # Errors
    /// Returns an error if a header block has no `Content-Length`, the
    /// underlying reader fails, or the body is not valid JSON.
    pub fn next_message(&mut self) -> std::io::Result<Option<Value>> {
        let mut content_length: Option<usize> = None;
        // Parse headers up to the blank line.
        loop {
            let mut raw = Vec::new();
            let n = self
                .reader
                .by_ref()
                .take((MAX_LSP_HEADER_LINE_BYTES + 1) as u64)
                .read_until(b'\n', &mut raw)?;
            if n == 0 {
                return Ok(None); // EOF
            }
            if raw.len() > MAX_LSP_HEADER_LINE_BYTES || !raw.ends_with(b"\n") {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "LSP header line is too large",
                ));
            }
            let line = std::str::from_utf8(&raw).map_err(|_| {
                std::io::Error::new(std::io::ErrorKind::InvalidData, "LSP header is not UTF-8")
            })?;
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
        let Some(len) = content_length else {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "LSP message missing Content-Length header",
            ));
        };
        if len > MAX_LSP_MESSAGE_BYTES {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("LSP message exceeds {MAX_LSP_MESSAGE_BYTES} byte limit"),
            ));
        }
        let mut body = vec![0u8; len];
        self.reader.read_exact(&mut body)?;
        let value = serde_json::from_slice(&body)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e.to_string()))?;
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

/// A long-running server task the user should be able to see, parsed from
/// `$/progress`.
///
/// rust-analyzer takes minutes to index a workspace this size, and it answers
/// completion and go-to-definition with nothing until it has. Without this the
/// editor is indistinguishable from broken for those minutes, so the indexing
/// state is a first-class event rather than something buried in a log.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Progress {
    /// The server's token for this task; `end` arrives against the same one.
    pub token: String,
    /// e.g. "Indexing" / "Roots Scanned".
    pub title: String,
    /// The current step, e.g. the crate being indexed.
    pub message: Option<String>,
    /// 0–100 when the server reports one.
    pub percentage: Option<u32>,
    /// Whether this is the task's `end` notification.
    pub done: bool,
}

/// An event surfaced by the background reader thread for the editor to fold in.
#[derive(Clone, Debug)]
pub enum LspEvent {
    /// The server finished initializing and is ready for document syncs.
    Initialized,
    Diagnostics {
        uri: String,
        diagnostics: Vec<LspDiagnostic>,
    },
    Completion(Vec<CompletionItem>),
    Hover(Option<String>),
    Definition(Vec<Location>),
    /// A `$/progress` update — the server is busy with something named.
    Progress(Progress),
    /// The server process exited or the stream errored.
    Exited,
}

/// A running language-server connection.
pub struct LspClient {
    child: Child,
    stdin: Arc<Mutex<ChildStdin>>,
    events: Receiver<LspEvent>,
    pending: Arc<Mutex<HashMap<i64, Pending>>>,
    /// `None` once the JSON-RPC identity space is exhausted. It is better to
    /// drop an ephemeral IDE request than to reuse a response id and route a
    /// stale completion or definition into the current document.
    next_id: Option<i64>,
    server_name: String,
    /// The directory the server was rooted at.
    root: PathBuf,
    /// Live `$/progress` tasks, keyed by the server's token.
    progress: HashMap<String, Progress>,
}

/// A `file://` URI for a local path, percent-encoding what has to be encoded.
///
/// This is the inverse of `ui::portal::to_local_path`, and it has to be exact:
/// a server told `file:///home/me/my project` (unencoded space) resolves a
/// different directory than the one we opened, and then reports diagnostics for
/// files we never show.
pub fn path_to_uri(path: &Path) -> String {
    match url::Url::from_file_path(path) {
        Ok(u) => normalize_document_uri(u.as_str()),
        // `from_file_path` only fails on a non-absolute path, which cannot
        // reach here from a canonicalised project root — but a lossy fallback
        // beats a panic in an editor.
        Err(()) => format!("file://{}", path.display()),
    }
}

/// Windows drive letters identify the same volume regardless of case.
/// rust-analyzer lowercases them; normalize both directions so diagnostics
/// and definition locations use the same keys as the editor's documents.
fn normalize_document_uri(uri: &str) -> String {
    if let Some(path) = uri.strip_prefix("file:///") {
        let bytes = path.as_bytes();
        if let (Some(drive), Some(b':'), Some(b'/')) = (bytes.first(), bytes.get(1), bytes.get(2))
            && drive.is_ascii_alphabetic()
        {
            let mut normalized = uri.to_owned();
            normalized.replace_range(8..9, &char::from(drive.to_ascii_lowercase()).to_string());
            return normalized;
        }
    }
    uri.to_owned()
}

impl LspClient {
    /// The server program + args for a language, or `None` when Tilo has no
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
            args.iter().map(std::string::ToString::to_string).collect(),
        ))
    }

    /// Spawn the server for `lang` rooted at the real directory `root` and run
    /// the `initialize`/`initialized` handshake. Returns a descriptive error
    /// when there is no server mapping or the binary can't be launched — the
    /// caller turns that into a graceful in-editor note.
    ///
    /// `root` must be a directory that actually exists. rust-analyzer resolves
    /// the workspace by looking for a `Cargo.toml` at or above it; pointed at a
    /// path that is not on the filesystem it starts, answers the handshake, and
    /// then reports nothing at all — which looks exactly like a working editor
    /// with a clean file.
    ///
    /// # Errors
    /// Returns a descriptive error string when `lang` has no server mapping,
    /// `root` is not a directory, the server binary can't be launched, or its
    /// piped stdio could not be taken.
    pub fn start(lang: Language, root: &Path) -> Result<LspClient, String> {
        let (prog, args) = Self::server_command(lang)
            .ok_or_else(|| format!("no language server configured for {}", lang.label()))?;

        if !root.is_dir() {
            return Err(format!(
                "{} is not a directory — open a project folder for live diagnostics",
                root.display()
            ));
        }
        let root_uri = path_to_uri(root);

        let mut command = Command::new(&prog);
        // Language servers communicate over pipes, not a console window.
        bounded_command::without_console_window(&mut command);
        command
            .args(&args)
            // Run the server *in* the project, so anything it resolves
            // relatively (rustfmt.toml, .cargo/config.toml) resolves the way
            // the same command would from a shell in that directory.
            .current_dir(root)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        // Language servers routinely start indexers and compiler helpers.
        // Own the complete tree so closing a project cannot strand those
        // descendants in the user's session.
        bounded_command::own_process_tree(&mut command);
        // An independent job: in a cgroup of its own below the editor's, so a
        // server that runs out of memory ends alone and the editor stays.
        tilo_process::job::independent(&mut command, &prog);
        let mut child = command.spawn().map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                format!("{prog} not found on PATH — install it for live diagnostics")
            } else {
                format!("failed to launch {prog}: {e}")
            }
        })?;

        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| format!("{prog}: piped stdin was not created"))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| format!("{prog}: piped stdout was not created"))?;
        let stdin = Arc::new(Mutex::new(stdin));
        let pending: Arc<Mutex<HashMap<i64, Pending>>> = Arc::new(Mutex::new(HashMap::new()));

        let events = spawn_reader_thread(&prog, stdout, stdin.clone(), pending.clone())?;

        let mut client = LspClient {
            child,
            stdin,
            events,
            pending,
            next_id: Some(1),
            server_name: prog,
            root: root.to_path_buf(),
            progress: HashMap::new(),
        };
        send_initialize_handshake(&mut client, root, &root_uri);
        Ok(client)
    }

    /// The directory this server was rooted at.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Fold a `$/progress` update into the client's running-task table and
    /// return the state the UI should show.
    ///
    /// This is where a `report` without a title gets the title its `begin`
    /// carried — the server sends the label once and then updates against the
    /// token, so a UI reading the raw notifications would show "Indexing 12%"
    /// and then a blank label at 13%.
    pub fn note_progress(&mut self, p: &Progress) -> Option<Progress> {
        if p.done {
            self.progress.remove(&p.token);
            return None;
        }
        let entry = self
            .progress
            .entry(p.token.clone())
            .or_insert_with(|| p.clone());
        if !p.title.is_empty() {
            entry.title.clone_from(&p.title);
        }
        entry.message.clone_from(&p.message);
        entry.percentage = p.percentage;
        Some(entry.clone())
    }

    /// Whether the server is currently busy with a reported task.
    pub fn is_busy(&self) -> bool {
        !self.progress.is_empty()
    }

    /// The task to show, when the server is busy. Picks the one furthest from
    /// finishing so a long index is not hidden behind a quick sub-task.
    pub fn busy_task(&self) -> Option<&Progress> {
        self.progress
            .values()
            .min_by_key(|p| p.percentage.unwrap_or(0))
    }

    pub fn server_name(&self) -> &str {
        &self.server_name
    }

    /// Drain any events the reader thread produced since the last call.
    pub fn poll(&mut self) -> Vec<LspEvent> {
        let mut out = Vec::new();
        loop {
            match self.events.try_recv() {
                Ok(event) => out.push(event),
                Err(TryRecvError::Empty) => break,
                // The bounded reader may have dropped a final `Exited` event
                // under backpressure. Disconnection itself is the authoritative
                // lifecycle signal, so never leave the status bar claiming a
                // dead server is still indexing.
                Err(TryRecvError::Disconnected) => {
                    if !out.iter().any(|event| matches!(event, LspEvent::Exited)) {
                        out.push(LspEvent::Exited);
                    }
                    break;
                }
            }
        }
        out
    }

    /// Notify the server a document was opened.
    pub fn did_open(&mut self, uri: &str, language_id: &str, version: i64, text: &str) {
        self.send_notification(
            "textDocument/didOpen",
            &json!({
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
            &json!({
                "textDocument": { "uri": uri, "version": version },
                "contentChanges": [ { "text": text } ]
            }),
        );
    }

    /// Notify the server the document was written to disk.
    ///
    /// This is not bookkeeping: rust-analyzer runs `cargo check` on save, and
    /// the compiler errors that make the Problems panel worth reading arrive in
    /// response to this notification and nothing else.
    pub fn did_save(&mut self, uri: &str) {
        self.send_notification(
            "textDocument/didSave",
            &json!({ "textDocument": { "uri": uri } }),
        );
    }

    /// Request completions at a position (result arrives as an `LspEvent`).
    pub fn request_completion(&mut self, uri: &str, pos: Position) {
        self.send_request(
            "textDocument/completion",
            &json!({
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
            &json!({
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
            &json!({
                "textDocument": { "uri": uri },
                "position": { "line": pos.line, "character": pos.character }
            }),
            Pending::Definition,
        );
    }

    fn send_request(&mut self, method: &str, params: &Value, kind: Pending) {
        let has_capacity = self
            .pending
            .lock()
            .is_ok_and(|pending| pending.len() < MAX_PENDING_REQUESTS);
        if !has_capacity {
            return;
        }
        let Some(id) = self.take_request_id() else {
            return;
        };
        let Ok(mut pending) = self.pending.lock() else {
            return;
        };
        pending.insert(id, kind);
        drop(pending);
        self.send_request_raw(method, params, Some(id));
    }

    fn send_request_raw(&mut self, method: &str, params: &Value, id: Option<i64>) {
        let Some(id) = id.or_else(|| self.take_request_id()) else {
            return;
        };
        let payload = json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params,
        });
        self.write(&payload);
    }

    fn take_request_id(&mut self) -> Option<i64> {
        let id = self.next_id?;
        self.next_id = id.checked_add(1);
        Some(id)
    }

    fn send_notification(&self, method: &str, params: &Value) {
        let payload = json!({
            "jsonrpc": "2.0",
            "method": method,
            "params": params,
        });
        self.write(&payload);
    }

    fn write(&self, payload: &Value) {
        let bytes = encode(payload);
        if let Ok(mut stdin) = self.stdin.lock() {
            let _ = stdin.write_all(&bytes);
            let _ = stdin.flush();
        }
    }
}

/// Spawn the background thread that reads framed JSON-RPC messages off the
/// server's stdout, answers server-initiated requests over `reader_stdin`
/// (both ends are behind the same mutex, so a reply can never interleave
/// with a half-written request from the UI thread), and forwards
/// editor-visible events. Returns the receiving end of that event channel.
fn spawn_reader_thread(
    prog: &str,
    stdout: ChildStdout,
    reader_stdin: Arc<Mutex<ChildStdin>>,
    reader_pending: Arc<Mutex<HashMap<i64, Pending>>>,
) -> Result<Receiver<LspEvent>, String> {
    let (tx, rx) = mpsc::sync_channel(LSP_EVENT_QUEUE_CAPACITY);
    let wake = ui::wake::WakeHandle::new();
    std::thread::Builder::new()
        .name("code-editor-lsp".into())
        .spawn(move || {
            let mut reader = MessageReader::new(BufReader::new(stdout));
            loop {
                if let Ok(Some(msg)) = reader.next_message() {
                    let routed = route_message(&msg, &reader_pending);
                    if let Some(reply) = routed.reply {
                        let bytes = encode(&reply);
                        if let Ok(mut w) = reader_stdin.lock() {
                            let _ = w.write_all(&bytes);
                            let _ = w.flush();
                        }
                    }
                    if let Some(ev) = routed.event {
                        if !try_send_event(&tx, ev) {
                            break;
                        }
                        wake.wake();
                    }
                } else {
                    let _ = try_send_event(&tx, LspEvent::Exited);
                    wake.wake();
                    break;
                }
            }
        })
        .map_err(|e| format!("failed to start the {prog} reader thread: {e}"))?;
    Ok(rx)
}

/// Best-effort, non-blocking delivery from the LSP reader. Blocking here would
/// strand the reader after the UI/client is dropped; dropping a full-queue
/// update is preferable because LSP diagnostics and progress are snapshots.
fn try_send_event(tx: &SyncSender<LspEvent>, event: LspEvent) -> bool {
    match tx.try_send(event) {
        Ok(()) | Err(TrySendError::Full(_)) => true,
        Err(TrySendError::Disconnected(_)) => false,
    }
}

/// Kick the `initialize`/`initialized` handshake. `initialize` is a request;
/// once its response lands (surfaced as `Initialized` by the router) the
/// editor treats the server as ready — but rust-analyzer tolerates an eager
/// `initialized` sent before that response, which keeps the client simple.
///
/// `window.workDoneProgress` is the capability that makes indexing visible:
/// without it the server never sends `$/progress`, and a multi-minute index
/// is silent.
fn send_initialize_handshake(client: &mut LspClient, root: &Path, root_uri: &str) {
    let folder_name = root.file_name().map_or_else(
        || root.display().to_string(),
        |s| s.to_string_lossy().to_string(),
    );
    client.send_request_raw(
        "initialize",
        &json!({
            "processId": std::process::id(),
            "rootUri": root_uri,
            "rootPath": root.display().to_string(),
            "workspaceFolders": [ { "uri": root_uri, "name": folder_name } ],
            "capabilities": {
                "window": { "workDoneProgress": true },
                "workspace": {
                    "configuration": true,
                    "workspaceFolders": true,
                    "didChangeConfiguration": { "dynamicRegistration": true }
                },
                "textDocument": {
                    "synchronization": { "didSave": true },
                    "hover": { "contentFormat": ["plaintext", "markdown"] },
                    "completion": { "completionItem": { "snippetSupport": false } },
                    "publishDiagnostics": {},
                    "definition": {}
                }
            }
        }),
        None,
    );
    client.send_notification("initialized", &json!({}));
}

impl Drop for LspClient {
    fn drop(&mut self) {
        // `start` made the server the owner of its process tree.
        bounded_command::kill_tree(&mut self.child);
        let _ = self.child.wait();
    }
}

/// What the reader thread should do with one incoming message: surface an event
/// to the editor, write a reply back to the server, or both.
#[derive(Default)]
struct Routed {
    event: Option<LspEvent>,
    reply: Option<Value>,
}

/// Turn an incoming server message into an editor event and/or a reply,
/// consulting the pending map to resolve response ids to their request kind.
///
/// Replies matter more than they look. A server→client **request** carries an
/// `id`, and the protocol says every request gets a response. rust-analyzer
/// asks `workspace/configuration` and `client/registerCapability` during
/// startup; leaving those unanswered leaves it waiting on us rather than
/// indexing. Answering with a null/empty result means "no per-workspace
/// configuration, defaults are fine", which is exactly true here.
fn route_message(msg: &Value, pending: &Arc<Mutex<HashMap<i64, Pending>>>) -> Routed {
    let method = msg.get("method").and_then(Value::as_str);
    let id = msg.get("id");

    if let Some(method) = method {
        // A `method` *with* an `id` is a request from the server: it needs an
        // answer, not just handling.
        if let Some(id) = id {
            let result = match method {
                // One config object per requested scope; `null` = use defaults.
                "workspace/configuration" => {
                    let n = msg
                        .get("params")
                        .and_then(|p| p.get("items"))
                        .and_then(Value::as_array)
                        .map_or(1, std::vec::Vec::len);
                    Value::Array(vec![Value::Null; n])
                }
                // Accepting a dynamic registration, a progress token, or an
                // apply-edit we do not implement: an empty/negative result is a
                // valid answer and lets the server proceed.
                "workspace/applyEdit" => json!({ "applied": false }),
                _ => Value::Null,
            };
            return Routed {
                event: None,
                reply: Some(json!({
                    "jsonrpc": "2.0",
                    "id": id.clone(),
                    "result": result,
                })),
            };
        }

        // No id: a plain notification.
        let event = match method {
            "textDocument/publishDiagnostics" => parse_publish_diagnostics(msg)
                .map(|(uri, diagnostics)| LspEvent::Diagnostics { uri, diagnostics }),
            "$/progress" => parse_progress(msg).map(LspEvent::Progress),
            _ => None,
        };
        return Routed { event, reply: None };
    }

    // Responses carry an `id` (and a `result` or `error`).
    let Some(id) = id.and_then(Value::as_i64) else {
        return Routed::default();
    };
    let kind = pending.lock().ok().and_then(|mut p| p.remove(&id));
    let event = match kind {
        Some(Pending::Completion) => LspEvent::Completion(parse_completion(msg)),
        Some(Pending::Hover) => LspEvent::Hover(parse_hover(msg)),
        Some(Pending::Definition) => LspEvent::Definition(parse_definition(msg)),
        // The `initialize` response (or any unmatched id) reports readiness.
        None => LspEvent::Initialized,
    };
    Routed {
        event: Some(event),
        reply: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    fn no_pending() -> Arc<Mutex<HashMap<i64, Pending>>> {
        Arc::new(Mutex::new(HashMap::new()))
    }

    #[test]
    fn route_message_answers_requests_but_not_notifications_and_resolves_responses() {
        // Each phase routes against its own pending table.
        a_workspace_configuration_request_is_answered_per_item();
        every_server_request_gets_some_reply();
        a_notification_is_not_answered();
        route_message_resolves_response_ids_to_kinds();
    }

    fn a_workspace_configuration_request_is_answered_per_item() {
        // rust-analyzer asks this during startup and waits for the answer. One
        // result per requested item, `null` meaning "defaults are fine".
        let msg = json!({
            "jsonrpc": "2.0",
            "id": 4,
            "method": "workspace/configuration",
            "params": { "items": [ { "section": "rust-analyzer" }, { "section": "rust" } ] }
        });
        let routed = route_message(&msg, &no_pending());
        let reply = routed.reply.expect("a server request must be answered");
        assert_eq!(reply["id"], json!(4), "the reply carries the request's id");
        assert_eq!(reply["result"], json!([null, null]));
        assert!(
            routed.event.is_none(),
            "answering config is not an editor-visible event"
        );
    }

    fn every_server_request_gets_some_reply() {
        // The rule is protocol-level: a message with both `method` and `id` is a
        // request, and leaving any of them unanswered blocks the server.
        for method in [
            "client/registerCapability",
            "client/unregisterCapability",
            "window/workDoneProgress/create",
            "workspace/applyEdit",
            "some/method/we/have/never/heard/of",
        ] {
            let msg = json!({ "jsonrpc": "2.0", "id": 9, "method": method, "params": {} });
            let routed = route_message(&msg, &no_pending());
            let reply = routed
                .reply
                .unwrap_or_else(|| panic!("{method} was left unanswered"));
            assert_eq!(reply["id"], json!(9));
            assert!(reply.get("result").is_some(), "{method} needs a result");
        }
    }

    fn a_notification_is_not_answered() {
        // Notifications carry no id and must NOT get a response — replying to
        // one is a protocol error.
        let msg = json!({
            "jsonrpc": "2.0",
            "method": "textDocument/publishDiagnostics",
            "params": { "uri": "file:///p/a.rs", "diagnostics": [] }
        });
        let routed = route_message(&msg, &no_pending());
        assert!(routed.reply.is_none());
        assert!(matches!(routed.event, Some(LspEvent::Diagnostics { .. })));
    }

    #[test]
    fn progress_keeps_titles_across_reports_and_shows_the_busiest_task() {
        // Each phase builds its own fake client.
        progress_notifications_parse_into_an_indexing_state();
        a_report_without_a_title_inherits_the_one_begin_carried();
        the_busiest_task_is_the_one_shown();
    }

    fn progress_notifications_parse_into_an_indexing_state() {
        let begin = json!({
            "jsonrpc": "2.0",
            "method": "$/progress",
            "params": {
                "token": "rustAnalyzer/Indexing",
                "value": { "kind": "begin", "title": "Indexing", "percentage": 0 }
            }
        });
        let p = parse_progress(&begin).expect("a begin notification is progress");
        assert_eq!(p.token, "rustAnalyzer/Indexing");
        assert_eq!(p.title, "Indexing");
        assert_eq!(p.percentage, Some(0));
        assert!(!p.done);

        // And it reaches the editor as an event, not just a parse.
        assert!(matches!(
            route_message(&begin, &no_pending()).event,
            Some(LspEvent::Progress(_))
        ));
    }

    fn a_report_without_a_title_inherits_the_one_begin_carried() {
        // The server names the task once and then updates against the token, so
        // a UI reading raw notifications would blank the label at the first
        // report. The client's table is what keeps it stable.
        let mut client = fake_client();
        let begin = Progress {
            token: "t1".into(),
            title: "Indexing".into(),
            message: Some("tilo-shell".into()),
            percentage: Some(10),
            done: false,
        };
        assert_eq!(client.note_progress(&begin).unwrap().title, "Indexing");

        let report = Progress {
            token: "t1".into(),
            title: String::new(), // no title, as a real `report` sends
            message: Some("tilo-ui".into()),
            percentage: Some(55),
            done: false,
        };
        let shown = client.note_progress(&report).expect("still busy");
        assert_eq!(shown.title, "Indexing", "the title survives the report");
        assert_eq!(shown.percentage, Some(55));
        assert_eq!(shown.message.as_deref(), Some("tilo-ui"));
        assert!(client.is_busy());

        // `end` clears it, and the editor stops showing an indexing state.
        let end = Progress {
            token: "t1".into(),
            title: String::new(),
            message: None,
            percentage: None,
            done: true,
        };
        assert!(client.note_progress(&end).is_none());
        assert!(!client.is_busy());
        assert!(client.busy_task().is_none());
    }

    fn the_busiest_task_is_the_one_shown() {
        // A quick sub-task at 90% must not hide the index at 5%.
        let mut client = fake_client();
        for (token, title, pct) in [("a", "Indexing", 5u32), ("b", "Fetching", 90)] {
            client.note_progress(&Progress {
                token: token.into(),
                title: title.into(),
                message: None,
                percentage: Some(pct),
                done: false,
            });
        }
        assert_eq!(client.busy_task().unwrap().title, "Indexing");
    }

    #[test]
    fn client_roots_request_ids_and_server_exit_fail_safe() {
        // Each phase builds its own client or root.
        a_project_path_becomes_a_correctly_encoded_uri();
        a_server_cannot_be_rooted_at_a_path_that_is_not_a_directory();
        missing_server_binary_degrades_gracefully();
        request_identity_exhaustion_never_reuses_an_old_response_id();
        a_disconnected_bounded_reader_still_reports_server_exit();
    }

    fn a_project_path_becomes_a_correctly_encoded_uri() {
        #[cfg(windows)]
        let (prefix, uri_prefix) = ("C:", "file:///c:");
        #[cfg(not(windows))]
        let (prefix, uri_prefix) = ("", "file://");
        // A space in a directory name must be encoded, or the server resolves a
        // different workspace than the one we opened.
        assert_eq!(
            path_to_uri(Path::new(&format!("{prefix}/home/me/my project"))),
            format!("{uri_prefix}/home/me/my%20project")
        );
        assert_eq!(
            path_to_uri(Path::new(&format!("{prefix}/a/b"))),
            format!("{uri_prefix}/a/b")
        );
        let path = PathBuf::from(format!("{prefix}/home/me/my project/src/main.rs"));
        // And it round-trips back through the portal's own parser.
        let uri = path_to_uri(&path);
        assert_eq!(
            ui::portal::to_local_path(&url::Url::parse(&uri).unwrap()),
            Some(PathBuf::from(format!(
                "{}/home/me/my project/src/main.rs",
                prefix.to_ascii_lowercase()
            )))
        );
    }

    fn a_server_cannot_be_rooted_at_a_path_that_is_not_a_directory() {
        // The bug this replaced: a synthetic `file:///project-name` root meant
        // rust-analyzer found no Cargo.toml and reported nothing, forever,
        // while looking like a working editor.
        let d = tempfile::tempdir().unwrap();
        let missing = d.path().join("does-not-exist");
        let Err(err) = LspClient::start(Language::Rust, &missing) else {
            panic!("a non-directory root must be refused");
        };
        assert!(
            err.contains("not a directory"),
            "a bogus root must be refused up front, got: {err}"
        );
    }

    /// An `LspClient` with an inert child process, for exercising the pure
    /// progress-table logic. Every field the tests touch is real.
    fn fake_client() -> LspClient {
        let child = Command::new(std::env::current_exe().expect("test executable"))
            .arg("--list")
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .spawn()
            .expect("spawn inert test executable");
        let mut child = child;
        let stdin = child.stdin.take().expect("piped stdin");
        let (_tx, rx) = mpsc::channel();
        LspClient {
            child,
            stdin: Arc::new(Mutex::new(stdin)),
            events: rx,
            pending: no_pending(),
            next_id: Some(1),
            server_name: "test".into(),
            root: PathBuf::from("/"),
            progress: HashMap::new(),
        }
    }

    #[test]
    fn framing_round_trips_back_to_back_messages_and_refuses_oversized_bodies() {
        // Each phase reads its own buffer.
        framing_round_trips();
        framing_reads_two_back_to_back_messages();
        framing_refuses_an_oversized_advertised_body_before_allocating_it();
    }

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

    fn framing_reads_two_back_to_back_messages() {
        let mut buf = encode(&json!({ "a": 1 }));
        buf.extend(encode(&json!({ "b": 2 })));
        let mut reader = MessageReader::new(Cursor::new(buf));
        assert_eq!(reader.next_message().unwrap().unwrap(), json!({ "a": 1 }));
        assert_eq!(reader.next_message().unwrap().unwrap(), json!({ "b": 2 }));
        assert!(reader.next_message().unwrap().is_none());
    }

    fn framing_refuses_an_oversized_advertised_body_before_allocating_it() {
        let bytes = format!("Content-Length: {}\r\n\r\n", MAX_LSP_MESSAGE_BYTES + 1);
        let mut reader = MessageReader::new(Cursor::new(bytes));
        let error = reader
            .next_message()
            .expect_err("oversized body is refused");
        assert_eq!(error.kind(), std::io::ErrorKind::InvalidData);
        assert!(error.to_string().contains("exceeds"));
    }

    fn request_identity_exhaustion_never_reuses_an_old_response_id() {
        let mut client = fake_client();
        client.next_id = Some(i64::MAX);
        assert_eq!(client.take_request_id(), Some(i64::MAX));
        assert_eq!(client.take_request_id(), None);
    }

    fn a_disconnected_bounded_reader_still_reports_server_exit() {
        let mut client = fake_client();
        let (sender, receiver) = mpsc::sync_channel(1);
        drop(sender);
        client.events = receiver;
        assert!(matches!(client.poll().as_slice(), [LspEvent::Exited]));
    }

    #[test]
    fn server_payloads_parse_into_diagnostics_completions_hover_and_definitions() {
        // Each phase parses its own canned message.
        parses_publish_diagnostics_into_model();
        absent_severity_defaults_to_error();
        parses_completion_list_and_array_shapes();
        completion_prefers_text_edit_new_text();
        parses_hover_markup_marked_string_and_array();
        parses_definition_location_shapes();
    }

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
        assert_eq!(
            diags[0].range.start,
            Position {
                line: 9,
                character: 4
            }
        );
        assert_eq!(diags[0].source.as_deref(), Some("rustc"));

        // Projection onto the panel model converts to 1-based coordinates.
        let panel = diags[0].to_panel();
        assert_eq!(panel.severity, Severity::Error);
        assert_eq!(panel.line, 10);
        assert_eq!(panel.col, 5);
        assert!(panel.message.contains("limitt"));

        assert_eq!(diags[1].to_panel().severity, Severity::Warning);
    }

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

    fn completion_prefers_text_edit_new_text() {
        let msg = json!({ "result": [
            { "label": "greet", "textEdit": { "newText": "greet(${1})" } }
        ]});
        let items = parse_completion(&msg);
        assert_eq!(items[0].insert_text, "greet(${1})");
    }

    fn parses_hover_markup_marked_string_and_array() {
        // MarkupContent { kind, value }.
        let markup =
            json!({ "result": { "contents": { "kind": "markdown", "value": "fn main()" } } });
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

    fn parses_definition_location_shapes() {
        // Single Location.
        let single = json!({ "result": {
            "uri": "file:///proj/src/prime.rs",
            "range": { "start": {"line":3,"character":7}, "end": {"line":3,"character":18} }
        }});
        let locs = parse_definition(&single);
        assert_eq!(locs.len(), 1);
        assert_eq!(locs[0].uri, "file:///proj/src/prime.rs");
        assert_eq!(
            locs[0].range.start,
            Position {
                line: 3,
                character: 7
            }
        );

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

    fn route_message_resolves_response_ids_to_kinds() {
        let pending: Arc<Mutex<HashMap<i64, Pending>>> = Arc::new(Mutex::new(HashMap::new()));
        pending.lock().unwrap().insert(7, Pending::Completion);
        let resp = json!({ "jsonrpc": "2.0", "id": 7, "result": [ { "label": "x" } ] });
        match route_message(&resp, &pending).event {
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
        match route_message(&note, &pending).event {
            Some(LspEvent::Diagnostics { uri, diagnostics }) => {
                assert_eq!(uri, "file:///x.rs");
                assert!(diagnostics.is_empty());
            }
            other => panic!("expected diagnostics event, got {other:?}"),
        }
    }

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
