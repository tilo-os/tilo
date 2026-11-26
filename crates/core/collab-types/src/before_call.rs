//! A signed op as builds wrote it before a claim named its caller's call.
//!
//! A claim used to say what a change does, the units it writes and who made it, and nothing of a
//! call; its author signed exactly those bytes. A claim now also writes the call the change was
//! made for, `None` included ([`crate::claim::ChangeClaim::call`]), so the bytes such an op was
//! signed over are no longer the bytes this build writes for it. Its signature still holds over
//! the bytes it was signed over: [`crate::verify_op`] checks an op whose claims name no call over
//! those as well ([`signing_bytes`]), and a store of writes kept in that layout reads them as
//! today's ops ([`SignedOpBeforeCall::into_op`]) instead of dropping what their authors signed.

use serde::{Deserialize, Serialize};
use web_time::SystemTime;

use crate::artifact::ArtifactId;
use crate::canonical::{CanonicalError, canonical_bytes};
use crate::claim::{ChangeClaim, ClaimedAction, Invoker, UnitPath};
use crate::crypto::{Ed25519PublicKey, Signature};
use crate::identity::AttestationId;
use crate::op::{EncryptionMetadata, SignedOp};
use crate::role::OpKind;

/// One run of a signed op's claims as it was written before claims named a call: the changes
/// that share an action, units and invoker text.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RunBeforeCall {
    changes: Vec<[u8; 32]>,
    action: ClaimedAction,
    units: Vec<UnitPath>,
    invoker: Option<String>,
}

/// A signed op as it was kept and sent before claims named a call.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SignedOpBeforeCall {
    #[serde(with = "serde_bytes")]
    payload: Vec<u8>,
    artifact: ArtifactId,
    author: Ed25519PublicKey,
    attestation_id: AttestationId,
    kind: OpKind,
    timestamp: SystemTime,
    seq: u64,
    encryption: Option<EncryptionMetadata>,
    claims: Vec<RunBeforeCall>,
    surface: Option<[u8; 32]>,
    governance: Vec<[u8; 4]>,
    signature: Signature,
}

impl SignedOpBeforeCall {
    /// The op as this build holds it: each claim of each run, naming no call. Its signature is
    /// the one its author made, which [`crate::verify_op`] checks over [`signing_bytes`].
    ///
    /// # Errors
    /// A run that names no change, or an invoker whose text is no tree.
    pub fn into_op(self) -> Result<SignedOp, String> {
        let mut claims = Vec::new();
        for run in self.claims {
            if run.changes.is_empty() {
                return Err("a claim run names no change".into());
            }
            let invoker = run
                .invoker
                .map(|text| {
                    Invoker::read(text.clone())
                        .map_err(|error| format!("a claim's invoker: {error}: {text}"))
                })
                .transpose()?;
            claims.extend(run.changes.into_iter().map(|change| ChangeClaim {
                change,
                action: run.action.clone(),
                units: run.units.clone(),
                invoker: invoker.clone(),
                call: None,
            }));
        }
        Ok(SignedOp {
            payload: self.payload,
            artifact: self.artifact,
            author: self.author,
            attestation_id: self.attestation_id,
            kind: self.kind,
            timestamp: self.timestamp,
            seq: self.seq,
            encryption: self.encryption,
            claims,
            surface: self.surface,
            governance: self.governance,
            signature: self.signature,
        })
    }

    /// The op's bytes as they were kept, which its id was taken over before
    /// ([`crate::canonical_bytes`] of the op).
    ///
    /// # Errors
    /// As [`canonical_bytes`].
    pub fn kept_bytes(&self) -> Result<Vec<u8>, CanonicalError> {
        canonical_bytes(self)
    }
}

/// Who an invoker's `text` names, as builds before agents named their person wrote it: those
/// named an agent by its id alone (`{"agent":"scout"}`), and an agent is now the pair of its id and
/// the person it acts for (`{"agent":{"id":"scout","person":…}}`), the person its acting chain
/// names. `None` for text that is no tree, or names an agent whose chain names no person.
pub(crate) fn acting_before_agents_named_their_person(
    text: &str,
) -> Option<tilo_action_policy::Acting> {
    let mut tree: serde_json::Value = serde_json::from_str(text).ok()?;
    name_agents_with_their_person(&mut tree, None).ok()?;
    serde_json::from_value(tree).ok()
}

/// The first person an acting chain's `for` names, depth first.
fn person_of_chain(node: &serde_json::Value) -> Option<serde_json::Value> {
    node.get("for")?.as_array()?.iter().find_map(|acted| {
        acted
            .pointer("/by/member/person")
            .cloned()
            .or_else(|| person_of_chain(acted))
    })
}

