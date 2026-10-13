//! The native primary channel addresses one canonical record, not App's outer
//! scratch document. A fresh broker epoch starts a fresh delivery frontier.
use super::{App, Widget};
use crate::bound_dispatch::CallbackBinding;
use crate::collaboration::PrimaryRouteWitness;
use crate::state::DocumentFrontier;

pub(super) struct BoundRecordSync {
    pub(super) binding: CallbackBinding,
    route: PrimaryRouteWitness,
    frontier: DocumentFrontier,
}

impl BoundRecordSync {
    /// Whether this publishes `checkout` of `records` as the primary
    /// document.
    pub(super) fn publishes(
        &self,
        records: &crate::RecordStore,
        checkout: &crate::record::RecordCheckout,
    ) -> bool {
        self.binding.checkout == *checkout && self.binding.records.same_owner(records)
    }

    /// Whether every change up to `heads` (sorted) is already published.
    pub(super) fn published_through(&self, heads: &[automerge::ChangeHash]) -> bool {
        let mut published = self.frontier.heads().to_vec();
        published.sort_unstable();
        published == heads
    }

    /// Count `heads` as published: history the primary document's owner
    /// already holds, merged in from it.
    pub(super) fn observe_published(&mut self, heads: Vec<automerge::ChangeHash>) {
        self.frontier.advance(heads);
    }
}

impl App {
    pub(super) fn prepare_primary_document(&mut self) -> Result<bool, String> {
        fn visit(
            widget: &dyn Widget,
            route: Option<&PrimaryRouteWitness>,
            found: &mut Option<CallbackBinding>,
            has_components: &mut bool,
        ) -> Result<(), String> {
            if let Some(binding) = widget.callback_binding() {
                *has_components = true;
                if binding.matches_broker_route(route) && binding.enter().is_ok() {
                    if let Some(previous) = found.as_ref()
                        && (!previous.records.same_owner(&binding.records)
                            || previous.checkout != binding.checkout)
                    {
                        return Err("primary document has competing canonical owners".into());
                    }
                    *found = Some(binding);
                }
            }
            let mut result = Ok(());
            widget.for_each_child(&mut |child| {
                if result.is_ok() {
                    result = visit(child, route, found, has_components);
                }
            });
            result
        }
        let route = self.bound_broker_route().cloned();
        let mut found = None;
        let mut has_components = false;
        visit(
            self.root_widget.as_ref(),
            route.as_ref(),
            &mut found,
            &mut has_components,
        )?;
        let Some(binding) = found else {
            self.actions.bound_record_sync = None;
            return if has_components {
                Err("no live component matches the admitted primary document route".into())
            } else {
                Ok(false)
            };
        };
        let route = route.ok_or("primary document route is unavailable")?;
        if let Some(sync) = self.actions.bound_record_sync.as_mut()
            && sync.route == route
            && sync.binding.records.same_owner(&binding.records)
            && sync.binding.checkout == binding.checkout
        {
            // A second view of the same owner may outlive the original view.
            sync.binding = binding;
        } else {
            self.actions.bound_record_sync = Some(BoundRecordSync {
                binding,
                route,
                // Include edits made before the connection was admitted.
                frontier: DocumentFrontier::default(),
            });
        }
        Ok(true)
    }

    pub(super) fn pending_primary_document_delta(
        &mut self,
    ) -> Result<(Vec<u8>, Vec<automerge::ChangeHash>), String> {
        if !self.prepare_primary_document()? {
            return Ok(self.state.pending_outbound_delta());
        }
        let sync = self
            .actions
            .bound_record_sync
            .as_mut()
            .ok_or("primary record is unavailable")?;
        let _scope = sync.binding.enter().map_err(|error| format!("{error:?}"))?;
        sync.binding
            .records
            .with_action_state(&sync.binding.checkout.id, |doc, _, actions| {
                actions.commit_for_save(doc);
                sync.frontier.pending_delta(doc)
            })
            .map_err(|error| error.to_string())
    }

    pub(super) fn advance_primary_document(&mut self, heads: Vec<automerge::ChangeHash>) {
        if let Some(sync) = self.actions.bound_record_sync.as_mut() {
            sync.frontier.advance(heads);
        } else {
            self.state.advance_outbound(heads);
        }
    }

    pub(super) fn primary_document_snapshot(&mut self) -> Result<Vec<u8>, String> {
        if !self.prepare_primary_document()? {
            return Ok(self.state.save());
        }
        let sync = self
            .actions
            .bound_record_sync
            .as_ref()
            .ok_or("primary record is unavailable")?;
        let _scope = sync.binding.enter().map_err(|error| format!("{error:?}"))?;
        sync.binding
            .records
            .with_action_state(&sync.binding.checkout.id, |doc, _, actions| {
                actions.commit_for_save(doc);
                doc.save()
            })
            .map_err(|error| error.to_string())
    }

    #[cfg(test)]
    pub(super) fn apply_primary_document(
        &mut self,
        mutation: collab_types::DocumentMutation,
    ) -> Result<bool, String> {
        if !self.prepare_primary_document()? {
            return match mutation {
                collab_types::DocumentMutation::Snapshot(data) => {
                    self.state.merge_snapshot(&data).map(|()| true)
                }
                collab_types::DocumentMutation::Incremental(data) => {
                    self.state.load_incremental(&data).map(|count| count > 0)
                }
            }
            .map_err(|error| error.to_string());
        }
        let sync = self
            .actions
            .bound_record_sync
            .as_mut()
            .ok_or("primary record is unavailable")?;
        let _scope = sync.binding.enter().map_err(|error| format!("{error:?}"))?;
        let (changed, observed) = sync
            .binding
            .records
            .merge_remote_document(&sync.binding.checkout, mutation)?;
        sync.frontier.observe(observed);
        Ok(changed)
    }
}
