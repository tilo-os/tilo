//! Wire-level tests for transient toplevels: a toplevel with an xdg parent
//! is a sheet on its parent's tile, never a tab of its own.

use vello::kurbo::Rect;
use wayland_client::protocol::{wl_compositor, wl_keyboard, wl_seat, wl_surface};
use wayland_client::{Connection, Dispatch, Proxy, QueueHandle, delegate_noop};
use wayland_protocols::xdg::dialog::v1::client::{xdg_dialog_v1, xdg_wm_dialog_v1};
use wayland_protocols::xdg::shell::client::{xdg_toplevel, xdg_wm_base};

use shell::compositor::foreign::{PointRoute, store};

#[derive(Default)]
struct Client {
    globals: Vec<(u32, String, u32)>,
    keyboard_enter: Option<wl_surface::WlSurface>,
    toplevel_configures: Vec<(u32, i32, i32)>,
    closed: Vec<u32>,
    pressed_keys: Vec<u32>,
}

crate::support::wayland::test_client!(Client);

impl Dispatch<wl_keyboard::WlKeyboard, ()> for Client {
    fn event(
        state: &mut Self,
        _: &wl_keyboard::WlKeyboard,
        event: wl_keyboard::Event,
        (): &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            wl_keyboard::Event::Enter { surface, .. } => state.keyboard_enter = Some(surface),
            wl_keyboard::Event::Key {
                key,
                state: wayland_client::WEnum::Value(wl_keyboard::KeyState::Pressed),
                ..
            } => state.pressed_keys.push(key),
            _ => {}
        }
    }
}

impl Dispatch<xdg_toplevel::XdgToplevel, ()> for Client {
    fn event(
        state: &mut Self,
        toplevel: &xdg_toplevel::XdgToplevel,
        event: xdg_toplevel::Event,
        (): &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        let id = toplevel.id().protocol_id();
        match event {
            xdg_toplevel::Event::Configure { width, height, .. } => {
                state.toplevel_configures.push((id, width, height));
            }
            xdg_toplevel::Event::Close => state.closed.push(id),
            _ => {}
        }
    }
}

delegate_noop!(Client: ignore wl_compositor::WlCompositor);
delegate_noop!(Client: ignore wl_surface::WlSurface);
delegate_noop!(Client: ignore wl_seat::WlSeat);
delegate_noop!(Client: ignore xdg_wm_dialog_v1::XdgWmDialogV1);
delegate_noop!(Client: ignore xdg_dialog_v1::XdgDialogV1);

type Harness = crate::support::wayland::Harness<Client>;

fn foreign_tabs(state: &shell::compositor::state::State) -> Vec<(u64, String)> {
    let tree = state.workspace_tree.read();
    tree.desktop
        .workspaces
        .iter()
        .flat_map(|workspace| workspace.strips.iter())
        .flat_map(|strip| strip.window_groups.iter())
        .flat_map(|group| group.tabs.iter())
        .filter_map(|tab| match &tab.tile {
            shell::shell::state::TileContent::Foreign { app_id, surface_id } => {
                Some((*surface_id, app_id.clone()))
            }
            _ => None,
        })
        .collect()
}

fn opaque(w: u32, h: u32) -> Vec<u8> {
    vec![255; usize::try_from(w.saturating_mul(h).saturating_mul(4)).expect("frame size")]
}

