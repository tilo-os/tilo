//! Input a person gives before the window's content has arrived. A window can stand before what it
//! shows is there: the page reserved while the workspace's holdings arrive, a tilolang window
//! waiting for its app's first view, the loading window of a launch whose record is on its way.
//! Nothing in it can take a key then, and a key it does not take is gone: Ctrl+N pressed as Notes
//! opens would make no note, and say nothing. So the keys, and the text an input method commits,
//! wait in order, each with the modifiers held when it was pressed, until the content stands; the
//! frame that first lays it out hands them on, as if they had just been typed. A window keeps
//! nothing back once its content arrived: what comes after is the content's own business.
//!
//! A key something in the window takes (a chord the window's framework answers, Tab moving the
//! keyboard) is not held: only what nothing took waits. A press of the pointer is not held: what it
//! lands on is what the person sees, and over a placeholder that is nothing a press could mean.

use std::collections::VecDeque;

use winit::event::{Ime, Modifiers, WindowEvent};
use winit::keyboard::{Key, NamedKey};

use crate::widget::{AwaitsContent, Widget, capability::capability};

/// The input waiting for the window's content, and whether that content arrived once.
#[derive(Debug, Default)]
pub(crate) struct HeldInput {
    queue: VecDeque<(WindowEvent, Modifiers)>,
    /// The content stood once: nothing is held from then on.
    arrived: bool,
    /// The window stands in for an app still on its way to it (a launch's loading window): it holds
    /// everything until it gives its place, and hands what it holds to the app that takes it.
    stands_in: bool,
}

impl HeldInput {
    /// Whether `event` is input that may wait for the content: a key that is more than a modifier
    /// (the modifiers held go with the key they modify), or what an input method commits or
    /// composes.
    fn waits(event: &WindowEvent) -> bool {
        match event {
            WindowEvent::KeyboardInput { event, .. } => !matches!(
                event.logical_key,
                Key::Named(
                    NamedKey::Control
                        | NamedKey::Shift
                        | NamedKey::Alt
                        | NamedKey::Super
                        | NamedKey::Meta
                        | NamedKey::Hyper
                        | NamedKey::AltGraph
                )
            ),
            WindowEvent::Ime(Ime::Commit(_) | Ime::Preedit(..)) => true,
            _ => false,
        }
    }

    /// Whether `event` takes its place behind input already waiting: it may not overtake it.
    pub(crate) fn behind(&self, event: &WindowEvent) -> bool {
        !self.queue.is_empty() && Self::waits(event)
    }

    /// Whether `event` might wait if nothing takes it: the window may still hold input.
    pub(crate) fn may_keep(&self, event: &WindowEvent) -> bool {
        (self.stands_in || !self.arrived) && Self::waits(event)
    }

    /// Queue `event`, pressed with `modifiers`: behind what waits already, or because nothing in
    /// the window `root` took it while its content is on its way. Whether it was queued.
    pub(crate) fn keep(
        &mut self,
        root: &dyn Widget,
        event: WindowEvent,
        modifiers: Modifiers,
    ) -> bool {
        if !self.queue.is_empty() || self.holding(root) {
            self.queue.push_back((event, modifiers));
            return true;
        }
        false
    }

    /// Whether the window holds input now: it stands in for an app, or its content has not arrived
    /// yet. Looking settles the second for good once the content stands.
    fn holding(&mut self, root: &dyn Widget) -> bool {
        if self.stands_in {
            return true;
        }
        if self.arrived {
            return false;
        }
        if awaits_content(root) {
            return true;
        }
        self.arrived = true;
        false
    }

    /// What waited, in order, once the content of `root` stands; nothing while it does not.
    pub(crate) fn release(&mut self, root: &dyn Widget) -> Vec<(WindowEvent, Modifiers)> {
        if self.queue.is_empty() || self.holding(root) {
            return Vec::new();
        }
        self.queue.drain(..).collect()
    }

    /// The window stands in for an app on its way ([`Self::stands_in`]).
    pub(crate) fn stand_in(&mut self) {
        self.stands_in = true;
    }

    /// What the window that stood in held, for the app that takes its place.
    pub(crate) fn hand_on(&mut self) -> VecDeque<(WindowEvent, Modifiers)> {
        std::mem::take(&mut self.queue)
    }

    /// Take what the window that stood in for this app held, ahead of anything typed since.
    pub(crate) fn take_over(&mut self, mut held: VecDeque<(WindowEvent, Modifiers)>) {
        held.append(&mut self.queue);
        self.queue = held;
    }
}

/// Whether `widget`, or a widget under it, stands for content that has not arrived yet.
fn awaits_content(widget: &dyn Widget) -> bool {
    if capability::<dyn AwaitsContent>(widget).is_some_and(AwaitsContent::awaits_content) {
        return true;
    }
    let mut found = false;
    widget.for_each_child(&mut |child| {
        found = found || awaits_content(child);
    });
    found
}

impl super::App {
    /// Queue `event` behind input already waiting for the window's content, handing it back when
    /// nothing waits ([`HeldInput`]).
    pub(super) fn queue_behind_held_input(&mut self, event: WindowEvent) -> Option<WindowEvent> {
        if !self.input.held.behind(&event) {
            return Some(event);
        }
        let modifiers = self.input.modifiers;
        self.input
            .held
            .keep(self.root_widget.as_ref(), event, modifiers);
        None
    }

    /// A copy of `event` to keep if nothing takes it while the window's content is on its way.
    pub(super) fn held_input_candidate(&self, event: &WindowEvent) -> Option<WindowEvent> {
        self.input.held.may_keep(event).then(|| event.clone())
    }

    /// Keep `event`, which nothing in the window took, when the window's content is still on its
    /// way. Whether it was kept.
    pub(super) fn keep_unhandled_input(&mut self, event: WindowEvent) -> bool {
        let modifiers = self.input.modifiers;
        self.input
            .held
            .keep(self.root_widget.as_ref(), event, modifiers)
    }

    /// Hand the input that waited for the window's content to it, in order, once it stands, each
    /// with the modifiers held as it was pressed. Whether anything was handed on: the surface draws
    /// again for it.
    pub(crate) fn release_held_input(&mut self, surface: &dyn super::AppSurface) -> bool {
        let held = self.input.held.release(self.root_widget.as_ref());
        if held.is_empty() {
            return false;
        }
        let now = self.input.modifiers;
        for (event, modifiers) in held {
            self.input.modifiers = modifiers;
            self.surface_input(surface, event, false);
        }
        self.input.modifiers = now;
        surface.request_redraw();
        true
    }

    /// This window stands in for an app still on its way to it: what is typed in it waits for that
    /// app ([`Self::hand_on_held_input`]).
    pub(crate) fn stand_in_for_arriving_app(&mut self) {
        self.input.held.stand_in();
    }

    /// What this window held while it stood in for the app that takes its place.
    #[cfg(feature = "native")]
    pub(crate) fn hand_on_held_input(&mut self) -> VecDeque<(WindowEvent, Modifiers)> {
        self.input.held.hand_on()
    }

    /// Take what the window that stood in for this app held.
    #[cfg(feature = "native")]
    pub(crate) fn take_over_held_input(&mut self, held: VecDeque<(WindowEvent, Modifiers)>) {
        self.input.held.take_over(held);
    }
}
