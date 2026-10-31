//! Task delegations: a device's signed statement that a key it minted for
//! one task acts for the device's person in one room, for that task only,
//! until the task ends (at most a day).
//!
//! A task agent is made when a person asks (Super+C, the palette, a context
//! menu): no prior admission, no membership row. Its authority is this
//! statement, signed by the asking *device* rather than the person's root,
//! so any device the person uses can make one. It verifies as a chain: the
//! device's root-signed certificate is in the room's device list (the
//! caller looks it up and checks it is neither revoked nor expired), and the
//! device signed the delegation inside the certificate's window. A device
//! already acts as its person in full, so a narrower agent it mints widens
//! nothing.
//!
//! The encoding and the verification follow [`crate::delegation`]: postcard
//! over a domain-separated unsigned view, one `verify` that checks the
//! signer, the bounds, the signature and the window.

use ed25519_dalek::{Signature, Signer, SigningKey, Verifier, VerifyingKey};
use serde::{Deserialize, Serialize};
use tilo_user_identity::DeviceCert;

use crate::delegation::{DelegationError, MAX_DELEGATE_TEXT_BYTES};

/// Longest window of a task delegation: a task agent nobody finishes stops
/// on its own within a day.
pub const MAX_TASK_DELEGATION_NS: u64 = 24 * 3600 * 1_000_000_000;
/// Longest work group, the task's address.
pub const MAX_WORK_GROUP_BYTES: usize = 256;
/// What every task agent's id starts with: a task agent is never a
/// collaborator, whose ids the room's governance admits.
pub const TASK_AGENT_PREFIX: &str = "task-";

const TASK_DELEGATION_DOMAIN: &[u8] = b"tilo-task-delegation-v1\0";

/// A device-signed statement that `agent_public_key` acts for
/// `user_public_key` in `room`, on the task `work_group`, as `agent_id`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskDelegation {
    /// The person the agent acts for: the signing device's root.
    pub user_public_key: [u8; 32],
    /// The asking device, whose key signed this.
    pub device_public_key: [u8; 32],
    /// The key the device minted for the task.
    pub agent_public_key: [u8; 32],
    /// The task agent's id, [`TASK_AGENT_PREFIX`] and the agent key's first
    /// six bytes in hex ([`task_agent_id`]).
    pub agent_id: String,
    /// The task's work group: its issue's address.
    pub work_group: String,
    /// The room uuid it may act in.
    pub room: String,
    /// Issue time, ns since the epoch.
    pub issued_at_ns: u64,
    /// Expiry, ns since the epoch: when the task does, at most a day on.
    pub expires_at_ns: u64,
    /// The device's signature over everything above.
    #[serde(with = "serde_bytes")]
    pub signature: [u8; 64],
}

#[derive(Serialize)]
struct Unsigned<'a> {
    user_public_key: &'a [u8; 32],
    device_public_key: &'a [u8; 32],
    agent_public_key: &'a [u8; 32],
    agent_id: &'a str,
    work_group: &'a str,
    room: &'a str,
    issued_at_ns: u64,
    expires_at_ns: u64,
}

/// The id of the task agent holding `agent_public_key`: derived from the
/// key, so two tasks never share an agent and nobody picks a collaborator's
/// name for one.
#[must_use]
pub fn task_agent_id(agent_public_key: &[u8; 32]) -> String {
    let mut id = String::from(TASK_AGENT_PREFIX);
    id.push_str(&hex_of(&agent_public_key[..6]));
    id
}

/// `bytes` as lowercase hex.
fn hex_of(bytes: &[u8]) -> String {
    bytes
        .iter()
        .flat_map(|byte| [byte >> 4, byte & 0x0f])
        .filter_map(|nibble| char::from_digit(u32::from(nibble), 16))
        .collect()
}

/// Whether `agent_id` names a task agent rather than a collaborator.
#[must_use]
pub fn is_task_agent(agent_id: &str) -> bool {
    agent_id.starts_with(TASK_AGENT_PREFIX)
}

impl TaskDelegation {
    /// Sign a delegation of `agent_public_key` for `work_group` in `room`
    /// with `device`, the asking device whose root is `user_public_key`.
    ///
    /// # Errors
    /// [`DelegationError::Invalid`] for out-of-bounds fields.
    pub fn sign(
        device: &SigningKey,
        user_public_key: [u8; 32],
        agent_public_key: [u8; 32],
        work_group: impl Into<String>,
        room: impl Into<String>,
        issued_at_ns: u64,
        expires_at_ns: u64,
    ) -> Result<Self, DelegationError> {
        let mut delegation = Self {
            user_public_key,
            device_public_key: device.verifying_key().to_bytes(),
            agent_public_key,
            agent_id: task_agent_id(&agent_public_key),
            work_group: work_group.into(),
            room: room.into(),
            issued_at_ns,
            expires_at_ns,
            signature: [0; 64],
        };
        delegation.validate()?;
        delegation.signature = device.sign(&delegation.signing_bytes()).to_bytes();
        Ok(delegation)
    }

