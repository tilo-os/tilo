//! Model progress shared by an application worker and the window runner.

pub(super) struct IdleModelProgress {
    pub(super) changed: bool,
}

/// `TILO_HOST_PERF_BASELINE=1` restores the previous always-refresh hosted
/// pump (sync, cache invalidation and full action-surface rebuild on every
/// pass) for paired before/after host measurements. `TILO_HOST_PERF2_BASELINE=1`
/// restores it too, and also the session peer's journaled aggregate ACK
/// commits (`store::side_log`).
fn host_perf_baseline() -> bool {
    static BASELINE: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *BASELINE.get_or_init(|| crate::host::environment().diagnostics.host_perf_baseline)
}

/// `TILO_HOSTED_PUMP_TIMING=1` prints one payload-free line per hosted pump.
fn hosted_pump_timing() -> bool {
    static ENABLED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ENABLED.get_or_init(|| crate::host::environment().diagnostics.hosted_pump_timing)
}

/// Policy meaning observed by the previous hosted pump: the App's own context
/// and the root component owner's host-supplied context (the `Rc` replaced by
/// `HostWorker::pump`). Directory witnesses include per-capability liveness at
/// a fresh policy-time sample, so expiry is a change even when the context
/// value is identical.
#[derive(PartialEq, Eq)]
struct HostedAuthorityWitness {
    app: crate::permissions::DirectoryPermissionWitness,
    owner: Option<crate::permissions::DirectoryPermissionWitness>,
}

/// Every input of the trailing `publish_action_surface` that can change
/// without a resource-cache invalidation. Equal stamps mean that publication
/// would recompute the same resources, descriptors and directory snapshot.
#[derive(PartialEq, Eq)]
struct HostedPublicationStamp {
    resource_generation: u64,
    permission_generation: u64,
    registry_generation: u64,
    route: Option<crate::collaboration::PrimaryRouteWitness>,
    revision: String,
}

#[derive(Default)]
pub(super) struct HostedPumpState {
    authority: Option<HostedAuthorityWitness>,
    publication: Option<HostedPublicationStamp>,
}

/// What one hosted pump did; used by focused tests and the timing trace.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) struct HostedPumpReport {
    pub(super) updated: bool,
    pub(super) synced: bool,
    pub(super) published: bool,
}

impl super::App {
    /// A worker has no redraw loop. Advance one bounded model pass using the
    /// same phases as the window runner, without adding caller attribution.
    pub fn pump_hosted_model(&mut self) {
        self.pump_hosted_model_as(host_perf_baseline());
    }

    /// [`Self::pump_hosted_model`], answering whether the app's record
    /// changed: a different document under the same widgets, which their
    /// damage does not describe.
    #[cfg(feature = "native")]
    pub(super) fn pump_hosted_model_record_changed(&mut self) -> bool {
        self.pump_hosted_model_with_record(host_perf_baseline()).1
    }

    /// Advance the ordinary dirty-tracked native worker without a window.
    /// This never forces synchronization or layout to mask a missing wake.
    #[cfg(any(test, feature = "test-support"))]
    pub fn controlled_hosted_model_for_test(&mut self) -> bool {
        self.pump_hosted_model_as(false).synced
    }

    /// Read the same state observer used around scheduled native inputs.
    #[cfg(all(feature = "native", any(test, feature = "test-support")))]
    pub fn controlled_native_input_snapshot_for_test(&self) -> Option<serde_json::Value> {
        self.native_input_snapshot()
    }

    /// Independently sample the existing owner and read gates for diagnosis.
    /// These observations never admit a mutation or acknowledge a capture.
    /// A missing owner or refused guard is reported as an unavailable merge phase.
    #[cfg(all(feature = "native", any(test, feature = "test-support")))]
    pub fn controlled_document_merge_phase_for_test(&self) -> Vec<serde_json::Value> {
        self.document_routes.owners.iter().map(|entry| {
            let merge_ready = entry.owners.first().and_then(|owner| {
                let (store, checkout) = owner.store_checkout();
                crate::bound_dispatch::can_merge_remote_owned_document(store, checkout).ok()
            });
            serde_json::json!({
                "owners": entry.owners.len(),
                "merge_ready": merge_ready,
                "pending_remote": !entry.pending.is_empty(),
                "committed_read_ready": self.collaboration.manager.document_route_ready(Some(&entry.route)),
                "committed_read_phase": self.collaboration.manager.controlled_document_read_phase_for_test(&entry.route),
                "committed_queue": self.committed_documents.iter().filter(|(route, _)| route == &entry.route).count(),
            })
        }).collect()
    }

    /// Observe the production guard for acknowledged private continuity.
    #[cfg(all(feature = "native", any(test, feature = "test-support")))]
    pub fn controlled_private_continuity_route_for_test(
        &mut self,
    ) -> Option<crate::collaboration::RouteSpec> {
        self.private_continuity_route()
    }

