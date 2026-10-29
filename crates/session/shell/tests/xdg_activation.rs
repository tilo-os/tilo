//! Wire-level presence and rejection tests for `xdg_activation_v1`.

use wayland_client::protocol::{wl_compositor, wl_keyboard, wl_seat, wl_shm, wl_surface, wl_touch};
use wayland_client::{Connection, Dispatch, QueueHandle, delegate_noop};
use wayland_protocols::xdg::activation::v1::client::{xdg_activation_token_v1, xdg_activation_v1};
use wayland_protocols::xdg::shell::client::{xdg_popup, xdg_positioner, xdg_wm_base};

use crate::support::wayland::{self, Toplevel, create_toplevel, foreign_id};

#[derive(Default)]
struct Client {
    globals: Vec<(u32, String, u32)>,
    completed_tokens: usize,
    token: Option<String>,
    keyboard_serial: Option<u32>,
    popup_configures: Vec<(i32, i32, i32, i32)>,
    repositioned: Vec<u32>,
    popup_done: usize,
}

crate::support::wayland::test_client!(Client: registry, ping, configure, surface, toplevel, shm, buffer, seat);

impl Dispatch<xdg_activation_token_v1::XdgActivationTokenV1, ()> for Client {
    fn event(
        state: &mut Self,
        _: &xdg_activation_token_v1::XdgActivationTokenV1,
        event: xdg_activation_token_v1::Event,
        (): &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let xdg_activation_token_v1::Event::Done { token } = event {
            state.completed_tokens = state.completed_tokens.saturating_add(1);
            state.token = Some(token);
        }
    }
}

impl Dispatch<wl_keyboard::WlKeyboard, ()> for Client {
    fn event(
        state: &mut Self,
        _: &wl_keyboard::WlKeyboard,
        event: wl_keyboard::Event,
        (): &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let wl_keyboard::Event::Enter { serial, .. } = event {
            state.keyboard_serial = Some(serial);
        }
    }
}

impl Dispatch<xdg_popup::XdgPopup, ()> for Client {
    fn event(
        state: &mut Self,
        _: &xdg_popup::XdgPopup,
        event: xdg_popup::Event,
        (): &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            xdg_popup::Event::Configure {
                x,
                y,
                width,
                height,
            } => {
                state.popup_configures.push((x, y, width, height));
            }
            xdg_popup::Event::Repositioned { token } => state.repositioned.push(token),
            xdg_popup::Event::PopupDone => state.popup_done = state.popup_done.saturating_add(1),
            _ => {}
        }
    }
}

delegate_noop!(Client: ignore xdg_activation_v1::XdgActivationV1);
delegate_noop!(Client: ignore wl_touch::WlTouch);
delegate_noop!(Client: ignore xdg_positioner::XdgPositioner);

type Harness = crate::support::wayland::Harness<Client>;

#[test]
#[serial_test::serial(process_env)]
fn activation_global_is_advertised_but_unattributed_tokens_are_not_authority() {
    let mut h = Harness::new();
    let manager = h.bind::<xdg_activation_v1::XdgActivationV1>("xdg_activation_v1", 1);
    let token = manager.get_activation_token(&h.queue.handle(), ());
    token.commit();
    h.pump();

    assert_eq!(
        h.client.completed_tokens, 1,
        "protocol commit must complete"
    );
    assert_eq!(
        h.state.xdg_activation_state.tokens().count(),
        0,
        "a token without seat, serial, and focused source must not enter the usable token store"
    );
}

