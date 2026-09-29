impl App {
    /// Pair with [`crate::persist::open`], which hands back the document the
    /// previous session left plus its writer:
    ///
    /// ```ignore
    /// let (state, autosave) = ui::persist::open("so.tilo.my-app");
    /// ui::App::from_factory(state, my_app::app_root)
    ///     .with_autosave(autosave)
    ///     .run();
    /// ```
    #[must_use]
    /// # Panics
    /// Fails closed if the host storage lifetime counter is exhausted.
    #[allow(
        clippy::expect_used,
        reason = "storage identity must fail closed rather than wrap and admit an old owner"
    )]
    pub fn with_autosave(mut self, autosave: crate::persist::DocWriter) -> Self {
        self.autosave_generation = self
            .autosave_generation
            .checked_add(1)
            .expect("autosave generation exhausted");
        if let Some(history) = &mut self.history {
            self.layout.node_map.clear();
            self.bindings.clear();
            self.focused_node = None;
            history.invalidate_owner();
            self.layout.structure_dirty = true;
        }
        self.persistence.autosave = Some(autosave);
        self
    }

    #[cfg(feature = "native")]
    pub(crate) fn with_legacy_persistence(
        mut self,
        policy: crate::launch::LegacyPersistencePolicy,
    ) -> Self {
        self.legacy_persistence = policy;
        self
    }
    #[must_use]
    pub fn with_records(
        mut self,
        mut records: crate::record::RecordSet,
        room_index: automerge::AutoCommit,
    ) -> Self {
        let _entry = self.enter_wake_scope();
        self.advance_autosave_generation();
        if let Some(history) = &mut self.history {
            self.layout.node_map.clear();
            self.bindings.clear();
            self.focused_node = None;
            history.invalidate_owner();
            self.layout.structure_dirty = true;
        }
        // Learn the current room grant before the widget selects a record.
        // Discovery is metadata-only: it must not create an empty record
        // replica or join a collaboration route for every indexed document.
        records.discover_room(&room_index);
        self.persistence.records = Some(records);
        self.persistence.room_index = Some(crate::state::DocumentReplica::new(room_index));
        self
    }

    /// Starts a new autosave generation so writes queued for the previous
    /// document are recognised as stale. A `u64` counter never wraps in
    /// practice; wrapping keeps this total instead of panicking.
    fn advance_autosave_generation(&mut self) {
        self.autosave_generation = self.autosave_generation.wrapping_add(1);
    }

    pub fn records(&self) -> Option<&crate::record::RecordSet> {
        self.persistence.records.as_ref()
    }

    pub fn room_index(&self) -> Option<&automerge::AutoCommit> {
        self.persistence
            .room_index
            .as_ref()
            .map(crate::state::DocumentReplica::document)
    }

    pub fn record_tick(&mut self) -> bool {
        let _entry = self.enter_wake_scope();
        let native = self.collaboration.manager.has_native_committed_primary();
        let initial = if native {
            let Some(want) = crate::record::capability::record_intent(self.root_widget.as_ref())
            else {
                return false;
            };
            if self
                .persistence
                .records
                .as_ref()
                .is_some_and(|records| records.active() == Some(&want.id))
            {
                return false;
            }
            let Some(read) = self.collaboration.manager.prepare_native_record(&want) else {
                return false;
            };
            Some((want, read))
        } else {
            None
        };
        let Some(set) = self.persistence.records.as_mut() else {
            return false;
        };
        let switched = if let Some((want, read)) = &initial {
            let accepted = read
                .with_mutation(|mutation| {
                    set.checkout_with_initial(
                        &mut self.state,
                        self.persistence
                            .room_index
                            .as_mut()
                            .map(crate::state::DocumentReplica::document_mut),
                        want,
                        mutation,
                    )
                })
                .unwrap_or(false);
            if accepted {
                read.applied();
                Some(want.clone())
            } else {
                None
            }
        } else {
            crate::record::tick(
                set,
                &mut self.state,
                self.persistence
                    .room_index
                    .as_mut()
                    .map(crate::state::DocumentReplica::document_mut),
                &*self.root_widget,
            )
        };
        match switched {
            Some(checkout) => {
                self.advance_autosave_generation();
                if let Some(history) = &mut self.history {
                    self.layout.node_map.clear();
                    self.bindings.clear();
                    self.focused_node = None;
                    history.invalidate_owner();
                    self.layout.structure_dirty = true;
                }
                self.state.rebase_outbound();
                // Preserve the launcher's unqualified route for the room
                // index before moving the editable document to its record
                // route. The two Automerge graphs never share a channel or
                // an apply target.
                if native {
                    if self.native_room_route.is_none() {
                        self.native_room_route = self
                            .collaboration
                            .manager
                            .primary_route()
                            .map(|witness| witness.route.clone());
                    }
                    if let Err(error) = self.collaboration.manager.switch_native_record(&checkout) {
                        tracing::warn!(%error,"native document route switch remains unavailable");
                        return false;
                    }
                } else if self.collaboration.room_manager.is_none() {
                    let room = std::mem::replace(
                        &mut self.collaboration.manager,
                        CollaborationManager::for_record(&checkout),
                    );
                    self.collaboration.room_manager = Some(room);
                } else {
                    self.collaboration.manager = CollaborationManager::for_record(&checkout);
                }
                tracing::debug!(
                    target: "tilo.state",
                    event = "record_checkout",
                    record = checkout.id.as_str(),
                    connected = self.collaboration.manager.is_connected(),
                    "switched document collaboration route"
                );
                self.needs_sync = true;
                self.layout.layout_dirty = true;
                // Presence announces the location; document catch-up is an
                // independently directed broker request on the new route.
                // Both go out once the new route is acknowledged; the manager
                // connects in the background.
                if let Some(scope) = self.presence.broadcast_scope.clone() {
                    self.broadcast_presence(scope, crate::collaboration::PresencePayload::Arrived);
                }
                self.collaboration.snapshot_on_ready = true;
                self.request_checkout_snapshot();
                true
            }
            None => false,
        }
    }

    /// With records in play, the App's autosave writes the room index
    /// instead of the live document — sending the live doc to both would put
    /// a record's content in the room index.
    #[cfg(feature = "native")]
    fn autosave_tick(&mut self) -> Result<(), crate::bound_dispatch::BoundDispatchError> {
        match self.persistence.records.as_mut() {
            Some(set) => {
                set.tick(&mut self.state.doc);
                if let (Some(saver), Some(room)) = (
                    self.persistence.autosave.as_mut(),
                    self.persistence.room_index.as_mut(),
                ) {
                    saver.request(room.document_mut());
                }
            }
            None => {
                if let Some(saver) = self.persistence.autosave.as_mut() {
                    saver.request(&mut self.state.doc);
                }
            }
        }
        // Collect through the same live component boundary used by dispatch.
        // Distinct embedded owners participate; repeated views share one tick.
        let mut owners: Vec<crate::record::RecordStore> = Vec::new();
        let mut visit = |binding: &crate::bound_dispatch::CallbackBinding,
                         _: &mut dyn crate::widget::AppWidget,
                         _: &tilo_actions::ActionRegistry| {
            if !owners
                .iter()
                .any(|owner| owner.same_owner(&binding.records))
            {
                owners.push(binding.records.clone());
            }
            None
        };
        let traversal =
            self.root_widget
                .visit_components(&mut crate::bound_dispatch::ComponentRequest {
                    target: None,
                    found: false,
                    visit: &mut visit,
                });
        // Release component callback loans before requesting writer work. Owners
        // already collected still progress if another live component refuses entry.
        for owner in owners {
            owner.tick();
        }
        traversal
    }

    /// Called on event-loop teardown so a `kill` right after the last
    /// keystroke still finds it on disk.
    #[cfg(feature = "native")]
    fn autosave_flush(&mut self) {
        if self.legacy_persistence == crate::launch::LegacyPersistencePolicy::WidgetOwned {
            self.close_components();
            if let Err(error) = flush_widget_owned_persistence(
                self.root_widget.as_mut(),
                &mut self.component_close_reports,
            ) {
                tracing::error!(%error, "widget-owned persistence failed during shutdown");
                eprintln!("tilo: notebook save failed during shutdown: {error}");
            }
            return;
        }
        if let Some(set) = self.persistence.records.as_mut() {
            set.flush(&mut self.state.doc);
            if let (Some(saver), Some(room)) = (
                self.persistence.autosave.as_mut(),
                self.persistence.room_index.as_mut(),
            ) {
                saver.request(room.document_mut());
                saver.flush();
            }
            return;
        }
        if let Some(saver) = self.persistence.autosave.as_mut() {
            saver.request(&mut self.state.doc);
            saver.flush();
        }
    }

    /// Lets a caller check [`crate::persist::DocWriter::last_error`] rather
    /// than assuming the disk accepted the writes.
    pub fn autosave(&self) -> Option<&crate::persist::DocWriter> {
        self.persistence.autosave.as_ref()
    }
}
