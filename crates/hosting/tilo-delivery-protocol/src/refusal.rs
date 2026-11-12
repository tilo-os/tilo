//! Why an action sent to a device could not be delivered.
//!
//! A v1 `RemoteActionUndeliverable` leads with [`UnavailableReason::wire_token`]
//! so a client can branch on the reason without the envelope changing shape.

use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum UnavailableReason {
    Offline,
    UnsupportedPeer,
    Backpressure,
    NotAuthorised,
    InvalidRequest,
    /// Durable admission has not been installed. Never a terminal result.
    AdmissionUnavailable,
}

impl UnavailableReason {
    /// Every reason, for exhaustive mappings and their tests.
    pub const ALL: [Self; 6] = [
        Self::Offline,
        Self::UnsupportedPeer,
        Self::Backpressure,
        Self::NotAuthorised,
        Self::InvalidRequest,
        Self::AdmissionUnavailable,
    ];

    /// Stable token that leads the free-form `reason` of a v1
    /// `RemoteActionUndeliverable`, so a client can branch on it without
    /// the frozen v1 envelope changing shape. The relay writes
    /// `"<token>"` or `"<token>: <detail>"`.
    pub const fn wire_token(self) -> &'static str {
        match self {
            Self::Offline => "offline",
            Self::UnsupportedPeer => "unsupported-peer",
            Self::Backpressure => tilo_relay_protocol::refusal::BACKPRESSURE,
            Self::NotAuthorised => "not-authorised",
            Self::InvalidRequest => "invalid-request",
            Self::AdmissionUnavailable => "admission-unavailable",
        }
    }

    /// The reason whose [`wire_token`](Self::wire_token) is exactly `token`.
    pub fn from_wire_token(token: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|reason| reason.wire_token() == token)
    }

    /// The reason a v1 `RemoteActionUndeliverable.reason` leads with
    /// (`"<token>"` or `"<token>: <detail>"`); `None` for a reason written
    /// by a relay that predates the tokens.
    pub fn from_wire_reason(reason: &str) -> Option<Self> {
        let token = reason.split_once(':').map_or(reason, |(token, _)| token);
        Self::from_wire_token(token.trim())
    }
}

#[cfg(test)]
mod tests {
    use super::UnavailableReason;

    #[test]
    fn wire_token_round_trips_every_reason() {
        let mut seen = std::collections::HashSet::new();
        for reason in UnavailableReason::ALL {
            let token = reason.wire_token();
            assert!(seen.insert(token), "{token} names one reason only");
            assert_eq!(UnavailableReason::from_wire_token(token), Some(reason));
            assert_eq!(UnavailableReason::from_wire_reason(token), Some(reason));
            assert_eq!(
                UnavailableReason::from_wire_reason(&format!("{token}: some detail")),
                Some(reason)
            );
        }
        assert_eq!(
            UnavailableReason::from_wire_reason("this host does not serve apps"),
            None,
            "a pre-token reason is not guessed"
        );
        assert_eq!(
            UnavailableReason::from_wire_token("not-authorised: x"),
            None
        );
    }
}
