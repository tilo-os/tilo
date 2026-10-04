//! The overview is a switcher for rooms: which one you're in, who's in
//! it, what leaves it, how to get to another. These pin geometry/data,
//! not pixels — every assertion runs against the same helpers
//! `draw_overview` itself calls (`overview_row_rect`, `room_cards`).
use crate::shell::state::FreshId as _;

use super::*;
use crate::shell::chrome_state::ChromeState;
use crate::shell::overview::room_cards;
use crate::shell::state::{CapOverride, Collaborator, Scope, Subject, Verb, WorkspaceTree};
use tilo_desktop::overview::{room_access_label, room_shares_label};
use ui::TextRole;
use ui::widget::Container;

/// A host laid out over a desktop-sized viewport with the overview
/// settled open, matching what the shots harness and the live binary
/// both produce.
fn host_with(tree: WorkspaceTree) -> ShellOverlayHost {
    let chrome = ChromeState::new(tree);
    chrome.borrow_mut().overlays.overview = true;
    let mut host = ShellOverlayHost::new(Box::new(Container::new())).with_chrome(chrome);
    host.rect = Rect::new(0.0, 0.0, 1600.0, 900.0);
    host.force_overview_settled(true);
    host
}

/// The panel is about several rooms, so it is built over the
/// populated fixture. `bootstrap_default` is a single empty
/// workspace — a first boot invents nothing — which is the right
/// shape for the boot-path tests and no shape at all for a list.
fn host() -> ShellOverlayHost {
    host_with(WorkspaceTree::bootstrap_demo())
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one scenario test keeps each merged phase and its fresh fixture visible"
)]
fn overview_rename_selection_is_directional_accessible_and_owned_by_the_palette() {
    fn rename_arrows_collapse_selection_to_its_directional_edge() {
        for (key, expected) in [(NamedKey::ArrowLeft, 0), (NamedKey::ArrowRight, 4)] {
            for selection in [
                ui::text_selection::Selection {
                    anchor: 0,
                    cursor: 4,
                },
                ui::text_selection::Selection {
                    anchor: 4,
                    cursor: 0,
                },
            ] {
                let mut name = "Home".to_string();
                let (_, _, actual) = ShellOverlayHost::apply_workspace_rename_key(
                    &Key::Named(key),
                    false,
                    false,
                    &mut name,
                    selection,
                );
                assert_eq!(actual, ui::text_selection::Selection::collapsed(expected));
            }
        }
        let mut name = "Home".to_string();
        let (_, _, shifted) = ShellOverlayHost::apply_workspace_rename_key(
            &Key::Named(NamedKey::ArrowLeft),
            false,
            true,
            &mut name,
            ui::text_selection::Selection {
                anchor: 0,
                cursor: 4,
            },
        );
        assert_eq!(
            shifted,
            ui::text_selection::Selection {
                anchor: 0,
                cursor: 3
            }
        );
    }

    fn workspace_select_all_invalidates_painted_selection() {
        let mut name = "Home".to_string();
        let (_, bump, selection) = ShellOverlayHost::apply_workspace_rename_ctrl_chord(
            "a",
            &mut name,
            ui::text_selection::Selection::collapsed(4),
        );
        assert_eq!(selection.range(), 0..4);
        assert!(bump, "selection changes need a fresh frame");
    }

    fn workspace_name_accessibility_exposes_current_selection() {
        fn find(node: &ui::a11y::AccessNode) -> Option<&ui::a11y::AccessNode> {
            if node.name.as_deref() == Some("Workspace name") {
                return Some(node);
            }
            node.children.iter().find_map(find)
        }
        let h = host_with(WorkspaceTree::bootstrap_default());
        {
            let mut state = h.chrome.as_ref().unwrap().borrow_mut();
            state.overlays.ws_settings = true;
            state.editing_workspace_name = Some("Home".into());
            state.editing_workspace_name_selection = ui::text_selection::Selection {
                anchor: 0,
                cursor: 4,
            };
        }
        let node = h.accessibility().unwrap();
        let selection = find(&node)
            .unwrap()
            .text_selection
            .as_ref()
            .expect("selection must be exposed");
        assert_eq!((selection.anchor, selection.focus), (0, 4));
    }

    fn desktop_palette_owns_keys_above_an_active_rename_field() {
        let mut h = host_with(WorkspaceTree::bootstrap_default());
        {
            let mut chrome = h.chrome.as_ref().unwrap().borrow_mut();
            chrome.overlays.ws_settings = true;
            chrome.editing_workspace_name = Some("Home".into());
        }
        h.palette_visible = true;
        let mut handled = false;
        assert!(!h.handle_workspace_rename_key(
            &Key::Character("Files".into()),
            winit::event::Modifiers::default(),
            &mut handled
        ));
        assert!(!handled);
        assert_eq!(
            h.chrome
                .as_ref()
                .unwrap()
                .borrow()
                .editing_workspace_name
                .as_deref(),
            Some("Home")
        );
    }

    rename_arrows_collapse_selection_to_its_directional_edge();
    workspace_select_all_invalidates_painted_selection();
    workspace_name_accessibility_exposes_current_selection();
    desktop_palette_owns_keys_above_an_active_rename_field();
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one scenario test keeps each merged phase and its fresh fixture visible"
)]
fn overview_help_and_hidden_controls_expose_no_stray_targets_and_scale_once() {
    fn app_content_has_no_floating_help_hit_target() {
        let mut h = host_with(WorkspaceTree::bootstrap_default());
        let chrome = h.chrome.clone().unwrap();
        crate::shell::window_actions::open_hosted(
            &mut chrome.borrow_mut(),
            crate::shell::state::AppKind::FileExplorer,
            "Files",
            crate::shell::window_actions::Destination::Beside,
        );
        chrome.borrow_mut().overlays = crate::shell::chrome_state::OverlayFlags::default();
        assert!(h.shortcut_help_button_is_suppressed());
        h.last_cursor = Some(h.shortcut_help_button_rect().center());
        let mut handled = false;
        h.handle_shell_mouse_click_event(
            &WindowEvent::MouseInput {
                device_id: winit::event::DeviceId::dummy(),
                state: ElementState::Pressed,
                button: winit::event::MouseButton::Left,
            },
            &mut handled,
        );
        assert!(!chrome.borrow().overlays.shortcut_help);
    }

    fn dismissed_palette_command_closes_focus_and_semantics() {
        let mut h = host();
        h.palette.open();
        h.palette_visible = true;
        h.dispatch_palette_command("unknown");
        assert!(!h.palette_visible);
        assert!(!h.palette.is_open());
        assert!(h.palette.accessibility().unwrap().state.hidden);
    }

    fn desktop_scrolled_settings_name_cannot_receive_projected_focus_or_hits() {
        let mut host = host_with(WorkspaceTree::bootstrap_default());
        {
            let mut state = host.chrome.as_ref().unwrap().borrow_mut();
            state.overlays.overview = false;
            state.overlays.ws_settings = true;
        }
        host.workspace_settings_overlay
            .layout(Rect::new(0.0, 0.0, 960.0, 540.0));
        let root = host.accessibility().unwrap();
        let overlay = root
            .children
            .iter()
            .position(|node| node.name.as_deref() == Some("Workspace settings"))
            .unwrap();
        let name = root.children[overlay]
            .children
            .iter()
            .position(|node| node.name.as_deref() == Some("Workspace name"))
            .unwrap();
        let path = [overlay, name];
        assert!(host.synthetic_accessibility_rect(&path).is_some());
        assert!(host.workspace_settings_overlay.scroll_general(1000.0));
        let root = host.accessibility().unwrap();
        assert!(root.children[overlay].children[name].state.hidden);
        assert!(host.synthetic_accessibility_rect(&path).is_none());
        assert!(!host.focus_synthetic_accessibility(&path));
        assert!(
            host.chrome
                .as_ref()
                .unwrap()
                .borrow()
                .editing_workspace_name
                .is_none()
        );
    }

    fn desktop_help_text_is_scaled_once_and_inside_its_hit_target() {
        let mut host = host_with(WorkspaceTree::bootstrap_default());
        host.chrome.as_ref().unwrap().borrow_mut().overlays.overview = false;
        host.force_overview_settled(false);
        host.rect = Rect::new(0.0, 0.0, 960.0, 540.0);
        let theme = Theme::dark().with_text_scale(2.0);
        let mut renderer = TextRenderer::new();
        renderer.set_text_scale(2.0);
        let mut scene = Scene::new();
        host.draw_shortcut_help_button(&mut scene, Affine::IDENTITY, &mut renderer, &theme);
        let rect = host.shortcut_help_button_rect();
        let (width, height) = renderer.measure_style(
            &crate::shell::i18n::t("help-pill"),
            None,
            &theme.text(TextRole::Label).strong(),
        );
        assert!(
            rect.width() >= f64::from(width) + 24.0 && rect.height() >= f64::from(height) + 12.0
        );
        assert!(
            rect.x0 >= 0.0 && rect.y0 >= 0.0 && rect.x1 <= host.rect.x1 && rect.y1 <= host.rect.y1
        );
        assert!(
            scene
                .encoding()
                .resources
                .glyph_runs
                .iter()
                .all(|run| run.font_size == theme.tokens.typography.size(TextRole::Label))
        );
    }

    fn desktop_overlay_text_applies_accessibility_scale_once() {
        let mut h = host_with(WorkspaceTree::bootstrap_default());
        let theme = Theme::dark().with_text_scale(1.5);
        let mut renderer = TextRenderer::new();
        let mut first = Scene::new();
        h.draw_overview_and_ws_settings_overlays(
            &mut first,
            Affine::IDENTITY,
            &mut renderer,
            &theme,
            h.overlay_flags(),
        );
        renderer.set_text_scale(1.5);
        let mut scaled = Scene::new();
        h.draw_overview_and_ws_settings_overlays(
            &mut scaled,
            Affine::IDENTITY,
            &mut renderer,
            &theme,
            h.overlay_flags(),
        );
        let sizes = |scene: &Scene| {
            scene
                .encoding()
                .resources
                .glyph_runs
                .iter()
                .map(|r| r.font_size)
                .collect::<Vec<_>>()
        };
        assert_eq!(
            sizes(&first),
            sizes(&scaled),
            "already-scaled overlay tokens must render once"
        );
        assert_eq!(
            renderer.text_scale(),
            1.5,
            "restore the shared renderer for child widgets"
        );
    }

    app_content_has_no_floating_help_hit_target();
    dismissed_palette_command_closes_focus_and_semantics();
    desktop_scrolled_settings_name_cannot_receive_projected_focus_or_hits();
    desktop_help_text_is_scaled_once_and_inside_its_hit_target();
    desktop_overlay_text_applies_accessibility_scale_once();
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one scenario test keeps each merged phase and its fresh fixture visible"
)]
fn overview_panel_bounds_hit_rooms_fit_the_viewport_and_scroll_when_long() {
    fn desktop_overview_publishes_real_workspace_and_create_bounds() {
        let h = host_with(WorkspaceTree::bootstrap_default());
        let root = h.accessibility().unwrap();
        let overview = root
            .children
            .iter()
            .find(|n| n.name.as_deref() == Some("Workspace overview"))
            .expect("visible overview semantics");
        assert!(
            overview
                .children
                .iter()
                .any(|n| n.name.as_deref() == Some("Home") && n.bounds.is_some())
        );
        assert!(
            overview
                .children
                .iter()
                .any(|n| n.name.as_deref() == Some("New workspace") && n.bounds.is_some())
        );
    }

    fn desktop_create_workspace_dismisses_overview_for_navigation() {
        let mut h = host_with(WorkspaceTree::bootstrap_default());
        h.add_workspace_rect = h.overview_add_workspace_rect_for(1);
        h.last_cursor = Some(h.add_workspace_rect.center());
        let event = WindowEvent::MouseInput {
            device_id: winit::event::DeviceId::dummy(),
            state: ElementState::Pressed,
            button: winit::event::MouseButton::Left,
        };
        let mut handled = false;
        h.handle_shell_overview_pointer_event(&event, 1.0, &mut handled);
        let chrome = h.chrome.as_ref().unwrap().borrow();
        assert_eq!(chrome.tree().desktop.workspaces.len(), 2);
        assert!(
            !chrome.overlays.overview,
            "new workspace must release navigation input"
        );
    }

    /// Draw and hit-test share one geometry oracle: clicking the middle
    /// of a room's painted rect selects that room.
    fn clicking_a_room_rect_hits_that_room() {
        let h = host();
        for i in 0..ws_count(&h) {
            let r = h.overview_row_rect(i);
            assert_eq!(
                h.hit_test_overview(r.center()),
                Some(i),
                "the centre of room {i}'s rect must hit room {i}"
            );
        }
    }

    /// A click outside the panel is not a room. The panel is a surface
    /// floating over a scrim, so the scrim has to stay clickable-through
    /// to "dismiss" rather than silently activating an edge row.
    fn clicks_outside_the_panel_hit_no_room() {
        let h = host();
        let panel = h.overview_panel_rect();
        assert_eq!(
            h.hit_test_overview(Point::new(panel.x0 - 20.0, panel.center().y)),
            None
        );
        assert_eq!(
            h.hit_test_overview(Point::new(panel.center().x, panel.y0 - 20.0)),
            None
        );
    }

    /// The panel is sized to what it says and stays on screen.
    fn the_panel_is_content_sized_and_stays_inside_the_viewport() {
        let small = host_with(WorkspaceTree::bootstrap_default());
        let many = host();
        assert!(
            many.overview_panel_rect().height() > small.overview_panel_rect().height(),
            "more rooms means a taller panel — it is not a fixed page"
        );
        for h in [&small, &many] {
            let p = h.overview_panel_rect();
            assert!(
                p.x0 >= h.rect.x0 && p.x1 <= h.rect.x1 && p.y0 >= h.rect.y0 && p.y1 <= h.rect.y1,
                "panel {p:?} escapes the viewport {:?}",
                h.rect
            );
        }
    }

    /// A tree with more rooms than fit is capped by the viewport and
    /// scrolls instead of growing off-screen.
    fn a_long_room_list_is_capped_and_scrolls() {
        let mut tree = WorkspaceTree::bootstrap_demo();
        for i in 0..40 {
            tree.desktop
                .workspaces
                .push(crate::shell::state::Workspace::new(
                    WorkspaceId::new(),
                    format!("Room {i}"),
                ));
        }
        let h = host_with(tree);
        let p = h.overview_panel_rect();
        assert!(
            p.y0 >= h.rect.y0 && p.y1 <= h.rect.y1,
            "panel stays on screen"
        );
        assert!(
            h.overview_max_scroll_y() > 0.0,
            "a list taller than its band has somewhere to scroll to"
        );
    }

    /// Everything fitting means nothing to scroll — a panel that could
    /// pan its content out of view for no reason would lose rooms.
    fn a_short_room_list_has_nothing_to_scroll() {
        let h = host();
        assert_eq!(h.overview_max_scroll_y(), 0.0);
    }

    desktop_overview_publishes_real_workspace_and_create_bounds();
    desktop_create_workspace_dismisses_overview_for_navigation();
    clicking_a_room_rect_hits_that_room();
    clicks_outside_the_panel_hit_no_room();
    the_panel_is_content_sized_and_stays_inside_the_viewport();
    a_long_room_list_is_capped_and_scrolls();
    a_short_room_list_has_nothing_to_scroll();
}

