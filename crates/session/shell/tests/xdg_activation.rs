//! Wire-level presence and rejection tests for `xdg_activation_v1`.

use wayland_client::protocol::{wl_compositor, wl_keyboard, wl_seat, wl_surface};
use wayland_client::{Connection, Dispatch, QueueHandle, delegate_noop};
use wayland_protocols::xdg::activation::v1::client::{xdg_activation_token_v1, xdg_activation_v1};
use wayland_protocols::xdg::shell::client::{xdg_popup, xdg_positioner, xdg_toplevel, xdg_wm_base};

use crate::support::wayland::foreign_id;

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

crate::support::wayland::test_client!(Client);

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
delegate_noop!(Client: ignore wl_compositor::WlCompositor);
delegate_noop!(Client: ignore wl_surface::WlSurface);
delegate_noop!(Client: ignore wl_seat::WlSeat);
delegate_noop!(Client: ignore xdg_toplevel::XdgToplevel);
delegate_noop!(Client: ignore xdg_positioner::XdgPositioner);

type Harness = crate::support::wayland::Harness<Client>;

#[test]
#[serial_test::serial(process_env)]
fn xdg_activation_needs_an_attributed_token_and_activates_once() {
    // Phase `activation_global_is_advertised_but_unattributed_tokens_are_not_authority`.
    {
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

    // Phase `focused_serial_activates_matching_toplevel_once`.
    {
        let mut h = Harness::new();
        let activation = h.bind::<xdg_activation_v1::XdgActivationV1>("xdg_activation_v1", 1);
        let compositor = h.bind::<wl_compositor::WlCompositor>("wl_compositor", 4);
        let wm = h.bind::<xdg_wm_base::XdgWmBase>("xdg_wm_base", 3);
        let seat = h.bind::<wl_seat::WlSeat>("wl_seat", 9);
        let _keyboard = seat.get_keyboard(&h.queue.handle(), ());
        let qh = h.queue.handle();

        let source = compositor.create_surface(&qh, ());
        let source_xdg = wm.get_xdg_surface(&source, &qh, ());
        let source_top = source_xdg.get_toplevel(&qh, ());
        source_top.set_app_id("org.example.Source".into());
        let target = compositor.create_surface(&qh, ());
        let target_xdg = wm.get_xdg_surface(&target, &qh, ());
        let target_top = target_xdg.get_toplevel(&qh, ());
        target_top.set_app_id("org.example.Target".into());
        h.pump();

        let source_id = foreign_id(&h.state, "org.example.Source");
        let target_id = foreign_id(&h.state, "org.example.Target");
        assert!(shell::compositor::tab_binding::activate_surface(
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

        assert!(shell::compositor::tab_binding::activate_surface(
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

    // Phase `compositor_minted_host_token_relocates_and_retires_its_launch_note`.
    // The host-launch path mints the token on the compositor thread, while the
    // child merely inherits its opaque standard value.  Exercise that wire path
    // with a real Wayland client: initial adoption, activation, placement/title
    // reconciliation, focus, single-use removal, and the later-window cleanup.
    {
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
        let target_tab = shell::compositor::tab_binding::adopt_toplevel(
            &mut h.state.workspace_tree.write(),
            0xfeed,
            Some("org.example.Existing".to_string()),
        );
        let intended = shell::compositor::tab_binding::Placement::IntoTab(target_tab);
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
        let target = compositor.create_surface(&qh, ());
        let target_xdg = wm.get_xdg_surface(&target, &qh, ());
        let target_top = target_xdg.get_toplevel(&qh, ());
        target_top.set_app_id(identity.into());
        h.pump();
        let target_id = foreign_id(&h.state, identity);
        assert_eq!(
            h.state.workspace_tree.read().desktop.workspaces[0].strips[0].window_groups[0].tabs[0]
                .id,
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
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "the protocol integration test must retain one ordered client/server transcript"
)]
fn real_xdg_popups_and_titles_are_handled_without_becoming_tabs() {
    // Phase `real_xdg_popup_is_grabbed_configured_repositioned_and_never_adopted_as_a_tab`.
    {
        let mut h = Harness::new();
        let compositor = h.bind::<wl_compositor::WlCompositor>("wl_compositor", 4);
        let wm = h.bind::<xdg_wm_base::XdgWmBase>("xdg_wm_base", 3);
        let seat = h.bind::<wl_seat::WlSeat>("wl_seat", 9);
        let _keyboard = seat.get_keyboard(&h.queue.handle(), ());
        let qh = h.queue.handle();

        let parent = compositor.create_surface(&qh, ());
        let parent_xdg = wm.get_xdg_surface(&parent, &qh, ());
        let parent_top = parent_xdg.get_toplevel(&qh, ());
        parent_top.set_app_id("org.example.MenuOwner".into());
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
        shell::compositor::foreign::store().note_placement(
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

    // Phase `real_xdg_title_change_distinguishes_same_app_windows`.
    {
        let mut h = Harness::new();
        let compositor = h.bind::<wl_compositor::WlCompositor>("wl_compositor", 4);
        let wm = h.bind::<xdg_wm_base::XdgWmBase>("xdg_wm_base", 3);
        let qh = h.queue.handle();
        for title in ["draft-a.md", "draft-b.md"] {
            let surface = compositor.create_surface(&qh, ());
            let xdg = wm.get_xdg_surface(&surface, &qh, ());
            let toplevel = xdg.get_toplevel(&qh, ());
            toplevel.set_app_id("org.example.Writer".into());
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
}
