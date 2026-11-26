use super::*;

use crate::testing::Dispatch;
use crate::widget::{AppKind, TabDescriptor, TileContent};

#[test]
fn external_navigation_omits_bar_from_layout_and_semantics_but_retains_tabs() {
    let mut group = make_group();
    assert_eq!(group.children().len(), 4);
    group.set_external_navigation(true);
    let mut tree = TaffyTree::new();
    let node = group.build_layout(&mut tree, &Theme::default(), &mut Default::default());
    assert_eq!(tree.children(node).unwrap().len(), 1);
    assert_eq!(group.children().len(), 1);
    assert_eq!(group.tabs().len(), 2);
}

#[test]
#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "sample points are small positive pixel coordinates"
)]
fn a_resting_pointer_lights_the_whole_edge_that_will_change() {
    use crate::offscreen::OffscreenRenderer;
    use taffy::prelude::TaffyMaxContent;
    let theme = Theme::dark();
    let mut renderer = OffscreenRenderer::new(400, 300);
    let mut render = |group: &mut WindowGroup| {
        let mut tree = TaffyTree::new();
        let node = group.build_layout(&mut tree, &theme, &mut Default::default());
        let mut style = tree.style(node).unwrap().clone();
        style.size.width = Dimension::length(200.0);
        tree.set_style(node, style).unwrap();
        let root = tree.parent_node(
            Style {
                size: Size {
                    width: Dimension::length(200.0),
                    height: Dimension::length(240.0),
                },
                ..Default::default()
            },
            &[node],
        );
        tree.compute_layout(root, Size::MAX_CONTENT).unwrap();
        group.apply_layout(&tree, node, Point::new(100.0, 30.0));
        let mut scene = Scene::new();
        group.draw(&mut crate::draw::DrawCtx::new(
            &mut scene,
            Affine::IDENTITY,
            &mut TextRenderer::new(),
            &theme,
            crate::presence::PresenceCtx::empty().draw_env(),
        ));
        renderer.render(&scene, vello::peniko::Color::from_rgb8(0, 0, 255))
    };
    let start = crate::time::now();
    let later = start.checked_add(EDGE_HOVER_DWELL).unwrap();
    use crate::time::testing::with_monotonic_time as at;
    let mut group = make_group();
    let idle = at(start, || render(&mut group));
    let rect = group.rect;
    let px = |rgba: &[u8], x: f64, y: f64| {
        let (r, g, b, _) = OffscreenRenderer::sample(rgba, 400, x as u32, y as u32);
        (r, g, b)
    };
    let trailing = rect.x1 - EDGE_LIT_PX / 2.0;
    let leading = rect.x0 + EDGE_LIT_PX / 2.0;
    let card = group.card_rect();
    let heights = [card.y0 + 24.0, card.center().y, card.y1 - 24.0];

    let passing = at(start, || {
        group.set_edge_highlight(Some(WindowEdge::Trailing));
        render(&mut group)
    });
    for y in heights {
        assert_eq!(
            px(&passing, trailing, y),
            px(&idle, trailing, y),
            "passing over an edge lights nothing (y = {y})"
        );
    }
    assert!(
        at(later, || group.needs_redraw()
            || group.needs_animation_frame()),
        "the dwell's end is a frame of its own"
    );
    assert_eq!(group.next_wake_deadline(), Some(later));

    let rested = at(later, || render(&mut group));
    for y in heights {
        assert_ne!(
            px(&rested, trailing, y),
            px(&idle, trailing, y),
            "a resting pointer lights the edge a press would resize (y = {y})"
        );
        assert_eq!(
            px(&rested, leading, y),
            px(&idle, leading, y),
            "and only that edge"
        );
    }
    assert_eq!(
        group.edge_reveal_at(),
        None,
        "lit, it waits for nothing more"
    );

    let dragging = at(start, || {
        group.set_edge_highlight(None);
        group.begin_edge_drag(WindowEdge::Leading);
        render(&mut group)
    });
    let y = card.center().y;
    assert_ne!(
        px(&dragging, leading, y),
        px(&idle, leading, y),
        "a drag lights its edge at once"
    );
    let beside = (12..60)
        .map(|dx| rect.x0 + f64::from(dx))
        .any(|x| px(&dragging, x, y) != px(&idle, x, y));
    assert!(beside, "the window's size shows beside the dragged edge");
    group.end_edge_drag(true);
    assert!(
        group.needs_animation_frame(),
        "the size fades out once let go"
    );
}

