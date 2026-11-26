use super::*;
use crate::testing::Dispatch;
use crate::widget::{EdgeGrab, TabDescriptor, TileContent, WindowEdge, WindowWidth, min_group_px};
use winit::event::MouseScrollDelta;

/// `t` minus `ms` milliseconds, saturating rather than panicking in
/// the astronomically unlikely case the subtraction would underflow
/// `Instant`'s valid range.
fn earlier(t: Instant, ms: u64) -> Instant {
    t.checked_sub(Duration::from_millis(ms)).unwrap_or(t)
}

pub(super) fn make_group(id: u64, tab_ids: &[u64]) -> WindowGroup {
    let tabs: Vec<(TabDescriptor, TileContent)> = tab_ids
        .iter()
        .map(|t| {
            (
                TabDescriptor::new(*t, format!("T{t}")),
                TileContent::Stub {
                    kind: crate::widget::AppKind::Settings,
                    label: format!("S{t}"),
                },
            )
        })
        .collect();
    let active = tab_ids[0];
    WindowGroup::new(id, tabs, active, WindowWidth::Normal)
}

tilo_test_env::scenario! {
    fn tabs_and_groups_move_split_and_close_in_the_strip;

    phase closing_the_last_window_at_the_end_rests_on_the_windows_that_remain;
    {
        let theme = Theme::dark().with_reduce_motion(true);
        let (row, _taffy, _root) = make_row_with_groups_with_taffy(&[&[1], &[2], &[3]]);
        let (mut row, _taffy, _root) = relayout_row_owned_with(row, 1200.0, &theme);
        let end = row.max_scroll_x();
        assert!(end > 1.0, "fixture: three windows overflow the strip");
        row.set_scroll_x_for_test(end);
        let (mut row, _taffy, _root) = relayout_row_owned_with(row, 1200.0, &theme);
        assert!(
            (row.scroll_x() - end).abs() < 0.5,
            "fixture: panned to the last window"
        );
        let last = row.groups[2].id;
        assert!(row.remove_group_by_id(last));
        let (row, _taffy, _root) = relayout_row_owned_with(row, 1200.0, &theme);
        let (row, _taffy, _root) = relayout_row_owned_with(row, 1200.0, &theme);
        assert!(
            row.pan_max_scroll_x() > row.max_scroll_x() + 1.0,
            "fixture: the strip keeps an allowance past its last window"
        );
        assert!(
            row.scroll_x() <= row.max_scroll_x() + 0.5,
            "the pan rests within the windows: {} past an end of {}",
            row.scroll_x(),
            row.max_scroll_x()
        );
    }

    phase drop_into_new_window_from_strip_areas_and_closing_groups;
    {
        fn drop_into_new_window_works_when_release_is_in_empty_strip_area_below_group() {
            let row = make_row_with_groups(&[&[1, 2, 3, 4]]);
            let release = Point::new(row.groups[0].get_rect().x1 + 4.0, row.rect.y1 + 20.0);
            assert_eq!(
                row.gap_index_for(release),
                Some(1),
                "release below the row's vertical band should still resolve to the right-of-last gap so \
         the tab splits into a new window"
            );
        }

        fn drop_into_new_window_works_when_release_is_above_row() {
            let row = make_row_with_groups(&[&[1, 2], &[10, 11]]);
            let r0 = row.groups[0].get_rect();
            let release = Point::new(r0.x0 - 4.0, row.rect.y0 - 15.0);
            assert_eq!(
                row.gap_index_for(release),
                Some(0),
                "release above the row should resolve to the left-of-first gap"
            );
        }

        fn drop_onto_closing_group_creates_new_group_instead() {
            use winit::event::WindowEvent;

            let (mut row, taffy, root) = make_row_with_groups_with_taffy(&[&[1, 2], &[10, 11]]);

            row.groups[1].set_closing_at_for_tests(crate::time::now());
            assert!(row.groups[1].is_closing());
            assert!(!row.groups[1].close_complete());
            let b_tabs_before: Vec<u64> = row.groups[1].tabs().iter().map(|t| t.id).collect();
            assert_eq!(b_tabs_before, vec![10, 11]);

            let b_bar = row.groups[1].bar_rect();
            let release = Point::new(
                f64::midpoint(b_bar.x0, b_bar.x1),
                f64::midpoint(b_bar.y0, b_bar.y1),
            );
            row.tab_drop_target = row.tab_drop_target_at(release);
            row.groups[0].bar_mut().pending_detach = Some((1, release));

            let event = WindowEvent::MouseInput {
                device_id: winit::event::DeviceId::dummy(),
                state: ElementState::Released,
                button: MouseButton::Left,
            };
            dispatch_at(&mut row, &taffy, root, &event);

            let b_idx = row
                .groups
                .iter()
                .position(WindowGroup::is_closing)
                .expect("B should still be present (mid-fade)");
            let b_tabs_after: Vec<u64> = row.groups[b_idx].tabs().iter().map(|t| t.id).collect();
            assert_eq!(
                b_tabs_after, b_tabs_before,
                "closing group's tabs must be unchanged — the drop must not insert into it",
            );

            assert_eq!(
                row.groups.len(),
                3,
                "drop-onto-closing falls through to split-into-new-group",
            );
            let a_tabs: Vec<u64> = row.groups[0].tabs().iter().map(|t| t.id).collect();
            assert_eq!(a_tabs, vec![2], "A lost its dragged tab (id 1)");
            assert!(
                row.groups[2].is_closing(),
                "B (closing) should still be present at the right of the row",
            );
            let new_group_tabs: Vec<u64> = row.groups[1].tabs().iter().map(|t| t.id).collect();
            assert_eq!(
                new_group_tabs,
                vec![1],
                "dragged tab must end up in a fresh group, not in the closing group",
            );
            assert!(
                !row.groups[1].is_closing(),
                "the new group must NOT inherit the closing state",
            );
        }

        fn drop_in_sibling_tile_area_left_half_prepends_right_half_appends() {
            use winit::event::WindowEvent;

            let (mut row, taffy, root) = make_row_with_groups_with_taffy(&[&[1, 2], &[10, 11, 12]]);
            let b_rect = row.groups[1].get_rect();
            let b_bar = row.groups[1].bar_rect();

            let release_left = Point::new(
                b_rect.x0 + (b_rect.width() * 0.25),
                f64::midpoint(b_bar.y1, b_rect.y1),
            );
            assert!(b_rect.contains(release_left));
            assert!(!b_bar.contains(release_left));
            assert!(release_left.x < b_rect.center().x);

            row.tab_drop_target = row.tab_drop_target_at(release_left);
            row.groups[0].bar_mut().pending_detach = Some((1, release_left));
            let release_event = WindowEvent::MouseInput {
                device_id: winit::event::DeviceId::dummy(),
                state: ElementState::Released,
                button: MouseButton::Left,
            };
            dispatch_at(&mut row, &taffy, root, &release_event);

            let b_ids: Vec<u64> = row.groups[1].tabs().iter().map(|t| t.id).collect();
            assert_eq!(
                b_ids,
                vec![1, 10, 11, 12],
                "left-half tile drop must prepend the dragged tab to the target's tab list",
            );
            let a_ids: Vec<u64> = row.groups[0].tabs().iter().map(|t| t.id).collect();
            assert_eq!(
                a_ids,
                vec![2],
                "source group must lose the dragged tab to the target",
            );
            assert_eq!(row.groups[1].active_tab_id(), 1);

            let (mut row, taffy, root) = make_row_with_groups_with_taffy(&[&[1, 2], &[10, 11, 12]]);
            let b_rect = row.groups[1].get_rect();
            let b_bar = row.groups[1].bar_rect();
            let release_right = Point::new(
                b_rect.x0 + (b_rect.width() * 0.75),
                f64::midpoint(b_bar.y1, b_rect.y1),
            );
            assert!(b_rect.contains(release_right));
            assert!(!b_bar.contains(release_right));
            assert!(release_right.x > b_rect.center().x);

            row.tab_drop_target = row.tab_drop_target_at(release_right);
            row.groups[0].bar_mut().pending_detach = Some((1, release_right));
            dispatch_at(&mut row, &taffy, root, &release_event);

            let b_ids: Vec<u64> = row.groups[1].tabs().iter().map(|t| t.id).collect();
            assert_eq!(
                b_ids,
                vec![10, 11, 12, 1],
                "right-half tile drop must append the dragged tab to the target's tab list",
            );
        }

        /// A release in a *closing* sibling's content area falls through
        /// every resolution branch (`gap_index_for` also returns `None` since
        /// it's inside a group's rect). Pins the fallback: no valid target
        /// means the tab stays in source.
        fn cross_group_drop_into_closing_content_area_keeps_tab_in_source() {
            use winit::event::WindowEvent;

            let (mut row, taffy, root) = make_row_with_groups_with_taffy(&[&[1, 2], &[10, 11]]);
            row.groups[1].set_closing_at_for_tests(crate::time::now());
            assert!(row.groups[1].is_closing());
            assert!(!row.groups[1].close_complete());

            let b_rect = row.groups[1].get_rect();
            let b_bar = row.groups[1].bar_rect();
            let release = Point::new(
                f64::midpoint(b_rect.x0, b_rect.x1),
                f64::midpoint(b_bar.y1, b_rect.y1),
            );
            assert!(
                b_rect.contains(release),
                "precondition: release is inside B's outer rect",
            );
            assert!(
                !b_bar.contains(release),
                "precondition: release is OUTSIDE B's bar rect (content area)",
            );
            assert_eq!(
                row.gap_index_for(release),
                None,
                "precondition: gap_index_for returns None for inside-group releases",
            );

            let a_tabs_before: Vec<u64> = row.groups[0].tabs().iter().map(|t| t.id).collect();
            row.tab_drop_target = row.tab_drop_target_at(release);
            row.groups[0].bar_mut().pending_detach = Some((1, release));

            let release_event = WindowEvent::MouseInput {
                device_id: winit::event::DeviceId::dummy(),
                state: ElementState::Released,
                button: MouseButton::Left,
            };
            dispatch_at(&mut row, &taffy, root, &release_event);

            let a_tabs_after: Vec<u64> = row.groups[0].tabs().iter().map(|t| t.id).collect();
            assert_eq!(
                a_tabs_after, a_tabs_before,
                "source group must keep the dragged tab when no valid target resolves",
            );
            assert_eq!(
                row.groups.len(),
                2,
                "no new group should be spliced in for a cancelled drop",
            );
            let b_idx = row
                .groups
                .iter()
                .position(WindowGroup::is_closing)
                .expect("B should still be present mid-fade");
            let b_tabs_after: Vec<u64> = row.groups[b_idx].tabs().iter().map(|t| t.id).collect();
            assert_eq!(
                b_tabs_after,
                vec![10, 11],
                "closing group's tabs must be untouched — drop did not insert into it",
            );
            assert!(
                row.groups[0].bar_mut().pending_detach.is_none(),
                "post-dispatch sweep must drain pending_detach even when no target resolves",
            );
        }

        fn drop_in_visible_trailing_area_creates_new_group_at_end() {
            let (mut row, _t, _r) = make_overflow_row(3, 1000.0);
            let last_rect = row.groups.last().unwrap().get_rect();
            let release = Point::new(last_rect.x1 + 50.0, last_rect.y0 + 20.0);
            assert_eq!(
                row.gap_index_for(release),
                Some(3),
                "release past the last group's right edge must resolve to gap_idx=N"
            );
            let src_tab = row.groups[0].tabs()[0].id;
            assert!(
                row.split_tab_into_new_group(0, src_tab, 3),
                "split_tab_into_new_group must succeed at the trailing gap"
            );
            // Source had only one tab, so it's pruned; net group count is 3.
            assert_eq!(row.groups.len(), 3);
            let last_ids: Vec<u64> = row
                .groups
                .last()
                .unwrap()
                .tabs()
                .iter()
                .map(|t| t.id)
                .collect();
            assert_eq!(last_ids, vec![src_tab]);
        }

        drop_into_new_window_works_when_release_is_in_empty_strip_area_below_group();
        drop_into_new_window_works_when_release_is_above_row();
        drop_onto_closing_group_creates_new_group_instead();
        drop_in_sibling_tile_area_left_half_prepends_right_half_appends();
        cross_group_drop_into_closing_content_area_keeps_tab_in_source();
        drop_in_visible_trailing_area_creates_new_group_at_end();
    }

    phase closing_groups_fade_before_pruning_with_aligned_nodes;
    {
        fn group_pruned_after_close_fade_completes() {
            use winit::event::WindowEvent;

            let (mut row, taffy, root) = make_row_with_groups_with_taffy(&[&[1], &[2]]);
            row.groups[0].set_closing_at_for_tests(
                crate::time::now()
                    .checked_sub(
                        Theme::dark()
                            .tokens
                            .motion
                            .durations
                            .base()
                            .saturating_add(std::time::Duration::from_millis(40)),
                    )
                    .expect("representable close deadline"),
            );
            assert_eq!(row.groups.len(), 2, "precondition: two groups before prune");

            let event = WindowEvent::CursorMoved {
                device_id: winit::event::DeviceId::dummy(),
                position: winit::dpi::PhysicalPosition::new(5000.0, 5000.0),
            };
            dispatch_at(&mut row, &taffy, root, &event);

            assert_eq!(
                row.groups.len(),
                1,
                "fully-faded group must be dropped from self.groups",
            );
            assert!(
                row.take_structure_dirty(),
                "prune must flag structure_dirty so App rebuilds taffy",
            );
        }

        /// The prune sweep removes faded groups from `self.groups` while
        /// `node_ids` stays stale until the next `build_layout`; the
        /// production `debug_assert` pins that. Guarded since the asserts
        /// exercised are themselves debug-only.
        #[cfg(debug_assertions)]
        fn group_prune_keeps_node_ids_aligned_in_debug() {
            use winit::event::WindowEvent;

            let (mut row, taffy, root) = make_row_with_groups_with_taffy(&[&[1], &[2], &[3]]);
            row.groups[0].set_closing_at_for_tests(earlier(crate::time::now(), 500));
            row.groups[2].set_closing_at_for_tests(earlier(crate::time::now(), 500));

            let groups_pre = row.groups.len();
            let node_ids_pre = row.node_ids.len();
            assert_eq!(
                groups_pre, 3,
                "precondition: row was seeded with three groups",
            );
            assert_eq!(
                node_ids_pre, groups_pre,
                "precondition: node_ids must equal groups.len() after build_layout",
            );

            let event = WindowEvent::CursorMoved {
                device_id: winit::event::DeviceId::dummy(),
                position: winit::dpi::PhysicalPosition::new(5000.0, 5000.0),
            };
            dispatch_at(&mut row, &taffy, root, &event);

            assert_eq!(
                row.groups.len(),
                1,
                "two fully-faded groups must be pruned in a single sweep",
            );

            // Only refused outcome: drift with no rebuild check set, which
            // would crash apply_layout.
            let aligned_now = row.node_ids.len() == row.groups.len();
            let dirty_for_rebuild = row.take_structure_dirty();
            assert!(
                aligned_now || dirty_for_rebuild,
                "after prune: node_ids must either match groups.len() ({}) directly \
         or structure_dirty must be set so build_layout rebuilds them \
         (node_ids.len() = {})",
                row.groups.len(),
                row.node_ids.len(),
            );
        }

        group_pruned_after_close_fade_completes();
        #[cfg(debug_assertions)]
        group_prune_keeps_node_ids_aligned_in_debug();
    }
}

