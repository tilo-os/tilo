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
}

impl Diagnostic {
    pub fn new(code: &str, span: Span, message: impl Into<String>) -> Self {
        Self {
            code: code.to_string(),
            message: message.into(),
            span,
            line: 0,
            col: 0,
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

    /// `path:line:col: E0xx message`, the form `tilolang check` prints.
    pub fn render(&self, path: &str) -> String {
        format!(
            "{path}:{}:{}: {} {}",
            self.line, self.col, self.code, self.message
        )
    }

    /// The structured form for agents.
    pub fn to_json(&self) -> serde_json::Value {
        serde_json::json!({
            "code": self.code,
            "message": self.message,
            "line": self.line,
            "col": self.col,
            "start": self.span.start,
            "end": self.span.end,
        })
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

/// All diagnostics as one JSON array.
pub fn diagnostics_json(diags: &[Diagnostic]) -> String {
    let items: Vec<serde_json::Value> = diags.iter().map(Diagnostic::to_json).collect();
    serde_json::to_string_pretty(&items).unwrap_or_else(|_| "[]".to_string())
}