/// Rewrite each agent named by its id alone in the acting `node` as the pair of its id and the
/// person its chain acts for, or `above`, the person the node is acted for in turn.
fn name_agents_with_their_person(
    node: &mut serde_json::Value,
    above: Option<&serde_json::Value>,
) -> Result<(), String> {
    let person = person_of_chain(node).or_else(|| above.cloned());
    if let Some(member) = node.pointer_mut("/by/member")
        && let Some(id) = member.get("agent").and_then(serde_json::Value::as_str)
    {
        let Some(person) = person.clone() else {
            return Err(format!("an agent {id} a claim names acts for no person"));
        };
        let id = id.to_owned();
        *member = serde_json::json!({ "agent": { "id": id, "person": person } });
    }
    if let Some(chain) = node
        .get_mut("for")
        .and_then(serde_json::Value::as_array_mut)
    {
        for acted in chain {
            name_agents_with_their_person(acted, person.as_ref())?;
        }
    }
    Ok(())
}

/// The bytes `op`'s author signed when its claims were written without a call: `op`'s fields
/// with each claim's change, action, units and invoker text, and no call. `None` when a claim
/// names a call, which no op of that layout did.
///
/// # Errors
/// As [`canonical_bytes`], or an invoker that does not write.
pub fn signing_bytes(op: &SignedOp) -> Result<Option<Vec<u8>>, CanonicalError> {
    #[derive(Serialize)]
    struct Claim<'a> {
        change: &'a [u8; 32],
        action: &'a ClaimedAction,
        units: &'a [UnitPath],
        invoker: Option<String>,
    }
    #[derive(Serialize)]
    struct View<'a> {
        #[serde(with = "serde_bytes")]
        payload: &'a [u8],
        artifact: &'a ArtifactId,
        author: &'a Ed25519PublicKey,
        attestation_id: &'a AttestationId,
        kind: &'a OpKind,
        timestamp: &'a SystemTime,
        seq: u64,
        encryption: &'a Option<EncryptionMetadata>,
        claims: Vec<Claim<'a>>,
        surface: &'a Option<[u8; 32]>,
        governance: &'a [[u8; 4]],
    }
    if op.claims.iter().any(|claim| claim.call.is_some()) {
        return Ok(None);
    }
    let mut claims = Vec::with_capacity(op.claims.len());
    for claim in &op.claims {
        claims.push(Claim {
            change: &claim.change,
            action: &claim.action,
            units: &claim.units,
            invoker: crate::claim::invoker_text::text(claim.invoker.as_ref())
                .map_err(|error| CanonicalError::Serialize(error.to_string()))?,
        });
    }
    canonical_bytes(&View {
        payload: &op.payload,
        artifact: &op.artifact,
        author: &op.author,
        attestation_id: &op.attestation_id,
        kind: &op.kind,
        timestamp: &op.timestamp,
        seq: op.seq,
        encryption: &op.encryption,
        claims,
        surface: &op.surface,
        governance: &op.governance,
    })
    .map(Some)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::Signer as _;

    fn acting() -> tilo_action_policy::Acting {
        tilo_action_policy::Acting {
            by: tilo_action_policy::Actor::member(tilo_action_policy::MemberId::person("ana")),
            app: None,
            agent_session: None,
            cause: None,
            foreign_device: false,
            handed: false,
            acted_for: Vec::new(),
        }
    }

    /// An op as a build before calls signed and kept it: built from that layout's own types,
    /// signed over that layout's bytes.
    fn kept(key: &ed25519_dalek::SigningKey) -> (SignedOpBeforeCall, Vec<u8>) {
        kept_with(key, serde_json::to_string(&acting()).unwrap())
    }

    /// [`kept`] with the invoker text `invoker`, as its signer wrote it.
    fn kept_with(
        key: &ed25519_dalek::SigningKey,
        invoker: String,
    ) -> (SignedOpBeforeCall, Vec<u8>) {
        #[derive(Serialize)]
        struct Claim<'a> {
            change: [u8; 32],
            action: &'a ClaimedAction,
            units: &'a [UnitPath],
            invoker: &'a Option<String>,
        }
        let mut op = SignedOpBeforeCall {
            payload: vec![7; 48],
            artifact: ArtifactId::new(),
            author: Ed25519PublicKey::from_bytes(key.verifying_key().to_bytes()),
            attestation_id: AttestationId::new(),
            kind: OpKind::Change,
            timestamp: SystemTime::UNIX_EPOCH
                .checked_add(std::time::Duration::from_secs(9))
                .unwrap(),
            seq: 9,
            encryption: None,
            claims: vec![RunBeforeCall {
                changes: vec![[1; 32], [2; 32]],
                action: ClaimedAction::whole_resource_edit(String::new()),
                units: vec![Vec::new()],
                invoker: Some(invoker),
            }],
            surface: None,
            governance: vec![[3; 4]],
            signature: Signature::from_bytes([0; 64]),
        };
        let run = &op.claims[0];
        let claims: Vec<Claim<'_>> = run
            .changes
            .iter()
            .map(|change| Claim {
                change: *change,
                action: &run.action,
                units: &run.units,
                invoker: &run.invoker,
            })
            .collect();
        let signed = postcard::to_allocvec(&(
            serde_bytes::Bytes::new(&op.payload),
            &op.artifact,
            &op.author,
            &op.attestation_id,
            &op.kind,
            &op.timestamp,
            op.seq,
            &op.encryption,
            claims,
            &op.surface,
            &op.governance,
        ))
        .unwrap();
        op.signature = Signature::from_bytes(key.sign(&signed).to_bytes());
        let bytes = postcard::to_allocvec(&op).unwrap();
        (op, bytes)
    }

    tilo_test_env::scenario! {
        /// An op a build before calls signed reads as today's op, keeps the signature its author
        /// made, and verifies; a call put on it, or a change to what it claims, does not.
        fn an_op_signed_before_claims_named_a_call_still_verifies;

        phase it_reads_as_todays_op_and_verifies;
        {
            let key = ed25519_dalek::SigningKey::from_bytes(&[0x42; 32]);
            let (kept, bytes) = kept(&key);
            let read: SignedOpBeforeCall = postcard::from_bytes(&bytes).unwrap();
            assert_eq!(read.kept_bytes().unwrap(), bytes, "the op's kept bytes are read back whole");
            let op = read.into_op().unwrap();
            assert_eq!(op.claims.len(), 2, "each change of the run is a claim");
            assert!(op.claims.iter().all(|claim| claim.call.is_none()));
            assert_eq!(op.signature, kept.signature);
            assert!(
                crate::crypto::verify_op(&op).is_ok(),
                "the author's signature verifies over the bytes it was made over"
            );
            let again: SignedOp =
                crate::canonical::from_canonical_bytes(&canonical_bytes(&op).unwrap()).unwrap();
            assert!(
                crate::crypto::verify_op(&again).is_ok(),
                "written as today and read back, it still verifies"
            );
        }

        phase an_agent_named_by_its_id_alone_is_read_as_the_agent_of_its_person;
        {
            // Those builds named an agent by its id; the text stays as signed, and the tree names
            // the person the agent acted for.
            let person = "6490098a9f305d98553e5c81b77264c8cec83878a77ce7bd9b10ea885d5531c5";
            let text = format!(
                r#"{{"by":{{"member":{{"agent":"claude"}}}},"handed":true,"for":[{{"by":{{"member":{{"person":"{person}"}}}}}}]}}"#
            );
            let key = ed25519_dalek::SigningKey::from_bytes(&[0x44; 32]);
            let (kept, _) = kept_with(&key, text.clone());
            let op = kept.into_op().unwrap();
            let invoker = op.claims[0].invoker.as_ref().unwrap();
            assert_eq!(invoker.text().unwrap(), text, "the text is kept as it was signed");
            assert_eq!(
                invoker.by,
                tilo_action_policy::Actor::member(tilo_action_policy::MemberId::Agent {
                    id: "claude".into(),
                    person: tilo_action_policy::PersonId::from(person.to_owned()),
                })
            );
            assert!(crate::crypto::verify_op(&op).is_ok());
            // Kept as this build keeps it, the op reads back whole, the old text and all.
            let again: SignedOp = postcard::from_bytes(&postcard::to_allocvec(&op).unwrap()).unwrap();
            assert_eq!(again, op, "written as today and read back, it is the same op");
            assert!(crate::crypto::verify_op(&again).is_ok());
            let (alone, _) = kept_with(&key, r#"{"by":{"member":{"agent":"claude"}}}"#.into());
            assert!(alone.into_op().is_err(), "an agent acting for no person is not guessed");
        }

        phase what_it_claims_cannot_change;
        {
            let key = ed25519_dalek::SigningKey::from_bytes(&[0x43; 32]);
            let (kept, _) = kept(&key);
            let op = kept.into_op().unwrap();
            let mut moved = op.clone();
            moved.claims[1].units = vec![vec![tilo_actions::PathStep::Key("other".into())]];
            assert!(crate::crypto::verify_op(&moved).is_err(), "a changed claim does not verify");
            let mut later = op;
            later.seq = 10;
            assert!(crate::crypto::verify_op(&later).is_err(), "a changed field does not verify");
        }
    }
}
