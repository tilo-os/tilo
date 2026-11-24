//! The window of a document that did not read as the app's resource: why, and
//! the resource's actions published unavailable for that reason.

use tilo_actions::ResourceSpec;
use ui_core::automerge::AutoCommit;
use ui_core::widget::{AppKind, AppWidget, Decorator, IconDescriptor};

use super::unreadable_state;

/// The window of a document that did not read as the resource when it was
/// opened: why, instead of the app's view, and the resource's actions
/// published as unavailable for that reason.
pub(crate) struct Unopened {
    title: &'static str,
    icon: IconDescriptor,
    /// What it publishes: the resource's declared actions, every document
    /// one unavailable for the reason.
    resource: ResourceSpec,
    shown: ui_core::widget::EmptyState,
}

impl Unopened {
    /// The window of a document that does not read as the resource of
    /// `kind` and `label`, whose actions are `declarations`.
    pub(crate) fn new(
        title: &'static str,
        icon: IconDescriptor,
        (kind, label): (&'static str, &'static str),
        declarations: &[tilo_resource::Declaration],
        reason: &str,
    ) -> Self {
        let mut resource = ResourceSpec::new(
            kind,
            kind,
            tilo_resource::label::localized(kind, label, None),
        );
        for decl in declarations {
            resource = resource.with_action(tilo_resource::declaration_spec(decl));
        }
        tilo_resource::withhold_document_actions(&mut resource, reason);
        Self {
            title,
            icon,
            shown: unreadable_state(reason),
            resource,
        }
    }
}

impl Unopened {
    /// The window as a host mounts it, its widget table compiled once for
    /// every app.
    pub(crate) fn boxed(self) -> Box<dyn AppWidget> {
        Box::new(self)
    }
}

impl Decorator for Unopened {
    ui_core::provides!(dyn ui_core::widget::resources::PublishesResources);

    type Inner = ui_core::widget::EmptyState;

    fn inner(&self) -> &ui_core::widget::EmptyState {
        &self.shown
    }

    fn inner_mut(&mut self) -> &mut ui_core::widget::EmptyState {
        &mut self.shown
    }
}

impl ui_core::widget::resources::PublishesResources for Unopened {
    fn resources(&self, _doc: &AutoCommit) -> Vec<ResourceSpec> {
        vec![self.resource.clone()]
    }
}

impl AppWidget for Unopened {
    fn app_kind(&self) -> AppKind {
        AppKind::Editor
    }

    fn app_title(&self) -> &str {
        self.title
    }

    fn app_icon(&self) -> IconDescriptor {
        self.icon.clone()
    }
}
