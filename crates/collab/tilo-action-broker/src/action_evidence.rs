//! Signed semantic admission, not proof of execution or successful completion.
//!
//! Retain the entire [`AdmissionEvidence`] only in private history or the
//! host-private execution journal. Publish only [`AdmissionReceipt`].
//! An opening permits offline guesses of the
//! intent and must be disclosed only through original-owner observation.
//! Certificate root trust, room/host authority, revocation, certificate time
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
        #[serde(deserialize_with = "required_agent")]
        agent: Option<String>,
    },
    Enrolled {
        root: [u8; 32],
        device: [u8; 32],
        task: String,
        run: String,
        channel: String,
    },
    NativeTask {
        root: [u8; 32],
        device: [u8; 32],
        agent: String,
        task: String,
        epoch: u64,
    },
}

fn required_agent<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<String>, D::Error> {
    Option::<String>::deserialize(deserializer)
}

/// Public routing and identity facts. Never put arguments, context, cause,
/// results, or other private intent contents in these fields.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AdmissionFacts {
    pub host: Uuid,
    pub room: String,
    pub invocation: InvocationId,
    pub actor: VerifiedActor,
    /// Host-verified effective actor, including a delegated agent's identity.
    pub actor_id: String,
    pub app_id: String,
    pub record_id: String,
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
    let mut bytes = b"tilo-semantic-admission-reference-v1\0".to_vec();
    bytes.extend(canonical_bytes(
        &serde_json::to_value(receipt).map_err(|_| "admission encoding failed")?,
    )?);
    Ok(*blake3::hash(&bytes).as_bytes())
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
    let (certificate, signature) =
        crate::evidence_signer::sign(&admission.facts.room, &terminal_bytes(&digest, category)?)?;
    Ok(TerminalReceipt {
        admission_digest: digest,
        category,
        certificate,
        signature: signature.to_vec(),
    })
}

