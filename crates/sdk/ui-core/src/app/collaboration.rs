// Collaboration transport, presence, permissions, and document integration.

impl App {
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

    /// The device key this app's collaboration signs with, once its
    /// connection has its identity: which device a participant runs on.
    pub fn collaboration_device(&self) -> Option<[u8; 32]> {
        self.collaboration.manager.document_author()
    }
    pub fn presence_ctx_mut(&mut self) -> &mut PresenceCtx {
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

    /// Take the broker's word for what this app may do now.
    fn adopt_grant(&mut self, grant: &collab_types::ConnectionGrant) {
        let next = PermissionContext::from_grant(grant);
        if next != self.permission_ctx {
            self.set_permission_ctx(next);
            self.invalidate_widget_resource_cache();
        }
    }
    pub fn set_permission_ctx(&mut self, ctx: PermissionContext) {
        let _entry = self.enter_wake_scope();
        self.permission_ctx = ctx;
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
        self.root_widget.permissions_changed(&self.permission_ctx);
        self.needs_sync = true;
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
        // With records in play the live document travels only on a
        // record's route. Until one is checked out the primary route is the
        // room's, which carries the room index: what an app wrote before its
        // checkout goes out with the record, once checked out.
        let awaiting_checkout = self
            .persistence
            .records
            .as_ref()
            .is_some_and(|records| records.active().is_none());
        if self.collaboration.manager.is_connected() && !awaiting_checkout {
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
        if self.persistence.records.is_none() || !self.collaboration.manager.is_connected() {
            return;
        }
        let Some(room) = self.persistence.room_index.as_mut() else {
            return;
        };
        let (delta, heads) = room.pending_outbound_delta();
        if delta.is_empty() {
            return;
        }
        // Adding only this device's own comment references is the one index
        // change a Commenter may make; it travels `Comment`-class.
        let author = self.collaboration.manager.document_author();
        let request =
            if author.is_some_and(|author| room.pending_outbound_is_comment_references(&author)) {
                crate::collaboration::CollabRequest::CommentReferences { delta }
            } else {
                crate::collaboration::CollabRequest::document_incremental(delta)
            };
        // Before the first record checkout the primary route is the room
        // route; after it, the room route is joined beside the record's.
        let room_route = self
            .native_room_route
            .clone()
            .or_else(|| self.collaboration.room_route.clone());
        let result = match room_route {
            Some(route) => self.collaboration.manager.try_send_to_route(route, request),
            None => self.collaboration.manager.try_send(request),
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
            self.inspect_node(id, |widget| widget.presence_payload())
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
        let current = self.root_widget.presence_payload();
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
        self.refresh_overlay_identity();
        let view = <Self as crate::view::FreezeThaw>::capture_view(self);
        let mut subset = crate::view::ViewSubset::from_view(&view);
        subset.save_status = self.root_widget.record_status();
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
    ///
    /// This is a live follow: see [`crate::view::FollowMode`].
    pub fn follow(&mut self, peer_id: impl Into<String>) {
        self.follow_with(peer_id, crate::view::FollowMode::Live);
    }

    /// Follow `peer_id` in `mode`. A replay peer follows the recorded
    /// participant with [`FollowMode::Replay`](crate::view::FollowMode::Replay).
    pub fn follow_with(&mut self, peer_id: impl Into<String>, mode: crate::view::FollowMode) {
        let _entry = self.enter_wake_scope();
        let peer_id = peer_id.into();
        // A replay's viewer stands where the recorded participant stood:
        // their presence is this view, not another person in it. The caret
        // or selection they already had here becomes this view's own.
        self.presence.stood_at = None;
        self.presence.stood_missed_at = None;
        if mode == crate::view::FollowMode::Replay {
            let participant = crate::permissions::UserId(peer_id.clone());
            let caret = self.presence_ctx.carets.iter().find_map(|(key, held)| {
                held.iter()
                    .find(|(user, _)| *user == participant)
                    .map(|(_, pos)| {
                        (
                            key.clone(),
                            crate::collaboration::PresencePayload::Caret { pos: *pos },
                        )
                    })
            });
            let selection = self.presence_ctx.selections.iter().find_map(|(key, held)| {
                held.iter()
                    .find(|(user, _)| *user == participant)
                    .map(|(_, range)| {
                        (
                            key.clone(),
                            crate::collaboration::PresencePayload::Selection {
                                start: range.start,
                                end: range.end,
                            },
                        )
                    })
            });
            self.presence.stood_at = selection.or(caret);
        }
        self.presence_ctx.set_stand_in(
            (mode == crate::view::FollowMode::Replay)
                .then(|| crate::permissions::UserId(peer_id.clone())),
        );
        self.presence.followed_peer = Some(peer_id);
        self.presence.follow_mode = mode;
        self.apply_follow();
        // What the View names may only exist once the widgets sync to the
        // document that arrived with it.
        self.presence.follow_after_sync = true;
        self.needs_sync = true;
    }

    /// Stop following. Overlays shown only because the followed participant
    /// had them open close with it; they were never this app's own.
    pub fn unfollow(&mut self) {
        let _entry = self.enter_wake_scope();
        self.presence.followed_peer = None;
        self.presence.follow_mode = crate::view::FollowMode::Live;
        self.presence_ctx.set_stand_in(None);
        self.presence.stood_at = None;
        self.presence.stood_missed_at = None;
        for owner in std::mem::take(&mut self.presence.mirrored_overlays) {
            self.restore_overlay_at(&owner, &[], crate::view::OverlayPresentation::Owned);
        }
    }

    pub fn follow_mode(&self) -> crate::view::FollowMode {
        self.presence.follow_mode
    }

    /// A live follower's own input must never land on an overlay they were
    /// only watching.
    ///
    /// Whether that input ends the follow is not this app's decision. Follow
    /// belongs to the session: the shell owns it, releases it on the same
    /// input (the first key, which it consumes, and the first workspace click
    /// or wheel, which it forwards), and fans the release to every app of the
    /// room as `CollabEvent::FollowChanged { peer_id: None }` while
    /// republishing its `Following` presence, so chrome, presence and apps
    /// agree. The forwarded click can reach this app before that release
    /// does, so the mirrored overlays close here first; the follow itself
    /// ends when the shell says so. Replay has no such input: its viewer
    /// drives nothing.
    pub(super) fn yield_mirrored_overlays_to_local_input(&mut self, event: &WindowEvent) {
        if self.presence.followed_peer.is_none()
            || self.presence.follow_mode != crate::view::FollowMode::Live
            || self.presence.mirrored_overlays.is_empty()
        {
            return;
        }
        let takes_control = match event {
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                ..
            }
            | WindowEvent::MouseWheel { .. } => true,
            WindowEvent::KeyboardInput { event, .. } => event.state == ElementState::Pressed,
            WindowEvent::Touch(touch) => touch.phase == winit::event::TouchPhase::Started,
            _ => false,
        };
        if takes_control {
            for owner in std::mem::take(&mut self.presence.mirrored_overlays) {
                self.restore_overlay_at(&owner, &[], crate::view::OverlayPresentation::Owned);
            }
        }
    }

    /// Re-apply the followed View after the widgets synced to a document
    /// change that arrived while following.
    #[cfg(any(test, feature = "native"))]
    pub(super) fn follow_after_sync(&mut self) {
        if std::mem::take(&mut self.presence.follow_after_sync) {
            self.apply_follow();
        }
        // A replay's caret stands in text that may only now have synced.
        if self.presence.follow_mode == crate::view::FollowMode::Replay {
            self.retry_standing_where_followed();
        }
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
        self.root_widget
            .restore_view_state(&crate::view::ViewContribution {
                scroll: view.scroll.clone(),
                view_mode: view.view_mode,
                open_resource: None,
            });
        if let Some(resource) = view.open_resource.as_deref() {
            self.root_widget.follow_open_resource(resource);
        }
        // A replay shows the durability the recorded participant's app
        // showed, as it shows their overlays.
        if self.presence.follow_mode == crate::view::FollowMode::Replay {
            self.root_widget
                .follow_record_status(view.save_status.clone());
        }
        // A follower sees what the followed participant sees, including the
        // menu or dialog they have open; a replay is this same path. The mode
        // decides whether those overlays take control (see `FollowMode`).
        // Toasts arrive without their text and never replace the follower's.
        let presentation = self.presence.follow_mode.overlay_presentation();
        self.restore_overlays(&view.overlays, presentation);
        // An overlay can name what only exists once the widgets have synced
        // to what was just opened (a menu for the page the follower is now
        // on); try again after the next sync until it is shown.
        self.refresh_semantic_tree();
        let shown = self.accessibility.semantic_tree.open_overlays().len();
        if shown < view.overlays.len() {
            self.presence.follow_after_sync = true;
            self.needs_sync = true;
        }
        self.presence.mirrored_overlays =
            if presentation == crate::view::OverlayPresentation::Mirrored {
                view.overlays
                    .iter()
                    .map(|overlay| overlay.owner.clone())
                    .collect()
            } else {
                Vec::new()
            };
        self.layout.layout_dirty = true;
        self.needs_sync = true;
    }

    /// One turn of the work a live window does between input and paint —
    /// records, collaboration, the model's update and sync, queued controls,
    /// directed invocations and document publication — without a window.
    /// For tests that drive apps over a real broker. Returns whether anything
    /// changed.
    #[doc(hidden)]
    pub fn step_headless(&mut self) -> bool {
        let _entry = self.enter_wake_scope();
        let mut changed = self.record_tick();
        changed |= self.pump_collab_presence();
        if self
            .root_widget
            .update(&mut self.state.doc, &self.state.root)
        {
            self.needs_sync = true;
        }
        if self.needs_sync {
            self.sync_root();
            self.invalidate_widget_resource_cache();
            self.needs_sync = false;
            changed = true;
        }
        changed |= self.dispatch_control_invocations() > 0;
        changed |= self.poll_directed_invocations();
        self.broadcast_doc_delta();
        self.publish_owned_documents();
        self.publish_action_surface();
        changed
    }

    /// Lets integration tests drive the collab pump without a real winit
    /// event loop.
    #[doc(hidden)]
    pub fn poll_and_apply_collab_events(&mut self) {
        let _entry = self.enter_wake_scope();
        let events = self.collaboration.manager.poll();
        for event in events {
            self.apply_collab_event_for_test(event);
        }
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
        // A comment-class index change is admitted by brokers on class alone;
        // its shape is checked here, where the index is held.
        if envelope.op_class == collab_types::OpClass::Comment
            && !matches!(
                &mutation,
                collab_types::DocumentMutation::Incremental(data)
                    if crate::record::room::comment_references_only(
                        room.document(),
                        data,
                        envelope.author.as_bytes(),
                    )
            )
        {
            tracing::warn!(
                "collab: refused a comment-class room-index change that adds more than the author's own comment references"
            );
            return false;
        }
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

    /// Answer a late joiner's request for the room index's state on the
    /// room route. Returns whether `route` was the room route.
    pub(super) fn answer_room_index_snapshot(
        &mut self,
        route: &crate::collaboration::RouteSpec,
    ) -> bool {
        let room_route = self
            .native_room_route
            .as_ref()
            .or(self.collaboration.room_route.as_ref());
        if room_route != Some(route) {
            return false;
        }
        if let Some(room) = self.persistence.room_index.as_mut() {
            let snapshot = room.document_mut().save();
            if let Err(error) = self.collaboration.manager.try_send_to_route(
                route.clone(),
                crate::collaboration::CollabRequest::document_snapshot(snapshot),
            ) {
                tracing::warn!(%error, "collab: room-index snapshot not sent");
            }
        }
        true
    }

    /// Shared by the per-frame tick and the headless test path. Returns
    /// `true` iff at least one event was processed.
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
        let mut any = false;
        // Replies to broker pull-directory callbacks must run before regular
        // inbound events. This is the sole place capability requests acquire
        // widget/document authority, and it always runs on the App owner
        // thread.
        self.pump_action_directory_requests();
        self.sync_joined_records();
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
        let (drained, declarations_may_have_changed, action_surface_may_have_changed) =
            self.drain_collab_presence_events();
        let mut invalidate = declarations_may_have_changed;
        any |= drained;
        if drained {
            self.mark_semantics_dirty();
        }
        self.report_semantic_change();
        let collaboration_state_changed = self.observe_collaboration_state();
        any |= collaboration_state_changed;
        invalidate |= collaboration_state_changed;
        if invalidate {
            self.invalidate_widget_resource_cache();
        }
        let published_snapshot_was_evicted = self.collaboration.manager.is_connected()
            && self.actions.last_published_actions.is_some()
            && self.actions.action_directory_snapshot.is_none();
        if action_surface_may_have_changed || published_snapshot_was_evicted {
            // The normal idle pump also repairs invalidation from a later
            // model sync or a presence-only batch. Neither necessarily brings
            // another action event after a terminal operation was published.
            // Query/resolve remain bounded reads; this refresh runs even with
            // no directory requests or inbound events, and skips stable idle.
            self.publish_action_surface();
        }
        (any, directory)
    }

    /// Tell every widget's [`crate::record::JoinedRecords`] who this device
    /// is, bring the records they created into the room's index, and tell
    /// the widgets when that index changed. The index delta then leaves on
    /// the room route like any other; one made only of this device's own
    /// comment references is signed `Comment`.
    pub(super) fn sync_joined_records(&mut self) {
        fn visit(
            widget: &dyn crate::widget::Widget,
            author: Option<[u8; 32]>,
            references: &mut Vec<crate::record::RecordCheckout>,
        ) {
            if let Some(joined) = widget.joined_records() {
                // A device key does not change while an app runs; before
                // collaboration connects there is simply none to tell.
                if author.is_some() {
                    joined.set_comment_author(author);
                }
                references.extend(joined.take_references());
            }
            for child in widget.children() {
                visit(child, author, references);
            }
        }
        let author = self.comment_author();
        let mut references = Vec::new();
        visit(self.root_widget.as_ref(), author, &mut references);
        let Some(index) = self.persistence.room_index.as_mut() else {
            return;
        };
        for checkout in &references {
            crate::record::room::reference(index.document_mut(), &checkout.id, &checkout.kind);
        }
        let heads = index.document_mut().get_heads();
        if self.persistence.room_index_seen.as_ref() != Some(&heads) {
            self.persistence.room_index_seen = Some(heads);
            self.root_widget.room_index_changed(index.document());
            self.needs_sync = true;
        }
    }

    /// The device key this app signs document changes with, once connected:
    /// the owner to name in [`crate::record::RecordId::for_comment`]. A
    /// comment record owned by any other key is not this app's to write.
    pub fn comment_author(&self) -> Option<[u8; 32]> {
        self.collaboration.manager.document_author()
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
        let Some(state) = self.collaboration.manager.take_state_change() else {
            return false;
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
            self.publish_continuation_metadata();
            self.publish_playback_handoff();
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
        self.publish_checked_out_record();
    }

    /// A writer that checked a record out leaves the record's whole state
    /// in the room once the record's route can carry it. The checkout took
    /// the document as its baseline, so nothing it held before — content an
    /// app wrote before the checkout, or kept from an earlier session — would
    /// otherwise ever reach that route: a member on another device, or one
    /// who arrives while this writer is away, learns the record only from
    /// what the room's host retains.
    fn publish_checked_out_record(&mut self) {
        if !self.collaboration.publish_on_ready || !self.collaboration.manager.is_connected() {
            return;
        }
        match self.primary_document_snapshot() {
            Ok(data) => {
                let result = self
                    .collaboration
                    .manager
                    .try_send(CollabRequest::document_snapshot(data));
                if self.note_document_send(&result) {
                    self.collaboration.publish_on_ready = false;
                }
            }
            Err(error) => {
                tracing::warn!(%error, "collab: checked-out record not published");
                self.collaboration.publish_on_ready = false;
            }
        }
    }

    /// Applies every queued collaboration event. Returns whether any event
    /// arrived and whether one of them may have changed the action surface.
    fn drain_collab_presence_events(&mut self) -> (bool, bool, bool) {
        let events = self.collaboration.manager.poll();
        let any = !events.is_empty();
        let mut declarations_may_have_changed = false;
        let mut action_surface_may_have_changed = false;
        for event in events {
            declarations_may_have_changed |= Self::collab_event_may_change_declarations(&event);
            action_surface_may_have_changed |= Self::collab_event_may_change_action_surface(&event);
            declarations_may_have_changed |= self.forward_session_notification(&event);
            self.apply_collab_presence_event(event);
        }
        (
            any,
            declarations_may_have_changed,
            action_surface_may_have_changed,
        )
    }
    fn apply_collab_presence_event(&mut self, event: CollabEvent) {
        match event {
            CollabEvent::Context { collaborators } => {
                self.apply_collab_context(collaborators);
            }
            CollabEvent::Grant { grant } => {
                self.adopt_grant(&grant);
            }
            CollabEvent::FollowChanged { peer_id, mode } => match peer_id {
                Some(peer_id) => self.follow_with(peer_id, mode),
                None => self.unfollow(),
            },
            CollabEvent::DocumentSnapshotRequested { route } => {
                if !self.answer_room_index_snapshot(&route)
                    && !self.answer_owned_document_snapshot(&route)
                {
                    self.respond_primary_document_snapshot();
                }
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
            CollabEvent::Inspection { id, request } => self.answer_inspection(id, &request),
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
            CollabEvent::ActionCatalogue { .. }
            | CollabEvent::ActionDirectoryPage { .. }
            | CollabEvent::PublishedActionResolution { .. }
            | CollabEvent::EnrolledTask(_)
            | CollabEvent::EnrolledTaskReply { .. }
            | CollabEvent::EnrolledTaskDisconnected { .. }
            | CollabEvent::NativeMediaCommandResult { .. }
            | CollabEvent::TaskRun(_)
            | CollabEvent::TaskActionInvocationResult { .. }
            | CollabEvent::TaskRunCancellation { .. }
            | CollabEvent::WorkspaceControl { .. }
            | CollabEvent::RealtimeCommandResult { .. }
            | CollabEvent::NearbyTransferCommandResult { .. }
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

    /// A history view reports how many loaded ops of `artifact` lie above
    /// its viewport; older history is fetched as it nears the loaded edge.
    pub fn document_history_viewport(
        &mut self,
        artifact: collab_types::ArtifactId,
        loaded_ops_above_viewport: usize,
    ) {
        if let Some(request) = self
            .collaboration
            .history
            .on_viewport(artifact, loaded_ops_above_viewport)
        {
            self.send_history_request(request);
        }
    }

    /// Something needs `artifact`'s history back to `seq` (opening an older
    /// version, a backwards search); pages are fetched until it is loaded.
    pub fn load_document_history_back_to(&mut self, artifact: collab_types::ArtifactId, seq: u64) {
        if let Some(request) = self
            .collaboration
            .history
            .ensure_loaded_back_to(artifact, seq)
        {
            self.send_history_request(request);
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
        match &message {
            RemoteActionMessage::Receipt(receipt) => self.advance_human_interaction(
                receipt.invocation,
                tilo_actions::InteractionPhase::from_receipt(&receipt.status),
            ),
            RemoteActionMessage::Update {
                invocation,
                state: Some(state),
            } => self.advance_human_interaction(
                *invocation,
                tilo_actions::InteractionPhase::from_update(&state.update),
            ),
            _ => {}
        }
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
                    ReceiptStatus::Rejected { error } => Err(error.to_string()),
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
        if self.close_broker_result(request_id, &result) {
            return;
        }
        if let Some(source) = self.actions.workspace_request_sources.remove(&request_id) {
            let outcome = match &result {
                Ok((_, tilo_actions::InvocationUpdate::Succeeded { .. })) => Ok(()),
                Ok((_, tilo_actions::InvocationUpdate::Failed { error, .. })) => {
                    Err(error.message.clone())
                }
                Ok((_, tilo_actions::InvocationUpdate::Cancelled { .. })) => {
                    Err("The workspace cancelled the request".into())
                }
                Ok((_, other)) => Err(format!("The workspace answered {other:?}")),
                Err(message) => Err(message.clone()),
            };
            self.root_widget
                .on_workspace_request_finished(&source, &outcome);
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
        for participant in &collaborators {
            self.presence_ctx.set_participant_kind(
                crate::permissions::UserId(participant.id.clone()),
                participant.is_agent,
            );
        }
        if let Some(me) = collaborators.iter().find(|c| c.is_self) {
            self.presence_ctx
                .set_self_participant(crate::permissions::UserId(me.id.clone()), me.name.clone());
        }
        // Filtered to others: letting the self row through would put the
        // local user in every list built from presence.
        for c in collaborators.into_iter().filter(|c| !c.is_self) {
            // Relabel only — the roster doesn't say where a peer's pointer
            // is, so it must not mint one at the origin.
            self.presence_ctx
                .set_display_name(crate::permissions::UserId(c.id.clone()), c.name.clone());
            self.presence_ctx.set_participant_kind(
                crate::permissions::UserId(c.id),
                if c.is_agent {
                    crate::presence::ParticipantKind::Agent
                } else {
                    crate::presence::ParticipantKind::Person
                },
            );
        }
    }

    /// Every app in the room is handed every action; the one whose registry
    /// knows the type applies it, so the owner stays the only writer.
    /// `before` MUST be read ahead of apply: a baseline left behind the
    /// incoming change means the user's next Ctrl+Z reverts someone else's
    /// edit along with their own keystroke.
    fn apply_collab_action_arrived(&mut self, type_id: &str, payload: &[u8], from_user_id: &str) {
        let before = self.state.doc.get_heads();
        let applied = self
            .root_widget
            .apply_room_action(&mut self.state.doc, type_id, payload);
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
            self.layout.structure_dirty = true;
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
        let _entry = self.enter_wake_scope();
        let action_surface_may_have_changed = Self::collab_event_may_change_action_surface(&event);
        self.forward_session_notification(&event);
        match event {
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
            CollabEvent::FollowChanged { peer_id, mode } => match peer_id {
                Some(peer_id) => self.follow_with(peer_id, mode),
                None => self.unfollow(),
            },
            CollabEvent::Handoff { request } => self.apply_handoff(request),
            CollabEvent::DocumentSnapshotRequested { route } => {
                if !self.answer_room_index_snapshot(&route)
                    && !self.answer_owned_document_snapshot(&route)
                {
                    self.respond_primary_document_snapshot();
                }
            }
            CollabEvent::Context { collaborators } => {
                self.apply_collab_context(collaborators);
            }
            CollabEvent::Grant { grant } => {
                self.adopt_grant(&grant);
            }
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
            CollabEvent::Inspection { id, request } => self.answer_inspection(id, &request),
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
