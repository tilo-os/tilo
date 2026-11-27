//! Who saved a shortcut, proven.
//!
//! A shortcut's `saved_by` is authority: its triggers fire on that person's
//! behalf and its unattended runs use the consents they gave in advance. A
//! field anyone writing the workspace could set is no such thing, and a host
//! merges and relays what members write, so neither the sender of a
//! batch nor an automerge actor id says who wrote a shortcut. The saver's
//! device signs the shortcut instead ([`sign_saved_shortcut`]), over the
//! workspace it is saved in, and every reader asks [`proven_saver`]: admission for
//! each shortcut a batch adds or changes, whoever sends it, and trigger
//! arming each time it arms, against the workspace's revocations then.

use automerge::AutoCommit;
use collab_types::signer::DeviceSigner;
use ed25519_dalek::Verifier as _;
use tilo_automation::{SavedProof, Shortcut};
use tilo_user_identity::{DeviceCert, RevocationList};

/// Sign `shortcut` as saved in `workspace` by the person `device` is certified to,
/// with that device's `key`: sets `saved_by` to the person and attaches the
/// proof.
///
/// # Errors
///
/// `key` is not the certified device's, or the certificate does not encode.
pub fn sign_saved_shortcut(
    workspace: &str,
    shortcut: &mut Shortcut,
    device: &DeviceCert,
    key: &dyn DeviceSigner,
) -> Result<(), String> {
    if key.device_public_key() != device.device_public_key {
        return Err("the signing key is not the certified device's".into());
    }
    shortcut.saved_by = Some(hex::encode(device.user_public_key));
    shortcut.saved_proof = None;
    let signature =
        collab_types::signer::sign_now(key, shortcut.saved_signing_digest(workspace).to_vec())
            .map_err(|error| error.to_string())?;
    shortcut.saved_proof = Some(SavedProof {
        device_cert: serde_json::to_value(device).map_err(|error| error.to_string())?,
        signature: hex::encode(signature),
    });
    Ok(())
}

/// Sign `shortcut` as saved in `workspace` by the agent `agent`, for the person its key's
/// grant `grant` was certified by, with that key: sets `saved_by` to the person and attaches a
/// proof that names the agent beside its grant. The shortcut then acts as that agent, held to
/// the agent's grants as well as its own.
///
/// # Errors
///
/// `key` is not the grant's, the grant is no grant for `agent`, or it does not encode.
pub fn sign_saved_shortcut_as_agent(
    workspace: &str,
    shortcut: &mut Shortcut,
    agent: &str,
    grant: &DeviceCert,
    key: &dyn DeviceSigner,
) -> Result<(), String> {
    if key.device_public_key() != grant.device_public_key {
        return Err("the signing key is not the agent's grant".into());
    }
    if !grant_for(grant, agent, workspace) {
        return Err(format!(
            "the key is no grant of the agent `{agent}` in this workspace"
        ));
    }
    shortcut.saved_by = Some(hex::encode(grant.user_public_key));
    shortcut.saved_proof = None;
    let signature =
        collab_types::signer::sign_now(key, shortcut.saved_signing_digest(workspace).to_vec())
            .map_err(|error| error.to_string())?;
    shortcut.saved_proof = Some(SavedProof {
        device_cert: serde_json::to_value(AgentProof {
            agent: agent.to_owned(),
            grant: serde_json::to_value(grant).map_err(|error| error.to_string())?,
        })
        .map_err(|error| error.to_string())?,
        signature: hex::encode(signature),
    });
    Ok(())
}

/// A proof's certificate where an agent saved the shortcut: the agent, and the grant its key
/// was certified with. A build that reads only a device's certificate there finds none, and the
/// shortcut does not arm on it.
#[derive(serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct AgentProof {
    agent: String,
    grant: serde_json::Value,
}

/// Who a proof shows saved a shortcut.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Saver {
    /// The person, by their root, hex.
    pub person: String,
    /// The agent of theirs that saved it with its own key: the shortcut acts as that agent.
    /// `None` where a device of the person's saved it.
    pub agent: Option<String>,
    /// The key that signed it: the device's, or the agent's grant's.
    pub key: [u8; 32],
}

