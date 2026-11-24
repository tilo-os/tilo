// Collaboration transport, presence, permissions, and document integration.

impl App {
    /// Prepare and mount an application on its already admitted collaboration manager.
    /// Host preparation runs inside the existing native read ownership. The original
    /// record binding is checked before the component factory runs, and the same
    /// manager and wake scope transfer into the mounted application exactly once.
    /// Construct the host and its record owners inside `prepare_host`, where
    /// they retain this construction's native read bridge. Only an existing
    /// record target is accepted; absence cannot become a default new document.
    ///
    /// # Errors
    /// Returns preparation, current-read, binding, factory or mount failures.
    /// Binding failures preserve the original record store for recovery.
    pub fn from_prepared_component_with_collaboration(
        collaboration: CollaborationManager,
        prepare_host: impl FnOnce() -> Result<
            crate::application::PreparedComponentInputs,
            crate::launch::LaunchError,
        >,
        instantiate: impl FnOnce(
            crate::application::ProcessContext,
            crate::application::LaunchTarget,
            crate::application::RestoreInput,
        )
            -> Result<crate::application::AppComponent, crate::launch::LaunchError>,
    ) -> Result<Self, crate::launch::LaunchError> {
        Self::mount_on_collaboration(collaboration, prepare_host, instantiate, false)
    }

    /// [`Self::from_prepared_component_with_collaboration`] for a window as
    /// the live native launch prepares it (`ApplicationOwner::prepare_default`):
    /// a workspace record the workspace does not hold yet is new, and the component
    /// makes it, its seed included. For tests that launch an app the way the
    /// session does, without a process of its own.
    ///
    /// # Errors
    /// As [`Self::from_prepared_component_with_collaboration`].
    #[cfg(any(test, feature = "test-support"))]
    pub fn from_launched_component_with_collaboration(
        collaboration: CollaborationManager,
        prepare_host: impl FnOnce() -> Result<
            crate::application::PreparedComponentInputs,
            crate::launch::LaunchError,
        >,
        instantiate: impl FnOnce(
            crate::application::ProcessContext,
            crate::application::LaunchTarget,
            crate::application::RestoreInput,
        )
            -> Result<crate::application::AppComponent, crate::launch::LaunchError>,
    ) -> Result<Self, crate::launch::LaunchError> {
        Self::mount_on_collaboration(collaboration, prepare_host, instantiate, true)
    }

    /// Mount a component on its admitted collaboration manager; a target
    /// other than an existing record only when `launched` (see
    /// [`Self::from_launched_component_with_collaboration`]).
    fn mount_on_collaboration(
        collaboration: CollaborationManager,
        prepare_host: impl FnOnce() -> Result<
            crate::application::PreparedComponentInputs,
            crate::launch::LaunchError,
        >,
        instantiate: impl FnOnce(
            crate::application::ProcessContext,
            crate::application::LaunchTarget,
            crate::application::RestoreInput,
        )
            -> Result<crate::application::AppComponent, crate::launch::LaunchError>,
        launched: bool,
    ) -> Result<Self, crate::launch::LaunchError> {
        use crate::collaboration::native_initial::{self, NativeConstruction};
        use crate::launch::{LaunchError, LaunchRecovery};
        let scope = crate::wake::WakeScope::new();
        let _entry = scope.enter();
        let construction = NativeConstruction::enter_component(collaboration, scope)
            .map_err(LaunchError::Operation)?;
        let prepared = prepare_host()?;
        let binding = prepared.context.callback_binding();
        if !launched
            && !matches!(
                prepared.target,
                crate::application::LaunchTarget::Record { .. }
            )
        {
            return Err(LaunchError::ComponentSetup {
                source: Box::new(LaunchError::Operation(
                    "cold component construction requires an existing record".into(),
                )),
                record: Box::new(binding.checkout),
                recovery: LaunchRecovery::records(binding.records),
            });
        }
        if let Err(error) = construction.bind_component(&binding) {
            return Err(LaunchError::ComponentSetup {
                source: Box::new(LaunchError::Operation(error)),
                record: Box::new(binding.checkout),
                recovery: LaunchRecovery::records(binding.records),
            });
        }
        let component = instantiate(prepared.context, prepared.target, prepared.restore)?;
        let mut app = native_initial::transfer_to_app(|| {
            Self::from_component_using(component, prepared.runner_inputs, |inputs, binding| {
                let mut inputs = AppConstructionInputs::component(inputs, binding);
                // The transfer guard above retains the original worker. This
                // callback runs after App assembly takes the original wake scope.
                inputs.collab_manager =
                    native_initial::take_manager().unwrap_or_else(CollaborationManager::disabled);
                inputs
            })
        })
        .map_err(LaunchError::Operation)??;
        if let Err(error) = native_initial::check_current() {
            app.close_components();
            return Err(LaunchError::ComponentSetup {
                source: Box::new(LaunchError::Operation(error)),
                record: Box::new(binding.checkout),
                recovery: LaunchRecovery::new(
                    Some(binding.records),
                    app.take_component_close_reports(),
                ),
            });
        }
        Ok(app)
    }
    pub fn set_presence_broadcast_sink<F>(&mut self, sink: F)
    where
        F: Fn(&PresenceKey, &PresencePayload) + Send + Sync + 'static,
    {
        self.presence.broadcast_sink = Some(Box::new(sink));
    }