    /// `always_refresh` is the previous behaviour: synchronize, invalidate and
    /// republish unconditionally. Otherwise the pump is dirty-tracked like the
    /// window runner, whose redraw syncs only on `needs_sync`: a pass with no
    /// inbound event, no model-dirty wake, no policy change, no in-flight job,
    /// no due widget deadline and no model progress keeps the resource cache
    /// and skips the action-surface rebuild.
    pub(super) fn pump_hosted_model_as(&mut self, always_refresh: bool) -> HostedPumpReport {
        self.pump_hosted_model_with_record(always_refresh).0
    }

    /// [`Self::pump_hosted_model_as`], and whether the record changed.
    fn pump_hosted_model_with_record(&mut self, always_refresh: bool) -> (HostedPumpReport, bool) {
        let started = hosted_pump_timing().then(crate::time::now);
        let _entry = self.enter_wake_scope();
        let (inbound, directory) = self.pump_collab_presence_inner(true);
        // Retire expired/cancelled jobs before their later model continuation.
        // A completion produced below is polled on the following host pass.
        let invocations = self.poll_model_invocations();
        let record = self.record_tick();
        // The owner retains wake flags even when its sink schedules a frame.
        // A presentation-only frame must not force model synchronization.
        // Detached or closed scopes remain conservatively model-dirty.
        let wakes = self.take_hosted_wakes();
        let authority_changed = self.refresh_hosted_authority();
        let in_flight = !self.actions.directed_invocations.is_empty()
            || !self.actions.bound_invocations.is_empty()
            || !self.actions.local_invocations.is_empty()
            || !self.actions.pending_application_launches.is_empty();
        let deadline_due = self
            .root_widget
            .next_wake_deadline()
            .is_some_and(|deadline| deadline <= crate::time::now());
        let mut force_sync = always_refresh
            || inbound
            || authority_changed
            || in_flight
            || deadline_due
            || wakes.is_none_or(|(_, sync)| sync);
        let mut report = HostedPumpReport::default();
        if force_sync
            || invocations
            || record
            || self.needs_sync
            || wakes.is_some_and(|(pending, _)| pending)
        {
            // Drain model notifications and advance update-driven widgets.
            report.updated = true;
            self.update_model();
            // A model-dirty wake raised while draining joins this sync.
            force_sync |= self.take_hosted_wakes().is_none_or(|(_, sync)| sync);
        }
        // A host pump is the worker's model frame and must drain asynchronous
        // notifications even when no native Wake/redraw event exists.
        report.synced = self.sync_model(force_sync);
        let progress = self.service_idle_model();
        if progress.changed && !report.synced {
            // Local control/history/commit progress changed model state after
            // the (skipped) sync; the publication below must not reuse it.
            self.invalidate_widget_resource_cache();
        }
        if always_refresh
            || self.actions.hosted.publication.is_none()
            || self.hosted_publication_stamp() != self.actions.hosted.publication
        {
            report.published = true;
            self.publish_action_surface();
        } else if self.collaboration.manager.is_connected() {
            // Keep the idempotent directory-capability registration that the
            // full publication performs on every pass.
            let app_id = self.publication_app_id();
            self.collaboration.manager.publish_action_directory(app_id);
        }
        self.broadcast_doc_delta();
        self.finish_model_directory_refresh(directory);
        if report.published {
            self.actions.hosted.publication = self.hosted_publication_stamp();
        }
        if let Some(started) = started {
            crate::diag!(
                "[hosted-pump] us={} updated={} synced={} published={} inbound={inbound} wakes={wakes:?} authority={authority_changed} in_flight={in_flight} deadline={deadline_due} invocations={invocations} record={record}",
                started.elapsed().as_micros(),
                report.updated,
                report.synced,
                report.published
            );
        }
        (report, record)
    }

    fn take_hosted_wakes(&self) -> Option<(bool, bool)> {
        self.wake_scope
            .as_ref()
            .and_then(crate::wake::WakeScope::take_pending)
    }

    /// Returns true when policy meaning changed since the previous hosted pump
    /// or cannot be sampled (fail closed: refresh on every such pass).
    fn refresh_hosted_authority(&mut self) -> bool {
        let owner = if let Some(binding) = self.root_widget.callback_binding() {
            let Ok(witness) = binding.permissions.borrow().directory_witness() else {
                self.actions.hosted.authority = None;
                return true;
            };
            Some(witness)
        } else {
            None
        };
        let Ok(app) = self.permission_ctx.directory_witness() else {
            self.actions.hosted.authority = None;
            return true;
        };
        let current = HostedAuthorityWitness { app, owner };
        let changed = self.actions.hosted.authority.as_ref() != Some(&current);
        self.actions.hosted.authority = Some(current);
        changed
    }

