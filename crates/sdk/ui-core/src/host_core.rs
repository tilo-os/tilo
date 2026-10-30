//! The app host core: what the native [`crate::app::App`] and the headless
//! [`crate::headless::FrameHarness`] do alike, written once. Each host keeps
//! its own fields and lends the parts this core reads through [`HostCore`].

use std::collections::HashMap;

use taffy::{NodeId, TaffyTree};

use crate::history_mode::embedded::{PendingCommand, PendingCommandOutcome};
use crate::history_mode::pending_controls::{
    ContinuationCommand, PendingControlStatus, PreparedContinuation,
};
use crate::widget::Widget;

mod controls;
mod keys;
mod tooltips;

pub(crate) use controls::{ControlHost, json_argument};
pub(crate) use keys::{KeyHost, RootEvent};
pub(crate) use tooltips::TooltipLayer;

/// A host's document and laid-out widget tree, borrowed for one read.
pub(crate) struct HostTree<'a> {
    pub(crate) state: &'a crate::State,
    pub(crate) root: &'a dyn Widget,
    /// The app's registered framework History, when it has one.
    pub(crate) history: Option<&'a dyn crate::history_mode::host::HistoryChromeApi>,
    pub(crate) bindings: &'a crate::bound_dispatch::BoundNodeMap,
    pub(crate) node_map: &'a HashMap<NodeId, *mut dyn Widget>,
    pub(crate) taffy: &'a TaffyTree<()>,
    pub(crate) focused: Option<NodeId>,
    /// The laid-out root, once the host built its layout.
    pub(crate) root_node: Option<NodeId>,
    pub(crate) pointer: &'a PointerPlace,
}

