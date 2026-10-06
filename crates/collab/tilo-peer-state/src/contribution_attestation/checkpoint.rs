use super::delegation::{validate_certificate, verify_contribution_delegation};
use ed25519_dalek::{Signer as _, Verifier as _};
use neighbour_wire::contribution::{
    CONTRIBUTION_CHECKPOINT_VERSION, ContributionAttestorV1, ContributionCheckpointBodyV1,
    ContributionOriginalPolicyV1, ContributionPurposeV1, DocumentContributionCheckpointV1,
    MAX_CONTRIBUTION_BASIS_HEADS, MAX_CONTRIBUTION_CHANGES_PER_OPERATION,
    MAX_CONTRIBUTION_CHECKPOINT_BYTES, MAX_CONTRIBUTION_RECEIPTS,
};
use std::collections::BTreeSet;
use tilo_user_identity::{DeviceCert, RevocationList};

const DOMAIN: &[u8] = b"tilo/document-contribution-checkpoint/v1\0";
const ROOM_BASIS_DOMAIN: &[u8] = b"tilo/contribution-room-basis/v1\0";

/// Digest the canonical room and exact sorted Automerge heads. Heads commit
/// the graph; transient document save encodings are not signing inputs.
///
/// # Errors
/// The room is not canonical or the head set is empty, oversized or unsorted.
pub fn contribution_room_basis_digest(room: &str, heads: &[[u8; 32]]) -> Result<[u8; 32], String> {
    let room_id = uuid::Uuid::parse_str(room).map_err(|_| "invalid contribution basis room")?;
    if room_id.to_string() != room
        || heads.is_empty()
        || heads.len() > MAX_CONTRIBUTION_BASIS_HEADS
        || !strictly_sorted(heads)
        || heads.contains(&[0; 32])
    {
        return Err("invalid contribution governed-room basis".into());
    }
    let mut hash = blake3::Hasher::new();
    hash.update(ROOM_BASIS_DOMAIN);
    hash.update(room_id.as_bytes());
    let count = u32::try_from(heads.len()).map_err(|_| "contribution basis head count overflow")?;
    hash.update(&count.to_le_bytes());
    for head in heads {
        hash.update(head);
    }
    Ok(*hash.finalize().as_bytes())
}

fn valid_text(value: &str, max: usize, allow_empty: bool) -> bool {
    (allow_empty || !value.is_empty())
        && value.len() <= max
        && value.trim() == value
        && !value.chars().any(char::is_control)
}

fn strictly_sorted<T: Ord>(values: &[T]) -> bool {
    values.windows(2).all(|pair| match pair {
        [left, right] => left < right,
        _ => false,
    })
}

fn strictly_sorted_by<T>(values: &[T], less: impl Fn(&T, &T) -> bool) -> bool {
    values.windows(2).all(|pair| match pair {
        [left, right] => less(left, right),
        _ => false,
    })
}

fn validate_header(body: &ContributionCheckpointBodyV1) -> Result<(), String> {
    let canonical_room = uuid::Uuid::parse_str(&body.room)
        .map_err(|_| "contribution checkpoint room is not a UUID")?
        .to_string();
    if body.version != CONTRIBUTION_CHECKPOINT_VERSION
        || body.room != canonical_room
        || body.generation == 0
        || body.authored_at_ns == 0
        || (body.generation == 1) == body.previous_digest.is_some()
        || body.previous_digest == Some([0; 32])
        || body.room_bases.is_empty()
        || tilo_relay_protocol::room_workspace_id(&body.room).map_err(|error| error.to_string())?
            != body.workspace_id
    {
        return Err("invalid contribution checkpoint binding".into());
    }
    Ok(())
}

fn validate_tables(body: &ContributionCheckpointBodyV1) -> Result<(), String> {
    if !strictly_sorted_by(&body.certificates, |left, right| {
        left.device_public_key < right.device_public_key
    }) || !strictly_sorted(&body.contracts)
        || !strictly_sorted(&body.room_bases)
        || body
            .contracts
            .iter()
            .any(|contract| contract.digest == [0; 32] || contract.permission > 5)
    {
        return Err("noncanonical contribution checkpoint tables".into());
    }
    for certificate in &body.certificates {
        tilo_user_identity::verify_device_cert(certificate).map_err(|error| error.to_string())?;
    }
    for basis in &body.room_bases {
        if contribution_room_basis_digest(&body.room, &basis.heads)? != basis.digest {
            return Err("contribution governed-room basis digest mismatch".into());
        }
    }
    Ok(())
}