fn ws_count(h: &ShellOverlayHost) -> usize {
    h.chrome
        .as_ref()
        .map_or(0, |c| c.borrow().tree().desktop.workspaces.len())
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one scenario test keeps each merged phase and its fresh fixture visible"
)]
fn overview_room_rows_pin_the_current_room_and_grow_on_the_spacing_scale() {
    /// The room you are in is the block at the top, not a row in the
    /// list you could scroll away from. That is the difference between
    /// "here is where you are" and "here is an index of rooms".
    fn the_room_you_are_in_is_the_pinned_block_not_a_list_row() {
        let h = host();
        let here = h
            .overview_here_index()
            .expect("demo tree has an active room");
        assert_eq!(
            h.overview_slot_of(here),
            None,
            "the current room occupies no list slot"
        );
        assert_eq!(h.overview_row_rect(here), h.overview_here_rect());
        for i in 0..ws_count(&h) {
            if i == here {
                continue;
            }
            assert!(
                h.overview_slot_of(i).is_some(),
                "every other room is a row you can go to"
            );
        }
    }

    /// Rows advance down the panel and never overlap: two rooms sharing
    /// pixels would make one of them unclickable.
    fn room_rows_advance_downward_without_overlapping() {
        let h = host();
        let here = h.overview_here_index();
        let mut prev: Option<Rect> = None;
        for i in 0..ws_count(&h) {
            if Some(i) == here {
                continue;
            }
            let r = h.overview_row_rect(i);
            if let Some(p) = prev {
                assert!(
                    r.y0 >= p.y1,
                    "row {i} starts at {} but the row above ends at {}",
                    r.y0,
                    p.y1
                );
            }
            prev = Some(r);
        }
        let add = h.overview_add_workspace_rect_for(ws_count(&h));
        let last = prev.expect("demo tree has rooms other than the current one");
        assert!(
            add.y0 >= last.y1,
            "\"New room\" is the slot after the last room, not on top of it"
        );
        assert_eq!(
            add.width(),
            last.width(),
            "\"New room\" is a peer row in the same list, not a banner"
        );
    }

    /// The panel's density is the spacing scale and the type scale under
    /// other names — pinned so an edit cannot move it off the grid.
    fn overview_metrics_sit_on_the_spacing_scale() {
        let theme = Theme::default();
        let m = OverviewMetrics::for_theme(&theme);
        let spacing = &theme.tokens.spacing;
        assert_eq!(
            m.pad,
            f64::from(spacing.s4),
            "Panel padding is s4 (HIG §9.6)"
        );
        assert_eq!(m.section_gap, f64::from(spacing.s4), "section gap is s4");
        assert_eq!(m.margin, f64::from(spacing.s12));
        assert_eq!(m.room_row_gap, f64::from(spacing.s2));
        assert_eq!(m.row_pad, f64::from(spacing.s4));
        assert_eq!(
            m.panel_radius,
            f64::from(theme.tokens.radii.panel),
            "Panel role"
        );
        assert_eq!(
            m.row_radius,
            f64::from(theme.tokens.radii.control),
            "Control role"
        );
        assert_eq!(OverviewMetrics::default(), m);
    }

    /// Every row of the panel is at least as tall as the text it holds,
    /// at 100 % and at 200 % text — fixed heights put "Private to you",
    /// "Shared out of here" and the share row on top of one another at
    /// 200 %. The panel itself grows with them rather than clipping.
    fn overview_rows_grow_with_the_text_scale_instead_of_stacking() {
        let lh = |size: f32| f64::from(ui::TextStyle::new(size).line_height_px());
        let base = Theme::dark();
        let large = Theme::dark().with_text_scale(2.0);
        let m1 = OverviewMetrics::for_theme(&base);
        let m2 = OverviewMetrics::for_theme(&large);
        for (name, theme, m) in [("100%", &base, m1), ("200%", &large, m2)] {
            let scale = &theme.tokens.typography;
            assert!(
                m.here_h >= lh(scale.size(TextRole::Display)) + lh(scale.size(TextRole::Body)),
                "{name}: the here block ({}) must hold its name and access lines",
                m.here_h
            );
            assert!(
                m.caption_h >= lh(scale.size(TextRole::Caption)),
                "{name}: the caption band ({}) must hold its text",
                m.caption_h
            );
            assert!(
                m.share_row_h >= lh(scale.size(TextRole::Body)),
                "{name}: a share row ({}) must hold its body line",
                m.share_row_h
            );
            assert!(
                m.room_row_h
                    >= lh(scale.size(TextRole::Heading)) + lh(scale.size(TextRole::Caption)),
                "{name}: a room row ({}) must hold its name and sub-line",
                m.room_row_h
            );
            assert!(
                m.chip_h >= lh(scale.size(TextRole::Caption)),
                "{name}: the chip ({}) must hold its label",
                m.chip_h
            );
        }
        for (what, a, b) in [
            ("here block", m1.here_h, m2.here_h),
            ("caption", m1.caption_h, m2.caption_h),
            ("share row", m1.share_row_h, m2.share_row_h),
            ("room row", m1.room_row_h, m2.room_row_h),
        ] {
            assert!(b > a, "{what} must grow with the text scale: {a} -> {b}");
        }

        // And the host follows: laying out with the large theme grows the
        // panel by the same rows, so nothing is clipped into a neighbour.
        let mut h = host();
        h.overview_metrics = m2;
        let panel_large = h.overview_panel_rect();
        h.overview_metrics = m1;
        let panel_base = h.overview_panel_rect();
        assert!(
            panel_large.height() > panel_base.height(),
            "the panel is sized from its rows: {} at 200% vs {} at 100%",
            panel_large.height(),
            panel_base.height()
        );
    }

    the_room_you_are_in_is_the_pinned_block_not_a_list_row();
    room_rows_advance_downward_without_overlapping();
    overview_metrics_sit_on_the_spacing_scale();
    overview_rows_grow_with_the_text_scale_instead_of_stacking();
}

