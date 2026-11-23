//! Signed semantic admission, not proof of execution or successful completion.
//!
//! Retain the entire [`AdmissionEvidence`] only in private history or the
//! host-private execution journal. Publish only [`AdmissionReceipt`].
//! An opening permits offline guesses of the
//! intent and must be disclosed only through original-owner observation.
//! Certificate root trust, workspace/host authority, revocation, certificate time
//! policy and live caller authentication remain the caller's responsibility.
//! Verification alone neither authorizes dispatch nor deduplicates invocations.

use ed25519_dalek::{Signature, VerifyingKey};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tilo_actions::InvocationId;
use tilo_user_identity::DeviceCert;
use uuid::Uuid;

const SIGN_DOMAIN: &[u8] = b"tilo-semantic-admission-v1\0";
const INTENT_DOMAIN: &[u8] = b"tilo-semantic-admission-intent-v1\0";
const COMMIT_DOMAIN: &[u8] = b"tilo-semantic-admission-commitment-v1\0";

/// Authenticated evidence only. Construct after checking the corresponding
/// channel credentials; deserializing this type does not authenticate anyone.
/// There is deliberately no actor-string-only local identity.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub enum VerifiedActor {
    Certified {
        root: [u8; 32],
        device: [u8; 32],
    },
    AuthenticatedLocal {
        root: [u8; 32],
        device: [u8; 32],
        /// The agent this session was delegated to, or `null` for the person.
        /// The field must be present: reading an omission as the person would
        /// hand a delegated agent's invocation to them.
        #[serde(default = "omitted_agent")]
        agent: Option<String>,
    },
}

/// The value an omitted `agent` deserializes to. Serde's default for an
/// `Option` field is `None`, which would read a dropped field as the person
/// themselves; this sentinel cannot be a real agent name, so
/// [`Self::validate`] (and [`crate::apps::action_evidence::verify`]) refuses it
/// instead.
#[allow(
    clippy::unnecessary_wraps,
    reason = "serde's field default must match the Option field type"
)]
fn omitted_agent() -> Option<String> {
    Some(String::from(OMITTED_AGENT))
}

/// The omission sentinel. Never a valid agent: it holds a control character,
/// which every agent-name check refuses.
const OMITTED_AGENT: &str = "\u{0}omitted-agent";

/// Public routing and identity facts. Never put arguments, context, cause,
/// results, or other private intent contents in these fields.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AdmissionFacts {
    pub host: Uuid,
    pub workspace: String,
    pub invocation: InvocationId,
    pub actor: VerifiedActor,
    /// Host-verified effective actor, including a delegated agent's identity.
    pub actor_id: tilo_actions::MemberId,
    pub app_id: String,
    pub document_id: String,
    pub resource: String,
    pub action: String,
    pub revision: Option<String>,
}

/// Shareable admission statement. Signature bytes use a serde-safe vector;
/// verification requires exactly 64 bytes. The certificate identifies the host
/// signer, which need not be the invoking actor.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AdmissionReceipt {
    pub facts: AdmissionFacts,
    pub intent_commitment: [u8; 32],
    pub certificate: DeviceCert,
    pub signature: Vec<u8>,
}

/// Private evidence value. Serialization includes the opening and is NOT a
/// public observation format. No plaintext intent is retained, even privately.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AdmissionEvidence {
    pub receipt: AdmissionReceipt,
    opening: IntentOpening,
}

/// Private nonce and digest, without plaintext. No Debug implementation: logs
/// must not accidentally turn a public commitment into a guessing oracle.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IntentOpening {
    nonce: [u8; 32],
    intent_digest: [u8; 32],
    retained_digest: [u8; 32],
    /// Absent only in historical openings. The wrapper distinguishes a bound
    /// missing request from an opening that predates retry-digest binding.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    request: Option<RequestBinding>,
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct RequestBinding {
    digest: Option<[u8; 32]>,
}

impl std::fmt::Debug for AdmissionEvidence {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AdmissionEvidence")
            .field("receipt", &self.receipt)
            .finish_non_exhaustive()
    }
}

/// Terminal classifications reserved for outcome records, not admission claims.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TerminalCategory {
    Succeeded,
    Failed,
    Cancelled,
    Uncertain,
}

/// Signed lifecycle classification. It attests what the host observed, not
/// whether an external side effect occurred. No result/error payload is public.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TerminalReceipt {
    pub admission_digest: [u8; 32],
    pub category: TerminalCategory,
    pub certificate: DeviceCert,
    pub signature: Vec<u8>,
}

