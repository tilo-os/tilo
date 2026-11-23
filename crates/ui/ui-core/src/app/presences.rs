//! Remote presences served by one app (an agent's run in a headless host
//! worker). Each has its own view state inside the root widget tree; the
//! App only routes lifecycle to it and drops resource caches that a new view
//! could change.

impl super::App {
    /// A presence joined the record this app serves. Multi-presence
    /// widgets create its own view (Writer: caret at the document end).
    /// Idempotent. Returns whether a view was newly created.
    pub fn join_presence(&mut self, presence: &tilo_actions::PresenceId) -> bool {
        let joined =
            crate::collaboration::presences::join_presence(self.root_widget.as_mut(), presence);
        if joined {
            self.invalidate_widget_resource_cache();
            self.needs_sync = true;
        }
        joined
    }

    /// Drop a presence's view (its run ended). Idempotent.
    pub fn leave_presence(&mut self, presence: &tilo_actions::PresenceId) {
        crate::collaboration::presences::leave_presence(self.root_widget.as_mut(), presence);
        self.invalidate_widget_resource_cache();
        self.needs_sync = true;
    }

    /// This app serves only remote presences and has no local person:
    /// multi-presence widgets publish their presence-independent
    /// catalogue schema instead of their own caret's snapshot.
    pub fn set_remote_only(&mut self, host: bool) {
        crate::collaboration::presences::set_remote_only(self.root_widget.as_mut(), host);
        self.invalidate_widget_resource_cache();
    }

    /// Name shown beside a presence's caret.
    pub fn set_presence_label(&mut self, presence: &tilo_actions::PresenceId, label: &str) {
        crate::collaboration::presences::set_presence_label(
            self.root_widget.as_mut(),
            presence,
            label,
        );
    }

    /// Current presence of every remote presence this app holds a view for.
    pub fn presences(&self) -> Vec<crate::widget::ViewPresence> {
        crate::collaboration::presences::presences(self.root_widget.as_ref(), &self.state.doc)
    }
}