    pub fn clear_presence_broadcast_sink(&mut self) {
        self.presence.broadcast_sink = None;
    }

    pub fn set_pointer_space(&mut self, space: crate::presence::PointerSpace) {
        self.presence.pointer_space = Some(space);
    }

    #[cfg(feature = "native")]
    fn local_pointer_key(&self) -> PresenceKey {
        match self.presence.pointer_space.clone() {
            Some(space) => PresenceKey::Pointer { space },
            None => crate::presence::local_surface_pointer(),
        }
    }

    /// Decides *whether* to publish and under what scope, not what space the
    /// pointer is in — see [`Self::set_pointer_space`].
    pub fn set_presence_broadcast_scope(&mut self, key: Option<PresenceKey>) {
        let _entry = self.enter_wake_scope();
        self.presence.broadcast_scope.clone_from(&key);
        // Reset the throttle so a new scope's first move fires immediately.
        self.presence.last_pointer_broadcast = None;
        // Arrival remains ephemeral presence only. Document catch-up is a
        // broker-directed request with one selected snapshot responder.
        if let Some(k) = key {
            tracing::debug!(
                target: "tilo.state",
                event = "presence_arrived",
                connected = self.collaboration.manager.is_connected(),
                "publishing collaboration presence"
            );
            self.broadcast_presence(k, crate::collaboration::PresencePayload::Arrived);
        }
    }

    pub fn presence_broadcast_scope(&self) -> Option<&PresenceKey> {
        self.presence.broadcast_scope.as_ref()
    }

    pub fn presence_ctx(&self) -> &PresenceCtx {
        &self.presence_ctx
    }
    pub fn presence_ctx_mut(&mut self) -> &mut PresenceCtx {
        self.mark_semantic_tree_stale();
        &mut self.presence_ctx
    }

    pub fn followed_peer(&self) -> Option<&crate::permissions::MemberId> {
        self.presence.followed_peer.as_ref()
    }

    pub fn followed_peer_target(&self) -> Option<[f32; 2]> {
        let id = self.presence.followed_peer.as_ref()?;
        self.presence_ctx.peer_pointer(id)
    }

    /// A miss leaves the current follow target untouched.
    pub fn try_follow_click(
        &mut self,
        viewport: vello::kurbo::Rect,
        point: vello::kurbo::Point,
    ) -> bool {
        let _entry = self.enter_wake_scope();
        if let Some(uid) = self.presence_ctx.hit_test_offscreen_mist(viewport, point) {
            self.presence.followed_peer = Some(uid);
            true
        } else {
            false
        }
    }

    pub fn permission_ctx(&self) -> &PermissionContext {
        &self.permission_ctx
    }
    pub fn set_permission_ctx(&mut self, ctx: PermissionContext) {
        let _entry = self.enter_wake_scope();
        self.permission_ctx = ctx;
        self.mark_semantic_tree_stale();
        // See `invalidate_widget_resource_cache`: once the numeric namespace
        // is exhausted, dropping the projection is the collision-free way to
        // make the changed permissions visible without reusing a live cache.
        if let Some(next) = self.actions.permission_generation.get().checked_add(1) {
            self.actions.permission_generation.set(next);
        }
        self.actions.action_directory_snapshot = None;
        self.actions.action_directory_stamp = None;
        self.actions.action_directory_cursors.clear();
        self.actions.action_directory_retired = None;
        self.actions.action_directory_borrowed = None;
        self.actions.local_resource_cache.get_mut().take();
        self.actions.open_resource_search_cache.get_mut().take();
        self.actions.last_control_availability = None;
        self.refresh_control_action_availability();
        self.layout.layout_dirty = true;
    }

    /// The server attributes the broadcast to this user's id automatically,
    /// so callers only specify what changed.
    pub fn broadcast_presence(
        &mut self,
        key: crate::presence::PresenceKey,
        payload: crate::collaboration::PresencePayload,
    ) {
        let _entry = self.enter_wake_scope();
        // Fan out to the optional out-of-band sink first (the shell's Iroh
        // bridge) so a slow socket never delays the network path.
        if let Some(sink) = self.presence.broadcast_sink.as_ref() {
            sink(&key, &payload);
        }
        self.collaboration
            .manager
            .send(CollabRequest::Presence { key, payload });
    }

    /// Publish the collaboration rail's current status through the real workspace
    /// presence path. `None` is an explicit clear. Only workspace keys are
    /// accepted, preventing an intention from being accidentally scoped to a
    /// single widget and disappearing as the member navigates.
    pub fn broadcast_status(
        &mut self,
        workspace: crate::presence::PresenceKey,
        status: Option<&collab_types::presence::LiveStatus>,
    ) -> bool {
        if !matches!(workspace, crate::presence::PresenceKey::Workspace { .. }) {
            return false;
        }
        let (text, automatic) = status.map_or_else(
            || (String::new(), false),
            |status| {
                (
                    status.text.clone(),
                    matches!(
                        status.origin,
                        collab_types::presence::StatusOrigin::Inferred { .. }
                    ),
                )
            },
        );
        self.broadcast_presence(
            workspace,
            crate::collaboration::PresencePayload::Status { text, automatic },
        );
        true
    }

