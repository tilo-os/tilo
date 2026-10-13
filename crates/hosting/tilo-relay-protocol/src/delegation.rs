//! Delegations: a person's root-signed statement that one of their devices
//! acts for them as an agent or as an installed bot, how it is shown, where
//! it may act and until when.
//!
//! Agents and bots hold the same kind of delegation, so one verification
//! rule serves both: the device's certificate root must have signed it,
//! it must name that device and the room, and it must be inside its
//! window. A bot's additionally names exactly one room and lasts at most
//! 30 days, because the room it was installed in is all it may touch and a
//! bot nobody renews should stop on its own.

use ed25519_dalek::{Signature, Signer, SigningKey, Verifier, VerifyingKey};
use serde::{Deserialize, Serialize};

/// Largest agent id and label.
pub const MAX_DELEGATE_TEXT_BYTES: usize = 64;
/// Rooms one delegation may name.
pub const MAX_DELEGATED_ROOMS: usize = 16;
/// Apps one delegation may name.
pub const MAX_DELEGATED_APPS: usize = 64;
/// Longest window of a bot delegation.
pub const MAX_BOT_DELEGATION_NS: u64 = 30 * 24 * 3600 * 1_000_000_000;

/// The signing domain. It changed with the delegate type, so a delegation
/// signed in the earlier agent-only shape never verifies.
const DELEGATION_DOMAIN: &[u8] = b"tilo-delegation-v2\0";

/// Who a delegation names.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
// Externally tagged: postcard, the canonical encoding, cannot decode an
// internally tagged enum.
#[serde(rename_all = "snake_case")]
pub enum Delegate {
    /// An agent.
    Agent {
        /// Stable id, 1..=[`MAX_DELEGATE_TEXT_BYTES`] bytes.
        agent_id: String,
    },
    /// An installed bot.
    Bot {
        /// Installation id: a lowercase hyphenated UUID.
        installation: String,
    },
}

impl Delegate {
    /// The agent id, when this names an agent.
    #[must_use]
    pub fn agent_id(&self) -> Option<&str> {
        match self {
            Self::Agent { agent_id } => Some(agent_id),
            Self::Bot { .. } => None,
        }
    }

    /// The installation id, when this names a bot.
    #[must_use]
    pub fn installation(&self) -> Option<&str> {
        match self {
            Self::Bot { installation } => Some(installation),
            Self::Agent { .. } => None,
        }
    }
}

/// Where a delegate may act.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DelegateScope {
    /// Room uuids, 1..=[`MAX_DELEGATED_ROOMS`]; exactly one for a bot.
    pub rooms: Vec<String>,
    /// App ids; empty means every app in those rooms.
    pub apps: Vec<String>,
}

/// A root-signed statement that `device_public_key` acts for
/// `user_public_key` as `delegate`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Delegation {
    /// The delegating person's root: an agent's person, a bot's installer.
    pub user_public_key: [u8; 32],
    /// The delegate's certified device key.
    pub device_public_key: [u8; 32],
    /// Whom the device acts as.
    pub delegate: Delegate,
    /// Display name, 1..=[`MAX_DELEGATE_TEXT_BYTES`] bytes.
    pub label: String,
    /// Cursor colour, sRGB.
    pub color: [u8; 3],
    /// Where it may act.
    pub scope: DelegateScope,
    /// Issue time, ns since the epoch.
    pub issued_at_ns: u64,
    /// Expiry, ns since the epoch. Delegations always expire.
    pub expires_at_ns: u64,
    /// Root signature over everything above.
    #[serde(with = "serde_bytes")]
    pub signature: [u8; 64],
}

/// Why a delegation is not usable.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum DelegationError {
    /// Signed by another root than the one expected.
    #[error("the delegation names another user")]
    WrongUser,
    /// The signature does not verify.
    #[error("the delegation signature does not verify")]
    Signature,
    /// Outside its validity window.
    #[error("the delegation has expired")]
    Expired,
    /// A field is out of bounds.
    #[error("invalid delegation: {0}")]
    Invalid(&'static str),
}

#[derive(Serialize)]
struct Unsigned<'a> {
    user_public_key: &'a [u8; 32],
    device_public_key: &'a [u8; 32],
    delegate: &'a Delegate,
    label: &'a str,
    color: &'a [u8; 3],
    scope: &'a DelegateScope,
    issued_at_ns: u64,
    expires_at_ns: u64,
}

fn is_installation_id(value: &str) -> bool {
    uuid::Uuid::parse_str(value).is_ok_and(|id| id.hyphenated().to_string() == value)
}