#[test]
fn a_host_keeps_its_address_while_other_tabs_come_and_go() {
    // The frame's layout pointer map names the shown host by address
    // until the next structural rebuild; another tab opening, moving or
    // leaving must not move it.
    let mut group = make_group();
    let shown: *const TileHost = group.host_for_tab_mut(1).expect("tab 1");
    for id in 3..40 {
        group.insert_detached_tab(
            0,
            TabDescriptor::new(id, format!("T{id}")),
            TileHost::new(TileContent::Stub {
                kind: AppKind::Settings,
                label: format!("t{id}"),
            }),
        );
    }
    assert!(group.reorder_tab(0, 5));
    assert!(group.detach_tab(2).is_some());
    let now: *const TileHost = group.host_for_tab_mut(1).expect("tab 1");
    assert_eq!(shown, now);
}

pub(super) fn make_group() -> WindowGroup {
    let tabs: Vec<(TabDescriptor, TileContent)> = vec![
        (
            TabDescriptor::new(1, "A"),
            TileContent::Stub {
                kind: AppKind::Settings,
                label: "a".into(),
            },
        ),
        (
            TabDescriptor::new(2, "B"),
            TileContent::Stub {
                kind: AppKind::Settings,
                label: "b".into(),
            },
        ),
    ];
    WindowGroup::new(10, tabs, 1, WindowWidth::Normal)
}

fn dispatch_at(
    g: &mut WindowGroup,
    taffy: &TaffyTree<()>,
    node: NodeId,
    event: &WindowEvent,
) -> bool {
    Dispatch::new().layout(taffy, node).send(g, event).handled
}