/// Whether `cert` is a grant its person certified for `agent` alone, reaching `workspace`:
/// the grant an agent acts by, never a device's own key nor a key a person made for their own
/// programs, which act as the person.
fn grant_for(cert: &DeviceCert, agent: &str, workspace: &str) -> bool {
    let Some(scope) = cert.capabilities.agent.as_ref() else {
        return false;
    };
    let certified = tilo_member_types::key_of_agent(agent).map_or_else(
        || scope.is_agent(agent),
        |key| key == cert.device_public_key,
    );
    let reaches = scope.workspace().is_none_or(|reached| {
        uuid::Uuid::parse_str(workspace).is_ok_and(|id| *id.as_bytes() == reached)
    });
    !cert.capabilities.server_key && certified && reaches
}

/// The person who saved `shortcut` in `workspace`, when its proof shows it at
/// `now_ns`: a certificate that chains to that person's root and is current,
/// a device the workspace has not revoked, and that device's signature over the
/// shortcut as saved in this workspace. `None` for a shortcut nobody is named as
/// saving, one without a proof (saved before proofs existed), and any proof
/// that does not hold.
#[must_use]
pub fn proven_saver(
    workspace: &str,
    shortcut: &Shortcut,
    revocations: &RevocationList,
    now_ns: u64,
) -> Option<String> {
    proven(workspace, shortcut, revocations, now_ns).map(|saver| saver.person)
}

/// Who saved `shortcut` in `workspace`, as [`proven_saver`] reads its proof, with the agent of
/// theirs that saved it with its own key, where one did: its grant certified for that agent
/// alone, reaching this workspace, current and not revoked.
#[must_use]
pub fn proven(
    workspace: &str,
    shortcut: &Shortcut,
    revocations: &RevocationList,
    now_ns: u64,
) -> Option<Saver> {
    let saver = shortcut.saved_by.as_deref()?;
    let proof = shortcut.saved_proof.as_ref()?;
    let (device, agent) =
        if let Ok(signed) = serde_json::from_value::<AgentProof>(proof.device_cert.clone()) {
            let grant: DeviceCert = serde_json::from_value(signed.grant).ok()?;
            if !grant_for(&grant, &signed.agent, workspace) {
                return None;
            }
            (grant, Some(signed.agent))
        } else {
            let device: DeviceCert = serde_json::from_value(proof.device_cert.clone()).ok()?;
            // An agent's grant acts as its agent, never as the person: it saves only with the
            // agent named beside it.
            if device.capabilities.agent.is_some() && !device.capabilities.server_key {
                return None;
            }
            (device, None)
        };
    if hex::encode(device.user_public_key) != saver {
        return None;
    }
    tilo_user_identity::verify_device_cert_at(&device, now_ns).ok()?;
    if revocations.refuses(&device) {
        return None;
    }
    let signature: [u8; 64] = hex::decode(&proof.signature).ok()?.try_into().ok()?;
    ed25519_dalek::VerifyingKey::from_bytes(&device.device_public_key)
        .ok()?
        .verify(
            &shortcut.saved_signing_digest(workspace),
            &ed25519_dalek::Signature::from_bytes(&signature),
        )
        .ok()?;
    Some(Saver {
        person: saver.to_owned(),
        agent,
        key: device.device_public_key,
    })
}

/// Every device revocation the workspace `doc` records, each verified under the
/// root of the identity row that carries it. A row or entry that does not
/// verify adds nothing.
#[must_use]
pub fn workspace_revocations(doc: &AutoCommit) -> RevocationList {
    let mut revocations = RevocationList::new();
    for bytes in super::governance_of(Some(doc)).identities.values() {
        let Ok(row) = super::identity_row(bytes) else {
            continue;
        };
        for entry in row.revocations {
            if entry.user_public_key == row.device_cert.user_public_key {
                let _ = revocations.insert(entry);
            }
        }
    }
    revocations
}

/// Nanoseconds since the Unix epoch at `secs` seconds.
#[cfg(test)]
pub(crate) const fn ns_at(secs: u64) -> u64 {
    secs.saturating_mul(1_000_000_000)
}

#[cfg(test)]
#[path = "saved_shortcut_tests.rs"]
pub(crate) mod tests;