impl Delegation {
    /// Sign a delegation of `device_public_key` with the person's `root`.
    ///
    /// # Errors
    /// [`DelegationError::Invalid`] for out-of-bounds fields.
    pub fn sign(
        root: &SigningKey,
        device_public_key: [u8; 32],
        delegate: Delegate,
        label: impl Into<String>,
        color: [u8; 3],
        scope: DelegateScope,
        issued_at_ns: u64,
        expires_at_ns: u64,
    ) -> Result<Self, DelegationError> {
        let mut delegation = Self {
            user_public_key: root.verifying_key().to_bytes(),
            device_public_key,
            delegate,
            label: label.into(),
            color,
            scope,
            issued_at_ns,
            expires_at_ns,
            signature: [0; 64],
        };
        delegation.validate()?;
        delegation.signature = root.sign(&delegation.signing_bytes()).to_bytes();
        Ok(delegation)
    }

    fn signing_bytes(&self) -> Vec<u8> {
        let unsigned = Unsigned {
            user_public_key: &self.user_public_key,
            device_public_key: &self.device_public_key,
            delegate: &self.delegate,
            label: &self.label,
            color: &self.color,
            scope: &self.scope,
            issued_at_ns: self.issued_at_ns,
            expires_at_ns: self.expires_at_ns,
        };
        let mut bytes = DELEGATION_DOMAIN.to_vec();
        bytes.extend(postcard::to_allocvec(&unsigned).unwrap_or_default());
        bytes
    }

    fn validate(&self) -> Result<(), DelegationError> {
        let text = |value: &str| !value.is_empty() && value.len() <= MAX_DELEGATE_TEXT_BYTES;
        if !text(&self.label) {
            return Err(DelegationError::Invalid("label"));
        }
        match &self.delegate {
            Delegate::Agent { agent_id } if !text(agent_id) => {
                return Err(DelegationError::Invalid("agent id"));
            }
            Delegate::Bot { installation } if !is_installation_id(installation) => {
                return Err(DelegationError::Invalid("installation id"));
            }
            _ => {}
        }
        if self.scope.rooms.is_empty()
            || self.scope.rooms.len() > MAX_DELEGATED_ROOMS
            || self
                .scope
                .rooms
                .iter()
                .any(|room| crate::room_workspace_id(room).is_err())
        {
            return Err(DelegationError::Invalid("rooms"));
        }
        if self.scope.apps.len() > MAX_DELEGATED_APPS
            || self
                .scope
                .apps
                .iter()
                .any(|app| app.is_empty() || app.len() > 128)
        {
            return Err(DelegationError::Invalid("apps"));
        }
        if self.expires_at_ns <= self.issued_at_ns {
            return Err(DelegationError::Invalid("validity window"));
        }
        if let Delegate::Bot { .. } = self.delegate {
            if self.scope.rooms.len() != 1 {
                return Err(DelegationError::Invalid(
                    "a bot delegation names exactly one room",
                ));
            }
            if self.expires_at_ns.saturating_sub(self.issued_at_ns) > MAX_BOT_DELEGATION_NS {
                return Err(DelegationError::Invalid(
                    "a bot delegation lasts at most 30 days",
                ));
            }
        }
        Ok(())
    }

    /// Check the delegation was signed by `user_public_key` and is valid at
    /// `now_ns`.
    ///
    /// # Errors
    /// The first reason it is not usable.
    pub fn verify(&self, user_public_key: &[u8; 32], now_ns: u64) -> Result<(), DelegationError> {
        if &self.user_public_key != user_public_key {
            return Err(DelegationError::WrongUser);
        }
        self.validate()?;
        let key =
            VerifyingKey::from_bytes(user_public_key).map_err(|_| DelegationError::Signature)?;
        key.verify(
            &self.signing_bytes(),
            &Signature::from_bytes(&self.signature),
        )
        .map_err(|_| DelegationError::Signature)?;
        if now_ns < self.issued_at_ns || now_ns >= self.expires_at_ns {
            return Err(DelegationError::Expired);
        }
        Ok(())
    }

    /// Whether the delegate may be in relay workspace `workspace_id`.
    #[must_use]
    pub fn permits_workspace(&self, workspace_id: u64) -> bool {
        self.scope
            .rooms
            .iter()
            .any(|room| crate::room_workspace_id(room).is_ok_and(|id| id == workspace_id))
    }