    /// Record what a document hand-off means for the user-visible sync
    /// status. Returns whether the transport accepted the change, which is
    /// the only condition under which a sender may advance its frontier.
    fn note_document_send(
        &mut self,
        result: &Result<(), crate::collaboration::CollabSendError>,
    ) -> bool {
        if let Some(status) = document_routes::outbound_failure(result)
            && self.document_routes.outbound != status
        {
            self.document_routes.outbound = status;
            self.layout.layout_dirty = true;
        }
        result.is_ok()
    }

    /// Live-edit path (the late-join handshake still ships a full snapshot).
    /// The frontier moves only once the transport has accepted the delta; a
    /// busy transport leaves it for the next wakeup and an over-limit delta
    /// is reported through [`Self::document_sync_status`].
    pub(super) fn broadcast_doc_delta(&mut self) {
        let _phase = tilo_resource::profiling::Phase::start("ui.broadcast_doc_delta");
        self.restore_failed_document_publications();
        // A surface's record is the shell's own: it syncs with no one, so
        // there is no document route to broadcast it on.
        if self.collaboration.manager.hosts_surface() {
            self.broadcast_workspace_index_delta();
            return;
        }
        if self.collaboration.manager.is_connected() {
            match self.pending_primary_document() {
                Ok(Some((mutation, heads, commit))) => {
                    tracing::debug!(
                        target: "tilo.state",
                        event = "document_send",
                        kind = match mutation {
                            collab_types::DocumentMutation::Snapshot(_) => "snapshot",
                            collab_types::DocumentMutation::Incremental(_) => "incremental",
                        },
                        bytes = mutation.data().len(),
                        heads = heads.len(),
                        "queueing document mutation"
                    );
                    let request = document_routes::part_request(mutation, commit);
                    let result = self.collaboration.manager.try_send(request);
                    if self.note_document_send(&result) {
                        self.advance_primary_document(heads);
                    }
                }
                Ok(None) => {}
                Err(error) => {
                    tracing::warn!(%error, "collab: primary document is unavailable");
                }
            }
        }
        self.broadcast_workspace_index_delta();
    }

    /// The workspace index is a distinct document from the live record.  Keep its
    /// incremental cursor and route separate so an annotation/document delta
    /// can never be merged into the index (or vice versa).
    fn broadcast_workspace_index_delta(&mut self) {
        self.restore_failed_document_publications();
        let connected = self.collaboration.workspace_manager.as_ref().map_or_else(
            || self.persistence.records.is_some() && self.collaboration.manager.is_connected(),
            CollaborationManager::is_connected,
        );
        if !connected {
            return;
        }
        let Some(workspace) = self.persistence.workspace_index.as_mut() else {
            return;
        };
        let (delta, heads) = workspace.pending_outbound_delta();
        if delta.is_empty() {
            return;
        }
        let request = crate::collaboration::CollabRequest::document_incremental(delta);
        let result = if let Some(route) = self.native_workspace_route.clone() {
            self.collaboration.manager.try_send_to_route(route, request)
        } else if let Some(manager) = self.collaboration.workspace_manager.as_mut() {
            if !manager.is_connected() {
                return;
            }
            manager.try_send(request)
        } else if self.persistence.records.is_some() && self.collaboration.manager.is_connected() {
            // Before the first record checkout the primary manager is the
            // workspace route. It becomes `collaboration.workspace_manager` at that checkout.
            // Do not advance this cursor for ordinary (non-record) apps.
            self.collaboration.manager.try_send(request)
        } else {
            return;
        };
        if result.is_ok() {
            workspace.advance_outbound(heads);
        }
        self.note_document_send(&result);
    }

    #[cfg(feature = "native")]
    fn broadcast_focus_presence(&mut self) {
        use crate::collaboration::PresencePayload;
        let current = self.focused_node.and_then(|id| {
            self.inspect_node(id, |widget| {
                crate::presence::capability::presence_payload(widget)
            })
            .ok()
            .flatten()
        });
        if current != self.presence.last_focus {
            // When the caret advances forward under the same scope, publish
            // `Typing` for the inserted run so peers light up a fading
            // highlighter. Backspace/left-arrow and scope changes don't
            // qualify.
            if let (
                Some((new_key, PresencePayload::Caret { pos: new_pos })),
                Some((old_key, PresencePayload::Caret { pos: old_pos })),
            ) = (&current, &self.presence.last_focus)
                && new_key == old_key
                && new_pos > old_pos
            {
                self.broadcast_presence(
                    new_key.clone(),
                    PresencePayload::Typing {
                        start: *old_pos,
                        end: *new_pos,
                    },
                );
            }
            if let Some((key, payload)) = current.clone() {
                self.broadcast_presence(key, payload);
            }
            self.presence.last_focus = current;
        }
    }

    /// What the app is composing, and where: a conversation's "… is typing". Looked at every
    /// frame in the focused widget (a field typed in), else the root (an app keeping its own
    /// composers): said while the person types, and a draft that empties or loses focus says so
    /// at once, so no reader is left typing after they stop.
    #[cfg(feature = "native")]
    fn broadcast_composing_presence(&mut self) {
        let focused = self.focused_node.and_then(|id| {
            self.inspect_node(id, |widget| {
                crate::presence::capability::composing_draft(widget)
            })
            .ok()
            .flatten()
        });
        let draft = focused
            .or_else(|| crate::presence::capability::composing_draft(self.root_widget.as_ref()));
        if let Some((key, payload)) = self.presence.composing.look(draft, crate::time::now()) {
            self.broadcast_presence(key, payload);
        }
    }

