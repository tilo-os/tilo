//! A private action signed by the device that sends it.
//!
//! The workspace host routes private actions and attaches the source device's
//! certificate, but certificates are public: a host that has seen one could
//! name that device as the source of an action it made up. The source device
//! therefore signs the exact action bytes with the key its certificate names,
//! and the target verifies that signature, so the host can forward an action
//! but cannot author or alter one. Freshness and replay are bounded by the
//! issue time the device signs and an [`ActionReplayWindow`] at the target.

use super::{DirectedAction, MAX_PRIVATE_BYTES};
use serde::{Deserialize, Serialize};

/// Domain separator for a source device's signature over one private action.
pub const ACTION_SIGNATURE_DOMAIN: &[u8] = b"tilo-peer-action-v1\0";

/// How far a signed action's issue time may lie from the verifier's clock,
/// either way, before the action is refused as stale.
pub const ACTION_SIGNATURE_TOLERANCE_NS: u64 = 60 * 1_000_000_000;

/// Why a signed action was not produced or not accepted.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum ActionSignatureError {
    /// The action does not encode within the private wire bound.
    #[error("the action does not encode within the private bound")]
    Encoding,
    /// The signature does not verify under the claimed source device key.
    #[error("the action is not signed by its source device")]
    Signature,
    /// The signed action names a different source device than the signer.
    #[error("the signed action names another source device")]
    Source,
    /// The issue time is outside [`ACTION_SIGNATURE_TOLERANCE_NS`] of now.
    #[error("the signed action is outside its freshness window")]
    Stale,
    /// The same signed action was already admitted within its window.
    #[error("the signed action was already admitted")]
    Replayed,
    /// The replay window is full of still-fresh actions.
    #[error("too many signed actions are still within their window")]
    ReplayCapacity,
}

impl ActionSignatureError {
    /// Stable refusal code a broker returns to the caller.
    pub const fn code(self) -> &'static str {
        match self {
            Self::Encoding | Self::Signature | Self::Source => "action_signature_invalid",
            Self::Stale => "action_stale",
            Self::Replayed => "action_replayed",
            Self::ReplayCapacity => "action_replay_capacity",
        }
    }
}

/// A [`DirectedAction`] signed by its source device for its target.
///
/// The signature covers the exact encoded action, so room, request id, source,
/// target and message are all bound, together with the issue time. A decoded
/// value carries the action as claimed; nothing about it is trusted until
/// [`Self::verify`] succeeds against the source certificate's device key.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(try_from = "SignedWire", into = "SignedWire")]
pub struct SignedDirectedAction {
    action: DirectedAction,
    encoded: Vec<u8>,
    issued_at_ns: u64,
    signature: [u8; 64],
}

/// The bytes on the wire: the action stays the exact bytes its source signed.
#[derive(Clone, Serialize, Deserialize)]
struct SignedWire {
    issued_at_ns: u64,
    #[serde(with = "serde_bytes")]
    action: Vec<u8>,
    #[serde(with = "serde_bytes")]
    signature: [u8; 64],
}

impl From<SignedDirectedAction> for SignedWire {
    fn from(signed: SignedDirectedAction) -> Self {
        Self {
            issued_at_ns: signed.issued_at_ns,
            action: signed.encoded,
            signature: signed.signature,
        }
    }
}

impl TryFrom<SignedWire> for SignedDirectedAction {
    type Error = ActionSignatureError;

    fn try_from(wire: SignedWire) -> Result<Self, Self::Error> {
        Ok(Self {
            action: decode_exact(&wire.action)?,
            encoded: wire.action,
            issued_at_ns: wire.issued_at_ns,
            signature: wire.signature,
        })
    }
}

fn decode_exact(bytes: &[u8]) -> Result<DirectedAction, ActionSignatureError> {
    if bytes.len() > MAX_PRIVATE_BYTES {
        return Err(ActionSignatureError::Encoding);
    }
    match postcard::take_from_bytes::<DirectedAction>(bytes) {
        Ok((action, [])) => Ok(action),
        _ => Err(ActionSignatureError::Encoding),
    }
}

impl SignedDirectedAction {
    /// Sign `action` as its source device at `issued_at_ns`.
    ///
    /// # Errors
    ///
    /// [`ActionSignatureError::Source`] when `device` is not the action's
    /// source, [`ActionSignatureError::Encoding`] when it does not encode
    /// within the private bound.
    pub fn sign(
        action: &DirectedAction,
        issued_at_ns: u64,
        device: &ed25519_dalek::SigningKey,
    ) -> Result<Self, ActionSignatureError> {
        if action.source != device.verifying_key().to_bytes() {
            return Err(ActionSignatureError::Source);
        }
        let encoded = postcard::to_allocvec(action).map_err(|_| ActionSignatureError::Encoding)?;
        if encoded.len() > MAX_PRIVATE_BYTES {
            return Err(ActionSignatureError::Encoding);
        }
        let signature =
            ed25519_dalek::Signer::sign(device, &signing_message(issued_at_ns, &encoded))
                .to_bytes();
        Ok(Self {
            action: action.clone(),
            encoded,
            issued_at_ns,
            signature,
        })
    }