impl HostTree<'_> {
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

    /// Whether `node` is History's: its chrome, or an embedded History
    /// surface's.
    pub(crate) fn history_owns(&self, node: NodeId) -> bool {
        crate::history_mode::embedded::owns_node(self.root, node, self.taffy)
            || self
                .history
                .is_some_and(|history| history.owns_node(node, self.taffy))
    }

    /// The presented action palette's laid-out node: while it is up, the
    /// whole focus order.
    pub(crate) fn action_palette_node(&self) -> Option<NodeId> {
        self.node_map.keys().copied().find(|&node| {
            self.inspect_node(node, |widget| widget.test_id() == Some("action-palette"))
                .unwrap_or(false)
        })
    }

    /// What embedded History surfaces allow of the focus order.
    pub(crate) fn focus_restrictions(&self) -> crate::history_mode::embedded::FocusRestrictions {
        crate::history_mode::embedded::FocusRestrictions::collect(self.root, self.taffy)
    }

    /// The keyboard focus order of the app laid out at `app_node`, and
    /// whether a focus trap holds it: read off the layout maps where History
    /// (open for review, it is walked instead), an embedded History surface
    /// or a hosted component lays out nodes the widget walk cannot reach,
    /// else walked over the widget tree.
    pub(crate) fn focus_order_under(
        &self,
        app_node: NodeId,
        restrictions: &crate::history_mode::embedded::FocusRestrictions,
    ) -> (Vec<NodeId>, bool) {
        // Focus travels the laid out tree: a widget can lay its children out
        // under nodes of its own (a shell's columns), and a component or
        // History lays out nodes no widget lists as a child, so a walk of the
        // widgets beside the layout pairs controls with the wrong nodes.
        let root = self
            .history
            .and_then(|history| history.review_layout_node())
            .unwrap_or(app_node);
        self.layout_map_focus_order(root, restrictions)
    }

    /// The focus order under `root` read off the layout maps, as History
    /// and embedded History surfaces need it, and whether a trap holds it.
    pub(crate) fn layout_map_focus_order(
        &self,
        root: NodeId,
        restrictions: &crate::history_mode::embedded::FocusRestrictions,
    ) -> (Vec<NodeId>, bool) {
        // One sweep asks every mapped node, entering each component's scope
        // once rather than once a node.
        let sweep = std::cell::RefCell::new(crate::bound_dispatch::NodeSweep::new(
            self.bindings,
            self.node_map,
        ));
        crate::focus_navigation::layout_map_order(
            root,
            self.taffy,
            restrictions,
            self.history,
            |node| {
                // SAFETY: the host owns this stable tree; no target reference
                // escapes.
                unsafe {
                    sweep
                        .borrow_mut()
                        .inspect(node, crate::focus_navigation::probe)
                }
                .ok()
            },
        )
    }

    /// The control of `order` that takes focus when content resets it: the
    /// one the content marks as its default (so Enter takes the encouraged
    /// action), else the first.
    pub(crate) fn default_focus(&self, order: &[NodeId]) -> Option<NodeId> {
        order
            .iter()
            .copied()
            .find(|&node| {
                self.inspect_node(node, |widget| widget.is_default_keyboard_focus())
                    .unwrap_or(false)
            })
            .or_else(|| order.first().copied())
    }

    /// Whether the app's registered History is still valid over its root.
    pub(crate) fn validate_history_registration(
        &self,
    ) -> Result<(), crate::history_mode::HistoryRegistrationError> {
        let history = self
            .history
            .ok_or(crate::history_mode::HistoryRegistrationError::Missing)?;
        history.validate()?;
        history.validate_root(self.root)
    }

    /// What the laid-out widgets offer at `point` (logical coords) for the
    /// pointer's shape ([`crate::widget::cursor`]).
    pub(crate) fn cursor_pick_at(
        &self,
        point: vello::kurbo::Point,
    ) -> crate::widget::cursor::CursorPick {
        let mut pick = crate::widget::cursor::CursorPick::default();
        for &node in self.node_map.keys() {
            let _ = self.inspect_node(node, |widget| pick.offer(widget, point));
        }
        pick
    }

    /// Every tooltip a laid-out widget offers at `point` (logical coords),
    /// with the side of its control it prefers.
    pub(crate) fn tooltips_at(
        &self,
        point: vello::kurbo::Point,
    ) -> Vec<(
        vello::kurbo::Rect,
        String,
        crate::widget::tooltip::TooltipSide,
    )> {
        self.node_map
            .keys()
            .filter_map(|&node| {
                self.inspect_node(node, |widget| {
                    crate::widget::tooltip::placed_target_at(widget, point)
                })
                .ok()
                .flatten()
            })
            .collect()
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
    /// and so may a widget around the focus that walks its own controls (a
    /// modal sheet with its own focus model), without broadening that
    /// exception to arbitrary widgets or escaping an active focus trap. With
    /// nothing focused, the root is asked.
    pub(crate) fn focused_widget_wants_tab_input(&self) -> bool {
        let mut node = self.focused.or(self.root_node);
        while let Some(at) = node {
            if self
                .inspect_node(at, |widget| widget.wants_tab_input())
                .unwrap_or(false)
            {
                return true;
            }
            node = self.taffy.parent(at);
        }
        false
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
}

/// A host's document beside its bindings, borrowed for one write through a
/// laid-out widget.
pub(crate) struct HostDocument<'a> {
    pub(crate) state: &'a mut crate::State,
    pub(crate) bindings: &'a crate::bound_dispatch::BoundNodeMap,
    pub(crate) node_map: &'a HashMap<NodeId, *mut dyn Widget>,
}

/// One invocation a host hands a door: who asks, with what argument, under
/// which confirmation, invocation and context.
#[derive(Clone, Copy)]
pub(crate) struct ResourceInvocation<'a> {
    pub(crate) address: &'a tilo_actions::ActionAddress,
    pub(crate) actor: &'a str,
    pub(crate) argument: Option<&'a str>,
    pub(crate) confirmation_satisfied: bool,
    pub(crate) broker_admitted: bool,
    pub(crate) confirmation_evidence: Option<&'a serde_json::Value>,
    pub(crate) invocation: Option<tilo_actions::InvocationId>,
    pub(crate) ambient_context: Option<&'a tilo_actions::ActionContext>,
    pub(crate) scope: Option<&'a crate::invocation_scope::ResourceInvocationScope>,
}

