//! Private local action checkpoints follow the exact document owner, not UI aliases.
use super::App;

#[derive(serde::Serialize, serde::Deserialize)]
pub(super) struct LocalActionCheckpoint {
    #[serde(default)]
    pub custody: Option<runtime_collab::committed_document::ConnectionSnapshot>,
    pub report: runtime_collab::LocalDocumentActionReport,
    #[serde(with = "checkpoint_row")]
    pub directory_row: Option<runtime_collab::ActionDirectoryRow>,
}

// Serialize the inert row without introducing a second wire protocol or
// changing the collaboration transport's public types.
#[derive(serde::Serialize, serde::Deserialize)]
#[serde(remote = "runtime_collab::ActionDirectoryRow")]
struct CheckpointRow {
    parent_resource: String,
    resource: String,
    resource_kind: String,
    resource_label: String,
    presentation: Option<tilo_actions::ResourcePresentation>,
    action: Option<String>,
    descriptor: Option<runtime_collab::ExportedAction>,
    has_children: bool,
    visibility: Option<tilo_actions::remote::RowVisibility>,
}

mod checkpoint_row {
    use super::CheckpointRow;
    use serde::{Deserialize, Serialize};
    #[derive(Serialize)]
    struct Borrowed<'a>(#[serde(with = "CheckpointRow")] &'a runtime_collab::ActionDirectoryRow);
    #[derive(Deserialize)]
    struct Owned(#[serde(with = "CheckpointRow")] runtime_collab::ActionDirectoryRow);
    #[allow(
        clippy::ref_option,
        reason = "serde's `with` adapters receive a reference to the field"
    )]
    pub fn serialize<S: serde::Serializer>(
        row: &Option<runtime_collab::ActionDirectoryRow>,
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        row.as_ref().map(Borrowed).serialize(serializer)
    }
    pub fn deserialize<'de, D: serde::Deserializer<'de>>(
        deserializer: D,
    ) -> Result<Option<runtime_collab::ActionDirectoryRow>, D::Error> {
        Ok(Option::<Owned>::deserialize(deserializer)?.map(|row| row.0))
    }
}

impl App {
    pub(super) fn stage_local_action_evidence(
        &mut self,
        route: &crate::collaboration::RouteSpec,
        capture: &crate::local_document_action::CapturedAction,
        receipt: Option<&str>,
    ) -> Result<(), String> {
        self.stage_local_action_checkpoint(route, capture, receipt, false)
    }

    pub(super) fn stage_local_action_checkpoint(
        &mut self,
        route: &crate::collaboration::RouteSpec,
        capture: &crate::local_document_action::CapturedAction,
        receipt: Option<&str>,
        rejected: bool,
    ) -> Result<(), String> {
        let report = capture.clone().into_report()?;
        let directory_row = self
            .checkpoint_local_directory_row(&report.revision, &report.resource, &report.action)
            .or_else(|| {
                self.local_action_checkpoint_entries(route)
                    .ok()?
                    .into_iter()
                    .find(|(checkpoint, _)| checkpoint.report.invocation == report.invocation)
                    .and_then(|(checkpoint, _)| checkpoint.directory_row)
            });
        let receipt = if rejected {
            Some("{\"local_capture_rejected\":true}")
        } else {
            receipt
        };
        let checkpoint = LocalActionCheckpoint {
            custody: capture.custody.clone(),
            report,
            directory_row,
        };
        let action = app_state::PrivateAction {
            id: *capture.invocation.as_bytes(),
            hashes: capture
                .original_heads
                .iter()
                .chain(&capture.mutation_hashes)
                .copied()
                .collect(),
            intent: if checkpoint.directory_row.is_some() || checkpoint.custody.is_some() {
                serde_json::to_vec(&checkpoint)
            } else {
                // Preserve the original intent bytes of older checkpoints.
                serde_json::to_vec(&checkpoint.report)
            }
            .map_err(|error| error.to_string())?,
            receipt: receipt.map(|receipt| receipt.as_bytes().to_vec()),
        };
        if let Some(entry) = self
            .document_routes
            .owners
            .iter()
            .find(|entry| &entry.route == route)
        {
            let owner = entry
                .owners
                .first()
                .ok_or("local action route has no owner")?;
            let (records, checkout) = owner.store_checkout();
            return records.record_private_action(&checkout.id, action);
        }
        if self
            .collaboration
            .manager
            .primary_route()
            .is_some_and(|w| &w.route == route)
        {
            if let Some(records) = &mut self.persistence.records {
                return records.record_active_private_action(action);
            }
            if let Some(writer) = &mut self.persistence.autosave {
                return writer.record_private_action(action);
            }
        }
        // An explicitly in-memory app retains its existing bounded evidence.
        if self.persistence.autosave.is_none() && self.persistence.records.is_none() {
            return Ok(());
        }
        Err("local action has no matching durable document owner".into())
    }