    /// Unlike focus presence, this is polled on the root regardless of
    /// focus.
    #[cfg(feature = "native")]
    fn broadcast_location_presence(&mut self) {
        let current = crate::presence::capability::presence_payload(self.root_widget.as_ref());
        if current != self.presence.last_location {
            if let Some((key, payload)) = current.clone() {
                self.broadcast_presence(key, payload);
            }
            self.presence.last_location = current;
        }
    }

    /// The subset has no `PresencePayload` variant of its own — it rides the
    /// existing presence frame inside a
    /// [`PresenceKey::view`](crate::presence::PresenceKey::view) key with an
    /// inert paired payload; the receiver recognises the key and decodes the
    /// subset instead.
    #[cfg(feature = "native")]
    fn broadcast_view_presence(&mut self) {
        if !self.collaboration.manager.is_connected() && self.presence.broadcast_sink.is_none() {
            return;
        }
        let view = <Self as crate::view::FreezeThaw>::capture_view(self);
        let subset = crate::view::broadcast_subset(&view);
        if self.presence.last_view.as_ref() == Some(&subset) {
            return;
        }
        self.broadcast_presence(
            crate::presence::PresenceKey::view(&subset),
            // Inert: the receiver dispatches on the key before inspecting
            // the payload; `Arrived` because the SDK transport relays it
            // with the routing key intact.
            crate::collaboration::PresencePayload::Arrived,
        );
        self.presence.last_view = Some(subset);
    }

    /// Following a peer who hasn't published a View yet latches the intent
    /// without moving anything; the viewport snaps to them once their first
    /// View arrives.
    pub fn follow(&mut self, peer: crate::permissions::MemberId) {
        let _entry = self.enter_wake_scope();
        self.presence.followed_peer = Some(peer);
        self.apply_follow();
    }

    pub fn unfollow(&mut self) {
        let _entry = self.enter_wake_scope();
        self.presence.followed_peer = None;
    }

    fn apply_follow(&mut self) {
        let Some(id) = self.presence.followed_peer.clone() else {
            return;
        };
        let Some(view) = self.presence_ctx.peer_view(&id).cloned() else {
            return;
        };
        crate::view::capability::restore_view_state(
            self.root_widget.as_mut(),
            &crate::view::ViewContribution {
                scroll: view.scroll.clone(),
                view_mode: view.view_mode,
            },
        );
        self.layout.layout_dirty = true;
        self.needs_sync = true;
    }

    /// Lets integration tests drive the collab pump without a real winit
    /// event loop.
    #[doc(hidden)]
    pub fn poll_and_apply_collab_events(&mut self) {
        let _entry = self.enter_wake_scope();
        self.pump_workspace_index_events();
        let events = self.collaboration.manager.poll();
        for event in events {
            self.apply_collab_event_for_test(event);
        }
        self.collaboration.manager.finish_event_batch();
        self.drain_local_input_capture();
    }

    /// Apply a change of the workspace index, the record of the unqualified
    /// workspace route.
    fn apply_workspace_index_mutation(
        &mut self,
        mutation: collab_types::DocumentMutation,
    ) -> bool {
        let Some(workspace) = self.persistence.workspace_index.as_mut() else {
            return false;
        };
        let applied = match mutation {
            collab_types::DocumentMutation::Snapshot(data) => {
                workspace.merge_snapshot(&data).is_ok()
            }
            collab_types::DocumentMutation::Incremental(data) => {
                workspace.load_incremental(&data).is_ok()
            }
        };
        if applied
            && let (Some(records), Some(workspace)) = (
                self.persistence.records.as_mut(),
                self.persistence.workspace_index.as_ref(),
            )
        {
            records.discover_workspace(workspace.document());
            self.invalidate_widget_resource_cache();
        }
        applied
    }

    /// Drain only document traffic on the workspace-index route. Actions and
    /// presence are record scoped, so an index update cannot alter the app's
    /// resource/action namespace or presentation state.
    fn pump_workspace_index_events(&mut self) -> bool {
        let Some(manager) = self.collaboration.workspace_manager.as_mut() else {
            return false;
        };
        if manager.primary_route().is_none() {
            return false;
        }
        let events = manager.poll();
        let mut any = false;
        for event in events {
            match event {
                CollabEvent::CommittedDocument { route, read } => {
                    self.queue_committed_document(route, read)
                }
                CollabEvent::Document { mutation, .. } => {
                    any |= self.apply_workspace_index_mutation(mutation);
                }
                _ => {}
            }
        }
        any
    }

    /// Shared by the per-frame tick and the headless test path. Returns
    /// `true` iff presentation changed (not merely that a packet arrived).
    pub fn pump_collab_presence(&mut self) -> bool {
        self.pump_collab_presence_inner(false).0
    }