    /// Whether the delegation names room `room`.
    #[must_use]
    pub fn permits_room(&self, room: &str) -> bool {
        crate::room_workspace_id(room).is_ok_and(|id| self.permits_workspace(id))
    }

    /// Whether the delegate may act in `app_id`.
    #[must_use]
    pub fn permits_app(&self, app_id: &str) -> bool {
        self.scope.apps.is_empty() || self.scope.apps.iter().any(|app| app == app_id)
    }

    /// Canonical encoding, for carrying the delegation to a browser.
    #[must_use]
    pub fn encode(&self) -> Vec<u8> {
        postcard::to_allocvec(self).unwrap_or_default()
    }

    /// Decode [`Self::encode`] output. The caller still verifies it.
    ///
    /// # Errors
    /// [`DelegationError::Invalid`] for undecodable bytes.
    pub fn decode(bytes: &[u8]) -> Result<Self, DelegationError> {
        postcard::from_bytes(bytes).map_err(|_| DelegationError::Invalid("encoding"))
    }
}

/// What a person asks a delegate to be allowed before the approver signs:
/// everything a [`Delegation`] states except the device and the times.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DelegateProposal {
    /// Whom the delegation will name.
    pub delegate: Delegate,
    /// Display name.
    pub label: String,
    /// Cursor colour, sRGB.
    pub color: [u8; 3],
    /// Where it may act.
    pub scope: DelegateScope,
    /// Latest expiry asked for, ns since the epoch.
    pub expires_at_ns: u64,
}

impl DelegateProposal {
    /// The delegation of this proposal for `device_public_key`, signed with
    /// the person's `root`.
    ///
    /// # Errors
    /// [`DelegationError::Invalid`] for out-of-bounds fields.
    pub fn sign(
        &self,
        root: &SigningKey,
        device_public_key: [u8; 32],
        now_ns: u64,
    ) -> Result<Delegation, DelegationError> {
        Delegation::sign(
            root,
            device_public_key,
            self.delegate.clone(),
            self.label.clone(),
            self.color,
            self.scope.clone(),
            now_ns,
            self.expires_at_ns,
        )
    }

