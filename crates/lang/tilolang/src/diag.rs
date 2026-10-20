//! Source spans and diagnostics.
//!
//! Every error carries a span, a stable code (`E0xx`) and a message written for an agent to act
//! on. The checker reports every error it can find, not only the first.

use serde::{Deserialize, Serialize};

/// A byte range in the source text.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Span {
    pub start: usize,
    pub end: usize,
}

impl Span {
    pub const fn new(start: usize, end: usize) -> Self {
        Self { start, end }
    }

    /// The span from the start of `self` to the end of `other`.
    #[must_use]
    pub const fn to(self, other: Self) -> Self {
        Self {
            start: self.start,
            end: other.end,
        }
    }
}

/// One reported problem.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Diagnostic {
    /// Stable code, for example `E030`.
    pub code: String,
    pub message: String,
    pub span: Span,
    /// 1-based line of `span.start`; 0 until [`Diagnostic::locate`] ran.
    pub line: usize,
    /// 1-based column (in characters) of `span.start`; 0 until located.
    pub col: usize,
    /// The file of a multi-file package the span points into, as the package named it; `None`
    /// for a single source text. The span is then an offset in that file.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file: Option<String>,
}

impl Diagnostic {
    pub fn new(code: &str, span: Span, message: impl Into<String>) -> Self {
        Self {
            code: code.to_string(),
            message: message.into(),
            span,
            line: 0,
            col: 0,
            file: None,
        }
    }

    /// Fills in `line` and `col` from the source text the span points into.
    pub fn locate(&mut self, src: &str) {
        let mut line = 1usize;
        let mut col = 1usize;
        for (offset, ch) in src.char_indices() {
            if offset >= self.span.start {
                break;
            }
            if ch == '\n' {
                line += 1;
                col = 1;
            } else {
                col += 1;
            }
        }
        self.line = line;
        self.col = col;
    }

    /// `path:line:col: E0xx message`, the form `tilolang check` prints. A diagnostic of a
    /// multi-file package is shown under its own file's path instead of `path`.
    pub fn render(&self, path: &str) -> String {
        let shown = self.file.as_deref().unwrap_or(path);
        format!(
            "{shown}:{}:{}: {} {}",
            self.line, self.col, self.code, self.message
        )
    }

    /// The structured form for agents.
    pub fn to_json(&self) -> serde_json::Value {
        let mut out = serde_json::json!({
            "code": self.code,
            "message": self.message,
            "line": self.line,
            "col": self.col,
            "start": self.span.start,
            "end": self.span.end,
        });
        if let (Some(file), Some(map)) = (&self.file, out.as_object_mut()) {
            map.insert("file".to_string(), serde_json::Value::String(file.clone()));
        }
        out
    }
}

/// Locates every diagnostic and orders them by position.
pub(crate) fn finish(mut diags: Vec<Diagnostic>, src: &str) -> Vec<Diagnostic> {
    for d in &mut diags {
        d.locate(src);
    }
    diags.sort_by_key(|d| (d.span.start, d.code.clone()));
    diags.dedup();
    diags
}

/// One file of a package in the shared span space: it starts at `base`.
pub(crate) struct Placed<'a> {
    pub path: &'a str,
    pub text: &'a str,
    pub base: usize,
}

/// Moves every diagnostic into the file its span points into, locates it there, and orders
/// them by file and position.
pub(crate) fn finish_files(mut diags: Vec<Diagnostic>, files: &[Placed<'_>]) -> Vec<Diagnostic> {
    for d in &mut diags {
        let Some(file) = files
            .iter()
            .rev()
            .find(|f| f.base <= d.span.start)
            .or_else(|| files.first())
        else {
            continue;
        };
        let start = d.span.start.saturating_sub(file.base).min(file.text.len());
        let end = d
            .span
            .end
            .saturating_sub(file.base)
            .clamp(start, file.text.len());
        d.span = Span::new(start, end);
        d.file = Some(file.path.to_string());
        d.locate(file.text);
    }
    diags.sort_by(|a, b| (&a.file, a.span.start, &a.code).cmp(&(&b.file, b.span.start, &b.code)));
    diags.dedup();
    diags
}

/// All diagnostics as one JSON array.
pub fn diagnostics_json(diags: &[Diagnostic]) -> String {
    let items: Vec<serde_json::Value> = diags.iter().map(Diagnostic::to_json).collect();
    serde_json::to_string_pretty(&items).unwrap_or_else(|_| "[]".to_string())
}