/// 1200 puts the width tiers on whole hundreds (Narrow 400 / Normal
/// 800 / Wide 1200), so fraction-based group widths get round pixel
/// numbers to assert against.
pub(super) const FIXTURE_USABLE_PX: f64 = 1200.0;

fn one_display(usable: f64) -> Vec<OutputSpan> {
    vec![OutputSpan {
        x: 0.0,
        width: usable,
        usable,
    }]
}

pub(super) fn make_row_with_groups_with_taffy(
    tab_specs: &[&[u64]],
) -> (GroupRow, TaffyTree<()>, NodeId) {
    make_row_with_groups_on_display(tab_specs, FIXTURE_USABLE_PX, 2000.0)
}

pub(super) fn relayout_row_owned(
    row: GroupRow,
    viewport_w: f64,
) -> (GroupRow, TaffyTree<()>, NodeId) {
    relayout_row_owned_with(row, viewport_w, &Theme::dark())
}

pub(super) fn relayout_row_owned_with(
    mut row: GroupRow,
    viewport_w: f64,
    theme: &Theme,
) -> (GroupRow, TaffyTree<()>, NodeId) {
    use std::collections::HashMap;
    use taffy::{AvailableSpace, Size as TSize, TaffyTree};

    let mut taffy: TaffyTree<()> = TaffyTree::new();
    let mut node_map: HashMap<taffy::NodeId, *mut dyn Widget> = HashMap::new();
    let root = row.build_layout(&mut taffy, theme, &mut node_map);
    let viewport = taffy.parent_node(
        Style {
            size: Size {
                width: Dimension::length(tilo_ui_base::num::f32_of_f64(viewport_w)),
                height: Dimension::length(600.0),
            },
            ..Default::default()
        },
        &[root],
    );
    taffy
        .compute_layout_with_measure(
            viewport,
            TSize {
                width: AvailableSpace::Definite(tilo_ui_base::num::f32_of_f64(viewport_w)),
                height: AvailableSpace::Definite(600.0),
            },
            |_, _, _, _, _| taffy::Size::ZERO,
        )
        .expect("taffy compute_layout");
    row.apply_layout(&taffy, root, Point::ORIGIN);
    (row, taffy, root)
}

fn make_row_with_groups_on_display(
    tab_specs: &[&[u64]],
    usable: f64,
    viewport_w: f64,
) -> (GroupRow, TaffyTree<()>, NodeId) {
    use std::collections::HashMap;
    use taffy::{AvailableSpace, Size as TSize, TaffyTree};

    let groups: Vec<WindowGroup> = tab_specs
        .iter()
        .enumerate()
        .map(|(i, ids)| make_group((i as u64).saturating_mul(100).saturating_add(100), ids))
        .collect();
    let mut row = GroupRow::new(groups);
    row.set_output_spans(&one_display(usable));
    let theme = Theme::dark();
    let mut taffy: TaffyTree<()> = TaffyTree::new();
    let mut node_map: HashMap<taffy::NodeId, *mut dyn Widget> = HashMap::new();
    let root = row.build_layout(&mut taffy, &theme, &mut node_map);
    let viewport = taffy.parent_node(
        Style {
            size: Size {
                width: Dimension::length(tilo_ui_base::num::f32_of_f64(viewport_w)),
                height: Dimension::length(600.0),
            },
            ..Default::default()
        },
        &[root],
    );
    taffy
        .compute_layout_with_measure(
            viewport,
            TSize {
                width: AvailableSpace::Definite(tilo_ui_base::num::f32_of_f64(viewport_w)),
                height: AvailableSpace::Definite(600.0),
            },
            |_, _, _, _, _| taffy::Size::ZERO,
        )
        .expect("taffy compute_layout");
    row.apply_layout(&taffy, root, Point::ORIGIN);
    (row, taffy, root)
}

fn make_row_with_groups(tab_specs: &[&[u64]]) -> GroupRow {
    make_row_with_groups_with_taffy(tab_specs).0
}

/// A width change lands on a slot whose opening spring is still in
/// flight, with a second window arriving beside it. Every frame after
/// that must settle: the springs move the slots, so each frame's rebuild
/// reports work and the composition has to come back to rest before the
/// next one — this is the frame the compositor draws.
#[test]
fn a_width_change_on_an_opening_window_lets_its_frames_settle() {
    tilo_ui_base::motion::forget_frames_for_tests();
    let mut row = strip_at(1, 1280.0, 0.0, crate::widget::WIDTH_NORMAL_PERMILLE);
    row.groups[0].start_spawn_animation();
    let t0 = crate::time::now();
    let frame_at = |row: &mut GroupRow, frame: u64| {
        crate::time::testing::with_monotonic_time(t0 + Duration::from_millis(frame * 16), || {
            row.frame_presented();
            let settle =
                tilo_ui_base::widget::layout::settle_widget_composition(false, |rebuild| {
                    if rebuild {
                        relayout_strip(row, 1280.0);
                    }
                    Ok((Some(Vec::new()), row.take_structure_dirty()))
                });
            if settle.is_ok() {
                row.mark_drawn();
            }
            settle
        })
    };
    for frame in 0..3u64 {
        let settle = frame_at(&mut row, frame);
        assert!(settle.is_ok(), "frame {frame} of the opening settles");
    }
    assert!(
        row.groups[0].has_width_spring_for_tests(),
        "the window is still opening"
    );
    row.insert_group(make_group(200, &[2]), 1);
    row.groups[1].start_spawn_animation();
    for frame in 3..6u64 {
        let settle = frame_at(&mut row, frame);
        assert!(settle.is_ok(), "frame {frame} of the arrival settles");
    }
    assert!(
        row.groups.iter().all(|g| g.has_width_spring_for_tests()),
        "both slots are still opening"
    );
    for group in &mut row.groups {
        group.set_width(WindowWidth::Fraction(333));
    }
    for frame in 6..30u64 {
        let settle = frame_at(&mut row, frame);
        assert!(
            settle.is_ok(),
            "frame {frame} after the width change settles: {settle:?}"
        );
    }
}

pub(super) fn drawn_strip(windows: u64) -> (GroupRow, Scene) {
    let specs: Vec<Vec<u64>> = (1..=windows).map(|id| vec![id]).collect();
    let specs: Vec<&[u64]> = specs.iter().map(Vec::as_slice).collect();
    let (mut row, _, _) = make_row_with_groups_on_display(&specs, FIXTURE_USABLE_PX, 2000.0);
    let mut scene = Scene::new();
    row.draw(&mut tilo_ui_base::draw::DrawCtx::new(
        &mut scene,
        Affine::IDENTITY,
        &mut TextRenderer::new(),
        &Theme::dark(),
        tilo_ui_base::presence::PresenceCtx::empty().draw_env(),
    ));
    (row, scene)
}

/// A point in group `idx`'s tab-bar background — right of the last tab pill, at the
/// bar's vertical centre — where a whole-group reorder grab lands.
pub(super) fn group_grab_point(row: &GroupRow, idx: usize) -> Point {
    let g = &row.groups[idx];
    let bar = g.bar_rect();
    let last = g.tab_rects().last().copied().unwrap_or(bar);
    Point::new(
        f64::midpoint(last.x1, bar.x1),
        f64::midpoint(bar.y0, bar.y1),
    )
}

// A press on a window's search control opens the platform search, which
// takes the pointer from there, so the release never reaches the row.
// Had the press lifted the window, the next motion would carry it as a
// ghost under the pointer with no button held.
#[test]
fn a_press_on_the_window_search_never_lifts_the_window() {
    let (mut row, taffy, root) = make_row_with_groups_with_taffy(&[&[1], &[2]]);
    let anchor = row.groups[0].window_search().anchor();
    assert!(anchor.area() > 0.0, "the bar shows its search control");
    let at = anchor.center();
    assert!(
        !row.groups[0].bar_grab_hit(at),
        "the search control is no drag handle"
    );
    press_at(&mut row, &taffy, root, at);
    dispatch_at(
        &mut row,
        &taffy,
        root,
        &move_to(Point::new(at.x + 150.0, at.y + 300.0)),
    );
    assert!(
        !row.group_drag_active(),
        "moving on after the press carries no window"
    );
}

pub(super) fn press_at(row: &mut GroupRow, taffy: &TaffyTree<()>, root: NodeId, p: Point) {
    dispatch_at(
        row,
        taffy,
        root,
        &WindowEvent::CursorMoved {
            device_id: winit::event::DeviceId::dummy(),
            position: winit::dpi::PhysicalPosition::new(p.x, p.y),
        },
    );
    dispatch_at(
        row,
        taffy,
        root,
        &WindowEvent::MouseInput {
            device_id: winit::event::DeviceId::dummy(),
            state: ElementState::Pressed,
            button: MouseButton::Left,
        },
    );
}

fn touch_event(id: u64, phase: winit::event::TouchPhase, p: Point) -> WindowEvent {
    WindowEvent::Touch(winit::event::Touch {
        device_id: winit::event::DeviceId::dummy(),
        phase,
        location: winit::dpi::PhysicalPosition::new(p.x, p.y),
        force: None,
        id,
    })
}

/// Send `ev` to `row` laid out at `root`; whether it was claimed.
pub(super) fn dispatch_at(
    row: &mut GroupRow,
    taffy: &TaffyTree<()>,
    root: NodeId,
    ev: &WindowEvent,
) -> bool {
    Dispatch::new().layout(taffy, root).send(row, ev).handled
}