#[test]
#[serial_test::serial(process_env)]
fn focused_serial_activates_matching_toplevel_once() {
    let mut h = Harness::new();
    let activation = h.bind::<xdg_activation_v1::XdgActivationV1>("xdg_activation_v1", 1);
    let compositor = h.bind::<wl_compositor::WlCompositor>("wl_compositor", 4);
    let wm = h.bind::<xdg_wm_base::XdgWmBase>("xdg_wm_base", 3);
    let seat = h.bind::<wl_seat::WlSeat>("wl_seat", 9);
    let _keyboard = seat.get_keyboard(&h.queue.handle(), ());
    let qh = h.queue.handle();

    let Toplevel {
        surface: source,
        xdg: _,
        toplevel: _,
    } = create_toplevel(&compositor, &wm, &qh, "org.example.Source");
    let Toplevel {
        surface: target,
        xdg: _,
        toplevel: _,
    } = create_toplevel(&compositor, &wm, &qh, "org.example.Target");
    h.pump();

    let source_id = foreign_id(&h.state, "org.example.Source");
    let target_id = foreign_id(&h.state, "org.example.Target");
    assert!(shell::shell::tab_binding::activate_surface(
        &mut h.state.workspace_tree.write(),
        source_id
    ));
    h.state.set_client_keyboard_focus(Some(source_id));
    h.pump();
    let serial = h.client.keyboard_serial.expect("keyboard enter serial");

    let token = activation.get_activation_token(&qh, ());
    token.set_serial(serial, &seat);
    token.set_surface(&source);
    token.set_app_id("org.example.Target".into());
    token.commit();
    h.pump();
    let token_text = h.client.token.clone().expect("activation token");
    assert_eq!(h.state.xdg_activation_state.tokens().count(), 1);

    activation.activate(token_text.clone(), &target);
    h.pump();
    assert_eq!(
        shell::compositor::client_input::active_foreign_surface(&h.state.workspace_tree.read()),
        Some(target_id)
    );
    assert_eq!(h.state.client_input.keyboard_focus(), Some(target_id));
    assert_eq!(h.state.xdg_activation_state.tokens().count(), 0);

    assert!(shell::shell::tab_binding::activate_surface(
        &mut h.state.workspace_tree.write(),
        source_id
    ));
    activation.activate(token_text, &target);
    h.pump();
    assert_eq!(
        shell::compositor::client_input::active_foreign_surface(&h.state.workspace_tree.read()),
        Some(source_id),
        "a consumed token must not be replayable"
    );
}

// The host-launch path mints the token on the compositor thread, while the
// child merely inherits its opaque standard value.  Exercise that wire path
// with a real Wayland client: initial adoption, activation, placement/title
// reconciliation, focus, single-use removal, and the later-window cleanup.
#[test]
#[serial_test::serial(process_env)]
fn compositor_minted_host_token_relocates_and_retires_its_launch_note() {
    shell::shell::launcher::clear_pending_placements();
    let mut h = Harness::new();
    let activation = h.bind::<xdg_activation_v1::XdgActivationV1>("xdg_activation_v1", 1);
    let compositor = h.bind::<wl_compositor::WlCompositor>("wl_compositor", 4);
    let wm = h.bind::<xdg_wm_base::XdgWmBase>("xdg_wm_base", 3);
    let identity = "org.example.HostToken";
    let desktop_id = "org.example.HostToken.desktop";
    // This is the pre-existing peer/document tab a host launch must adopt,
    // rather than assuming the test harness's fresh workspace carries a demo
    // group. The production tree may quite legitimately start empty.
    let target_tab = shell::shell::tab_binding::adopt_toplevel(
        &mut h.state.workspace_tree.write(),
        0xfeed,
        Some("org.example.Existing".to_string()),
    );
    let intended = shell::shell::tab_binding::Placement::IntoTab(target_tab);
    shell::shell::launcher::set_pending_placement_for_identities(
        desktop_id,
        &[identity.to_string()],
        intended,
    );
    let token = h.state.issue_host_launch_activation(
        desktop_id.to_string(),
        "Token host application".to_string(),
        intended,
    );

    let qh = h.queue.handle();
    let Toplevel {
        surface: target,
        xdg: _,
        toplevel: _,
    } = create_toplevel(&compositor, &wm, &qh, identity);
    h.pump();
    let target_id = foreign_id(&h.state, identity);
    assert_eq!(
        h.state.workspace_tree.read().desktop.workspaces[0].strips[0].window_groups[0].tabs[0].id,
        target_tab,
        "normal first adoption used the staged host placement"
    );

    // This is what a Wayland child does with its inherited
    // XDG_ACTIVATION_TOKEN after mapping its toplevel.
    activation.activate(token.clone(), &target);
    h.pump();
    assert_eq!(
        shell::compositor::client_input::active_foreign_surface(&h.state.workspace_tree.read()),
        Some(target_id),
        "activation focuses the actual mapped host surface"
    );
    assert_eq!(h.state.client_input.keyboard_focus(), Some(target_id));
    let tree = h.state.workspace_tree.read();
    let tab = tree
        .desktop.workspaces
        .iter()
        .flat_map(|workspace| workspace.strips.iter())
        .flat_map(|strip| strip.window_groups.iter())
        .flat_map(|group| group.tabs.iter())
        .find(|tab| {
            matches!(
                &tab.tile,
                shell::shell::state::TileContent::Foreign { surface_id, .. } if *surface_id == target_id
            )
        })
        .expect("activated host surface remains represented by one tab");
    assert_eq!(tab.id, target_tab, "relocation kept the claimed target tab");
    assert_eq!(tab.title, "Token host application");
    assert_eq!(h.state.xdg_activation_state.tokens().count(), 0);
    assert!(
        shell::shell::launcher::placement_for_toplevel(identity, target_id.wrapping_add(1))
            .is_none(),
        "activation consumed the desktop-id placement note before another window can claim it"
    );
    drop(tree);

    activation.activate(token, &target);
    h.pump();
    assert_eq!(
        shell::compositor::client_input::active_foreign_surface(&h.state.workspace_tree.read()),
        Some(target_id),
        "the consumed host token cannot be replayed"
    );
    shell::shell::launcher::clear_pending_placements();
}