pub(crate) fn terminal_category(
    update: &tilo_actions::InvocationUpdate,
    uncertain: bool,
) -> Option<TerminalCategory> {
    use tilo_actions::InvocationUpdate as Update;
    match update {
        Update::Succeeded { .. } if !uncertain => Some(TerminalCategory::Succeeded),
        Update::Failed { .. } if !uncertain => Some(TerminalCategory::Failed),
        Update::Cancelled { .. } if !uncertain => Some(TerminalCategory::Cancelled),
        _ if uncertain => Some(TerminalCategory::Uncertain),
        _ => None,
    }
}

fn admission_digest(receipt: &AdmissionReceipt) -> Result<[u8; 32], String> {
    let value = serde_json::to_value(receipt).map_err(|_| "admission encoding failed")?;
    let mut hash = blake3::Hasher::new();
    hash.update(b"tilo-semantic-admission-reference-v1\0");
    tilo_canonical_json::write_bytes(&value, |chunk| {
        hash.update(chunk);
    });
    Ok(*hash.finalize().as_bytes())
}

fn terminal_bytes(digest: &[u8; 32], category: TerminalCategory) -> Result<Vec<u8>, String> {
    let mut bytes = b"tilo-semantic-terminal-v1\0".to_vec();
    bytes.extend(serde_json::to_vec(&(digest, category)).map_err(|_| "terminal encoding failed")?);
    Ok(bytes)
}

pub(crate) fn mint_terminal(
    admission: &AdmissionReceipt,
    category: TerminalCategory,
) -> Result<TerminalReceipt, String> {
    let digest = admission_digest(admission)?;
    let (certificate, signature) = crate::apps::evidence_signer::sign(
        &admission.facts.workspace,
        &terminal_bytes(&digest, category)?,
    )?;
    Ok(TerminalReceipt {
        admission_digest: digest,
        category,
        certificate,
        signature: signature.to_vec(),
    })
}

/// Apply the same independent signer trust policy as for [`verify`].
pub fn verify_terminal(
    terminal: &TerminalReceipt,
    admission: &AdmissionReceipt,
) -> Result<(), String> {
    if terminal.admission_digest != admission_digest(admission)? {
        return Err("terminal references another admission".into());
    }
    tilo_user_identity::verify_device_cert(&terminal.certificate)
        .map_err(|_| "invalid terminal certificate")?;
    let key = VerifyingKey::from_bytes(&terminal.certificate.device_public_key)
        .map_err(|_| "invalid terminal signer")?;
    let signature = Signature::from_slice(&terminal.signature)
        .map_err(|_| "invalid terminal signature length")?;
    key.verify_strict(
        &terminal_bytes(&terminal.admission_digest, terminal.category)?,
        &signature,
    )
    .map_err(|_| "invalid terminal signature".into())
}

/// Mint only after semantic admission has authenticated the actor and resolved
/// the resource/action. `exact_intent` must be the complete admission value,
/// including arguments, cause and context; no fields are selected or dropped.
pub(crate) fn mint(
    facts: AdmissionFacts,
    exact_intent: &Value,
) -> Result<AdmissionEvidence, String> {
    mint_with_signer(facts, exact_intent, crate::apps::evidence_signer::sign)
}

/// A local report can be reconstructed after cache eviction or host restart.
/// Derive its private salt from a domain-separated device signature, not from
/// public input: a public content hash would allow guesses of private text.
/// This is not an execution permit and never reserves a journal identity.
pub(crate) fn mint_local_report(
    facts: AdmissionFacts,
    exact_intent: &Value,
) -> Result<AdmissionEvidence, String> {
    let mut seed = b"tilo-local-report-private-salt-v1\0".to_vec();
    seed.extend(signing_bytes(&facts, &intent_digest(exact_intent)?)?);
    let (_, signature) = crate::apps::evidence_signer::sign(&facts.workspace, &seed)?;
    mint_with_nonce(
        facts,
        exact_intent,
        *blake3::hash(&signature).as_bytes(),
        crate::apps::evidence_signer::sign,
    )
}

fn mint_with_signer(
    facts: AdmissionFacts,
    exact_intent: &Value,
    signer: impl FnOnce(&str, &[u8]) -> Result<(DeviceCert, [u8; 64]), String>,
) -> Result<AdmissionEvidence, String> {
    // Two independent UUIDv4s provide a 256-bit nonce representation (244
    // random bits after UUID version/variant bits), never an intent-derived salt.
    let mut nonce = [0; 32];
    nonce[..16].copy_from_slice(Uuid::new_v4().as_bytes());
    nonce[16..].copy_from_slice(Uuid::new_v4().as_bytes());
    mint_with_nonce(facts, exact_intent, nonce, signer)
}

