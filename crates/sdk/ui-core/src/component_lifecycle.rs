//! How a widget takes part in closing its window and components: unsaved work, persistence checkpoints,
//! component close reports and deferred window close.

use crate::widget::capability::{capability, capability_mut};
use crate::widget::{Widget, WindowCloseAdmission};
use automerge::AutoCommit;

/// A widget owning components, documents or a window-close policy.
pub trait ComponentLifecycle: Widget {
    /// App-declared external document obligations.
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

    /// Only the host may call this after every checkpoint succeeded, without dispatching another
    /// document mutation in between.
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

    /// The component this root component asked to show in its place
    /// ([`AppComponent::take_replacement`](crate::application::AppComponent::take_replacement)), for
    /// the host that mounted it.
    fn take_replacement(&mut self) -> Option<crate::application::ComponentReplacement> {
        None
    }

    /// One-shot: the durability of this root component's record changed since this was last asked, and
    /// the root already synced with the new state.
    fn take_save_status_presentation(&mut self) -> bool {
        false
    }
}

/// Whether the record `widget`'s own component shows changed durability,
/// which the root has synced with and a frame must present.
pub fn take_save_status_presentation(widget: &mut dyn Widget) -> bool {
    capability_mut::<dyn ComponentLifecycle>(widget)
        .is_some_and(ComponentLifecycle::take_save_status_presentation)
}

/// The component `widget`'s own component asked to show in its place.
pub fn take_replacement(
    widget: &mut dyn Widget,
) -> Option<crate::application::ComponentReplacement> {
    capability_mut::<dyn ComponentLifecycle>(widget)?.take_replacement()
}

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

/// A window whose content says what it shows: the page a note is open on, the place a list is at, the
/// record a sheet edits.
pub trait WindowTitle {
    /// What the window shows now, as words a person reads: `None` where it is empty.
    fn window_title(&self) -> Option<&str>;
}

/// What `widget`'s window shows ([`WindowTitle`]), through its capability door.
pub fn window_title(widget: &(impl Widget + ?Sized)) -> Option<&str> {
    capability::<dyn WindowTitle>(widget).and_then(WindowTitle::window_title)
}

/// A presentation whose app runs elsewhere (a tilolang host's instance) can ask the shell for what only
/// it does, as an app surface asks ([`crate::widget::AppHostIntent`]): show a task run's log when a
/// change mark is activated.
pub trait HostIntents {
    /// Consume one request for the shell.
    fn take_host_intent(&mut self) -> Option<crate::widget::AppHostIntent>;
}

/// Consume one request `widget` has for the shell, through its capability door.
pub fn take_host_intent(
    widget: &mut (impl Widget + ?Sized),
) -> Option<crate::widget::AppHostIntent> {
    capability_mut::<dyn HostIntents>(widget).and_then(HostIntents::take_host_intent)
}
