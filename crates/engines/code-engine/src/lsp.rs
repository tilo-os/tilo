//! A small Language Server Protocol client: the transport, the messages it answers, and the state a
//! request keeps while it runs.
//!
//! The client is complete for the asks a code editor makes — did-open, did-change, diagnostics,
//! completion, hover, go to definition, formatting, progress — and the engine answers the app's
//! asks from it. The parts the app's page has yet to reach (a failure's own words, the server's
//! root, a definition's byte offsets) stay with the client rather than going with the move.
#![expect(
    dead_code,
    reason = "the client's complete surface, which the app asks for one part at a time"
)]

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::sync::mpsc::{self, Receiver, SyncSender, TryRecvError, TrySendError};
use std::sync::{Arc, Mutex};
use lsp_types::notification::{
    DidChangeTextDocument, DidOpenTextDocument, DidSaveTextDocument, Initialized, Notification,
    Progress as ProgressNotification, PublishDiagnostics,
};
use lsp_types::request::{
    ApplyWorkspaceEdit, Completion, Formatting, GotoDefinition, HoverRequest, Initialize, Request,
    WorkspaceConfiguration,
};
use lsp_types::{
    ClientCapabilities, CompletionClientCapabilities, CompletionItemCapability, CompletionItemKind,
    CompletionParams, CompletionResponse, CompletionTextEdit, DiagnosticSeverity,
    DidChangeTextDocumentParams, DidOpenTextDocumentParams, DidSaveTextDocumentParams,
    DocumentFormattingClientCapabilities, DocumentFormattingParams,
    DynamicRegistrationClientCapabilities, FormattingOptions, GotoCapability, GotoDefinitionParams,
    GotoDefinitionResponse, Hover, HoverClientCapabilities, HoverContents, HoverParams,
    InitializeParams, InitializedParams, MarkedString, MarkupKind, NumberOrString,
    PartialResultParams, ProgressParams, ProgressParamsValue, PublishDiagnosticsClientCapabilities,
    PublishDiagnosticsParams, TextDocumentClientCapabilities, TextDocumentContentChangeEvent,
    TextDocumentIdentifier, TextDocumentItem, TextDocumentPositionParams,
    TextDocumentSyncClientCapabilities, VersionedTextDocumentIdentifier, WindowClientCapabilities,
    WorkDoneProgress, WorkDoneProgressParams, WorkspaceClientCapabilities, WorkspaceFolder,
};
pub use lsp_types::{Position, Range, TextEdit};
use serde::Deserialize as _;
use serde_json::{Value, json};

use crate::lang::{Language, Severity};

pub const MAX_LSP_MESSAGE_BYTES: usize = 16 * 1024 * 1024;
const MAX_LSP_HEADER_LINE_BYTES: usize = 8 * 1024;
const LSP_EVENT_QUEUE_CAPACITY: usize = 64;
const MAX_PENDING_REQUESTS: usize = 64;
const MAX_LSP_DIAGNOSTICS: usize = 10_000;
const MAX_LSP_COMPLETIONS: usize = 256;
const MAX_LSP_DEFINITIONS: usize = 128;
const MAX_LSP_DISPLAY_TEXT_CHARS: usize = 64 * 1024;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LspDiagnostic {
    pub range: Range,
    pub severity: DiagnosticSeverity,
    pub message: String,
    pub source: Option<String>,
}

