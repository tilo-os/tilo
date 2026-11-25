//! A route's checkpoint coverage, as one peer holds it.
//!
//! One checkpoint covers each route: the one the workspace's coverage row
//! names, which is the first of the owner's devices to cover it. Every device
//! of the owner may publish the record as it has it, and only that checkpoint
//! is covered; the others are their authors' own changes.

use super::{AdmissionError, AdmissionReport, InputDisposition, ReceiveOutcome, Route, StatePeer};

impl StatePeer {
    /// `route`'s coverage row as this peer holds it: which checkpoint of the
    /// route the workspace covers, and for whom.
    pub(super) fn checkpoint_coverage(
        &self,
        route: &Route,
    ) -> Result<Option<crate::coverage::Coverage>, String> {
        let Some(workspace) = self.core.workspace_doc.as_deref() else {
            return Ok(None);
        };
        Ok(crate::coverage::entries(workspace)?
            .remove(&crate::coverage::route_key(&route.app, &route.record)))
    }
    /// True only when governance covers this exact route with a checkpoint by
    /// the same author at a strictly greater sequence than `sequence`. Any other
    /// coverage relation is not a supersession and keeps its normal handling.
    pub(super) fn superseded_checkpoint(
        &self,
        route: &Route,
        source: &[u8; 32],
        sequence: u64,
    ) -> Result<bool, AdmissionError> {
        Ok(self
            .checkpoint_coverage(route)
            .map_err(crate::AdmissionError::malformed)?
            .is_some_and(|coverage| {
                coverage.workspace == self.core.workspace
                    && coverage.app == route.app
                    && coverage.record == route.record
                    && coverage.author == *source
                    && coverage.sequence > sequence
            }))
    }
    /// A superseded checkpoint is settled without being applied, so the
    /// delivery cursor can move past it; nothing from it enters state.
    pub(super) fn superseded_checkpoint_report() -> AdmissionReport {
        AdmissionReport::new(
            InputDisposition::MetadataOnly,
            ReceiveOutcome {
                duplicate: true,
                ..Default::default()
            },
        )
    }
}