struct ReferencedTables {
    certificates: BTreeSet<u32>,
    contracts: BTreeSet<u32>,
    room_bases: BTreeSet<u32>,
    changes: BTreeSet<[u8; 32]>,
    receipt_count: usize,
}

impl ReferencedTables {
    fn new() -> Self {
        Self {
            certificates: BTreeSet::new(),
            contracts: BTreeSet::new(),
            room_bases: BTreeSet::new(),
            changes: BTreeSet::new(),
            receipt_count: 0,
        }
    }
}

fn validate_receipt(
    body: &ContributionCheckpointBodyV1,
    operation: &neighbour_wire::contribution::ContributionOperationV1,
    receipt: &neighbour_wire::contribution::ContributionReceiptV1,
    referenced: &mut ReferencedTables,
) -> Result<(), String> {
    if receipt.change_hash == [0; 32]
        || !referenced.changes.insert(receipt.change_hash)
        || !valid_text(&receipt.actor, 256, false)
        || receipt
            .participant
            .as_deref()
            .is_some_and(|value| !valid_text(value, 256, false))
        || receipt.source_invocation == [0; 16]
        || !valid_text(&receipt.action, 256, false)
        || usize::try_from(receipt.original_certificate)
            .map_or(true, |index| index >= body.certificates.len())
        || usize::try_from(receipt.contract).map_or(true, |index| index >= body.contracts.len())
        || matches!(
            receipt.original_policy,
            ContributionOriginalPolicyV1::ManagedAgent
        ) != receipt.purpose.is_some()
    {
        return Err("invalid contribution checkpoint receipt".into());
    }
    referenced.certificates.insert(receipt.original_certificate);
    referenced.contracts.insert(receipt.contract);
    match &receipt.purpose {
        Some(ContributionPurposeV1::Task(task)) => {
            if !valid_text(&task.task_id, 256, false)
                || !valid_text(&task.agent, 128, false)
                || task.agent != receipt.actor
                || task.run_epoch == 0
            {
                return Err("invalid contribution task frontier".into());
            }
        }
        Some(ContributionPurposeV1::ConversationReply(reply)) => {
            let request = uuid::Uuid::from_bytes(reply.request_invocation);
            if reply.room != body.room
                || operation.app != "so.tilo.chat"
                || reply.chat_record != operation.record
                || !valid_text(&reply.chat_record, 256, false)
                || !valid_text(&reply.channel, 256, false)
                || !valid_text(&reply.agent, 128, false)
                || reply.agent != receipt.actor
                || request.is_nil()
                || reply.delivery_invocation == [0; 16]
                || reply.message != format!("m_{}", request.simple())
                || receipt.action != "complete-agent-message"
            {
                return Err("invalid contribution Conversation reply frontier".into());
            }
        }
        None => {}
    }
    Ok(())
}

fn validate_operations(body: &ContributionCheckpointBodyV1) -> Result<(), String> {
    if !strictly_sorted(&body.operations) {
        return Err("noncanonical contribution checkpoint operations".into());
    }
    let mut referenced = ReferencedTables::new();
    for operation in &body.operations {
        if !valid_text(&operation.app, 256, false)
            || !valid_text(&operation.record, 4096, true)
            || operation.artifact == [0; 16]
            || operation.operation_id == [0; 32]
            || operation.outer_signer == [0; 32]
            || usize::try_from(operation.room_basis)
                .map_or(true, |index| index >= body.room_bases.len())
            || operation.receipts.is_empty()
            || operation.receipts.len() > MAX_CONTRIBUTION_CHANGES_PER_OPERATION
            || !strictly_sorted(&operation.receipts)
        {
            return Err("invalid contribution checkpoint operation".into());
        }
        referenced.room_bases.insert(operation.room_basis);
        referenced.receipt_count = referenced
            .receipt_count
            .checked_add(operation.receipts.len())
            .filter(|count| *count <= MAX_CONTRIBUTION_RECEIPTS)
            .ok_or("contribution checkpoint receipt limit exceeded")?;
        for receipt in &operation.receipts {
            validate_receipt(body, operation, receipt, &mut referenced)?;
        }
    }
    if referenced.certificates.len() != body.certificates.len()
        || referenced.contracts.len() != body.contracts.len()
        || referenced.room_bases.len() != body.room_bases.len()
    {
        return Err("contribution checkpoint contains unused table entries".into());
    }
    Ok(())
}

fn validate_body(body: &ContributionCheckpointBodyV1) -> Result<(), String> {
    validate_header(body)?;
    validate_tables(body)?;
    validate_operations(body)
}