    fn signing_bytes(&self) -> Vec<u8> {
        let unsigned = Unsigned {
            user_public_key: &self.user_public_key,
            device_public_key: &self.device_public_key,
            agent_public_key: &self.agent_public_key,
            agent_id: &self.agent_id,
            work_group: &self.work_group,
            room: &self.room,
            issued_at_ns: self.issued_at_ns,
            expires_at_ns: self.expires_at_ns,
        };
        let mut bytes = TASK_DELEGATION_DOMAIN.to_vec();
        bytes.extend(postcard::to_allocvec(&unsigned).unwrap_or_default());
        bytes
    }

    fn validate(&self) -> Result<(), DelegationError> {
        if self.agent_id != task_agent_id(&self.agent_public_key)
            || self.agent_id.len() > MAX_DELEGATE_TEXT_BYTES
        {
            return Err(DelegationError::Invalid("agent id"));
        }
        if self.work_group.is_empty()
            || self.work_group.len() > MAX_WORK_GROUP_BYTES
            || self.work_group.chars().any(char::is_control)
        {
            return Err(DelegationError::Invalid("work group"));
        }
        if uuid::Uuid::parse_str(&self.room)
            .map_or(true, |room| room.hyphenated().to_string() != self.room)
        {
            return Err(DelegationError::Invalid("room"));
        }
        if self.expires_at_ns <= self.issued_at_ns {
            return Err(DelegationError::Invalid("validity window"));
        }
        if self.expires_at_ns.saturating_sub(self.issued_at_ns) > MAX_TASK_DELEGATION_NS {
            return Err(DelegationError::Invalid(
                "a task delegation lasts at most a day",
            ));
        }
        Ok(())
    }

    /// Check the chain at `now_ns`: `device` is the signing device's
    /// root-signed certificate, valid now, and it signed this delegation for
    /// `room` inside the certificate's window and its own.
    ///
    /// The caller has taken `device` from the room's device list and checked
    /// it is not revoked; this checks everything the two signatures state.
    ///
    /// # Errors
    /// [`DelegationError::WrongUser`] when the certificate is another device's
    /// or another person's, [`DelegationError::Uncertified`] when it does not
    /// verify now, [`DelegationError::Signature`] for a bad signature,
    /// [`DelegationError::Expired`] outside the window and
    /// [`DelegationError::Invalid`] for another room or bad bounds.
    pub fn verify(
        &self,
        device: &DeviceCert,
        room: &str,
        now_ns: u64,
    ) -> Result<(), DelegationError> {
        if device.device_public_key != self.device_public_key
            || device.user_public_key != self.user_public_key
        {
            return Err(DelegationError::WrongUser);
        }
        tilo_user_identity::verify_device_cert_at(device, now_ns)
            .map_err(|_| DelegationError::Uncertified)?;
        self.validate()?;
        if self.room != room {
            return Err(DelegationError::Invalid("room"));
        }
        // Signed while the device was certified: never before its
        // certificate, never past its expiry.
        if self.issued_at_ns < device.issued_at_ns
            || device
                .expires_at_ns
                .is_some_and(|expiry| self.issued_at_ns >= expiry)
        {
            return Err(DelegationError::Uncertified);
        }
        let key = VerifyingKey::from_bytes(&self.device_public_key)
            .map_err(|_| DelegationError::Signature)?;
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

    /// Check `proof` is the agent key's signature over `message`: what the
    /// agent presents to show it holds the key this delegation names.
    ///
    /// # Errors
    /// [`DelegationError::Signature`] when it is not.
    pub fn verify_agent_signature(
        &self,
        message: &[u8],
        proof: &[u8],
    ) -> Result<(), DelegationError> {
        let proof = <[u8; 64]>::try_from(proof).map_err(|_| DelegationError::Signature)?;
        VerifyingKey::from_bytes(&self.agent_public_key)
            .map_err(|_| DelegationError::Signature)?
            .verify(message, &Signature::from_bytes(&proof))
            .map_err(|_| DelegationError::Signature)
    }

    /// Canonical encoding, for carrying the delegation on a run and in a
    /// join.
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

    /// [`Self::encode`] as lowercase hex, the form a run's record keeps.
    #[must_use]
    pub fn to_hex(&self) -> String {
        hex_of(&self.encode())
    }

    /// Decode [`Self::to_hex`] output. The caller still verifies it.
    ///
    /// # Errors
    /// [`DelegationError::Invalid`] for text that is not such hex.
    pub fn from_hex(text: &str) -> Result<Self, DelegationError> {
        if !text.len().is_multiple_of(2) || text.len() > 4096 {
            return Err(DelegationError::Invalid("encoding"));
        }
        let bytes = (0..text.len())
            .step_by(2)
            .map(|at| {
                text.get(at..at.saturating_add(2))
                    .and_then(|pair| u8::from_str_radix(pair, 16).ok())
            })
            .collect::<Option<Vec<u8>>>()
            .ok_or(DelegationError::Invalid("encoding"))?;
        Self::decode(&bytes)
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, reason = "in a test these are the assertion")]

    use super::*;

