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
        // See `invalidate_widget_resource_cache`: once the numeric namespace
        // is exhausted, dropping the projection is the collision-free way to
        // make the changed permissions visible without reusing a live cache.
        if let Some(next) = self.actions.permission_generation.get().checked_add(1) {
            self.actions.permission_generation.set(next);
        }
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

    /// Live-edit path (the late-join handshake still ships a full snapshot).
    fn broadcast_doc_delta(&mut self) {
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
                    if self.collaboration.manager.send(
                        crate::collaboration::CollabRequest::document_incremental(delta),
                    ) {
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
        if let Some(manager) = self.collaboration.room_manager.as_mut() {
            if manager.is_connected()
                && manager.send(crate::collaboration::CollabRequest::document_incremental(
                    delta,
                ))
            {
                room.advance_outbound(heads);
            }
        } else if self.persistence.records.is_some() && self.collaboration.manager.is_connected() {
            // Before the first record checkout the primary manager is the
            // room route. It becomes `collaboration.room_manager` at that checkout.
            // Do not advance this cursor for ordinary (non-record) apps.
            if self.collaboration.manager.send(
                crate::collaboration::CollabRequest::document_incremental(delta),
            ) {
                room.advance_outbound(heads);
            }
        }
    }

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
        self.root_widget
            .restore_view_state(&crate::view::ViewContribution {
                scroll: view.scroll.clone(),
                view_mode: view.view_mode,
            });
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
    }

    fn apply_signed_document_envelope(
        &mut self,
        envelope: &collab_types::SignedOp,
        decrypted_payload: Option<&[u8]>,
    ) {
        if self.persistence.records.is_some() && self.collaboration.room_manager.is_none() {
            let _ = self.apply_room_index_document_envelope(envelope, decrypted_payload);
            return;
        }
        let applied = match collab_types::DocumentMutation::decode(
            decrypted_payload.unwrap_or(&envelope.payload),
        ) {
            Ok(mutation) => match self.apply_primary_document(mutation) {
                Ok(changed) => changed,
                Err(error) => {
                    tracing::warn!(%error, "collab: primary document mutation refused");
                    false
                }
            },
            Err(error) => {
                tracing::warn!(%error, "collab: broker delivered an untyped signed document op");
                false
            }
        };
        if applied {
            self.invalidate_widget_resource_cache();
            self.needs_sync = true;
            self.layout.layout_dirty = true;
        }
    }

    /// Apply a document envelope received on the unqualified room route.
    /// The broker authenticated the route; discovery still validates and
    /// bounds its names before exposing them to the app.
    fn apply_room_index_document_envelope(
        &mut self,
        envelope: &collab_types::SignedOp,
        decrypted_payload: Option<&[u8]>,
    ) -> bool {
        let Some(room) = self.persistence.room_index.as_mut() else {
            return false;
        };
        let applied = match collab_types::DocumentMutation::decode(
            decrypted_payload.unwrap_or(&envelope.payload),
        ) {
            Ok(collab_types::DocumentMutation::Snapshot(data)) => {
                room.merge_snapshot(&data).is_ok()
            }
            Ok(collab_types::DocumentMutation::Incremental(data)) => {
                room.load_incremental(&data).is_ok()
            }
            Err(error) => {
                tracing::warn!(%error, "collab: broker delivered an untyped room-index op");
                false
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
        let events = manager.poll();
        let mut any = false;
        for event in events {
            match event {
                CollabEvent::Document {
                    envelope,
                    decrypted_payload,
                    ..
                } => {
                    any |= self.apply_room_index_document_envelope(
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
                        let _ = manager.send(
                            crate::collaboration::CollabRequest::document_snapshot(snapshot),
                        );
                    }
                }
                _ => {}
            }
        }
        any
    }

    /// Shared by the per-frame tick and the headless test path. Returns
    /// `true` iff at least one event was processed.
    #[allow(
        clippy::too_many_lines,
        reason = "one ordered pump drains directory callbacks and applies every collaboration lifecycle event before the next frame"
    )]
    pub fn pump_collab_presence(&mut self) -> bool {
        let _entry = self.enter_wake_scope();
        let mut any = self.pump_room_index_events();
        // Replies to broker pull-directory callbacks must run before regular
        // inbound events. This is the sole place capability requests acquire
        // widget/document authority, and it always runs on the App owner
        // thread.
        self.pump_action_directory_requests();
        for request in crate::widget::take_collab_requests(self.root_widget.as_mut()) {
            self.collaboration.manager.send(request);
        }
        let events = self.collaboration.manager.poll();
        for event in events {
            any = true;
            crate::widget::handle_collab_event(self.root_widget.as_mut(), &event);
            match event {
                CollabEvent::Context { collaborators, .. } => {
                    self.apply_collab_context(collaborators);
                }
                CollabEvent::FollowChanged { peer_id } => match peer_id {
                    Some(peer_id) => self.follow(peer_id),
                    None => self.unfollow(),
                },
                CollabEvent::DocumentSnapshotRequested { .. } => {
                    match self.primary_document_snapshot() {
                        Ok(data) => {
                            let _ = self
                                .collaboration
                                .manager
                                .send(crate::collaboration::CollabRequest::document_snapshot(data));
                        }
                        Err(error) => {
                            tracing::warn!(%error, "collab: primary snapshot refused");
                        }
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
                    self.apply_signed_document_envelope(&envelope, decrypted_payload.as_deref());
                }
                CollabEvent::ScrollbackBatch {
                    artifact: _,
                    ops,
                    decrypted_payloads,
                    exhausted: _,
                } => {
                    // Broker pre-validated each envelope, trusted the same
                    // way as a live document mutation.
                    tracing::debug!(
                        target: "tilo.state",
                        event = "document_scrollback",
                        operations = ops.len(),
                        "applying document scrollback"
                    );
                    for (index, envelope) in ops.into_iter().enumerate() {
                        let decrypted = decrypted_payloads
                            .get(index)
                            .and_then(|payload| payload.as_deref());
                        self.apply_signed_document_envelope(&envelope, decrypted);
                    }
                }
                CollabEvent::Invocation {
                    id,
                    resource,
                    action,
                    argument,
                    actor,
                    context,
                } => {
                    self.invoke_published_action(
                        id,
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
                CollabEvent::PublishedInvocationResult { request_id, result } => {
                    if self.close_broker_result(request_id, &result) {
                        continue;
                    }
                    let Some(invocation) = self
                        .actions
                        .pending_application_launches
                        .remove(&request_id)
                    else {
                        continue;
                    };
                    let Some(pending) = self.actions.directed_invocations.remove(&invocation)
                    else {
                        continue;
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
                                message: format!(
                                    "application launch returned non-terminal {other:?}"
                                ),
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
                // The generic catalogue/client responses are consumed by
                // widgets such as Agent through their collaboration event
                // hook; an ordinary application has no local state to apply.
                CollabEvent::ActionCatalogue { .. }
                | CollabEvent::TaskRun(_)
                | CollabEvent::TaskActionInvocationResult { .. }
                | CollabEvent::TaskRunCancellation { .. }
                | CollabEvent::WorkspaceControl { .. }
                | CollabEvent::RealtimeCommandResult { .. }
                | CollabEvent::NearbyTransferCommandResult { .. }
                | CollabEvent::Realtime(_) => {}
            }
        }
        if any {
            self.invalidate_widget_resource_cache();
        }
        any
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
        crate::widget::handle_collab_event(self.root_widget.as_mut(), &event);
        match event {
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
                self.apply_signed_document_envelope(&envelope, decrypted_payload.as_deref());
            }
            CollabEvent::FollowChanged { peer_id } => match peer_id {
                Some(peer_id) => self.follow(peer_id),
                None => self.unfollow(),
            },
            CollabEvent::DocumentSnapshotRequested { .. } => {
                let data = self.state.save();
                let _ = self
                    .collaboration
                    .manager
                    .send(crate::collaboration::CollabRequest::document_snapshot(data));
            }
            CollabEvent::Context { collaborators, .. } => {
                self.apply_collab_context(collaborators);
            }
            CollabEvent::Invocation {
                id,
                resource,
                action,
                argument,
                actor,
                context,
            } => self.invoke_published_action(
                id,
                resource,
                action,
                argument.as_deref(),
                &actor,
                context.as_deref(),
            ),
            CollabEvent::InvocationCancellation { id } => {
                self.request_invocation_cancellation(id);
            }
            CollabEvent::PublishedInvocationResult { request_id, result } => {
                if self.close_broker_result(request_id, &result) {
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
            _ => {}
        }
        self.invalidate_widget_resource_cache();
    }

    pub fn local_author(&self) -> &crate::permissions::UserId {
        &self.collaboration.local_author
    }
}
