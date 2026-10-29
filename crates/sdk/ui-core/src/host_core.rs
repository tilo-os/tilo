//! The app host core: what the native [`crate::app::App`] and the headless
//! [`crate::headless::FrameHarness`] do alike, written once. Each host keeps
//! its own fields and lends the parts this core reads through [`HostCore`].

use std::collections::HashMap;

use taffy::{NodeId, TaffyTree};

use crate::history_mode::embedded::{PendingCommand, PendingCommandOutcome};
use crate::history_mode::pending_controls::{
    ContinuationCommand, PendingControlStatus, PreparedContinuation,
};
use crate::standalone_history::{HistoryError, HistoryReadContext, HistoryTarget};
use crate::standalone_history_host::{HostHistoryLayout, StandaloneHistoryHost};
use crate::widget::Widget;

/// A host's document and laid-out widget tree, borrowed for one read.
pub(crate) struct HostTree<'a> {
    pub(crate) state: &'a crate::State,
    pub(crate) root: &'a dyn Widget,
    /// The app's registered framework History, when it has one.
    pub(crate) history: Option<&'a dyn crate::history_mode::host::HistoryChromeApi>,
    pub(crate) bindings: &'a crate::bound_dispatch::BoundNodeMap,
    pub(crate) node_map: &'a HashMap<NodeId, *mut dyn Widget>,
    pub(crate) taffy: &'a TaffyTree<()>,
    pub(crate) records: Option<&'a crate::record::RecordSet>,
    pub(crate) permissions: &'a crate::permissions::PermissionContext,
    pub(crate) focused: Option<NodeId>,
    pub(crate) history_layout: Option<HostHistoryLayout>,
    /// The laid-out root, once the host built its layout.
    pub(crate) root_node: Option<NodeId>,
    pub(crate) pointer: &'a PointerPlace,
}

