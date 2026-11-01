//! One action sent to another device of the workspace, signed by the device
//! that sends it.
//!
//! The addressed device answers it on its own engine, with no host, store or
//! relay in between. The QUIC
//! connection already proves which device is on the other end, so why the
//! signature: an invocation recorded in the workspace has to prove to *every*
//! device who asked, not only to the one that answered.
//!
//! The signature covers the exact encoded action, so the workspace, the
//! request id, the source, the target and the message are all bound, together
//! with the issue time. A decoded value carries the action as claimed;
//! nothing about it is trusted until [`SignedDeviceAction::verify`] succeeds
//! against the source certificate's device key.

use serde::{Deserialize, Serialize};
use tilo_actions::remote::{RemoteActionFrame, RemoteActionMessage};
use uuid::Uuid;

/// Domain separator for a source device's signature over one directed action.
pub const ACTION_SIGNATURE_DOMAIN: &[u8] = b"tilo-device-action-v1\0";

/// How far a signed action's issue time may lie from the verifier's clock,
/// either way, before the action is refused as stale.
pub const ACTION_SIGNATURE_TOLERANCE_NS: u64 = 60 * 1_000_000_000;

/// Room for a whole remote-action message
/// ([`tilo_actions::remote::MAX_REMOTE_ACTION_FRAME_BYTES`]: a record piece
/// or a content chunk) plus its routing and signature.
pub const MAX_ACTION_BYTES: usize = tilo_actions::remote::MAX_REMOTE_ACTION_FRAME_BYTES + 16 * 1024;

/// Why a signed action was not produced or not accepted.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum ActionSignatureError {
    /// The action does not encode within the wire bound.
    #[error("the action does not encode within its bound")]
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
    /// The source device's signer failed, or returned no valid signature.
    #[error("the source device's signer failed")]
    Signer,
    /// The same signed action was already admitted within its window.
    #[error("the signed action was already admitted")]
    Replayed,
    /// The replay window is full of still-fresh actions.
    #[error("too many signed actions are still within their window")]
    ReplayCapacity,
}

impl ActionSignatureError {
    /// Stable refusal code a device returns to the caller.
    #[must_use]
    pub const fn code(self) -> &'static str {
        match self {
            Self::Encoding | Self::Signature | Self::Source => "action_signature_invalid",
            Self::Signer => "action_signer_failed",
            Self::Stale => "action_stale",
            Self::Replayed => "action_replayed",
            Self::ReplayCapacity => "action_replay_capacity",
        }
    }
}

/// One device's action message to another device of one workspace.
///
/// `workspace` is the workspace's room id as text: one request names its
/// workspace, and a device answers it only for a workspace it holds.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct DeviceAction {
    /// The workspace whose state the action is decided against.
    pub workspace: String,
    /// Names this request, so a retry after a lost answer is recognised.
    pub request_id: Uuid,
    /// The device that asks.
    pub source: [u8; 32],
    /// The device that answers: never a third one.
    pub target: [u8; 32],
    /// What is asked or answered, the payload apps and hosts already speak.
    ///
    /// As JSON bytes, like every transport carries it: the vocabulary holds
    /// payloads a binary codec cannot decode back, and its own frame version
    /// is checked before the message is read.
    #[serde(with = "message_frame")]
    pub message: RemoteActionMessage,
}

