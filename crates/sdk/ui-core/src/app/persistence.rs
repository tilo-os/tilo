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
    pub fn with_autosave(mut self, autosave: crate::persist::Autosave) -> Self {
        self.persistence.autosave = Some(autosave);
        self
    }

    pub(crate) fn with_legacy_persistence(
        mut self,
        policy: crate::launch::LegacyPersistencePolicy,
    ) -> Self {
        self.legacy_persistence = policy;
        self
    }
    /// `records` owns per-record documents and persistence; `room_index` is
    /// which records the room holds. Pair with [`App::with_autosave`] only if
    /// the room index itself should persist — records persist through
    /// `records`.
    #[must_use]
    pub fn with_records(
        mut self,
        mut records: crate::record::RecordSet,
        room_index: automerge::AutoCommit,
    ) -> Self {
        let _entry = self.enter_wake_scope();
        // Learn the current room grant before the widget selects a record.
        // Discovery is metadata-only: it must not create an empty record
        // replica or join a collaboration route for every indexed document.
        records.discover_room(&room_index);
        self.persistence.records = Some(records);
        self.persistence.room_index = Some(crate::state::DocumentReplica::new(room_index));
        self
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

    /// The record is the collaboration routing key, so a checkout is a
    /// change of address — without re-announcing it, deltas keep publishing
    /// under the previous record's name.
    pub fn record_tick(&mut self) -> bool {
        let _entry = self.enter_wake_scope();
        let Some(set) = self.persistence.records.as_mut() else {
            return false;
        };
        let switched = crate::record::tick(
            set,
            &mut self.state,
            self.persistence
                .room_index
                .as_mut()
                .map(crate::state::DocumentReplica::document_mut),
            &*self.root_widget,
        );
        match switched {
            Some(id) => {
                self.state.rebase_outbound();
                // Preserve the launcher's unqualified route for the room
                // index before moving the editable document to its record
                // route. The two Automerge graphs never share a channel or
                // an apply target.
                if self.collaboration.room_manager.is_none() {
                    let room = std::mem::replace(
                        &mut self.collaboration.manager,
                        CollaborationManager::for_record(&id),
                    );
                    self.collaboration.room_manager = Some(room);
                } else {
                    self.collaboration.manager = CollaborationManager::for_record(&id);
                }
                tracing::debug!(
                    target: "tilo.state",
                    event = "record_checkout",
                    record = id.as_str(),
                    connected = self.collaboration.manager.is_connected(),
                    "switched document collaboration route"
                );
                self.needs_sync = true;
                self.layout.layout_dirty = true;
                // Presence announces the location; document catch-up is an
                // independently directed broker request on the new route.
                if let Some(scope) = self.presence.broadcast_scope.clone() {
                    self.broadcast_presence(scope, crate::collaboration::PresencePayload::Arrived);
                }
                let _ = self
                    .collaboration
                    .manager
                    .send(crate::collaboration::CollabRequest::RequestDocumentSnapshot);
                true
            }
            None => false,
        }
    }

    /// With records in play, the App's autosave writes the room index
    /// instead of the live document — sending the live doc to both would put
    /// a record's content in the room index.
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
    fn autosave_flush(&mut self) {
        if self.legacy_persistence == crate::launch::LegacyPersistencePolicy::WidgetOwned {
            if let Err(error) = self
                .root_widget
                .try_flush_owned_persistence()
                .unwrap_or_else(|| {
                    Err("widget-owned persistence requires a checked flush hook".into())
                })
            {
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

    /// Lets a caller check [`crate::persist::Autosave::last_error`] rather
    /// than assuming the disk accepted the writes.
    pub fn autosave(&self) -> Option<&crate::persist::Autosave> {
        self.persistence.autosave.as_ref()
    }
}
