//! The records an app keeps beside one of its own: the parts of a chain
//! (a chat's messages, [`ui::record::RecordId::part`]). A part is never a
//! record the agent opens; it is held in the replica of the record that
//! keeps it, as a window holds it, so the record's operations read and
//! write it (a post goes to the chat's head part). A part that arrives
//! before its record is open waits with the room's unopened records and is
//! held when the record opens; what an operation changed of a part goes out
//! on the part's own route.

use collab_types::DocumentMutation;
use tilo_browser_peer::DocumentRoute;

use super::AgentParticipant;

/// Whether the wire record `record` is a part of a chain, held beside its
/// app's record rather than opened.
pub(crate) fn is_part(record: &str) -> bool {
    ui::record::RecordId::parse(record).is_some_and(|id| id.chain_part().is_some())
}

impl AgentParticipant {
    /// Merge `mutation` of the part at `route` into the open record that
    /// keeps it. `None` when no open record of its app holds it (it waits
    /// with the unopened records then); else the holder's route.
    pub(super) fn merge_part(
        &mut self,
        route: &DocumentRoute,
        mutation: &DocumentMutation,
    ) -> Option<DocumentRoute> {
        let record = ui::record::RecordId::parse(&route.record)?;
        let known = self
            .beside
            .get(route)
            .filter(|holder| self.apps.contains_key(*holder))
            .cloned();
        let candidates: Vec<DocumentRoute> = match known {
            Some(holder) => vec![holder],
            None => self
                .apps
                .keys()
                .filter(|held| {
                    held.app_id == route.app_id
                        && held.cluster_id == route.cluster_id
                        && !is_part(&held.record)
                })
                .cloned()
                .collect(),
        };
        for holder in candidates {
            let Some(app) = self.apps.get(&holder) else {
                continue;
            };
            match app.apply_record(&record, mutation) {
                Ok(()) => {
                    tracing::debug!(part = %route.record, "agent: merged a part");
                    self.beside.insert(route.clone(), holder.clone());
                    return Some(holder);
                }
                Err(error) => {
                    tracing::debug!(%error, part = %route.record, "agent: a part is not held here");
                }
            }
        }
        None
    }

    /// Hold in the record just opened at `holder` the parts of its app the
    /// room sent before it was open.
    pub(super) fn hold_waiting_parts(&mut self, holder: &DocumentRoute) {
        let waiting: Vec<DocumentRoute> = self
            .unopened
            .routes()
            .into_iter()
            .filter(|route| {
                route.app_id == holder.app_id
                    && route.cluster_id == holder.cluster_id
                    && is_part(&route.record)
            })
            .collect();
        for part in waiting {
            let Some(Ok(saved)) = self.unopened.seed(&part) else {
                continue;
            };
            let Some(record) = ui::record::RecordId::parse(&part.record) else {
                continue;
            };
            let Some(app) = self.apps.get(holder) else {
                return;
            };
            match app.apply_record(&record, &DocumentMutation::Snapshot(saved)) {
                Ok(()) => {
                    tracing::debug!(part = %part.record, "agent: held a waiting part");
                    self.unopened.forget(&part);
                    self.beside.insert(part, holder.clone());
                }
                Err(error) => {
                    tracing::debug!(%error, part = %part.record, "agent: a part is not held here");
                }
            }
        }
    }

    /// Publish what the record at `holder` changed of the parts it holds
    /// beside it, each on the part's own route.
    pub(super) fn publish_parts(&mut self, holder: &DocumentRoute) -> Result<(), String> {
        let Some(app) = self.apps.get(holder) else {
            return Ok(());
        };
        let taken = app
            .take_record_mutations()
            .map_err(|error| error.to_string())?;
        let known = self.peer.status().routes;
        for (record, mutation) in taken {
            let part = DocumentRoute {
                app_id: holder.app_id.clone(),
                cluster_id: holder.cluster_id.clone(),
                record: record.route().to_owned(),
            };
            // A part another member already started, which this agent
            // started again without holding it (its keys had not come):
            // the room keeps the first start as the part's checkpoint, so
            // this one goes out as changes on top of it, which meet the
            // others' as two starts of one part do.
            let mutation = match mutation {
                DocumentMutation::Snapshot(bytes) if known.contains_key(&part) => {
                    DocumentMutation::Incremental(bytes)
                }
                mutation => mutation,
            };
            self.generation = self.generation.wrapping_add(1);
            tracing::debug!(part = %part.record, "agent: published a part");
            self.peer.publish(part.clone(), &mutation)?;
            self.beside.insert(part, holder.clone());
        }
        Ok(())
    }
}
