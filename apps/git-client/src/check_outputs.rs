//! Bounded references to owner-selected check outputs in the existing record.
//! The record reports captured identities, not authority or verdicts about their contents.
use serde::{Deserialize, Serialize};
use ui::automerge::transaction::Transactable as _;
use ui::automerge::{AutoCommit, ObjId, ROOT};
use ui::invocation_scope::{ApprovedCheckOutput, ApprovedCheckReceipt};
use ui::tilo_actions::{ActionError, Resource};
use ui::tilo_resource::Effect as _;
const KEY: &str = "selected_check_outputs";
const MAX_RECORDS: usize = 16;
const MAX_BYTES: usize = 8192;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct CheckOutputs {
    pub invocation: String,
    pub check_id: String,
    pub candidate_source_sha256: String,
    pub outputs: Vec<ApprovedCheckOutput>,
}
impl CheckOutputs {
    pub(crate) fn from_receipt(receipt: &ApprovedCheckReceipt) -> Result<Self, String> {
        receipt.encode()?;
        Ok(Self {
            invocation: receipt.invocation.clone(),
            check_id: receipt.check_id.clone(),
            candidate_source_sha256: receipt
                .candidate_source_sha256
                .clone()
                .ok_or("output receipt has no fresh candidate")?,
            outputs: receipt.selected_outputs.clone(),
        })
    }
    fn valid(&self) -> bool {
        let identity = |value: &str| {
            !value.is_empty()
                && !value.starts_with('-')
                && value.len() <= 128
                && value
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || b"._-".contains(&byte))
        };
        self.invocation != "00000000-0000-0000-0000-000000000000"
            && self.invocation.len() == 36
            && self.invocation.bytes().enumerate().all(|(index, byte)| {
                if matches!(index, 8 | 13 | 18 | 23) {
                    byte == b'-'
                } else {
                    byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase()
                }
            })
            && identity(&self.check_id)
            && ui::tilo_actions::content_resource(&self.candidate_source_sha256).is_some()
            && !self.outputs.is_empty()
            && self.outputs.len() <= 8
            && self.outputs.iter().all(ApprovedCheckOutput::valid)
            && self
                .outputs
                .iter()
                .map(|output| &output.id)
                .collect::<std::collections::BTreeSet<_>>()
                .len()
                == self.outputs.len()
            && self
                .outputs
                .iter()
                .try_fold(0_u64, |sum, output| sum.checked_add(output.size_bytes))
                .is_some_and(|sum| sum <= 32 * 1024 * 1024)
    }
}
fn map(doc: &AutoCommit) -> Option<ObjId> {
    use ui::automerge::ReadDoc as _;
    doc.get(ROOT, KEY)
        .ok()
        .flatten()
        .and_then(|(value, object)| {
            matches!(
                value,
                ui::automerge::Value::Object(ui::automerge::ObjType::Map)
            )
            .then_some(object)
        })
}
pub(crate) fn read(doc: &AutoCommit) -> Vec<CheckOutputs> {
    use ui::automerge::ReadDoc as _;
    let Some(map) = map(doc) else {
        return Vec::new();
    };
    doc.keys(&map)
        .take(MAX_RECORDS)
        .filter_map(|key| {
            let json = ui::doc::read_str(doc, &map, &key)?;
            if json.len() > MAX_BYTES {
                return None;
            }
            let entry: CheckOutputs = serde_json::from_str(&json).ok()?;
            (entry.valid() && entry.invocation == key).then_some(entry)
        })
        .collect()
}
pub(crate) fn retain(
    doc: &mut AutoCommit,
    repository: Option<&str>,
    json: &str,
) -> Result<(), ActionError> {
    use ui::automerge::ReadDoc as _;
    if ui::doc::read_str(doc, &ROOT, crate::doc::keys::REPOSITORY).as_deref() != repository {
        return Err(ActionError::Refused(
            "repository changed before check output publication".into(),
        ));
    }
    if json.len() > MAX_BYTES {
        return Err(ActionError::Refused("check outputs exceed bound".into()));
    }
    let entry: CheckOutputs =
        serde_json::from_str(json).map_err(|error| ActionError::Refused(error.to_string()))?;
    if !entry.valid() {
        return Err(ActionError::Refused(
            "invalid check output references".into(),
        ));
    }
    let map = match map(doc) {
        Some(map) => map,
        None => doc.put_object(ROOT, KEY, ui::automerge::ObjType::Map)?,
    };
    if let Some(previous) = ui::doc::read_str(doc, &map, &entry.invocation) {
        if previous == json {
            return Ok(());
        }
        return Err(ActionError::Refused(
            "check output invocation cannot change".into(),
        ));
    }
    doc.put(&map, entry.invocation.as_str(), json)?;
    let excess: Vec<_> = doc.keys(&map).skip(MAX_RECORDS).collect();
    for key in excess {
        doc.delete(&map, key)?;
    }
    Ok(())
}
pub(crate) fn clear(doc: &mut AutoCommit) -> Result<(), ActionError> {
    doc.delete(ROOT, KEY)?;
    Ok(())
}
pub(crate) fn resource_id(invocation: &str, output: &str) -> String {
    format!("git_client.check-output:{invocation}:{output}")
}
pub(crate) fn child(entries: &[CheckOutputs], id: &str) -> Option<Resource> {
    let (invocation, output_id) = id
        .strip_prefix("git_client.check-output:")?
        .split_once(':')?;
    let entry = entries
        .iter()
        .find(|entry| entry.invocation == invocation)?;
    let output = entry.outputs.iter().find(|output| output.id == output_id)?;
    Some(
        Resource::new(
            id,
            "git_client.check-output",
            format!("{} / {}", entry.check_id, output.id),
        )
        .with_action(ui::framework::read_item(id))
        .with_action(
            crate::actions::GitClientEffect::TransferCheckOutput {
                invocation: invocation.into(),
                output_id: output_id.into(),
            }
            .bound(),
        ),
    )
}
pub(crate) fn children(entries: &[CheckOutputs]) -> Vec<Resource> {
    entries
        .iter()
        .flat_map(|entry| {
            entry
                .outputs
                .iter()
                .filter_map(|output| child(entries, &resource_id(&entry.invocation, &output.id)))
        })
        .collect()
}
pub(crate) fn metadata(entries: &[CheckOutputs], id: &str) -> Option<serde_json::Value> {
    let (invocation, output_id) = id
        .strip_prefix("git_client.check-output:")?
        .split_once(':')?;
    let entry = entries
        .iter()
        .find(|entry| entry.invocation == invocation)?;
    let output = entry.outputs.iter().find(|output| output.id == output_id)?;
    Some(
        serde_json::json!({"kind":"git_client.check-output", "custody":"reported_reference", "resource":id,"invocation":entry.invocation,"check_id":entry.check_id,"candidate_source_sha256":entry.candidate_source_sha256,"output":output}),
    )
}