tilo_test_env::scenario! {
    fn a_window_is_dragged_held_and_swiped_by_its_bar;

    phase bar_drag_reorders_groups_only_from_the_grab_zone;
    {
        fn bar_grab_point_hits_the_group_grab_zone() {
            let row = make_row_with_groups(&[&[1], &[2]]);
            let p = group_grab_point(&row, 0);
            assert!(
                row.groups[0].bar_grab_hit(p),
                "the trailing tab-bar background must be a valid group-grab zone",
            );
            assert_eq!(row.cursor_shape(p), Some(winit::window::CursorIcon::Grab));
            let pill = row.groups[0].tab_rects()[0].center();
            assert!(row.groups[0].bar_grab_hit(pill), "the lone tab is the title");
        }

        fn drag_group_to_end_reorders_group_vector_and_publishes_one_shot() {
            let (mut row, taffy, root) = make_row_with_groups_with_taffy(&[&[1], &[2], &[3]]);
            assert_eq!(row.group_ids(), vec![100, 200, 300]);

            let grab = group_grab_point(&row, 0);
            press_at(&mut row, &taffy, root, grab);
            let past = Point::new(row.groups[2].get_rect().center().x + 60.0, grab.y);
            dispatch_at(
                &mut row,
                &taffy,
                root,
                &WindowEvent::CursorMoved {
                    device_id: winit::event::DeviceId::dummy(),
                    position: winit::dpi::PhysicalPosition::new(past.x, past.y),
                },
            );
            assert!(
                row.group_drag_active(),
                "past-threshold move must engage the drag"
            );
            assert_eq!(
                row.group_reorder_target(),
                Some(3),
                "cursor past every group's centre targets the append gap",
            );
            dispatch_at(
                &mut row,
                &taffy,
                root,
                &WindowEvent::MouseInput {
                    device_id: winit::event::DeviceId::dummy(),
                    state: ElementState::Released,
                    button: MouseButton::Left,
                },
            );

            assert_eq!(
                row.group_ids(),
                vec![200, 300, 100],
                "dragged group must land at the dropped slot; neighbours shift left",
            );
            assert_eq!(
                row.take_pending_group_reorder(),
                Some((100, 2)),
                "commit must publish (group_id, landing_index) for the shell to persist",
            );
        }

        fn drag_group_backwards_between_two_groups() {
            let (mut row, taffy, root) = make_row_with_groups_with_taffy(&[&[1], &[2], &[3]]);
            let grab = group_grab_point(&row, 2);
            press_at(&mut row, &taffy, root, grab);
            let target = Point::new(row.groups[1].get_rect().center().x - 20.0, grab.y);
            dispatch_at(
                &mut row,
                &taffy,
                root,
                &WindowEvent::CursorMoved {
                    device_id: winit::event::DeviceId::dummy(),
                    position: winit::dpi::PhysicalPosition::new(target.x, target.y),
                },
            );
            dispatch_at(
                &mut row,
                &taffy,
                root,
                &WindowEvent::MouseInput {
                    device_id: winit::event::DeviceId::dummy(),
                    state: ElementState::Released,
                    button: MouseButton::Left,
                },
            );
            assert_eq!(row.group_ids(), vec![100, 300, 200]);
        }

        fn plain_click_on_bar_background_does_not_reorder() {
            let (mut row, taffy, root) = make_row_with_groups_with_taffy(&[&[1], &[2], &[3]]);
            let grab = group_grab_point(&row, 0);
            press_at(&mut row, &taffy, root, grab);
            dispatch_at(
                &mut row,
                &taffy,
                root,
                &WindowEvent::MouseInput {
                    device_id: winit::event::DeviceId::dummy(),
                    state: ElementState::Released,
                    button: MouseButton::Left,
                },
            );
            assert_eq!(
                row.group_ids(),
                vec![100, 200, 300],
                "a click must not reorder"
            );
            assert_eq!(row.take_pending_group_reorder(), None);
            assert!(!row.group_drag_active());
        }

        bar_grab_point_hits_the_group_grab_zone();
        drag_group_to_end_reorders_group_vector_and_publishes_one_shot();
        drag_group_backwards_between_two_groups();
        plain_click_on_bar_background_does_not_reorder();
    }

    phase a_held_window_resists_a_drag_and_stays_in_place;
    // A window someone else holds follows a drag a little, springs back
    // and never moves.
    {
        let (mut row, taffy, root) = make_row_with_groups_with_taffy(&[&[1], &[2], &[3]]);
        row.groups[0].set_tab_holder(
            1,
            Some(crate::widget::TabHolder {
                name: "Ben".into(),
                rgb: (200, 80, 40),
                keeps: true,
            }),
        );
        assert!(row.groups[0].is_held());
        let grab = group_grab_point(&row, 0);
        press_at(&mut row, &taffy, root, grab);
        let past = Point::new(row.groups[2].get_rect().center().x + 60.0, grab.y);
        dispatch_at(
            &mut row,
            &taffy,
            root,
            &WindowEvent::CursorMoved {
                device_id: winit::event::DeviceId::dummy(),
                position: winit::dpi::PhysicalPosition::new(past.x, past.y),
            },
        );
        assert_eq!(
            row.group_reorder_target(),
            None,
            "there is no gap to drop it in"
        );
        let pull = row.group_pull(100).expect("it follows the drag");
        assert!(
            pull.x > 0.0 && pull.x < f64::from(tilo_ui_base::tokens::Spacing::BASE.s4),
            "{pull:?}"
        );
        dispatch_at(
            &mut row,
            &taffy,
            root,
            &WindowEvent::MouseInput {
                device_id: winit::event::DeviceId::dummy(),
                state: ElementState::Released,
                button: MouseButton::Left,
            },
        );
        assert_eq!(row.group_ids(), vec![100, 200, 300]);
        assert_eq!(row.take_pending_group_reorder(), None);
        assert!(row.group_pull(100).is_some(), "it springs back");
        assert!(row.needs_animation_frame());
    }

    phase a_held_window_lands_back_at_once_under_reduced_motion;
    {
        let (mut row, taffy, root) = make_row_with_groups_with_taffy(&[&[1], &[2], &[3]]);
        row.reduce_motion = true;
        row.groups[0].set_tab_holder(
            1,
            Some(crate::widget::TabHolder {
                name: "Ben".into(),
                rgb: (200, 80, 40),
                keeps: true,
            }),
        );
        let grab = group_grab_point(&row, 0);
        press_at(&mut row, &taffy, root, grab);
        let past = Point::new(row.groups[2].get_rect().center().x + 60.0, grab.y);
        dispatch_at(
            &mut row,
            &taffy,
            root,
            &WindowEvent::CursorMoved {
                device_id: winit::event::DeviceId::dummy(),
                position: winit::dpi::PhysicalPosition::new(past.x, past.y),
            },
        );
        dispatch_at(
            &mut row,
            &taffy,
            root,
            &WindowEvent::MouseInput {
                device_id: winit::event::DeviceId::dummy(),
                state: ElementState::Released,
                button: MouseButton::Left,
            },
        );
        assert_eq!(row.group_pull(100), None, "no spring-back");
        assert!(!row.needs_animation_frame());
    }

    phase one_finger_horizontal_swipe_on_the_tab_bar_pans_the_strip;
    {
        let (mut row, taffy, root) = make_overflow_row(8, 1200.0);
        assert!(
            row.pan_max_scroll_x() > 80.0,
            "the fixture must fit to pan"
        );
        let start = group_grab_point(&row, 0);
        let before = row.scroll_x();
        dispatch_at(
            &mut row,
            &taffy,
            root,
            &touch_event(1, winit::event::TouchPhase::Started, start),
        );
        let mid = Point::new(start.x - 80.0, start.y);
        dispatch_at(
            &mut row,
            &taffy,
            root,
            &touch_event(1, winit::event::TouchPhase::Moved, mid),
        );
        assert!(
            row.scroll_x() - before > 20.0,
            "a leftward swipe must pan toward later groups; before {before} after {}",
            row.scroll_x()
        );
        assert!(
            !row.group_drag_active(),
            "a horizontal swipe is a pan, not a reorder"
        );
        dispatch_at(
            &mut row,
            &taffy,
            root,
            &touch_event(1, winit::event::TouchPhase::Ended, mid),
        );
    }

    phase touch_hold_then_drag_on_the_tab_bar_moves_the_window;
    {
        let (mut row, taffy, root) = make_row_with_groups_with_taffy(&[&[1], &[2], &[3]]);
        let start = group_grab_point(&row, 0);
        let before = row.scroll_x();
        dispatch_at(
            &mut row,
            &taffy,
            root,
            &touch_event(1, winit::event::TouchPhase::Started, start),
        );
        let drag = row
            .group_drag
            .as_mut()
            .expect("a finger on the bar lifts the group");
        drag.lifted_at = drag
            .lifted_at
            .checked_sub(TOUCH_GRAB_HOLD)
            .expect("monotonic clock fits");
        let to = Point::new(row.groups[2].get_rect().center().x + 60.0, start.y);
        for step in 1..=8 {
            let t = f64::from(step) / 8.0;
            let p = Point::new(start.x + (to.x - start.x) * t, start.y);
            dispatch_at(
                &mut row,
                &taffy,
                root,
                &touch_event(1, winit::event::TouchPhase::Moved, p),
            );
        }
        assert!(
            row.group_drag_active(),
            "after a hold, a horizontal drag carries the window"
        );
        assert!(
            (row.scroll_x() - before).abs() < f64::EPSILON,
            "a carried window does not pan the strip"
        );
        dispatch_at(
            &mut row,
            &taffy,
            root,
            &touch_event(1, winit::event::TouchPhase::Ended, to),
        );
        assert_eq!(
            row.group_ids(),
            vec![200, 300, 100],
            "the carried window lands where the finger lifted"
        );
        assert_eq!(row.take_pending_group_reorder(), Some((100, 2)));
    }

    phase one_finger_swipe_onto_another_display_rehomes_the_group;
    {
        let (mut row, taffy, root) =
            relayout_row_owned(super::section_tests::partitioned_row(), 1800.0);
        // Leftmost group: its grab sits on display 0. Group 200 can hang
        // across the border, so a grab there never "crosses".
        let start = group_grab_point(&row, 0);
        assert_eq!(row.output_of_group(100), Some(0));
        assert_eq!(
            row.output_at(start),
            Some(0),
            "grab {start:?} must start on display 0"
        );
        assert!(
            row.groups[0].bar_grab_hit(start),
            "{start:?} must lift group 100"
        );
        dispatch_at(
            &mut row,
            &taffy,
            root,
            &touch_event(1, winit::event::TouchPhase::Started, start),
        );
        let to = Point::new(1200.0, start.y);
        assert_eq!(
            row.output_at(to),
            Some(1),
            "{to:?} is on the second display"
        );
        for step in 1..=8 {
            let t = f64::from(step) / 8.0;
            let p = Point::new(start.x + (to.x - start.x) * t, start.y);
            dispatch_at(
                &mut row,
                &taffy,
                root,
                &touch_event(1, winit::event::TouchPhase::Moved, p),
            );
        }
        assert!(
            row.group_drag_active(),
            "crossing a display must keep the group drag, not convert it to a pan"
        );
        dispatch_at(
            &mut row,
            &taffy,
            root,
            &touch_event(1, winit::event::TouchPhase::Ended, to),
        );
        assert_eq!(
            row.output_of_group(100),
            Some(1),
            "the group belongs to the display it was dropped on"
        );
    }

    phase a_drag_resting_at_an_edge_pans_the_strip_under_it;
    {
        let (mut row, _taffy, _root) =
            make_row_with_groups_with_taffy(&[&[1], &[2], &[3], &[4], &[5]]);
        row.set_rect_for_test(Rect::new(0.0, 0.0, 700.0, 600.0));
        row.set_content_w_for_test(row.content_width());
        let max = row.max_scroll_x();
        assert!(max > 700.0, "fixture: most of the strip is off screen");
        let t0 = crate::time::now();
        let frame = Duration::from_millis(16);

        row.set_external_content_drag_hover(Some(Point::new(350.0, 300.0)));
        assert!(!row.drag_reach_pending());
        assert!(!row.advance_drag_reach_for_test(t0));

        row.set_external_content_drag_hover(Some(Point::new(698.0, 300.0)));
        assert!(row.drag_reach_pending(), "the edge pan asks for frames");
        let mut now = t0;
        row.advance_drag_reach_for_test(now);
        for _ in 0..10 {
            now += frame;
            assert!(row.advance_drag_reach_for_test(now));
        }
        let fast = row.scroll_x();
        assert!(fast > 100.0, "about 1.5 px/ms near the edge: {fast}");

        let (mut slow_row, _t, _r) =
            make_row_with_groups_with_taffy(&[&[1], &[2], &[3], &[4], &[5]]);
        slow_row.set_rect_for_test(Rect::new(0.0, 0.0, 700.0, 600.0));
        slow_row.set_content_w_for_test(slow_row.content_width());
        slow_row.set_external_content_drag_hover(Some(Point::new(700.0 - 24.0, 300.0)));
        let mut slow_now = t0;
        slow_row.advance_drag_reach_for_test(slow_now);
        for _ in 0..10 {
            slow_now += frame;
            slow_row.advance_drag_reach_for_test(slow_now);
        }
        assert!(slow_row.scroll_x() > 0.0 && slow_row.scroll_x() < fast / 2.0);

        for _ in 0..200 {
            now += frame;
            row.advance_drag_reach_for_test(now);
        }
        assert!((row.scroll_x() - max).abs() < 1e-6);
        assert!(
            !row.drag_reach_pending(),
            "nowhere further to go: no frames"
        );
        row.set_external_content_drag_hover(Some(Point::new(2.0, 300.0)));
        now += frame;
        row.advance_drag_reach_for_test(now);
        now += frame;
        assert!(row.advance_drag_reach_for_test(now));
        assert!(row.scroll_x() < max);

        let at = row.scroll_x();
        row.set_external_content_drag_hover(None);
        now += frame;
        assert!(!row.advance_drag_reach_for_test(now));
        assert!((row.scroll_x() - at).abs() < f64::EPSILON);
        assert!(!row.drag_reach_pending());
    }
}

