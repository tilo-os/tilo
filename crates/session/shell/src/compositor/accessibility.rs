//! AT-SPI export for compositor-owned UI.
//!
//! Normal qOS applications are winit windows and use `ui::App`'s
//! `accesskit_winit` adapter. Native compositor chrome is painted straight to
//! an output and has no application window to attach that adapter to. This
//! bridge therefore owns AccessKit's Unix adapter directly, while retaining
//! the same asynchronous activation/action contract: AccessKit's worker thread
//! sends requests through a mailbox and only the compositor thread reads or
//! mutates the widget tree.

use std::sync::mpsc::{self, Receiver, Sender};

use ui::accesskit::{
    ActionHandler, ActionRequest, ActivationHandler, DeactivationHandler, Rect, TreeUpdate,
};

/// Work requested by an assistive technology. The native event loop drains
/// this before deciding whether another frame is needed.
#[derive(Debug)]
pub enum Request {
    /// AccessKit became active and needs a complete current tree.
    InitialTree,
    /// Invoke an action on one exported node.
    Action(ActionRequest),
    /// The last assistive client disconnected.
    Deactivated,
}

struct Activation {
    tx: Sender<Request>,
}

impl ActivationHandler for Activation {
    fn request_initial_tree(&mut self) -> Option<TreeUpdate> {
        // The widget tree is compositor-thread-owned. Returning `None` is the
        // documented asynchronous path; `pump` publishes the full tree during
        // this event-loop iteration (well before the next display refresh).
        let _ = self.tx.send(Request::InitialTree);
        None
    }
}

struct Actions {
    tx: Sender<Request>,
}

impl ActionHandler for Actions {
    fn do_action(&mut self, request: ActionRequest) {
        let _ = self.tx.send(Request::Action(request));
    }
}

struct Deactivation {
    tx: Sender<Request>,
}

impl DeactivationHandler for Deactivation {
    fn deactivate_accessibility(&mut self) {
        let _ = self.tx.send(Request::Deactivated);
    }
}

/// Production AT-SPI endpoint for the native shell.
pub struct NativeAccessibility {
    adapter: accesskit_unix::Adapter,
    rx: Receiver<Request>,
}

impl NativeAccessibility {
    pub fn new(viewport: (u32, u32)) -> Self {
        let (tx, rx) = mpsc::channel();
        let mut adapter = accesskit_unix::Adapter::new(
            Activation { tx: tx.clone() },
            Actions { tx: tx.clone() },
            Deactivation { tx },
        );
        adapter.update_window_focus_state(true);
        let bounds = Rect::new(0.0, 0.0, f64::from(viewport.0), f64::from(viewport.1));
        adapter.set_root_window_bounds(bounds, bounds);
        Self { adapter, rx }
    }

    /// Drain without blocking; callers can act on the live widget tree on the
    /// compositor thread and then publish one coherent post-action update.
    pub fn drain(&self) -> Vec<Request> {
        self.rx.try_iter().collect()
    }

    /// Publish only when AccessKit has an active consumer. The closure avoids
    /// cloning or building semantic nodes on desktops with no assistive client.
    pub fn update_if_active(&mut self, update: impl FnOnce() -> TreeUpdate) {
        self.adapter.update_if_active(update);
    }

    pub fn set_viewport(&mut self, viewport: (u32, u32)) {
        let bounds = Rect::new(0.0, 0.0, f64::from(viewport.0), f64::from(viewport.1));
        self.adapter.set_root_window_bounds(bounds, bounds);
    }

    /// Stop assistive clients from traversing stale desktop chrome while the
    /// separately-painted lock surface owns the display. Unlocking restores
    /// focus before the next complete tree update is published.
    pub fn set_window_focus(&mut self, focused: bool) {
        self.adapter.update_window_focus_state(focused);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn handlers_mailbox_activation_actions_and_deactivation() {
        let (tx, rx) = mpsc::channel();
        let mut activation = Activation { tx: tx.clone() };
        let mut actions = Actions { tx: tx.clone() };
        let mut deactivation = Deactivation { tx };

        assert!(activation.request_initial_tree().is_none());
        actions.do_action(ActionRequest {
            action: ui::accesskit::Action::Focus,
            target_tree: ui::accesskit::TreeId::ROOT,
            target_node: ui::accesskit::NodeId(42),
            data: None,
        });
        deactivation.deactivate_accessibility();

        assert!(matches!(rx.recv().unwrap(), Request::InitialTree));
        assert!(matches!(
            rx.recv().unwrap(),
            Request::Action(ActionRequest {
                target_node: ui::accesskit::NodeId(42),
                ..
            })
        ));
        assert!(matches!(rx.recv().unwrap(), Request::Deactivated));
    }
}
