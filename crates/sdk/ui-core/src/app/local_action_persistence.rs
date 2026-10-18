//! Private local action checkpoints follow the exact document owner, not UI aliases.
use super::*;

impl App {
    pub(super) fn stage_local_action_evidence(
        &mut self,
        route: &crate::collaboration::RouteSpec,
        capture: &crate::local_document_action::CapturedAction,
        receipt: Option<&str>,
    ) -> Result<(), String> {
        let report = capture.clone().into_report()?;
        let action = app_state::PrivateAction {
            id: *capture.invocation.as_bytes(),
            hashes: capture
                .original_heads
                .iter()
                .chain(&capture.mutation_hashes)
                .copied()
                .collect(),
            intent: serde_json::to_vec(&report).map_err(|error| error.to_string())?,
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
                let report: runtime_collab::LocalDocumentActionReport =
                    serde_json::from_slice(&entry.intent).map_err(|e| e.to_string())?;
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
                Ok((report, receipt))
            })
            .collect()
    }
}