/// Show `surface` with a small SHM frame: from then on it is a mapped
/// window, no longer one still opening.
fn show_a_frame(h: &mut Harness, shm: &wl_shm::WlShm, surface: &wl_surface::WlSurface) {
    const SIDE: i32 = 16;
    let qh = h.queue.handle();
    let buffer = wayland::shm_buffer(shm, &qh, SIDE, SIDE, &wayland::solid(SIDE, SIDE, [0; 4]));
    surface.attach(Some(&buffer), 0, 0);
    surface.damage(0, 0, SIDE, SIDE);
    surface.commit();
    h.pump();
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one ordered scenario keeps the person's steps and what each changed visible"
)]
#[serial_test::serial(process_env)]
fn an_app_activating_another_with_a_valid_token_brings_it_forward() {
    let mut h = Harness::new();
    let activation = h.bind::<xdg_activation_v1::XdgActivationV1>("xdg_activation_v1", 1);
    let compositor = h.bind::<wl_compositor::WlCompositor>("wl_compositor", 4);
    let wm = h.bind::<xdg_wm_base::XdgWmBase>("xdg_wm_base", 3);
    let seat = h.bind::<wl_seat::WlSeat>("wl_seat", 9);
    let shm = h.bind::<wl_shm::WlShm>("wl_shm", 1);
    let _keyboard = seat.get_keyboard(&h.queue.handle(), ());
    let qh = h.queue.handle();

    let Toplevel {
        surface: source,
        xdg: _,
        toplevel: source_top,
    } = create_toplevel(&compositor, &wm, &qh, "org.example.Mail");
    source.commit();
    let Toplevel {
        surface: target,
        xdg: _,
        toplevel: _,
    } = create_toplevel(&compositor, &wm, &qh, "org.example.Browser");
    target.commit();
    h.pump();
    show_a_frame(&mut h, &shm, &source);
    show_a_frame(&mut h, &shm, &target);
    let source_id = foreign_id(&h.state, "org.example.Mail");
    let target_id = foreign_id(&h.state, "org.example.Browser");

    // The browser lives on a strip of its own.
    let elsewhere = {
        let mut tree = h.state.workspace_tree.write();
        let group = shell::shell::tab_binding::group_id_for_surface(&tree, target_id)
            .expect("the browser has a group");
        let workspace = tree.desktop.active_workspace_mut().expect("a workspace");
        let moved = workspace
            .strips
            .iter_mut()
            .find_map(|strip| {
                let at = strip.window_groups.iter().position(|g| g.id == group)?;
                Some(strip.window_groups.remove(at))
            })
            .expect("the browser's group is on a strip");
        let mut strip =
            shell::shell::state::Strip::new(shell::shell::state::StripId(9_200_001), "Elsewhere");
        strip.window_groups.push(moved);
        let id = strip.id;
        workspace.strips.push(strip);
        drop(tree);
        id
    };

    // The person is in Mail, full screen.
    assert!(shell::shell::tab_binding::activate_surface(
        &mut h.state.workspace_tree.write(),
        source_id
    ));
    h.state.set_client_keyboard_focus(Some(source_id));
    h.pump();
    let _ = h.state.leave_fullscreen_when_focus_moved(None);
    source_top.set_fullscreen(None);
    h.pump();
    let mail_group = h.state.client_fullscreen_group();
    let mail_strip = {
        let tree = h.state.workspace_tree.read();
        tree.desktop.active_strip.get(&tree.desktop.active).copied()
    };
    let serial = h.client.keyboard_serial.expect("keyboard enter serial");

    // A background process asks for the browser without the person's input.
    let unattributed = activation.get_activation_token(&qh, ());
    unattributed.set_app_id("org.example.Browser".into());
    unattributed.commit();
    h.pump();
    if let Some(token) = h.client.token.take() {
        activation.activate(token, &target);
        h.pump();
    }
    let _ = h.state.leave_fullscreen_when_focus_moved(None);
    h.pump();
    let after_background = (
        shell::compositor::client_input::active_foreign_surface(&h.state.workspace_tree.read()),
        h.state.client_fullscreen_group(),
    );

    // A link clicked in Mail opens in the browser: Mail hands it a token
    // minted from the click.
    let token = activation.get_activation_token(&qh, ());
    token.set_serial(serial, &seat);
    token.set_surface(&source);
    token.set_app_id("org.example.Browser".into());
    token.commit();
    h.pump();
    let token_text = h.client.token.clone().expect("activation token");
    activation.activate(token_text, &target);
    h.pump();
    let _ = h.state.leave_fullscreen_when_focus_moved(None);
    h.pump();
    let tree = h.state.workspace_tree.read();
    let active_strip = tree.desktop.active_strip.get(&tree.desktop.active).copied();
    let in_front = shell::compositor::client_input::active_foreign_surface(&tree);
    drop(tree);

    assert!(mail_group.is_some(), "fixture: Mail is full screen");
    assert_ne!(
        mail_strip,
        Some(elsewhere),
        "fixture: the browser is on another strip"
    );
    assert_eq!(
        after_background,
        (Some(source_id), mail_group),
        "an activation without the person's input moves nothing and ends nothing"
    );
    assert_eq!(in_front, Some(target_id), "the browser is in front");
    assert_eq!(h.state.client_input.keyboard_focus(), Some(target_id));
    assert_eq!(active_strip, Some(elsewhere), "the view goes to its strip");
    assert_eq!(
        h.state.client_fullscreen_group(),
        None,
        "Mail leaves full screen"
    );
}