fn mint_with_nonce(
    facts: AdmissionFacts,
    exact_intent: &Value,
    nonce: [u8; 32],
    signer: impl FnOnce(&str, &[u8]) -> Result<(DeviceCert, [u8; 64]), String>,
) -> Result<AdmissionEvidence, String> {
    let opening = IntentOpening {
        nonce,
        intent_digest: intent_digest(exact_intent)?,
        retained_digest: retained_digest(exact_intent)?,
        request: Some(RequestBinding {
            digest: exact_intent
                .get("request")
                .map(crate::apps::collab_app_broker::admission_request_digest),
        }),
    };
    let intent_commitment = opening.commitment();
    let bytes = signing_bytes(&facts, &intent_commitment)?;
    let (certificate, signature) = signer(&facts.workspace, &bytes)?;
    let evidence = AdmissionEvidence {
        receipt: AdmissionReceipt {
            facts,
            intent_commitment,
            certificate,
            signature: signature.to_vec(),
        },
        opening,
    };
    evidence.validate(&evidence.receipt.facts)?;
    Ok(evidence)
}

/// Verify the certificate's root signature and host receipt signature, and
/// require exact equality with independently supplied expected facts.
///
/// This deliberately uses `verify_device_cert`, not a wall-clock check: durable
/// historical admissions outlive certificates. Callers must separately decide
/// which root/device is trusted for this host and workspace, including revocation
/// and certificate validity policy. Supply expected facts from trusted state,
/// not by copying them from an untrusted receipt. Replays with identical facts
/// still verify: the durable journal owns execution uniqueness and dispatch.
/// Local reports instead use content-bound capture identities, never dispatch.
pub fn verify(receipt: &AdmissionReceipt, expected_facts: &AdmissionFacts) -> Result<(), String> {
    receipt.facts.actor.validate()?;
    if receipt.facts != *expected_facts {
        return Err("admission facts do not match".into());
    }
    tilo_user_identity::verify_device_cert(&receipt.certificate)
        .map_err(|error| format!("invalid admission certificate: {error}"))?;
    let key = VerifyingKey::from_bytes(&receipt.certificate.device_public_key)
        .map_err(|_| "invalid admission signer key")?;
    let signature = Signature::from_slice(&receipt.signature)
        .map_err(|_| "invalid admission signature length")?;
    key.verify_strict(
        &signing_bytes(&receipt.facts, &receipt.intent_commitment)?,
        &signature,
    )
    .map_err(|_| "invalid admission signature".into())
}

impl VerifiedActor {
    /// Reject an agent name that no delegated session could have: the
    /// omission sentinel, an empty or over-long name, or control characters.
    pub fn validate(&self) -> Result<(), String> {
        if let Self::AuthenticatedLocal {
            agent: Some(agent), ..
        } = self
            && (agent == OMITTED_AGENT
                || agent.is_empty()
                || agent.len() > 512
                || agent.bytes().any(|byte| byte.is_ascii_control()))
        {
            return Err("invalid authenticated local admission agent".into());
        }
        Ok(())
    }
}

impl AdmissionEvidence {
    /// The only shareable projection; never serialize `self` for public use.
    pub fn public_receipt(&self) -> &AdmissionReceipt {
        &self.receipt
    }

    /// Validate recovered private journal data without retaining plaintext.
    /// Caller must also apply signer trust policy as documented on [`verify`].
    pub fn validate(&self, expected_facts: &AdmissionFacts) -> Result<(), String> {
        verify(&self.receipt, expected_facts)?;
        self.opening.validate_commitment(&self.receipt)
    }

    pub(crate) fn validate_retained(&self, retained: &Value) -> Result<(), String> {
        if retained_digest(retained)? != self.opening.retained_digest {
            return Err("retained admission intent differs from signed intent".into());
        }
        Ok(())
    }

    pub(crate) fn validate_request_digest(&self, digest: Option<[u8; 32]>) -> Result<(), String> {
        if let Some(request) = &self.opening.request
            && request.digest != digest
        {
            return Err("retained request digest differs from signed request".into());
        }
        // Historical openings cannot authenticate the separately stored digest;
        // do not manufacture a binding or rewrite their signed commitments.
        Ok(())
    }

    /// Release the private opening only for the exact original owner. The
    /// caller must authenticate the live observer before supplying its actor;
    /// equality with a deserialized identity is not authentication.
    pub fn opening_for_owner(&self, observer: &VerifiedActor) -> Result<&IntentOpening, String> {
        if observer != &self.receipt.facts.actor {
            return Err("admission opening belongs to another owner".into());
        }
        Ok(&self.opening)
    }
}