/// Apply the same independent signer trust policy as for [`verify`].
///
/// # Errors
/// Returns an error if admission or terminal encoding fails, the terminal
/// references another admission, or its certificate, signer or signature is invalid.
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
    mint_with_signer(facts, exact_intent, crate::evidence_signer::sign)
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
    let (_, signature) = crate::evidence_signer::sign(&facts.room, &seed)?;
    mint_with_nonce(
        facts,
        exact_intent,
        *blake3::hash(&signature).as_bytes(),
        crate::evidence_signer::sign,
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
                .map(crate::collab_broker_capnp::admission_request_digest),
        }),
    };
    let intent_commitment = opening.commitment();
    let bytes = signing_bytes(&facts, &intent_commitment)?;
    let (certificate, signature) = signer(&facts.room, &bytes)?;
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
/// which root/device is trusted for this host and room, including revocation
/// and certificate validity policy. Supply expected facts from trusted state,
/// not by copying them from an untrusted receipt. Replays with identical facts
/// still verify: the durable journal owns execution uniqueness and dispatch.
/// Local reports instead use content-bound capture identities, never dispatch.
///
/// # Errors
/// Returns an error if the facts differ, encoding fails, or the certificate,
/// signer key or receipt signature is invalid.
pub fn verify(receipt: &AdmissionReceipt, expected_facts: &AdmissionFacts) -> Result<(), String> {
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

impl AdmissionEvidence {
    /// The only shareable projection; never serialize `self` for public use.
    pub fn public_receipt(&self) -> &AdmissionReceipt {
        &self.receipt
    }

    /// Validate recovered private journal data without retaining plaintext.
    /// Caller must also apply signer trust policy as documented on [`verify`].
    ///
    /// # Errors
    /// Returns an error if receipt verification fails or the private opening
    /// does not match the receipt's intent commitment.
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
    ///
    /// # Errors
    /// Returns an error if the observer is not the original owner.
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
    ///
    /// # Errors
    /// Returns an error if intent encoding fails, the intent digest differs,
    /// or this opening does not match the receipt's intent commitment.
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

fn signing_bytes(facts: &AdmissionFacts, commitment: &[u8; 32]) -> Result<Vec<u8>, String> {
    let mut bytes = SIGN_DOMAIN.to_vec();
    bytes.extend(canonical_bytes(&serde_json::json!({
        "facts": facts,
        "intent_commitment": commitment,
    }))?);
    Ok(bytes)
}

fn retained_digest(intent: &Value) -> Result<[u8; 32], String> {
    let mut retained = intent.clone();
    if let Some(fields) = retained.as_object_mut() {
        fields.remove("request");
    }
    let mut hash = blake3::Hasher::new_derive_key("tilo semantic admission retained intent v1");
    hash.update(&canonical_bytes(&retained)?);
    Ok(*hash.finalize().as_bytes())
}

fn intent_digest(intent: &Value) -> Result<[u8; 32], String> {
    let mut hash = blake3::Hasher::new();
    hash.update(INTENT_DOMAIN);
    hash.update(&canonical_bytes(intent)?);
    Ok(*hash.finalize().as_bytes())
}

// Sort recursively even when serde_json's preserve_order feature is enabled by
// another workspace member. Arrays retain order; absent and null stay distinct.
// This is the v1 JSON-value encoding, not a claim of RFC 8785 conformance.
fn canonical_bytes(value: &Value) -> Result<Vec<u8>, String> {
    fn sorted(value: &Value) -> Value {
        match value {
            Value::Object(map) => {
                let mut entries: Vec<_> = map.iter().collect();
                entries.sort_unstable_by_key(|(key, _)| *key);
                Value::Object(
                    entries
                        .into_iter()
                        .map(|(k, v)| (k.clone(), sorted(v)))
                        .collect(),
                )
            }
            Value::Array(values) => Value::Array(values.iter().map(sorted).collect()),
            other => other.clone(),
        }
    }
    serde_json::to_vec(&sorted(value)).map_err(|_| "admission encoding failed".into())
}

#[cfg(test)]
mod tests {
    use ed25519_dalek::{Signer as _, SigningKey};
    use tilo_user_identity::{DeviceCapabilities, sign_device_cert};

    use super::*;

    fn facts() -> AdmissionFacts {
        AdmissionFacts {
            host: Uuid::from_u128(1),
            room: "room".into(),
            invocation: InvocationId(Uuid::from_u128(2)),
            actor: VerifiedActor::AuthenticatedLocal {
                root: [3; 32],
                device: [4; 32],
                agent: Some("agent".into()),
            },
            actor_id: "agent:agent".into(),
            app_id: "notes".into(),
            record_id: "record".into(),
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
        };
        sign_device_cert(&mut certificate, &root).unwrap();
        mint_with_signer(facts(), intent, |room, bytes| {
            assert_eq!(room, "room");
            assert!(bytes.starts_with(SIGN_DOMAIN));
            Ok((certificate, device.sign(bytes).to_bytes()))
        })
        .unwrap()
    }

    #[test]
    fn privacy_randomization_and_private_round_trip() {
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

    #[test]
    fn every_intent_field_and_private_opening_are_bound() {
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

    #[test]
    fn retry_digest_presence_and_contents_are_private_and_bound() {
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
                .map(crate::collab_broker_capnp::admission_request_digest);
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

    #[test]
    fn historical_opening_keeps_its_original_commitment_encoding() {
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

    #[test]
    fn substitution_and_cross_scope_replay_fail() {
        let evidence = evidence();
        let original = serde_json::to_value(facts()).unwrap();
        for (field, value) in original.as_object().unwrap() {
            let mut changed = original.clone();
            changed[field] = match field.as_str() {
                "host" | "invocation" => serde_json::to_value(Uuid::from_u128(99)).unwrap(),
                "actor" => serde_json::to_value(VerifiedActor::NativeTask {
                    root: [7; 32],
                    device: [8; 32],
                    agent: "other".into(),
                    task: "task:other".into(),
                    epoch: 1,
                })
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

    #[test]
    fn signatures_certificates_and_commitments_cannot_be_replaced() {
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

    #[test]
    fn canonical_intent_covers_nested_order_null_and_arrays() {
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
                serde_json::json!({"Local":{"actor":"person"}})
            )
            .is_err()
        );
        let zeros = [0u8; 32];
        assert!(
            serde_json::from_value::<VerifiedActor>(
                serde_json::json!({"AuthenticatedLocal":{"root":zeros,"device":zeros}})
            )
            .is_err()
        );
    }
}
