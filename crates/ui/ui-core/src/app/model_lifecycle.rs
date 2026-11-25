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
    *BASELINE.get_or_init(|| crate::process::environment().diagnostics.host_perf_baseline)
}

/// `TILO_HOSTED_PUMP_TIMING=1` prints one payload-free line per hosted pump.
fn hosted_pump_timing() -> bool {
    static ENABLED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ENABLED.get_or_init(|| crate::process::environment().diagnostics.hosted_pump_timing)
}

#[cfg(feature = "model")]
/// Policy meaning observed by the previous hosted pump: the App's own context
/// and the root component owner's host-supplied context (the `Rc` replaced by
/// `HostWorker::pump`). Directory observations include per-capability liveness at
/// a fresh policy-time sample, so expiry is a change even when the context
/// value is identical.
#[derive(PartialEq, Eq)]
struct HostedAuthorityObservation {
    app: crate::permissions::DirectoryPermissionObservation,
    owner: Option<crate::permissions::DirectoryPermissionObservation>,
}

#[cfg(feature = "model")]
/// Every input of `publish_action_surface` that can change without a
/// rebuild of the widget tree. Equal stamps mean that publication would
/// recompute the same resources, descriptors and directory snapshot.
#[derive(PartialEq, Eq)]
pub(super) struct PublicationStamp {
    resource_generation: u64,
    permission_generation: u64,
    registry_generation: u64,
    route: Option<crate::collaboration::AcceptedRoute>,
    revision: String,
}

#[cfg(feature = "model")]
impl PublicationStamp {
    /// The action directory's revision the surface was published at.
    pub(super) fn revision(&self) -> &str {
        &self.revision
    }
}