impl IntentOpening {
    /// Check the complete original intent against this opening and receipt.
    /// Also call [`verify`] with independently expected facts and trust policy;
    /// a commitment check alone does not authenticate the receipt.
    pub fn verify(&self, receipt: &AdmissionReceipt, exact_intent: &Value) -> Result<(), String> {
        if intent_digest(exact_intent)? != self.intent_digest {
            return Err("admission intent does not match".into());
        }
        self.validate_commitment(receipt)
    }

    fn commitment(&self) -> [u8; 32] {
        let mut hash = blake3::Hasher::new();
        hash.update(COMMIT_DOMAIN);
        hash.update(&self.nonce);
        hash.update(&self.intent_digest);
        hash.update(&self.retained_digest);
        if let Some(request) = &self.request {
            hash.update(b"tilo-semantic-admission-request-v1\0");
            match request.digest {
                Some(digest) => {
                    hash.update(&[1]);
                    hash.update(&digest);
                }
                None => {
                    hash.update(&[0]);
                }
            }
        }
        *hash.finalize().as_bytes()
    }

    fn validate_commitment(&self, receipt: &AdmissionReceipt) -> Result<(), String> {
        if self.commitment() != receipt.intent_commitment {
            return Err("admission opening does not match commitment".into());
        }
        Ok(())
    }
}

#[allow(
    clippy::unnecessary_wraps,
    reason = "preserves the existing private signature checked by pinning tests"
)]
fn signing_bytes(facts: &AdmissionFacts, commitment: &[u8; 32]) -> Result<Vec<u8>, String> {
    let mut bytes = SIGN_DOMAIN.to_vec();
    tilo_canonical_json::append_vec(
        &mut bytes,
        &serde_json::json!({
            "facts": facts,
            "intent_commitment": commitment,
        }),
    );
    Ok(bytes)
}

#[allow(
    clippy::unnecessary_wraps,
    reason = "preserves the existing private signature checked by pinning tests"
)]
fn retained_digest(intent: &Value) -> Result<[u8; 32], String> {
    let mut hash = blake3::Hasher::new_derive_key("tilo semantic admission retained intent v1");
    if intent
        .as_object()
        .is_some_and(|fields| fields.contains_key("request"))
    {
        let mut retained = intent.clone();
        if let Some(fields) = retained.as_object_mut() {
            fields.remove("request");
        }
        tilo_canonical_json::write_bytes(&retained, |chunk| {
            hash.update(chunk);
        });
    } else {
        tilo_canonical_json::write_bytes(intent, |chunk| {
            hash.update(chunk);
        });
    }
    Ok(*hash.finalize().as_bytes())
}

#[allow(
    clippy::unnecessary_wraps,
    reason = "preserves the existing private signature checked by pinning tests"
)]
fn intent_digest(intent: &Value) -> Result<[u8; 32], String> {
    let mut hash = blake3::Hasher::new();
    hash.update(INTENT_DOMAIN);
    tilo_canonical_json::write_bytes(intent, |chunk| {
        hash.update(chunk);
    });
    Ok(*hash.finalize().as_bytes())
}

#[cfg(test)]
mod tests {
    use ed25519_dalek::{Signer as _, SigningKey};
    use tilo_user_identity::{DeviceCapabilities, sign_device_cert};

    use super::*;

    fn facts() -> AdmissionFacts {
        AdmissionFacts {
            host: Uuid::from_u128(1),
            workspace: "workspace".into(),
            invocation: InvocationId(Uuid::from_u128(2)),
            actor: VerifiedActor::AuthenticatedLocal {
                root: [3; 32],
                device: [4; 32],
                agent: Some("agent".into()),
            },
            actor_id: tilo_actions::MemberId::agent("agent"),
            app_id: "notes".into(),
            document_id: "record".into(),
            resource: "document/body".into(),
            action: "append".into(),
            revision: Some("revision".into()),
        }
    }

    fn intent() -> Value {
        serde_json::json!({
            "arguments": {"text": "private phrase"},
            "cause": {"parent": "private cause"},
            "context": {"selection": "private context"}
        })
    }

    fn evidence() -> AdmissionEvidence {
        evidence_for(&intent())
    }