#[test]
fn a_dragged_window_tracks_the_pointer_one_to_one() {
    let theme = Theme::dark();
    let mut now = crate::time::now();
    crate::time::testing::with_monotonic_time(now, || {
        let (mut row, taffy, root) = make_row_with_groups_with_taffy(&[&[1], &[2], &[3]]);
        let grab = group_grab_point(&row, 0);
        press_at(&mut row, &taffy, root, grab);
        let mut at = grab;
        for step in 1..=24 {
            now += Duration::from_millis(16);
            at = Point::new(
                grab.x + 15.0 * f64::from(step),
                grab.y + 6.0 * f64::from(step),
            );
            crate::time::testing::with_monotonic_time(now, || {
                dispatch_at(&mut row, &taffy, root, &move_to(at));
                let lift = row.group_drag_lift(&theme).expect("the window is carried");
                let drawn = lift * grab;
                assert!(
                    (drawn - at).hypot() < 1e-9,
                    "frame {step}: grabbed point drawn at {drawn:?}, pointer at {at:?}"
                );
            });
        }
        assert!(at.x - grab.x >= 350.0 && at.y - grab.y >= 140.0);
    });
}

tilo_test_env::scenario! {
    fn the_strip_pans_reveals_and_snaps;

    phase the_reveal_target_of_a_group_still_growing_is_where_it_will_end;
    {
        // A new window's slot grows from zero width. The pan toward it
        // must aim at the finished layout, not at the first frames' rect.
        let layout = |animate: bool| {
            let (mut row, _taffy, _root) =
                make_row_with_groups_with_taffy(&[&[1], &[2], &[3], &[4], &[5]]);
            row.set_rect_for_test(Rect::new(0.0, 0.0, 700.0, 600.0));
            if animate {
                row.groups
                    .last_mut()
                    .expect("groups")
                    .start_spawn_animation();
                let (relaid, _t, _r) = relayout_row_owned(row, 700.0);
                row = relaid;
                row.set_rect_for_test(Rect::new(0.0, 0.0, 700.0, 600.0));
            }
            row
        };
        let settled = layout(false);
        let growing = layout(true);
        let last = growing.groups.last().expect("groups");
        assert!(
            last.dest_width_px() > last.layout_width_px() + 1.0,
            "the fixture must still be growing: {} of {}",
            last.layout_width_px(),
            last.dest_width_px()
        );
        let id = last.id;
        assert_eq!(
            growing.scroll_target_for(id),
            settled.scroll_target_for(id),
            "the pan aims at the finished layout"
        );
    }

    phase a_strip_past_its_viewport_never_widens_its_container;
    {
        let viewport_w = 2000.0;
        let sections = [
            StripSection {
                output: 0,
                area: Rect::new(0.0, 0.0, 1200.0, 600.0),
                usable: 1200.0,
                leading: false,
            },
            StripSection {
                output: 1,
                area: Rect::new(1200.0, 0.0, viewport_w, 600.0),
                usable: 800.0,
                leading: false,
            },
        ];
        for (what, sections) in [("one run", &[][..]), ("a section per display", &sections[..])] {
            let (rect, max_scroll) = column_around_strip(8, viewport_w, sections);
            assert!(
                max_scroll > 0.0,
                "{what}: eight groups overflow, so the strip pans ({max_scroll})"
            );
            assert!(
                (rect.width() - viewport_w).abs() < 0.5,
                "{what}: the column is {rect:?}, not the {viewport_w} it was given"
            );
        }
    }

    phase direct_pan_keeps_trailing_area_and_elastic_overpull;
    {
        fn wheel_over_strip_never_moves_it() {
            use winit::dpi::PhysicalPosition;

            for (mut row, taffy, root) in [
                make_overflow_row(8, 1200.0),
                make_row_with_groups_with_taffy(&[&[1], &[10]]),
            ] {
                let pre_rect_0 = row.groups[0].get_rect();
                let cy = f64::midpoint(row.rect.y0, row.rect.y1);
                let cx = f64::midpoint(row.rect.x0, row.rect.x1);
                let dev = winit::event::DeviceId::dummy();
                let mut handled = dispatch_at(
                    &mut row,
                    &taffy,
                    root,
                    &WindowEvent::CursorMoved {
                        device_id: dev,
                        position: PhysicalPosition::new(cx, cy),
                    },
                );
                handled |= dispatch_at(
                    &mut row,
                    &taffy,
                    root,
                    &WindowEvent::MouseWheel {
                        device_id: dev,
                        delta: MouseScrollDelta::LineDelta(0.0, -3.0),
                        phase: winit::event::TouchPhase::Moved,
                    },
                );
                assert!(!handled, "a bare wheel must be left unclaimed by the strip");
                row.tick_snap_at(tilo_ui_base::motion::frame_now() + Duration::from_millis(400));
                assert_eq!(row.scroll_x(), 0.0, "a bare wheel must not move the strip");
                row.apply_layout(&taffy, root, Point::ORIGIN);
                assert_eq!(
                    row.groups[0].get_rect().x0,
                    pre_rect_0.x0,
                    "group rects must not shift after a bare wheel"
                );
            }
        }

        fn direct_pan_keeps_a_fifth_viewport_after_the_last_app() {
            let mut row = make_row_with_groups(&[&[1], &[2], &[3]]);
            row.set_rect_for_test(Rect::new(0.0, 0.0, 1000.0, 600.0));
            row.set_content_w_for_test(2200.0);

            assert_eq!(
                row.max_scroll_x(),
                1200.0,
                "focus-follow stays content-tight"
            );
            assert_eq!(
                row.pan_max_scroll_x(),
                1400.0,
                "direct pan reveals 20% of the viewport beyond the last app"
            );

            row.set_content_w_for_test(700.0);
            assert_eq!(
                row.pan_max_scroll_x(),
                0.0,
                "a strip which already fits must not drift into empty canvas"
            );
        }

        fn direct_pan_tracks_reversals_and_only_settles_elastic_overpull() {
            let mut row = make_row_with_groups(&[&[1], &[2], &[3]]);
            row.set_rect_for_test(Rect::new(0.0, 0.0, 1000.0, 600.0));
            row.set_content_w_for_test(2200.0);

            let t0 = crate::time::now();
            let at = |ms: u64| t0 + Duration::from_millis(ms);
            assert!(row.update_strip_pan(450.0, at(0)));
            assert!((row.scroll_x() - 450.0).abs() < 0.01);
            assert!(row.update_strip_pan(320.0, at(500)));
            assert!(
                (row.scroll_x() - 320.0).abs() < 0.01,
                "total gesture travel must reverse without rebasing its origin"
            );
            assert!(
                !row.end_strip_pan(at(600)),
                "an in-range release at rest must not move away from the fingers"
            );
            assert!((row.scroll_x() - 320.0).abs() < 0.01);

            // A new physical stream explicitly rebases, so a missing End at a
            // lock/session boundary cannot carry old total travel into it.
            row.begin_strip_pan();
            assert!(row.update_strip_pan(10.0, at(700)));
            assert!((row.scroll_x() - 330.0).abs() < 0.01);
            let _ = row.end_strip_pan(at(800));
        }

        fn direct_pan_overpull_has_diminishing_gain_and_returns_to_resting_end() {
            let mut row = make_row_with_groups(&[&[1], &[2], &[3]]);
            row.set_rect_for_test(Rect::new(0.0, 0.0, 1000.0, 600.0));
            row.set_content_w_for_test(2200.0);
            let end = row.pan_max_scroll_x();

            let t0 = crate::time::now();
            assert!(row.update_strip_pan(end, t0));
            let mut previous = row.scroll_x();
            let mut previous_gain = f64::INFINITY;
            let mut at = t0;
            for extra in [100.0, 200.0, 300.0, 400.0] {
                at += Duration::from_millis(16);
                assert!(row.update_strip_pan(end + extra, at));
                let gain = row.scroll_x() - previous;
                assert!(
                    gain > 0.0 && gain < previous_gain,
                    "each equal pull must still move, but less than the one before: {gain} >= {previous_gain}"
                );
                assert!(
                    row.scroll_x() < end + 280.0,
                    "the 28%-viewport asymptote is never crossed"
                );
                previous = row.scroll_x();
                previous_gain = gain;
            }

            let now = at + Duration::from_millis(8);
            assert!(row.end_strip_pan(now));
            row.tick_snap_at(now + Duration::from_millis(100));
            assert!(
                row.scroll_x() > end && row.scroll_x() < end + 280.0,
                "the settle stays inside the rubber band on its way back: {}",
                row.scroll_x()
            );
            row.tick_snap_at(now + Duration::from_secs(1));
            assert!(
                (row.scroll_x() - end).abs() < 0.01,
                "release must return exactly to the legal trailing rest"
            );
        }

        wheel_over_strip_never_moves_it();
        direct_pan_keeps_a_fifth_viewport_after_the_last_app();
        direct_pan_tracks_reversals_and_only_settles_elastic_overpull();
        direct_pan_overpull_has_diminishing_gain_and_returns_to_resting_end();
    }

    phase reveals_keep_windows_clear_of_the_screen_edges;
    // A window's edge is drawn just outside its rect, so a reveal that
    // stopped flush with the screen edge cut the window's border and
    // rounded corners off. Every reveal leaves at least the gap between
    // two windows, and the strip's first and last windows their gutter.
    {
        let gap = HANDLE_SLOT_PX;
        // Viewport 0..1000 with a 96 px gutter, so the first group lies at
        // canvas 96..696, the second at 704..1304, the last ends at 2400
        // and the strip's end, the gutter after it, is scroll 1496.
        let pan = Pan {
            viewport: Rect::new(0.0, 0.0, 1000.0, 600.0),
            inset: 96.0,
            scroll_x: 400.0,
            content_w: 2400.0,
            ..Pan::default()
        };
        let leftmost = Rect::new(-304.0, 0.0, 296.0, 600.0);
        let target = pan
            .scroll_target_for(leftmost)
            .expect("a cut group is revealed");
        assert!(
            target.abs() < 0.01,
            "home (0) with the gutter before it, not flush with the screen \
         edge (96) with its border cut off: {target}"
        );
        assert_eq!(
            pan.scroll_target_for(Rect::new(10.0, 0.0, 610.0, 600.0)),
            None
        );
        assert_eq!(
            pan.scroll_target_for(Rect::new(3.0, 0.0, 603.0, 600.0)),
            Some(400.0 - (gap - 3.0))
        );
        let target = pan
            .scroll_target_for(Rect::new(
                704.0 - 400.0 + 500.0,
                0.0,
                1404.0 - 400.0 + 500.0,
                600.0,
            ))
            .expect("a group cut on the right is revealed");
        assert!((target - (904.0 + gap)).abs() < 0.01, "{target}");
        let target = pan
            .scroll_target_for(Rect::new(-200.0, 0.0, 400.0, 600.0))
            .expect("a group cut on the left is revealed");
        assert!((target - (200.0 - gap)).abs() < 0.01, "{target}");
        let target = pan
            .scroll_target_for(Rect::new(-100.0, 0.0, 900.0, 600.0))
            .expect("a full-width group is revealed");
        assert!((target - 300.0).abs() < 0.01, "{target}");
        let target = pan
            .scroll_target_for(Rect::new(1800.0 - 400.0, 0.0, 2400.0 - 400.0, 600.0))
            .expect("the last group is revealed");
        assert!(
            (target - 1496.0).abs() < 0.01,
            "the strip's end with its gutter: {target}"
        );
    }

    phase revealing_the_last_window_without_a_gutter_stops_at_the_strip_end;
    {
        let pan = Pan {
            viewport: Rect::new(0.0, 0.0, 1000.0, 600.0),
            scroll_x: 400.0,
            content_w: 2400.0,
            ..Pan::default()
        };
        let target = pan
            .scroll_target_for(Rect::new(1800.0 - 400.0, 0.0, 2400.0 - 400.0, 600.0))
            .expect("the last group is revealed");
        assert!(
            (target - pan.max_scroll_x()).abs() < 0.01,
            "the strip's end ({}), not past it: {target}",
            pan.max_scroll_x()
        );
    }

    phase wheel_idle_snaps_to_nearest_group_once;
    {
        fn scroll_snaps_to_nearest_group_after_wheel_idle() {
            let mut row = make_row_with_groups(&[&[1], &[2], &[3]]);
            row.set_group_left_edges_for_test(vec![0.0, 608.0, 1216.0]);
            row.set_content_w_for_test(1816.0);
            row.set_rect_for_test(Rect::new(0.0, 0.0, 1000.0, 600.0));
            row.set_scroll_x_for_test(400.0);
            let base = crate::time::now();
            row.set_last_wheel_at_for_test(earlier(base, 250));

            // First tick after idle: snap kicks off and advances ~25% of the
            // way (50 ms into a 200 ms cubic-out curve). scroll_x must have
            // moved toward 608 but not yet arrived.
            row.tick_snap_at(base + Duration::from_millis(50));
            assert!(
                row.scroll_x() > 400.0,
                "snap must move scroll_x toward the target; got {}",
                row.scroll_x()
            );
            assert!(
                row.scroll_x() < 608.0,
                "snap must NOT have arrived after only 50 ms; got {}",
                row.scroll_x()
            );

            row.tick_snap_at(base + Duration::from_millis(250));
            assert!(
                (row.scroll_x() - 608.0).abs() < 1.0,
                "snap must settle at 608.0 after 200 ms; got {}",
                row.scroll_x()
            );
        }

        fn snap_does_not_fire_during_wheel_idle_window() {
            let mut row = make_row_with_groups(&[&[1], &[2], &[3]]);
            row.set_group_left_edges_for_test(vec![0.0, 608.0, 1216.0]);
            row.set_content_w_for_test(1816.0);
            row.set_rect_for_test(Rect::new(0.0, 0.0, 1000.0, 600.0));
            row.set_scroll_x_for_test(400.0);
            let base = crate::time::now();
            // Only 100 ms since the last wheel — inside the idle window.
            row.set_last_wheel_at_for_test(earlier(base, 100));

            row.tick_snap_at(base);
            assert_eq!(
                row.scroll_x(),
                400.0,
                "snap must not fire while still inside the idle window",
            );
        }

        fn wheel_mid_snap_leaves_the_snap_untouched() {
            use winit::dpi::PhysicalPosition;

            let (mut row, taffy, root) = make_overflow_row(8, 1200.0);
            row.set_scroll_x_for_test(300.0);
            let base = crate::time::now();
            row.set_last_wheel_at_for_test(earlier(base, 250));
            row.tick_snap_at(base);
            assert!(
                row.pan.motion.is_some(),
                "precondition: snap must be in flight",
            );

            // Cursor must be inside the row's rect for the handler to claim it.
            let cy = f64::midpoint(row.rect.y0, row.rect.y1);
            let cx = f64::midpoint(row.rect.x0, row.rect.x1);
            let dev = winit::event::DeviceId::dummy();
            dispatch_at(
                &mut row,
                &taffy,
                root,
                &WindowEvent::CursorMoved {
                    device_id: dev,
                    position: PhysicalPosition::new(cx, cy),
                },
            );
            dispatch_at(
                &mut row,
                &taffy,
                root,
                &WindowEvent::MouseWheel {
                    device_id: dev,
                    delta: MouseScrollDelta::LineDelta(0.0, -1.0),
                    phase: winit::event::TouchPhase::Moved,
                },
            );
            let anim = row
                .pan
                .motion
                .expect("the in-flight snap must survive a bare wheel notch");
            // Was easing toward the stop nearest 300; must not be re-targeted.
            let edges = row.group_left_edges_for_test().to_vec();
            let expected = edges
                .iter()
                .copied()
                .min_by(|a, b| (a - 300.0).abs().partial_cmp(&(b - 300.0).abs()).unwrap())
                .unwrap();
            assert!(
                (anim.target() - expected).abs() < 0.5,
                "the snap must still target the stop nearest its origin \
         ({expected}); got {}",
                anim.target()
            );
            row.tick_snap_at(tilo_ui_base::motion::frame_now() + Duration::from_millis(400));
            assert!(
                (row.scroll_x() - expected).abs() < 0.5,
                "the snap must settle on its original target ({expected}); \
         got {}",
                row.scroll_x()
            );
        }

        fn reduced_motion_snaps_land_on_their_first_tick() {
            let mut row = make_row_with_groups(&[&[1], &[2], &[3]]);
            row.reduce_motion = true;
            row.set_group_left_edges_for_test(vec![0.0, 608.0, 1216.0]);
            row.set_content_w_for_test(1816.0);
            row.set_rect_for_test(Rect::new(0.0, 0.0, 1000.0, 600.0));
            row.set_scroll_x_for_test(400.0);
            let base = crate::time::now();
            row.set_last_wheel_at_for_test(earlier(base, 250));
            row.tick_snap_at(base);
            assert!(
                (row.scroll_x() - 608.0).abs() < 0.01,
                "the idle snap must land at once; got {}",
                row.scroll_x()
            );
            assert!(!row.snap_in_flight(), "and nothing is left in flight");

            row.begin_strip_pan();
            assert!(row.update_strip_pan(-800.0, base));
            assert!(row.scroll_x() < 0.0, "fixture: the pan overpulls");
            assert!(row.end_strip_pan(base));
            row.tick_snap_at(base);
            assert!(
                row.scroll_x().abs() < 0.01,
                "a released overpull must settle at once; got {}",
                row.scroll_x()
            );
        }

        scroll_snaps_to_nearest_group_after_wheel_idle();
        snap_does_not_fire_during_wheel_idle_window();
        wheel_mid_snap_leaves_the_snap_untouched();
        reduced_motion_snaps_land_on_their_first_tick();
    }
}