pub fn severity_from_lsp(severity: DiagnosticSeverity) -> Severity {
    match severity {
        DiagnosticSeverity::ERROR => Severity::Error,
        DiagnosticSeverity::WARNING => Severity::Warning,
        DiagnosticSeverity::INFORMATION => Severity::Info,
        _ => Severity::Hint,
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CompletionItem {
    pub label: String,
    pub kind: Option<CompletionItemKind>,
    pub detail: Option<String>,
    pub insert_text: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Location {
    pub uri: String,
    pub range: Range,
}

fn payload<T: serde::de::DeserializeOwned>(msg: &Value, field: &str) -> Option<T> {
    T::deserialize(msg.get(field).unwrap_or(msg)).ok()
}

pub fn parse_publish_diagnostics(msg: &Value) -> Option<(String, Vec<LspDiagnostic>)> {
    let params: PublishDiagnosticsParams = payload(msg, "params")?;
    let uri = normalize_document_uri(&clip_display_text(params.uri.as_str()));
    let diags = params
        .diagnostics
        .into_iter()
        .take(MAX_LSP_DIAGNOSTICS)
        .map(|d| LspDiagnostic {
            range: d.range,
            severity: d.severity.unwrap_or(DiagnosticSeverity::ERROR),
            message: clip_display_text(&d.message),
            source: d.source.as_deref().map(clip_display_text),
        })
        .collect();
    Some((uri, diags))
}

/// A `$/progress` notification as a [`Progress`]. `begin` carries the title and `end` may carry only
/// a closing message, so remembering it is [`LspClient`]'s job.
pub fn parse_progress(msg: &Value) -> Option<Progress> {
    let params: ProgressParams = payload(msg, "params")?;
    let token = match params.token {
        NumberOrString::String(s) => clip_display_text(&s),
        NumberOrString::Number(n) => n.to_string(),
    };
    let ProgressParamsValue::WorkDone(value) = params.value;
    let (title, message, percentage, done) = match value {
        WorkDoneProgress::Begin(b) => (b.title, b.message, b.percentage, false),
        WorkDoneProgress::Report(r) => (String::new(), r.message, r.percentage, false),
        WorkDoneProgress::End(e) => (String::new(), e.message, None, true),
    };
    Some(Progress {
        token,
        title: clip_display_text(&title),
        message: message.as_deref().map(clip_display_text),
        percentage: percentage.map(|p| p.min(100)),
        done,
    })
}

pub fn parse_completion(msg: &Value) -> Vec<CompletionItem> {
    let items = match payload(msg, "result") {
        Some(CompletionResponse::Array(items)) => items,
        Some(CompletionResponse::List(list)) => list.items,
        None => return Vec::new(),
    };
    items
        .into_iter()
        .take(MAX_LSP_COMPLETIONS)
        .map(|item| {
            let label = clip_display_text(&item.label);
            let insert_text = item
                .insert_text
                .or_else(|| {
                    item.text_edit.map(|edit| match edit {
                        CompletionTextEdit::Edit(edit) => edit.new_text,
                        CompletionTextEdit::InsertAndReplace(edit) => edit.new_text,
                    })
                })
                .map_or_else(|| label.clone(), |text| clip_display_text(&text));
            CompletionItem {
                label,
                kind: item.kind,
                detail: item.detail.as_deref().map(clip_display_text),
                insert_text,
            }
        })
        .collect()
}

pub fn parse_hover(msg: &Value) -> Option<String> {
    let marked = |marked: MarkedString| match marked {
        MarkedString::String(text) => text,
        MarkedString::LanguageString(code) => code.value,
    };
    let text = match payload::<Option<Hover>>(msg, "result")??.contents {
        HoverContents::Scalar(text) => marked(text),
        HoverContents::Array(parts) => parts
            .into_iter()
            .map(marked)
            .filter(|part| !part.trim().is_empty())
            .collect::<Vec<_>>()
            .join("\n"),
        HoverContents::Markup(markup) => markup.value,
    };
    let text = clip_display_text(&text);
    (!text.is_empty()).then_some(text)
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

pub fn parse_definition(msg: &Value) -> Vec<Location> {
    let location = |uri: &lsp_types::Uri, range| Location {
        uri: normalize_document_uri(&clip_display_text(uri.as_str())),
        range,
    };
    match payload::<Option<GotoDefinitionResponse>>(msg, "result").flatten() {
        None => Vec::new(),
        Some(GotoDefinitionResponse::Scalar(at)) => vec![location(&at.uri, at.range)],
        Some(GotoDefinitionResponse::Array(all)) => all
            .iter()
            .take(MAX_LSP_DEFINITIONS)
            .map(|at| location(&at.uri, at.range))
            .collect(),
        Some(GotoDefinitionResponse::Link(links)) => links
            .iter()
            .take(MAX_LSP_DEFINITIONS)
            .map(|link| location(&link.target_uri, link.target_selection_range))
            .collect(),
    }
}

pub fn parse_formatting(msg: &Value) -> Result<Vec<TextEdit>, String> {
    if let Some(error) = msg.get("error") {
        return Err(clip_display_text(
            error
                .get("message")
                .and_then(Value::as_str)
                .unwrap_or("the formatter failed"),
        ));
    }
    Option::<Vec<TextEdit>>::deserialize(msg.get("result").unwrap_or(&Value::Null))
        .map(Option::unwrap_or_default)
        .map_err(|_| "the formatter answered something other than edits".into())
}

/// The byte offset in `text` of an LSP position, its column counted in UTF-16
/// code units. A position past the end of its line is the line's end, and a
/// line past the last is the end of the text, as the protocol asks.
fn byte_offset(text: &str, line_starts: &[usize], position: Position) -> usize {
    let Some(&start) = line_starts.get(position.line as usize) else {
        return text.len();
    };
    let mut units = 0;
    for (offset, ch) in text.get(start..).unwrap_or_default().char_indices() {
        if ch == '\n' || units >= position.character as usize {
            return start + offset;
        }
        units += ch.len_utf16();
    }
    text.len()
}

pub fn apply_text_edits(text: &str, edits: &[TextEdit]) -> Result<String, String> {
    let line_starts: Vec<usize> = std::iter::once(0)
        .chain(text.match_indices('\n').map(|(at, _)| at + 1))
        .collect();
    let mut spans: Vec<(usize, usize, &str)> = edits
        .iter()
        .map(|edit| {
            let start = byte_offset(text, &line_starts, edit.range.start);
            let end = byte_offset(text, &line_starts, edit.range.end).max(start);
            (start, end, edit.new_text.as_str())
        })
        .collect();
    spans.sort_by_key(|&(start, end, _)| (start, end));
    let mut out = String::with_capacity(text.len());
    let mut at = 0;
    for (start, end, new_text) in spans {
        if start < at {
            return Err("the formatter's edits overlap".into());
        }
        out.push_str(text.get(at..start).unwrap_or_default());
        out.push_str(new_text);
        at = end;
    }
    out.push_str(text.get(at..).unwrap_or_default());
    Ok(out)
}

/// A JSON-RPC payload as bytes: `Content-Length: N\r\n\r\n<body>`.
pub fn encode(payload: &Value) -> Vec<u8> {
    let body = serde_json::to_vec(payload).unwrap_or_default();
    let mut out = format!("Content-Length: {}\r\n\r\n", body.len()).into_bytes();
    out.extend_from_slice(&body);
    out
}

pub struct MessageReader<R: BufRead> {
    reader: R,
}

impl<R: BufRead> MessageReader<R> {
    pub fn new(reader: R) -> Self {
        Self { reader }
    }

    pub fn next_message(&mut self) -> std::io::Result<Option<Value>> {
        let mut content_length: Option<usize> = None;
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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Pending {
    Completion,
    Hover,
    Definition,
    Formatting,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Progress {
    pub token: String,
    pub title: String,
    pub message: Option<String>,
    pub percentage: Option<u32>,
    pub done: bool,
}

#[derive(Clone, Debug)]
pub enum LspEvent {
    Initialized,
    Diagnostics {
        uri: String,
        diagnostics: Vec<LspDiagnostic>,
    },
    Completion(Vec<CompletionItem>),
    Hover(Option<String>),
    Definition(Vec<Location>),
    Formatted(Result<Vec<TextEdit>, String>),
    Progress(Progress),
    Exited,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LspFailure {
    NotInstalled {
        server: String,
    },
    Stopped {
        server: String,
        detail: Option<String>,
    },
    Unknown {
        server: String,
        detail: String,
    },
}

impl LspFailure {
    pub fn server(&self) -> &str {
        match self {
            LspFailure::NotInstalled { server }
            | LspFailure::Stopped { server, .. }
            | LspFailure::Unknown { server, .. } => server,
        }
    }

    pub fn detail(&self) -> Option<&str> {
        match self {
            LspFailure::NotInstalled { .. } => None,
            LspFailure::Stopped { detail, .. } => detail.as_deref(),
            LspFailure::Unknown { detail, .. } => Some(detail),
        }
    }
}

impl std::fmt::Display for LspFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LspFailure::NotInstalled { server } => write!(f, "{server} is not installed"),
            LspFailure::Stopped {
                server,
                detail: None,
            } => write!(f, "{server} stopped"),
            LspFailure::Stopped {
                server,
                detail: Some(detail),
            } => write!(f, "{server} stopped ({detail})"),
            LspFailure::Unknown { detail, .. } => f.write_str(detail),
        }
    }
}

pub struct LspClient {
    child: Child,
    stdin: Arc<Mutex<ChildStdin>>,
    events: Receiver<LspEvent>,
    pending: Arc<Mutex<HashMap<i64, Pending>>>,
    next_id: Option<i64>,
    server_name: String,
    root: PathBuf,
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

/// The path a document URI stands for: what the server names a document by, read back as the
/// path the app keeps it at. A URI that names no local file keeps its scheme, so nothing is
/// mistaken for a path.
#[must_use]
pub fn uri_to_path(uri: &str) -> String {
    let Some(rest) = uri.strip_prefix("file://") else {
        return uri.to_owned();
    };
    // Windows: `file:///c:/a` is `c:/a`; elsewhere `file:///a` is `/a`.
    let bytes = rest.as_bytes();
    let drive = bytes.first().is_some_and(|b| b.is_ascii_alphabetic())
        && bytes.get(1) == Some(&b':');
    let rest = if drive && rest.len() > 3 {
        rest.get(1..).unwrap_or(rest)
    } else {
        rest
    };
    let bytes = rest.as_bytes();
    let mut out = String::with_capacity(rest.len());
    let mut at = 0usize;
    while at < bytes.len() {
        let byte = bytes.get(at).copied().unwrap_or_default();
        if byte == b'%' {
            let hex = bytes
                .get(at + 1..at.saturating_add(3))
                .and_then(|hex| std::str::from_utf8(hex).ok());
            if let Some(value) = hex.and_then(|hex| u8::from_str_radix(hex, 16).ok()) {
                out.push(char::from(value));
                at = at.saturating_add(3);
                continue;
            }
        }
        out.push(char::from(byte));
        at = at.saturating_add(1);
    }
    out
}

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
    /// [`LspFailure::NotInstalled`] when the server's program is not on this
    /// computer; [`LspFailure::Unknown`] when `lang` has no server mapping,
    /// `root` is not a directory, the program can't be launched, or its piped
    /// stdio could not be taken.
    pub fn start(lang: Language, root: &Path) -> Result<LspClient, LspFailure> {
        Self::start_with(lang, root, &tilo_process::tools::ToolDirs::current())
    }

    pub fn start_with(
        lang: Language,
        root: &Path,
        tools: &tilo_process::tools::ToolDirs,
    ) -> Result<LspClient, LspFailure> {
        let (prog, args) = Self::server_command(lang).ok_or_else(|| LspFailure::Unknown {
            server: lang.label().to_owned(),
            detail: format!("no language server configured for {}", lang.label()),
        })?;
        let unknown = |detail: String| LspFailure::Unknown {
            server: prog.clone(),
            detail,
        };

        if !root.is_dir() {
            return Err(unknown(format!("{} is not a directory", root.display())));
        }
        let root_uri = path_to_uri(root);

        let program = tools.find(&prog).ok_or_else(|| LspFailure::NotInstalled {
            server: prog.clone(),
        })?;
        let mut command = Command::new(&program);
        bounded_command::without_console_window(&mut command);
        command
            .args(&args)
            .current_dir(root)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        bounded_command::own_process_tree(&mut command);
        let mut child = command.spawn().map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                LspFailure::NotInstalled {
                    server: prog.clone(),
                }
            } else {
                unknown(format!("failed to launch {prog}: {e}"))
            }
        })?;

        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| unknown(format!("{prog}: piped stdin was not created")))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| unknown(format!("{prog}: piped stdout was not created")))?;
        let stdin = Arc::new(Mutex::new(stdin));
        let pending: Arc<Mutex<HashMap<i64, Pending>>> = Arc::new(Mutex::new(HashMap::new()));

        let events =
            spawn_reader_thread(&prog, stdout, stdin.clone(), pending.clone()).map_err(unknown)?;

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

    pub fn exit_detail(&mut self) -> Option<String> {
        self.child
            .try_wait()
            .ok()
            .flatten()
            .map(|status| status.to_string())
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

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

    pub fn is_busy(&self) -> bool {
        !self.progress.is_empty()
    }

    pub fn busy_task(&self) -> Option<&Progress> {
        self.progress
            .values()
            .min_by_key(|p| p.percentage.unwrap_or(0))
    }

    pub fn server_name(&self) -> &str {
        &self.server_name
    }

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

    pub fn did_open(&mut self, uri: &str, language_id: &str, version: i64, text: &str) {
        let Some(uri) = parse_uri(uri) else { return };
        self.notify::<DidOpenTextDocument>(&DidOpenTextDocumentParams {
            text_document: TextDocumentItem::new(
                uri,
                language_id.to_owned(),
                protocol_version(version),
                text.to_owned(),
            ),
        });
    }

    pub fn did_change(&mut self, uri: &str, version: i64, text: &str) {
        let Some(uri) = parse_uri(uri) else { return };
        self.notify::<DidChangeTextDocument>(&DidChangeTextDocumentParams {
            text_document: VersionedTextDocumentIdentifier::new(uri, protocol_version(version)),
            content_changes: vec![TextDocumentContentChangeEvent {
                range: None,
                range_length: None,
                text: text.to_owned(),
            }],
        });
    }

    pub fn did_save(&mut self, uri: &str) {
        let Some(uri) = parse_uri(uri) else { return };
        self.notify::<DidSaveTextDocument>(&DidSaveTextDocumentParams {
            text_document: TextDocumentIdentifier::new(uri),
            text: None,
        });
    }

    pub fn request_completion(&mut self, uri: &str, pos: Position) {
        let Some(at) = position_params(uri, pos) else {
            return;
        };
        self.send_request::<Completion>(
            &CompletionParams {
                text_document_position: at,
                work_done_progress_params: WorkDoneProgressParams::default(),
                partial_result_params: PartialResultParams::default(),
                context: None,
            },
            Pending::Completion,
        );
    }

    pub fn request_hover(&mut self, uri: &str, pos: Position) {
        let Some(at) = position_params(uri, pos) else {
            return;
        };
        self.send_request::<HoverRequest>(
            &HoverParams {
                text_document_position_params: at,
                work_done_progress_params: WorkDoneProgressParams::default(),
            },
            Pending::Hover,
        );
    }

    pub fn request_definition(&mut self, uri: &str, pos: Position) {
        let Some(at) = position_params(uri, pos) else {
            return;
        };
        self.send_request::<GotoDefinition>(
            &GotoDefinitionParams {
                text_document_position_params: at,
                work_done_progress_params: WorkDoneProgressParams::default(),
                partial_result_params: PartialResultParams::default(),
            },
            Pending::Definition,
        );
    }

    pub fn request_formatting(&mut self, uri: &str, tab_size: u32, insert_spaces: bool) -> bool {
        let Some(uri) = parse_uri(uri) else {
            return false;
        };
        self.send_request::<Formatting>(
            &DocumentFormattingParams {
                text_document: TextDocumentIdentifier::new(uri),
                options: FormattingOptions {
                    tab_size,
                    insert_spaces,
                    ..FormattingOptions::default()
                },
                work_done_progress_params: WorkDoneProgressParams::default(),
            },
            Pending::Formatting,
        )
    }

    fn send_request<R: Request>(&mut self, params: &R::Params, kind: Pending) -> bool {
        let has_capacity = self
            .pending
            .lock()
            .is_ok_and(|pending| pending.len() < MAX_PENDING_REQUESTS);
        if !has_capacity {
            return false;
        }
        let Some(id) = self.take_request_id() else {
            return false;
        };
        let Ok(mut pending) = self.pending.lock() else {
            return false;
        };
        pending.insert(id, kind);
        drop(pending);
        self.write_request::<R>(id, params);
        true
    }

    fn write_request<R: Request>(&self, id: i64, params: &R::Params) {
        self.write(&json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": R::METHOD,
            "params": params,
        }));
    }

    fn take_request_id(&mut self) -> Option<i64> {
        let id = self.next_id?;
        self.next_id = id.checked_add(1);
        Some(id)
    }

    fn notify<N: Notification>(&self, params: &N::Params) {
        self.write(&json!({
            "jsonrpc": "2.0",
            "method": N::METHOD,
            "params": params,
        }));
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
                        // A save waits on the formatter's answer, so that one
                        // is delivered, never dropped as a snapshot may be.
                        let delivered = if matches!(ev, LspEvent::Formatted(_)) {
                            tx.send(ev).is_ok()
                        } else {
                            try_send_event(&tx, ev)
                        };
                        if !delivered {
                            break;
                        }
                    }
                } else {
                    let _ = try_send_event(&tx, LspEvent::Exited);
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
    let Some(id) = client.take_request_id() else {
        return;
    };
    let root_uri = parse_uri(root_uri);
    #[allow(
        deprecated,
        reason = "servers that predate workspace folders still read rootUri and rootPath"
    )]
    let params = InitializeParams {
        process_id: Some(std::process::id()),
        root_path: Some(root.display().to_string()),
        root_uri: root_uri.clone(),
        workspace_folders: root_uri.map(|uri| {
            vec![WorkspaceFolder {
                uri,
                name: folder_name,
            }]
        }),
        capabilities: ClientCapabilities {
            window: Some(WindowClientCapabilities {
                work_done_progress: Some(true),
                ..WindowClientCapabilities::default()
            }),
            workspace: Some(WorkspaceClientCapabilities {
                configuration: Some(true),
                workspace_folders: Some(true),
                did_change_configuration: Some(DynamicRegistrationClientCapabilities {
                    dynamic_registration: Some(true),
                }),
                ..WorkspaceClientCapabilities::default()
            }),
            text_document: Some(TextDocumentClientCapabilities {
                synchronization: Some(TextDocumentSyncClientCapabilities {
                    did_save: Some(true),
                    ..TextDocumentSyncClientCapabilities::default()
                }),
                hover: Some(HoverClientCapabilities {
                    content_format: Some(vec![MarkupKind::PlainText, MarkupKind::Markdown]),
                    ..HoverClientCapabilities::default()
                }),
                completion: Some(CompletionClientCapabilities {
                    completion_item: Some(CompletionItemCapability {
                        snippet_support: Some(false),
                        ..CompletionItemCapability::default()
                    }),
                    ..CompletionClientCapabilities::default()
                }),
                publish_diagnostics: Some(PublishDiagnosticsClientCapabilities::default()),
                definition: Some(GotoCapability::default()),
                formatting: Some(DocumentFormattingClientCapabilities::default()),
                ..TextDocumentClientCapabilities::default()
            }),
            ..ClientCapabilities::default()
        },
        ..InitializeParams::default()
    };
    client.write_request::<Initialize>(id, &params);
    client.notify::<Initialized>(&InitializedParams {});
}