    /// Restart evidence for one exact live route. Admissions remain private
    /// status, not execution proof or permission to publish. An absent admission
    /// retains the original report and content-bound identity for observation retry.
    pub fn local_document_action_checkpoint(
        &self,
        route: &crate::collaboration::RouteSpec,
    ) -> Result<Vec<(runtime_collab::LocalDocumentActionReport, Option<String>)>, String> {
        Ok(self
            .local_action_checkpoint_entries(route)?
            .into_iter()
            .filter(|(checkpoint, receipt)| {
                checkpoint.custody.is_some()
                    && checkpoint.custody == self.collaboration.manager.connection_snapshot(route)
                    && receipt.as_deref() != Some("{\"local_capture_rejected\":true}")
            })
            .map(|(checkpoint, receipt)| (checkpoint.report, receipt))
            .collect())
    }

    pub(super) fn local_action_checkpoint_entries(
        &self,
        route: &crate::collaboration::RouteSpec,
    ) -> Result<Vec<(LocalActionCheckpoint, Option<String>)>, String> {
        let entries = if let Some(entry) = self
            .document_routes
            .owners
            .iter()
            .find(|entry| &entry.route == route)
        {
            let owner = entry
                .owners
                .first()
                .ok_or("local action route has no owner")?;
            let (records, checkout) = owner.store_checkout();
            records.private_actions(&checkout.id)?
        } else if self
            .collaboration
            .manager
            .primary_route()
            .is_some_and(|w| &w.route == route)
        {
            if let Some(records) = &self.persistence.records {
                records.active_private_actions(&self.state.doc)?
            } else {
                self.persistence
                    .autosave
                    .as_ref()
                    .ok_or("no durable document owner")?
                    .private_actions(&self.state.doc)?
            }
        } else {
            return Err("local action route is not live".into());
        };
        entries
            .into_iter()
            .map(|entry| {
                let checkpoint: LocalActionCheckpoint = serde_json::from_slice(&entry.intent)
                    .or_else(|_| {
                        serde_json::from_slice(&entry.intent).map(|report| LocalActionCheckpoint {
                            custody: None,
                            report,
                            directory_row: None,
                        })
                    })
                    .map_err(|e| e.to_string())?;
                if let Some(custody) = &checkpoint.custody {
                    if !custody.valid()
                        || custody.app_id != route.app_id
                        || custody.room != route.cluster_id.to_string()
                        || custody.document != route.record.as_deref().unwrap_or("")
                    {
                        return Err("private action checkpoint custody mismatch".into());
                    }
                }
                let report = &checkpoint.report;
                let hashes: Vec<_> = report
                    .original_heads
                    .iter()
                    .chain(&report.mutation_hashes)
                    .copied()
                    .collect();
                if report.bound_invocation()? != report.invocation
                    || *report.invocation.as_bytes() != entry.id
                    || hashes != entry.hashes
                {
                    return Err("private action checkpoint identity mismatch".into());
                }
                let receipt = entry
                    .receipt
                    .map(String::from_utf8)
                    .transpose()
                    .map_err(|e| e.to_string())?;
                Ok((checkpoint, receipt))
            })
            .collect()
    }
}