/// Each `WindowWidth::Normal` group is 600px, so `count`×600 +
/// (`count`-1)×8 (gap) is the natural content width; pass a
/// `viewport_w` strictly less to force horizontal overflow.
fn make_overflow_row(count: usize, viewport_w: f64) -> (GroupRow, TaffyTree<()>, NodeId) {
    use std::collections::HashMap;
    use taffy::{AvailableSpace, Size as TSize, TaffyTree};

    let groups: Vec<WindowGroup> = (0..count)
        .map(|i| {
            make_group(
                (i as u64).saturating_mul(100).saturating_add(100),
                &[(i as u64).saturating_add(1)],
            )
        })
        .collect();
    let mut row = GroupRow::new(groups);
    let theme = Theme::dark();
    let mut taffy: TaffyTree<()> = TaffyTree::new();
    let mut node_map: HashMap<taffy::NodeId, *mut dyn Widget> = HashMap::new();
    let root = row.build_layout(&mut taffy, &theme, &mut node_map);
    let viewport = taffy.parent_node(
        Style {
            size: Size {
                width: Dimension::length(tilo_ui_base::num::f32_of_f64(viewport_w)),
                height: Dimension::length(600.0),
            },
            ..Default::default()
        },
        &[root],
    );
    taffy
        .compute_layout_with_measure(
            viewport,
            TSize {
                width: AvailableSpace::Definite(tilo_ui_base::num::f32_of_f64(viewport_w)),
                height: AvailableSpace::Definite(600.0),
            },
            |_, _, _, _, _| taffy::Size::ZERO,
        )
        .expect("taffy compute_layout");
    row.apply_layout(&taffy, root, Point::ORIGIN);
    (row, taffy, root)
}

fn column_around_strip(count: usize, viewport_w: f64, sections: &[StripSection]) -> (Rect, f64) {
    use crate::widget::Column;
    use std::collections::HashMap;
    use taffy::{AvailableSpace, Size as TSize};

    let groups: Vec<WindowGroup> = (0..count)
        .map(|i| {
            let id = (i as u64).saturating_mul(100).saturating_add(100);
            make_group(id, &[(i as u64).saturating_add(1)])
        })
        .collect();
    let mut row = GroupRow::new(groups);
    let assignment: HashMap<u64, u32> = row
        .groups
        .iter()
        .map(|g| (g.id, sections.first().map_or(0, |section| section.output)))
        .collect();
    row.set_sections(sections, &assignment);
    let mut column = Column::new().with_no_padding().push(Box::new(row));
    let theme = Theme::dark();
    let mut rect = Rect::ZERO;
    for _ in 0..2 {
        let mut taffy: TaffyTree<()> = TaffyTree::new();
        let mut node_map: HashMap<taffy::NodeId, *mut dyn Widget> = HashMap::new();
        let node = column.build_layout(&mut taffy, &theme, &mut node_map);
        let viewport = taffy.parent_node(
            Style {
                size: Size {
                    width: Dimension::length(tilo_ui_base::num::f32_of_f64(viewport_w)),
                    height: Dimension::length(600.0),
                },
                ..Default::default()
            },
            &[node],
        );
        taffy
            .compute_layout_with_measure(
                viewport,
                TSize {
                    width: AvailableSpace::Definite(tilo_ui_base::num::f32_of_f64(viewport_w)),
                    height: AvailableSpace::Definite(600.0),
                },
                |_, _, _, _, _| taffy::Size::ZERO,
            )
            .expect("taffy compute_layout");
        column.apply_layout(&taffy, node, Point::ORIGIN);
        rect = column.get_rect();
    }
    let max_scroll = column
        .children_mut()
        .into_iter()
        .next()
        .and_then(|row| row.as_any_mut())
        .and_then(|row| row.downcast_mut::<GroupRow>())
        .map_or(0.0, |row| row.max_scroll_x());
    (rect, max_scroll)
}

#[test]
fn strip_layout_never_overlaps_or_cuts_windows() {
    fn content_width_exceeds_viewport_when_groups_overflow() {
        let (row, _t, _r) = make_overflow_row(8, 1200.0);
        let expected_min = 8.0 * 300.0 + 7.0 * 8.0 - 1.0; // 2455, 1 px fp slop.
        assert!(
            row.content_width() >= expected_min,
            "content_w must reflect the floored group widths; got {} expected ≥{}",
            row.content_width(),
            expected_min
        );
        assert!(
            row.max_scroll_x() > 0.0,
            "viewport 1200 < content {} → max_scroll must be positive",
            row.content_width()
        );
    }

    /// `group_canvas_spans` is what the shell reads to place a new
    /// launch, so overlapping spans would place a window on top of one
    /// already there. Every pair is compared, not just neighbours: two
    /// groups sharing an origin only shows up under a full pairwise sweep.
    fn no_two_groups_on_a_strip_share_horizontal_space() {
        for (count, viewport) in [(2usize, 1280.0_f64), (3, 1280.0), (4, 900.0), (8, 1200.0)] {
            let (row, _t, _r) = make_overflow_row(count, viewport);
            let spans = row.group_canvas_spans();
            assert_eq!(
                spans.len(),
                count,
                "every group on a {count}-group strip gets a span",
            );

            let mut ids: Vec<u64> = spans.iter().map(|(id, _, _)| *id).collect();
            ids.compact_sort();
            let distinct = {
                let mut d = ids.clone();
                d.dedup();
                d.len()
            };
            assert_eq!(
                distinct, count,
                "a strip holds {count} distinct groups, not the same one twice \
             (ids: {ids:?})",
            );

            for (i, (id, l, r)) in spans.iter().enumerate() {
                assert!(
                    r > l,
                    "group {id} (index {i}) occupies a positive width, got \
                 {l:.1}..{r:.1}",
                );
            }
            for (i, (a_id, a_l, a_r)) in spans.iter().enumerate() {
                for (b_id, b_l, b_r) in spans.iter().skip(i + 1) {
                    assert!(
                        a_r <= b_l || b_r <= a_l,
                        "groups {a_id} ({a_l:.0}..{a_r:.0}) and {b_id} \
                     ({b_l:.0}..{b_r:.0}) overlap on a {count}-group strip \
                     at viewport {viewport} — that is two windows drawn in \
                     one place",
                    );
                }
            }
        }
    }

    /// Windows changing width on their own (a new share of the strip,
    /// a peer's resize) never pan the strip: it scrolls only for
    /// explicit navigation and to bring a newly opened or focused
    /// window into view. Sequence mirrors the compositor's own: open +
    /// bring into view, then give groups a new share of the strip.
    fn widening_a_strip_leaves_its_pan_where_it_was() {
        let mut row = strip_at(2, 1920.0, 291.0, 667);
        let parked = row.max_scroll_x();
        row.set_scroll_x_for_test(parked);
        assert!(parked > 0.0, "two groups at 667 permille overflow 1920");

        set_strip_permille(&mut row, 720);
        relayout_strip(&mut row, 1920.0);

        assert!(
            (row.scroll_x() - parked).abs() < 1e-6,
            "the strip moved from {parked:.0} to {:.0} by itself — spans {:?}",
            row.scroll_x(),
            row.pan.laid_spans,
        );
    }

    fn narrowing_until_everything_fits_does_not_frame_the_strip() {
        let mut row = strip_at(2, 1920.0, 291.0, 720);
        row.set_scroll_x_for_test(300.0);
        relayout_strip(&mut row, 1920.0);

        set_strip_permille(&mut row, 580);
        relayout_strip(&mut row, 1920.0);
        row.frame_presented();
        let settled = crate::time::now() + Duration::from_secs(2);
        crate::time::testing::with_monotonic_time(settled, || {
            relayout_strip(&mut row, 1920.0);
        });

        let span = row
            .pan
            .laid_spans
            .last()
            .zip(row.pan.laid_spans.first())
            .map(|((_, _, r), (_, l, _))| r - l)
            .unwrap_or_default();
        assert!(
            span <= 1920.0,
            "test setup: two groups at 580 permille must fit 1920, got {span:.0}",
        );
        assert!(
            (row.scroll_x() - 300.0).abs() < 1e-6,
            "the strip moved to {:.0} by itself to frame the windows — spans {:?}",
            row.scroll_x(),
            row.pan.laid_spans,
        );
        assert_eq!(
            row.groups_cut_by_row_edges(),
            vec![row.groups[0].id],
            "the first window stays cut where the person left the strip",
        );
    }

    content_width_exceeds_viewport_when_groups_overflow();
    no_two_groups_on_a_strip_share_horizontal_space();
    widening_a_strip_leaves_its_pan_where_it_was();
    narrowing_until_everything_fits_does_not_frame_the_strip();
}

