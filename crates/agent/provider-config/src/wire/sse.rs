//! A streamed answer: server-sent events read as they arrive, each folded
//! into the whole answer by its wire, until the provider says the turn is
//! over.
//!
//! The reader is strict where a lenient one would pass off a cut answer as a
//! finished one: bytes must be UTF-8, an event counts only once a blank line
//! ends it, and the stream must reach its wire's end (Anthropic's
//! `message_stop`, a chat finish reason, Responses' completed answer) before
//! [`Stream::finish`] gives the answer.

use serde_json::Value;

use super::{Api, Delta, anthropic, chat, responses};
use crate::protocol::{ErrorKind, ModelError, ModelRequest, ModelResponse};

/// The most bytes one streamed answer may take.
pub const MOST_BYTES: usize = 1024 * 1024;
/// The most tool calls one turn may make.
pub(super) const MOST_CALLS: usize = 256;
/// The most content blocks one Anthropic message may have.
pub(super) const MOST_BLOCKS: usize = 1024;
/// The longest one line of the stream may be.
const MOST_LINE_BYTES: usize = MOST_BYTES;

enum Fold {
    Anthropic(anthropic::Fold),
    Chat(chat::Fold),
    Responses(responses::Fold),
}

/// Why a stream gave no answer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StreamError {
    /// The provider reported a failure inside the stream; its event, to be
    /// classified like an HTTP error body (and never shown as it is).
    Provider(String),
    /// The stream ended before the turn did.
    Truncated,
    /// The stream is no answer on its wire.
    Malformed(String),
    /// More was sent than one answer may take.
    TooLarge,
}

impl StreamError {
    /// The model error this is, when no provider body says better.
    #[must_use]
    pub fn model_error(&self) -> ModelError {
        match self {
            Self::Provider(_) => ModelError::new(ErrorKind::Provider),
            Self::Truncated | Self::TooLarge => ModelError::new(ErrorKind::Truncated),
            Self::Malformed(why) => ModelError::invalid(why.clone()),
        }
    }
}

/// One streamed answer being read.
pub struct Stream {
    api: Api,
    fold: Fold,
    /// Bytes of an unfinished line, and of a codepoint cut between reads.
    line: Vec<u8>,
    /// The `data` lines of the event being read.
    data: String,
    /// Whether the event being read has any field yet.
    pending: bool,
    /// A carriage return just ended a line (a following newline is part of it).
    cr: bool,
    read: usize,
    done: bool,
}

impl Stream {
    /// A stream on `api`'s wire.
    #[must_use]
    pub fn new(api: Api) -> Self {
        Self::reading(api, false)
    }

    /// A stream of an answer to a request [`super::body`] wrote for `profile`.
    #[must_use]
    pub fn of(profile: &crate::ProviderProfile) -> Self {
        Self::reading(Api::of(profile), profile.is_googles_endpoint())
    }

    fn reading(api: Api, google: bool) -> Self {
        Self {
            api,
            fold: match api {
                Api::Anthropic => Fold::Anthropic(anthropic::Fold::default()),
                Api::Chat => Fold::Chat(chat::Fold::new(google)),
                Api::Responses | Api::ChatGpt => Fold::Responses(responses::Fold::default()),
            },
            line: Vec::new(),
            data: String::new(),
            pending: false,
            cr: false,
            read: 0,
            done: false,
        }
    }

    /// The next `bytes` of the stream; the deltas they complete go to `out`.
    ///
    /// # Errors
    /// The provider failing in the stream, or a stream that is no answer.
    pub fn push(&mut self, bytes: &[u8], out: &mut Vec<Delta>) -> Result<(), StreamError> {
        self.read = self.read.saturating_add(bytes.len());
        if self.read > MOST_BYTES {
            return Err(StreamError::TooLarge);
        }
        for &byte in bytes {
            if byte == b'\n' && self.cr {
                self.cr = false;
                continue;
            }
            self.cr = byte == b'\r';
            if matches!(byte, b'\r' | b'\n') {
                let line = std::mem::take(&mut self.line);
                let line = String::from_utf8(line)
                    .map_err(|_| StreamError::Malformed("the stream is not UTF-8".into()))?;
                self.line_ended(&line, out)?;
            } else {
                if self.line.len() >= MOST_LINE_BYTES {
                    return Err(StreamError::TooLarge);
                }
                self.line.push(byte);
            }
        }
        Ok(())
    }

    fn line_ended(&mut self, line: &str, out: &mut Vec<Delta>) -> Result<(), StreamError> {
        if line.is_empty() {
            if self.pending {
                self.pending = false;
                let data = std::mem::take(&mut self.data);
                self.dispatch(&data, out)?;
            }
            return Ok(());
        }
        if line.starts_with(':') {
            return Ok(());
        }
        self.pending = true;
        let (field, value) = line.split_once(':').unwrap_or((line, ""));
        let value = value.strip_prefix(' ').unwrap_or(value);
        if field == "data" {
            if !self.data.is_empty() {
                self.data.push('\n');
            }
            self.data.push_str(value);
        }
        Ok(())
    }

    fn dispatch(&mut self, data: &str, out: &mut Vec<Delta>) -> Result<(), StreamError> {
        if data.is_empty() {
            return Ok(());
        }
        if data == "[DONE]" {
            self.done = true;
            return Ok(());
        }
        if self.done {
            return Err(StreamError::Malformed(
                "the provider wrote after its stream ended".into(),
            ));
        }
        let event: Value = serde_json::from_str(data)
            .map_err(|_| StreamError::Malformed("an event that is not JSON".into()))?;
        let folded = match &mut self.fold {
            Fold::Anthropic(fold) => fold.event(&event, out),
            Fold::Chat(fold) => fold.chunk(&event, out),
            Fold::Responses(fold) => fold.event(&event, out),
        };
        folded.map_err(|why| {
            if event.get("error").is_some()
                || matches!(
                    event.get("type").and_then(Value::as_str),
                    Some("error" | "response.failed")
                )
            {
                StreamError::Provider(why)
            } else {
                StreamError::Malformed(why)
            }
        })
    }

    /// The answer the whole stream gave, checked against `request`; the
    /// deltas closing its calls go to `out` first.
    ///
    /// # Errors
    /// A stream cut before its wire's end, or an answer that fails
    /// [`super::check`].
    pub fn finish(
        self,
        request: &ModelRequest,
        out: &mut Vec<Delta>,
    ) -> Result<ModelResponse, StreamError> {
        // An event without its closing blank line was cut, however
        // complete it looks.
        if self.pending || !self.line.is_empty() {
            return Err(StreamError::Truncated);
        }
        let google = matches!(&self.fold, Fold::Chat(fold) if fold.google());
        let answer = match self.fold {
            Fold::Anthropic(fold) if fold.finished() => fold.answer(),
            Fold::Chat(fold) if fold.finished() => Ok(fold.answer(out)),
            Fold::Responses(fold) if fold.finished() => fold.answer(),
            _ => return Err(StreamError::Truncated),
        }
        .map_err(StreamError::Malformed)?;
        let response = super::decode(self.api, google, &answer)
            .map_err(|error| StreamError::Malformed(error.message))?;
        super::check(&response, request).map_err(|error| StreamError::Malformed(error.message))?;
        Ok(response)
    }
}