    /// Send what the window's clients asked of the broker. An action request made while the
    /// connection still joins its primary route waits for it: the manager carries nothing else
    /// until then, and an invocation it dropped would never be answered.
    fn send_collab_requests(&mut self) {
        let collaboration = &mut self.collaboration;
        collaboration
            .held_requests
            .extend(crate::widget::take_collab_requests(
                self.root_widget.as_mut(),
            ));
        let connecting = matches!(
            collaboration.manager.state(),
            crate::collaboration::CollaborationState::Connecting
        );
        let mut held = std::collections::VecDeque::new();
        for request in collaboration.held_requests.drain(..) {
            let action = matches!(
                request,
                crate::collaboration::CollabRequest::RemoteAction { .. }
            );
            if action && connecting {
                held.push_back(request);
            } else {
                collaboration.manager.send(request);
            }
        }
        collaboration.held_requests = held;
    }

    fn pump_collab_presence_inner(
        &mut self,
        model_refresh: bool,
    ) -> (
        bool,
        Option<action_directory_snapshot::ModelDirectoryCandidate>,
    ) {
        let _entry = self.enter_wake_scope();
        let mut any = self.pump_workspace_index_events();
        // Replies to broker pull-directory callbacks must run before regular
        // inbound events. This is the sole place capability requests acquire
        // widget/document authority, and it always runs on the App owner
        // thread.
        self.pump_action_directory_requests();
        // Hosted continuity starts after the sole request drain, before
        // notifications can cause scheduling-only invalidation. Retain live
        // stamp/snapshot for invocation checks later in this same batch.
        let directory = model_refresh
            .then(|| self.begin_model_directory_refresh())
            .flatten();
        self.pump_committed_documents();
        // One route reconcile serves both the inbound drain and the owned
        // publication queue.
        self.pump_document_routes();
        self.send_collab_requests();
        any |= self.apply_heard_elsewhere();
        self.pump_document_content();
        let (drained, action_surface_may_have_changed) = self.drain_collab_presence_events();
        any |= drained;
        any |= self.observe_collaboration_state();
        if any {
            self.invalidate_widget_resource_cache();
        }
        // Held own invocations wait for the publication this pump makes once
        // the route is ready, or for the refusal once the connection is gone.
        let action_surface_needs_publication = (self.collaboration.manager.is_connected()
            && (self.actions.last_published_actions.is_none()
                || self.actions.action_directory_snapshot.is_none()))
            || !self.actions.held_own_invocations.is_empty();
        // The broker refused the last surface: republish it now, not on the
        // next unrelated change.
        let refused_surface = self.collaboration.manager.take_action_surface_failed();
        if refused_surface {
            self.actions.last_published_actions = None;
        }
        if action_surface_may_have_changed || action_surface_needs_publication || refused_surface {
            // The normal idle pump also repairs invalidation from a later
            // model sync or a presence-only batch. Neither necessarily brings
            // another action event after a terminal operation was published.
            // An already-ready transport also needs its first publication;
            // it will not produce a new connection-state transition.
            // Query/resolve remain bounded reads; this refresh runs even with
            // no directory requests or inbound events, and skips stable idle.
            // The same pump already reconciled ownership for its document drain.
            self.publish_action_surface_after_refresh();
        }
        // Before-action diagnostics use the existing launcher-owned startup
        // checkpoint, never a published resource or a document mutation.
        let diagnostics = &crate::process::environment().diagnostics;
        // Ordinary launched workers have no synthetic native scenario. Their
        // explicit timing flag must still export the before-action checkpoint.
        if diagnostics.presentation_timing || diagnostics.presentation_profile {
            tilo_startup_metrics::checkpoint_native_admission(self.native_admission_snapshot());
        }
        (any, directory)
    }

    /// Where the app's collaboration connection stands. Managers connect in
    /// the background, so an app opens [`CollaborationState::Connecting`]
    /// and later becomes ready or offline; a frame is scheduled at each
    /// change.
    ///
    /// [`CollaborationState::Connecting`]: crate::collaboration::CollaborationState::Connecting
    pub fn collaboration_state(&self) -> crate::collaboration::CollaborationState {
        self.collaboration.manager.state()
    }

    /// Whether the app offered its action directory on its route, and how
    /// many of its offers the broker took and refused (`None` without a
    /// transport): what a host waiting for the app's directory says when it
    /// never arrives.
    pub fn action_directory_observation(&self) -> (bool, Option<(u64, u64)>) {
        self.collaboration.manager.action_directory_observation()
    }

    /// What this device can do with the document's end-to-end encrypted
    /// channel: while it is waiting for access (a new device before its
    /// welcome arrives) or was removed, nothing is read or written and the
    /// app says so instead of showing stale or undecryptable content. `None`
    /// until the route reports.
    pub fn key_access(&self) -> Option<crate::collaboration::KeyAccessState> {
        self.collaboration.key_access
    }