impl<'a> ResourceInvocation<'a> {
    pub(crate) fn new(address: &'a tilo_actions::ActionAddress, actor: &'a str) -> Self {
        Self {
            address,
            actor,
            argument: None,
            confirmation_satisfied: false,
            broker_admitted: false,
            confirmation_evidence: None,
            invocation: None,
            ambient_context: None,
            scope: None,
        }
    }

    pub(crate) fn argument(mut self, argument: Option<&'a str>) -> Self {
        self.argument = argument;
        self
    }

    pub(crate) fn confirmed(mut self, confirmed: bool) -> Self {
        self.confirmation_satisfied = confirmed;
        self
    }

    pub(crate) fn broker_admitted(mut self, admitted: bool) -> Self {
        self.broker_admitted = admitted;
        self
    }

    pub(crate) fn evidence(mut self, evidence: Option<&'a serde_json::Value>) -> Self {
        self.confirmation_evidence = evidence;
        self
    }

    pub(crate) fn invocation(mut self, invocation: Option<tilo_actions::InvocationId>) -> Self {
        self.invocation = invocation;
        self
    }

    pub(crate) fn ambient_context(
        mut self,
        context: Option<&'a tilo_actions::ActionContext>,
    ) -> Self {
        self.ambient_context = context;
        self
    }

    pub(crate) fn scope(
        mut self,
        scope: Option<&'a crate::invocation_scope::ResourceInvocationScope>,
    ) -> Self {
        self.scope = scope;
        self
    }
}

/// A host's laid-out tree, writable: what letting go of nodes touches.
pub(crate) struct HostLayout<'a> {
    pub(crate) taffy: &'a mut TaffyTree<()>,
    pub(crate) node_map: &'a mut HashMap<NodeId, *mut dyn Widget>,
    pub(crate) bindings: &'a mut crate::bound_dispatch::BoundNodeMap,
    pub(crate) focused: &'a mut Option<NodeId>,
}

/// What moving keyboard focus between the app and History touches.
pub(crate) struct HostFocus<'a> {
    pub(crate) root: &'a mut dyn Widget,
    pub(crate) history: Option<&'a mut crate::history_mode::host::HistoryChrome>,
    pub(crate) node_map: &'a HashMap<NodeId, *mut dyn Widget>,
    pub(crate) bindings: &'a crate::bound_dispatch::BoundNodeMap,
    pub(crate) taffy: &'a TaffyTree<()>,
    pub(crate) focused: &'a mut Option<NodeId>,
}

/// What laying a host's widgets out again in place touches.
pub(crate) struct HostRemount<'a> {
    pub(crate) root: &'a mut dyn Widget,
    pub(crate) state: &'a crate::State,
    pub(crate) theme: &'a crate::theme::Theme,
    pub(crate) taffy: &'a mut TaffyTree<()>,
    pub(crate) node_map: &'a mut HashMap<NodeId, *mut dyn Widget>,
    pub(crate) bindings: &'a mut crate::bound_dispatch::BoundNodeMap,
    pub(crate) close_reports: &'a mut Vec<crate::application::ComponentCloseReport>,
    /// How the person reaches the window: what the remount mounts hears it.
    pub(crate) interaction: crate::interaction::InteractionEnvironment,
}

pub(crate) trait HostCore {
    /// The host's document and laid-out tree.
    fn tree(&self) -> HostTree<'_>;

    /// The host's document, writable, beside its bindings.
    fn document(&mut self) -> HostDocument<'_>;

    /// Every tree the layout maps can name: the app root and what the host
    /// lays out around and over it.
    fn visit_layout_roots(&self, visit: &mut dyn FnMut(&dyn Widget));