/// Lay `row` out inside a `viewport_w`-wide viewport through the
/// real `build_layout` / `apply_layout` pair, rebuilding the taffy
/// tree so a changed width actually takes — the baked length on the
/// old node is what a stale tree would keep serving.
pub(super) fn relayout_strip(row: &mut GroupRow, viewport_w: f64) {
    use std::collections::HashMap;
    use taffy::{AvailableSpace, Size as TSize, TaffyTree};

    let theme = Theme::dark();
    let mut taffy: TaffyTree<()> = TaffyTree::new();
    let mut node_map: HashMap<taffy::NodeId, *mut dyn Widget> = HashMap::new();
    let root = row.build_layout(&mut taffy, &theme, &mut node_map);
    let viewport = taffy.parent_node(
        Style {
            size: Size {
                width: Dimension::length(tilo_ui_base::num::f32_of_f64(viewport_w)),
                height: Dimension::length(1080.0),
            },
            ..Default::default()
        },
        &[root],
    );
    taffy
        .compute_layout_with_measure(
            viewport,
            TSize {
                width: AvailableSpace::Definite(tilo_ui_base::num::f32_of_f64(viewport_w)),
                height: AvailableSpace::Definite(1080.0),
            },
            |_, _, _, _, _| taffy::Size::ZERO,
        )
        .expect("taffy compute_layout");
    row.apply_layout(&taffy, root, Point::ORIGIN);
}

/// A strip of `count` groups, each `permille` of the strip's usable width, behind the
/// shell's wheel-disc gutter — the shape the compositor actually lays out.
pub(super) fn strip_at(
    count: usize,
    viewport_w: f64,
    leading_inset: f64,
    permille: u32,
) -> GroupRow {
    let groups: Vec<WindowGroup> = (0..count)
        .map(|i| {
            make_group(
                (i as u64).saturating_mul(100).saturating_add(100),
                &[(i as u64).saturating_add(1)],
            )
        })
        .collect();
    let mut row = GroupRow::new(groups);
    row.set_leading_inset(leading_inset);
    set_strip_permille(&mut row, permille);
    relayout_strip(&mut row, viewport_w);
    relayout_strip(&mut row, viewport_w);
    row
}

fn set_strip_permille(row: &mut GroupRow, permille: u32) {
    for g in &mut row.groups {
        g.set_width(WindowWidth::Fraction(permille));
    }
}

tilo_test_env::scenario! {
    fn a_window_edge_resizes_at_its_border;

    phase a_lone_windows_edge_resizes_it_rather_than_moving_it;
    {
        let (mut row, taffy, root) = make_row_with_groups_with_taffy(&[&[1]]);
        let group_rect = row.groups[0].get_rect();
        let grab = Point::new(group_rect.x1 - 2.0, group_rect.center().y);
        let _ = edge_event(&mut row, &taffy, root, false, &move_to(grab));
        assert!(edge_event(
            &mut row,
            &taffy,
            root,
            false,
            &button(ElementState::Pressed)
        ));
        assert!(
            row.wants_pointer_capture(),
            "the lone window's resize must own pointer capture",
        );
        assert!(edge_event(
            &mut row,
            &taffy,
            root,
            false,
            &move_to(Point::new(grab.x - 80.0, grab.y))
        ));
        assert_eq!(
            row.groups[0].layout_width_px(),
            group_rect.width() - 80.0,
            "dragging the lone trailing edge resizes rather than reorders the window",
        );
        assert!(!row.group_drag_active());
    }

    phase a_window_edge_shows_the_resize_cursor_through_the_shared_rule;
    {
        use tilo_ui_base::widget::cursor::in_tree;
        use winit::window::CursorIcon;
        let (mut row, taffy, root) = edge_strip(None, None);
        let r = rects(&row);
        let y = r[0].center().y;
        let border = Point::new(r[0].x1 - 2.0, y);
        assert_eq!(in_tree(&row, border), Some(CursorIcon::EwResize));
        let outer = Point::new(r[2].x1 + crate::widget::RESIZE_HANDLE_PX - 1.0, y);
        assert_eq!(
            in_tree(&row, outer),
            Some(CursorIcon::EwResize),
            "an outer edge reaches out over the empty canvas"
        );
        let body = Point::new(r[1].center().x, y);
        assert_ne!(in_tree(&row, body), Some(CursorIcon::EwResize));
        let _ = edge_event(&mut row, &taffy, root, false, &move_to(border));
        assert!(edge_event(
            &mut row,
            &taffy,
            root,
            false,
            &button(ElementState::Pressed)
        ));
        let _ = edge_event(&mut row, &taffy, root, false, &move_to(body));
        assert_eq!(
            in_tree(&row, body),
            Some(CursorIcon::EwResize),
            "the drag holds the resize cursor off the edge it began on"
        );
    }

    phase the_border_belongs_to_the_focused_window_and_hovering_shows_whose_edge_it_is;
    {
        {
            let (mut row, taffy, root) = edge_strip(None, None);
            let r = rects(&row);
            let y = r[0].center().y;
            let _ = edge_event(
                &mut row,
                &taffy,
                root,
                false,
                &move_to(Point::new(r[0].x1 - 2.0, y)),
            );
            assert_eq!(
                row.edge_hover(),
                Some((
                    EdgeGrab {
                        group: 0,
                        edge: WindowEdge::Trailing
                    },
                    false
                ))
            );
            assert_eq!(row.groups[0].edge_highlight(), Some(WindowEdge::Trailing));
            assert_eq!(row.groups[1].edge_highlight(), None);
            assert_eq!(
                row.cursor_shape(Point::new(r[0].x1 - 2.0, y)),
                Some(winit::window::CursorIcon::EwResize)
            );
            let _ = edge_event(
                &mut row,
                &taffy,
                root,
                false,
                &move_to(Point::new(r[1].x0 + 2.0, y)),
            );
            assert_eq!(
                row.edge_hover(),
                Some((
                    EdgeGrab {
                        group: 1,
                        edge: WindowEdge::Leading
                    },
                    false
                ))
            );
            assert_eq!(row.groups[0].edge_highlight(), None, "the highlight moves");
            assert_eq!(row.groups[1].edge_highlight(), Some(WindowEdge::Leading));
        }
        // Phase `the_window_in_use_owns_the_whole_border`: the pointer in the
        // terminal's half of the border still names Files' left edge, because
        // Files is the window the person is in.
        {
            let (mut row, taffy, root) = edge_strip(Some(1), None);
            let r = rects(&row);
            let y = r[0].center().y;
            for x in [
                r[0].x1 - 5.0,
                r[0].x1 - 1.0,
                f64::midpoint(r[0].x1, r[1].x0),
                r[1].x0 + 2.0,
            ] {
                let _ = edge_event(&mut row, &taffy, root, false, &move_to(Point::new(x, y)));
                assert_eq!(
                    row.edge_hover(),
                    Some((
                        EdgeGrab {
                            group: 1,
                            edge: WindowEdge::Leading
                        },
                        false
                    )),
                    "x = {x}"
                );
                assert_eq!(row.groups[0].edge_highlight(), None, "x = {x}");
                assert_eq!(row.groups[1].edge_highlight(), Some(WindowEdge::Leading));
            }
            let _ = edge_event(
                &mut row,
                &taffy,
                root,
                true,
                &move_to(Point::new(r[0].x1 - 2.0, y)),
            );
            assert_eq!(row.groups[0].edge_highlight(), Some(WindowEdge::Trailing));
            assert_eq!(row.groups[1].edge_highlight(), Some(WindowEdge::Leading));
            assert_eq!(row.groups[2].edge_highlight(), None);
            let _ = edge_event(&mut row, &taffy, root, false, &move_to(r[1].center()));
            assert_eq!(row.edge_hover(), None);
            assert!(row.groups.iter().all(|g| g.edge_highlight().is_none()));
        }
    }

    phase a_plain_drag_resizes_only_the_grabbed_window_and_holds_its_opposite_edge;
    {
        {
            let (row, taffy, root) = edge_strip(Some(1), None);
            let before = rects(&row);
            let from = Point::new(before[0].x1 - 2.0, before[0].center().y);
            let (mut row, taffy, root, frames) =
                drag_edge(row, &taffy, root, from, &[-20.0, -40.0, 40.0], false);
            assert_eq!(frames[0], before, "no jump at the press");
            for (frame, dx) in frames[1..].iter().zip([-20.0, -40.0, 40.0]) {
                assert_eq!(
                    widths(frame),
                    vec![400.0, 400.0 - dx, 400.0],
                    "only the grabbed window changes width (dx {dx})"
                );
                assert!(
                    (frame[1].x1 - before[1].x1).abs() < 1e-6,
                    "its right edge holds still on screen (dx {dx}): {frame:?}"
                );
                assert!(
                    (frame[1].x0 - (before[1].x0 + dx)).abs() < 1e-6,
                    "its left edge stays under the pointer (dx {dx})"
                );
                assert!(
                    (frame[0].x1 - (before[0].x1 + dx)).abs() < 1e-6,
                    "the window before it slides with the edge at its own width"
                );
                assert_eq!(frame[2], before[2], "the window after it does not move");
            }
            assert!(edge_event(
                &mut row,
                &taffy,
                root,
                false,
                &button(ElementState::Released)
            ));
            assert!(!row.edge_resize_in_flight());
            assert_eq!(
                row.groups[1].take_pending_width_change(),
                Some(WindowWidth::Fraction(crate::widget::permille_for_px(
                    360.0,
                    FIXTURE_USABLE_PX
                )))
            );
            assert_eq!(row.groups[0].take_pending_width_change(), None);
            assert_eq!(row.groups[2].take_pending_width_change(), None);
        }
        {
            let (row, taffy, root) = edge_strip(Some(1), None);
            let before = rects(&row);
            let from = Point::new(before[2].x0 + 2.0, before[1].center().y);
            let (_row, _taffy, _root, frames) =
                drag_edge(row, &taffy, root, from, &[30.0, -50.0], false);
            for (frame, dx) in frames[1..].iter().zip([30.0, -50.0]) {
                assert_eq!(widths(frame), vec![400.0, 400.0 + dx, 400.0], "dx {dx}");
                assert!((frame[1].x0 - before[1].x0).abs() < 1e-6, "left edge holds");
                assert!((frame[2].x0 - (before[2].x0 + dx)).abs() < 1e-6, "pushed");
                assert_eq!(frame[0], before[0]);
            }
        }
        {
            let (row, taffy, root) = edge_strip(None, None);
            let before = rects(&row);
            let from = Point::new(before[0].x1 - 2.0, before[0].center().y);
            let (_row, _taffy, _root, frames) = drag_edge(row, &taffy, root, from, &[25.0], false);
            assert_eq!(widths(&frames[1]), vec![425.0, 400.0, 400.0]);
            assert_eq!(frames[1][0].x0, before[0].x0);
        }
    }

    phase shift_drag_trades_width_between_the_two_windows_at_the_border_only;
    {
        let (row, taffy, root) = edge_strip(Some(1), None);
        let before = rects(&row);
        let from = Point::new(before[1].x0 + 2.0, before[1].center().y);
        let (mut row, taffy, root, frames) = drag_edge(row, &taffy, root, from, &[-60.0], true);
        assert_eq!(widths(&frames[1]), vec![340.0, 460.0, 400.0]);
        assert_eq!(frames[1][0].x0, before[0].x0, "the outer edges stay put");
        assert!((frames[1][1].x1 - before[1].x1).abs() < 1e-6);
        assert_eq!(frames[1][2], before[2]);
        assert!(edge_event(
            &mut row,
            &taffy,
            root,
            false,
            &button(ElementState::Released)
        ));
        assert!(row.groups[0].take_pending_width_change().is_some());
        assert!(row.groups[1].take_pending_width_change().is_some());
        assert_eq!(row.groups[2].take_pending_width_change(), None);
    }

    phase escape_puts_the_windows_and_the_strip_back;
    {
        let (row, taffy, root) = edge_strip(Some(1), None);
        let before = rects(&row);
        let scroll = row.scroll_x();
        let from = Point::new(before[1].x0 + 2.0, before[1].center().y);
        let (mut row, taffy, root, _) = drag_edge(row, &taffy, root, from, &[-80.0], false);
        let escape = tilo_ui_base::synthetic_input::named_key(winit::keyboard::NamedKey::Escape);
        assert!(edge_event(&mut row, &taffy, root, false, &escape));
        assert!(!row.edge_resize_in_flight());
        assert_eq!(row.scroll_x(), scroll);
        let (mut row, _, _) = relayout_row_owned(row, 2000.0);
        assert_eq!(rects(&row), before);
        assert!(
            row.groups
                .iter_mut()
                .all(|g| g.take_pending_width_change().is_none())
        );
    }

    phase resting_at_the_display_edge_auto_scrolls_and_the_edge_stays_under_the_pointer;
    {
        let (row, taffy, root) = edge_strip(Some(2), None);
        let before = rects(&row);
        let viewport = row.get_rect();
        let from = Point::new(before[2].x1 - 2.0, before[2].center().y);
        // Into the edge band, 4 px from the display's right edge.
        let to = viewport.x1 - 4.0 - from.x;
        let (mut row, _taffy, _root, frames) = drag_edge(row, &taffy, root, from, &[to], false);
        let width = frames[1][2].width();
        let t0 = crate::time::now();
        assert!(
            !row.advance_edge_autoscroll(t0),
            "the first tick only starts the clock"
        );
        assert!(row.advance_edge_autoscroll(t0 + Duration::from_millis(50)));
        let (row, _, _) = relayout_row_owned(row, 2000.0);
        let after = rects(&row);
        assert!(
            after[2].width() > width + 20.0,
            "it grew past the screen: {after:?}"
        );
        assert!(
            (after[2].x1 - (viewport.x1 - 4.0 + 2.0)).abs() < 1.0,
            "its right edge is still under the pointer: {after:?}"
        );
        assert_eq!(
            widths(&after)[..2],
            widths(&before)[..2],
            "no one else changed"
        );
    }
}