    /// React to the managers' connection changes. Work that only runs while
    /// connected is level-triggered, so a manager that has just become ready
    /// publishes the current state once instead of waiting for the next
    /// unrelated change. Returns whether anything changed.
    fn observe_collaboration_state(&mut self) -> bool {
        let workspace_ready = self
            .collaboration
            .workspace_manager
            .as_mut()
            .and_then(CollaborationManager::take_state_change)
            .is_some_and(|state| state.is_ready());
        if workspace_ready {
            self.broadcast_workspace_index_delta();
        }
        // What holds a join up, and a join refused or lost, change what the
        // window's status line says, not the window's model.
        let status = self
            .document_sync_status()
            .map(std::borrow::Cow::into_owned);
        let restated = status != self.collaboration.shown_sync_status;
        if restated {
            self.collaboration.shown_sync_status = status;
            self.layout.layout_dirty = true;
        }
        let Some(state) = self.collaboration.manager.take_state_change() else {
            return workspace_ready || restated;
        };
        tracing::debug!(
            target: "tilo.state",
            event = "collaboration_state",
            state = ?state,
            "collaboration connection changed"
        );
        if state.is_ready() {
            // Announcements made while connecting were refused; repeat the
            // current one on the acknowledged route.
            if let Some(scope) = self.presence.broadcast_scope.clone() {
                self.collaboration.manager.send(CollabRequest::Presence {
                    key: scope,
                    payload: crate::collaboration::PresencePayload::Arrived,
                });
            }
            // Only a window on its own instance route has an owner view to
            // replay; a window showing a sealed record has none to ask for.
            if self.private_continuity_route().is_some() {
                self.report_handoff(
                    &crate::collaboration::handoff::HandoffReport::OwnerSubscribe { version: 1 },
                );
            }
            self.publish_continuation_metadata();
            self.publish_search_index();
            self.broadcast_doc_delta();
            self.publish_action_surface();
        }
        true
    }

    /// Applies every queued collaboration event. Returns whether presentation
    /// changed and whether one of them may have changed the action surface.
    ///
    /// Widget-owned replies (Nearby Send snapshots, catalogues) still run
    /// `handle_collab_event`. They do not by themselves request a frame:
    /// Files' 2s poll used to recommit an unchanged listing because `any`
    /// meant "a packet arrived".
    fn drain_collab_presence_events(&mut self) -> (bool, bool) {
        let events = self.collaboration.manager.poll();
        let mut any = false;
        let mut action_surface_may_have_changed = false;
        for event in events {
            action_surface_may_have_changed |= Self::collab_event_may_change_action_surface(&event);
            // Always deliver the event. Skipping an unchanged present must
            // not skip widget handling (keys, catalogues, Nearby replies).
            self.forward_session_notification(&event);
            any |= Self::collab_event_updates_app_presentation(&event);
            self.apply_collab_presence_event(event);
            if self.forget_replaced_widgets() {
                any = true;
            }
        }
        self.collaboration.manager.finish_event_batch();
        self.drain_local_input_capture();
        (any, action_surface_may_have_changed)
    }

    /// The content links `route`'s record has its content on in this process: the
    /// record's own, and each window's showing it
    /// ([`ContentLink::on_route`](crate::collaboration::document_content::ContentLink::on_route)).
    fn links_on_route(
        route: &crate::collaboration::RouteSpec,
    ) -> Vec<std::sync::Arc<crate::collaboration::document_content::ContentLink>> {
        crate::collaboration::document_content::ContentLink::on_route(route)
    }

    /// Send what the window's record content store queued on the record's
    /// route: content it lacks, and content it stored for the record's
    /// other readers. See [`crate::collaboration::document_content`].
    fn pump_document_content(&mut self) {
        use crate::collaboration::document_content::ContentLink;
        if !self.collaboration.manager.is_connected() {
            return;
        }
        let Some(route) = self
            .collaboration
            .manager
            .primary_route()
            .map(|observation| observation.route().clone())
        else {
            return;
        };
        // The window's own content link, the one its record content travels on:
        // what another window of this app in this process asked for is not
        // this window's to send.
        let own = {
            let _entry = self.enter_wake_scope();
            ContentLink::of_window()
        };
        let links = std::iter::once((None, own)).chain(
            ContentLink::routed(&route.app_id)
                .into_iter()
                .map(|(route, link)| (Some(route), link)),
        );
        for (route, link) in links {
            {
                let _entry = self.enter_wake_scope();
                link.attach(crate::WakeHandle::new());
            }
            self.send_link_requests(route.as_ref(), &link);
        }
    }

    /// Send what `link` queued: on the window's own route, or on `route`
    /// for a record that is not the window's.
    fn send_link_requests(
        &mut self,
        route: Option<&crate::collaboration::RouteSpec>,
        link: &crate::collaboration::document_content::ContentLink,
    ) {
        let mut send = |request: CollabRequest| match route {
            Some(route) => self
                .collaboration
                .manager
                .try_send_to_route(route.clone(), request),
            None => self.collaboration.manager.try_send(request),
        };
        let requests = link.take_requests();
        for hash in requests.wants {
            if let Err(error) = send(CollabRequest::WantContent { hash: hash.clone() }) {
                tracing::debug!(%error, "collab: record content request deferred");
                link.retry_want(hash);
            }
        }
        for (hash, bytes) in requests.offers {
            match send(CollabRequest::OfferContent {
                hash: hash.clone(),
                bytes: bytes.clone(),
            }) {
                Ok(()) | Err(crate::collaboration::CollabSendError::TooLarge { .. }) => {}
                Err(_) => link.retry_offer(hash, bytes),
            }
        }
    }