    /// The widgets the host shows, for the lookups that must see what a
    /// container places itself (a desktop's strip of windows, a window's
    /// bar) and so stays out of the layout maps: the app while it is live,
    /// and what stands over it.
    fn presentation_roots(&self) -> Vec<&dyn Widget>;

    /// Whether what drives the pointer can hover at all (a touch screen
    /// cannot).
    fn pointer_hovers(&self) -> bool;

    /// The tooltip the laid-out surface offers at logical `point`: the
    /// smallest-area widget there offering one, with the side of its control
    /// it prefers (so a nested icon button beats its enclosing toolbar).
    fn tooltip_target_at(
        &self,
        point: vello::kurbo::Point,
    ) -> Option<(
        vello::kurbo::Rect,
        String,
        crate::widget::tooltip::TooltipSide,
    )> {
        if !self.pointer_hovers() {
            return None;
        }
        let mut targets = self.tree().tooltips_at(point);
        for root in self.presentation_roots() {
            tooltips::targets_under(root, point, &mut targets);
        }
        crate::widget::tooltip::smallest_target(targets)
    }

    /// The walk the layout maps were admitted against.
    fn adopted_layout_mut(&mut self) -> &mut crate::retained_widgets::Adopted;

    /// The laid-out tree, writable.
    fn layout_mut(&mut self) -> HostLayout<'_>;

    /// Forget the layout maps and build the layout again before anything
    /// reads them.
    fn discard_layout_maps(&mut self);

    /// The maps were just admitted.
    fn layout_admitted(&mut self) {}

    /// Whether a widget replaced children since the maps were admitted, so
    /// they no longer describe the live tree.
    fn composition_replaced(&mut self) -> bool;

    /// Right after a build registered widgets in the maps, before any
    /// widget code runs: find each of them in the live tree.
    fn admit_layout_maps(&mut self) {
        self.layout_admitted();
        let mut maps = std::mem::take(self.adopted_layout_mut());
        let tree = self.tree();
        // SAFETY: every pointer not yet adopted was registered by the build
        // that just ran on these trees, and no widget code ran since.
        unsafe {
            maps.admit(
                tree.node_map,
                &|visit| self.visit_layout_roots(visit),
                tree.taffy,
            );
        }
        *self.adopted_layout_mut() = maps;
    }

    /// Whether the maps name a widget that is no longer in the tree: a
    /// widget replaced children without reporting it. The maps are then
    /// forgotten and the layout built again, so nothing reads them first.
    fn layout_maps_stale(&mut self) -> bool {
        if self.tree().node_map.is_empty() {
            return false;
        }
        let maps = std::mem::take(self.adopted_layout_mut());
        let mut relaid = Vec::new();
        let live = maps.still_live(&|visit| self.visit_layout_roots(visit), &mut relaid);
        *self.adopted_layout_mut() = maps;
        if live {
            let layout = self.layout_mut();
            let removed =
                crate::retained_widgets::let_go_under(&relaid, layout.taffy, layout.node_map);
            layout.bindings.forget(&removed);
            if layout.focused.is_some_and(|node| removed.contains(&node)) {
                *layout.focused = None;
            }
            return false;
        }
        self.discard_layout_maps();
        true
    }

    /// Where focus is, as an identity that survives a rebuild. The mapped
    /// tree is read only while it still is the live composition; once a
    /// widget replaced children since the maps were admitted, the focused
    /// control is found in the live tree instead.
    fn capture_focus_identity(&mut self) -> Option<crate::widget::layout::CompositionFocus> {
        let replaced = self.composition_replaced();
        let tree = self.tree();
        if replaced || tree.node_map.is_empty() {
            return crate::widget::layout::CompositionFocus::capture_live(tree.root);
        }
        // SAFETY: no widget was replaced since the maps were admitted, so
        // they describe this exclusively owned live tree.
        unsafe {
            crate::widget::layout::CompositionFocus::capture(
                tree.focused,
                tree.node_map,
                tree.bindings,
            )
        }
    }