#[allow(
    clippy::type_complexity,
    reason = "test fixture bundling everything one maximize-click test needs; a named struct would only move the same fields, not reduce them"
)]
#[test]
fn tab_bar_requests_reorder_search_and_pin_keep_hosts_aligned() {
    {
        use std::collections::HashMap;
        use taffy::Style;
        use taffy::prelude::TaffyMaxContent;

        let theme = Theme::dark();
        let mut g = make_group();
        assert_eq!(
            g.take_pending_tab_reorder(),
            None,
            "nothing pending initially"
        );

        let mut taffy = TaffyTree::<()>::new();
        let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
        let node = g.build_layout(&mut taffy, &theme, &mut node_map);
        let root = taffy.parent_node(Style::default(), &[node]);
        taffy
            .compute_layout(root, taffy::Size::MAX_CONTENT)
            .unwrap();

        g.bar_mut_for_tests().pending_reorder = Some((0, 1));
        let moved = WindowEvent::CursorMoved {
            device_id: winit::event::DeviceId::dummy(),
            position: winit::dpi::PhysicalPosition::new(9000.0, 9000.0),
        };
        dispatch_at(&mut g, &taffy, node, &moved);

        assert_eq!(
            g.take_pending_tab_reorder(),
            Some((0, 1)),
            "a harvested reorder publishes (from, to) for the shell exactly once",
        );
        assert_eq!(
            g.take_pending_tab_reorder(),
            None,
            "take-pattern: drained on the first poll",
        );
    }

    // Case `reorder_keeps_hosts_and_bar_tabs_aligned`.
    // Parallel-vec invariant: drift between `bar.tabs()` and `hosts`
    // would render tab pill `i`'s content from a different host.
    {
        use crate::widget::{AppKind, TabDescriptor, TileContent};
        use std::collections::HashMap;
        use taffy::Style;
        use taffy::prelude::TaffyMaxContent;

        fn make_group_4() -> WindowGroup {
            let tabs: Vec<(TabDescriptor, TileContent)> = (1..=4u64)
                .map(|i| {
                    (
                        TabDescriptor::new(i, format!("T{i}")),
                        TileContent::Stub {
                            kind: AppKind::Settings,
                            label: format!("s{i}"),
                        },
                    )
                })
                .collect();
            WindowGroup::new(10, tabs, 1, WindowWidth::Normal)
        }

        let cases: &[(usize, usize)] = &[(0, 2), (3, 1), (1, 2), (2, 2), (0, 3)];

        for &(from, to) in cases {
            let mut g = make_group_4();

            let theme = Theme::dark();
            let mut taffy = TaffyTree::<()>::new();
            let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
            let group_node = g.build_layout(&mut taffy, &theme, &mut node_map);
            let root = taffy.parent_node(Style::default(), &[group_node]);
            taffy
                .compute_layout(root, taffy::Size::MAX_CONTENT)
                .unwrap();
            g.apply_layout(&taffy, group_node, Point::ZERO);

            for (i, t) in g.bar.tabs().iter().enumerate() {
                assert_eq!(
                    t.id, g.hosts[i].0,
                    "precondition: tab[{i}] id must equal hosts[{i}].0",
                );
            }

            g.bar.pending_reorder = Some((from, to));

            let moved = WindowEvent::CursorMoved {
                device_id: winit::event::DeviceId::dummy(),
                position: winit::dpi::PhysicalPosition::new(9000.0, 9000.0),
            };
            dispatch_at(&mut g, &taffy, group_node, &moved);

            assert_eq!(
                g.bar.tabs().len(),
                g.hosts.len(),
                "case ({from}, {to}): bar.tabs().len() must equal hosts.len() after reorder",
            );
            for (i, t) in g.bar.tabs().iter().enumerate() {
                assert_eq!(
                    t.id, g.hosts[i].0,
                    "case ({from}, {to}): bar.tabs()[{i}].id must equal hosts[{i}].0 after reorder",
                );
            }
        }
    }

    {
        let mut g = make_group();
        assert!(!g.search_active, "fresh group must not be searching");
        g.bar.pending_search = Some(1);
        if let Some(_id) = g.bar.take_search() {
            g.search_active = !g.search_active;
        }
        assert!(g.search_active, "drain should flip search_active on");
        g.bar.pending_search = Some(1);
        if let Some(_id) = g.bar.take_search() {
            g.search_active = !g.search_active;
        }
        assert!(
            !g.search_active,
            "second drain should flip search_active off"
        );
    }

    {
        use crate::theme::Theme;
        use std::collections::HashMap;
        use taffy::Style;
        use taffy::prelude::TaffyMaxContent;
        use winit::event::Modifiers;
        use winit::keyboard::{Key, ModifiersState, SmolStr};

        fn key_press(logical_key: Key) -> WindowEvent {
            crate::synthetic_input::keyboard_input(crate::synthetic_input::key_event(
                logical_key,
                None,
                ElementState::Pressed,
            ))
        }

        let mut g = make_group();
        let active = g.bar.active_id();
        assert!(
            !g.bar.is_pinned(active),
            "fresh group: active tab is unpinned"
        );

        let mods = Modifiers::from(ModifiersState::CONTROL | ModifiersState::SHIFT);
        let event = key_press(Key::Character(SmolStr::new_inline("p")));

        let theme = Theme::dark();
        let mut taffy = TaffyTree::<()>::new();
        let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
        let group_node = g.build_layout(&mut taffy, &theme, &mut node_map);
        let root = taffy.parent_node(Style::default(), &[group_node]);
        taffy
            .compute_layout(root, taffy::Size::MAX_CONTENT)
            .unwrap();
        g.apply_layout(&taffy, group_node, Point::ZERO);

        let mut ctrl_shift = Dispatch::new().layout(&taffy, group_node).modifiers(mods);
        let handled = ctrl_shift.send(&mut g, &event).handled;
        assert!(handled, "Ctrl+Shift+P press must be claimed");
        assert!(
            g.bar.is_pinned(active),
            "first Ctrl+Shift+P must pin the active tab",
        );

        let handled = ctrl_shift.send(&mut g, &event).handled;
        assert!(handled, "second Ctrl+Shift+P press is also claimed");
        assert!(
            !g.bar.is_pinned(active),
            "second Ctrl+Shift+P must unpin the active tab",
        );

        dispatch_at(&mut g, &taffy, group_node, &event);
        assert!(
            !g.bar.is_pinned(active),
            "plain `P` (no Ctrl+Shift) must not toggle pin state",
        );
    }
}