struct MinWidthApp {
    min: f64,
    anchor: std::rc::Rc<std::cell::Cell<(Option<WindowEdge>, bool)>>,
    rect: Rect,
}

impl Widget for MinWidthApp {
    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        _theme: &Theme,
        node_map: &mut std::collections::HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        taffy.fill_leaf(node_map, self)
    }
    tilo_ui_base::rect_geometry!();
    fn name(&self) -> &'static str {
        "MinWidthApp"
    }
}

impl crate::widget::AppWidget for MinWidthApp {
    fn app_kind(&self) -> crate::widget::AppKind {
        crate::widget::AppKind::Files
    }
    fn app_title(&self) -> &'static str {
        "Files"
    }
    fn app_icon(&self) -> crate::widget::IconDescriptor {
        crate::widget::IconDescriptor::Glyph("F")
    }
    fn min_window_width(&self) -> Option<f64> {
        Some(self.min)
    }
    fn set_live_resize_anchor(&mut self, fixed: Option<WindowEdge>) {
        self.anchor.set((fixed, false));
    }
    fn release_live_resize_anchor(&mut self) {
        let (fixed, _) = self.anchor.get();
        self.anchor.set((fixed, true));
    }
}

/// A strip of three 400 px windows (a third of the 1200 px display
/// each) on a 2000 px viewport, with window `focused` the one in use.
/// `middle`, when given, stands in for the middle window.
pub(super) fn edge_strip(
    focused: Option<usize>,
    middle: Option<WindowGroup>,
) -> (GroupRow, TaffyTree<()>, NodeId) {
    let mut groups = vec![
        make_group(100, &[1]),
        make_group(200, &[2]),
        make_group(300, &[3]),
    ];
    if let Some(middle) = middle {
        groups[1] = middle;
    }
    for g in &mut groups {
        g.set_width(WindowWidth::Narrow);
    }
    let mut row = GroupRow::new(groups);
    row.set_output_spans(&one_display(FIXTURE_USABLE_PX));
    if let Some(i) = focused {
        let _ = row.groups[i].set_focused(true);
    }
    let (row, _, _) = relayout_row_owned(row, 2000.0);
    relayout_row_owned(row, 2000.0)
}

fn files_window(
    min: f64,
) -> (
    WindowGroup,
    std::rc::Rc<std::cell::Cell<(Option<WindowEdge>, bool)>>,
) {
    let anchor = std::rc::Rc::new(std::cell::Cell::new((None, false)));
    let group = WindowGroup::new(
        200,
        vec![(
            TabDescriptor::new(2, "Files"),
            TileContent::App(Box::new(MinWidthApp {
                min,
                anchor: std::rc::Rc::clone(&anchor),
                rect: Rect::ZERO,
            })),
        )],
        2,
        WindowWidth::Narrow,
    );
    (group, anchor)
}

pub(super) fn edge_event(
    row: &mut GroupRow,
    taffy: &TaffyTree<()>,
    root: NodeId,
    shift: bool,
    event: &WindowEvent,
) -> bool {
    let modifiers = if shift {
        winit::event::Modifiers::from(winit::keyboard::ModifiersState::SHIFT)
    } else {
        winit::event::Modifiers::default()
    };
    Dispatch::new()
        .modifiers(modifiers)
        .layout(taffy, root)
        .send(row, event)
        .handled
}

pub(super) fn move_to(p: Point) -> WindowEvent {
    WindowEvent::CursorMoved {
        device_id: winit::event::DeviceId::dummy(),
        position: winit::dpi::PhysicalPosition::new(p.x, p.y),
    }
}

fn button(state: ElementState) -> WindowEvent {
    WindowEvent::MouseInput {
        device_id: winit::event::DeviceId::dummy(),
        state,
        button: MouseButton::Left,
    }
}

pub(super) fn rects(row: &GroupRow) -> Vec<Rect> {
    row.groups.iter().map(WindowGroup::get_rect).collect()
}

/// Press at `from`, carry the pointer through `steps` (each relative to
/// `from`), laying the strip out after every move as the frame loop
/// does, and return the row (still pressed) and the rects after each
/// step, the press's own frame first.
pub(super) fn drag_edge(
    row: GroupRow,
    taffy: &TaffyTree<()>,
    root: NodeId,
    from: Point,
    steps: &[f64],
    shift: bool,
) -> (GroupRow, TaffyTree<()>, NodeId, Vec<Vec<Rect>>) {
    let mut row = row;
    let _ = edge_event(&mut row, taffy, root, shift, &move_to(from));
    assert!(
        edge_event(&mut row, taffy, root, shift, &button(ElementState::Pressed)),
        "a press on an edge is the resize's, not the app's"
    );
    assert!(row.edge_resize_in_flight());
    let (mut row, mut taffy, mut root) = relayout_row_owned(row, 2000.0);
    let mut frames = vec![rects(&row)];
    for dx in steps {
        let at = Point::new(from.x + dx, from.y);
        assert!(edge_event(&mut row, &taffy, root, shift, &move_to(at)));
        (row, taffy, root) = relayout_row_owned(row, 2000.0);
        frames.push(rects(&row));
    }
    (row, taffy, root, frames)
}

pub(super) fn widths(frame: &[Rect]) -> Vec<f64> {
    frame.iter().map(|r| r.width()).collect()
}

#[test]
fn after_releasing_a_resize_no_window_moves() {
    const VIEWPORT: f64 = 1400.0;
    let (row, _, _) = edge_strip(Some(1), None);
    let (mut row, _, _) = relayout_row_owned(row, VIEWPORT);
    row.set_scroll_x_for_test(60.0);
    let (mut row, taffy, root) = relayout_row_owned(row, VIEWPORT);
    let before = rects(&row);
    assert!(
        before[2].x1 - before[0].x0 <= VIEWPORT,
        "fixture: every window fits on screen: {before:?}"
    );
    assert!(
        before[0].x0 < row.get_rect().x0,
        "fixture: the first window is cut off: {before:?}"
    );
    let from = Point::new(before[2].x0 + 2.0, before[1].center().y);
    let _ = edge_event(&mut row, &taffy, root, false, &move_to(from));
    assert!(edge_event(
        &mut row,
        &taffy,
        root,
        false,
        &button(ElementState::Pressed)
    ));
    let (mut row, taffy, root) = relayout_row_owned(row, VIEWPORT);
    assert!(edge_event(
        &mut row,
        &taffy,
        root,
        false,
        &move_to(Point::new(from.x + 20.0, from.y))
    ));
    let (mut row, taffy, root) = relayout_row_owned(row, VIEWPORT);
    let held = rects(&row);
    assert!(
        (held[1].width() - before[1].width() - 20.0).abs() < 1e-6,
        "fixture: the drag widened the window in use: {held:?}"
    );
    let scroll = row.scroll_x();

    assert!(edge_event(
        &mut row,
        &taffy,
        root,
        false,
        &button(ElementState::Released)
    ));
    assert!(!row.edge_resize_in_flight());
    for _ in 0..3 {
        let (laid, _, _) = relayout_row_owned(row, VIEWPORT);
        row = laid;
    }
    assert!(
        (row.scroll_x() - scroll).abs() < 1e-6,
        "the strip stays where the drag left it: {scroll} became {}",
        row.scroll_x()
    );
    assert!(!row.snap_in_flight(), "and nothing is on its way anywhere");
    for (i, (after, held)) in rects(&row).iter().zip(&held).enumerate() {
        assert!(
            (after.x0 - held.x0).abs() < 0.5 && (after.x1 - held.x1).abs() < 0.5,
            "window {i} moved after the release: {held:?} became {after:?}"
        );
    }
}

/// An app that states its minimum only through its widget tree, as a
/// `NavShell` does ([`Widget::minimum_window_width`]), and nothing on
/// `AppWidget`: an app running in the shell's own process.
struct TreeMinimumApp {
    min: f64,
    rect: Rect,
}

impl Widget for TreeMinimumApp {
    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        _theme: &Theme,
        node_map: &mut std::collections::HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        taffy.fill_leaf(node_map, self)
    }
    tilo_ui_base::rect_geometry!();
    fn name(&self) -> &'static str {
        "TreeMinimumApp"
    }
    fn minimum_window_width(&self) -> Option<f64> {
        Some(self.min)
    }
}

impl crate::widget::AppWidget for TreeMinimumApp {
    fn app_kind(&self) -> crate::widget::AppKind {
        crate::widget::AppKind::Files
    }
    fn app_title(&self) -> &'static str {
        "Files"
    }
    fn app_icon(&self) -> crate::widget::IconDescriptor {
        crate::widget::IconDescriptor::Glyph("F")
    }
}

#[test]
fn an_in_process_apps_layout_minimum_stops_the_edge() {
    let group = WindowGroup::new(
        200,
        vec![(
            TabDescriptor::new(2, "Files"),
            TileContent::App(Box::new(TreeMinimumApp {
                min: 340.0,
                rect: Rect::ZERO,
            })),
        )],
        2,
        WindowWidth::Narrow,
    );
    let (row, taffy, root) = edge_strip(Some(1), Some(group));
    assert_eq!(row.groups[1].width_limits().min, 340.0);
    let before = rects(&row);
    let from = Point::new(before[1].x0 + 2.0, before[1].center().y);
    let (_row, _taffy, _root, frames) = drag_edge(row, &taffy, root, from, &[150.0], false);
    assert_eq!(widths(&frames[1])[1], 340.0, "stops at the app's minimum");
}

#[test]
fn a_limit_stops_the_edge_and_resizes_nothing_else() {
    {
        let (files, anchor) = files_window(320.0);
        let (row, taffy, root) = edge_strip(Some(1), Some(files));
        let before = rects(&row);
        let from = Point::new(before[1].x0 + 2.0, before[1].center().y);
        let (mut row, taffy, root, frames) =
            drag_edge(row, &taffy, root, from, &[150.0, 600.0], false);
        assert_eq!(
            anchor.get(),
            (Some(WindowEdge::Trailing), false),
            "the app hears that its right edge holds while its left one moves"
        );
        assert_eq!(
            widths(&frames[1]),
            vec![400.0, 320.0, 400.0],
            "stops at 320"
        );
        assert_eq!(
            widths(&frames[2]),
            vec![400.0, 320.0, 400.0],
            "and stays there"
        );
        assert!(
            (frames[2][1].x1 - before[1].x1).abs() < 1e-6,
            "right edge holds"
        );
        assert!(edge_event(
            &mut row,
            &taffy,
            root,
            false,
            &button(ElementState::Released)
        ));
        assert!(anchor.get().1, "and that the drag ended");
    }
    {
        let (row, taffy, root) = edge_strip(Some(1), None);
        let before = rects(&row);
        let from = Point::new(before[1].x1 - 2.0, before[1].center().y);
        let (_row, _taffy, _root, frames) = drag_edge(row, &taffy, root, from, &[-900.0], false);
        assert_eq!(
            widths(&frames[1]),
            vec![400.0, min_group_px(FIXTURE_USABLE_PX), 400.0]
        );
    }
    {
        let (files, _anchor) = files_window(320.0);
        let (row, taffy, root) = edge_strip(Some(1), Some(files));
        let before = rects(&row);
        let from = Point::new(before[1].x0 + 2.0, before[1].center().y);
        let (_row, _taffy, _root, frames) = drag_edge(row, &taffy, root, from, &[200.0], true);
        assert_eq!(widths(&frames[1]), vec![480.0, 320.0, 400.0]);
    }
}