    /// What laying the widgets out again in place touches.
    fn remount_parts(&mut self) -> HostRemount<'_>;

    /// A remount is about to be tried: whether the host's laid-out tree
    /// allows one (it is built, and nothing else asked for a rebuild).
    fn begin_remount(&mut self) -> bool;

    /// The remount mounted `built` fresh nodes and admitted them.
    fn remounted(&mut self, built: usize);

    /// Whether the composition changed since the maps were admitted (and
    /// the layout must be built again).
    fn composition_dirty(&mut self) -> bool;

    /// Carry out the change the root's widgets make in place
    /// ([`crate::widget::StructureChange::Children`]) in the retained tree:
    /// mount what they added, unmount what they let go of, keep the rest.
    /// `false` when the change needs the whole tree built again instead:
    /// History is not live, a widget did not carry it out, or it set off
    /// another.
    fn remount_in_place(&mut self) -> bool {
        // History lays out its own chrome around the app; while it is live,
        // the app's widgets change only what is theirs.
        if !self.begin_remount()
            || self.tree().history.is_some_and(|history| {
                !history.live_interactive() || history.returning_live() || history.review_is_open()
            })
        {
            return false;
        }
        let parts = self.remount_parts();
        // Components a widget replaced report their close here, as before a
        // rebuild.
        parts
            .close_reports
            .extend(crate::component_lifecycle::take_component_close_reports(
                parts.root,
            ));
        // What a widget made since the input last changed (a view a press
        // opened) hears how the person reaches the window before it is laid
        // out; a widget that already has it keeps it.
        parts.root.set_interaction_environment(parts.interaction);
        let mapped = parts.node_map.len();
        let mut remount = crate::widget::Remount::new(parts.taffy, parts.theme, parts.node_map);
        let applied = parts.root.remount_children(&mut remount);
        let (mounted, unmounted) = remount.finish();
        let built = (parts.node_map.len() + unmounted.len()).saturating_sub(mapped);
        // The rows it mounted are checked against the live tree at once, as
        // a build's are, so the next structure check finds the maps live.
        self.admit_layout_maps();
        let parts = self.remount_parts();
        // A change no widget carried out, or any other beside it, needs the
        // whole tree.
        if !applied || parts.root.take_structure_change() != crate::widget::StructureChange::None {
            return false;
        }
        parts.bindings.forget(&unmounted);
        for (parent, child) in mounted {
            // SAFETY: the host exclusively owns this tree, its maps and
            // widgets; the remount above admitted `child` into both.
            if !unsafe {
                parts
                    .bindings
                    .admit_mounted(parent, child, parts.taffy, parts.node_map)
            } {
                return false;
            }
        }
        // The mounted widgets are synced before they are measured, as a
        // rebuild syncs its fresh ones; a sync that replaces delegates needs
        // the whole tree after all.
        parts.root.sync(&parts.state.doc, &parts.state.root);
        self.remounted(built);
        !self.composition_dirty()
    }

    /// What moving focus between the app and History touches.
    fn focus_parts(&mut self) -> HostFocus<'_>;

    /// Focus left History's chrome for the live app, which History had
    /// saved focus for.
    fn history_focus_resumed(&mut self) {}

