//! The keys one route's endpoint holds for the content its record references.
//!
//! The record's changes go to the device's store, which seals and signs them.
//! What the record references (a picture's bytes) is stored by the app, sealed
//! under a key derived from the content secret of the members who read the
//! record, and private fields are sealed under the record's document keys. A
//! route of a certified app asks the workspace identity service for those keys
//! when it joins and keeps asking until they arrive; until then the route is
//! *waiting*, and what it stores is sealed under a key of its own.
//!
//! Once a route holds keys it asks again only when told its channel changed
//! (`groupKeysChanged`); see `broker::key_schedule::KeySchedule`.
//!
//! Only an endpoint with no identity service at all (an explicit test
//! harness) holds none, and only until a group is installed on it.

use super::KeyAccessState;

pub(super) enum Keys {
    /// No identity service: no keys.
    Plain,
    /// Channel keys requested and not held.
    Pending(KeyAccessState),
    /// A group installed explicitly on this endpoint.
    Group(Box<key_group::KeyGroup>),
    /// Content keys from the identity service, at a service generation.
    Content {
        keys: key_group::ContentKeys,
        generation: u64,
    },
}

/// A route's keys, and what the app was last told of them.
pub(super) struct RouteKeys {
    pub(super) keys: Keys,
    reported: Option<KeyAccessState>,
    /// The channel service answered the last question about these keys, so
    /// it tells the route when that answer may change. False until the
    /// first answer, and after a question the identity service refused or
    /// could not take.
    answered: bool,
}

impl RouteKeys {
    /// Keys of a route whose endpoint has an identity service to ask.
    pub(super) fn requested() -> Self {
        Self::with(Keys::Pending(KeyAccessState::WaitingForAccess))
    }

    /// Keys of a harness route with no identity service.
    pub(super) fn plain() -> Self {
        Self::with(Keys::Plain)
    }

    fn with(keys: Keys) -> Self {
        Self {
            keys,
            reported: None,
            answered: false,
        }
    }

    /// Whether this endpoint asks the identity service for keys.
    pub(super) fn asks_identity_service(&self) -> bool {
        matches!(self.keys, Keys::Pending(_) | Keys::Content { .. })
    }

    /// Whether this route asks the identity service and holds no keys yet:
    /// it waits for access, was removed, or the service is unavailable.
    pub(super) fn awaiting_keys(&self) -> bool {
        matches!(self.keys, Keys::Pending(_))
    }

    /// Whether the endpoint asks about this route on its timer while the
    /// broker pushes channel changes: it holds no keys, and either nothing
    /// will tell it when that changes (no answer from the channel service
    /// yet) or it was removed, whose rejoin each question asks for again.
    pub(super) fn asks_on_timer(&self) -> bool {
        match self.keys {
            Keys::Pending(KeyAccessState::Removed) => true,
            Keys::Pending(_) => !self.answered,
            _ => false,
        }
    }

    /// Record whether the channel service answered the last question.
    pub(super) fn set_answered(&mut self, answered: bool) {
        self.answered = answered;
    }

    /// The service generation of the keys held, 0 when none.
    pub(super) fn generation(&self) -> u64 {
        match &self.keys {
            Keys::Content { generation, .. } => *generation,
            _ => 0,
        }
    }

    pub(super) fn state(&self) -> KeyAccessState {
        match &self.keys {
            Keys::Plain => KeyAccessState::Unencrypted,
            Keys::Pending(state) => *state,
            Keys::Group(_) | Keys::Content { .. } => KeyAccessState::Encrypted,
        }
    }

    /// The content secret of the members these keys belong to
    /// ([`key_group::ContentKeys::document_content_secret`]), while
    /// the current epoch's key is held.
    pub(super) fn content_secret(&self) -> Option<tilo_seal::SymmetricKey> {
        match &self.keys {
            Keys::Content { keys, .. } => keys.document_content_secret().ok(),
            Keys::Group(group) => group.content_keys().ok()?.document_content_secret().ok(),
            Keys::Plain | Keys::Pending(_) => None,
        }
    }

    /// Keys retained for fields whose plaintext must stay out of the local document store.
    pub(super) fn document_keys(&self) -> Option<key_group::ContentKeys> {
        match &self.keys {
            Keys::Content { keys, .. } => Some(keys.clone()),
            Keys::Group(group) => group.content_keys().ok(),
            Keys::Plain | Keys::Pending(_) => None,
        }
    }

    /// The state, once per change, for the app.
    pub(super) fn take_state_change(&mut self) -> Option<KeyAccessState> {
        let state = self.state();
        (self.reported != Some(state)).then(|| {
            self.reported = Some(state);
            state
        })
    }

    /// Install an explicitly provisioned group.
    pub(super) fn install_group(&mut self, group: key_group::KeyGroup) {
        self.keys = Keys::Group(Box::new(group));
    }

    /// Apply what the identity service answered.
    pub(super) fn update(
        &mut self,
        access: tilo_identity_client::wire::KeyAccess,
        generation: u64,
        encoded: Option<&[u8]>,
    ) {
        use tilo_identity_client::wire::KeyAccess as Wire;
        match access {
            Wire::Ready => {
                if let Some(encoded) = encoded {
                    match key_group::ContentKeys::from_bytes(encoded) {
                        Ok(keys) => self.keys = Keys::Content { keys, generation },
                        Err(error) => {
                            tracing::warn!(%error, "collab: identity service sent malformed channel keys");
                        }
                    }
                }
            }
            Wire::Waiting => self.keys = Keys::Pending(KeyAccessState::WaitingForAccess),
            Wire::Removed => self.keys = Keys::Pending(KeyAccessState::Removed),
            Wire::Unavailable => self.keys = Keys::Pending(KeyAccessState::Unavailable),
            Wire::NoSecretStore => self.keys = Keys::Pending(KeyAccessState::NoSecretStore),
        }
    }
}