    fn collab_event_updates_app_presentation(event: &CollabEvent) -> bool {
        !matches!(
            event,
            CollabEvent::NearbyTransferCommandResult { .. }
                | CollabEvent::NearbyTransferChanged
                | CollabEvent::SpeakersChanged
                | CollabEvent::DrivesChanged
                | CollabEvent::ApplicationsChanged
                | CollabEvent::ActionCatalogue { .. }
                | CollabEvent::NativeMediaCommandResult { .. }
                | CollabEvent::WorkspaceControl { .. }
                | CollabEvent::RealtimeCommandResult { .. }
                | CollabEvent::ResourceCommandResult { .. }
                | CollabEvent::ResourcePage { .. }
                | CollabEvent::RecordsMoved { .. }
                | CollabEvent::Realtime(_)
        )
    }
    fn apply_collab_presence_event(&mut self, event: CollabEvent) {
        let _background = crate::local_document_action::suspend();
        match event {
            CollabEvent::LocalDocumentActionReported { request_id, result } => {
                self.observe_local_input_capture(request_id, result);
            }
            CollabEvent::Context { members, apps, .. } => {
                self.apply_collab_context(members);
                // What each app installed here is called: the same feed names the people in it.
                crate::history_source::set_installed_apps(apps);
            }
            CollabEvent::ResourcesArrived { records } => self.resources_arrived(&records),
            CollabEvent::ResourceUnavailable { route } => self.resource_unavailable(&route),
            CollabEvent::FollowChanged { peer_id } => match peer_id.clone() {
                Some(peer) => self.follow(peer),
                None => self.unfollow(),
            },
            CollabEvent::DocumentContent { route, hash, bytes } => {
                // The record's content reaches every content link carrying it, the
                // windows' own included: a window's store reads the content link it
                // keeps, never the record's.
                for link in Self::links_on_route(&route) {
                    link.arrive(hash.clone(), bytes.clone());
                }
            }
            CollabEvent::DocumentContentState { route, hash, state } => {
                for link in Self::links_on_route(&route) {
                    link.stands(hash.clone(), state);
                }
            }
            CollabEvent::RemoteCursorMoved { member, x, y } => {
                self.presence_ctx.upsert_remote_cursor(member, [x, y], None);
            }
            CollabEvent::RemotePresenceUpdate {
                member,
                key,
                payload,
            } => {
                self.apply_remote_presence(member, key, payload);
            }
            CollabEvent::Document { route, mutation } => {
                self.apply_document_collab_event(&route, mutation);
                self.observe_primary_retirement();
            }
            CollabEvent::Handoff { request } => self.apply_handoff(request),
            CollabEvent::CommittedDocument { route, read } => {
                self.queue_committed_document(route, read);
            }
            CollabEvent::Invocation {
                invocation,
                id,
                resource,
                action,
                argument,
                actor,
                context,
            } => {
                self.invoke_published_action(
                    id,
                    invocation,
                    resource,
                    action,
                    argument.as_deref(),
                    &actor,
                    context.as_deref(),
                );
            }
            CollabEvent::InvocationCancellation { id } => {
                self.request_invocation_cancellation(id);
            }
            CollabEvent::RemoteAction { message } => self.apply_remote_action_answer(message),
            CollabEvent::KeyAccess { route, state } => {
                if self.collaboration.manager.requested_primary_route() == Some(&route) {
                    self.collaboration.key_access = Some(state);
                }
            }
            // The generic client responses are consumed by widgets such as
            // Agent through their collaboration event hook; an ordinary
            // application has no local state to apply.
            CollabEvent::InstalledApplicationLaunchResult { .. }
            | CollabEvent::ActionCatalogue { .. }
            | CollabEvent::NativeMediaCommandResult { .. }
            | CollabEvent::WorkspaceControl { .. }
            | CollabEvent::RealtimeCommandResult { .. }
            | CollabEvent::ResourceCommandResult { .. }
            | CollabEvent::ResourcePage { .. }
            | CollabEvent::ActionVerdicts { .. }
            | CollabEvent::NearbyTransferCommandResult { .. }
            | CollabEvent::NearbyTransferChanged
            | CollabEvent::SpeakersChanged
            | CollabEvent::DrivesChanged
            | CollabEvent::ApplicationsChanged
            | CollabEvent::LocalFileClipboardResult { .. }
            // A window that holds the workspace's documents applies each as it arrives from
            // the broker; only a window that reads the store, as a tilolang app's window does,
            // re-reads the records named.
            | CollabEvent::RecordsMoved { .. }
            | CollabEvent::Realtime(_) => {}
        }
    }

    fn apply_document_collab_event(
        &mut self,
        route: &crate::collaboration::RouteSpec,
        mutation: collab_types::DocumentMutation,
    ) {
        tracing::debug!(
            target: "tilo.state",
            event = "document_receive",
            bytes = mutation.data().len(),
            app = %route.app_id,
            "applying document mutation"
        );
        self.apply_routed_document(route, mutation);
    }