    /// Before focus moves to `target`: an embedded History surface lets go
    /// of it; leaving History's unopened chrome for the live app resumes the
    /// app (its controls focused before are told they are not); entering
    /// History suspends the live focus without a blur, so an unfinished
    /// editor keeps its caret and input state.
    fn suspend_history_focus(&mut self, target: Option<NodeId>) {
        let parts = self.focus_parts();
        // SAFETY: the host exclusively owns this admitted tree and its maps.
        if unsafe {
            crate::history_mode::embedded::move_focus(
                parts.root,
                target,
                *parts.focused,
                parts.node_map,
                parts.bindings,
                parts.taffy,
            )
        } {
            *parts.focused = None;
        }
        let taffy = parts.taffy;
        if let Some(history) = parts.history
            && !history.captures_input()
            && history.has_saved_focus()
            && target.is_some_and(|node| !history.owns_node(node, taffy))
        {
            history.discard_saved_focus();
            self.history_focus_resumed();
            let tree = self.tree();
            let outgoing: Vec<_> = tree
                .node_map
                .keys()
                .copied()
                .filter(|&node| {
                    Some(node) != target
                        && tree
                            .inspect_node(node, |widget| widget.is_focused())
                            .unwrap_or(false)
                })
                .collect();
            for node in outgoing {
                let _ = self.mutate_node(node, |widget| widget.set_focus(false));
            }
        }
        let parts = self.focus_parts();
        if let Some(history) = parts.history
            && target.is_some_and(|node| history.owns_node(node, parts.taffy))
            && parts
                .focused
                .is_some_and(|node| !history.owns_node(node, parts.taffy))
        {
            // SAFETY: as above.
            unsafe {
                history.preserve_focus(*parts.focused, parts.node_map, parts.bindings, parts.taffy);
            }
            *parts.focused = None;
        }
    }

    /// Something is about to reach a laid-out widget, which may change what
    /// the host presents next.
    fn widget_reached(&mut self);

    /// The live app root, for History surfaces it embeds.
    fn host_root_mut(&mut self) -> &mut dyn Widget;

    /// The app's registered framework History, when it has one.
    fn registered_history_mut(&mut self) -> Option<&mut crate::history_mode::host::HistoryChrome>;

    /// The effects running for their invokers, by invocation.
    fn acting_for_mut(
        &mut self,
    ) -> &mut HashMap<tilo_actions::InvocationId, crate::invocation_scope::Continuation>;

    /// Run a pending History control whose meaning
    /// [`Self::dispatch_prepared_history_control`] found unchanged.
    fn dispatch_current_history_control(
        &mut self,
        prepared: PreparedContinuation,
    ) -> Result<crate::widget::ResourceOperationStart, tilo_actions::DoorError>;

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

    /// Perform `action` on the widget laid out at `node`, writing through
    /// its binding. Whether the widget took it.
    fn perform_node_action(
        &mut self,
        node: NodeId,
        action: &crate::a11y::Action,
    ) -> Result<bool, crate::bound_dispatch::BoundDispatchError> {
        self.dispatch_node(node, |widget, doc, root| {
            widget.perform_action(action, &[], &mut crate::widget::Edits::lend(doc), root)
        })
    }

    /// Keep why an edit the running effect `on_behalf_of` submitted was
    /// refused: the effect then fails rather than claim what it did not do.
    fn note_continuation(
        &mut self,
        on_behalf_of: Option<tilo_actions::InvocationId>,
        result: &Result<crate::widget::ResourceOperationStart, tilo_actions::DoorError>,
    ) {
        let Err(error) = result else {
            return;
        };
        if let Some(continuation) = on_behalf_of.and_then(|id| self.acting_for_mut().get_mut(&id))
            && continuation.refused.is_none()
        {
            continuation.refused = Some(crate::invocation_scope::Continuation::failure(error));
        }
    }