// The interim proof that the person is behind an app's open: their own
// press reaching the app, from the same input serials an activation token
// is minted from. One press proves one open, and an agent's press through
// the shared seat proves none.
#[test]
#[serial_test::serial(process_env)]
fn a_press_of_the_persons_in_an_app_proves_its_next_open_and_an_agents_does_not() {
    use shell::compositor::activation::person_initiated;
    let mut h = Harness::new();
    let compositor = h.bind::<wl_compositor::WlCompositor>("wl_compositor", 4);
    let wm = h.bind::<xdg_wm_base::XdgWmBase>("xdg_wm_base", 3);
    let seat = h.bind::<wl_seat::WlSeat>("wl_seat", 9);
    let _keyboard = seat.get_keyboard(&h.queue.handle(), ());
    let qh = h.queue.handle();
    create_toplevel(&compositor, &wm, &qh, "org.example.Chat");
    h.pump();
    let chat_id = foreign_id(&h.state, "org.example.Chat");
    assert!(shell::shell::tab_binding::activate_surface(
        &mut h.state.workspace_tree.write(),
        chat_id
    ));
    h.state.set_client_keyboard_focus(Some(chat_id));
    h.pump();
    let proved = |h: &Harness, app: &str| person_initiated(&h.state.workspace_tree.read(), app);
    let press = |h: &mut Harness| {
        h.state.deliver_client_key(30, true, true);
        h.state.deliver_client_key(30, false, true);
        h.pump();
    };

    let before_any = proved(&h, "org.example.Chat");
    h.state.client_input.set_participant(Some(0x5eed_0003));
    press(&mut h);
    h.state.client_input.set_participant(None);
    let after_agent = proved(&h, "org.example.Chat");
    press(&mut h);
    let first = proved(&h, "org.example.Chat");
    let second = proved(&h, "org.example.Chat");

    assert!(!before_any, "no press, no proof");
    assert!(
        !after_agent,
        "an agent's press through the shared seat proves nothing"
    );
    assert!(first, "the person's press in Chat proves Chat's next open");
    assert!(!second, "one press proves one open");
}