fn parse_uri(uri: &str) -> Option<lsp_types::Uri> {
    uri.parse().ok()
}

fn position_params(uri: &str, position: Position) -> Option<TextDocumentPositionParams> {
    Some(TextDocumentPositionParams::new(
        TextDocumentIdentifier::new(parse_uri(uri)?),
        position,
    ))
}

fn protocol_version(version: i64) -> i32 {
    i32::try_from(version).unwrap_or(i32::MAX)
}

impl LspClient {
    /// A client to a process that reads what it is sent and never answers.
    #[cfg(test)]
    pub(crate) fn inert() -> LspClient {
        let child = Command::new(std::env::current_exe().expect("test executable"))
            .arg("--list")
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .spawn_locked()
            .expect("spawn inert test executable");
        let mut child = child;
        let stdin = child.stdin.take().expect("piped stdin");
        let (_tx, rx) = mpsc::channel();
        LspClient {
            child,
            stdin: Arc::new(Mutex::new(stdin)),
            events: rx,
            pending: Arc::new(Mutex::new(HashMap::new())),
            next_id: Some(1),
            server_name: "test".into(),
            root: PathBuf::from("/"),
            progress: HashMap::new(),
        }
    }
}

impl Drop for LspClient {
    fn drop(&mut self) {
        bounded_command::kill_tree(&mut self.child);
        let _ = self.child.wait();
    }
}