#[allow(
    clippy::too_many_lines,
    reason = "one ordered client/server transcript from dialog open to parent close"
)]
#[test]
fn a_modal_dialog_is_a_sheet_on_its_parent_tile_and_owns_its_input() {
    let mut h = Harness::new();
    let compositor = h.bind::<wl_compositor::WlCompositor>("wl_compositor", 4);
    let wm = h.bind::<xdg_wm_base::XdgWmBase>("xdg_wm_base", 3);
    let dialogs = h.bind::<xdg_wm_dialog_v1::XdgWmDialogV1>("xdg_wm_dialog_v1", 1);
    let seat = h.bind::<wl_seat::WlSeat>("wl_seat", 9);
    let _keyboard = seat.get_keyboard(&h.queue.handle(), ());
    let qh = h.queue.handle();

    let parent = compositor.create_surface(&qh, ());
    let parent_xdg = wm.get_xdg_surface(&parent, &qh, ());
    let parent_top = parent_xdg.get_toplevel(&qh, ());
    parent_top.set_app_id("org.example.Editor".into());
    parent.commit();
    h.pump();
    let tabs = foreign_tabs(&h.state);
    assert_eq!(tabs.len(), 1, "the parent is a tab");
    let parent_id = tabs[0].0;

    // What GTK sends for "Save changes?": a toplevel, its parent, the modal
    // hint, then the initial commit.
    let child = compositor.create_surface(&qh, ());
    let child_xdg = wm.get_xdg_surface(&child, &qh, ());
    let child_top = child_xdg.get_toplevel(&qh, ());
    child_top.set_app_id("org.example.Editor".into());
    child_top.set_parent(Some(&parent_top));
    let dialog = dialogs.get_xdg_dialog(&child_top, &qh, ());
    dialog.set_modal();
    child.commit();
    h.pump();

    assert_eq!(
        foreign_tabs(&h.state)
            .into_iter()
            .map(|(id, _)| id)
            .collect::<Vec<_>>(),
        vec![parent_id],
        "a dialog never becomes a tab of its own"
    );
    let sheets = store().sheets_over(parent_id);
    assert_eq!(sheets.len(), 1, "the dialog is a sheet on its parent");
    let child_id = sheets[0].surface;
    assert!(sheets[0].modal, "xdg_dialog_v1.set_modal reached the sheet");

    // The sheet sizes itself: its initial configure carries no size.
    shell::compositor::foreign::flush_configures(&mut h.state);
    h.pump();
    let child_proto = child_top.id().protocol_id();
    assert!(
        h.client
            .toplevel_configures
            .iter()
            .any(|&(id, w, hgt)| id == child_proto && w == 0 && hgt == 0),
        "the sheet was configured to choose its own size: {:?}",
        h.client.toplevel_configures
    );

    // Frames as the capture path publishes them, and the parent's tile.
    store().publish_rgba(parent_id, &opaque(800, 600), 800, 600);
    store().publish_rgba(child_id, &opaque(300, 120), 300, 120);
    let tile = Rect::new(100.0, 80.0, 900.0, 680.0);
    store().note_placement(parent_id, tile);

    // The rect the control socket's state dump reports for the sheet.
    let rects = store().sheet_rects(parent_id);
    assert_eq!(rects.len(), 1);
    let (link, sheet_rect) = rects[0];
    assert_eq!(link.surface, child_id);
    assert!(
        sheet_rect.x0 >= tile.x0
            && sheet_rect.y0 >= tile.y0
            && sheet_rect.x1 <= tile.x1
            && sheet_rect.y1 <= tile.y1,
        "the sheet {sheet_rect:?} sits inside its parent's tile {tile:?}"
    );
    assert_eq!(sheet_rect.center(), tile.center(), "and is centred on it");

    // Modal: the dimmed parent gets nothing, the sheet gets its own clicks.
    assert_eq!(
        store().route_point(parent_id, 5.0, 5.0),
        PointRoute::Blocked
    );
    let local = store()
        .sheet_rect_in_root(parent_id, child_id)
        .expect("sheet rect in the parent's frame");
    assert!(matches!(
        store().route_point(parent_id, local.x0 + 4.0, local.y0 + 4.0),
        PointRoute::Surface { surface, .. } if surface == child_id
    ));

    // Focusing the tile types into the sheet.
    h.state.set_client_keyboard_focus(Some(parent_id));
    h.pump();
    assert_eq!(h.state.client_input.keyboard_focus(), Some(child_id));
    assert_eq!(
        h.client.keyboard_enter.as_ref().map(Proxy::id),
        Some(child.id()),
        "the client saw keyboard enter on the dialog"
    );

    // A remote viewer addresses the streamed parent, while the seat enters
    // its modal child. Publish the real frame's view and deliver a key through
    // the production firewall: the parent share must still reach its sheet.
    let mut fb = shell::compositor::frame::FrameBuilder::with_tree(
        h.state.workspace_tree.read().clone(),
        (1000, 800),
    );
    fb.relayout();
    shell::compositor::client_input::publish_remote_input_view(&mut h.state, &fb, false);
    drop(fb);
    assert!(
        h.state
            .remote_input_firewall
            .view()
            .rect(parent_id)
            .is_some(),
        "the published view must observe the parent tile"
    );
    h.state
        .share_registry
        .insert(9, parent_id, shell::network::StreamingShare::new());
    assert!(h.state.apply_remote_input(
        9,
        parent_id,
        shell::network::InputEvent::KeyboardFocus {
            surface: parent_id,
            focused: true
        }
    ));
    assert!(
        h.state.apply_remote_input(
            9,
            parent_id,
            shell::network::InputEvent::Key {
                keysym: 30,
                pressed: true,
                modifiers: 0
            }
        ),
        "the focused modal child accepts a key addressed to its streamed parent"
    );
    assert!(h.state.apply_remote_input(
        9,
        parent_id,
        shell::network::InputEvent::Key {
            keysym: 30,
            pressed: false,
            modifiers: 0
        }
    ));
    h.pump();
    assert_eq!(h.client.pressed_keys, vec![30]);
    assert_eq!(
        h.client.keyboard_enter.as_ref().map(Proxy::id),
        Some(child.id())
    );

    // Dropping the modal hint gives the parent its pointer back.
    dialog.unset_modal();
    h.pump();
    assert_eq!(store().route_point(parent_id, 5.0, 5.0), PointRoute::Root);

    // Closing the parent asks the dialog to close too; until it does, it is
    // a window of its own rather than an unreachable one.
    parent_top.destroy();
    parent_xdg.destroy();
    parent.destroy();
    h.pump();
    assert!(
        h.client.closed.contains(&child_proto),
        "the dialog was asked to close with its parent"
    );
    assert!(!store().is_sheet(child_id));
    assert_eq!(
        foreign_tabs(&h.state)
            .into_iter()
            .map(|(id, _)| id)
            .collect::<Vec<_>>(),
        vec![child_id]
    );
}

