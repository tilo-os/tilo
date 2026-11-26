//! Publishing the app's action surface on its route, sent as the change
//! since the broker's copy. What it holds was validated as it was collected
//! (`ensure_resource_cache`, bound components' contracts).

use super::{App, ControlBindingStatus};
use tilo_ui_base::collaboration::CollabRequest;

impl App {
    /// Whether the route now carries this app's current offer: its surface
    /// went out (or was already the host's copy) after the route's history
    /// was applied.
    pub(super) fn publish_action_surface_on_route(&mut self) -> bool {
        let _background = crate::local_document_action::suspend();
        #[cfg(test)]
        {
            self.actions.action_surface_publication_attempts = self
                .actions
                .action_surface_publication_attempts
                .saturating_add(1);
        }
        // A surface's actions are the shell's, which placed its window:
        // its records stay in its process, so it has no document route for
        // them to wait on.
        if !self.collaboration.manager.is_connected()
            || !(self.collaboration.manager.hosts_surface()
                || self.published_action_history_ready_after_refresh())
        {
            return false;
        }
        if self.collaboration.manager.take_action_surface_failed() {
            // The broker did not install the last surface: nothing counts as
            // published until it acknowledges one.
            self.actions.last_published_actions = None;
        }
        let route = self.bound_broker_route().cloned();
        if self.actions.last_published_bound_route != route {
            self.actions.last_published_actions = None;
            self.actions.action_directory_snapshot = None;
            self.actions.last_published_bound_route = route;
        }
        let publication_app_id = self.publication_app_id();
        // This is independent of the legacy eager surface below.  The
        // broker owns the capability and asks the UI thread to materialise a
        // bounded page; large apps therefore do not become a flattened RPC
        // catalogue just because they are visible in shell search.
        self.collaboration
            .manager
            .publish_action_directory(publication_app_id.clone());
        let resources = self.publication_resources();
        // The surface is read from the same tree as the controls, so every
        // control's action is on it. One that is not is a bug in how the
        // declarations follow the tree: it is reported, and the rest of the
        // surface still goes out.
        for entry in self.control_binding_coverage_for(
            &self.collect_resources(),
            tilo_ui_base::widget::controls::action_control_bindings(self.root_widget.as_ref()),
        ) {
            let binding = format!("{:?}", entry.binding.kind);
            if entry.status == ControlBindingStatus::StaleAddress
                && self.actions.refusals_logged.first(&(
                    "stale",
                    &entry.binding.control_id,
                    &binding,
                ))
            {
                tracing::error!(
                    control = %entry.binding.control_id,
                    %binding,
                    "a control's action is missing from the surface it publishes"
                );
            }
        }
        let descriptors = tilo_actions::api::survey(&resources);
        self.capture_action_directory_snapshot(&resources, &descriptors);
        if self.actions.last_published_actions.as_ref() == Some(&descriptors) {
            return true;
        }
        // Only what changed since the broker's copy crosses: each distinct
        // catalogue entry once, and per resource action its own fields. With
        // nothing the broker is known to hold (first publication, or it
        // refused the last one) the delta is a reset.
        let prior = self
            .actions
            .last_published_actions
            .as_deref()
            .zip(self.actions.published_surface.as_ref())
            .map(|(descriptors, state)| (state, descriptors));
        let (delta, state) = tilo_ui_base::collaboration::SurfacePublication::next(prior, &descriptors);
        // A surface the transport did not take is retried on the next frame.
        if delta.is_empty()
            || self
                .collaboration
                .manager
                .send(CollabRequest::PublishActionSurface { delta })
        {
            self.actions.last_published_actions = Some(descriptors);
            self.actions.published_surface = Some(state);
            return true;
        }
        false
    }
}
