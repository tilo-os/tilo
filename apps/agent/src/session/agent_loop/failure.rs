//! What a failed turn says, split into the sentence a person reads and the
//! record that helps someone debug it.
//!
//! A person reads the reply the turn leaves in their conversation: one
//! sentence of what happened, in their own terms, and at most what they may
//! do next. Anything else — the transport's error, the partial answer, the
//! provider's own exposed reasoning, the recovery instruction written for
//! the agent — is the run's working record, kept so a failure can be looked
//! at later, never shown as the reply. The two are separate values for that
//! reason: a caller that wants the person's sentence cannot get the record by
//! accident.

use agent_provider_config::protocol::{ErrorKind, ModelError};

const FAILURE_UNAVAILABLE: &str = "agent-failure-unavailable";
const FAILURE_NO_PROVIDER: &str = "agent-failure-no-provider";
const FAILURE_AUTHENTICATION: &str = "agent-failure-authentication";
const FAILURE_PERMISSION_DENIED: &str = "agent-failure-permission-denied";
const FAILURE_INVALID_REQUEST: &str = "agent-failure-invalid-request";
const FAILURE_CONFLICT: &str = "agent-failure-conflict";
const FAILURE_INVALID_RESPONSE: &str = "agent-failure-invalid-response";
const FAILURE_UNSUPPORTED: &str = "agent-failure-unsupported";
const FAILURE_RATE_LIMITED: &str = "agent-failure-rate-limited";
const FAILURE_CREDITS_EXHAUSTED: &str = "agent-failure-credits-exhausted";
const FAILURE_CONTEXT_LIMIT: &str = "agent-failure-context-limit";
const FAILURE_PROVIDER: &str = "agent-failure-provider";
const FAILURE_TIMED_OUT: &str = "agent-failure-timed-out";
const FAILURE_CANCELLED: &str = "agent-failure-cancelled";
const FAILURE_TRUNCATED: &str = "agent-failure-truncated";
const FAILURE_REFUSED: &str = "agent-failure-refused";
const FAILURE_TRANSPORT: &str = "agent-failure-transport";

/// The failure a turn ended in: what the person is told, and what the run
/// keeps for whoever looks at it later.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Failure {
    /// One sentence of what happened, and at most what the person may do
    /// next. Never the provider's words, never recovery instructions written
    /// for the agent, never model reasoning.
    message: String,
    /// The transport's error, the partial answer and the provider's exposed
    /// reasoning, as the turn left them: kept in the run's working record for
    /// debugging, never shown to people in the conversation.
    detail: String,
}

impl Failure {
    /// A failure the run itself decided, with no provider detail: the
    /// sentence is both what a person reads and the whole record.
    pub(crate) fn said(message: impl Into<String>) -> Self {
        let message = message.into();
        Self {
            detail: message.clone(),
            message,
        }
    }

    /// A model call that failed: the person's sentence says what happened in
    /// their terms and what they can do; the detail keeps the provider's
    /// error with the recovery the agent may take, for debugging only.
    pub(crate) fn model(error: &ModelError) -> Self {
        Self {
            message: crate::i18n::t(person_key(error.kind)),
            detail: format!("{error} {}", error.recovery().guidance()),
        }
    }

    /// A failure whose sentence a person reads already, with the run's own
    /// reason kept beside it as the record: the loop stopping a turn names the
    /// stop in words a person knows and the reason for whoever debugs it.
    pub(crate) fn spoken(said: &str, reason: &str) -> Self {
        Self {
            message: said.to_owned(),
            detail: reason.to_owned(),
        }
    }

    /// A model call whose transport failed: the person reads that the model
    /// could not be reached, and the transport's own error is the record.
    pub(crate) fn unreachable(error: impl Into<String>) -> Self {
        Self {
            message: crate::i18n::t(FAILURE_TRANSPORT),
            detail: error.into(),
        }
    }

    /// The person's sentence: what the reply says happened.
    #[must_use]
    pub(crate) fn message(&self) -> &str {
        &self.message
    }

    /// What the reply's work record keeps for debugging.
    #[must_use]
    pub(crate) fn detail(&self) -> &str {
        &self.detail
    }

    /// The record, with what the turn streamed before it ended, kept under
    /// labels: the partial answer and the provider's own exposed reasoning
    /// are for debugging, never the reply.
    #[must_use]
    pub(crate) fn retained(&self, partial: &str, reasoning: &str) -> Self {
        let mut detail = self.detail.clone();
        if !partial.is_empty() {
            detail.push_str("\nPartial answer (not completed):\n");
            detail.push_str(partial);
        }
        if !reasoning.is_empty() {
            detail.push_str("\nProvider-exposed summary:\n");
            detail.push_str(reasoning);
        }
        Self {
            message: self.message.clone(),
            detail,
        }
    }
}

/// The failure's record, as a log line reads it: the detail, never mistaken
/// for the sentence a person reads.
impl std::fmt::Display for Failure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.detail)
    }
}