/// The strip state a dialog must leave alone: which workspace, strip and
/// group are active (what the viewport follows) and how many groups exist.
fn strip_view(state: &shell::compositor::state::State) -> String {
    let tree = state.workspace_tree.read();
    let workspace = tree.desktop.active;
    let strip = tree.desktop.active_strip.get(&workspace).copied();
    let group = strip.and_then(|strip| tree.desktop.active_group_for(strip));
    let groups: usize = tree
        .desktop
        .workspaces
        .iter()
        .flat_map(|ws| ws.strips.iter())
        .map(|strip| strip.window_groups.len())
        .sum();
    let view = format!("{workspace:?} {strip:?} {group:?} {groups}");
    drop(tree);
    view
}

/// What a host-service prompt (gcr-prompter's keyring unlock) sends: no
/// parent, no dialog hint, the id announced after `get_toplevel`.
#[test]
fn a_parentless_system_prompt_is_a_sheet_and_leaves_the_strip_alone() {
    let mut h = Harness::new();
    let compositor = h.bind::<wl_compositor::WlCompositor>("wl_compositor", 4);
    let wm = h.bind::<xdg_wm_base::XdgWmBase>("xdg_wm_base", 3);
    let qh = h.queue.handle();

    let mut keep = Vec::new();
    for app in ["org.example.Editor", "org.example.Browser"] {
        let surface = compositor.create_surface(&qh, ());
        let xdg = wm.get_xdg_surface(&surface, &qh, ());
        let top = xdg.get_toplevel(&qh, ());
        top.set_app_id(app.into());
        surface.commit();
        h.pump();
        keep.push((surface, xdg, top));
    }
    let tabs = foreign_tabs(&h.state);
    assert_eq!(tabs.len(), 2);
    let focused = tabs[1].0;
    h.state.set_client_keyboard_focus(Some(focused));
    h.pump();

    let before = strip_view(&h.state);

    let prompt = compositor.create_surface(&qh, ());
    let prompt_xdg = wm.get_xdg_surface(&prompt, &qh, ());
    let prompt_top = prompt_xdg.get_toplevel(&qh, ());
    prompt_top.set_app_id("gcr-prompter".into());
    prompt.commit();
    h.pump();

    assert_eq!(
        foreign_tabs(&h.state).len(),
        2,
        "a system prompt is never a tab"
    );
    assert_eq!(
        strip_view(&h.state),
        before,
        "the strip did not change or pan"
    );
    let sheets = store().sheets_over(focused);
    assert_eq!(
        sheets.len(),
        1,
        "the prompt is a sheet over the focused window"
    );
    drop(keep);
}

#[test]
fn a_system_prompt_with_no_window_to_sit_on_is_a_system_modal_overlay() {
    let mut h = Harness::new();
    h.state.output_size = (1000, 800);
    let compositor = h.bind::<wl_compositor::WlCompositor>("wl_compositor", 4);
    let wm = h.bind::<xdg_wm_base::XdgWmBase>("xdg_wm_base", 3);
    let qh = h.queue.handle();
    let before = strip_view(&h.state);

    let prompt = compositor.create_surface(&qh, ());
    let prompt_xdg = wm.get_xdg_surface(&prompt, &qh, ());
    let prompt_top = prompt_xdg.get_toplevel(&qh, ());
    prompt_top.set_app_id("gcr-prompter".into());
    prompt.commit();
    h.pump();

    assert!(foreign_tabs(&h.state).is_empty(), "never a tab");
    assert_eq!(strip_view(&h.state), before, "the strip did not move");
    assert!(h.state.has_system_modal());
    let id = h.state.top_system_modal().expect("the prompt");
    assert!(!store().is_sheet(id));
    assert!(
        h.state.is_transient_surface(id),
        "painted and hit above the desktop"
    );

    store().publish_rgba(id, &opaque(300, 200), 300, 200);
    assert_eq!(
        h.state.system_modal_surfaces(),
        vec![(id, Rect::new(350.0, 300.0, 650.0, 500.0))],
        "centred on the output"
    );
}