    /// The action as claimed. Trust it only after [`Self::verify`].
    pub const fn action(&self) -> &DirectedAction {
        &self.action
    }

    /// The source device's clock when it signed, in ns since the epoch.
    pub const fn issued_at_ns(&self) -> u64 {
        self.issued_at_ns
    }

    /// Check that `device_public_key` signed exactly this action and is its
    /// source. Authority, freshness and replay are the caller's to decide.
    ///
    /// # Errors
    ///
    /// [`ActionSignatureError::Source`] when the action names another source,
    /// [`ActionSignatureError::Signature`] when the signature does not verify.
    pub fn verify(&self, device_public_key: &[u8; 32]) -> Result<(), ActionSignatureError> {
        if self.action.source != *device_public_key {
            return Err(ActionSignatureError::Source);
        }
        let key = ed25519_dalek::VerifyingKey::from_bytes(device_public_key)
            .map_err(|_| ActionSignatureError::Signature)?;
        key.verify_strict(
            &signing_message(self.issued_at_ns, &self.encoded),
            &ed25519_dalek::Signature::from_bytes(&self.signature),
        )
        .map_err(|_| ActionSignatureError::Signature)
    }

    /// Whether the issue time is within [`ACTION_SIGNATURE_TOLERANCE_NS`] of `now_ns`.
    ///
    /// # Errors
    ///
    /// [`ActionSignatureError::Stale`] when it is not.
    pub const fn check_fresh(&self, now_ns: u64) -> Result<(), ActionSignatureError> {
        if self.issued_at_ns.abs_diff(now_ns) > ACTION_SIGNATURE_TOLERANCE_NS {
            return Err(ActionSignatureError::Stale);
        }
        Ok(())
    }

    /// Names the signed statement, whatever signature encoding carried it.
    fn replay_key(&self) -> [u8; 32] {
        *blake3::hash(&signing_message(self.issued_at_ns, &self.encoded)).as_bytes()
    }
}

fn signing_message(issued_at_ns: u64, encoded: &[u8]) -> Vec<u8> {
    let mut message = Vec::new();
    message.extend_from_slice(ACTION_SIGNATURE_DOMAIN);
    message.extend_from_slice(&issued_at_ns.to_le_bytes());
    message.extend_from_slice(blake3::hash(encoded).as_bytes());
    message
}

/// Signed actions a target admitted while they are still fresh, so the host
/// that routes them cannot deliver one twice.
#[derive(Debug)]
pub struct ActionReplayWindow {
    seen: std::collections::HashMap<[u8; 32], u64>,
    capacity: usize,
}

impl Default for ActionReplayWindow {
    fn default() -> Self {
        Self::with_capacity(Self::CAPACITY)
    }
}

impl ActionReplayWindow {
    /// Most still-fresh actions remembered by default. A full window refuses
    /// new actions rather than forgetting ones that could still be replayed.
    pub const CAPACITY: usize = 16_384;

    /// A window remembering at most `capacity` still-fresh actions.
    pub fn with_capacity(capacity: usize) -> Self {
        Self {
            seen: std::collections::HashMap::new(),
            capacity,
        }
    }

    /// Admit a verified `signed` action at `now_ns`: fresh, and not admitted
    /// before. Verify the signature first; this only bounds time and repeats.
    ///
    /// # Errors
    ///
    /// [`ActionSignatureError::Stale`], [`ActionSignatureError::Replayed`] or
    /// [`ActionSignatureError::ReplayCapacity`].
    pub fn admit(
        &mut self,
        signed: &SignedDirectedAction,
        now_ns: u64,
    ) -> Result<(), ActionSignatureError> {
        signed.check_fresh(now_ns)?;
        let key = signed.replay_key();
        // An expired entry can stay until room is needed: its action is
        // refused as stale above before it could match here.
        if self.seen.contains_key(&key) {
            return Err(ActionSignatureError::Replayed);
        }
        if self.seen.len() >= self.capacity {
            self.seen
                .retain(|_, issued| issued.saturating_add(ACTION_SIGNATURE_TOLERANCE_NS) >= now_ns);
        }
        if self.seen.len() >= self.capacity {
            return Err(ActionSignatureError::ReplayCapacity);
        }
        self.seen.insert(key, signed.issued_at_ns);
        Ok(())
    }
}

#[cfg(test)]
mod tests;