    /// `None` whenever publication cannot be proven current: disconnected,
    /// nothing published yet, an evicted directory snapshot or stamp, or an
    /// exhausted generation that no longer distinguishes later mutations.
    fn hosted_publication_stamp(&self) -> Option<HostedPublicationStamp> {
        let resource_generation = self.actions.resource_generation.get();
        let permission_generation = self.actions.permission_generation.get();
        let registry_generation = self.actions.action_registry_generation;
        if !self.collaboration.manager.is_connected()
            || self.actions.last_published_actions.is_none()
            || self.actions.action_directory_snapshot.is_none()
            || [
                resource_generation,
                permission_generation,
                registry_generation,
            ]
            .contains(&u64::MAX)
        {
            return None;
        }
        let (_, revision) = self.actions.action_directory_stamp.as_ref()?;
        Some(HostedPublicationStamp {
            resource_generation,
            permission_generation,
            registry_generation,
            route: self.bound_broker_route().cloned(),
            revision: revision.clone(),
        })
    }

    /// A native model pass acknowledges only wakes preceding this drain.
    /// Later worker completions retain their pending bit for the idle pass.
    #[cfg(feature = "native")]
    pub(super) fn update_native_model(&mut self) -> bool {
        self.needs_sync |= self.take_hosted_wakes().is_some_and(|(_, sync)| sync);
        self.update_model()
    }

    pub(super) fn update_model(&mut self) -> bool {
        if !self.root_widget.update(&self.state.doc, &self.state.root) {
            return false;
        }
        self.invalidate_widget_resource_cache();
        self.needs_sync = true;
        self.layout.layout_dirty = true;
        true
    }

    pub(super) fn sync_model(&mut self, force: bool) -> bool {
        if !force && !self.needs_sync {
            return false;
        }
        // What the widgets took up since the last frame (an edit a child
        // made on its own) is written before the document is read again.
        self.root_widget.settle(
            &mut crate::widget::Edits::lend(&mut self.state.doc),
            &self.state.root,
        );
        self.root_widget.sync(&self.state.doc, &self.state.root);
        // A sync reads the document only. What it took up is written now,
        // through actions, and shown in this same frame.
        let before = crate::widget::settle_mark(&self.state.doc);
        self.root_widget.settle(
            &mut crate::widget::Edits::lend(&mut self.state.doc),
            &self.state.root,
        );
        let settled = crate::widget::settle_mark(&self.state.doc) != before;
        let submitted = self.dispatch_control_invocations() != 0;
        if settled || submitted {
            self.root_widget.sync(&self.state.doc, &self.state.root);
        }
        self.invalidate_widget_resource_cache();
        self.needs_sync = false;
        self.forget_replaced_widgets();
        true
    }

    /// After a sync replaced widgets (a view switched what it shows), the
    /// layout's map from nodes to widgets still points at the freed ones
    /// until the next layout rebuilds it. Drop the map now, so nothing in
    /// between (the cursor and tooltip a pointer move looks up, a key's
    /// focused target) reaches a widget that is gone. Whether it did.
    pub(super) fn forget_replaced_widgets(&mut self) -> bool {
        if !self.take_composition_dirty() {
            return false;
        }
        self.layout.structure_dirty = true;
        self.layout.layout_dirty = true;
        self.bindings.clear();
        self.layout.node_map.clear();
        true
    }

    pub(super) fn poll_model_invocations(&mut self) -> bool {
        let changed = self.poll_directed_invocations();
        if changed {
            self.needs_sync = true;
            self.layout.layout_dirty = true;
        }
        changed
    }

    #[cfg(feature = "native")]
    pub(super) fn service_native_idle_model(&mut self) -> IdleModelProgress {
        let mut changed = self.service_idle_model().changed;
        // Hidden windows may not receive frame callbacks. Only an unconsumed
        // model wake advances their owner; presentation notifications stay on
        // the redraw path. Prepare the checked owner exactly as a frame does.
        if self.take_hosted_wakes().is_some_and(|(_, sync)| sync) {
            changed |= self.swap_requested_component() | self.record_tick();
            changed |= self.update_model();
            changed |= self.sync_model(true);
        }
        IdleModelProgress { changed }
    }

    pub(super) fn service_idle_model(&mut self) -> IdleModelProgress {
        let history_changed = self.refresh_local_undo_discovery();
        let changed = self.service_local_control_history() || history_changed;
        // This is local writer progress, never a durable owner receipt. The
        // existing scoped publication barrier still gates terminal success.
        #[cfg(feature = "native")]
        if let Err(error) = self.autosave_tick() {
            crate::diag!("[app] component autosave tick refused: {error:?}");
        }
        IdleModelProgress { changed }
    }
}

#[cfg(test)]
#[path = "replaced_widget_tests.rs"]
mod replaced_widget_tests;