/// Beside a 2x laptop panel, a 1x monitor: a parentless system prompt
/// opens centred on the display the person is working on, never on the
/// seam between the two.
#[test]
fn a_system_modal_opens_on_the_focused_display() {
    let outputs = shell::compositor::backend::headless::output_fixture("laptop-beside-1080p")
        .expect("fixture");
    let mut h = Harness::new();
    h.state.set_output_layout(&outputs);
    let (width, height) = shell::compositor::backend::logical_geometry(&outputs);
    h.state.output_size = (
        i32::try_from(width).expect("width"),
        i32::try_from(height).expect("height"),
    );
    let displays: Vec<shell::shell::state::Display> = outputs
        .iter()
        .map(|o| {
            let r = o.logical_rect();
            shell::shell::state::Display {
                id: o.id.0,
                monitor: format!("output:{}", o.id.0),
                x: r.x0,
                y: r.y0,
                width: r.width(),
                height: r.height(),
            }
        })
        .collect();
    h.state.workspace_tree.write().reconcile_displays(&displays);
    let compositor = h.bind::<wl_compositor::WlCompositor>("wl_compositor", 4);
    let wm = h.bind::<xdg_wm_base::XdgWmBase>("xdg_wm_base", 3);
    let qh = h.queue.handle();

    let prompt = compositor.create_surface(&qh, ());
    let prompt_xdg = wm.get_xdg_surface(&prompt, &qh, ());
    let prompt_top = prompt_xdg.get_toplevel(&qh, ());
    prompt_top.set_app_id("gcr-prompter".into());
    prompt.commit();
    h.pump();
    let id = h.state.top_system_modal().expect("the prompt");
    store().publish_rgba(id, &opaque(300, 200), 300, 200);

    for output in outputs.iter().rev() {
        h.state
            .workspace_tree
            .write()
            .desktop
            .outputs
            .set_focused(output.id.0);
        let display = output.logical_rect();
        let placed = h.state.system_modal_surfaces();
        assert_eq!(placed.len(), 1);
        let (_, rect) = placed[0];
        assert_eq!(
            rect.center(),
            display.center(),
            "the prompt {rect:?} is centred on the focused display {display:?}"
        );
    }
}

#[test]
fn modal_and_fixed_size_dialogs_that_name_no_parent_are_sheets() {
    let mut h = Harness::new();
    h.state.output_size = (1000, 800);
    let compositor = h.bind::<wl_compositor::WlCompositor>("wl_compositor", 4);
    let wm = h.bind::<xdg_wm_base::XdgWmBase>("xdg_wm_base", 3);
    let dialogs = h.bind::<xdg_wm_dialog_v1::XdgWmDialogV1>("xdg_wm_dialog_v1", 1);
    let qh = h.queue.handle();

    let window = compositor.create_surface(&qh, ());
    let window_xdg = wm.get_xdg_surface(&window, &qh, ());
    let window_top = window_xdg.get_toplevel(&qh, ());
    window_top.set_app_id("org.example.Editor".into());
    window.commit();
    h.pump();
    let host = foreign_tabs(&h.state)[0].0;
    h.state.set_client_keyboard_focus(Some(host));
    let before = strip_view(&h.state);

    // xdg_dialog_v1 modal, no parent, an app id nobody listed.
    let modal = compositor.create_surface(&qh, ());
    let modal_xdg = wm.get_xdg_surface(&modal, &qh, ());
    let modal_top = modal_xdg.get_toplevel(&qh, ());
    modal_top.set_app_id("org.example.Whatever".into());
    dialogs.get_xdg_dialog(&modal_top, &qh, ()).set_modal();
    modal.commit();
    h.pump();

    // A fixed-size window: min == max.
    let fixed = compositor.create_surface(&qh, ());
    let fixed_xdg = wm.get_xdg_surface(&fixed, &qh, ());
    let fixed_top = fixed_xdg.get_toplevel(&qh, ());
    fixed_top.set_app_id("org.example.Other".into());
    fixed_top.set_min_size(320, 200);
    fixed_top.set_max_size(320, 200);
    fixed.commit();
    h.pump();
    shell::compositor::foreign::flush_configures(&mut h.state);
    h.pump();

    assert_eq!(foreign_tabs(&h.state).len(), 1, "neither dialog is a tab");
    assert_eq!(strip_view(&h.state), before, "the strip did not move");
    assert_eq!(
        store().sheets_over(host).len(),
        2,
        "both are sheets on the focused window"
    );
}
