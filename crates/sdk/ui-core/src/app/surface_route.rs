//! Publishing the app's action surface on its route: validated against the
//! operations the app handles, refused whole when a control's binding is
//! stale, and sent as the change since the broker's copy.

use super::{App, ControlBindingStatus};
use crate::collaboration::CollabRequest;

impl App {
    /// Whether the route now carries this app's current offer: its surface
    /// went out (or was already the host's copy) after the route's history
    /// was applied, or the app withdrew an offer it may not make.
    pub(super) fn publish_action_surface_on_route(&mut self) -> bool {
        let _background = crate::local_document_action::suspend();
        #[cfg(test)]
        {
            self.actions.action_surface_publication_attempts = self
                .actions
                .action_surface_publication_attempts
                .saturating_add(1);
        }
        if !self.collaboration.manager.is_connected()
            || !self.published_action_history_ready_after_refresh()
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
        // Use this exact snapshot for coverage as well as publication. That
        // prevents recursive/repeated resource collection and makes the audit
        // correspond to the catalogue about to cross the wire.
        let binding_coverage = self.control_binding_coverage_for(
            &self.collect_resources(),
            crate::widget::controls::action_control_bindings(self.root_widget.as_ref()),
        );
        if binding_coverage
            .iter()
            .any(|entry| entry.status == ControlBindingStatus::StaleAddress)
        {
            self.actions.action_directory_snapshot = None;
            self.actions.action_directory_stamp = None;
            self.actions.action_directory_cursors.clear();
            for entry in binding_coverage
                .iter()
                .filter(|entry| entry.status == ControlBindingStatus::StaleAddress)
            {
                let binding = format!("{:?}", entry.binding.kind);
                if self.actions.refusals_logged.first(&(
                    "stale",
                    &entry.binding.control_id,
                    &binding,
                )) {
                    tracing::error!(
                        control = %entry.binding.control_id,
                        %binding,
                        "refusing to publish a surface with a stale control action binding"
                    );
                }
            }
            if self
                .actions
                .last_published_actions
                .as_ref()
                .is_some_and(|actions| !actions.is_empty())
            {
                return self.withdraw_action_surface(publication_app_id);
            }
            return true;
        }
        let mut operation_handlers = crate::widget::resource_operation_handlers(&*self.root_widget);
        operation_handlers.insert(crate::a11y_tree::SEMANTIC_OPERATION_TYPE.to_string());
        #[cfg(feature = "native")]
        if self.accessibility.zoom.in_window() {
            operation_handlers.insert(crate::host_chrome::zoom::ZOOM_OPERATION.to_string());
        }
        // Bound components have already been validated against their own registries
        // by publication_resources. Only legacy resources use the host registry.
        let unbound_resources = resources
            .iter()
            .filter(|resource| {
                !resource
                    .id
                    .as_str()
                    .starts_with(crate::bound_dispatch::PUBLICATION_PREFIX)
            })
            .cloned()
            .collect::<Vec<_>>();
        let violations = tilo_actions::validate_surface(
            &unbound_resources,
            &self.actions.action_registry,
            &operation_handlers,
        );
        if !violations.is_empty() {
            for violation in &violations {
                if self.actions.refusals_logged.first(&(
                    "surface",
                    &violation.address,
                    &violation.reason,
                )) {
                    tracing::error!(
                        address = %violation.address,
                        reason = %violation.reason,
                        "refusing to publish an invalid resource/action surface"
                    );
                }
            }
            // Withdraw anything previously published. A stale, apparently
            // valid catalogue is more dangerous than an empty one.
            if self
                .actions
                .last_published_actions
                .as_ref()
                .is_none_or(Vec::is_empty)
            {
                return true;
            }
            return self.withdraw_action_surface(crate::collaboration::local_app_id());
        }
        let descriptors = tilo_actions::door::survey(&resources);
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
        let (delta, state) = crate::collaboration::SurfacePublication::next(prior, &descriptors);
        // A surface the transport did not take is retried on the next frame.
        if delta.is_empty()
            || self
                .collaboration
                .manager
                .send(CollabRequest::PublishActionSurface {
                    app_id: publication_app_id,
                    delta,
                })
        {
            self.actions.last_published_actions = Some(descriptors);
            self.actions.published_surface = Some(state);
            return true;
        }
        false
    }
}