    fn dispatch_semantic_node<R>(
        &mut self,
        node: NodeId,
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
                document.state,
                callback,
            )
        }
    }

    /// Perform `action` on an accessibility node the widget laid out at
    /// `node` exports: the widget itself, a child its painting ancestor
    /// routes to (`semantic_target`), or the synthetic element `key_path`
    /// names. Whether the widget took it.
    fn perform_exported_action(
        &mut self,
        node: NodeId,
        semantic_target: Option<&str>,
        key_path: &[u64],
        action: &crate::a11y::Action,
    ) -> Result<bool, String> {
        self.dispatch_semantic_node(node, |widget, doc, root| {
            let edits = &mut crate::widget::Edits::lend(doc);
            match semantic_target {
                Some(target) => {
                    crate::a11y_child_route::perform(widget, target, action, edits, root)
                }
                None => widget.perform_action(action, key_path, edits, root),
            }
        })
    }

    /// Perform `verb` on the semantic target `target` the widget laid out at
    /// `owner` publishes: a `tilo.key-action:` target names the element by
    /// its key path, any other is routed to the child it names. Whether the
    /// widget took it.
    fn perform_semantic_target(
        &mut self,
        owner: NodeId,
        target: &str,
        verb: &crate::a11y::Action,
    ) -> Result<bool, String> {
        let key_path = target
            .strip_prefix("tilo.key-action:")
            .and_then(|target| target.rsplit_once(':'))
            .map(|(_, path)| {
                path.split('/')
                    .map(str::parse::<u64>)
                    .collect::<Result<Vec<_>, _>>()
            })
            .transpose()
            .map_err(|error| format!("invalid keyed semantic action: {error}"))?;
        self.dispatch_node(owner, |widget, doc, root| {
            let edits = &mut crate::widget::Edits::lend(doc);
            match &key_path {
                Some(key_path) => widget.perform_action(verb, key_path, edits, root),
                None => crate::a11y_child_route::perform(widget, target, verb, edits, root),
            }
        })
        .map_err(|error| format!("component dispatch refused: {error:?}"))
    }

    /// Where an activation the widget left to the pointer presses: on the
    /// routed child where its painting ancestor stands it, else on the
    /// synthetic element `ordinal_path` names, else on the widget.
    fn exported_press_point(
        &self,
        node: NodeId,
        semantic_target: Option<&str>,
        ordinal_path: &[usize],
    ) -> vello::kurbo::Point {
        self.inspect_node(node, |widget| {
            semantic_target
                .and_then(|target| crate::a11y_child_route::rect(widget, target))
                .or_else(|| {
                    (!ordinal_path.is_empty())
                        .then(|| widget.synthetic_accessibility_rect(ordinal_path))
                        .flatten()
                })
                .unwrap_or_else(|| widget.get_rect())
        })
        .unwrap_or(vello::kurbo::Rect::ZERO)
        .center()
    }

    fn touch_claim_at(&self, point: vello::kurbo::Point) -> crate::interaction::TouchClaim {
        self.tree().touch_claim_at(point)
    }

    fn focused_widget_wants_tab_input(&self) -> bool {
        self.tree().focused_widget_wants_tab_input()
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

    /// History asked about its held-back controls: the tree it lays out is
    /// about to change under the host's maps.
    fn history_commands_arrived(&mut self);

    /// Carry out what the registered History and the History surfaces
    /// embedded in the app asked of their held-back controls: review,
    /// confirm or cancel each, while History still holds input. Whether
    /// anything asked.
    fn service_pending_history_commands(&mut self) -> bool {
        let commands = self
            .registered_history_mut()
            .map_or_else(Vec::new, |history| history.drain_pending_control_commands());
        let embedded = crate::history_mode::embedded::drain_pending_commands(self.host_root_mut());
        let asked = !commands.is_empty() || !embedded.is_empty();
        if asked {
            self.history_commands_arrived();
        }
        for command in commands {
            let (token, confirmation) = match command {
                ContinuationCommand::Review(token) => (token, None),
                ContinuationCommand::Confirm(token, value) => (token, Some(value)),
                ContinuationCommand::Cancel(token) => {
                    if let Some(history) = self.registered_history_mut() {
                        history.cancel_pending_control(&token);
                    }
                    continue;
                }
            };
            let Some(prepared) =
                self.registered_history_mut()
                    .and_then(|history| match confirmation.as_deref() {
                        Some(value) => history.prepare_pending_control_typed(&token, value),
                        None => history.prepare_pending_control(&token, false),
                    })
            else {
                continue;
            };
            self.settle_history_control(prepared, confirmation.is_some());
        }
        self.service_embedded_history_commands(embedded);
        asked
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