/// The catalogue message for what a person reads when a model call failed.
#[must_use]
fn person_key(kind: ErrorKind) -> &'static str {
    match kind {
        ErrorKind::Unavailable => FAILURE_UNAVAILABLE,
        ErrorKind::NoProvider => FAILURE_NO_PROVIDER,
        ErrorKind::Authentication => FAILURE_AUTHENTICATION,
        ErrorKind::PermissionDenied => FAILURE_PERMISSION_DENIED,
        ErrorKind::InvalidRequest => FAILURE_INVALID_REQUEST,
        ErrorKind::Conflict => FAILURE_CONFLICT,
        ErrorKind::InvalidResponse => FAILURE_INVALID_RESPONSE,
        ErrorKind::Unsupported => FAILURE_UNSUPPORTED,
        ErrorKind::RateLimited => FAILURE_RATE_LIMITED,
        ErrorKind::CreditsExhausted => FAILURE_CREDITS_EXHAUSTED,
        ErrorKind::ContextLimit => FAILURE_CONTEXT_LIMIT,
        ErrorKind::Provider => FAILURE_PROVIDER,
        ErrorKind::TimedOut => FAILURE_TIMED_OUT,
        ErrorKind::Cancelled => FAILURE_CANCELLED,
        ErrorKind::Truncated => FAILURE_TRUNCATED,
        ErrorKind::Refused => FAILURE_REFUSED,
        ErrorKind::Transport => FAILURE_TRANSPORT,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The sentence a person reads never carries the record: no recovery
    /// instruction written for the agent, no partial answer, no reasoning.
    #[test]
    fn a_model_failure_keeps_its_record_out_of_the_persons_sentence() {
        for kind in [
            ErrorKind::TimedOut,
            ErrorKind::Transport,
            ErrorKind::Provider,
            ErrorKind::Authentication,
            ErrorKind::NoProvider,
        ] {
            let error = ModelError {
                kind,
                message: "Model request timed out".into(),
            };
            let failure = Failure::model(&error)
                .retained("Let me read r4, r9..", "Let's do it: 25,26,27,36,37,38,…");
            let message = failure.message();
            assert!(
                !message.contains("Retry inference from saved history"),
                "{message}"
            );
            assert!(
                !message.contains("do not replay completed actions"),
                "{message}"
            );
            assert!(!message.contains("Partial answer"), "{message}");
            assert!(!message.contains("Provider-exposed summary"), "{message}");
            assert!(!message.contains("Let me read"), "{message}");
            assert!(!message.contains("Let's do it"), "{message}");
            assert!(!message.contains(".."), "{message}");
            assert!(!message.contains('…'), "{message}");
            // One sentence: it names what happened, in the person's terms.
            assert!(message.ends_with('.'), "{message}");
            assert!(!message.contains('\n'), "{message}");
            // The record keeps everything the sentence leaves out.
            assert!(failure.detail().contains("Partial answer (not completed)"));
            assert!(failure.detail().contains("Provider-exposed summary"));
        }
        // A timeout's record keeps the recovery the agent itself may take,
        // which is never the person's sentence.
        let failure = Failure::model(&ModelError {
            kind: ErrorKind::TimedOut,
            message: "Model request timed out".into(),
        });
        assert!(
            failure
                .detail()
                .contains("the turn starts again from committed history"),
            "{}",
            failure.detail()
        );
    }

    /// A timeout reads as one plain sentence about time.
    #[test]
    fn a_timeout_reads_as_one_sentence_about_time() {
        let failure = Failure::model(&ModelError::new(ErrorKind::TimedOut));
        assert_eq!(failure.message(), "The model didn't answer in time.");
    }

    /// A configuration failure names the setting to look at.
    #[test]
    fn a_configuration_failure_names_the_setting() {
        let failure = Failure::model(&ModelError::new(ErrorKind::NoProvider));
        assert!(failure.message().contains("Settings"));
        assert!(failure.message().contains("Agent models"));
    }

    /// Every kind has its own sentence, and none tells the person to replay
    /// or continue anything only the agent decides.
    #[test]
    fn no_kind_tells_a_person_to_replay_or_continue() {
        for kind in [
            ErrorKind::Unavailable,
            ErrorKind::NoProvider,
            ErrorKind::Authentication,
            ErrorKind::PermissionDenied,
            ErrorKind::InvalidRequest,
            ErrorKind::Conflict,
            ErrorKind::InvalidResponse,
            ErrorKind::Unsupported,
            ErrorKind::RateLimited,
            ErrorKind::CreditsExhausted,
            ErrorKind::ContextLimit,
            ErrorKind::Provider,
            ErrorKind::TimedOut,
            ErrorKind::Cancelled,
            ErrorKind::Truncated,
            ErrorKind::Refused,
            ErrorKind::Transport,
        ] {
            let said = crate::i18n::t(person_key(kind));
            assert_ne!(said, person_key(kind), "{kind:?} has no sentence");
            for forbidden in ["replay", "inference", "partial", "reasoning", "history"] {
                assert!(
                    !said.to_lowercase().contains(forbidden),
                    "{kind:?} says {forbidden}: {said}"
                );
            }
        }
    }
}
