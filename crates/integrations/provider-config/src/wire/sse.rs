//! A streamed answer: server-sent events read as they arrive, each folded
//! into the whole answer by its wire, until the provider says the turn is
//! over.
//!
//! The reader is strict where a lenient one would pass off a cut answer as a
//! finished one: bytes must be UTF-8, an event counts only once a blank line
//! ends it, and the stream must reach its wire's end (Anthropic's
//! `message_stop`, a chat finish reason, Responses' completed answer) before
//! [`Stream::finish`] gives the answer.

use std::ops::ControlFlow;

use serde_json::Value;
use wire_frame::sse::{Error as SseError, Reader, Utf8};

use super::{Api, Delta, anthropic, chat, responses};
use crate::protocol::{ErrorKind, ModelError, ModelRequest, ModelResponse};

/// The most bytes one streamed answer may take.
pub const MOST_BYTES: usize = 1024 * 1024;
/// The most tool calls one turn may make.
pub(super) const MOST_CALLS: usize = 256;
/// The most content blocks one Anthropic message may have.
pub(super) const MOST_BLOCKS: usize = 1024;

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
    sse: Reader,
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
            sse: Reader::new(Utf8::Strict, Some(MOST_BYTES)),
            done: false,
        }
    }

    /// The next `bytes` of the stream; the deltas they complete go to `out`.
    ///
    /// # Errors
    /// The provider failing in the stream, or a stream that is no answer.
    pub fn push(&mut self, bytes: &[u8], out: &mut Vec<Delta>) -> Result<(), StreamError> {
        let fold = &mut self.fold;
        let done = &mut self.done;
        match self
            .sse
            .push(bytes, |event| match dispatch(fold, done, event.data, out) {
                Ok(()) => ControlFlow::Continue(()),
                Err(error) => ControlFlow::Break(error),
            }) {
            Ok(ControlFlow::Continue(())) => Ok(()),
            Ok(ControlFlow::Break(error)) => Err(error),
            Err(SseError::TooLarge) => Err(StreamError::TooLarge),
            Err(SseError::InvalidUtf8) => {
                Err(StreamError::Malformed("the stream is not UTF-8".into()))
            }
        }
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
        if self.sse.has_pending() {
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

fn dispatch(
    fold: &mut Fold,
    done: &mut bool,
    data: &str,
    out: &mut Vec<Delta>,
) -> Result<(), StreamError> {
    if data.is_empty() {
        return Ok(());
    }
    if data == "[DONE]" {
        *done = true;
        return Ok(());
    }
    if *done {
        return Err(StreamError::Malformed(
            "the provider wrote after its stream ended".into(),
        ));
    }
    let event: Value = serde_json::from_str(data)
        .map_err(|_| StreamError::Malformed("an event that is not JSON".into()))?;
    let folded = match fold {
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