// A tap is a press too: the person's finger down in an app proves its next
// open as a key or a click does, and an agent's touch through the shared
// seat proves none.
#[test]
#[serial_test::serial(process_env)]
fn a_tap_of_the_persons_in_an_app_proves_its_next_open_and_an_agents_does_not() {
    use shell::compositor::activation::person_initiated;
    use shell::compositor::backend::TouchEvent;
    let mut h = Harness::new();
    let compositor = h.bind::<wl_compositor::WlCompositor>("wl_compositor", 4);
    let wm = h.bind::<xdg_wm_base::XdgWmBase>("xdg_wm_base", 3);
    let seat = h.bind::<wl_seat::WlSeat>("wl_seat", 9);
    let _touch = seat.get_touch(&h.queue.handle(), ());
    let qh = h.queue.handle();
    create_toplevel(&compositor, &wm, &qh, "org.example.Chat");
    // Notes is another app: its own connection, so its own client.
    let notes_conn = crate::support::wayland::connect(&h.display);
    let mut notes_queue = notes_conn.new_event_queue::<Client>();
    let notes_qh = notes_queue.handle();
    let notes_registry = notes_conn.display().get_registry(&notes_qh, ());
    let mut notes_client = Client::default();
    let mut pump_both = |h: &mut Harness, notes_client: &mut Client| {
        for _ in 0..4 {
            let _ = notes_queue.flush();
            h.pump();
            if let Some(guard) = notes_conn.prepare_read() {
                let _ = guard.read();
            }
            let _ = notes_queue.dispatch_pending(notes_client);
        }
    };
    pump_both(&mut h, &mut notes_client);
    let global = |name: &str| {
        notes_client
            .globals
            .iter()
            .find(|(_, interface, _)| interface == name)
            .map_or_else(
                || panic!("{name} is advertised"),
                |(id, _, version)| (*id, *version),
            )
    };
    let (compositor_name, compositor_version) = global("wl_compositor");
    let (wm_name, wm_version) = global("xdg_wm_base");
    let notes_compositor: wl_compositor::WlCompositor =
        notes_registry.bind(compositor_name, compositor_version.min(4), &notes_qh, ());
    let notes_wm: xdg_wm_base::XdgWmBase =
        notes_registry.bind(wm_name, wm_version.min(3), &notes_qh, ());
    let (seat_name, seat_version) = global("wl_seat");
    let notes_seat: wl_seat::WlSeat =
        notes_registry.bind(seat_name, seat_version.min(9), &notes_qh, ());
    let _notes_touch = notes_seat.get_touch(&notes_qh, ());
    create_toplevel(&notes_compositor, &notes_wm, &notes_qh, "org.example.Notes");
    pump_both(&mut h, &mut notes_client);
    let chat_id = foreign_id(&h.state, "org.example.Chat");
    let notes_id = foreign_id(&h.state, "org.example.Notes");
    let proved = |h: &Harness, app: &str| person_initiated(&h.state.workspace_tree.read(), app);
    let tap = |h: &mut Harness, surface: u64| {
        h.state.forward_client_touch(
            TouchEvent::Down {
                slot: 0,
                x: 4.0,
                y: 4.0,
            },
            Some((surface, (0.0, 0.0))),
        );
        h.state
            .forward_client_touch(TouchEvent::Up { slot: 0 }, Some((surface, (0.0, 0.0))));
        h.state
            .forward_client_touch(TouchEvent::Frame, Some((surface, (0.0, 0.0))));
        h.pump();
    };

    let before_any = proved(&h, "org.example.Chat");
    h.state.client_input.set_participant(Some(0x5eed_0003));
    tap(&mut h, chat_id);
    h.state.client_input.set_participant(None);
    let after_agent = proved(&h, "org.example.Chat");
    tap(&mut h, notes_id);
    let after_elsewhere = proved(&h, "org.example.Chat");
    let notes_own = proved(&h, "org.example.Notes");
    tap(&mut h, chat_id);
    let first = proved(&h, "org.example.Chat");
    let second = proved(&h, "org.example.Chat");

    assert!(!before_any, "no tap, no proof");
    assert!(
        !after_agent,
        "an agent's touch through the shared seat proves nothing"
    );
    assert!(
        !after_elsewhere,
        "a tap in another app's window proves nothing for Chat"
    );
    assert!(notes_own, "it proves the next open of the app it reached");
    assert!(first, "the person's tap in Chat proves Chat's next open");
    assert!(!second, "one tap proves one open");
}

