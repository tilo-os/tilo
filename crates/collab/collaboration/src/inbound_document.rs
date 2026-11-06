//! An inbound document op on its route: opened under the broker's verdict
//! for it, held while this device waits for the channel's keys, or refused,
//! and why.

use runtime_collab::OpAdmission;

use super::super::route_keys::Arrived;
use super::{CollabEvent, SharedRouteKeys, SignedOp};

/// Why an inbound document envelope was not delivered.
#[derive(Debug)]
pub(super) enum UndeliveredDocument {
    /// No key for it yet; it was held for when keys arrive.
    Held,
    /// Refused, and why.
    Refused(Refusal),
}

impl std::fmt::Display for UndeliveredDocument {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Held => f.write_str("held until this device has the channel's keys"),
            Self::Refused(error) => error.fmt(f),
        }
    }
}

/// An opened document op and the action commit its payload carries.
pub(super) struct SignedDocument {
    pub(super) event: CollabEvent,
    pub(super) commit: Option<collab_types::WorkspaceCommit>,
}

/// Open a live envelope into a document event, or hold it on the route
/// while this device waits for the channel's keys.
pub(super) fn signed_document_event(
    arrived: Arrived,
    encryption: &SharedRouteKeys,
) -> Result<SignedDocument, UndeliveredDocument> {
    match decrypt_document_payload(&arrived.envelope, encryption, arrived.admission) {
        Ok(decrypted_payload) => {
            // The commit rides inside the signed payload, which on a sealed
            // route is the plaintext only this device can open.
            let envelope = arrived.envelope;
            let cleartext = decrypted_payload.as_deref().unwrap_or(&envelope.payload);
            let commit =
                match collab_types::WorkspaceCommit::of_envelope_payload(&envelope, cleartext) {
                    Ok(commit) => commit,
                    Err(error) => {
                        return Err(UndeliveredDocument::Refused(Refusal::Uncommitted(error)));
                    }
                };
            Ok(SignedDocument {
                event: CollabEvent::Document {
                    envelope: Box::new(envelope),
                    from_user_id: arrived.from_user_id,
                    decrypted_payload,
                },
                commit,
            })
        }
        Err(DecryptFailure::Later) => {
            if let Ok(mut keys) = encryption.lock() {
                keys.hold_inbound(arrived);
            }
            Err(UndeliveredDocument::Held)
        }
        Err(DecryptFailure::Refused(error)) => Err(UndeliveredDocument::Refused(error)),
    }
}

pub(super) enum DecryptFailure {
    Later,
    Refused(Refusal),
}

/// Why an inbound document envelope is refused on its route.
#[derive(Debug)]
pub(super) enum Refusal {
    /// The route's keys were poisoned by a panic while held.
    KeysPoisoned,
    /// The clear storage class it was retained under does not describe
    /// what it contains.
    RetentionMismatch,
    /// The route can never open it.
    Unopened(super::super::route_keys::Never),
    /// Its document frame does not decode.
    Malformed(collab_types::DocumentMutationError),
    /// The action commit its payload carries does not check.
    Uncommitted(collab_types::CommitError),
}

impl std::fmt::Display for Refusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::KeysPoisoned => f.write_str("the route's keys are poisoned"),
            Self::RetentionMismatch => {
                f.write_str("retained under a storage class that does not describe it")
            }
            Self::Unopened(never) => never.fmt(f),
            Self::Malformed(error) => write!(f, "malformed document frame: {error}"),
            Self::Uncommitted(error) => write!(f, "unreadable action commit: {error}"),
        }
    }
}

/// Open an envelope's typed payload under the broker's `admission` for it.
pub(super) fn decrypt_document_payload(
    envelope: &SignedOp,
    encryption: &SharedRouteKeys,
    admission: OpAdmission,
) -> Result<Option<Vec<u8>>, DecryptFailure> {
    let refused = DecryptFailure::Refused;
    let malformed = |error| DecryptFailure::Refused(Refusal::Malformed(error));
    let opened = encryption
        .lock()
        .map_err(|_| refused(Refusal::KeysPoisoned))?
        .open(envelope, admission);
    match opened {
        Ok(Some(plaintext)) => {
            let plaintext = plaintext.to_vec();
            collab_types::DocumentFrame::decode(&plaintext).map_err(malformed)?;
            // The clear storage class hosts retained the op under must
            // describe what it actually contains.
            if envelope
                .encryption
                .and_then(|metadata| metadata.retention())
                .is_some_and(|retention| {
                    retention != collab_types::ContentRetention::of_frame(&plaintext)
                })
            {
                return Err(refused(Refusal::RetentionMismatch));
            }
            Ok(Some(plaintext))
        }
        Ok(None) => {
            collab_types::DocumentFrame::decode(&envelope.payload).map_err(malformed)?;
            Ok(None)
        }
        Err(super::super::route_keys::Unopened::Later) => Err(DecryptFailure::Later),
        Err(super::super::route_keys::Unopened::Never(never)) => {
            Err(refused(Refusal::Unopened(never)))
        }
    }
}
