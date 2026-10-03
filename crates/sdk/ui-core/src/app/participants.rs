//! Remote participants served by one app (agent tasks in a headless host
//! worker). Each has its own view state inside the root widget tree; the
//! App only routes lifecycle to it and drops resource caches that a new view
//! could change.

impl super::App {
    /// A participant joined the record this app serves. Multi-participant
    /// widgets create its own view (Writer: caret at the document end).
    /// Idempotent. Returns whether a view was newly created.
    pub fn join_participant(&mut self, participant: &tilo_actions::ParticipantId) -> bool {
        let joined = crate::collaboration::participants::join_participant(
            self.root_widget.as_mut(),
            participant,
        );
        if joined {
            self.invalidate_widget_resource_cache();
            self.needs_sync = true;
        }
        joined
    }

    /// Drop a participant's view (its task ended). Idempotent.
    pub fn leave_participant(&mut self, participant: &tilo_actions::ParticipantId) {
        crate::collaboration::participants::leave_participant(
            self.root_widget.as_mut(),
            participant,
        );
        self.invalidate_widget_resource_cache();
        self.needs_sync = true;
    }

    /// This app serves only remote participants and has no local person:
    /// multi-participant widgets publish their participant-independent
    /// catalogue schema instead of their own caret's snapshot.
    pub fn set_participant_host(&mut self, host: bool) {
        crate::collaboration::participants::set_participant_host(self.root_widget.as_mut(), host);
        self.invalidate_widget_resource_cache();
    }

    /// Name shown beside a participant's caret.
    pub fn set_participant_label(
        &mut self,
        participant: &tilo_actions::ParticipantId,
        label: &str,
    ) {
        crate::collaboration::participants::set_participant_label(
            self.root_widget.as_mut(),
            participant,
            label,
        );
    }

    /// Current presence of every remote participant this app holds a view for.
    pub fn participant_presence(&self) -> Vec<crate::widget::ParticipantPresence> {
        crate::collaboration::participants::participant_presence(
            self.root_widget.as_ref(),
            &self.state.doc,
        )
    }
}
