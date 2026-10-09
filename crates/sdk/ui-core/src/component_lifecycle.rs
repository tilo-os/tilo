//! How a widget takes part in closing its window and components: unsaved
//! work, persistence checkpoints, component close reports and deferred
//! window close.
//!
//! A widget providing [`ComponentLifecycle`] answers for its subtree; the
//! defaults do what an ordinary container did (reach the children), so an
//! override names only what differs.

use crate::widget::capability::{capability, capability_mut};
use crate::widget::{Widget, WindowCloseAdmission};
use automerge::AutoCommit;

/// A widget owning components, documents or a window-close policy.
pub trait ComponentLifecycle: Widget {
    /// App-declared external document obligations. An in-memory record set
    /// is not evidence that an editable file has been saved.
    fn unsaved_documents(&self, doc: &AutoCommit) -> Vec<crate::close_admission::UnsavedDocument> {
        let mut out = Vec::new();
        self.for_each_child(&mut |child| out.extend(unsaved_documents(child, doc)));
        out
    }

    /// Non-destructive persistence checkpoint. Never closes a wake scope or
    /// removes a tree; a failed host close must leave every child usable.
    fn checkpoint_components(&mut self) -> Vec<String> {
        self.children_mut()
            .into_iter()
            .flat_map(checkpoint_components)
            .collect()
    }

    /// Only the host may call this after every checkpoint succeeded, without
    /// dispatching another document mutation in between.
    fn finish_admitted_close(&mut self) -> Vec<crate::application::ComponentCloseReport> {
        self.children_mut()
            .into_iter()
            .flat_map(finish_admitted_close)
            .collect()
    }

    /// Capture and close nested components before removing their live trees,
    /// transferring all recovery reports to the caller.
    fn close_components(&mut self) -> Vec<crate::application::ComponentCloseReport> {
        self.children_mut()
            .into_iter()
            .flat_map(close_components)
            .collect()
    }

    /// Drain inert close-report ownership once; never invoke ordinary app
    /// callbacks through a closed component.
    fn take_component_close_reports(&mut self) -> Vec<crate::application::ComponentCloseReport> {
        self.children_mut()
            .into_iter()
            .flat_map(take_component_close_reports)
            .collect()
    }

    /// Admit or defer the primary native window's ordinary close request.
    /// Roots that defer must later produce a one-shot close intent.
    fn on_window_close_requested(&mut self) -> WindowCloseAdmission {
        WindowCloseAdmission::Allow
    }

    /// One-shot approval after an earlier deferred ordinary window close.
    fn take_window_close_intent(&mut self) -> bool {
        false
    }

    /// The component this root's own component asked to show in its place
    /// ([`AppComponent::take_replacement`](crate::application::AppComponent::take_replacement)),
    /// for the host that mounted it. A component nested deeper answers to
    /// the host that mounted it, so the default reaches no child.
    fn take_replacement(&mut self) -> Option<crate::application::ComponentReplacement> {
        None
    }
}

/// The component `widget`'s own component asked to show in its place.
pub fn take_replacement(
    widget: &mut dyn Widget,
) -> Option<crate::application::ComponentReplacement> {
    capability_mut::<dyn ComponentLifecycle>(widget)?.take_replacement()
}

/// Unsaved external documents under `widget`.
pub fn unsaved_documents(
    widget: &dyn Widget,
    doc: &AutoCommit,
) -> Vec<crate::close_admission::UnsavedDocument> {
    if let Some(lifecycle) = capability::<dyn ComponentLifecycle>(widget) {
        return lifecycle.unsaved_documents(doc);
    }
    let mut out = Vec::new();
    widget.for_each_child(&mut |child| out.extend(unsaved_documents(child, doc)));
    out
}

/// Checkpoint every component under `widget`; returns the errors.
pub fn checkpoint_components(widget: &mut dyn Widget) -> Vec<String> {
    if let Some(lifecycle) = capability_mut::<dyn ComponentLifecycle>(&mut *widget) {
        return lifecycle.checkpoint_components();
    }
    widget
        .children_mut()
        .into_iter()
        .flat_map(checkpoint_components)
        .collect()
}

/// Finish an admitted close under `widget`.
pub fn finish_admitted_close(
    widget: &mut dyn Widget,
) -> Vec<crate::application::ComponentCloseReport> {
    if let Some(lifecycle) = capability_mut::<dyn ComponentLifecycle>(&mut *widget) {
        return lifecycle.finish_admitted_close();
    }
    widget
        .children_mut()
        .into_iter()
        .flat_map(finish_admitted_close)
        .collect()
}

/// Close every component under `widget`.
pub fn close_components(widget: &mut dyn Widget) -> Vec<crate::application::ComponentCloseReport> {
    if let Some(lifecycle) = capability_mut::<dyn ComponentLifecycle>(&mut *widget) {
        return lifecycle.close_components();
    }
    widget
        .children_mut()
        .into_iter()
        .flat_map(close_components)
        .collect()
}

/// Drain close reports under `widget`.
pub fn take_component_close_reports(
    widget: &mut dyn Widget,
) -> Vec<crate::application::ComponentCloseReport> {
    if let Some(lifecycle) = capability_mut::<dyn ComponentLifecycle>(&mut *widget) {
        return lifecycle.take_component_close_reports();
    }
    widget
        .children_mut()
        .into_iter()
        .flat_map(take_component_close_reports)
        .collect()
}

/// `widget`'s answer to an ordinary window close request.
pub fn on_window_close_requested(widget: &mut (impl Widget + ?Sized)) -> WindowCloseAdmission {
    capability_mut::<dyn ComponentLifecycle>(widget).map_or(
        WindowCloseAdmission::Allow,
        ComponentLifecycle::on_window_close_requested,
    )
}

/// Whether `widget` approved a deferred window close.
pub fn take_window_close_intent(widget: &mut (impl Widget + ?Sized)) -> bool {
    capability_mut::<dyn ComponentLifecycle>(widget)
        .is_some_and(ComponentLifecycle::take_window_close_intent)
}

/// A presentation can request activation without changing its document or owner.
pub trait WindowActivation {
    /// Consume one request to reveal this presentation's existing native window.
    fn take_window_activation_intent(&self) -> bool;
}

/// Consume an activation request through the root's existing capability door.
pub fn take_window_activation_intent(widget: &(impl Widget + ?Sized)) -> bool {
    capability::<dyn WindowActivation>(widget)
        .is_some_and(WindowActivation::take_window_activation_intent)
}
