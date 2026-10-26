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
    #[cfg(test)]
    pub(super) calls: usize,
    authority: Option<HostedAuthorityWitness>,
    publication: Option<HostedPublicationStamp>,
}

/// What one hosted pump did; used by focused tests and the timing trace.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) struct HostedPumpReport {
    pub(super) presentation_changed: bool,
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

    /// Presence and connection changes can change pixels without dirtying a widget.
    #[cfg(feature = "native")]
    pub(super) fn pump_embedded_model(&mut self) -> bool {
        self.pump_hosted_model_as(host_perf_baseline())
            .presentation_changed
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

    /// Why the session action host last refused one of this window's
    /// invocations outright, if it has: a refused action never runs, so a
    /// fixture waiting for its effect fails on this at once.
    #[cfg(any(test, feature = "test-support"))]
    pub fn controlled_refused_action_for_test(&self) -> Option<&str> {
        self.actions.refused_broker_receipt.as_deref()
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
                "app": entry.route.app_id,
                "record": entry.route.record,
                "owners": entry.owners.len(),
                "merge_ready": merge_ready,
                "pending_remote": !entry.pending.is_empty(),
                "committed_read_ready": self.collaboration.manager.document_route_ready(Some(&entry.route)),
                "committed_read_phase": self.collaboration.manager.controlled_document_read_phase_for_test(&entry.route),
                "join_failure": self.collaboration.manager.route_join_failure(&entry.route),
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
        #[cfg(test)]
        {
            self.actions.hosted.calls += 1;
        }
        let started = hosted_pump_timing().then(crate::time::now);
        let _entry = self.enter_wake_scope();
        #[cfg(test)]
        let probe = crate::profiling::frame_probe::span("pump.inputs");
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
        let next_deadline = self.root_widget.next_wake_deadline();
        let deadline_due = next_deadline.is_some_and(|deadline| deadline <= crate::time::now());
        #[cfg(test)]
        drop(probe);
        let mut force_sync = always_refresh
            || inbound
            || authority_changed
            || in_flight
            || deadline_due
            || wakes.is_none_or(|(_, sync)| sync);
        let mut report = HostedPumpReport {
            presentation_changed: inbound,
            ..HostedPumpReport::default()
        };
        if force_sync
            || invocations
            || record
            || self.needs_sync
            || wakes.is_some_and(|(pending, _)| pending)
        {
            report.updated = true;
            #[cfg(test)]
            let _probe = crate::profiling::frame_probe::span("pump.update");
            self.update_model();
            // A model-dirty wake raised while draining joins this sync.
            force_sync |= self.take_hosted_wakes().is_none_or(|(_, sync)| sync);
        }
        // A host pump is the worker's model frame and must drain asynchronous
        // notifications even when no native Wake/redraw event exists.
        #[cfg(test)]
        let probe = crate::profiling::frame_probe::span("pump.sync");
        report.synced = self.sync_model(force_sync);
        #[cfg(test)]
        drop(probe);
        #[cfg(test)]
        let probe = crate::profiling::frame_probe::span("pump.idle");
        let progress = self.service_idle_model();
        #[cfg(test)]
        drop(probe);
        if progress.changed && !report.synced {
            // Local control/history/commit progress changed model state after
            // the (skipped) sync; the publication below must not reuse it.
            self.invalidate_widget_resource_cache();
        }
        #[cfg(test)]
        let _publish_probe = crate::profiling::frame_probe::span("pump.publish");
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
        // What search finds can move with the model (a refreshed vault, a
        // timer): the session's index follows it here too, not only on input.
        self.publish_search_index();
        self.finish_model_directory_refresh(directory);
        if report.published {
            self.actions.hosted.publication = self.hosted_publication_stamp();
        }
        if let Some(started) = started {
            crate::diag!(
                "[hosted-pump] app={} late_us={:?} us={} updated={} synced={} published={} inbound={inbound} wakes={wakes:?} authority={authority_changed} in_flight={in_flight} deadline={deadline_due} invocations={invocations} record={record}",
                self.publication_app_id(),
                next_deadline.map(|deadline| crate::time::now()
                    .saturating_duration_since(deadline)
                    .as_micros()),
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
        // The room's presence is state of the frame, lent under the window's
        // authority, as the draw sees it.
        let (root, state, presence) = (&mut self.root_widget, &self.state, &self.presence_ctx);
        // What this update moved, not what a widget updated outside an app
        // noted before it.
        let _ = crate::widget::take_placement_moved();
        let updated = presence.with_permissions(self.permission_ctx.clone(), || {
            crate::presence::frame::lend(presence, || root.update(&state.doc, &state.root))
        });
        // A glide moved content its widgets lay out: they are placed again.
        self.layout.layout_dirty |= crate::widget::take_placement_moved();
        if !updated {
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
        let _phase = tilo_resource::profiling::Phase::start("ui.frame_sync");
        // What the widgets took up since the last frame (an edit a child
        // made on its own) is written before the document is read again.
        self.root_widget.settle(
            &mut crate::widget::Edits::lend(&mut self.state.doc),
            &self.state.root,
        );
        self.root_widget.sync(&self.state.doc, &self.state.root);
        // A sync reads the document only. What it took up is written now,
        // through actions, and shown in this same frame.
        let before = crate::doc::settle_mark(&self.state.doc);
        self.root_widget.settle(
            &mut crate::widget::Edits::lend(&mut self.state.doc),
            &self.state.root,
        );
        let settled = crate::doc::settle_mark(&self.state.doc) != before;
        // Declaring window-bar slots (or ceasing to) moves the app's
        // History between a band of its own and the bar.
        if self.floating.window_bar.note_slots_declared() {
            self.layout.request_rebuild();
        }
        // The record's durability changed and the root synced with it: a
        // frame presents it. Nothing about the tree changed, so it is not a
        // rebuild (which would repaint the whole window for a save bubble).
        // Taken before replaced widgets are looked for, which would read it
        // as a structure change.
        let mut save_status =
            crate::component_lifecycle::take_save_status_presentation(self.root_widget.as_mut());
        // Settlement can have replaced widgets (a surface closing its own
        // dialog): forget their nodes before dispatch, which may open a
        // confirmation that captures where focus returns.
        self.forget_replaced_widgets();
        let submitted = self.dispatch_control_invocations() != 0;
        if settled || submitted {
            self.root_widget.sync(&self.state.doc, &self.state.root);
        }
        save_status |=
            crate::component_lifecycle::take_save_status_presentation(self.root_widget.as_mut());
        if save_status {
            self.layout.layout_dirty = true;
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
    /// Where focus is, as an identity that survives a rebuild. The layout's
    /// map is read only while it still describes the live tree: replaced
    /// widgets are forgotten first, and then the focused control is found
    /// in the live tree instead.
    pub(super) fn capture_focus_identity(
        &mut self,
    ) -> Option<crate::widget::layout::CompositionFocus> {
        if self.forget_replaced_widgets() || self.layout.node_map.is_empty() {
            return crate::widget::layout::CompositionFocus::capture_live(
                self.root_widget.as_ref(),
            );
        }
        // SAFETY: no widget was replaced since the map was admitted, so it
        // describes this exclusively owned live tree.
        unsafe {
            crate::widget::layout::CompositionFocus::capture(
                self.focused_node,
                &self.layout.node_map,
                &self.bindings,
            )
        }
    }

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

    /// Whether a state pass outside a frame has work to do: a model update
    /// anything asked for, a sync the tree owes, or a control invocation
    /// settled state left queued for the sync to submit.
    #[cfg(all(feature = "native", test))]
    pub(super) fn state_tick_pending(&self) -> bool {
        self.needs_sync
            || crate::widget::controls::has_pending_control_invocation(self.root_widget.as_ref())
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

    /// Run the passes a frame runs before it paints, for a window that will
    /// not paint: model update, sync and the control invocations a sync
    /// submits, plus the layout those effects address. Rendering and state
    /// are decoupled, so an app nobody is looking at (occluded on macOS, on
    /// another Space, behind a full-screen app) still answers the wakes its
    /// workers, its engine and its action door send, and a deferred
    /// invocation is executed instead of waiting for a `RedrawRequested`
    /// that never comes.
    #[cfg(feature = "native")]
    pub(super) fn run_state_tick(&mut self) {
        let _entry = self.enter_wake_scope();
        self.poll_model_invocations();
        let updated = self.update_native_model();
        // A widget schedules work for itself ([`Widget::next_wake_deadline`]:
        // an engine handshake that is due, an operation whose window is about
        // to time out) and serves it from the sync. A window that paints
        // reaches it through the loop's deadline; one that is never drawn
        // reaches it here, and a deadline left to wait for a frame is a
        // request that answered nobody.
        let now = crate::time::now();
        let deadline_due = self
            .root_widget
            .next_wake_deadline()
            .is_some_and(|deadline| deadline <= now);
        let served = deadline_due && self.root_widget.serve_deadline(now);
        if !self.sync_model(updated || served) {
            return;
        }
        // What the sync submitted and what widgets settled are geometry the
        // effects behind them address.
        self.layout.structure_dirty |= self.take_root_structure_dirty();
        self.forget_replaced_widgets();
        if self.layout.structure_dirty {
            self.rebuild_structure();
        }
    }

    pub(super) fn service_idle_model(&mut self) -> IdleModelProgress {
        #[cfg(test)]
        let probe = crate::profiling::frame_probe::span("idle.discovery");
        let history_changed = self.refresh_local_undo_discovery();
        #[cfg(test)]
        drop(probe);
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
