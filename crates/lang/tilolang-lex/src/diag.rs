//! Source spans, and the diagnostics a line of source can carry: every one has a span and a
//! stable code (`E0xx`).

use serde::{Deserialize, Serialize};

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

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Diagnostic {
    /// Stable code, for example `E030`.
    pub code: String,
    pub message: String,
    pub span: Span,
    /// 1-based line of `span.start`; 0 until [`Diagnostic::locate`] ran.
    pub line: usize,
    /// 1-based column, in characters, of `span.start`; 0 until located.
    pub col: usize,
    /// The file of a multi-file package the span points into; `None` for a single source text,
    /// where the span is an offset in that file.
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

    /// `path:line:col: E0xx message`: what `tilolang check` prints, under the file's own path
    /// for a diagnostic of a multi-file package.
    pub fn render(&self, path: &str) -> String {
        let shown = self.file.as_deref().unwrap_or(path);
        format!(
            "{shown}:{}:{}: {} {}",
            self.line, self.col, self.code, self.message
        )
    }

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
