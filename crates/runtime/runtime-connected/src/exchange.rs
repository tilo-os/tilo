use thiserror::Error;
use tilo_connected_protocol::{
    CodecError, DecodeStatus, Envelope, EnvelopeKind, FrameDecoder, encode_frame,
};

/// A local failure known to occur before request bytes were exposed.
#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum BeginFailure {
    /// Another exchange already owns the channel.
    #[error("an exchange is already in flight")]
    Busy,
    /// The candidate request was structurally invalid.
    #[error("invalid request: {0}")]
    InvalidRequest(CodecError),
    /// The local correlation counter cannot advance without reuse.
    #[error("correlation space exhausted")]
    CorrelationExhausted,
}

/// Failure to begin an exchange.
#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum BeginError {
    /// No bytes for this attempt were exposed to a transport.
    #[error("request definitely not sent: {0}")]
    DefinitelyNotSent(BeginFailure),
    /// This channel was poisoned and cannot be reused.
    #[error("exchange channel is unusable")]
    Unusable,
}

/// Complete bytes the caller may attempt to write once.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OutboundRequest {
    correlation: u64,
    frame: Vec<u8>,
}

impl OutboundRequest {
    /// Returns the stream-local correlation assigned before exposure.
    #[must_use]
    pub const fn correlation(&self) -> u64 {
        self.correlation
    }

    /// Returns the complete request frame.
    #[must_use]
    pub fn frame(&self) -> &[u8] {
        &self.frame
    }
}

/// A matching structural response whose domain payload remains unvalidated.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UnvalidatedResponse {
    envelope: Envelope,
}

impl UnvalidatedResponse {
    /// Returns structural envelope data only.
    #[must_use]
    pub const fn envelope(&self) -> &Envelope {
        &self.envelope
    }
}

/// Progress after one atomic response fragment.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FeedResponse {
    /// No complete response exists yet.
    Pending,
    /// Exactly one matching frame completed with no trailing bytes.
    Complete(UnvalidatedResponse),
}

/// Response processing failure.
#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum ExchangeError {
    /// This channel was poisoned and cannot be reused.
    #[error("exchange channel is unusable")]
    Unusable,
    /// Malformed, mismatched, or unsolicited data poisoned the channel.
    #[error("response protocol violation ({outcome:?})")]
    ProtocolViolation {
        /// Conservative operation classification at the poison boundary.
        outcome: TerminalOutcome,
    },
}

/// Conservative result of cancellation, transport failure, or terminal EOF.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TerminalOutcome {
    /// No request was active, but the channel is now poisoned.
    NoOperation,
    /// Bytes had been exposed, so the remote operation outcome is unknowable.
    OutcomeUnknown {
        /// Stream-local correlation of the ambiguous request.
        correlation: u64,
        /// Candidate type of the ambiguous request.
        message_type: String,
    },
    /// The channel was already poisoned.
    Unusable,
}

#[derive(Debug)]
enum State {
    Ready,
    InFlight {
        correlation: u64,
        message_type: String,
        decoder: FrameDecoder,
    },
    Poisoned {
        ambiguous: Option<AmbiguousOperation>,
    },
}

#[derive(Clone, Debug)]
struct AmbiguousOperation {
    correlation: u64,
    message_type: String,
}

impl AmbiguousOperation {
    fn outcome(&self) -> TerminalOutcome {
        TerminalOutcome::OutcomeUnknown {
            correlation: self.correlation,
            message_type: self.message_type.clone(),
        }
    }
}

/// One-owner, one-operation, no-I/O exchange state.
#[derive(Debug)]
pub struct ClientCore {
    state: State,
    next_correlation: Option<u64>,
}

impl Default for ClientCore {
    fn default() -> Self {
        Self::new()
    }
}