    fn evidence_for(intent: &Value) -> AdmissionEvidence {
        let root = SigningKey::from_bytes(&[11; 32]);
        let device = SigningKey::from_bytes(&[12; 32]);
        let mut certificate = DeviceCert {
            user_public_key: root.verifying_key().to_bytes(),
            device_public_key: device.verifying_key().to_bytes(),
            device_label: "receipt signer".into(),
            capabilities: DeviceCapabilities::primary(),
            issued_at_ns: 1,
            expires_at_ns: None,
            signature: [0; 64],
            issuer: None,
        };
        sign_device_cert(&mut certificate, &root).unwrap();
        mint_with_signer(facts(), intent, |workspace, bytes| {
            assert_eq!(workspace, "workspace");
            assert!(bytes.starts_with(SIGN_DOMAIN));
            Ok((certificate, device.sign(bytes).to_bytes()))
        })
        .unwrap()
    }

    tilo_test_env::scenario! {
        fn action_evidence_binds_its_intent_and_keeps_its_private_parts_private;

        phase privacy_randomization_and_private_round_trip;
        {
            let first = evidence();
            let second = evidence();
            assert_ne!(
                first.receipt.intent_commitment,
                second.receipt.intent_commitment
            );
            let public = serde_json::to_string(first.public_receipt()).unwrap();
            for secret in [
                "private phrase",
                "private cause",
                "private context",
                "nonce",
                "intent_digest",
            ] {
                assert!(!public.contains(secret));
            }
            let private = serde_json::to_string(&first).unwrap();
            assert!(!private.contains("private phrase"));
            assert!(private.contains("nonce"));
            let recovered: AdmissionEvidence = serde_json::from_str(&private).unwrap();
            recovered.validate(&facts()).unwrap();
            let opening = recovered.opening_for_owner(&facts().actor).unwrap();
            opening
                .verify(recovered.public_receipt(), &intent())
                .unwrap();
            assert!(opening.verify(second.public_receipt(), &intent()).is_err());
            let foreign = VerifiedActor::Certified {
                root: [3; 32],
                device: [4; 32],
            };
            assert!(recovered.opening_for_owner(&foreign).is_err());
        }

        phase an_authenticated_local_actor_that_omits_its_agent_is_refused;
        // An authenticated local actor must say whether it is the person
        // (`null`) or the agent named: a statement that drops the field is not
        // read as the person's.
        {
            let evidence = evidence();
            let mut encoded = serde_json::to_value(evidence.public_receipt()).unwrap();
            assert_eq!(
                encoded["facts"]["actor"]["AuthenticatedLocal"]["agent"],
                "agent"
            );
            encoded["facts"]["actor"]["AuthenticatedLocal"]
                .as_object_mut()
                .unwrap()
                .remove("agent");
            // serde's field attribute does not fire through `Value` for this
            // shape, so the omission arrives as the sentinel and is refused when
            // the statement is validated: it is never read as the person's.
            let receipt: AdmissionReceipt = serde_json::from_value(encoded).unwrap();
            assert_eq!(
                receipt.facts.actor.validate(),
                Err("invalid authenticated local admission agent".into())
            );
            assert!(verify(&receipt, &facts()).is_err());
        }

        phase every_intent_field_and_private_opening_are_bound;
        {
            let evidence = evidence();
            for field in ["arguments", "cause", "context"] {
                let mut changed = intent();
                changed[field] = Value::Null;
                assert!(
                    evidence
                        .opening
                        .verify(&evidence.receipt, &changed)
                        .is_err()
                );
            }
            let mut corrupt = evidence.clone();
            corrupt.opening.nonce[0] ^= 1;
            assert!(corrupt.validate(&facts()).is_err());
            let mut corrupt = evidence;
            corrupt.opening.intent_digest[0] ^= 1;
            assert!(corrupt.validate(&facts()).is_err());
        }

        phase retry_digest_presence_and_contents_are_private_and_bound;
        {
            for request in [
                None,
                Some(Value::Null),
                Some(serde_json::json!({"argument": "secret"})),
            ] {
                let mut intent = intent();
                if let Some(request) = request {
                    intent["request"] = request;
                }
                let evidence = evidence_for(&intent);
                let digest = intent
                    .get("request")
                    .map(crate::apps::collab_app_broker::admission_request_digest);
                evidence.validate_request_digest(digest).unwrap();
                assert!(evidence.validate_request_digest(Some([0; 32])).is_err());
                if digest.is_some() {
                    assert!(evidence.validate_request_digest(None).is_err());
                }
                let mut removed = evidence.clone();
                removed.opening.request = None;
                assert!(
                    removed.validate(&facts()).is_err(),
                    "cannot downgrade a new opening"
                );
                let mut replaced = evidence;
                replaced.opening.request.as_mut().unwrap().digest = Some([0; 32]);
                assert!(replaced.validate(&facts()).is_err());
            }
        }

        phase historical_opening_keeps_its_original_commitment_encoding;
        {
            let intent = serde_json::json!({"request": {"argument": "historical"}});
            let evidence = evidence_for(&intent);
            let mut historical = serde_json::to_value(&evidence).unwrap();
            historical["opening"]
                .as_object_mut()
                .unwrap()
                .remove("request");
            // Reproduce the original three-component commitment independently of
            // the current writer, then sign it with the fixture's device identity.
            let mut hash = blake3::Hasher::new();
            hash.update(b"tilo-semantic-admission-commitment-v1\0");
            hash.update(&evidence.opening.nonce);
            hash.update(&evidence.opening.intent_digest);
            hash.update(&evidence.opening.retained_digest);
            let commitment = *hash.finalize().as_bytes();
            let signature = SigningKey::from_bytes(&[12; 32])
                .sign(&signing_bytes(&facts(), &commitment).unwrap())
                .to_bytes();
            historical["receipt"]["intent_commitment"] = serde_json::json!(commitment);
            historical["receipt"]["signature"] = serde_json::json!(signature.to_vec());
            let restored: AdmissionEvidence = serde_json::from_value(historical.clone()).unwrap();
            restored.validate(&facts()).unwrap();
            restored.opening.verify(&restored.receipt, &intent).unwrap();
            restored.validate_retained(&serde_json::json!({})).unwrap();
            assert_eq!(serde_json::to_value(restored).unwrap(), historical);
        }

        phase substitution_and_cross_scope_replay_fail;
        {
            let evidence = evidence();
            let original = serde_json::to_value(facts()).unwrap();
            for (field, value) in original.as_object().unwrap() {
                let mut changed = original.clone();
                changed[field] = match field.as_str() {
                    "host" | "invocation" => serde_json::to_value(Uuid::from_u128(99)).unwrap(),
                    "actor" => serde_json::to_value(VerifiedActor::AuthenticatedLocal {
                        root: [7; 32],
                        device: [8; 32],
                        agent: Some("other".into()),
                    })
                    .unwrap(),
                    "actor_id" => serde_json::to_value(tilo_actions::MemberId::agent("other"))
                        .unwrap(),
                    _ => Value::String(format!("changed-{value}")),
                };
                let expected: AdmissionFacts = serde_json::from_value(changed).unwrap();
                assert!(verify(&evidence.receipt, &expected).is_err(), "{field}");
                let mut substituted = evidence.receipt.clone();
                substituted.facts = expected.clone();
                assert!(verify(&substituted, &expected).is_err(), "{field}");
            }
            // Identical replay is a journal concern, never a new dispatch permit.
            verify(&evidence.receipt, &facts()).unwrap();
            verify(&evidence.receipt, &facts()).unwrap();
        }

        phase signatures_certificates_and_commitments_cannot_be_replaced;
        {
            let evidence = evidence();
            let mut bad = evidence.receipt.clone();
            bad.signature.pop();
            assert!(verify(&bad, &facts()).is_err());
            let mut bad = evidence.receipt.clone();
            bad.signature[0] ^= 1;
            assert!(verify(&bad, &facts()).is_err());
            let mut bad = evidence.receipt.clone();
            bad.certificate.signature[0] ^= 1;
            assert!(verify(&bad, &facts()).is_err());
            let mut bad = evidence.receipt.clone();
            bad.certificate.device_public_key[0] ^= 1;
            assert!(verify(&bad, &facts()).is_err());
            let mut bad = evidence.receipt;
            bad.intent_commitment[0] ^= 1;
            assert!(verify(&bad, &facts()).is_err());
        }

        phase canonical_intent_covers_nested_order_null_and_arrays;
        {
            let left: Value = serde_json::from_str(r#"{"b":{"y":2,"x":1},"a":[1,2]}"#).unwrap();
            let right: Value = serde_json::from_str(r#"{"a":[1,2],"b":{"x":1,"y":2}}"#).unwrap();
            assert_eq!(
                intent_digest(&left).unwrap(),
                intent_digest(&right).unwrap()
            );
            assert_ne!(
                intent_digest(&serde_json::json!({})).unwrap(),
                intent_digest(&serde_json::json!({"x":null})).unwrap()
            );
            assert_ne!(
                intent_digest(&serde_json::json!([1, 2])).unwrap(),
                intent_digest(&serde_json::json!([2, 1])).unwrap()
            );
            assert!(
                serde_json::from_value::<VerifiedActor>(
                    serde_json::json!({"Local":{"actor":{"person":"person"}}})
                )
                .is_err()
            );
            let zeros = [0u8; 32];
            // An authenticated local actor that drops its `agent` is refused: an
            // omitted field is not the person. `validate` refuses it, and so does
            // every path that checks a statement before believing it.
            let omitted: VerifiedActor = serde_json::from_value(
                serde_json::json!({"AuthenticatedLocal":{"root":zeros,"device":zeros}}),
            )
            .unwrap();
            assert_eq!(
                omitted.validate(),
                Err("invalid authenticated local admission agent".into())
            );
        }
    }

    const CANONICAL_JSON_CORPUS: &[(&str, &str, &[u8])] = &[
        (
            "nested_unsorted",
            r#"{"z":{"b":2,"a":1},"a":{"d":{"y":true,"x":false},"c":[3,2,1]},"m":0}"#,
            br#"{"a":{"c":[3,2,1],"d":{"x":false,"y":true}},"m":0,"z":{"a":1,"b":2}}"#,
        ),
        (
            "utf8_vs_utf16_key_order",
            r#"{"\uD83D\uDE00":4,"\uE000":3,"é":2,"z":1}"#,
            b"{\"z\":1,\"\xc3\xa9\":2,\"\xee\x80\x80\":3,\"\xf0\x9f\x98\x80\":4}",
        ),
        (
            "non_bmp_vs_bmp_high_key_order",
            r#"{"\uD83D\uDE80":1,"\uFFFF":2,"\uE000":3}"#,
            b"{\"\xee\x80\x80\":3,\"\xef\xbf\xbf\":2,\"\xf0\x9f\x9a\x80\":1}",
        ),
        (
            "ascii_empty_prefix_key_order",
            r#"{"b":1,"":2,"a":3,"ab":4,"B":5,"A":6}"#,
            br#"{"":2,"A":6,"B":5,"a":3,"ab":4,"b":1}"#,
        ),
        (
            "nested_ten_keys",
            r#"{"k9":9,"k8":8,"k7":{"j9":9,"j8":8,"j7":7,"j6":6,"j5":5,"j4":4,"j3":3,"j2":2,"j1":1,"j0":0},"k6":6,"k5":5,"k4":4,"k3":3,"k2":2,"k1":1,"k0":0}"#,
            br#"{"k0":0,"k1":1,"k2":2,"k3":3,"k4":4,"k5":5,"k6":6,"k7":{"j0":0,"j1":1,"j2":2,"j3":3,"j4":4,"j5":5,"j6":6,"j7":7,"j8":8,"j9":9},"k8":8,"k9":9}"#,
        ),
        ("empty_object", "{}", b"{}"),
        ("empty_array", "[]", b"[]"),
        ("empty_string", r#""""#, br#""""#),
        (
            "line_seps_fffd_and_solidus",
            r#""\u2028\u2029\ufffd/""#,
            b"\"\xe2\x80\xa8\xe2\x80\xa9\xef\xbf\xbd/\"",
        ),
        (
            "all_32_control_bytes_value",
            r#""\u0000\u0001\u0002\u0003\u0004\u0005\u0006\u0007\u0008\u0009\u000a\u000b\u000c\u000d\u000e\u000f\u0010\u0011\u0012\u0013\u0014\u0015\u0016\u0017\u0018\u0019\u001a\u001b\u001c\u001d\u001e\u001f""#,
            br#""\u0000\u0001\u0002\u0003\u0004\u0005\u0006\u0007\b\t\n\u000b\f\r\u000e\u000f\u0010\u0011\u0012\u0013\u0014\u0015\u0016\u0017\u0018\u0019\u001a\u001b\u001c\u001d\u001e\u001f""#,
        ),
        (
            "all_32_control_bytes_key",
            r#"{"a":2,"\u0000\u0001\u0002\u0003\u0004\u0005\u0006\u0007\u0008\u0009\u000a\u000b\u000c\u000d\u000e\u000f\u0010\u0011\u0012\u0013\u0014\u0015\u0016\u0017\u0018\u0019\u001a\u001b\u001c\u001d\u001e\u001f":1}"#,
            br#"{"\u0000\u0001\u0002\u0003\u0004\u0005\u0006\u0007\b\t\n\u000b\f\r\u000e\u000f\u0010\u0011\u0012\u0013\u0014\u0015\u0016\u0017\u0018\u0019\u001a\u001b\u001c\u001d\u001e\u001f":1,"a":2}"#,
        ),
        (
            "escaped_and_unicode_string",
            r#""q\"b\\c\u0000\u0001\u0007\u0008\u0009\u000a\u000b\u000c\u000d\u000e\u001f\u007f café 日本語 🚀""#,
            b"\"q\\\"b\\\\c\\u0000\\u0001\\u0007\\b\\t\\n\\u000b\\f\\r\\u000e\\u001f\x7f caf\xc3\xa9 \xe6\x97\xa5\xe6\x9c\xac\xe8\xaa\x9e \xf0\x9f\x9a\x80\"",
        ),
        (
            "escaped_keys_object",
            r#"{"q\"b\\c\u0000\b\t\n\f\r\u001f\u007f":2,"a":1}"#,
            b"{\"a\":1,\"q\\\"b\\\\c\\u0000\\b\\t\\n\\f\\r\\u001f\x7f\":2}",
        ),
        ("i64_min", "-9223372036854775808", b"-9223372036854775808"),
        ("i64_max", "9223372036854775807", b"9223372036854775807"),
        (
            "u64_above_i64_max",
            "9223372036854775808",
            b"9223372036854775808",
        ),
        ("u64_max", "18446744073709551615", b"18446744073709551615"),
        ("zero", "0", b"0"),
        ("negative_zero", "-0.0", b"-0.0"),
        ("float_whole", "1.0", b"1.0"),
        ("float_tenth", "0.1", b"0.1"),
        ("float_1e20", "1e20", b"1e+20"),
        ("float_1e21", "1e21", b"1e+21"),
        ("float_1e_neg5", "1e-5", b"0.00001"),
        ("float_1e_neg7", "1e-7", b"1e-7"),
        ("float_min_subnormal", "5e-324", b"5e-324"),
        (
            "float_max_finite",
            "1.7976931348623157e308",
            b"1.7976931348623157e+308",
        ),
        ("float_1e15", "1e15", b"1000000000000000.0"),
        ("float_0_3_ulp", "0.30000000000000004", b"0.30000000000000004"),
        ("bool_true", "true", b"true"),
        ("bool_false", "false", b"false"),
        ("null", "null", b"null"),
        (
            "mixed_array",
            r#"[null,true,false,0,-1,1.0,0.1,-0.0,"a\"b",[],{},{"b":2,"a":1}]"#,
            br#"[null,true,false,0,-1,1.0,0.1,-0.0,"a\"b",[],{},{"a":1,"b":2}]"#,
        ),
    ];

    #[test]
    fn canonical_evidence_pins_canonical_bytes_for_shared_corpus() {
        const PINNED_NESTED_INTENT_DIGEST: [u8; 32] = [
            0x0d, 0x59, 0xbe, 0x96, 0xd4, 0x62, 0x6b, 0xca, 0xf4, 0x48, 0xd1, 0xa7, 0xd8, 0x01,
            0x08, 0x5e, 0x3b, 0xc4, 0xfe, 0x22, 0x8c, 0xf2, 0xc5, 0xa9, 0xc0, 0x9a, 0x00, 0xbb,
            0x6f, 0x72, 0x92, 0x7e,
        ];

        for &(name, input, expected) in CANONICAL_JSON_CORPUS {
            let value: Value = serde_json::from_str(input).unwrap();
            let mut hasher = blake3::Hasher::new();
            hasher.update(INTENT_DOMAIN);
            hasher.update(expected);
            assert_eq!(
                intent_digest(&value).unwrap(),
                *hasher.finalize().as_bytes(),
                "row {name}"
            );
        }
        let nested: Value = serde_json::from_str(CANONICAL_JSON_CORPUS[0].1).unwrap();
        assert_eq!(intent_digest(&nested).unwrap(), PINNED_NESTED_INTENT_DIGEST);

        let signed = signing_bytes(&facts(), &[0; 32]).unwrap();
        assert_eq!(
            signed,
            [
                SIGN_DOMAIN,
                br#"{"facts":{"action":"append","actor":{"AuthenticatedLocal":{"agent":"agent","device":[4,4,4,4,4,4,4,4,4,4,4,4,4,4,4,4,4,4,4,4,4,4,4,4,4,4,4,4,4,4,4,4],"root":[3,3,3,3,3,3,3,3,3,3,3,3,3,3,3,3,3,3,3,3,3,3,3,3,3,3,3,3,3,3,3,3]}},"actor_id":{"agent":"agent"},"app_id":"notes","document_id":"record","host":"00000000-0000-0000-0000-000000000001","invocation":"00000000-0000-0000-0000-000000000002","resource":"document/body","revision":"revision","workspace":"workspace"},"intent_commitment":[0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0]}"#,
            ]
            .concat()
        );
    }
}