tilo_test_env::scenario! {
    fn maximize_fullscreen_and_hiding_keep_the_row;

    phase maximize_and_fullscreen_absorb_the_row_and_restore_exactly;
    {
        fn maximized_group_hides_siblings_and_absorbs_row_width() {
            use taffy::AvailableSpace;

            let (mut row, taffy, root) = make_row_with_groups_with_taffy(&[&[1], &[2], &[3]]);
            assert!(row.groups[0].get_rect().width() > 0.0);
            assert!(row.groups[1].get_rect().width() > 0.0);
            assert!(row.groups[2].get_rect().width() > 0.0);
            assert_eq!(row.handle_count(), 2, "two gaps → two handles pre-maximize");

            row.groups[1].maximized = true;
            let theme = Theme::dark();
            let mut taffy_new: TaffyTree<()> = TaffyTree::new();
            let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
            let new_root = row.build_layout(&mut taffy_new, &theme, &mut node_map);
            let viewport = taffy_new.parent_node(
                Style {
                    size: Size {
                        width: Dimension::length(2000.0),
                        height: Dimension::length(600.0),
                    },
                    ..Default::default()
                },
                &[new_root],
            );
            taffy_new
                .compute_layout_with_measure(
                    viewport,
                    taffy::Size {
                        width: AvailableSpace::Definite(2000.0),
                        height: AvailableSpace::Definite(600.0),
                    },
                    |_, _, _, _, _| taffy::Size::ZERO,
                )
                .expect("taffy compute_layout");
            row.apply_layout(&taffy_new, new_root, Point::ORIGIN);
            let _ = taffy; // silence unused-binding lint from the helper.
            let _ = root;

            let r0 = row.groups[0].get_rect();
            let r2 = row.groups[2].get_rect();
            assert!(
                r0.width() > 1.0,
                "left sibling keeps its last tiled rect so the fade can draw it; got {}",
                r0.width(),
            );
            assert!(
                r2.width() > 1.0,
                "right sibling keeps its last tiled rect so the fade can draw it; got {}",
                r2.width(),
            );

            let r1 = row.groups[1].get_rect();
            let row_w = row.rect.width();
            assert!(
                (r1.width() - row_w).abs() < 1.0,
                "maximized group must absorb the row's full width; got group={} row={}",
                r1.width(),
                row_w,
            );

            // Pool itself stays sized to N-1; `handle_nodes` must be
            // empty so no draw/dispatch runs on collapsed gaps.
            assert_eq!(
                row.handle_nodes.iter().flatten().count(),
                0,
                "no handle taffy nodes must be built while a group is maximized",
            );
        }

        fn unmaximize_restores_siblings_and_handles() {
            let (mut row, _t, _r) = make_row_with_groups_with_taffy(&[&[1], &[2], &[3]]);
            row.groups[1].maximized = true;
            let theme = Theme::dark();
            let relayout = |row: &mut GroupRow| -> TaffyTree<()> {
                use taffy::AvailableSpace;

                let mut taffy: TaffyTree<()> = TaffyTree::new();
                let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
                let root = row.build_layout(&mut taffy, &theme, &mut node_map);
                let viewport = taffy.parent_node(
                    Style {
                        size: Size {
                            width: Dimension::length(2000.0),
                            height: Dimension::length(600.0),
                        },
                        ..Default::default()
                    },
                    &[root],
                );
                taffy
                    .compute_layout_with_measure(
                        viewport,
                        taffy::Size {
                            width: AvailableSpace::Definite(2000.0),
                            height: AvailableSpace::Definite(600.0),
                        },
                        |_, _, _, _, _| taffy::Size::ZERO,
                    )
                    .expect("taffy compute_layout");
                row.apply_layout(&taffy, root, Point::ORIGIN);
                taffy
            };
            let _ = relayout(&mut row);

            row.groups[1].maximized = false;
            let _ = relayout(&mut row);

            for (i, g) in row.groups.iter().enumerate() {
                assert!(
                    g.get_rect().width() > 0.0,
                    "group {i} must regain positive width after un-maximize",
                );
            }
            assert_eq!(
                row.handle_nodes.iter().flatten().count(),
                2,
                "handle nodes must be rebuilt on un-maximize (2 gaps → 2 handles)",
            );
        }

        fn maximized_group_drops_the_strip_pan_for_output_aligned_controls() {
            let mut row = strip_at(4, 1280.0, 248.0, 667);
            row.set_scroll_x_for_test(248.0);
            row.groups[3].maximized = true;

            relayout_strip(&mut row, 1280.0);

            let rect = row.groups[3].get_rect();
            assert!(
                (rect.x0 - row.rect.x0).abs() < 0.5,
                "maximized content and its Restore/Close controls must start at the output edge; group={rect:?} row={:?}",
                row.rect,
            );
            assert!(
                (rect.x1 - row.rect.x1).abs() < 0.5,
                "maximized content and its controls must end at the output edge; group={rect:?} row={:?}",
                row.rect,
            );
        }

        fn fullscreen_group_collapses_siblings_and_absorbs_row() {
            let (mut row, _t, _r) = make_row_with_groups_with_taffy(&[&[1], &[2], &[3]]);
            assert!(!row.is_fullscreen());
            row.set_fullscreen(Some(200), false);
            assert!(
                row.groups.iter().all(|g| !g.maximized),
                "no persisted maximize"
            );
            relayout_row(&mut row);

            assert!(
                row.is_fullscreen(),
                "row reports it owns the fullscreen group"
            );
            assert!(
                row.groups[0].get_rect().width() > 1.0,
                "left sibling keeps its last tiled rect under fullscreen; got {}",
                row.groups[0].get_rect().width(),
            );
            assert!(
                row.groups[2].get_rect().width() > 1.0,
                "right sibling keeps its last tiled rect under fullscreen; got {}",
                row.groups[2].get_rect().width(),
            );
            // The wheel gutter is dropped because the sidebar is suppressed.
            let r1 = row.groups[1].get_rect();
            assert!(
                (r1.width() - row.rect.width()).abs() < 1.0,
                "fullscreen group absorbs the row width; got {} vs {}",
                r1.width(),
                row.rect.width(),
            );
            assert!(
                (r1.x0 - row.rect.x0).abs() < 1.0,
                "fullscreen group pins flush-left with no wheel gutter; got x0 {} vs {}",
                r1.x0,
                row.rect.x0,
            );
            assert_eq!(
                row.handle_nodes.iter().flatten().count(),
                0,
                "no resize handles under fullscreen"
            );
        }

        fn exiting_fullscreen_restores_prior_layout_exactly() {
            let (mut row, _t, _r) = make_row_with_groups_with_taffy(&[&[1], &[2], &[3]]);
            let before: Vec<Rect> = row.groups.iter().map(WindowGroup::get_rect).collect();
            let handles_before = row.handle_nodes.iter().flatten().count();
            assert_eq!(handles_before, 2);

            row.set_fullscreen(Some(300), false);
            relayout_row(&mut row);
            assert!(row.is_fullscreen());
            row.set_fullscreen(None, false);
            relayout_row(&mut row);

            assert!(!row.is_fullscreen(), "exit clears the fullscreen promotion");
            let after: Vec<Rect> = row.groups.iter().map(WindowGroup::get_rect).collect();
            for (i, (b, a)) in before.iter().zip(after.iter()).enumerate() {
                assert!(
                    (b.x0 - a.x0).abs() < 0.5
                        && (b.x1 - a.x1).abs() < 0.5
                        && (b.width() - a.width()).abs() < 0.5,
                    "group {i} rect must restore exactly: before {b:?} after {a:?}",
                );
            }
            assert_eq!(
                row.handle_nodes.iter().flatten().count(),
                2,
                "resize handles rebuilt on fullscreen exit",
            );
        }

        fn hidden_row_collapses_and_restores() {
            let (mut row, _t, _r) = make_row_with_groups_with_taffy(&[&[1], &[2]]);
            let before: Vec<Rect> = row.groups.iter().map(WindowGroup::get_rect).collect();
            assert!(before.iter().all(|r| r.width() > 0.0));

            row.set_fullscreen(None, true);
            relayout_row(&mut row);
            for (i, g) in row.groups.iter().enumerate() {
                let r = g.get_rect();
                assert!(
                    r.width() <= 0.5 && r.height() <= 0.5,
                    "group {i} in a hidden strip must collapse; got {r:?}",
                );
            }

            row.set_fullscreen(None, false);
            relayout_row(&mut row);
            let after: Vec<Rect> = row.groups.iter().map(WindowGroup::get_rect).collect();
            for (i, (b, a)) in before.iter().zip(after.iter()).enumerate() {
                assert!(
                    (b.width() - a.width()).abs() < 0.5,
                    "group {i} width restores after un-hide: before {} after {}",
                    b.width(),
                    a.width(),
                );
            }
        }

        maximized_group_hides_siblings_and_absorbs_row_width();
        unmaximize_restores_siblings_and_handles();
        maximized_group_drops_the_strip_pan_for_output_aligned_controls();
        fullscreen_group_collapses_siblings_and_absorbs_row();
        exiting_fullscreen_restores_prior_layout_exactly();
        hidden_row_collapses_and_restores();
    }

    phase the_bar_shown_over_the_shell_leaves_the_covering_window_where_it_stands;
    {
        let (mut row, _t, _r) = make_row_with_groups_with_taffy(&[&[1], &[2], &[3]]);
        row.set_fullscreen(Some(200), false);
        row.set_top_overlay_inset(32.0);
        let insets: Vec<f64> = row
            .groups
            .iter()
            .map(WindowGroup::top_overlay_inset)
            .collect();
        assert_eq!(
            insets,
            [32.0, 0.0, 32.0],
            "only the windows that do not cover the display move down"
        );
        let bars: Vec<Option<f64>> = row.groups.iter().map(|g| g.fullscreen_bar).collect();
        assert_eq!(bars, [None, Some(32.0), None], "its own bar stands under the shell's");
        row.set_fullscreen(None, false);
        row.set_top_overlay_inset(32.0);
        assert!(
            row.groups
                .iter()
                .all(|group| (group.top_overlay_inset() - 32.0).abs() < f64::EPSILON),
            "every window moves down once none covers the display"
        );
    }

}

#[test]
fn maximize_fades_siblings_on_the_fast_curve_and_keeps_velocity() {
    // Fresh curves are read in the instant they start: a descheduled
    // runner cannot age them.
    crate::time::testing::with_monotonic_time(crate::time::now(), || {
        {
            let (row, _t, _r) = make_row_with_groups_with_taffy(&[&[1], &[2], &[3]]);
            let before = row.groups[0].get_rect().width();
            assert!(
                (row.sibling_fade_value() - 1.0).abs() < 0.02,
                "a tiled strip starts with siblings fully visible",
            );
            let mut row = row;
            row.groups[1].maximized = true;
            let (row, ..) = relayout_row_owned_with(row, 2000.0, &Theme::dark());
            let mut row = row;
            assert!(
                row.sibling_fade_is_animating(),
                "maximize must run the sibling fade, not snap Display::None",
            );
            assert!(
                (row.groups[0].get_rect().width() - before).abs() < 1.0,
                "the fading sibling keeps its last tiled width, was {before} now {}",
                row.groups[0].get_rect().width(),
            );
            row.advance_sibling_fade(Duration::from_millis(32));
            let mid = row.sibling_fade_value();
            assert!(
                mid > 0.2 && mid < 0.8,
                "a quarter of the way through fast the sibling is still on screen, got {mid}",
            );

            row.groups[1].maximized = false;
            let (row, ..) = relayout_row_owned_with(row, 2000.0, &Theme::dark());
            let after = row.sibling_fade_value();
            assert!(
                (after - mid).abs() < 0.15,
                "a new Super+M must keep the current alpha, was {mid} now {after}",
            );
            assert!(
                row.sibling_fade_is_animating(),
                "restore from mid-fade must keep pumping toward visible",
            );
        }

        {
            let (mut row, _t, _r) = make_row_with_groups_with_taffy(&[&[1], &[2], &[3]]);
            row.groups[1].maximized = true;
            let theme = Theme::dark().with_reduce_motion(true);
            let (row, ..) = relayout_row_owned_with(row, 2000.0, &theme);
            assert!(
                row.sibling_fade_value() < 0.02,
                "reduced motion hides siblings on the first frame, got {}",
                row.sibling_fade_value(),
            );
            assert!(
                !row.sibling_fade_is_animating(),
                "reduced motion must not pump fade frames",
            );
        }
    });
}

fn relayout_row(row: &mut GroupRow) {
    use taffy::AvailableSpace;

    let theme = Theme::dark();
    let mut taffy: TaffyTree<()> = TaffyTree::new();
    let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
    let root = row.build_layout(&mut taffy, &theme, &mut node_map);
    let viewport = taffy.parent_node(
        Style {
            size: Size {
                width: Dimension::length(2000.0),
                height: Dimension::length(600.0),
            },
            ..Default::default()
        },
        &[root],
    );
    taffy
        .compute_layout_with_measure(
            viewport,
            taffy::Size {
                width: AvailableSpace::Definite(2000.0),
                height: AvailableSpace::Definite(600.0),
            },
            |_, _, _, _, _| taffy::Size::ZERO,
        )
        .expect("taffy compute_layout");
    row.apply_layout(&taffy, root, Point::ORIGIN);
}