pub(super) fn g_normal_width() -> WindowWidth {
    WindowWidth::Normal
}

#[test]
fn deferred_close_retains_host_pill_order_and_focus() {
    let mut group = make_group();
    assert!(group.request_close_tab(1));
    assert!(group.has_closing_host(1));
    assert!(group.cancel_tab_close(1));
    group.sweep_expired_closing_hosts();
    assert_eq!(
        group
            .bar
            .tabs()
            .iter()
            .map(|tab| tab.id)
            .collect::<Vec<_>>(),
        [1, 2]
    );
    assert_eq!(group.active_tab_id(), 1);
    assert!(group.has_host(1));
    assert!(!group.has_closing_host(1));
    assert!(!group.bar.is_closing(1));
    assert!(
        !group.cancel_tab_close(1),
        "restoring twice must not duplicate the pill"
    );
    assert_eq!(group.tab_count(), 2);
}

#[test]
fn refused_final_tab_close_restores_live_group_input() {
    let mut group = WindowGroup::new(
        10,
        vec![(
            TabDescriptor::new(1, "solo"),
            TileContent::Stub {
                kind: AppKind::Settings,
                label: "a".into(),
            },
        )],
        1,
        WindowWidth::Normal,
    );
    assert!(group.request_close_tab(1));
    assert!(group.is_closing());
    assert_eq!(crate::bound_dispatch::keyboard_input_scope(&group), None);
    assert!(group.cancel_tab_close(1));
    assert!(!group.is_closing());
    assert!(!group.has_closing_host(1));
    assert_eq!(crate::bound_dispatch::keyboard_input_scope(&group), Some(1));
    assert!(group.has_host(1));
}