    /// Whether `delegation` grants no more than this proposal: the same
    /// delegate, label and colour, rooms and apps within it, and an expiry
    /// no later.
    ///
    /// # Errors
    /// What is wider than proposed.
    pub fn admits(&self, delegation: &Delegation) -> Result<(), &'static str> {
        if delegation.delegate != self.delegate
            || delegation.label != self.label
            || delegation.color != self.color
        {
            return Err("the delegation names another delegate than proposed");
        }
        let scope = &delegation.scope;
        if !scope
            .rooms
            .iter()
            .all(|room| self.scope.rooms.contains(room))
        {
            return Err("the delegation names a room that was not proposed");
        }
        if !self.scope.apps.is_empty()
            && (scope.apps.is_empty()
                || !scope.apps.iter().all(|app| self.scope.apps.contains(app)))
        {
            return Err("the delegation names an app that was not proposed");
        }
        if delegation.expires_at_ns > self.expires_at_ns {
            return Err("the delegation outlasts the proposal");
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ROOM: &str = "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa";
    const OTHER_ROOM: &str = "bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb";
    const INSTALLATION: &str = "6f1c2a8e-3b4d-4c5e-9f60-718293a4b5c6";
    const DAY_NS: u64 = 24 * 3600 * 1_000_000_000;

    fn root() -> SigningKey {
        SigningKey::from_bytes(&[1; 32])
    }

    fn scope(rooms: &[&str]) -> DelegateScope {
        DelegateScope {
            rooms: rooms.iter().map(|room| (*room).to_owned()).collect(),
            apps: Vec::new(),
        }
    }

    fn bot(rooms: &[&str], issued: u64, expires: u64) -> Result<Delegation, DelegationError> {
        Delegation::sign(
            &root(),
            [9; 32],
            Delegate::Bot {
                installation: INSTALLATION.into(),
            },
            "Discord Bridge",
            [88, 101, 242],
            scope(rooms),
            issued,
            expires,
        )
    }

    #[test]
    fn an_agent_delegation_round_trips_and_verifies() {
        let delegation = Delegation::sign(
            &root(),
            [8; 32],
            Delegate::Agent {
                agent_id: "scribe".into(),
            },
            "Scribe",
            [10, 20, 30],
            scope(&[ROOM, OTHER_ROOM]),
            10,
            20,
        )
        .unwrap();
        let decoded = Delegation::decode(&delegation.encode()).unwrap();
        assert_eq!(decoded, delegation);
        decoded
            .verify(&root().verifying_key().to_bytes(), 15)
            .unwrap();
        assert_eq!(decoded.delegate.agent_id(), Some("scribe"));
        assert_eq!(decoded.delegate.installation(), None);
        assert!(decoded.permits_room(OTHER_ROOM));
        assert_eq!(
            decoded.verify(&root().verifying_key().to_bytes(), 20),
            Err(DelegationError::Expired)
        );
        assert_eq!(
            decoded.verify(&[2; 32], 15),
            Err(DelegationError::WrongUser)
        );
    }

    #[test]
    fn a_bot_delegation_names_one_room_for_at_most_thirty_days() {
        bot(&[ROOM], DAY_NS, DAY_NS + MAX_BOT_DELEGATION_NS).unwrap();
        assert_eq!(
            bot(&[ROOM], DAY_NS, DAY_NS + MAX_BOT_DELEGATION_NS + 1),
            Err(DelegationError::Invalid(
                "a bot delegation lasts at most 30 days"
            ))
        );
        assert_eq!(
            bot(&[ROOM, OTHER_ROOM], DAY_NS, 2 * DAY_NS),
            Err(DelegationError::Invalid(
                "a bot delegation names exactly one room"
            ))
        );
        let not_uuid = Delegation::sign(
            &root(),
            [9; 32],
            Delegate::Bot {
                installation: "Discord-Bridge".into(),
            },
            "Discord Bridge",
            [0; 3],
            scope(&[ROOM]),
            1,
            2,
        );
        assert_eq!(not_uuid, Err(DelegationError::Invalid("installation id")));
    }

    #[test]
    fn a_delegation_signed_in_the_old_shape_never_verifies() {
        // The agent shape signed `tilo-agent-delegation-v1\0` over a struct
        // with a bare agent id. A signature over those bytes must not verify
        // as a delegation of the new type.
        #[derive(serde::Serialize)]
        struct OldUnsigned<'a> {
            user_public_key: &'a [u8; 32],
            agent_device_public_key: &'a [u8; 32],
            agent_id: &'a str,
            label: &'a str,
            color: &'a [u8; 3],
            scope: &'a DelegateScope,
            issued_at_ns: u64,
            expires_at_ns: u64,
        }
        let root = root();
        let user = root.verifying_key().to_bytes();
        let scope = scope(&[ROOM]);
        let mut old = b"tilo-agent-delegation-v1\0".to_vec();
        old.extend(
            postcard::to_allocvec(&OldUnsigned {
                user_public_key: &user,
                agent_device_public_key: &[8; 32],
                agent_id: "scribe",
                label: "Scribe",
                color: &[10, 20, 30],
                scope: &scope,
                issued_at_ns: 10,
                expires_at_ns: 20,
            })
            .unwrap(),
        );
        let forged = Delegation {
            user_public_key: user,
            device_public_key: [8; 32],
            delegate: Delegate::Agent {
                agent_id: "scribe".into(),
            },
            label: "Scribe".into(),
            color: [10, 20, 30],
            scope,
            issued_at_ns: 10,
            expires_at_ns: 20,
            signature: root.sign(&old).to_bytes(),
        };
        assert_eq!(forged.verify(&user, 15), Err(DelegationError::Signature));
    }

    #[test]
    fn changing_the_delegate_breaks_the_signature() {
        let mut delegation = bot(&[ROOM], 1, 2).unwrap();
        delegation.delegate = Delegate::Bot {
            installation: "7f1c2a8e-3b4d-4c5e-9f60-718293a4b5c6".into(),
        };
        assert_eq!(
            delegation.verify(&root().verifying_key().to_bytes(), 1),
            Err(DelegationError::Signature)
        );
    }

    #[test]
    fn a_proposal_admits_only_what_it_proposed() {
        let proposal = DelegateProposal {
            delegate: Delegate::Bot {
                installation: INSTALLATION.into(),
            },
            label: "Discord Bridge".into(),
            color: [88, 101, 242],
            scope: scope(&[ROOM]),
            expires_at_ns: 10 * DAY_NS,
        };
        let signed = proposal.sign(&root(), [9; 32], DAY_NS).unwrap();
        proposal.admits(&signed).unwrap();
        let mut other = proposal.clone();
        other.delegate = Delegate::Agent {
            agent_id: "scribe".into(),
        };
        assert!(other.admits(&signed).is_err());
        let later = bot(&[ROOM], DAY_NS, 11 * DAY_NS).unwrap();
        assert!(proposal.admits(&later).is_err());
    }
}