impl ClientCore {
    /// Creates a ready core whose first local correlation is one.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            state: State::Ready,
            next_correlation: Some(1),
        }
    }

    /// Validates and exposes one complete request while marking it in flight.
    ///
    /// # Errors
    ///
    /// Returns [`BeginError::DefinitelyNotSent`] for local validation,
    /// encoding, busy, or exhaustion failures, and [`BeginError::Unusable`]
    /// after the channel has been poisoned.
    pub fn begin(
        &mut self,
        message_type: &str,
        payload_json: &str,
    ) -> Result<OutboundRequest, BeginError> {
        match self.state {
            State::Poisoned { .. } => return Err(BeginError::Unusable),
            State::InFlight { .. } => {
                return Err(BeginError::DefinitelyNotSent(BeginFailure::Busy));
            }
            State::Ready => {}
        }
        let correlation = self.next_correlation.ok_or(BeginError::DefinitelyNotSent(
            BeginFailure::CorrelationExhausted,
        ))?;
        let envelope = Envelope::request(correlation, message_type, payload_json)
            .map_err(|error| BeginError::DefinitelyNotSent(BeginFailure::InvalidRequest(error)))?;
        let frame = encode_frame(&envelope)
            .map_err(|error| BeginError::DefinitelyNotSent(BeginFailure::InvalidRequest(error)))?;

        // Install InFlight before returning frame bytes. The core retains no
        // frame and therefore cannot queue, retry, or replay it automatically.
        self.state = State::InFlight {
            correlation,
            message_type: message_type.to_owned(),
            decoder: FrameDecoder::new(),
        };
        self.next_correlation = correlation.checked_add(1);
        Ok(OutboundRequest { correlation, frame })
    }

    /// Atomically consumes one response fragment.
    ///
    /// # Errors
    ///
    /// Returns [`ExchangeError::ProtocolViolation`] and poisons the channel
    /// for malformed, mismatched, or unsolicited data. A previously poisoned
    /// channel returns [`ExchangeError::Unusable`].
    pub fn feed_response(&mut self, input: &[u8]) -> Result<FeedResponse, ExchangeError> {
        let (correlation, message_type, mut decoder) =
            match std::mem::replace(&mut self.state, State::Poisoned { ambiguous: None }) {
                State::InFlight {
                    correlation,
                    message_type,
                    decoder,
                } => (correlation, message_type, decoder),
                State::Poisoned { ambiguous } => {
                    self.state = State::Poisoned { ambiguous };
                    return Err(ExchangeError::Unusable);
                }
                State::Ready => {
                    return Err(ExchangeError::ProtocolViolation {
                        outcome: TerminalOutcome::NoOperation,
                    });
                }
            };

        match decoder.push(input) {
            Ok(DecodeStatus::Pending { .. }) => {
                self.state = State::InFlight {
                    correlation,
                    message_type,
                    decoder,
                };
                Ok(FeedResponse::Pending)
            }
            Ok(DecodeStatus::Complete { envelope, consumed })
                if consumed == input.len()
                    && envelope.kind() == EnvelopeKind::Response
                    && envelope.correlation() == correlation
                    && envelope.message_type() == message_type =>
            {
                self.state = State::Ready;
                Ok(FeedResponse::Complete(UnvalidatedResponse { envelope }))
            }
            Ok(DecodeStatus::Complete { .. }) | Err(_) => {
                let ambiguous = AmbiguousOperation {
                    correlation,
                    message_type,
                };
                let outcome = ambiguous.outcome();
                self.state = State::Poisoned {
                    ambiguous: Some(ambiguous),
                };
                Err(ExchangeError::ProtocolViolation { outcome })
            }
        }
    }

    /// Poisons the channel after explicit local cancellation.
    pub fn cancel(&mut self) -> TerminalOutcome {
        self.terminate()
    }

    /// Poisons the channel after a driver reports transport failure.
    pub fn transport_failed(&mut self) -> TerminalOutcome {
        self.terminate()
    }

    /// Poisons the channel after a driver reports terminal end-of-input.
    pub fn end_of_input(&mut self) -> TerminalOutcome {
        self.terminate()
    }

    fn terminate(&mut self) -> TerminalOutcome {
        match std::mem::replace(&mut self.state, State::Poisoned { ambiguous: None }) {
            State::Ready => TerminalOutcome::NoOperation,
            State::InFlight {
                correlation,
                message_type,
                ..
            } => {
                let ambiguous = AmbiguousOperation {
                    correlation,
                    message_type,
                };
                let outcome = ambiguous.outcome();
                self.state = State::Poisoned {
                    ambiguous: Some(ambiguous),
                };
                outcome
            }
            State::Poisoned { ambiguous } => {
                let outcome = ambiguous
                    .as_ref()
                    .map_or(TerminalOutcome::Unusable, AmbiguousOperation::outcome);
                self.state = State::Poisoned { ambiguous };
                outcome
            }
        }
    }

    /// Reports whether this channel must be physically discarded.
    #[must_use]
    pub const fn is_poisoned(&self) -> bool {
        matches!(self.state, State::Poisoned { .. })
    }

    /// Returns the next correlation only while this channel remains usable.
    #[must_use]
    pub const fn next_correlation(&self) -> Option<u64> {
        if matches!(self.state, State::Poisoned { .. }) {
            None
        } else {
            self.next_correlation
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn correlation_exhaustion_never_wraps_or_reuses() {
        let mut core = ClientCore {
            state: State::Ready,
            next_correlation: Some(u64::MAX),
        };
        let outbound = core.begin("fixture.echo", "{}").expect("last correlation");
        assert_eq!(outbound.correlation(), u64::MAX);
        let response = Envelope::response(u64::MAX, "fixture.echo", "{}").expect("response");
        let frame = encode_frame(&response).expect("response frame");
        assert!(matches!(
            core.feed_response(&frame),
            Ok(FeedResponse::Complete(_))
        ));
        assert_eq!(core.next_correlation(), None);
        assert_eq!(
            core.begin("fixture.echo", "{}"),
            Err(BeginError::DefinitelyNotSent(
                BeginFailure::CorrelationExhausted
            ))
        );
    }
}