#[test]
fn closing_tab_hosts_fade_out_and_the_last_one_closes_the_group() {
    // One instant for the whole test: each case backdates its own
    // timestamps, so a descheduled runner cannot move them.
    crate::time::testing::with_monotonic_time(crate::time::now(), || {
        // Case `detach_tab_clears_closing_state_for_moved_tab`.
        // Its id sits in `closing_hosts` — a stale entry whose timer never
        // expires once the host is moved — so `needs_animation_frame` would
        // otherwise keep pumping redraws for a host that's gone.
        {
            let mut g = make_group();
            g.closing_hosts.insert(1, crate::time::now());
            assert!(
                g.closing_hosts.contains_key(&1),
                "precondition: closing_hosts carries the moved tab id",
            );

            let detached = g.detach_tab(1);
            assert!(detached.is_some(), "detach_tab returns the moved tab");

            assert!(
                !g.closing_hosts.contains_key(&1),
                "detach_tab must clear closing_hosts for the moved tab so \
             needs_animation_frame stops pumping for a host that's gone",
            );
            assert!(
                !g.bar.closing_tab_ids().contains(&1),
                "detach_tab must call bar.finalize_close so any pill-side \
             fade state for the moved id is also dropped",
            );
        }

        {
            use crate::theme::Theme;
            use std::collections::HashMap;
            use taffy::Style;
            use taffy::prelude::TaffyMaxContent;

            let mut g = make_group();
            let theme = Theme::dark();
            let mut taffy = TaffyTree::<()>::new();
            let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
            let group_node = g.build_layout(&mut taffy, &theme, &mut node_map);
            let root = taffy.parent_node(Style::default(), &[group_node]);
            taffy
                .compute_layout(root, taffy::Size::MAX_CONTENT)
                .unwrap();
            g.apply_layout(&taffy, group_node, Point::ZERO);

            g.bar_mut().start_closing(2);

            let moved = WindowEvent::CursorMoved {
                device_id: winit::event::DeviceId::dummy(),
                position: winit::dpi::PhysicalPosition::new(1000.0, 1000.0),
            };
            dispatch_at(&mut g, &taffy, group_node, &moved);

            assert!(
                g.has_closing_host(2),
                "drain must latch the host-level close fade",
            );
            assert!(
                g.needs_animation_frame(),
                "in-flight close fade must pump redraws",
            );
            assert!(g.has_host(2), "host stays in `hosts` for the fade duration");
            let host_count_during = g.host_count();

            g.force_expire_closing(2);
            let moved2 = WindowEvent::CursorMoved {
                device_id: winit::event::DeviceId::dummy(),
                position: winit::dpi::PhysicalPosition::new(1000.0, 1000.0),
            };
            dispatch_at(&mut g, &taffy, group_node, &moved2);
            assert!(
                !g.has_host(2),
                "sweep drops the host after the fade expires",
            );
            assert_eq!(
                g.host_count(),
                host_count_during - 1,
                "exactly one host removed",
            );
            assert!(
                !g.has_closing_host(2),
                "sweep clears the closing_hosts entry",
            );
        }

        // Case `closing_last_tab_kicks_off_group_close`.
        // `GroupRow` relies on `is_closing()` → `close_complete()` to
        // sequence the prune, so if `closing_at` never flips the group
        // stays around as an empty shell forever.
        {
            use crate::theme::Theme;
            use std::collections::HashMap;
            use taffy::Style;
            use taffy::prelude::TaffyMaxContent;

            let tabs: Vec<(TabDescriptor, TileContent)> = vec![(
                TabDescriptor::new(1, "solo"),
                TileContent::Stub {
                    kind: AppKind::Settings,
                    label: "a".into(),
                },
            )];
            let mut g = WindowGroup::new(10, tabs, 1, WindowWidth::Normal);

            let theme = Theme::dark();
            let mut taffy = TaffyTree::<()>::new();
            let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
            let group_node = g.build_layout(&mut taffy, &theme, &mut node_map);
            let root = taffy.parent_node(Style::default(), &[group_node]);
            taffy
                .compute_layout(root, taffy::Size::MAX_CONTENT)
                .unwrap();
            g.apply_layout(&taffy, group_node, Point::ZERO);

            g.bar_mut().start_closing(1);

            let moved = WindowEvent::CursorMoved {
                device_id: winit::event::DeviceId::dummy(),
                position: winit::dpi::PhysicalPosition::new(1000.0, 1000.0),
            };
            dispatch_at(&mut g, &taffy, group_node, &moved);
            assert!(
                g.has_closing_host(1),
                "precondition: host-level close latched",
            );
            assert!(
                g.is_closing(),
                "the last tab and group close together, before canonical reconciliation",
            );

            // A later event must not restart or prolong the already-owned group close.
            g.force_expire_closing(1);
            let moved2 = WindowEvent::CursorMoved {
                device_id: winit::event::DeviceId::dummy(),
                position: winit::dpi::PhysicalPosition::new(1000.0, 1000.0),
            };
            dispatch_at(&mut g, &taffy, group_node, &moved2);
            assert!(
                g.is_closing(),
                "the group close remains latched without a second fade",
            );
        }
    });
}

/// What each tab of a window was asked, as a window's lifecycle, history and collaboration walk
/// the tabs.
#[derive(Default)]
struct Reached {
    unsaved: std::cell::Cell<u32>,
    checkpoint: std::cell::Cell<u32>,
    finish: std::cell::Cell<u32>,
    history: std::cell::Cell<u32>,
}

struct TabProbe(std::rc::Rc<Reached>);

impl Widget for TabProbe {
    crate::provides!(
        dyn crate::history_mode::capability::FrameworkHistoryTopology,
        dyn crate::component_lifecycle::ComponentLifecycle
    );
    crate::testing::inert_widget_methods!(name = "TabProbe");
    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        _: &Theme,
        _: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        taffy.leaf_node(Style::default())
    }
}