/// The action message as the bytes every transport carries it as.
mod message_frame {
    use super::{MAX_ACTION_BYTES, RemoteActionFrame, RemoteActionMessage};
    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S: Serializer>(
        message: &RemoteActionMessage,
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        let bytes = RemoteActionFrame::new(message.clone())
            .encode()
            .map_err(serde::ser::Error::custom)?;
        serializer.serialize_bytes(&bytes)
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(
        deserializer: D,
    ) -> Result<RemoteActionMessage, D::Error> {
        let bytes = serde_bytes::ByteBuf::deserialize(deserializer)?;
        if bytes.len() > MAX_ACTION_BYTES {
            return Err(serde::de::Error::custom("action message too large"));
        }
        RemoteActionFrame::decode(&bytes)
            .map(|frame| frame.message)
            .map_err(serde::de::Error::custom)
    }
}

/// A [`DeviceAction`] signed by its source device for its target.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(try_from = "SignedWire", into = "SignedWire")]
pub struct SignedDeviceAction {
    action: DeviceAction,
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

impl From<SignedDeviceAction> for SignedWire {
    fn from(signed: SignedDeviceAction) -> Self {
        Self {
            issued_at_ns: signed.issued_at_ns,
            action: signed.encoded,
            signature: signed.signature,
        }
    }
}

impl TryFrom<SignedWire> for SignedDeviceAction {
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

fn decode_exact(bytes: &[u8]) -> Result<DeviceAction, ActionSignatureError> {
    if bytes.len() > MAX_ACTION_BYTES {
        return Err(ActionSignatureError::Encoding);
    }
    match postcard::take_from_bytes::<DeviceAction>(bytes) {
        Ok((action, [])) => Ok(action),
        _ => Err(ActionSignatureError::Encoding),
    }
}

impl SignedDeviceAction {
    /// Sign `action` as its source device at `issued_at_ns`.
    ///
    /// # Errors
    ///
    /// [`ActionSignatureError::Source`] when `device` is not the action's
    /// source, [`ActionSignatureError::Encoding`] when it does not encode
    /// within the wire bound.
    pub fn sign(
        action: &DeviceAction,
        issued_at_ns: u64,
        device: &ed25519_dalek::SigningKey,
    ) -> Result<Self, ActionSignatureError> {
        let (encoded, message) =
            statement(action, issued_at_ns, &device.verifying_key().to_bytes())?;
        let signature = ed25519_dalek::Signer::sign(device, &message).to_bytes();
        Ok(Self {
            action: action.clone(),
            encoded,
            issued_at_ns,
            signature,
        })
    }

    /// The action as claimed. Trust it only after [`Self::verify`].
    #[must_use]
    pub const fn action(&self) -> &DeviceAction {
        &self.action
    }

    /// The source device's clock when it signed, in ns since the epoch.
    #[must_use]
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

    /// Whether the issue time is within [`ACTION_SIGNATURE_TOLERANCE_NS`] of
    /// `now_ns`.
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

/// The encoded action and the message its source device signs over it.
fn statement(
    action: &DeviceAction,
    issued_at_ns: u64,
    device_public_key: &[u8; 32],
) -> Result<(Vec<u8>, Vec<u8>), ActionSignatureError> {
    if action.source != *device_public_key {
        return Err(ActionSignatureError::Source);
    }
    let encoded = postcard::to_allocvec(action).map_err(|_| ActionSignatureError::Encoding)?;
    if encoded.len() > MAX_ACTION_BYTES {
        return Err(ActionSignatureError::Encoding);
    }
    let message = signing_message(issued_at_ns, &encoded);
    Ok((encoded, message))
}

fn signing_message(issued_at_ns: u64, encoded: &[u8]) -> Vec<u8> {
    let mut message = Vec::new();
    message.extend_from_slice(ACTION_SIGNATURE_DOMAIN);
    message.extend_from_slice(&issued_at_ns.to_le_bytes());
    message.extend_from_slice(blake3::hash(encoded).as_bytes());
    message
}

/// Signed actions a target admitted while they are still fresh, so a device
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
    #[must_use]
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
        signed: &SignedDeviceAction,
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
mod tests {
    use super::{
        ACTION_SIGNATURE_TOLERANCE_NS, ActionReplayWindow, ActionSignatureError, DeviceAction,
        SignedDeviceAction, SignedWire,
    };
    use tilo_actions::remote::{CatalogueQuery, RemoteActionMessage};

    fn key(seed: u8) -> ed25519_dalek::SigningKey {
        ed25519_dalek::SigningKey::from_bytes(&[seed; 32])
    }

    fn device(seed: u8) -> [u8; 32] {
        key(seed).verifying_key().to_bytes()
    }

    fn action() -> DeviceAction {
        DeviceAction {
            workspace: "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa".into(),
            request_id: uuid::Uuid::from_u128(9),
            source: key(0x21).verifying_key().to_bytes(),
            target: key(0x22).verifying_key().to_bytes(),
            message: RemoteActionMessage::CatalogueRequest {
                correlation: 3,
                query: CatalogueQuery::default(),
            },
        }
    }