    const ROOM: &str = "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa";
    const OTHER_ROOM: &str = "bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb";
    const TASK: &str = "tasks.tracker/issues/7d3c";
    const HOUR_NS: u64 = 3600 * 1_000_000_000;
    const NOW: u64 = 1_800_000_000_000_000_000;

    fn root() -> SigningKey {
        SigningKey::from_bytes(&[1; 32])
    }

    fn device() -> SigningKey {
        SigningKey::from_bytes(&[2; 32])
    }

    fn agent() -> [u8; 32] {
        SigningKey::from_bytes(&[3; 32]).verifying_key().to_bytes()
    }

    fn certificate(root: &SigningKey, device: &SigningKey) -> DeviceCert {
        let mut cert = DeviceCert {
            user_public_key: root.verifying_key().to_bytes(),
            device_public_key: device.verifying_key().to_bytes(),
            device_label: "Laptop".into(),
            capabilities: tilo_user_identity::DeviceCapabilities::member(),
            issued_at_ns: NOW - HOUR_NS,
            expires_at_ns: None,
            signature: [0; 64],
        };
        tilo_user_identity::sign_device_cert(&mut cert, root).unwrap();
        cert
    }

    fn delegation(expires: u64) -> TaskDelegation {
        TaskDelegation::sign(
            &device(),
            root().verifying_key().to_bytes(),
            agent(),
            TASK,
            ROOM,
            NOW,
            expires,
        )
        .unwrap()
    }

    #[test]
    fn a_device_signed_task_delegation_verifies_against_its_certificate() {
        let delegation = delegation(NOW + HOUR_NS);
        let cert = certificate(&root(), &device());
        assert_eq!(delegation.verify(&cert, ROOM, NOW + 1), Ok(()));
        assert!(is_task_agent(&delegation.agent_id));
        let decoded = TaskDelegation::from_hex(&delegation.to_hex()).unwrap();
        assert_eq!(decoded, delegation);
    }

    #[test]
    fn an_expired_delegation_is_refused() {
        let delegation = delegation(NOW + HOUR_NS);
        let cert = certificate(&root(), &device());
        assert_eq!(
            delegation.verify(&cert, ROOM, NOW + HOUR_NS),
            Err(DelegationError::Expired)
        );
        assert_eq!(
            delegation.verify(&cert, ROOM, NOW - 1),
            Err(DelegationError::Expired)
        );
    }

    #[test]
    fn a_delegation_signed_by_an_uncertified_device_is_refused() {
        let delegation = delegation(NOW + HOUR_NS);
        // Certified by another person's root: not the device's person.
        let foreign = certificate(&SigningKey::from_bytes(&[9; 32]), &device());
        assert_eq!(
            delegation.verify(&foreign, ROOM, NOW + 1),
            Err(DelegationError::WrongUser)
        );
        // A certificate whose signature does not verify.
        let mut forged = certificate(&root(), &device());
        forged.device_label = "Changed".into();
        assert_eq!(
            delegation.verify(&forged, ROOM, NOW + 1),
            Err(DelegationError::Uncertified)
        );
        // Signed before the device was certified.
        let mut late = certificate(&root(), &device());
        late.issued_at_ns = NOW + 1;
        tilo_user_identity::sign_device_cert(&mut late, &root()).unwrap();
        assert_eq!(
            delegation.verify(&late, ROOM, NOW + 2),
            Err(DelegationError::Uncertified)
        );
        // Another device's signature over the same fields.
        let mut stolen = delegation.clone();
        stolen.signature = SigningKey::from_bytes(&[4; 32])
            .sign(&delegation.signing_bytes())
            .to_bytes();
        assert_eq!(
            stolen.verify(&certificate(&root(), &device()), ROOM, NOW + 1),
            Err(DelegationError::Signature)
        );
    }

    #[test]
    fn a_delegation_names_one_room_one_task_and_lasts_at_most_a_day() {
        let cert = certificate(&root(), &device());
        assert_eq!(
            delegation(NOW + HOUR_NS).verify(&cert, OTHER_ROOM, NOW + 1),
            Err(DelegationError::Invalid("room"))
        );
        assert_eq!(
            TaskDelegation::sign(
                &device(),
                root().verifying_key().to_bytes(),
                agent(),
                TASK,
                ROOM,
                NOW,
                NOW + 25 * HOUR_NS,
            ),
            Err(DelegationError::Invalid(
                "a task delegation lasts at most a day"
            ))
        );
        let mut renamed = delegation(NOW + HOUR_NS);
        renamed.agent_id = "scribe".into();
        assert_eq!(
            renamed.verify(&cert, ROOM, NOW + 1),
            Err(DelegationError::Invalid("agent id"))
        );
    }

    #[test]
    fn only_the_minted_key_proves_it_holds_the_delegation() {
        let delegation = delegation(NOW + HOUR_NS);
        let message = b"join";
        let proof = SigningKey::from_bytes(&[3; 32]).sign(message).to_bytes();
        assert_eq!(delegation.verify_agent_signature(message, &proof), Ok(()));
        let other = device().sign(message).to_bytes();
        assert_eq!(
            delegation.verify_agent_signature(message, &other),
            Err(DelegationError::Signature)
        );
    }
}