impl<'a> HostTree<'a> {
    pub(crate) fn history_context(&self) -> HistoryReadContext<'a> {
        HistoryReadContext::of(self.state, self.records, self.permissions)
    }

    /// The document History opens on: the focused component's, else the
    /// root's, else the first bound component laid out, else the host's own.
    pub(crate) fn capture_history_target(&self) -> Result<HistoryTarget, HistoryError> {
        crate::standalone_history_host::capture_history_target(
            self.focused
                .and_then(|node| self.bindings.binding_for(node))
                .or_else(|| self.root.callback_binding()),
            self.bindings,
            self.node_map.keys().copied(),
            self.records,
            self.state.document_identity(),
        )
    }

    /// Whether `node` belongs to the standalone History chrome rather than
    /// the app.
    pub(crate) fn is_history_node(&self, node: NodeId) -> bool {
        self.history_layout
            .is_some_and(|layout| layout.contains(self.taffy, node))
    }

    /// Whether the app already presents History of its own: a registered
    /// framework History, or a root (a hosted component, a registered
    /// framework app) that supplies a shared History surface. The standalone
    /// History chrome serves only the others; wrapping such a root as well
    /// would stack two History toolbars and squeeze the app's layout.
    pub(crate) fn supplies_own_history(&self) -> bool {
        self.history.is_some()
            || crate::history_mode::embedded::has_surfaces(self.root)
            || crate::history_mode::capability::provides_document_history(self.root)
    }

    /// The window a palette is asked for from, when the root lays several
    /// out (the desktop's strip of app tiles): the tile under the pointer
    /// that pressed, else the one holding keyboard focus. `None` for a lone
    /// app, whose window is the whole surface
    /// ([`crate::action_palette::invoking_window`]).
    pub(crate) fn invoking_window_rect(&self) -> Option<vello::kurbo::Rect> {
        crate::action_palette::invoking_window(
            self.taffy,
            self.root_node,
            self.pointer.pressed_at,
            self.focused,
            |node| {
                self.inspect_node(node, crate::action_palette::tile_rect)
                    .ok()
                    .flatten()
            },
        )
    }

    /// The control the person acted on last, in window coordinates: under
    /// their last press, else the one holding the keyboard.
    pub(crate) fn acting_control_rect(&self) -> Option<vello::kurbo::Rect> {
        crate::control_notice::acting_control(
            self.taffy,
            self.root_node,
            self.pointer.pressed_at,
            self.focused,
            |node| {
                self.inspect_node(node, |widget| {
                    Some((widget.get_rect(), widget.is_focusable()))
                })
                .ok()
                .flatten()
            },
        )
    }

    /// Read the widget laid out at `node`, through its component binding.
    pub(crate) fn inspect_node<R>(
        &self,
        node: NodeId,
        callback: impl FnOnce(&dyn Widget) -> R,
    ) -> Result<R, crate::bound_dispatch::BoundDispatchError> {
        // SAFETY: the host owns this stable tree; no target reference escapes.
        unsafe {
            crate::bound_dispatch::inspect_bound_node(self.bindings, self.node_map, node, callback)
        }
    }

    /// Whether a laid-out widget claims raw per-finger touch at `point`
    /// (logical coords); see [`Widget::claims_raw_touch`].
    pub(crate) fn claims_raw_touch_at(&self, point: vello::kurbo::Point) -> bool {
        self.node_map.keys().any(|&node| {
            self.inspect_node(node, |widget| {
                widget.get_rect().contains(point) && widget.claims_raw_touch(point)
            })
            .unwrap_or(false)
        })
    }

    /// What a contact beginning at `point` (logical coords) means: raw for a
    /// widget that reads fingers itself, else what the widget under it
    /// claims ([`crate::widget::Widget::touch_claim`]), else ordinary pointer
    /// emulation.
    pub(crate) fn touch_claim_at(
        &self,
        point: vello::kurbo::Point,
    ) -> crate::interaction::TouchClaim {
        use crate::interaction::TouchClaim;
        if self.claims_raw_touch_at(point) {
            return TouchClaim::Raw;
        }
        self.node_map
            .keys()
            .find_map(|&node| {
                self.inspect_node(node, |widget| {
                    if widget.get_rect().contains(point) {
                        widget.touch_claim(point)
                    } else {
                        TouchClaim::Pointer
                    }
                })
                .ok()
                .filter(|claim| *claim != TouchClaim::Pointer)
            })
            .unwrap_or(TouchClaim::Pointer)
    }

    /// Tab normally belongs to the host's focus order. A focused editor-like
    /// widget may explicitly claim it first (for indentation or completion),
    /// without broadening that exception to arbitrary widgets or escaping an
    /// active focus trap.
    pub(crate) fn focused_widget_wants_tab_input(&self) -> bool {
        self.focused.is_some_and(|node| {
            self.inspect_node(node, |widget| widget.wants_tab_input())
                .unwrap_or(false)
        })
    }

    /// `order` kept inside the keyboard scope: the focused desktop window's
    /// content ([`crate::widget::Widget::scopes_keyboard_focus`]).
    pub(crate) fn within_keyboard_scope(&self, order: Vec<NodeId>) -> Vec<NodeId> {
        let scopes: Vec<NodeId> = self
            .node_map
            .keys()
            .copied()
            .filter(|&node| {
                self.inspect_node(node, |widget| widget.scopes_keyboard_focus())
                    .unwrap_or(false)
            })
            .collect();
        crate::focus_navigation::within_keyboard_scope(order, &scopes, self.taffy)
    }

    /// Where focus lands when History opens: its timeline, else the first
    /// focusable History control in `order`.
    pub(crate) fn history_focus_target(&self, order: &[NodeId]) -> Option<NodeId> {
        order
            .iter()
            .copied()
            .find(|&node| {
                self.is_history_node(node)
                    && self
                        .inspect_node(node, |widget| widget.name() == "ActivityTimeline")
                        .unwrap_or(false)
            })
            .or_else(|| {
                order
                    .iter()
                    .copied()
                    .find(|&node| self.is_history_node(node))
            })
    }
}

/// A host's document beside its bindings, borrowed for one write through a
/// laid-out widget.
pub(crate) struct HostDocument<'a> {
    pub(crate) state: &'a mut crate::State,
    pub(crate) bindings: &'a crate::bound_dispatch::BoundNodeMap,
    pub(crate) node_map: &'a HashMap<NodeId, *mut dyn Widget>,
    pub(crate) permissions: &'a crate::permissions::PermissionContext,
    pub(crate) author: &'a crate::permissions::UserId,
}