#[derive(serde::Serialize)]
struct SigningView<'a> {
    body: &'a ContributionCheckpointBodyV1,
    attestor: &'a ContributionAttestorV1,
}

fn digest(
    body: &ContributionCheckpointBodyV1,
    attestor: &ContributionAttestorV1,
) -> Result<[u8; 32], String> {
    let bytes = postcard::to_allocvec(&SigningView { body, attestor })
        .map_err(|error| error.to_string())?;
    let mut hash = blake3::Hasher::new();
    hash.update(DOMAIN);
    hash.update(&bytes);
    Ok(*hash.finalize().as_bytes())
}

fn attestor<'a>(
    body: &ContributionCheckpointBodyV1,
    proof: &'a ContributionAttestorV1,
    authority: &DeviceCert,
    revocations: &RevocationList,
    accepted_at_ns: u64,
) -> Result<&'a DeviceCert, String> {
    match proof {
        ContributionAttestorV1::Governor { certificate } => {
            if certificate.as_ref() != authority {
                return Err("contribution checkpoint signer is not the exact governor".into());
            }
            validate_certificate(certificate, revocations, body.authored_at_ns)?;
            validate_certificate(certificate, revocations, accepted_at_ns)?;
            Ok(certificate)
        }
        ContributionAttestorV1::DelegatedHost {
            certificate,
            delegation,
        } => {
            verify_contribution_delegation(delegation, authority, revocations, accepted_at_ns)?;
            validate_certificate(certificate, revocations, body.authored_at_ns)?;
            if certificate.as_ref() != &delegation.delegate
                || delegation.room != body.room
                || delegation.workspace_id != body.workspace_id
            {
                return Err("contribution checkpoint differs from delegated scope".into());
            }
            Ok(certificate)
        }
    }
}

fn validate_encoded_bound(checkpoint: &DocumentContributionCheckpointV1) -> Result<(), String> {
    let bytes = postcard::to_allocvec(checkpoint).map_err(|error| error.to_string())?;
    if bytes.len() > MAX_CONTRIBUTION_CHECKPOINT_BYTES {
        return Err("contribution checkpoint exceeds encoded limit".into());
    }
    Ok(())
}

/// Sign a canonical, bounded contribution checkpoint.
///
/// # Errors
/// The body, authority proof, signing custody or encoded bound is invalid.
pub fn sign_contribution_checkpoint(
    body: ContributionCheckpointBodyV1,
    proof: ContributionAttestorV1,
    authority: &DeviceCert,
    revocations: &RevocationList,
    key: &ed25519_dalek::SigningKey,
) -> Result<DocumentContributionCheckpointV1, String> {
    validate_body(&body)?;
    let certificate = attestor(&body, &proof, authority, revocations, body.authored_at_ns)?;
    if key.verifying_key().to_bytes() != certificate.device_public_key {
        return Err("contribution checkpoint signing custody changed".into());
    }
    let signature =
        collab_types::Signature::from_bytes(key.sign(&digest(&body, &proof)?).to_bytes());
    let checkpoint = DocumentContributionCheckpointV1 {
        body,
        attestor: proof,
        signature,
    };
    validate_encoded_bound(&checkpoint)?;
    Ok(checkpoint)
}

/// Verify a newly observed canonical checkpoint and return its stable digest.
/// A delegated signer must still be valid at `accepted_at_ns`; its self-claimed
/// authored time cannot backdate around a revocation.
///
/// This verifies shape and signature only, not the referenced signed operation
/// or historical room graph. A receiver must correlate and materialize those
/// exact bases before treating any receipt as verified Agent attribution.
///
/// # Errors
/// The body, current authority proof, signature or encoded bound is invalid.
pub fn verify_contribution_checkpoint_signature(
    checkpoint: &DocumentContributionCheckpointV1,
    authority: &DeviceCert,
    revocations: &RevocationList,
    accepted_at_ns: u64,
) -> Result<[u8; 32], String> {
    validate_body(&checkpoint.body)?;
    validate_encoded_bound(checkpoint)?;
    let certificate = attestor(
        &checkpoint.body,
        &checkpoint.attestor,
        authority,
        revocations,
        accepted_at_ns,
    )?;
    let digest = digest(&checkpoint.body, &checkpoint.attestor)?;
    let key = ed25519_dalek::VerifyingKey::from_bytes(&certificate.device_public_key)
        .map_err(|_| "invalid contribution checkpoint signer key")?;
    key.verify(
        &digest,
        &ed25519_dalek::Signature::from_bytes(checkpoint.signature.as_bytes()),
    )
    .map_err(|_| "contribution checkpoint signature did not verify")?;
    Ok(digest)
}