    /// A signed action verifies under its source device and only there, and
    /// the exact bytes it signed survive a trip through the wire.
    #[test]
    fn a_signed_action_verifies_only_under_its_source() {
        let signed = SignedDeviceAction::sign(&action(), 1_000, &key(0x21)).unwrap();
        assert_eq!(signed.verify(&device(0x21)), Ok(()));
        assert_eq!(signed.action(), &action());
        let bytes = postcard::to_allocvec(&signed).unwrap();
        let back: SignedDeviceAction = postcard::from_bytes(&bytes).unwrap();
        assert_eq!(back, signed);
        assert_eq!(back.verify(&device(0x21)), Ok(()));
        assert_eq!(
            back.verify(&device(0x22)),
            Err(ActionSignatureError::Source)
        );
    }

    /// A device cannot sign for another one, and the signature covers the
    /// routing and the message alike: the old signature does not carry a
    /// changed action.
    #[test]
    fn a_forged_or_changed_action_is_refused() {
        assert_eq!(
            SignedDeviceAction::sign(&action(), 1_000, &key(0x22)),
            Err(ActionSignatureError::Source),
            "a device cannot sign for another source"
        );
        let signed = SignedDeviceAction::sign(&action(), 1_000, &key(0x21)).unwrap();
        let mut changed = action();
        changed.message = RemoteActionMessage::CatalogueRequest {
            correlation: 4,
            query: CatalogueQuery::default(),
        };
        // The signature of the first action, under the bytes of the second.
        let wire = SignedWire {
            issued_at_ns: signed.issued_at_ns(),
            action: postcard::to_allocvec(&changed).unwrap(),
            signature: signed.signature,
        };
        let tampered = SignedDeviceAction::try_from(wire).unwrap();
        assert_eq!(
            tampered.verify(&device(0x21)),
            Err(ActionSignatureError::Signature)
        );
        // Bytes past the action are not an action this build reads.
        let mut padded = SignedWire::from(signed);
        padded.action.push(0);
        assert_eq!(
            SignedDeviceAction::try_from(padded).err(),
            Some(ActionSignatureError::Encoding)
        );
    }

    /// A signature is only good inside its freshness window, and one action
    /// is admitted once.
    #[test]
    fn a_stale_or_replayed_action_is_refused() {
        let signed = SignedDeviceAction::sign(&action(), 1_000, &key(0x21)).unwrap();
        let mut window = ActionReplayWindow::default();
        assert_eq!(window.admit(&signed, 1_000), Ok(()));
        assert_eq!(
            window.admit(&signed, 1_000),
            Err(ActionSignatureError::Replayed)
        );
        assert_eq!(
            window.admit(&signed, 1_000 + ACTION_SIGNATURE_TOLERANCE_NS + 1),
            Err(ActionSignatureError::Stale)
        );
    }

    /// A full window of still-fresh actions refuses a new one rather than
    /// forgetting one that could still be replayed, and lets a lapsed entry
    /// go once it can no longer be replayed.
    #[test]
    fn a_full_window_refuses_rather_than_forgets() {
        let mut window = ActionReplayWindow::with_capacity(2);
        let sign = |correlation: u64, at: u64| {
            let mut action = action();
            action.message = RemoteActionMessage::CatalogueRequest {
                correlation,
                query: CatalogueQuery::default(),
            };
            SignedDeviceAction::sign(&action, at, &key(0x21)).unwrap()
        };
        let first = sign(1, 1_000);
        let second = sign(2, 1_000);
        let third = sign(3, 1_000);
        assert_eq!(window.admit(&first, 1_000), Ok(()));
        assert_eq!(window.admit(&second, 1_000), Ok(()));
        assert_eq!(
            window.admit(&third, 1_000),
            Err(ActionSignatureError::ReplayCapacity)
        );
        // Once the two admitted ones have lapsed they cannot be replayed, so
        // they make room for an action that is fresh when it arrives.
        let later = 1_000 + ACTION_SIGNATURE_TOLERANCE_NS + 1;
        assert_eq!(
            window.admit(&sign(4, later), later),
            Ok(()),
            "the entries that lapsed make room"
        );
    }
}
