//! Driving a real `ShellOverlayHost` (or any one widget) the way `App`
//! does: build, compute and apply its layout against a fixed viewport, then
//! route winit events through `handle_event`. Without the layout pass every
//! hit rect is `Rect::ZERO` and no event lands anywhere.

use std::collections::HashMap;

use shell::shell::overlay_host::ShellOverlayHost;
use shell::shell::state::{FreshId as _, WorkspaceId, WorkspaceTree};
use taffy::{AvailableSpace, Dimension, NodeId, Size, Style, TaffyTree};
use ui::theme::Theme;
use ui::widget::Widget;
use vello::kurbo::Point;
use winit::event::{ElementState, Modifiers, MouseButton, WindowEvent};
use winit::keyboard::{Key, ModifiersState};

/// A laid-out widget: the tree and the widget's own node in it, which is
/// what every `EventCtx` names.
pub struct Layout {
    pub taffy: TaffyTree<()>,
    pub child_node: NodeId,
}

/// `build_layout -> compute_layout -> apply_layout` inside a `width` x
/// `height` root, so every rect lands in screen-space coords.
pub fn lay_out<W: Widget + ?Sized>(host: &mut W, width: f32, height: f32) -> Layout {
    let theme = Theme::dark();
    let mut taffy: TaffyTree<()> = TaffyTree::new();
    let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
    let child = host.build_layout(&mut taffy, &theme, &mut node_map);
    let root_node = taffy
        .new_with_children(
            Style {
                size: Size {
                    width: Dimension::length(width),
                    height: Dimension::length(height),
                },
                ..Default::default()
            },
            &[child],
        )
        .unwrap();
    let mut tr = ui::text_render::TextRenderer::new();
    taffy
        .compute_layout_with_measure(
            root_node,
            Size {
                width: AvailableSpace::Definite(width),
                height: AvailableSpace::Definite(height),
            },
            |known, available, node_id, _, _| {
                if let Some(&ptr) = node_map.get(&node_id) {
                    unsafe { (*ptr).measure(known, available, &mut tr) }
                } else {
                    taffy::Size::ZERO
                }
            },
        )
        .expect("compute_layout");
    host.apply_layout(&taffy, child, Point::ORIGIN);
    Layout {
        taffy,
        child_node: child,
    }
}

/// [`lay_out`] at the 1600x900 viewport the chrome-overlay tests share,
/// as the `(taffy, host node)` pair their event helpers take.
pub fn layout_host(host: &mut ShellOverlayHost) -> (TaffyTree<()>, NodeId) {
    let Layout { taffy, child_node } = lay_out(host, 1600.0, 900.0);
    (taffy, child_node)
}

/// Route one event through the widget against `doc`; returns whether it
/// was consumed. Callers that type into CRDT-backed inputs keep `doc`
/// across events, or every character but the last is lost.
pub fn dispatch_in<W: Widget + ?Sized>(
    doc: &mut automerge::AutoCommit,
    host: &mut W,
    taffy: &TaffyTree<()>,
    node: NodeId,
    modifiers: Modifiers,
    event: &WindowEvent,
) -> bool {
    let mut handled = false;
    let mut focus_target: Option<NodeId> = None;
    host.handle_event(
        event,
        &mut ui::widget::EventCtx {
            scale_factor: 1.0,
            modifiers: &modifiers,
            state: ui::widget::Edits::lend(doc),
            path: &automerge::ROOT,
            handled: &mut handled,
            focus_target: &mut focus_target,
            node_id: node,
            taffy,
        },
    );
    handled
}

/// [`dispatch_in`] against a fresh document.
pub fn dispatch<W: Widget + ?Sized>(
    host: &mut W,
    taffy: &TaffyTree<()>,
    node: NodeId,
    modifiers: Modifiers,
    event: &WindowEvent,
) -> bool {
    dispatch_in(
        &mut automerge::AutoCommit::new(),
        host,
        taffy,
        node,
        modifiers,
        event,
    )
}

/// Route a pressed logical key through the host dispatcher; returns
/// whether the host consumed it.
pub fn press_key(
    host: &mut ShellOverlayHost,
    taffy: &TaffyTree<()>,
    node: NodeId,
    key: Key,
    modifiers: ModifiersState,
) -> bool {
    let event = ui::synthetic_input::keyboard_input(ui::synthetic_input::key_event(
        key,
        None,
        ElementState::Pressed,
    ));
    dispatch(host, taffy, node, Modifiers::from(modifiers), &event)
}

/// Drive a whole click — `CursorMoved`, `Pressed`, `Released` — at `at`.
/// The release matters: sliders hold their value until button-up (so a
/// drag doesn't spawn one process call per frame), while toggles dispatch
/// on press.
pub fn click(host: &mut ShellOverlayHost, taffy: &TaffyTree<()>, node: NodeId, at: Point) {
    let mut doc = automerge::AutoCommit::new();
    let moved = ui::synthetic_input::cursor_moved(at.x, at.y);
    dispatch_in(&mut doc, host, taffy, node, Modifiers::default(), &moved);
    for state in [ElementState::Pressed, ElementState::Released] {
        let button = ui::synthetic_input::mouse_input(state, MouseButton::Left);
        dispatch_in(&mut doc, host, taffy, node, Modifiers::default(), &button);
    }
}

/// Paint one frame so retained chrome geometry matches what a person sees.
pub fn paint(host: &mut ShellOverlayHost) {
    host.draw(
        &mut vello::Scene::new(),
        vello::kurbo::Affine::IDENTITY,
        &mut ui::text_render::TextRenderer::new(),
        &Theme::dark(),
        &ui::presence::PresenceCtx::empty(),
    );
}

/// One empty workspace, active — enough for chrome that never looks at
/// workspace content.
pub fn one_workspace_tree() -> WorkspaceTree {
    let ws = shell::shell::state::Workspace::new(WorkspaceId::new(), "W");
    let active = ws.id;
    WorkspaceTree {
        desktop: shell::shell::state::DesktopTree {
            workspaces: vec![ws],
            active,
            ..Default::default()
        },
        ..Default::default()
    }
}
