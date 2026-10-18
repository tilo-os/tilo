//! Portable custody evidence for private directions added after a room checkpoint.
use super::super::{StatePeer, sanitized_baseline};
use crate::workspace_task::NativeTaskPrivateReferences;
use automerge::{
    AutoCommit, ObjType, ROOT, ReadDoc, ScalarValue, Value, transaction::Transactable,
};
use ed25519_dalek::{Signature, Signer, SigningKey, Verifier, VerifyingKey};
use serde::{Deserialize, Serialize};

#[cfg(test)]
#[path = "direction_references_tests.rs"]
mod tests;

const FIELD: &str = "private_task_direction_custody_v1";
const DOMAIN: &[u8] = b"tilo/private-task-direction-custody/v1\0";

#[derive(Serialize, Deserialize)]
struct DirectionCustody {
    task: String,
    revision: u64,
    reference: String,
    signature: Vec<u8>,
}

impl DirectionCustody {
    fn signing_bytes(
        &self,
        binding: &sanitized_baseline::BaselineBinding,
    ) -> Result<Vec<u8>, String> {
        let mut bytes = DOMAIN.to_vec();
        bytes.extend(
            postcard::to_allocvec(&(binding, &self.task, self.revision, &self.reference))
                .map_err(|error| error.to_string())?,
        );
        Ok(bytes)
    }
}

impl StatePeer {
    /// Add portable, governor-authenticated direction bindings from this chain.
    /// No reference-shaped plaintext or foreign signature grants custody.
    /// # Errors
    /// Refuses malformed evidence and conflicting authenticated bindings.
    pub fn merge_native_task_private_direction_references(
        &self,
        references: &mut NativeTaskPrivateReferences,
    ) -> Result<(), String> {
        merge(
            self.core
                .room_doc
                .as_deref()
                .ok_or("private task room absent")?,
            self.core.authority.device_public_key,
            references,
        )
    }

    /// Attest only exact direction bindings backed by the caller's private custody.
    /// The owner must retain the bodies and this candidate atomically before publication.
    /// # Errors
    /// Refuses foreign signing custody, absent checkpoints or malformed controls.
    pub fn attest_native_task_private_directions(
        &mut self,
        key: &SigningKey,
        references: &NativeTaskPrivateReferences,
    ) -> Result<(), String> {
        if key.verifying_key().to_bytes() != self.core.authority.device_public_key {
            return Err("private direction custody requires exact governor signing custody".into());
        }
        let room = self
            .core
            .room_doc
            .as_deref()
            .ok_or("private task room absent")?;
        let binding =
            sanitized_baseline::binding(room)?.ok_or("private direction checkpoint absent")?;
        let controls = crate::workspace_task::controls_of(room)?;
        let mut candidate = room.clone();
        let mut evidence = Vec::new();
        for (task, values) in references {
            let Some(control) = controls.get(task) else {
                continue;
            };
            for direction in &control.directions {
                if values.get(&direction.revision) != Some(&direction.text) {
                    continue;
                }
                let mut proof = DirectionCustody {
                    task: task.clone(),
                    revision: direction.revision,
                    reference: direction.text.clone(),
                    signature: Vec::new(),
                };
                proof.signature = key
                    .sign(&proof.signing_bytes(&binding)?)
                    .to_bytes()
                    .to_vec();
                evidence.push(proof);
            }
        }
        if evidence.is_empty() {
            return Ok(());
        }
        let map = match candidate
            .get(ROOT, FIELD)
            .map_err(|error| error.to_string())?
        {
            None => candidate
                .put_object(ROOT, FIELD, ObjType::Map)
                .map_err(|error| error.to_string())?,
            Some((Value::Object(ObjType::Map), map)) => map,
            Some(_) => return Err("private direction custody is not a map".into()),
        };
        let mut changed = false;
        for proof in evidence {
            let id = serde_json::to_string(&(&proof.task, proof.revision))
                .map_err(|error| error.to_string())?;
            let bytes = postcard::to_allocvec(&proof).map_err(|error| error.to_string())?;
            if matches!(candidate.get(&map, &id).map_err(|error| error.to_string())?,
                Some((Value::Scalar(value), _)) if matches!(value.as_ref(), ScalarValue::Bytes(old) if old == &bytes))
            {
                continue;
            }
            candidate
                .put(&map, id, ScalarValue::Bytes(bytes))
                .map_err(|error| error.to_string())?;
            changed = true;
        }
        if changed {
            candidate.commit();
            self.core.room_doc = Some(candidate.into());
        }
        Ok(())
    }
}

pub(super) fn merge(
    room: &AutoCommit,
    governor: [u8; 32],
    references: &mut NativeTaskPrivateReferences,
) -> Result<(), String> {
    let Some(binding) = sanitized_baseline::binding(room)? else {
        return Ok(());
    };
    let map = match room.get(ROOT, FIELD).map_err(|error| error.to_string())? {
        None => return Ok(()),
        Some((Value::Object(ObjType::Map), map)) => map,
        Some(_) => return Err("private direction custody is not a map".into()),
    };
    let key = VerifyingKey::from_bytes(&governor).map_err(|error| error.to_string())?;
    for row in room.map_range(&map, ..) {
        let Value::Scalar(value) = row.value.into_value() else {
            return Err("private direction custody is not bytes".into());
        };
        let ScalarValue::Bytes(bytes) = value.as_ref() else {
            return Err("private direction custody is not bytes".into());
        };
        let (proof, tail): (DirectionCustody, _) =
            postcard::take_from_bytes(bytes).map_err(|error| error.to_string())?;
        if !tail.is_empty() {
            return Err("trailing private direction custody bytes".into());
        }
        // Old proofs can survive a privacy migration, but cannot authorize a
        // reference in its successor chain. Invalid evidence grants no custody.
        if key
            .verify(
                &proof.signing_bytes(&binding)?,
                &Signature::from_slice(&proof.signature).map_err(|error| error.to_string())?,
            )
            .is_err()
        {
            continue;
        }
        let values = references.entry(proof.task).or_default();
        if values
            .get(&proof.revision)
            .is_some_and(|value| value != &proof.reference)
        {
            return Err("private direction differs from signed custody".into());
        }
        values.insert(proof.revision, proof.reference);
    }
    Ok(())
}