fn bump(cell: &std::cell::Cell<u32>) {
    cell.set(cell.get() + 1);
}

impl crate::component_lifecycle::ComponentLifecycle for TabProbe {
    fn unsaved_documents(&self, _: &AutoCommit) -> Vec<crate::close_admission::UnsavedDocument> {
        bump(&self.0.unsaved);
        vec![]
    }
    fn checkpoint_components(&mut self) -> Vec<String> {
        bump(&self.0.checkpoint);
        vec![]
    }
    fn finish_admitted_close(&mut self) -> Vec<crate::application::ComponentCloseReport> {
        bump(&self.0.finish);
        vec![]
    }
}

impl crate::history_mode::capability::FrameworkHistoryTopology for TabProbe {
    fn visit_framework_history(
        &self,
        _: &mut dyn FnMut(&crate::history_mode::FrameworkHistorySurface, &dyn Widget),
    ) {
        bump(&self.0.history);
    }
    fn visit_framework_history_mut(
        &mut self,
        _: &mut dyn FnMut(&mut crate::history_mode::FrameworkHistorySurface, &mut dyn Widget),
    ) {
        bump(&self.0.history);
    }
}

crate::testing::test_app_widget!(
    TabProbe,
    "Tab probe",
    icon = crate::widget::app_widget::IconDescriptor::Glyph("T"),
    kind = AppKind::Settings
);

/// A tab in the background is still part of its window: others' collaboration reaches it, a
/// close asks it for unsaved work and checkpoints and finishes it, and history walks it, as it
/// does the tab in front.
#[test]
fn a_background_tab_is_still_part_of_its_window() {
    let active = crate::history_mode::isolation_tests::native_effect_probe_parts(
        "active-session",
        "fixture.active-session",
    );
    let inactive = crate::history_mode::isolation_tests::native_effect_probe_parts(
        "inactive-session",
        "fixture.inactive-session",
    );
    let heard = [&active.collab_events, &inactive.collab_events].map(std::rc::Rc::clone);
    let mut group = WindowGroup::new(
        10,
        vec![
            (
                TabDescriptor::new(1, "Active"),
                TileContent::App(active.root),
            ),
            (
                TabDescriptor::new(2, "Inactive"),
                TileContent::App(inactive.root),
            ),
        ],
        1,
        WindowWidth::Normal,
    );
    crate::widget::handle_collab_event(
        &mut group,
        &::collaboration::CollabEvent::FollowChanged { peer_id: None },
    );
    assert_eq!(
        heard.map(|events| events.get()),
        [1, 1],
        "both tabs hear collaboration"
    );

    let reached = [std::rc::Rc::<Reached>::default(), std::rc::Rc::default()];
    let mut group = WindowGroup::new(
        10,
        vec![
            (
                TabDescriptor::new(1, "Active"),
                TileContent::App(Box::new(TabProbe(reached[0].clone()))),
            ),
            (
                TabDescriptor::new(2, "Inactive"),
                TileContent::App(Box::new(TabProbe(reached[1].clone()))),
            ),
        ],
        1,
        WindowWidth::Normal,
    );
    crate::component_lifecycle::unsaved_documents(&group, &AutoCommit::new());
    crate::component_lifecycle::checkpoint_components(&mut group);
    crate::component_lifecycle::finish_admitted_close(&mut group);
    crate::history_mode::capability::visit_framework_history(&group, &mut |_, _| {});
    crate::history_mode::capability::visit_framework_history_mut(&mut group, &mut |_, _| {});
    for (tab, reached) in ["active", "background"].into_iter().zip(reached) {
        assert_eq!(
            reached.unsaved.get(),
            1,
            "{tab}: asked for unsaved work at close"
        );
        assert_eq!(reached.checkpoint.get(), 1, "{tab}: checkpointed");
        assert_eq!(reached.finish.get(), 1, "{tab}: finished");
        assert_eq!(reached.history.get(), 2, "{tab}: walked by history");
    }
}
