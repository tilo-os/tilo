//! How a widget takes part in closing its window and components: unsaved work, persistence checkpoints,
//! component close reports and deferred window close.

use tilo_ui_base::{widget::capability::capability, widget::capability::capability_mut};
use tilo_ui_base::{widget::Widget, widget::WindowCloseAdmission};
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

    /// What the components' checkpoint sent that the workspace has not answered yet, asked
    /// each pass of a close without waiting ([`crate::close_admission::Settling`]).
    #[doc(hidden)]
    #[allow(
        private_interfaces,
        reason = "what a close waits on is the toolkit's own, asked only by its hosts"
    )]
    fn close_settling(&mut self) -> crate::close_admission::Settling {
        let mut settling = crate::close_admission::Settling::default();
        for child in self.children_mut() {
            settling.join(close_settling(child));
        }
        settling
    }

    /// Only the host may call this after every checkpoint succeeded, without dispatching another
    /// document mutation in between.
    #[cfg(feature = "model")]
    fn finish_admitted_close(&mut self) -> Vec<crate::application::ComponentCloseReport> {
        self.children_mut()
            .into_iter()
            .flat_map(finish_admitted_close)
            .collect()
    }

    /// Capture and close nested components before removing their live trees,
    /// transferring all recovery reports to the caller.
    #[cfg(feature = "model")]
    fn close_components(&mut self) -> Vec<crate::application::ComponentCloseReport> {
        self.children_mut()
            .into_iter()
            .flat_map(close_components)
            .collect()
    }

    /// Drain inert close-report ownership once; never invoke ordinary app
    /// callbacks through a closed component.
    #[cfg(feature = "model")]
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
    #[cfg(feature = "model")]
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
#[cfg(feature = "model")]
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

/// What the checkpoint of every component under `widget` sent that the workspace has not answered
/// yet.
pub(crate) fn close_settling(widget: &mut dyn Widget) -> crate::close_admission::Settling {
    if let Some(lifecycle) = capability_mut::<dyn ComponentLifecycle>(&mut *widget) {
        return lifecycle.close_settling();
    }
    let mut settling = crate::close_admission::Settling::default();
    for child in widget.children_mut() {
        settling.join(close_settling(child));
    }
    settling
}

#[cfg(feature = "model")]
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

#[cfg(feature = "model")]
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

#[cfg(feature = "model")]
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

/// Consume an activation request through the root's existing capability API.
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

/// What `widget`'s window shows ([`WindowTitle`]), through its capability API.
pub fn window_title(widget: &(impl Widget + ?Sized)) -> Option<&str> {
    capability::<dyn WindowTitle>(widget).and_then(WindowTitle::window_title)
}

/// A presentation whose app runs elsewhere (a tilolang host's instance) can ask the shell for what only
/// it does, as an app surface asks ([`crate::widget::AppProcessIntent`]): show a task run's log when a
/// change mark is activated.
pub trait HostIntents {
    /// Consume one request for the shell.
    fn take_process_intent(&mut self) -> Option<crate::widget::AppProcessIntent>;
}

/// Consume one request `widget` has for the shell, through its capability API.
pub fn take_process_intent(
    widget: &mut (impl Widget + ?Sized),
) -> Option<crate::widget::AppProcessIntent> {
    capability_mut::<dyn HostIntents>(widget).and_then(HostIntents::take_process_intent)
}

/// What the connection of an app that runs elsewhere (a tilolang host's instance) heard at the
/// app, handed to the window that presents it. That window has no connection of its own: it
/// shows these as a native app's window shows what its own connection hears. Who the workspace's
/// members are is its `workspace/members` resource, which the app's host reads through the store.
#[cfg(feature = "model")]
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum Heard {
    /// A step of an action someone takes on one of the app's resources.
    Interaction(tilo_actions::SemanticInteraction),
    /// What the workspace calls each member, from the app's own read of its
    /// `workspace/members` resource: a window with no connection of its own names the people in
    /// its presence marks by these.
    Names(Vec<(tilo_ui_base::collaboration::MemberId, String)>),
}

/// A presentation whose app runs elsewhere hands over what its app's connection heard of the
/// people at it ([`Heard`]).
#[cfg(feature = "model")]
pub trait HearsElsewhere {
    /// Consume what arrived since this was last asked, oldest first.
    fn take_heard(&mut self) -> Vec<Heard>;
}

/// Consume what `widget` heard of the people at its app ([`HearsElsewhere`]), through its
/// capability API.
#[cfg(feature = "model")]
pub fn take_heard(widget: &mut (impl Widget + ?Sized)) -> Vec<Heard> {
    capability_mut::<dyn HearsElsewhere>(widget).map_or_else(Vec::new, HearsElsewhere::take_heard)
}