pub(crate) trait HostCore {
    /// The host's document and laid-out tree.
    fn tree(&self) -> HostTree<'_>;

    /// The host's document, writable, beside its bindings.
    fn document(&mut self) -> HostDocument<'_>;

    /// Something is about to reach a laid-out widget, which may change what
    /// the host presents next.
    fn widget_reached(&mut self);

    /// The standalone History host beside the tree it presents.
    fn document_history_with_tree(&mut self) -> (&mut StandaloneHistoryHost, HostTree<'_>);

    /// The live app root, for History surfaces it embeds.
    fn host_root_mut(&mut self) -> &mut dyn Widget;

    /// The app's registered framework History, when it has one.
    fn registered_history_mut(&mut self) -> Option<&mut crate::history_mode::host::HistoryChrome>;

    /// Run a pending History control whose meaning
    /// [`Self::dispatch_prepared_history_control`] found unchanged.
    fn dispatch_current_history_control(
        &mut self,
        prepared: PreparedContinuation,
    ) -> Result<crate::widget::ResourceOperationStart, tilo_actions::DoorError>;

    fn with_history<R>(
        &mut self,
        visit: impl FnOnce(&mut StandaloneHistoryHost, &HistoryReadContext<'_>) -> R,
    ) -> R {
        let (host, tree) = self.document_history_with_tree();
        visit(host, &tree.history_context())
    }

    fn inspect_node<R>(
        &self,
        node: NodeId,
        callback: impl FnOnce(&dyn Widget) -> R,
    ) -> Result<R, crate::bound_dispatch::BoundDispatchError> {
        self.tree().inspect_node(node, callback)
    }

    fn mutate_node<R>(
        &mut self,
        node: NodeId,
        callback: impl FnOnce(&mut dyn Widget) -> R,
    ) -> Result<R, crate::bound_dispatch::BoundDispatchError> {
        self.widget_reached();
        let document = self.document();
        // SAFETY: exclusive host access prevents overlapping target references.
        unsafe {
            crate::bound_dispatch::mutate_bound_node(
                document.bindings,
                document.node_map,
                node,
                callback,
            )
        }
    }

    fn dispatch_node<R>(
        &mut self,
        node: NodeId,
        callback: impl FnOnce(&mut dyn Widget, &mut automerge::AutoCommit, &automerge::ObjId) -> R,
    ) -> Result<R, crate::bound_dispatch::BoundDispatchError> {
        self.widget_reached();
        let document = self.document();
        // SAFETY: the exclusive host owns both maps and the stable live tree.
        unsafe {
            crate::bound_dispatch::invoke_bound_node(
                document.bindings,
                document.node_map,
                node,
                &mut document.state.doc,
                &document.state.root,
                callback,
            )
        }
    }

    fn dispatch_semantic_node<R>(
        &mut self,
        node: NodeId,
        action: &crate::a11y::Action,
        callback: impl FnOnce(&mut dyn Widget, &mut automerge::AutoCommit, &automerge::ObjId) -> R,
    ) -> Result<R, String> {
        self.widget_reached();
        let document = self.document();
        // SAFETY: the host exclusively owns the unchanged admitted tree.
        unsafe {
            crate::bound_dispatch::invoke_bound_semantic_node(
                document.bindings,
                document.node_map,
                node,
                action,
                document.state,
                crate::bound_dispatch::SemanticCaller {
                    authority: document.permissions,
                    author: document.author,
                },
                callback,
            )
        }
    }

    fn touch_claim_at(&self, point: vello::kurbo::Point) -> crate::interaction::TouchClaim {
        self.tree().touch_claim_at(point)
    }

    fn focused_widget_wants_tab_input(&self) -> bool {
        self.tree().focused_widget_wants_tab_input()
    }

    #[cfg(any(feature = "native", test))]
    fn capture_history_target(&self) -> Result<HistoryTarget, HistoryError> {
        self.tree().capture_history_target()
    }

    fn is_history_node(&self, node: NodeId) -> bool {
        self.tree().is_history_node(node)
    }

    /// Note what History would open on and where focus is in the app, unless
    /// focus is in History itself.
    fn remember_history_content(&mut self) {
        let (host, tree) = self.document_history_with_tree();
        if tree.focused.is_some_and(|node| tree.is_history_node(node)) {
            return;
        }
        let target = tree.capture_history_target();
        let focus = crate::widget::layout::CompositionFocus::capture_live(tree.root);
        host.remember_content_target(target, focus);
    }

    /// Run a control History held back while it was open, refused when its
    /// meaning or confirmation policy changed since.
    fn dispatch_prepared_history_control(
        &mut self,
        prepared: PreparedContinuation,
    ) -> Result<crate::widget::ResourceOperationStart, tilo_actions::DoorError> {
        let captured =
            prepared
                .witness
                .as_ref()
                .map_err(|reason| tilo_actions::DoorError::Unavailable {
                    code: "stale_pending_control".into(),
                    reason: reason.clone(),
                })?;
        let tree = self.tree();
        let current = crate::bound_dispatch::control_witness(
            tree.root,
            &tree.state.doc,
            &prepared.target,
            &prepared.invocation,
        )?;
        if &current != captured {
            return Err(tilo_actions::DoorError::Unavailable {
                code: "stale_pending_control".into(),
                reason: "The action meaning or confirmation policy changed while History was open"
                    .into(),
            });
        }
        self.dispatch_current_history_control(prepared)
    }

    /// Run a control the registered History held back and record how it
    /// ended there: done, refused, or (unless the person already confirmed)
    /// waiting for confirmation. `None` once the History is gone.
    fn settle_history_control(
        &mut self,
        prepared: PreparedContinuation,
        confirmed: bool,
    ) -> Option<PendingControlStatus> {
        let id = prepared.id;
        let result = self.dispatch_prepared_history_control(prepared);
        let history = self.registered_history_mut()?;
        Some(match result {
            Err(tilo_actions::DoorError::ConfirmationRequired) if !confirmed => {
                history.pending_control_needs_confirmation(id);
                PendingControlStatus::NeedsConfirmation
            }
            Ok(_) => {
                history.pending_control_consumed(id);
                PendingControlStatus::Consumed
            }
            Err(error) => {
                let reason = error.to_string();
                history.pending_control_refused(id, reason.clone());
                PendingControlStatus::Refused(reason)
            }
        })
    }

    /// Serve what History surfaces embedded in the app asked of their
    /// held-back controls: review, confirm or cancel each.
    fn service_embedded_history_commands(&mut self, embedded: Vec<PendingCommand>) {
        use crate::history_mode::embedded as surfaces;
        for pending in embedded {
            let (token, confirmation) = match pending.command {
                ContinuationCommand::Review(token) => (token, None),
                ContinuationCommand::Confirm(token, value) => (token, Some(value)),
                ContinuationCommand::Cancel(token) => {
                    surfaces::cancel_pending_command(self.host_root_mut(), pending.surface, &token);
                    continue;
                }
            };
            let prepared = match confirmation.as_deref() {
                Some(value) => surfaces::prepare_pending_command_typed(
                    self.host_root_mut(),
                    pending.surface,
                    &token,
                    value,
                ),
                None => surfaces::prepare_pending_command(
                    self.host_root_mut(),
                    pending.surface,
                    &token,
                    false,
                ),
            };
            let Some(prepared) = prepared else {
                continue;
            };
            let confirmed = confirmation.is_some();
            let id = prepared.id;
            let outcome = match self.dispatch_prepared_history_control(prepared) {
                Err(tilo_actions::DoorError::ConfirmationRequired) if !confirmed => {
                    PendingCommandOutcome::NeedsConfirmation
                }
                Ok(_) => PendingCommandOutcome::Consumed,
                Err(error) => PendingCommandOutcome::Refused(error.to_string()),
            };
            surfaces::finish_pending_command(self.host_root_mut(), pending.surface, id, &outcome);
        }
    }
}

/// Where the person acts with the pointer, followed through each input event
/// a host receives.
#[derive(Default)]
pub(crate) struct PointerPlace {
    /// The pointer buttons held down, one bit each. Motion with a button
    /// held is a drag, which may move what a widget declares (a selection, a
    /// split) without marking each move handled, so it is never inert.
    pub(crate) buttons: u32,
    /// Where the pointer is, in logical pixels of the host's surface.
    pub(crate) at: Option<vello::kurbo::Point>,
    /// Where the person last pressed, while a press (not a key) is the last
    /// thing they did: what they pressed is in the window there, which is the
    /// window that asked for whatever the press opens.
    pub(crate) pressed_at: Option<vello::kurbo::Point>,
}

impl PointerPlace {
    /// Follow which pointer buttons are down.
    pub(crate) fn track_buttons(&mut self, event: &winit::event::WindowEvent) {
        use winit::event::{ElementState, MouseButton, WindowEvent};
        if let WindowEvent::MouseInput { state, button, .. } = event {
            let bit = match button {
                MouseButton::Left => 1,
                MouseButton::Right => 1 << 1,
                MouseButton::Middle => 1 << 2,
                MouseButton::Back => 1 << 3,
                MouseButton::Forward => 1 << 4,
                MouseButton::Other(n) => 1 << (5 + u32::from(*n) % 27),
            };
            match state {
                ElementState::Pressed => self.buttons |= bit,
                ElementState::Released => self.buttons &= !bit,
            }
        }
    }

    /// Follow where the person acts: the pointer's place, and where the last
    /// press landed until a key is pressed. `scale` turns the event's
    /// physical pixels into the surface's logical ones. Whether the person
    /// acted anew: a press, a touch or a key.
    pub(crate) fn track_place(&mut self, event: &winit::event::WindowEvent, scale: f64) -> bool {
        use winit::event::{ElementState, TouchPhase, WindowEvent};
        let logical = |x: f64, y: f64| vello::kurbo::Point::new(x / scale, y / scale);
        match event {
            WindowEvent::CursorMoved { position, .. } => {
                self.at = Some(logical(position.x, position.y));
                false
            }
            WindowEvent::CursorLeft { .. } => {
                self.at = None;
                false
            }
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                ..
            } => {
                self.pressed_at = self.at;
                true
            }
            WindowEvent::Touch(touch) if touch.phase == TouchPhase::Started => {
                let at = logical(touch.location.x, touch.location.y);
                self.at = Some(at);
                self.pressed_at = Some(at);
                true
            }
            WindowEvent::KeyboardInput { event: key, .. } if key.state == ElementState::Pressed => {
                self.pressed_at = None;
                true
            }
            _ => false,
        }
    }

    /// Whether `event` is pointer motion with no button held: hover.
    pub(crate) fn is_bare_motion(&self, event: &winit::event::WindowEvent) -> bool {
        use winit::event::WindowEvent;
        match event {
            WindowEvent::CursorMoved { .. } => self.buttons == 0,
            WindowEvent::CursorEntered { .. } | WindowEvent::CursorLeft { .. } => true,
            _ => false,
        }
    }

    /// Input that activates nothing by itself: hover, a key's release, a
    /// modifier key's press, a modifier change and a wheel. Unless something
    /// consumed it, it keeps the published tree when nothing the tree depends
    /// on moved. A key's release followed every keystroke and dropped the
    /// tree its press had just republished; a wheel at the end of a list,
    /// which no scroller takes, dropped it at every notch.
    pub(crate) fn is_inert(&self, event: &winit::event::WindowEvent) -> bool {
        use winit::event::{ElementState, WindowEvent};
        use winit::keyboard::{Key, NamedKey};
        match event {
            WindowEvent::KeyboardInput { event, .. } => {
                event.state == ElementState::Released
                    || matches!(
                        event.logical_key,
                        Key::Named(
                            NamedKey::Shift
                                | NamedKey::Control
                                | NamedKey::Alt
                                | NamedKey::AltGraph
                                | NamedKey::Super
                                | NamedKey::Meta
                                | NamedKey::Hyper
                        )
                    )
            }
            WindowEvent::ModifiersChanged(_) | WindowEvent::MouseWheel { .. } => true,
            _ => self.is_bare_motion(event),
        }
    }
}