#[derive(Default)]
pub(super) struct HostedPumpState {
    #[cfg(test)]
    pub(super) calls: usize,
    #[cfg(feature = "model")]
    authority: Option<HostedAuthorityObservation>,
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
        self.pump_hosted_model_report();
    }

    /// [`Self::pump_hosted_model`], answering what the pass took up, synchronized and published:
    /// a frame counts itself quiet only when the pass brought nothing.
    pub(super) fn pump_hosted_model_report(&mut self) -> HostedPumpReport {
        self.pump_hosted_model_as(host_perf_baseline())
    }

    /// One wake of an app the session runs with no window
    /// ([`crate::process::NO_WINDOW_ENV`]): the passes a frame runs before it
    /// paints, so it takes its actions and publishes. Answers when it wakes
    /// next.
    ///
    /// The shell speaks to it as to a window: it says where the app's
    /// workspace stands, which the app's views wait for before they build
    /// and publish what they answer. What the shell asks of a window (a
    /// surface to prepare) has no window here and goes unanswered.
    ///
    /// Only a native launch runs an app with no window; the portable
    /// toolkit's host always draws one, and the tests wake one here.
    #[cfg(any(feature = "native", test))]
    pub(crate) fn wake_unshown(&mut self) -> Option<crate::time::Instant> {
        #[cfg(feature = "native")]
        {
            if self.pump_floating_surface_service() {
                self.invalidate_widget_resource_cache();
            }
            drop(self.take_floating_host_messages());
        }
        // A window shown before its record is replaced once the record is had, as a frame
        // replaces it: with no frame, this wake is the one that takes it.
        self.swap_requested_component();
        self.pump_hosted_model();
        self.root_widget.next_wake_deadline()
    }

    /// Presence and connection changes can change pixels without dirtying a widget.
    #[cfg(feature = "native")]
    pub(super) fn pump_embedded_model(&mut self) -> bool {
        self.pump_hosted_model_as(host_perf_baseline())
            .presentation_changed
    }

    /// Why the session action host last refused one of this window's
    /// invocations outright, if it has: a refused action never runs, so a
    /// fixture waiting for its effect fails on this at once.
    #[cfg(any(test, feature = "test-support"))]
    pub fn controlled_refused_action_for_test(&self) -> Option<&str> {
        self.actions.refused_broker_receipt.as_deref()
    }

    /// `always_refresh` is the previous behaviour: synchronize, invalidate and
    /// republish unconditionally. Otherwise the pump is dirty-tracked like the
    /// window runner, whose redraw syncs only on `needs_sync`: a pass with no
    /// inbound event, no model-dirty wake, no policy change, no in-flight job,
    /// no due widget deadline and no model progress keeps the resource cache
    /// and skips the action-surface rebuild.
    pub(super) fn pump_hosted_model_as(&mut self, always_refresh: bool) -> HostedPumpReport {
        #[cfg(test)]
        {
            self.actions.hosted.calls += 1;
        }
        #[cfg(feature = "model")]
        let started = hosted_pump_timing().then(crate::time::now);
        let _entry = self.enter_wake_scope();
        #[cfg(test)]
        let probe = crate::profiling::frame_probe::span("pump.inputs");
        #[cfg(feature = "model")]
        let (inbound, directory) = self.pump_collab_presence_inner(true);
        // Without the model no peer or broker sends the window anything.
        #[cfg(not(feature = "model"))]
        let inbound = false;
        // Retire expired/cancelled jobs before their later model continuation.
        // A completion produced below is polled on the following host pass.
        let invocations = self.poll_model_invocations();
        // The owner retains wake flags even when its sink schedules a frame.
        // A presentation-only frame must not force model synchronization.
        // Detached or closed scopes remain conservatively model-dirty.
        let wakes = self.take_hosted_wakes();
        let authority_changed = self.refresh_hosted_authority();
        let in_flight = !self.actions.directed_invocations.is_empty()
            || self.actions.bound_calls_running()
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
        let taken_up = force_sync
            || invocations
            || self.needs_sync
            || wakes.is_some_and(|(pending, _)| pending);
        {
            #[cfg(test)]
            let _probe = crate::profiling::frame_probe::span("pump.update");
            // Every frame advances what moves in the widgets' update, as a window's frame does
            // (`update_native_model`): a pan, a fade that ends and leaves something to prune.
            // A widget that changed asks for the sync; one that moved nothing leaves the frame
            // quiet.
            let changed = self.update_model();
            report.updated = taken_up || changed;
            // A model-dirty wake raised while draining joins this sync. It
            // is left pending as well: the host prepares a frame only for a
            // wake it can see, so taking it here would leave the work it
            // asks for to a frame nobody requests.
            force_sync |= self.hosted_wake_sync_pending();
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
        // What this pass's drain invalidated is published here, after the sync its work went
        // through: building it before the sync published a tree this pass then changed, and the
        // pass built it again.
        #[cfg(feature = "model")]
        {
            let drained = std::mem::take(&mut self.actions.action_surface_owed);
            if always_refresh || !self.publication_current() {
                report.published = true;
                if always_refresh {
                    // Built again whatever the stamp says.
                    self.actions.published_stamp = None;
                }
                self.publish_action_surface();
            } else if drained {
                // A drain can invalidate what the surface offers without moving the stamp that
                // counts the tree (`App::pump_collab_presence_inner`), so it is published
                // whatever the stamp says, as that drain asks.
                report.published = true;
                self.publish_action_surface_after_refresh();
            }
            self.broadcast_doc_delta();
            // What search finds can move with the model (a refreshed vault, a
            // timer): the session's index follows it here too, not only on input.
            self.publish_search_index();
            self.finish_model_directory_refresh(directory);
        }
        #[cfg(feature = "model")]
        if let Some(started) = started {
            crate::diag!(
                "[hosted-pump] app={} late_us={:?} us={} updated={} synced={} published={} inbound={inbound} wakes={wakes:?} authority={authority_changed} in_flight={in_flight} deadline={deadline_due} invocations={invocations}",
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
        report
    }

    fn take_hosted_wakes(&self) -> Option<(bool, bool)> {
        self.wake_scope
            .as_ref()
            .and_then(crate::wake::WakeScope::take_pending)
    }

    /// Whether a wake raised during the pump asks for a model pass, asked
    /// without taking it: the wake stays pending for the next one too.
    fn hosted_wake_sync_pending(&self) -> bool {
        self.wake_scope
            .as_ref()
            .and_then(crate::wake::WakeScope::sync_pending)
            .is_none_or(|sync| sync)
    }

    /// Returns true when policy meaning changed since the previous hosted pump
    /// or cannot be sampled (fail closed: refresh on every such pass).
    fn refresh_hosted_authority(&mut self) -> bool {
        #[cfg(feature = "model")]
        let changed = self.refresh_workspace_authority();
        // A standalone window's authority is the person's own, and never moves.
        #[cfg(not(feature = "model"))]
        let changed = false;
        changed
    }

    #[cfg(feature = "model")]
    fn refresh_workspace_authority(&mut self) -> bool {
        let owner =
            if let Some(binding) = crate::bound_dispatch::callback_binding(&*self.root_widget) {
                let Ok(observation) = binding.permissions.borrow().directory_observation() else {
                    self.actions.hosted.authority = None;
                    return true;
                };
                Some(observation)
            } else {
                None
            };
        let Ok(app) = self.permission_ctx.directory_observation() else {
            self.actions.hosted.authority = None;
            return true;
        };
        let current = HostedAuthorityObservation { app, owner };
        let changed = self.actions.hosted.authority.as_ref() != Some(&current);
        self.actions.hosted.authority = Some(current);
        changed
    }

    #[cfg(feature = "model")]
    /// `None` whenever publication cannot be proven current: disconnected,
    /// nothing published yet, an evicted directory snapshot or stamp, or an
    /// exhausted generation that no longer distinguishes later mutations.
    pub(super) fn publication_stamp(&self) -> Option<PublicationStamp> {
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
        Some(PublicationStamp {
            resource_generation,
            permission_generation,
            registry_generation,
            route: self.bound_broker_route().cloned(),
            revision: revision.clone(),
        })
    }

    #[cfg(feature = "model")]
    /// Whether a publication now would recompute what the last one
    /// published, as far as the widget tree it reads stands.
    pub(super) fn publication_current(&self) -> bool {
        let Some(stamp) = self.publication_stamp() else {
            // A window with no workspace behind it has no route to publish on: its surface goes
            // out when it connects ([`Self::pump_collab_presence_inner`]). Until then only an
            // invocation of its own held for that route asks a publication, which refuses it.
            // Counting it stale ran a publication that carried nothing every frame, and no frame
            // of such a window was quiet.
            return !self.collaboration.manager.is_connected()
                && self.actions.held_own_invocations.is_empty();
        };
        Some(stamp) == self.actions.published_stamp
    }

    /// A native model pass acknowledges only wakes preceding this drain.
    /// Later worker completions retain their pending bit for the idle pass.
    #[cfg(feature = "native")]
    pub(super) fn update_native_model(&mut self) -> bool {
        self.needs_sync |= self.take_hosted_wakes().is_some_and(|(_, sync)| sync);
        self.update_model()
    }

    pub(super) fn update_model(&mut self) -> bool {
        let _phase = tilo_actions::profiling::Phase::start("ui.frame_update");
        // The workspace's presence is state of the frame, lent under the window's
        // authority, as the draw sees it.
        let (root, state, presence) = (&mut self.root_widget, &self.state, &self.presence_ctx);
        // What this update moved, not what a widget updated outside an app
        // noted before it: that stays noted for its own host (an app hosted
        // in a tile updates in the middle of its host's update).
        let outer = crate::widget::take_layout_moved();
        let update =
            || crate::presence::frame::lend(presence, || root.update(&state.doc, &state.root));
        #[cfg(feature = "model")]
        let updated = presence.with_permissions(self.permission_ctx.clone(), update);
        #[cfg(not(feature = "model"))]
        let updated = {
            let mut update = update;
            update()
        };
        // An animation moved what its widget lays out: the window is laid
        // out again. Any other animation frame is only drawn.
        self.layout.layout_dirty |= crate::widget::take_layout_moved();
        if outer {
            crate::widget::note_layout_moved();
        }
        // A part built for what arrived (a query's answer) is a new tree
        // though no model state moved: what it declares is read from it
        // from now on, before anything publishes.
        self.forget_replaced_widgets();
        if !updated {
            return false;
        }
        // What the model moved is laid out when a widget says its size moved: a picture that
        // arrived, a label that changed; a value that only repaints is drawn.
        self.invalidate_widget_resource_cache();
        self.needs_sync = true;
        self.layout.layout_dirty |=
            self.root_widget.is_layout_dirty() || self.root_widget.layout_needs_measure();
        true
    }

    pub(super) fn sync_model(&mut self, force: bool) -> bool {
        if !force && !self.needs_sync {
            return false;
        }
        let _phase = tilo_actions::profiling::Phase::start("ui.frame_sync");
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
        if self.floating.window_bar.note_parts_declared() {
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
        // An entry opening now seals what components queued before it, before anything below
        // would run or discard it.
        #[cfg(feature = "model")]
        if self
            .history
            .as_ref()
            .is_some_and(|history| history.entry_pending())
        {
            self.capture_history_entry();
        }
        // What a control queued runs in the frame that follows the input which queued it, as a
        // window runs it once its input stops: an entry that seals queued work (History taking
        // the input) has taken it before this, input framework chrome took runs nothing at all,
        // and a window still opening runs none of it either.
        #[cfg(feature = "model")]
        let submitted = {
            let queued =
                crate::widget::controls::has_pending_control_invocation(self.root_widget.as_ref());
            let held = self.input.opening || self.input.passive_input;
            !held && (settled || queued) && self.dispatch_control_invocations() != 0
        };
        // Controls bound to actions run where the model runs them.
        #[cfg(not(feature = "model"))]
        let submitted = false;
        if settled || submitted {
            self.root_widget.sync(&self.state.doc, &self.state.root);
        }
        save_status |=
            crate::component_lifecycle::take_save_status_presentation(self.root_widget.as_mut());
        // What the sync handed the widgets (a value, a label, a page shown) is laid out when
        // one of them says its size moved: an input that only repainted asked for no layout.
        if save_status
            || self.root_widget.is_layout_dirty()
            || self.root_widget.layout_needs_measure()
        {
            self.layout.layout_dirty = true;
        }
        self.invalidate_widget_resource_cache();
        self.needs_sync = false;
        // What the sync handed the widgets is what they declare (a value, a label, a row that
        // arrived), whether or not it moved anything the layout places: the tree is read again.
        self.mark_semantic_tree_stale();
        self.forget_replaced_widgets();
        #[cfg(feature = "model")]
        {
            self.answer_held_invocations();
        }
        true
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
        // Without the model nothing directs an invocation at the window.
        #[cfg(feature = "model")]
        let changed = self.poll_directed_invocations();
        #[cfg(not(feature = "model"))]
        let changed = false;
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
            changed |= self.swap_requested_component();
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
    /// workers, its engine and its action API send, and a deferred
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
        #[cfg(feature = "model")]
        let changed = {
            let history_changed = self.refresh_local_undo_discovery();
            #[cfg(test)]
            drop(probe);
            self.service_local_control_history() || history_changed
        };
        #[cfg(not(feature = "model"))]
        let changed = {
            #[cfg(test)]
            drop(probe);
            false
        };
        // This is local writer progress, never a durable owner receipt. The
        // existing scoped publication barrier still checks terminal success.
        #[cfg(all(feature = "native", feature = "model"))]
        if let Err(error) = self.autosave_tick() {
            crate::diag!("[app] component autosave tick refused: {error:?}");
        }
        IdleModelProgress { changed }
    }
}

#[cfg(test)]
#[path = "replaced_widget_tests.rs"]
mod replaced_widget_tests;