#[test]
#[allow(clippy::too_many_lines, reason = "one scenario read top to bottom")]
fn real_xdg_popup_is_grabbed_configured_repositioned_and_never_adopted_as_a_tab() {
    let mut h = Harness::new();
    let compositor = h.bind::<wl_compositor::WlCompositor>("wl_compositor", 4);
    let wm = h.bind::<xdg_wm_base::XdgWmBase>("xdg_wm_base", 3);
    let seat = h.bind::<wl_seat::WlSeat>("wl_seat", 9);
    let _keyboard = seat.get_keyboard(&h.queue.handle(), ());
    let qh = h.queue.handle();

    let Toplevel {
        surface: _,
        xdg: parent_xdg,
        toplevel: _,
    } = create_toplevel(&compositor, &wm, &qh, "org.example.MenuOwner");
    h.pump();
    let parent_id = foreign_id(&h.state, "org.example.MenuOwner");
    h.state.set_client_keyboard_focus(Some(parent_id));
    h.pump();
    let serial = h.client.keyboard_serial.expect("focused parent serial");

    let positioner = wm.create_positioner(&qh, ());
    positioner.set_size(300, 120);
    positioner.set_anchor_rect(290, 10, 10, 20);
    let popup_surface = compositor.create_surface(&qh, ());
    let popup_xdg = wm.get_xdg_surface(&popup_surface, &qh, ());
    let popup = popup_xdg.get_popup(Some(&parent_xdg), &positioner, &qh, ());
    h.pump();
    popup.grab(&seat, serial);
    h.pump();
    assert!(
        h.state
            .seat
            .get_pointer()
            .is_some_and(|pointer| pointer.is_grabbed())
    );
    assert!(
        h.state
            .seat
            .get_keyboard()
            .is_some_and(|keyboard| keyboard.is_grabbed())
    );
    assert_eq!(
        h.state.xdg_popup_count(),
        1,
        "popup is transient compositor state"
    );
    assert_eq!(
        h.state
            .workspace_tree
            .read()
            .desktop
            .workspaces
            .iter()
            .flat_map(|workspace| workspace.strips.iter())
            .flat_map(|strip| strip.window_groups.iter())
            .flat_map(|group| group.tabs.iter())
            .filter(|tab| matches!(&tab.tile, shell::shell::state::TileContent::Foreign { .. }))
            .count(),
        1,
        "popup never becomes a workspace tab"
    );

    h.state
        .set_output_layout(&[shell::compositor::backend::Output::new(
            shell::compositor::backend::OutputId(0),
            (1920, 1080),
            1.0,
        )]);
    shell::shell::foreign::store().note_placement(
        parent_id,
        vello::kurbo::Rect::new(100.0, 100.0, 400.0, 300.0),
    );
    let visible = h.state.popup_surfaces_for_tests();
    h.pump();
    let (_, rect) = visible.first().expect("popup receives a desktop rect");
    assert!(
        rect.x1 > 400.0,
        "menu may extend beyond parent within output"
    );
    assert!(
        !h.client.popup_configures.is_empty(),
        "popup configure reached client"
    );

    let repositioner = wm.create_positioner(&qh, ());
    repositioner.set_size(160, 100);
    repositioner.set_anchor_rect(0, 0, 10, 10);
    popup.reposition(&repositioner, 77);
    h.pump();
    let _ = h.state.popup_surfaces_for_tests();
    h.pump();
    assert_eq!(h.client.repositioned, vec![77]);

    // A submenu is a popup whose parent is the already-grabbed popup.  It is
    // accepted in protocol order, and the same outside press closes both.
    let child_positioner = wm.create_positioner(&qh, ());
    child_positioner.set_size(80, 60);
    child_positioner.set_anchor_rect(0, 0, 10, 10);
    let child_surface = compositor.create_surface(&qh, ());
    let child_xdg = wm.get_xdg_surface(&child_surface, &qh, ());
    let child_popup = child_xdg.get_popup(Some(&popup_xdg), &child_positioner, &qh, ());
    h.pump();
    child_popup.grab(&seat, serial);
    h.pump();
    assert_eq!(
        h.state.popup_surfaces_for_tests().len(),
        2,
        "nested popup is tracked"
    );

    h.state.forward_client_pointer_motion((900.0, 700.0), None);
    assert!(h.state.forward_client_pointer_button(272, true));
    h.pump();
    assert_eq!(
        h.client.popup_done, 2,
        "outside press dismisses nested popup grab tree"
    );
}

#[test]
fn real_xdg_title_change_distinguishes_same_app_windows() {
    let mut h = Harness::new();
    let compositor = h.bind::<wl_compositor::WlCompositor>("wl_compositor", 4);
    let wm = h.bind::<xdg_wm_base::XdgWmBase>("xdg_wm_base", 3);
    let qh = h.queue.handle();
    for title in ["draft-a.md", "draft-b.md"] {
        let Toplevel {
            surface: _,
            xdg: _,
            toplevel,
        } = create_toplevel(&compositor, &wm, &qh, "org.example.Writer");
        toplevel.set_title(title.into());
    }
    h.pump();
    let titles = h
        .state
        .workspace_tree
        .read()
        .desktop.workspaces
        .iter()
        .flat_map(|workspace| workspace.strips.iter())
        .flat_map(|strip| strip.window_groups.iter())
        .flat_map(|group| group.tabs.iter())
        .filter_map(|tab| matches!(&tab.tile, shell::shell::state::TileContent::Foreign { app_id, .. } if app_id == "org.example.Writer").then_some(tab.title.clone()))
        .collect::<Vec<_>>();
    assert_eq!(titles, vec!["draft-a.md", "draft-b.md"]);
}
