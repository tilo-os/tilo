//! A mounted record kept current: what the workspace holds of it merged in,
//! only the changes after the heads it merged last.
use super::{DocumentRoute, Mounted, WorkspaceState, reads};

/// Merge into `mounted` what `state` holds of its record and its parts that
/// changed since it merged them last.
pub(super) fn catch_up(route: &DocumentRoute, mounted: &mut Mounted, state: &WorkspaceState) {
    match state.update(route, mounted.merged.as_deref()) {
        Ok(Some((mutation, heads))) => {
            // A change that did not merge leaves the replica where it was:
            // the next merge takes the record whole.
            mounted.merged = match mounted.app.apply(&mutation) {
                Ok(()) => Some(heads),
                Err(error) => {
                    tracing::warn!(%error, "workspace actions: a change did not merge");
                    None
                }
            };
        }
        Ok(None) => {}
        Err(error) => tracing::debug!(%error, "workspace actions: a record does not read"),
    }
    for (part, merged) in &mut mounted.parts {
        let Ok(record) = reads::record_id(&part.record) else {
            continue;
        };
        let Ok(Some((mutation, heads))) = state.update(part, merged.as_deref()) else {
            continue;
        };
        *merged = match mounted.app.apply_record(&record, &mutation) {
            Ok(()) => Some(heads),
            Err(error) => {
                tracing::warn!(%error, "workspace actions: a change to a part did not merge");
                None
            }
        };
    }
}