#[test]
fn overview_selection_and_row_content_follow_the_room_and_its_capabilities() {
    /// Opening the panel points the selection at the room you are in, so
    /// the first arrow press steps from *here*.
    fn opening_seeds_the_selection_from_the_room_you_are_in() {
        let h = host();
        assert_eq!(h.overview_selection, h.overview_here_index());
        assert!(h.overview_selection.is_some());
    }

    /// Closing forgets it, so a later open does not resume pointing at
    /// wherever a previous visit stopped.
    fn closing_clears_the_selection() {
        let mut h = host();
        h.force_overview_settled(false);
        assert_eq!(h.overview_selection, None);
    }

    /// A room only you own reads as private, and says so on its row.
    fn a_private_room_says_so_on_its_row() {
        let mut tree = WorkspaceTree::bootstrap_default();
        // What boot does before the session paints: a room you made is one you own.
        tree.adopt_ownerless_workspaces(&crate::shell::state::local_user_id());
        let cards = room_cards(&tree, &crate::shell::state::local_user_id());
        assert_eq!(cards.len(), 1);
        assert_eq!(room_access_label(&cards[0]), "Private to you");
        assert_eq!(
            room_shares_label(&cards[0]),
            None,
            "a room that shares nothing shows no share count"
        );
    }

    /// The panel's people come from the room's capabilities, not from
    /// whoever this session has happened to hear from. Showing a face on
    /// a room states that person can reach it, which is a claim about
    /// access.
    fn faces_come_from_capabilities_not_from_the_collaborator_list() {
        let mut tree = WorkspaceTree::bootstrap_default();
        tree.desktop.workspaces[0].owner = None;
        tree.desktop.workspaces[0].capabilities.clear();
        tree.desktop.workspaces[0].collaborators = vec![Collaborator::new("ada", "Ada")];
        assert!(
            room_cards(&tree, &crate::shell::state::local_user_id())[0]
                .people
                .is_empty(),
            "a person with no capability here is not on this room"
        );

        let room = tree.desktop.workspaces[0].id.raw().to_string();
        tree.desktop.workspaces[0]
            .capabilities
            .push(CapOverride::grant(
                Subject::person("ada"),
                Scope::Room(room),
                Verb::Write,
            ));
        let card = &room_cards(&tree, &crate::shell::state::local_user_id())[0];
        assert_eq!(
            card.people
                .iter()
                .map(|p| p.id.as_str())
                .collect::<Vec<_>>(),
            vec!["ada"],
            "the member who holds a capability is exactly who the room shows"
        );
        assert_eq!(card.people[0].name, "Ada", "the roster supplies the name");
    }

    /// The panel never grows without bound as sharing does: past a few
    /// rows the rest collapse into a count, so the block stays a summary.
    fn the_share_list_collapses_rather_than_growing_without_bound() {
        let mut tree = WorkspaceTree::bootstrap_default();
        for i in 0..12 {
            tree.desktop.workspaces[0]
                .capabilities
                .push(CapOverride::grant(
                    Subject::person("ada"),
                    Scope::FsPath(format!("/var/workspace/doc{i}")),
                    Verb::Read,
                ));
        }
        let card = &room_cards(&tree, &crate::shell::state::local_user_id())[0];
        assert_eq!(card.shares.len(), 12, "the model keeps every share");
        assert!(
            ShellOverlayHost::overview_share_rows(card) <= OVERVIEW_SHARES_MAX_ROWS + 1,
            "the panel draws at most the cap plus one overflow line"
        );
    }

    /// A room that shares nothing still gets a line, because "nothing
    /// leaves this room" is an answer the screen exists to give.
    fn a_room_that_shares_nothing_still_gets_a_row() {
        let tree = WorkspaceTree::bootstrap_default();
        let card = &room_cards(&tree, &crate::shell::state::local_user_id())[0];
        assert!(card.shares.is_empty());
        assert_eq!(ShellOverlayHost::overview_share_rows(card), 1);
    }

    opening_seeds_the_selection_from_the_room_you_are_in();
    closing_clears_the_selection();
    a_private_room_says_so_on_its_row();
    faces_come_from_capabilities_not_from_the_collaborator_list();
    the_share_list_collapses_rather_than_growing_without_bound();
    a_room_that_shares_nothing_still_gets_a_row();
}
