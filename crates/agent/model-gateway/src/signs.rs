//! The signs a provider gives that a request reached it, and how long it may
//! give none before its call is let go.
use std::future::Future;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use super::{Again, ProviderFailure};

/// When a provider first showed that a request had reached it. A call's
/// `first_byte` is the first event its decoder understood, and a provider
/// that holds its answer back sends that last: these are the signs that come
/// before it, so a call that waited on a silent provider and one that waited
/// on a working one read differently. Moments only; nothing a provider sent
/// is kept.
#[derive(Default)]
pub(super) struct Signs {
    /// When the response's headers came. A request the gateway sent again
    /// names the headers of its last ask.
    headers: Mutex<Option<Instant>>,
    /// When the first bytes of the response's body came, whatever they
    /// were: a piece of the answer, a keep-alive comment, a ping.
    first_event: OnceLock<Instant>,
    /// How long the provider may show nothing until its call is let go:
    /// no response headers after the request, no bytes after the headers
    /// or after the bytes before. Unset where the call's deadline alone
    /// bounds it.
    quiet: OnceLock<Duration>,
}

impl Signs {
    /// The response's headers came now.
    pub(super) fn headers_came(&self) {
        *self
            .headers
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(Instant::now());
    }

    /// When the response's headers came.
    pub(super) fn headers(&self) -> Option<Instant> {
        *self
            .headers
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Bytes of the response's body came now.
    pub(super) fn event_came(&self) {
        self.first_event.get_or_init(Instant::now);
    }

    /// When the first bytes of the response's body came.
    pub(super) fn first_event(&self) -> Option<Instant> {
        self.first_event.get().copied()
    }

    /// The call is let go where its provider shows nothing for `quiet`.
    pub(super) fn let_go_after(&self, quiet: Duration) {
        self.quiet.get_or_init(|| quiet);
    }

    /// How long the provider may show nothing, where the call names a time.
    pub(super) fn quiet(&self) -> Option<Duration> {
        self.quiet.get().copied()
    }

    /// What `waiting` on the provider gives, or the call's failure where
    /// the provider showed nothing for as long as it may: `what` did not
    /// come. Whether the provider took the request is then not known, so
    /// the gateway does not send it again; its caller may ask anew.
    pub(super) async fn or_silence<T>(
        &self,
        waiting: impl Future<Output = T>,
        what: &str,
    ) -> Result<T, ProviderFailure> {
        let Some(quiet) = self.quiet.get() else {
            return Ok(waiting.await);
        };
        tokio::time::timeout(*quiet, waiting)
            .await
            .map_err(|_| ProviderFailure {
                model_error: agent_provider_config::protocol::ModelError {
                    kind: agent_provider_config::protocol::ErrorKind::TimedOut,
                    message: format!(
                        "The model's provider sent nothing for {} s",
                        quiet.as_secs_f32()
                    ),
                },
                why: format!(
                    "provider sent {what} for {} ms; the call was let go",
                    quiet.as_millis()
                ),
                again: Again::Soon(None),
                outcome_unknown: true,
                unprocessed: false,
                status: None,
            })
    }
}