    /// Route an answer from the session action host to the local request
    /// waiting for it. Answers nobody asked for are dropped.
    fn apply_remote_action_answer(&mut self, message: tilo_actions::remote::RemoteActionMessage) {
        use tilo_actions::remote::{ReceiptStatus, RemoteActionMessage};
        let (reply, result) = match message {
            RemoteActionMessage::Receipt(receipt) => {
                let invocation = receipt.invocation;
                let update = match receipt.status {
                    ReceiptStatus::Accepted => {
                        Ok(tilo_actions::InvocationUpdate::Accepted { invocation })
                    }
                    ReceiptStatus::Completed { output } => {
                        Ok(tilo_actions::InvocationUpdate::Succeeded { invocation, output })
                    }
                    ReceiptStatus::Failed { error } => {
                        Ok(tilo_actions::InvocationUpdate::Failed { invocation, error })
                    }
                    ReceiptStatus::Cancelled { message } => {
                        Ok(tilo_actions::InvocationUpdate::Cancelled {
                            invocation,
                            message,
                        })
                    }
                    ReceiptStatus::Rejected { error } => {
                        #[cfg(any(test, feature = "test-support"))]
                        {
                            self.actions.refused_broker_receipt = Some(error.to_string());
                        }
                        Err(error.to_string())
                    }
                };
                (BrokerReply::Receipt(invocation), update)
            }
            RemoteActionMessage::Update {
                invocation, state, ..
            } => (
                BrokerReply::Update(invocation),
                state
                    .map(|state| state.update)
                    .ok_or_else(|| "unknown invocation id".to_string()),
            ),
            _ => return,
        };
        let Some(request_id) = self.actions.broker_requests.remove(&reply) else {
            return;
        };
        let invocation = match reply {
            BrokerReply::Receipt(invocation) | BrokerReply::Update(invocation) => invocation,
        };
        self.apply_published_invocation_result(
            request_id,
            result.map(|update| (invocation, update)),
        );
    }

    fn apply_published_invocation_result(
        &mut self,
        request_id: u64,
        result: Result<(tilo_actions::InvocationId, tilo_actions::InvocationUpdate), String>,
    ) {
        if self.settle_follow_up(request_id, &result)
            || self.settle_hosted(request_id, &result)
            || self.close_broker_result(request_id, &result)
        {
            return;
        }
        let Some(invocation) = self
            .actions
            .pending_application_launches
            .remove(&request_id)
        else {
            self.settle_own_refusal(request_id, &result);
            return;
        };
        let Some(pending) = self.actions.directed_invocations.remove(&invocation) else {
            return;
        };
        let update = match result {
            Ok((_id, tilo_actions::InvocationUpdate::Succeeded { output, .. })) => {
                tilo_actions::InvocationUpdate::Succeeded { invocation, output }
            }
            Ok((_id, tilo_actions::InvocationUpdate::Failed { error, .. })) => {
                tilo_actions::InvocationUpdate::Failed { invocation, error }
            }
            Ok((_id, tilo_actions::InvocationUpdate::Cancelled { message, .. })) => {
                tilo_actions::InvocationUpdate::Cancelled {
                    invocation,
                    message,
                }
            }
            Ok((_id, other)) => tilo_actions::InvocationUpdate::Failed {
                invocation,
                error: tilo_actions::ActionFailure {
                    code: "application_launch_invalid_receipt".into(),
                    message: format!("application launch returned non-terminal {other:?}"),
                    details: None,
                    retryable: false,
                },
            },
            Err(message) => tilo_actions::InvocationUpdate::Failed {
                invocation,
                error: tilo_actions::ActionFailure {
                    code: "application_launch_failed".into(),
                    message,
                    details: None,
                    retryable: false,
                },
            },
        };
        self.collaboration
            .manager
            .send(CollabRequest::UpdateInvocation {
                id: pending.wire_id,
                update,
            });
    }

    /// The self row carries the address others use for this member (so
    /// a locally-derived id is recognised in a workspace-keyed roster) and the
    /// host-assigned name; it never enters the roster itself. The records this window runs read
    /// every member by the name the list gives them ([`tilo_resource::members`]).
    fn apply_collab_context(&mut self, members: Vec<crate::collaboration::Member>) {
        tilo_resource::members::know(
            members
                .iter()
                .map(|member| (member.id.clone(), member.name.clone())),
        );
        self.presence_ctx.apply_members(members);
    }

    /// Show what the connection of an app that runs elsewhere heard of the people at it, as what
    /// this window's own connection hears is shown: the member list names them, and a step of
    /// an agent's action outlines the control it stands for.
    pub(crate) fn apply_heard_elsewhere(&mut self) -> bool {
        use crate::component_lifecycle::Heard;
        let heard = crate::component_lifecycle::take_heard(self.root_widget.as_mut());
        let any = !heard.is_empty();
        for heard in heard {
            match heard {
                Heard::Members(members) => self.apply_collab_context(members),
                // The app's connection heard it on the app's own resources: it is this window's.
                Heard::Interaction(interaction) => {
                    if self.presence_ctx.apply_interaction(interaction.clone()) == Ok(true) {
                        self.announce_interaction(&interaction);
                    }
                }
            }
        }
        any
    }

    /// Bypasses the collab manager's socket/poll path — for tests using a
    /// `MemoryRelay` — through the routing the window's loop applies every
    /// event with ([`Self::apply_collab_presence_event`]).
    #[doc(hidden)]
    pub fn apply_collab_event_for_test(&mut self, event: CollabEvent) {
        let _entry = self.enter_wake_scope();
        let action_surface_may_have_changed = Self::collab_event_may_change_action_surface(&event);
        self.forward_session_notification(&event);
        self.apply_collab_presence_event(event);
        self.invalidate_widget_resource_cache();
        if action_surface_may_have_changed {
            self.publish_action_surface();
        }
    }

    pub fn local_author(&self) -> &crate::permissions::MemberId {
        &self.collaboration.local_author
    }
}

#[cfg(test)]
#[path = "cold_construction_tests.rs"]
mod cold_construction_tests;

#[cfg(test)]
#[path = "person_colour_tests.rs"]
mod person_colour_tests;
