// Collaboration transport, presence, permissions, and document integration.

impl App {
    /// Prepare and mount an application on its already admitted collaboration manager.
    /// Host preparation runs inside the existing native read custody. The original
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
            crate::application::HostContext,
            crate::application::LaunchTarget,
            crate::application::RestoreInput,
        )
            -> Result<crate::application::AppComponent, crate::launch::LaunchError>,
    ) -> Result<Self, crate::launch::LaunchError> {
        use crate::collaboration::native_initial::{self, NativeConstruction};
        use crate::launch::{LaunchError, LaunchRecovery};
        let scope = crate::wake::WakeScope::new();
        let _entry = scope.enter();
        let construction = NativeConstruction::enter_component(collaboration, scope)
            .map_err(LaunchError::Operation)?;
        let prepared = prepare_host()?;
        let binding = prepared.context.callback_binding();
        if !matches!(
            prepared.target,
            crate::application::LaunchTarget::Record { .. }
        ) {
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
    /// Called once per event-loop wakeup, so it must be cheap and
    /// non-blocking.
    pub fn set_frame_tick<F>(&mut self, tick: F)
    where
        F: FnMut(&mut PresenceCtx) -> bool + Send + Sync + 'static,
    {
        self.presence.frame_tick = Some(Box::new(tick));
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
            None => PresenceKey::local_surface_pointer(),
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

    pub fn set_followed_peer(&mut self, user_id: Option<String>) {
        self.presence.followed_peer = user_id;
    }

    pub fn followed_peer(&self) -> Option<&str> {
        self.presence.followed_peer.as_deref()
    }

    pub fn followed_peer_target(&self) -> Option<[f32; 2]> {
        let id = self.presence.followed_peer.as_deref()?;
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

    /// Publish the collaboration rail's current status through the real room
    /// presence path. `None` is an explicit clear. Only workspace keys are
    /// accepted, preventing an intention from being accidentally scoped to a
    /// single widget and disappearing as the participant navigates.
    pub fn broadcast_status(
        &mut self,
        workspace: crate::presence::PresenceKey,
        status: Option<&crate::live_collaboration::LiveStatus>,
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
                        crate::live_collaboration::StatusOrigin::Inferred { .. }
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
        self.restore_failed_document_publications();
        if self.collaboration.manager.is_connected() {
            match self.pending_primary_document_delta() {
                Ok((delta, heads)) if !delta.is_empty() => {
                    tracing::debug!(
                        target: "tilo.state",
                        event = "document_send",
                        kind = "incremental",
                        bytes = delta.len(),
                        heads = heads.len(),
                        "queueing document mutation"
                    );
                    let result = self.collaboration.manager.try_send(
                        crate::collaboration::CollabRequest::document_incremental(delta),
                    );
                    if self.note_document_send(&result) {
                        self.advance_primary_document(heads);
                    }
                }
                Ok(_) => {}
                Err(error) => {
                    tracing::warn!(%error, "collab: primary document is unavailable");
                }
            }
        }
        self.broadcast_room_index_delta();
    }

    /// The room index is a distinct document from the live record.  Keep its
    /// incremental cursor and route separate so an annotation/document delta
    /// can never be merged into the index (or vice versa).
    fn broadcast_room_index_delta(&mut self) {
        self.restore_failed_document_publications();
        let connected = self.collaboration.room_manager.as_ref().map_or_else(
            || self.persistence.records.is_some() && self.collaboration.manager.is_connected(),
            CollaborationManager::is_connected,
        );
        if !connected {
            return;
        }
        let Some(room) = self.persistence.room_index.as_mut() else {
            return;
        };
        let (delta, heads) = room.pending_outbound_delta();
        if delta.is_empty() {
            return;
        }
        let request = crate::collaboration::CollabRequest::document_incremental(delta);
        let result = if let Some(route) = self.native_room_route.clone() {
            self.collaboration.manager.try_send_to_route(route, request)
        } else if let Some(manager) = self.collaboration.room_manager.as_mut() {
            if !manager.is_connected() {
                return;
            }
            manager.try_send(request)
        } else if self.persistence.records.is_some() && self.collaboration.manager.is_connected() {
            // Before the first record checkout the primary manager is the
            // room route. It becomes `collaboration.room_manager` at that checkout.
            // Do not advance this cursor for ordinary (non-record) apps.
            self.collaboration.manager.try_send(request)
        } else {
            return;
        };
        if result.is_ok() {
            room.advance_outbound(heads);
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
        let subset = crate::view::ViewSubset::from_view(&view);
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
    pub fn follow(&mut self, peer_id: impl Into<String>) {
        let _entry = self.enter_wake_scope();
        self.presence.followed_peer = Some(peer_id.into());
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
        let Some(view) = self
            .presence_ctx
            .peer_view(&crate::permissions::UserId(id))
            .cloned()
        else {
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
        self.pump_room_index_events();
        let events = self.collaboration.manager.poll();
        for event in events {
            self.apply_collab_event_for_test(event);
        }
        self.collaboration.manager.finish_event_batch();
        self.drain_local_input_capture();
    }

    /// Apply a document envelope received on the unqualified room route.
    /// The broker authenticated the route; discovery still validates and
    /// bounds its names before exposing them to the app.
    fn apply_room_index_document_envelope(
        &mut self,
        route: &crate::collaboration::RouteSpec,
        envelope: &collab_types::SignedOp,
        decrypted_payload: Option<&[u8]>,
    ) -> bool {
        let Some(payload) = crate::app::document_routes::validated_route_payload_with(
            route,
            envelope,
            decrypted_payload,
        ) else {
            tracing::warn!("collab: broker delivered a room-index op outside its joined route");
            return false;
        };
        let mutation = match crate::app::document_routes::decode_routed_mutation(
            route,
            payload,
            decrypted_payload,
        ) {
            Ok(mutation) => mutation,
            Err(error) => {
                tracing::warn!(%error, "collab: broker delivered an invalid room-index mutation");
                return false;
            }
        };
        let Some(room) = self.persistence.room_index.as_mut() else {
            return false;
        };
        let applied = match mutation {
            collab_types::DocumentMutation::Snapshot(data) => room.merge_snapshot(&data).is_ok(),
            collab_types::DocumentMutation::Incremental(data) => {
                room.load_incremental(&data).is_ok()
            }
        };
        if applied
            && let (Some(records), Some(room)) = (
                self.persistence.records.as_mut(),
                self.persistence.room_index.as_ref(),
            )
        {
            records.discover_room(room.document());
            self.invalidate_widget_resource_cache();
        }
        applied
    }

    /// Drain only document traffic on the room-index route. Actions and
    /// presence are record scoped, so an index update cannot alter the app's
    /// resource/action namespace or presentation state.
    fn pump_room_index_events(&mut self) -> bool {
        let Some(manager) = self.collaboration.room_manager.as_mut() else {
            return false;
        };
        let Some(route) = manager.primary_route().map(|witness| witness.route.clone()) else {
            return false;
        };
        let events = manager.poll();
        let mut any = false;
        for event in events {
            match event {
                CollabEvent::CommittedDocument { route, read } => {
                    self.queue_committed_document(route, read)
                }
                CollabEvent::Document {
                    envelope,
                    decrypted_payload,
                    ..
                } => {
                    any |= self.apply_room_index_document_envelope(
                        &route,
                        &envelope,
                        decrypted_payload.as_deref(),
                    );
                }
                CollabEvent::ScrollbackBatch {
                    ops,
                    decrypted_payloads,
                    ..
                } => {
                    for (index, envelope) in ops.into_iter().enumerate() {
                        any |= self.apply_room_index_document_envelope(
                            &route,
                            &envelope,
                            decrypted_payloads
                                .get(index)
                                .and_then(|payload| payload.as_deref()),
                        );
                    }
                }
                CollabEvent::DocumentSnapshotRequested { .. } => {
                    if let (Some(manager), Some(room)) = (
                        self.collaboration.room_manager.as_mut(),
                        self.persistence.room_index.as_mut(),
                    ) {
                        let snapshot = room.document_mut().save();
                        if let Err(error) = manager.try_send(
                            crate::collaboration::CollabRequest::document_snapshot(snapshot),
                        ) {
                            tracing::warn!(%error, "collab: room-index snapshot not sent");
                        }
                    }
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

    fn pump_collab_presence_inner(
        &mut self,
        model_refresh: bool,
    ) -> (
        bool,
        Option<action_directory_snapshot::ModelDirectoryCandidate>,
    ) {
        let _entry = self.enter_wake_scope();
        let mut any = self.pump_room_index_events();
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
        for request in crate::widget::take_collab_requests(self.root_widget.as_mut()) {
            self.collaboration.manager.send(request);
        }
        self.pump_record_content();
        let (drained, action_surface_may_have_changed) = self.drain_collab_presence_events();
        any |= drained;
        any |= self.observe_collaboration_state();
        if any {
            self.invalidate_widget_resource_cache();
        }
        let action_surface_needs_publication = self.collaboration.manager.is_connected()
            && (self.actions.last_published_actions.is_none()
                || self.actions.action_directory_snapshot.is_none());
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
            // The same pump already reconciled custody for its document drain.
            self.publish_action_surface_after_refresh();
        }
        // Before-action diagnostics use the existing launcher-owned startup
        // checkpoint, never a published resource or a document mutation.
        let diagnostics = &crate::host::environment().diagnostics;
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

    /// What this device can do with the document's end-to-end encrypted
    /// channel: while it is waiting for access (a new device before its
    /// welcome arrives) or was removed, nothing is read or written and the
    /// app says so instead of showing stale or undecryptable content. `None`
    /// until the route reports.
    pub fn channel_access(&self) -> Option<crate::collaboration::ChannelAccessState> {
        self.collaboration.channel_access
    }

    /// React to the managers' connection changes. Work that only runs while
    /// connected is level-triggered, so a manager that has just become ready
    /// publishes the current state once instead of waiting for the next
    /// unrelated change. Returns whether anything changed.
    fn observe_collaboration_state(&mut self) -> bool {
        let room_ready = self
            .collaboration
            .room_manager
            .as_mut()
            .and_then(CollaborationManager::take_state_change)
            .is_some_and(|state| state.is_ready());
        if room_ready {
            self.broadcast_room_index_delta();
        }
        let Some(state) = self.collaboration.manager.take_state_change() else {
            return room_ready;
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
            self.request_checkout_snapshot();
            self.report_handoff(
                &crate::collaboration::handoff::HandoffReport::OwnerSubscribe { version: 1 },
            );
            self.publish_continuation_metadata();
            self.publish_playback_handoff();
            self.publish_search_index();
            self.broadcast_doc_delta();
            self.publish_action_surface();
        }
        true
    }

    /// Ask a freshly checked-out record route for its document, once the
    /// route can carry the request.
    pub(super) fn request_checkout_snapshot(&mut self) {
        if self.collaboration.snapshot_on_ready
            && self
                .collaboration
                .manager
                .send(CollabRequest::RequestDocumentSnapshot)
        {
            self.collaboration.snapshot_on_ready = false;
        }
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

    /// Send what the window's record content store queued on the record's
    /// route: content it lacks, and content it stored for the record's
    /// other readers. See [`crate::collaboration::record_content`].
    fn pump_record_content(&mut self) {
        use crate::collaboration::record_content::ContentLane;
        if !self.collaboration.manager.is_connected() {
            return;
        }
        let Some(route) = self
            .collaboration
            .manager
            .primary_route()
            .map(|witness| witness.route.clone())
        else {
            return;
        };
        let own = ContentLane::of(&route.app_id, route.record.as_deref().unwrap_or(""));
        let lanes = std::iter::once((None, own)).chain(
            ContentLane::routed(&route.app_id)
                .into_iter()
                .map(|(route, lane)| (Some(route), lane)),
        );
        for (route, lane) in lanes {
            {
                let _entry = self.enter_wake_scope();
                lane.attach(crate::WakeHandle::new());
            }
            self.send_lane_requests(route.as_ref(), &lane);
        }
    }

    /// Send what `lane` queued: on the window's own route, or on `route`
    /// for a record that is not the window's.
    fn send_lane_requests(
        &mut self,
        route: Option<&crate::collaboration::RouteSpec>,
        lane: &crate::collaboration::record_content::ContentLane,
    ) {
        let mut send = |request: CollabRequest| match route {
            Some(route) => self
                .collaboration
                .manager
                .try_send_to_route(route.clone(), request),
            None => self.collaboration.manager.try_send(request),
        };
        let requests = lane.take_requests();
        for hash in requests.wants {
            if let Err(error) = send(CollabRequest::WantContent { hash: hash.clone() }) {
                tracing::debug!(%error, "collab: record content request deferred");
                lane.retry_want(hash);
            }
        }
        for (hash, bytes) in requests.offers {
            match send(CollabRequest::OfferContent {
                hash: hash.clone(),
                bytes: bytes.clone(),
            }) {
                Ok(()) | Err(crate::collaboration::CollabSendError::TooLarge { .. }) => {}
                Err(_) => lane.retry_offer(hash, bytes),
            }
        }
    }

    fn collab_event_updates_app_presentation(event: &CollabEvent) -> bool {
        !matches!(
            event,
            CollabEvent::SelectedDiscoveryResult { .. }
                | CollabEvent::OpenSelectedV2Result { .. }
                | CollabEvent::OpenSelectedResult { .. }
                | CollabEvent::NearbyTransferCommandResult { .. }
                | CollabEvent::NearbyTransferChanged
                | CollabEvent::ActionCatalogue { .. }
                | CollabEvent::ActionDirectoryPage { .. }
                | CollabEvent::PublishedActionResolution { .. }
                | CollabEvent::NativeMediaCommandResult { .. }
                | CollabEvent::TaskRun(_)
                | CollabEvent::TaskActionInvocationResult { .. }
                | CollabEvent::TaskRunCancellation { .. }
                | CollabEvent::TaskRunControl { .. }
                | CollabEvent::TaskRunReportResult { .. }
                | CollabEvent::WorkspaceControl { .. }
                | CollabEvent::RealtimeCommandResult { .. }
                | CollabEvent::Realtime(_)
        )
    }
    fn apply_collab_presence_event(&mut self, event: CollabEvent) {
        let _background = crate::local_document_action::suspend();
        match event {
            CollabEvent::LocalDocumentActionReported { request_id, result } => {
                self.observe_local_input_capture(request_id, result);
            }
            CollabEvent::Context { collaborators, .. } => {
                self.apply_collab_context(collaborators);
            }
            CollabEvent::HistoryAgents { cluster_id, agents } => {
                crate::history_source::set_room_agents(&cluster_id, agents);
            }
            CollabEvent::RecordsArrived { records } => self.records_arrived(&records),
            CollabEvent::FollowChanged { peer_id } => match peer_id {
                Some(peer_id) => self.follow(peer_id),
                None => self.unfollow(),
            },
            CollabEvent::DocumentSnapshotRequested { route } => {
                self.respond_document_snapshot(&route);
            }
            CollabEvent::RecordContent { route, hash, bytes } => {
                crate::collaboration::record_content::ContentLane::of(
                    &route.app_id,
                    route.record.as_deref().unwrap_or(""),
                )
                .arrive(hash, bytes);
            }
            CollabEvent::RemoteCursorMoved { user_id, x, y } => {
                self.presence_ctx
                    .upsert_remote_cursor(user_id, [x, y], None);
            }
            CollabEvent::RemotePresenceUpdate {
                user_id,
                key,
                payload,
            } => {
                self.apply_remote_presence(user_id, key, payload);
            }
            CollabEvent::ActionArrived {
                type_id,
                payload,
                from_user_id,
            } => {
                self.apply_collab_action_arrived(&type_id, &payload, &from_user_id);
            }
            event @ (CollabEvent::Document { .. } | CollabEvent::ScrollbackBatch { .. }) => {
                self.apply_document_collab_event(event);
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
            CollabEvent::PublishedInvocationResult {
                request_id, result, ..
            } => {
                let result = result.map(|(_id, update)| (update.invocation(), update));
                self.apply_published_invocation_result(request_id, result);
            }
            #[cfg(unix)]
            CollabEvent::ManagedInvocationObservation { .. } => {}
            CollabEvent::ChannelAccess { route, state } => {
                if self.collaboration.manager.requested_primary_route() == Some(&route) {
                    self.collaboration.channel_access = Some(state);
                }
            }
            // The generic client responses are consumed by widgets such as
            // Agent through their collaboration event hook; an ordinary
            // application has no local state to apply.
            CollabEvent::InstalledApplicationLaunchResult { .. }
            | CollabEvent::ActionCatalogue { .. }
            | CollabEvent::ActionDirectoryPage { .. }
            | CollabEvent::PublishedActionResolution { .. }
            | CollabEvent::NativeMediaCommandResult { .. }
            | CollabEvent::TaskRun(_)
            | CollabEvent::TaskActionInvocationResult { .. }
            | CollabEvent::TaskRunCancellation { .. }
            | CollabEvent::TaskRunControl { .. }
            | CollabEvent::TaskRunReportResult { .. }
            | CollabEvent::WorkspaceControl { .. }
            | CollabEvent::RealtimeCommandResult { .. }
            | CollabEvent::NearbyTransferCommandResult { .. }
            | CollabEvent::SelectedDiscoveryResult { .. }
            | CollabEvent::OpenSelectedV2Result { .. }
            | CollabEvent::OpenSelectedResult { .. }
            | CollabEvent::NearbyTransferChanged
            | CollabEvent::LocalFileClipboardResult { .. }
            | CollabEvent::Realtime(_) => {}
        }
    }

    fn apply_document_collab_event(&mut self, event: CollabEvent) {
        match event {
            CollabEvent::Document {
                envelope,
                decrypted_payload,
                ..
            } => {
                // The broker already validated the envelope (signature +
                // role table + replay defence); the toolkit trusts it.
                tracing::debug!(
                    target: "tilo.state",
                    event = "document_receive",
                    bytes = envelope.payload.len(),
                    author = ?envelope.author,
                    sequence = envelope.seq,
                    encrypted = envelope.encryption.is_some(),
                    "applying document mutation"
                );
                self.collaboration
                    .history
                    .observe_live(envelope.artifact, envelope.seq);
                self.apply_signed_document_envelope(&envelope, decrypted_payload.as_deref());
            }
            CollabEvent::ScrollbackBatch {
                artifact,
                ops,
                decrypted_payloads,
                oldest_seq,
                exhausted,
            } => {
                self.continue_document_history(artifact, oldest_seq, exhausted);
                // Broker pre-validated each envelope, trusted the same
                // way as a live document mutation.
                tracing::debug!(
                    target: "tilo.state",
                    event = "document_scrollback",
                    operations = ops.len(),
                    "applying document scrollback"
                );
                self.apply_document_scrollback(ops, &decrypted_payloads);
            }
            _ => {}
        }
    }

    fn continue_document_history(
        &mut self,
        artifact: collab_types::ArtifactId,
        oldest_seq: Option<u64>,
        exhausted: bool,
    ) {
        if let Some(request) = self
            .collaboration
            .history
            .on_batch(artifact, oldest_seq, exhausted)
        {
            self.send_history_request(request);
        }
    }

    fn send_history_request(&mut self, request: crate::collaboration::CollabRequest) {
        if let Err(error) = self.collaboration.manager.try_send(request) {
            tracing::debug!(
                ?error,
                "collab: history page request not sent; retried on next trigger"
            );
            self.collaboration.history.reset_in_flight();
        }
    }

    /// Answer a request for the whole document on `route`: a record kept
    /// beside the shown one ([`Widget::record_owners`](crate::widget::Widget::record_owners))
    /// is published whole on its own route with the next publication; any
    /// other request is the primary document's.
    fn respond_document_snapshot(&mut self, route: &crate::collaboration::RouteSpec) {
        if let Some(entry) = self
            .document_routes
            .owners
            .iter_mut()
            .find(|entry| entry.answers_snapshots && entry.route == *route)
        {
            entry.needs_snapshot = true;
            return;
        }
        self.respond_primary_document_snapshot();
    }

    fn respond_primary_document_snapshot(&mut self) {
        match self.primary_document_snapshot() {
            Ok(data) => {
                let result = self
                    .collaboration
                    .manager
                    .try_send(crate::collaboration::CollabRequest::document_snapshot(data));
                if let Err(error) = &result {
                    // The requester keeps asking while no holder answers, so
                    // a busy transport delays onboarding rather than losing it.
                    tracing::warn!(%error, "collab: primary snapshot not sent");
                }
                self.note_document_send(&result);
            }
            Err(error) => {
                tracing::warn!(%error, "collab: primary snapshot refused");
            }
        }
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
            RemoteActionMessage::Update { invocation, state } => (
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
            || self.close_broker_result(request_id, &result)
        {
            return;
        }
        let Some(invocation) = self
            .actions
            .pending_application_launches
            .remove(&request_id)
        else {
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

    /// The self row carries the address others use for this participant (so
    /// a locally-derived id is recognised in a room-keyed roster) and the
    /// host-assigned name; it never enters the roster itself.
    fn apply_collab_context(&mut self, collaborators: Vec<crate::collaboration::Collaborator>) {
        let current = collaborators
            .iter()
            .map(|participant| crate::permissions::UserId(participant.id.clone()))
            .collect::<std::collections::HashSet<_>>();
        for departed in self.collaboration.context_participants.difference(&current) {
            self.presence_ctx.unregister_peer(&departed.0);
        }
        for participant in &collaborators {
            self.presence_ctx.set_participant_kind(
                crate::permissions::UserId(participant.id.clone()),
                participant.is_agent,
            );
            self.presence_ctx.set_joined_at(
                crate::permissions::UserId(participant.id.clone()),
                participant.joined_at,
            );
        }
        if let Some(me) = collaborators.iter().find(|c| c.is_self) {
            self.presence_ctx
                .set_self_participant(crate::permissions::UserId(me.id.clone()), me.name.clone());
        } else {
            self.presence_ctx.clear_self_participant();
        }
        // Filtered to others: letting the self row through would put the
        // local user in every list built from presence.
        for c in collaborators.into_iter().filter(|c| !c.is_self) {
            // Relabel only — the roster doesn't say where a peer's pointer
            // is, so it must not mint one at the origin.
            self.presence_ctx
                .set_display_name(crate::permissions::UserId(c.id.clone()), c.name.clone());
            // The colour they wear everywhere, once the room has heard it.
            if let Some(hue) = c.colour {
                self.presence_ctx
                    .set_presence_hue(crate::permissions::UserId(c.id.clone()), hue);
            }
            self.presence_ctx.set_participant_kind(
                crate::permissions::UserId(c.id),
                if c.is_agent {
                    crate::presence::ParticipantKind::Agent
                } else {
                    crate::presence::ParticipantKind::Person
                },
            );
        }
        self.collaboration.context_participants = current;
    }

    /// Every app in the room is handed every action; the one whose registry
    /// knows the type applies it, so the owner stays the only writer.
    /// `before` MUST be read ahead of apply: a baseline left behind the
    /// incoming change means the user's next Ctrl+Z reverts someone else's
    /// edit along with their own keystroke.
    fn apply_collab_action_arrived(&mut self, type_id: &str, payload: &[u8], from_user_id: &str) {
        let before = self.state.doc.get_heads();
        let applied = self.root_widget.apply_room_action(
            &mut crate::widget::Edits::lend(&mut self.state.doc),
            type_id,
            payload,
        );
        if applied {
            self.invalidate_widget_resource_cache();
            // Named after the publisher so local Ctrl+Z walks past it
            // instead of through it.
            self.state.record_committed_action_authored(
                before,
                None,
                None,
                Some(crate::permissions::UserId(from_user_id.to_string())),
            );
            self.needs_sync = true;
            self.layout.request_rebuild();
        }
    }

    /// Bypasses the collab manager's socket/poll path — for tests using a
    /// `MemoryRelay`.
    #[doc(hidden)]
    // This test adapter deliberately mirrors the complete collaboration event
    // match so lifecycle tests exercise the same routing as the runtime loop.
    #[allow(
        clippy::too_many_lines,
        reason = "test adapter mirrors complete collaboration event routing for lifecycle coverage"
    )]
    pub fn apply_collab_event_for_test(&mut self, event: CollabEvent) {
        let _background = crate::local_document_action::suspend();
        let _entry = self.enter_wake_scope();
        let action_surface_may_have_changed = Self::collab_event_may_change_action_surface(&event);
        self.forward_session_notification(&event);
        match event {
            CollabEvent::LocalDocumentActionReported { request_id, result } => {
                self.observe_local_input_capture(request_id, result);
            }
            CollabEvent::CommittedDocument { route, read } => {
                self.queue_committed_document(route, read)
            }
            CollabEvent::RemotePresenceUpdate {
                user_id,
                key,
                payload,
            } => {
                self.apply_remote_presence(user_id, key, payload);
            }
            CollabEvent::RemoteCursorMoved { user_id, x, y } => {
                self.presence_ctx
                    .upsert_remote_cursor(user_id, [x, y], None);
            }
            CollabEvent::Document {
                envelope,
                decrypted_payload,
                ..
            } => {
                self.collaboration
                    .history
                    .observe_live(envelope.artifact, envelope.seq);
                self.apply_signed_document_envelope(&envelope, decrypted_payload.as_deref());
                self.observe_primary_retirement();
            }
            CollabEvent::ScrollbackBatch {
                artifact,
                ops,
                decrypted_payloads,
                oldest_seq,
                exhausted,
            } => {
                self.continue_document_history(artifact, oldest_seq, exhausted);
                self.apply_document_scrollback(ops, &decrypted_payloads);
            }
            CollabEvent::FollowChanged { peer_id } => match peer_id {
                Some(peer_id) => self.follow(peer_id),
                None => self.unfollow(),
            },
            CollabEvent::Handoff { request } => self.apply_handoff(request),
            CollabEvent::DocumentSnapshotRequested { route } => {
                self.respond_document_snapshot(&route);
            }
            CollabEvent::Context { collaborators, .. } => {
                self.apply_collab_context(collaborators);
            }
            CollabEvent::HistoryAgents { cluster_id, agents } => {
                crate::history_source::set_room_agents(&cluster_id, agents);
            }
            CollabEvent::RecordsArrived { records } => self.records_arrived(&records),
            CollabEvent::Invocation {
                invocation,
                id,
                resource,
                action,
                argument,
                actor,
                context,
            } => self.invoke_published_action(
                id,
                invocation,
                resource,
                action,
                argument.as_deref(),
                &actor,
                context.as_deref(),
            ),
            CollabEvent::InvocationCancellation { id } => {
                self.request_invocation_cancellation(id);
            }
            CollabEvent::RemoteAction { message } => self.apply_remote_action_answer(message),
            _ => {}
        }
        self.invalidate_widget_resource_cache();
        if action_surface_may_have_changed {
            self.publish_action_surface();
        }
    }

    pub fn local_author(&self) -> &crate::permissions::UserId {
        &self.collaboration.local_author
    }
}

#[cfg(test)]
#[path = "cold_construction_tests.rs"]
mod cold_construction_tests;
