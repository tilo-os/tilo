//! The native primary channel addresses one canonical record, not App's outer
//! scratch document. A fresh broker epoch is given the record whole, then a
//! fresh delivery frontier for the changes this replica makes.
use super::{App, Widget};
use crate::bound_dispatch::CallbackBinding;
use crate::collaboration::PrimaryRouteWitness;
use crate::state::DocumentFrontier;

pub(super) struct BoundRecordSync {
    pub(super) binding: CallbackBinding,
    route: PrimaryRouteWitness,
    frontier: DocumentFrontier,
    /// Whether this epoch was given the record whole. A route's first
    /// publication in a session is the whole document; only the changes this
    /// replica makes after it go out one by one.
    whole: bool,
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

    /// Count changes the primary document's owner served this replica as
    /// published: it holds them, so publishing them back would be an echo.
    /// Changes this replica still has to send stay outside the frontier.
    pub(super) fn observe_published(&mut self, observed: Vec<automerge::ChangeHash>) {
        self.frontier.observe(observed);
    }
}

/// A primary document's unsent changes, the heads they leave, and the
/// commit of the action they travel under.
pub(super) type PrimaryPublication = (
    collab_types::DocumentMutation,
    Vec<automerge::ChangeHash>,
    Option<collab_types::WorkspaceCommit>,
);

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
                whole: false,
            });
        }
        Ok(true)
    }

    /// What of the primary document the route has not been sent, the heads
    /// it leaves, and the commit it travels under.
    pub(super) fn pending_primary_document(
        &mut self,
    ) -> Result<Option<PrimaryPublication>, String> {
        let route = self
            .bound_broker_route()
            .map(|witness| witness.route().clone());
        let commit = move |mutation: &collab_types::DocumentMutation,
                           document: &automerge::Automerge| {
            let route = route.as_ref()?;
            let unit = collab_types::UnitMutation {
                app: &route.app_id,
                record: route.record.as_deref().unwrap_or(""),
                mutation,
                document: Some(document),
            };
            super::document_routes::unit_commits(route.cluster_id, &[unit])
                .pop()
                .flatten()
        };
        if !self.prepare_primary_document()? {
            // A route's first publication in a session is the document
            // whole: what this device held before the route opened — a
            // record its launch seeded, or one it reopened from its store —
            // reaches no peer in an incremental of this replica's own
            // changes, and one made now would build on history they lack.
            if !self.document_routes.primary_whole {
                let heads = self.state.doc.get_heads();
                return Ok((!heads.is_empty()).then(|| {
                    (
                        collab_types::DocumentMutation::Snapshot(self.state.doc.save()),
                        heads,
                        None,
                    )
                }));
            }
            let (delta, heads) = self.state.pending_outbound_delta();
            if delta.is_empty() {
                return Ok(None);
            }
            let mutation = collab_types::DocumentMutation::Incremental(delta);
            let commit = commit(&mutation, self.state.doc.document());
            return Ok(Some((mutation, heads, commit)));
        }
        let sync = self
            .actions
            .bound_record_sync
            .as_mut()
            .ok_or("primary record is unavailable")?;
        let _scope = sync.binding.enter().map_err(|error| format!("{error:?}"))?;
        let BoundRecordSync {
            binding,
            frontier,
            whole,
            ..
        } = sync;
        binding
            .records
            .with_action_state(&binding.checkout.id, |doc, _, actions| {
                actions.commit_for_save(doc);
                if !*whole {
                    let heads = doc.get_heads();
                    return (!heads.is_empty()).then(|| {
                        (
                            collab_types::DocumentMutation::Snapshot(doc.save()),
                            heads,
                            None,
                        )
                    });
                }
                let (delta, heads) = frontier.pending_delta(doc, &actions.writers.borrow());
                if delta.is_empty() {
                    return None;
                }
                let mutation = collab_types::DocumentMutation::Incremental(delta);
                let commit = commit(&mutation, doc.document());
                Some((mutation, heads, commit))
            })
            .map_err(|error| error.to_string())
    }

    pub(super) fn advance_primary_document(&mut self, heads: Vec<automerge::ChangeHash>) {
        if let Some(sync) = self.actions.bound_record_sync.as_mut() {
            sync.frontier.advance(heads);
            sync.whole = true;
        } else {
            self.document_routes.primary_whole = true;
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
            .merge_replica(&sync.binding.checkout, mutation)?;
        sync.frontier.observe(observed);
        Ok(changed)
    }
}