#[derive(Default)]
struct Routed {
    event: Option<LspEvent>,
    reply: Option<Value>,
}

fn route_message(msg: &Value, pending: &Arc<Mutex<HashMap<i64, Pending>>>) -> Routed {
    let method = msg.get("method").and_then(Value::as_str);
    let id = msg.get("id");

    if let Some(method) = method {
        if let Some(id) = id {
            let result = match method {
                WorkspaceConfiguration::METHOD => {
                    let n = msg
                        .get("params")
                        .and_then(|p| p.get("items"))
                        .and_then(Value::as_array)
                        .map_or(1, std::vec::Vec::len);
                    Value::Array(vec![Value::Null; n])
                }
                ApplyWorkspaceEdit::METHOD => json!({ "applied": false }),
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

        let event = match method {
            PublishDiagnostics::METHOD => parse_publish_diagnostics(msg)
                .map(|(uri, diagnostics)| LspEvent::Diagnostics { uri, diagnostics }),
            ProgressNotification::METHOD => parse_progress(msg).map(LspEvent::Progress),
            _ => None,
        };
        return Routed { event, reply: None };
    }

    let Some(id) = id.and_then(Value::as_i64) else {
        return Routed::default();
    };
    let kind = pending.lock().ok().and_then(|mut p| p.remove(&id));
    let event = match kind {
        Some(Pending::Completion) => LspEvent::Completion(parse_completion(msg)),
        Some(Pending::Hover) => LspEvent::Hover(parse_hover(msg)),
        Some(Pending::Definition) => LspEvent::Definition(parse_definition(msg)),
        Some(Pending::Formatting) => LspEvent::Formatted(parse_formatting(msg)),
        None => LspEvent::Initialized,
    };
    Routed {
        event: Some(event),
        reply: None,
    }
}
