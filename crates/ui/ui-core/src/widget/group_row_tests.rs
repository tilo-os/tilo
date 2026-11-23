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

    phase transfer_tab_moves_between_groups_and_ignores_same_or_unknown;
    {
        fn transfer_tab_moves_descriptor_and_host_between_groups() {
            let g0 = make_group(100, &[1, 2, 3]);
            let g1 = make_group(200, &[10, 11]);
            let mut row = GroupRow::new(vec![g0, g1]);

            assert!(row.transfer_tab(0, 1, 2, 0));

            let g0 = &row.groups[0];
            let g1 = &row.groups[1];
            let g0_ids: Vec<u64> = g0.tabs().iter().map(|t| t.id).collect();
            let g1_ids: Vec<u64> = g1.tabs().iter().map(|t| t.id).collect();
            assert_eq!(g0_ids, vec![1, 3]);
            assert_eq!(g1_ids, vec![2, 10, 11]);
        }

        fn transfer_tab_is_noop_for_same_group() {
            let g0 = make_group(100, &[1, 2]);
            let g1 = make_group(200, &[3]);
            let mut row = GroupRow::new(vec![g0, g1]);
            assert!(!row.transfer_tab(0, 0, 1, 0));
        }

        fn transfer_tab_is_noop_for_unknown_tab() {
            let g0 = make_group(100, &[1, 2]);
            let g1 = make_group(200, &[3]);
            let mut row = GroupRow::new(vec![g0, g1]);
            assert!(!row.transfer_tab(0, 1, 999, 0));
        }

        fn transfer_active_tab_promotes_sibling_in_source_and_activates_in_dest() {
            let g0 = make_group(100, &[1, 2, 3]); // active = 1
            let g1 = make_group(200, &[10, 11]); // active = 10
            let mut row = GroupRow::new(vec![g0, g1]);
            assert_eq!(row.groups[0].active_tab_id(), 1);

            assert!(row.transfer_tab(0, 1, 1, 0));

            let src_ids: Vec<u64> = row.groups[0].tabs().iter().map(|t| t.id).collect();
            assert_eq!(src_ids, vec![2, 3]);
            assert_eq!(
                row.groups[0].active_tab_id(),
                2,
                "source must pick a surviving neighbour as the new active"
            );

            let dst_ids: Vec<u64> = row.groups[1].tabs().iter().map(|t| t.id).collect();
            assert_eq!(dst_ids, vec![1, 10, 11]);
            assert_eq!(
                row.groups[1].active_tab_id(),
                1,
                "dragged tab should be active in its new group"
            );
        }

        fn active_tab_address_must_exist_even_when_the_bar_is_stale() {
            let stale = WindowGroup::new(
                100,
                vec![(
                    TabDescriptor::new(2, "surviving"),
                    TileContent::Stub {
                        kind: crate::widget::AppKind::Settings,
                        label: "surviving".into(),
                    },
                )],
                1,
                WindowWidth::Normal,
            );
            let mut row = GroupRow::new(vec![stale]);

            assert!(
                !row.set_group_active_tab(100, 1),
                "an active id left behind by a close is not a valid focus target"
            );
        }

        transfer_tab_moves_descriptor_and_host_between_groups();
        transfer_tab_is_noop_for_same_group();
        transfer_tab_is_noop_for_unknown_tab();
        transfer_active_tab_promotes_sibling_in_source_and_activates_in_dest();
        active_tab_address_must_exist_even_when_the_bar_is_stale();
    }

    phase split_tab_into_new_group_inserts_at_gap_and_prunes_source;
    {
        fn split_tab_into_new_group_appends_with_empty_source_pruned() {
            let g0 = make_group(100, &[1]);
            let g1 = make_group(200, &[10, 11]);
            let mut row = GroupRow::new(vec![g0, g1]);

            assert!(row.split_tab_into_new_group(0, 1, 2));

            assert_eq!(row.groups.len(), 2);
            let first_ids: Vec<u64> = row.groups[0].tabs().iter().map(|t| t.id).collect();
            let second_ids: Vec<u64> = row.groups[1].tabs().iter().map(|t| t.id).collect();
            assert_eq!(first_ids, vec![10, 11]);
            assert_eq!(second_ids, vec![1]);
            assert_ne!(row.groups[1].id, 200, "new group id must be unique");
        }

        fn split_tab_into_new_group_inserts_between_existing_groups() {
            let g0 = make_group(100, &[1, 2, 3]);
            let g1 = make_group(200, &[10, 11]);
            let mut row = GroupRow::new(vec![g0, g1]);

            assert!(row.split_tab_into_new_group(0, 2, 1));

            assert_eq!(row.groups.len(), 3);
            let ids: Vec<Vec<u64>> = row
                .groups
                .iter()
                .map(|g| g.tabs().iter().map(|t| t.id).collect())
                .collect();
            assert_eq!(ids[0], vec![1, 3]);
            assert_eq!(ids[1], vec![2]);
            assert_eq!(ids[2], vec![10, 11]);
            assert_eq!(row.groups[1].active_tab_id(), 2);
        }

        fn split_tab_into_new_group_flags_structure_dirty_once() {
            let g0 = make_group(100, &[1, 2, 3]);
            let g1 = make_group(200, &[10, 11]);
            let mut row = GroupRow::new(vec![g0, g1]);

            assert!(!row.take_structure_dirty(), "fresh row must not be dirty");

            assert!(row.split_tab_into_new_group(0, 2, 1));
            assert!(
                row.take_structure_dirty(),
                "successful split must flag structure_dirty for the App to rebuild taffy"
            );
            assert!(
                !row.take_structure_dirty(),
                "flag must clear after first poll so we don't rebuild every frame"
            );

            assert!(!row.split_tab_into_new_group(0, 9999, 0));
            assert!(
                !row.take_structure_dirty(),
                "failed split must not flag structure_dirty"
            );
        }

        fn split_tab_into_new_group_noop_for_unknown_tab() {
            let g0 = make_group(100, &[1, 2]);
            let g1 = make_group(200, &[3]);
            let mut row = GroupRow::new(vec![g0, g1]);
            assert!(!row.split_tab_into_new_group(0, 999, 0));
            assert_eq!(row.groups.len(), 2);
            let g0_ids: Vec<u64> = row.groups[0].tabs().iter().map(|t| t.id).collect();
            assert_eq!(g0_ids, vec![1, 2]);
        }

        fn split_tab_into_new_group_prepends_when_gap_zero() {
            let g0 = make_group(100, &[1, 2]);
            let g1 = make_group(200, &[10]);
            let mut row = GroupRow::new(vec![g0, g1]);

            assert!(row.split_tab_into_new_group(0, 1, 0));

            assert_eq!(row.groups.len(), 3);
            let ids: Vec<Vec<u64>> = row
                .groups
                .iter()
                .map(|g| g.tabs().iter().map(|t| t.id).collect())
                .collect();
            assert_eq!(ids[0], vec![1]);
            assert_eq!(ids[1], vec![2]);
            assert_eq!(ids[2], vec![10]);
        }

        split_tab_into_new_group_appends_with_empty_source_pruned();
        split_tab_into_new_group_inserts_between_existing_groups();
        split_tab_into_new_group_flags_structure_dirty_once();
        split_tab_into_new_group_noop_for_unknown_tab();
        split_tab_into_new_group_prepends_when_gap_zero();
    }

    phase tab_drop_targets_classify_gaps_and_hold_hysteresis;
    {
        fn gap_index_for_classifies_inter_group_drops() {
            use std::collections::HashMap;
            use taffy::{AvailableSpace, Size as TSize, TaffyTree};

            let g0 = make_group(100, &[1]);
            let g1 = make_group(200, &[10]);
            let mut row = GroupRow::new(vec![g0, g1]);
            let theme = Theme::dark();
            let mut taffy: TaffyTree<()> = TaffyTree::new();
            let mut node_map: HashMap<taffy::NodeId, *mut dyn Widget> = HashMap::new();
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
                    TSize {
                        width: AvailableSpace::Definite(2000.0),
                        height: AvailableSpace::Definite(600.0),
                    },
                    |_, _, _, _, _| taffy::Size::ZERO,
                )
                .expect("taffy compute_layout");
            row.apply_layout(&taffy, root, Point::ORIGIN);

            let r0 = row.groups[0].get_rect();
            let r1 = row.groups[1].get_rect();
            let row_rect = row.rect;
            let y = f64::midpoint(row_rect.y0, row_rect.y1);

            assert_eq!(row.gap_index_for(Point::new(r0.x0 - 4.0, y)), Some(0));
            assert_eq!(row.gap_index_for(Point::new(r1.x1 + 4.0, y)), Some(2));
            assert_eq!(
                row.gap_index_for(Point::new(r0.x0 + 4.0, y)),
                None,
                "drops inside a group tile should not be inter-group gaps"
            );
            assert_eq!(
                row.gap_index_for(Point::new(r0.x0 - 4.0, row_rect.y0 - 10.0)),
                Some(0)
            );
            // Only assert if the layout actually left a gap.
            if r1.x0 > r0.x1 {
                let mid = f64::midpoint(r0.x1, r1.x0);
                assert_eq!(row.gap_index_for(Point::new(mid, y)), Some(1));
            }
        }

        fn unified_tab_targets_cover_normal_and_compact_windows() {
            assert_unified_tab_targets_at_width(FIXTURE_USABLE_PX);
            assert_unified_tab_targets_at_width(480.0);
        }

        fn tab_target_hysteresis_retains_preview_then_crosses_cleanly() {
            let mut row = make_row_with_groups_on_display(&[&[1, 2], &[10, 11]], 1200.0, 2000.0).0;
            let rect = row.groups[1].get_rect();
            let body_y = f64::midpoint(row.groups[1].bar_rect().y1, rect.y1);
            let boundary = rect.x0 + tab_group_edge_width(rect.width());
            row.tab_drop_target = Some(TabDropTarget::GroupGap { gap_index: 1 });

            assert_eq!(
                row.tab_drop_target_with_hysteresis(Point::new(boundary + 2.0, body_y)),
                row.tab_drop_target,
                "two pixels of boundary jitter must retain the indicator the user saw",
            );
            assert!(matches!(
                row.tab_drop_target_with_hysteresis(Point::new(
                    boundary + TAB_DROP_HYSTERESIS_PX + 2.0,
                    body_y,
                )),
                Some(TabDropTarget::ExistingGroup { tab_index: 0, .. })
            ));
        }

        fn release_commits_the_previewed_target_without_resolving_again() {
            let (mut row, taffy, root) = make_row_with_groups_with_taffy(&[&[1, 2], &[10, 11]]);
            let target_id = row.groups[1].id;
            let misleading_release = Point::new(
                row.groups[1].get_rect().x1 - 1.0,
                row.groups[1].get_rect().center().y,
            );
            assert!(matches!(
                row.tab_drop_target_at(misleading_release),
                Some(TabDropTarget::GroupGap { .. })
            ));
            row.tab_drop_target = Some(TabDropTarget::ExistingGroup {
                group_id: target_id,
                tab_index: 0,
            });
            row.groups[0].bar_mut().pending_detach = Some((1, misleading_release));

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
                row.groups.len(),
                2,
                "release must not create an unpreviewed group"
            );
            assert_eq!(
                row.groups[1]
                    .tabs()
                    .iter()
                    .map(|tab| tab.id)
                    .collect::<Vec<_>>(),
                vec![1, 10, 11],
            );
            assert_eq!(
                row.tab_drop_target(),
                None,
                "release clears the consumed preview"
            );
        }

        gap_index_for_classifies_inter_group_drops();
        unified_tab_targets_cover_normal_and_compact_windows();
        tab_target_hysteresis_retains_preview_then_crosses_cleanly();
        release_commits_the_previewed_target_without_resolving_again();
    }

    phase cross_strip_moves_accept_exact_targets_and_refuse_stale_ones;
    {
        fn cross_strip_accept_consumes_exact_existing_group_target() {
            let mut row = make_row_with_groups_on_display(&[&[1, 2]], 1200.0, 2000.0).0;
            let mut source = make_group(900, &[9]);
            let (desc, host) = source.detach_tab(9).expect("detached source tab");
            let target = TabDropTarget::ExistingGroup {
                group_id: row.groups[0].id,
                tab_index: 1,
            };
            row.cross_strip_drop_target = Some(target);

            assert!(row.accept_cross_strip_drop(901, desc, host, target));
            assert_eq!(
                row.groups[0]
                    .tabs()
                    .iter()
                    .map(|tab| tab.id)
                    .collect::<Vec<_>>(),
                vec![1, 9, 2]
            );
            assert_eq!(row.cross_strip_drop_target(), None);
        }

        fn stale_cross_strip_existing_group_target_is_refused_without_mutation() {
            let mut row = make_row_with_groups_on_display(&[&[1, 2]], 1200.0, 2000.0).0;
            let mut source = make_group(900, &[9]);
            let (desc, host) = source.detach_tab(9).expect("detached source tab");

            assert!(!row.accept_cross_strip_drop(
                901,
                desc,
                host,
                TabDropTarget::ExistingGroup {
                    group_id: u64::MAX,
                    tab_index: 0,
                },
            ));
            assert_eq!(row.groups.len(), 1);
            assert_eq!(
                row.groups[0]
                    .tabs()
                    .iter()
                    .map(|tab| tab.id)
                    .collect::<Vec<_>>(),
                vec![1, 2]
            );
        }

        fn refused_cross_strip_move_restores_exact_group_and_tab_order() {
            let mut row = make_row_with_groups(&[&[1, 2, 3], &[10]]);
            let source_group_index = 0;
            let source_group_id = row.groups[source_group_index].id;
            let source_tab_index = 1;
            let (desc, host) = row.groups[source_group_index]
                .detach_tab(2)
                .expect("fixture tab detaches");
            row.restore_cross_strip_detach(CrossStripDetach {
                tab_id: 2,
                source_group_id,
                source_group_index,
                source_tab_index,
                desc,
                host,
                release_point: Point::new(900.0, 900.0),
            });

            assert_eq!(row.group_ids(), vec![100, 200]);
            assert_eq!(
                row.groups[0]
                    .tabs()
                    .iter()
                    .map(|tab| tab.id)
                    .collect::<Vec<_>>(),
                vec![1, 2, 3],
            );
        }

        cross_strip_accept_consumes_exact_existing_group_target();
        stale_cross_strip_existing_group_target_is_refused_without_mutation();
        refused_cross_strip_move_restores_exact_group_and_tab_order();
    }

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

    phase a_closing_window_lets_its_frame_settle;
    {
        let mut row = make_row_with_groups(&[&[1], &[2], &[3]]);
        row.take_structure_dirty();
        row.groups[0].start_closing();
        let mut passes = 0;
        let settled = crate::widget::layout::settle_widget_composition(false, |_| {
            passes += 1;
            Ok((Some(Vec::new()), row.take_structure_dirty()))
        });
        assert!(settled.is_ok(), "{settled:?} after {passes} passes");
        assert_eq!(passes, 2, "one rebuild for the fade's frame, then settled");
        row.mark_drawn();
        assert!(
            row.take_structure_dirty(),
            "the next frame steps the fade again"
        );
        assert!(!row.take_structure_dirty());
    }

    phase refused_cross_strip_move_restores_exact_group_and_tab_order;
    {
        let mut row = make_row_with_groups(&[&[1, 2, 3], &[10]]);
        let source_group_index = 0;
        let source_group_id = row.groups[source_group_index].id;
        let source_tab_index = 1;
        let (desc, host) = row.groups[source_group_index]
            .detach_tab(2)
            .expect("fixture tab detaches");
        row.restore_cross_strip_detach(CrossStripDetach {
            tab_id: 2,
            source_group_id,
            source_group_index,
            source_tab_index,
            desc,
            host,
            release_point: Point::new(900.0, 900.0),
        });

        assert_eq!(row.group_ids(), vec![100, 200]);
        assert_eq!(
            row.groups[0]
                .tabs()
                .iter()
                .map(|tab| tab.id)
                .collect::<Vec<_>>(),
            vec![1, 2, 3],
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

    phase single_tab_group_splits_on_any_gap_repeatedly;
    {
        fn single_tab_group_can_split_into_new_group_on_any_adjacent_gap() {
            let mut row = make_row_with_groups(&[&[1]]);
            assert!(
                row.split_tab_into_new_group(0, 1, 1),
                "single-tab group must be splittable into an adjacent gap"
            );
            assert_eq!(row.groups.len(), 1);
            assert_eq!(row.groups[0].tabs().len(), 1);
            assert_eq!(row.groups[0].tabs()[0].id, 1);
            assert!(
                row.take_structure_dirty(),
                "split must flag structure_dirty"
            );
        }

        fn single_tab_group_split_is_not_refused_by_handle_event_sweep() {
            use winit::event::WindowEvent;

            let (mut row, taffy, root) = make_row_with_groups_with_taffy(&[&[7]]);
            let g0_rect = row.groups[0].get_rect();
            let release = Point::new(g0_rect.x1 + 10.0, f64::midpoint(g0_rect.y0, g0_rect.y1));
            row.tab_drop_target = row.tab_drop_target_at(release);
            row.groups[0].bar_mut().pending_detach = Some((7, release));

            let event = WindowEvent::MouseInput {
                device_id: winit::event::DeviceId::dummy(),
                state: ElementState::Released,
                button: MouseButton::Left,
            };
            dispatch_at(&mut row, &taffy, root, &event);

            assert!(
                row.take_structure_dirty(),
                "single-tab source must split into adjacent gap: handle_event sweep should \
         flag structure_dirty. Today the `src_only_tab && adjacent` refuse rule \
         silently skips this path"
            );
            assert_eq!(row.groups.len(), 1);
            assert_eq!(row.groups[0].tabs().len(), 1);
            assert_eq!(row.groups[0].tabs()[0].id, 7);
            assert!(row.groups[0].bar_mut().pending_detach.is_none());
        }

        fn two_sequential_splits_do_not_hang() {
            let (mut row, taffy, root) = make_row_with_groups_with_taffy(&[&[1, 2, 3, 4]]);

            // DROP #1: tab 4 dropped right of the only group's bar → 2nd group.
            let g0_rect = row.groups[0].get_rect();
            let release_1 = Point::new(g0_rect.x1 + 30.0, g0_rect.y0 + 6.0);
            row.tab_drop_target = row.tab_drop_target_at(release_1);
            row.groups[0].bar_mut_for_tests().pending_detach = Some((4, release_1));

            dispatch_at(
                &mut row,
                &taffy,
                root,
                &winit::event::WindowEvent::MouseInput {
                    device_id: winit::event::DeviceId::dummy(),
                    state: winit::event::ElementState::Released,
                    button: winit::event::MouseButton::Left,
                },
            );
            assert_eq!(row.groups.len(), 2, "drop 1 must produce 2 groups");
            assert!(
                row.take_structure_dirty(),
                "split must flag structure_dirty"
            );
            let (taffy, root) = relayout_via_new_taffy_tree(&mut row);

            let g0_rect = row.groups[0].get_rect();
            let g1_rect = row.groups[1].get_rect();
            eprintln!("after drop 1: g0={g0_rect:?} g1={g1_rect:?}");
            eprintln!("  g0 bar_rect={:?}", row.groups[0].bar_rect());
            eprintln!("  g1 bar_rect={:?}", row.groups[1].bar_rect());
            eprintln!("  row.rect={:?}", row.rect);
            let gap_mid = f64::midpoint(g0_rect.x1, g1_rect.x0);
            let release_2 = Point::new(gap_mid, g0_rect.y0 + 6.0);
            eprintln!("  release_2={release_2:?}");
            eprintln!(
                "  gap_index_for(release_2)={:?}",
                row.gap_index_for(release_2)
            );
            row.tab_drop_target = row.tab_drop_target_at(release_2);
            row.groups[0].bar_mut_for_tests().pending_detach = Some((3, release_2));

            dispatch_at(
                &mut row,
                &taffy,
                root,
                &winit::event::WindowEvent::MouseInput {
                    device_id: winit::event::DeviceId::dummy(),
                    state: winit::event::ElementState::Released,
                    button: winit::event::MouseButton::Left,
                },
            );
            assert_eq!(
                row.groups.len(),
                3,
                "drop 2 must produce 3 groups (was {})",
                row.groups.len()
            );
        }

        single_tab_group_can_split_into_new_group_on_any_adjacent_gap();
        single_tab_group_split_is_not_refused_by_handle_event_sweep();
        two_sequential_splits_do_not_hang();
    }

    phase hover_gap_tracks_drag_and_clears_without_one;
    {
        fn hover_gap_tracks_cursor_during_drag() {
            let (mut row, taffy, root) = make_row_with_groups_with_taffy(&[&[1], &[2]]);
            start_drag(&mut row, 0, 1);
            let r0 = row.groups[0].get_rect();
            let r1 = row.groups[1].get_rect();
            let mid_y = f64::midpoint(row.rect.y0, row.rect.y1);
            let mid_gap = Point::new(f64::midpoint(r0.x1, r1.x0), mid_y);

            fire_cursor_moved(&mut row, &taffy, root, mid_gap);

            assert_eq!(
                row.tab_drop_target,
                Some(TabDropTarget::GroupGap { gap_index: 1 }),
                "cursor in the gap must preview that exact group gap"
            );
        }

        fn hover_gap_clears_on_release() {
            let (mut row, taffy, root) = make_row_with_groups_with_taffy(&[&[1], &[2]]);
            start_drag(&mut row, 0, 1);
            let r0 = row.groups[0].get_rect();
            let r1 = row.groups[1].get_rect();
            let mid_y = f64::midpoint(row.rect.y0, row.rect.y1);
            let mid_gap = Point::new(f64::midpoint(r0.x1, r1.x0), mid_y);
            fire_cursor_moved(&mut row, &taffy, root, mid_gap);
            assert!(
                row.tab_drop_target.is_some(),
                "precondition: drag tracked the gap"
            );

            let event = WindowEvent::MouseInput {
                device_id: winit::event::DeviceId::dummy(),
                state: ElementState::Released,
                button: MouseButton::Left,
            };
            dispatch_at(&mut row, &taffy, root, &event);

            assert_eq!(
                row.tab_drop_target, None,
                "MouseInput::Released must clear the preview so no ghost indicator persists"
            );
        }

        fn hover_gap_is_none_when_no_drag() {
            let (mut row, taffy, root) = make_row_with_groups_with_taffy(&[&[1], &[2]]);
            let r0 = row.groups[0].get_rect();
            let r1 = row.groups[1].get_rect();
            let mid_y = f64::midpoint(row.rect.y0, row.rect.y1);
            let mid_gap = Point::new(f64::midpoint(r0.x1, r1.x0), mid_y);

            fire_cursor_moved(&mut row, &taffy, root, mid_gap);

            assert_eq!(
                row.tab_drop_target, None,
                "cursor motion without an active drag must not publish a drop preview"
            );
        }

        hover_gap_tracks_cursor_during_drag();
        hover_gap_clears_on_release();
        hover_gap_is_none_when_no_drag();
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

        fn group_retained_during_close_fade() {
            use winit::event::WindowEvent;

            let (mut row, taffy, root) = make_row_with_groups_with_taffy(&[&[1], &[2]]);
            row.groups[0].set_closing_at_for_tests(crate::time::now());
            assert!(row.groups[0].is_closing());
            assert!(!row.groups[0].close_complete());

            let event = WindowEvent::CursorMoved {
                device_id: winit::event::DeviceId::dummy(),
                position: winit::dpi::PhysicalPosition::new(5000.0, 5000.0),
            };
            dispatch_at(&mut row, &taffy, root, &event);

            assert_eq!(
                row.groups.len(),
                2,
                "group mid-fade must NOT be pruned — the user is still watching the animation",
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
        group_retained_during_close_fade();
        #[cfg(debug_assertions)]
        group_prune_keeps_node_ids_aligned_in_debug();
    }
}

fn assert_unified_tab_targets_at_width(usable: f64) {
    let row = make_row_with_groups_on_display(&[&[1, 2], &[10, 11, 12]], usable, 2000.0).0;
    let group = &row.groups[1];
    let rect = group.get_rect();
    let bar = group.bar_rect();
    let body_y = f64::midpoint(bar.y1, rect.y1);
    let edge = tab_group_edge_width(rect.width());
    assert!(
        edge * 2.0 <= rect.width() * (TAB_GROUP_EDGE_MAX_FRAC * 2.0 + f64::EPSILON),
        "edge targets must leave at least 36% of a compact body for tab insertion"
    );

    assert_eq!(
        row.tab_drop_target_at(Point::new(rect.x0 + edge - 1.0, body_y)),
        Some(TabDropTarget::GroupGap { gap_index: 1 }),
        "the full left border band creates a group before the window (width {})",
        rect.width(),
    );
    assert_eq!(
        row.tab_drop_target_at(Point::new(rect.x0 + edge, body_y)),
        Some(TabDropTarget::ExistingGroup {
            group_id: group.id,
            tab_index: 0,
        }),
        "the border's inner boundary belongs continuously to the left interior",
    );
    assert_eq!(
        row.tab_drop_target_at(Point::new(rect.center().x - 1.0, body_y)),
        Some(TabDropTarget::ExistingGroup {
            group_id: group.id,
            tab_index: 0,
        }),
    );
    assert_eq!(
        row.tab_drop_target_at(Point::new(rect.center().x, body_y)),
        Some(TabDropTarget::ExistingGroup {
            group_id: group.id,
            tab_index: group.tabs().len(),
        }),
        "the exact centre starts the append half",
    );
    assert_eq!(
        row.tab_drop_target_at(Point::new(rect.x1 - edge, body_y)),
        Some(TabDropTarget::ExistingGroup {
            group_id: group.id,
            tab_index: group.tabs().len(),
        }),
        "the border's inner boundary still belongs to the right interior",
    );
    assert_eq!(
        row.tab_drop_target_at(Point::new(rect.x1 - edge + 1.0, body_y)),
        Some(TabDropTarget::GroupGap { gap_index: 2 }),
        "the full right border band creates a group after the window",
    );

    let pills = group.tab_rects();
    let between_first_two = f64::midpoint(pills[0].x1, pills[1].x0);
    assert_eq!(
        row.tab_drop_target_at(Point::new(between_first_two, bar.center().y)),
        Some(TabDropTarget::ExistingGroup {
            group_id: group.id,
            tab_index: 1,
        }),
        "the top bar keeps exact pill-gap insertion instead of body zones",
    );
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
                width: Dimension::length(crate::num::f32_of_f64(viewport_w)),
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
                width: AvailableSpace::Definite(crate::num::f32_of_f64(viewport_w)),
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
                width: Dimension::length(crate::num::f32_of_f64(viewport_w)),
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
                width: AvailableSpace::Definite(crate::num::f32_of_f64(viewport_w)),
                height: AvailableSpace::Definite(600.0),
            },
            |_, _, _, _, _| taffy::Size::ZERO,
        )
        .expect("taffy compute_layout");
    row.apply_layout(&taffy, root, Point::ORIGIN);
    (row, taffy, root)
}

/// A presenter pans a strip drawn in canvas coordinates. The encoded
/// scene must be the same at two pans, or every pan draws the strip
/// again.
#[test]
fn panning_a_strip_drawn_in_canvas_coordinates_does_not_change_its_scene() {
    let specs: Vec<Vec<u64>> = (1..=12).map(|id| vec![id]).collect();
    let specs: Vec<&[u64]> = specs.iter().map(Vec::as_slice).collect();
    let (mut row, taffy, root) = make_row_with_groups_on_display(&specs, 2400.0, 800.0);
    assert!(
        row.max_scroll_x() > 200.0,
        "the fixture must be able to pan"
    );
    let theme = Theme::dark().with_reduce_motion(true);
    let mut text = TextRenderer::new();
    let mut draw_at = |row: &mut GroupRow, scroll: f64| {
        row.reduce_motion = true;
        row.set_scroll_x_for_test(scroll);
        row.apply_layout(&taffy, root, Point::ORIGIN);
        let mut scene = Scene::new();
        row.draw(&mut crate::draw::DrawCtx::new(
            &mut scene,
            Affine::IDENTITY,
            &mut text,
            &theme,
            crate::presence::PresenceCtx::empty().draw_env(),
        ));
        scene
    };
    row.reduce_motion = true;
    crate::retained_present::begin_retained_scene(true);
    let _warm = draw_at(&mut row, 0.0);
    let rested = draw_at(&mut row, 0.0);
    let again = draw_at(&mut row, 0.0);
    let panned = draw_at(&mut row, 200.0);
    let shift = row.canvas_shift();
    let on_screen = row.groups[0].get_rect().x0;
    crate::retained_present::end_retained_scene();
    row.set_scroll_x_for_test(0.0);
    row.apply_layout(&taffy, root, Point::ORIGIN);
    let at_rest = row.groups[0].get_rect().x0;
    assert!(
        rested.encoding_eq(&again),
        "two settled frames at rest must match before a pan is meaningful"
    );
    assert!(
        rested.encoding_eq(&panned),
        "a pan must leave the strip's scene as it was"
    );
    assert_eq!(
        shift,
        200.0 + row.canvas_pad(),
        "floating surfaces follow the same shift"
    );
    assert!(
        (on_screen - at_rest).abs() > 1.0,
        "drawing must put the windows back into screen coordinates"
    );
}

#[test]
fn an_overscrolled_strip_pans_within_the_padded_canvas() {
    let specs: Vec<Vec<u64>> = (1..=12).map(|id| vec![id]).collect();
    let specs: Vec<&[u64]> = specs.iter().map(Vec::as_slice).collect();
    let (mut row, taffy, root) = make_row_with_groups_on_display(&specs, 2400.0, 800.0);
    let max = row.max_scroll_x();
    let pad = row.canvas_pad();
    assert!(
        pad > row.leading_pan().viewport().width() * STRIP_OVERSCROLL_REACH_FRAC,
        "the pad holds the furthest rubber band"
    );
    assert!(pan_within_canvas(-pad, max, pad) && pan_within_canvas(max + pad, max, pad));
    assert!(!pan_within_canvas(-pad - 1.0, max, pad));

    row.reduce_motion = true;
    row.apply_layout(&taffy, root, Point::ORIGIN);
    // A rubber band in progress: the setters clamp to the content.
    row.pan.scroll_x = -30.0;
    crate::retained_present::begin_retained_scene(true);
    let overscrolled = row.canvas_shift();
    row.set_scroll_x_for_test(120.0);
    let panned = row.canvas_shift();
    crate::retained_present::end_retained_scene();
    let outside = row.canvas_shift();
    assert_eq!(
        overscrolled,
        pad - 30.0,
        "a rubber band past the start pans within the canvas"
    );
    assert_eq!(panned, 120.0 + pad);
    assert_eq!(outside, 0.0, "outside a retained draw nothing is shifted");
}

fn make_row_with_groups(tab_specs: &[&[u64]]) -> GroupRow {
    make_row_with_groups_with_taffy(tab_specs).0
}

struct PannedRow {
    row: GroupRow,
    travel: std::rc::Rc<std::cell::Cell<Option<f64>>>,
}

impl crate::widget::Decorator for PannedRow {
    type Inner = GroupRow;
    fn inner(&self) -> &GroupRow {
        &self.row
    }
    fn inner_mut(&mut self) -> &mut GroupRow {
        &mut self.row
    }
    fn needs_redraw(&self) -> bool {
        self.travel.get().is_some() || (self.row.needs_redraw() || self.row.needs_animation_frame())
    }
    fn update(&mut self, state: &crate::widget::AppModel, path: &ModelPath) -> bool {
        if let Some(travel) = self.travel.take() {
            self.row.begin_strip_pan();
            assert!(
                self.row.update_strip_pan(travel, crate::time::now()),
                "the strip moves"
            );
            self.row.end_strip_pan(crate::time::now());
        }
        self.row.update(state, path)
    }
}

#[test]
fn a_strip_pan_re_applies_positions_without_measuring() {
    let travel = std::rc::Rc::new(std::cell::Cell::new(None));
    let groups = (1..=6).map(|id| make_group(id * 100, &[id])).collect();
    let mut harness = crate::headless::FrameHarness::for_root(
        crate::State::new(),
        PannedRow {
            row: GroupRow::new(groups),
            travel: std::rc::Rc::clone(&travel),
        },
        (1200, 800),
        Theme::dark(),
    );
    for _ in 0..4 {
        harness.relayout();
    }
    let row = |harness: &crate::headless::FrameHarness| {
        let row = &harness.root::<PannedRow>().row;
        (
            row.scroll_x(),
            row.groups[0].get_rect(),
            row.pan_max_scroll_x(),
        )
    };
    let (start, first, max) = row(&harness);
    assert!(max > 300.0, "six windows overflow 1200 px: {max}");
    let nodes = harness.layout_node_ids();

    travel.set(Some(150.0));
    assert!(harness.poll_frame().unwrap(), "a pan presents a frame");
    let stats = harness.last_relayout();
    assert!(!stats.rebuilt, "a pan keeps the composition: {stats:?}");
    assert!(
        !stats.measured,
        "a pan solves no size: {stats:?}, asked by {:?}",
        harness.layout_measure_requesters()
    );
    assert_eq!(harness.layout_node_ids(), nodes);
    let (panned, moved, _) = row(&harness);
    assert!((panned - start - 150.0).abs() < 1e-9, "{start} -> {panned}");
    assert!(
        (first.x0 - moved.x0 - 150.0).abs() < 1e-9,
        "the windows are laid out where the strip panned to: {first:?} -> {moved:?}"
    );
    harness.verify_retained_geometry().unwrap();
}

/// An app whose wake deadline only its paint moves on, as a status pill
/// that clears itself once drawn after its linger.
struct PaintedDeadline {
    deadline: std::rc::Rc<std::cell::Cell<Option<Instant>>>,
    rect: Rect,
}

impl Widget for PaintedDeadline {
    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        _theme: &Theme,
        node_map: &mut std::collections::HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        taffy.fill_leaf(node_map, self)
    }
    crate::rect_geometry!();
    fn draw(&mut self, ctx: &mut crate::draw::DrawCtx<'_>) {
        #[allow(
            unused_variables,
            unused_mut,
            reason = "a draw binds only the parts it uses"
        )]
        let (_, _, _, _, mut env) = ctx.parts();
        self.deadline.set(None);
    }
    fn next_wake_deadline(&self) -> Option<Instant> {
        self.deadline.get()
    }
    fn name(&self) -> &'static str {
        "PaintedDeadline"
    }
}

impl crate::widget::AppWidget for PaintedDeadline {
    fn app_kind(&self) -> crate::widget::AppKind {
        crate::widget::AppKind::Editor
    }
    fn app_title(&self) -> &'static str {
        "Painted deadline"
    }
    fn app_icon(&self) -> crate::widget::IconDescriptor {
        crate::widget::IconDescriptor::Glyph("P")
    }
}

/// A window past the strip's edge is not painted, so a deadline only its
/// paint consumes stays due: once a frame has run for it, it must not
/// wake the host on every frame until the window is shown. Its later
/// deadlines still wake the host, and shown again it is painted.
#[test]
fn an_unpainted_window_does_not_keep_waking_the_host_for_its_paint() {
    let deadline = std::rc::Rc::new(std::cell::Cell::new(None));
    let mut groups: Vec<WindowGroup> = (1..=5).map(|id| make_group(id * 100, &[id])).collect();
    groups.push(WindowGroup::new(
        600,
        vec![(
            TabDescriptor::new(6, "T6"),
            TileContent::App(Box::new(PaintedDeadline {
                deadline: std::rc::Rc::clone(&deadline),
                rect: Rect::ZERO,
            })),
        )],
        6,
        WindowWidth::Normal,
    ));
    let travel = std::rc::Rc::new(std::cell::Cell::new(None));
    let mut harness = crate::headless::FrameHarness::for_root(
        crate::State::new(),
        PannedRow {
            row: GroupRow::new(groups),
            travel: std::rc::Rc::clone(&travel),
        },
        (1200, 800),
        Theme::dark(),
    );
    for _ in 0..4 {
        harness.relayout();
    }
    let due = |harness: &crate::headless::FrameHarness| {
        harness
            .frame_schedule()
            .next_wake_deadline
            .is_some_and(|at| at <= crate::time::now())
    };

    deadline.set(Some(crate::time::now()));
    assert!(due(&harness), "a due deadline wakes the host");
    assert!(harness.poll_frame().unwrap());
    assert!(
        deadline.get().is_some(),
        "an off-screen window is not painted"
    );
    assert!(
        !due(&harness),
        "the deadline had its frame: no frame after frame"
    );

    let later = crate::time::now() + Duration::from_secs(60);
    deadline.set(Some(later));
    assert_eq!(harness.frame_schedule().next_wake_deadline, Some(later));

    deadline.set(Some(crate::time::now()));
    let max = harness.root::<PannedRow>().row.pan_max_scroll_x();
    travel.set(Some(max));
    assert!(harness.poll_frame().unwrap());
    assert_eq!(deadline.get(), None, "the window shown is painted");
}

/// A width change lands on a slot whose opening spring is still in
/// flight, with a second window arriving beside it. Every frame after
/// that must settle: the springs move the slots, so each frame's rebuild
/// reports work and the composition has to come back to rest before the
/// next one — this is the frame the compositor draws.
#[test]
fn a_width_change_on_an_opening_window_lets_its_frames_settle() {
    crate::motion::forget_frames_for_tests();
    let mut row = strip_at(1, 1280.0, 0.0, crate::widget::WIDTH_NORMAL_PERMILLE);
    row.groups[0].start_spawn_animation();
    let t0 = crate::time::now();
    let frame_at = |row: &mut GroupRow, frame: u64| {
        crate::time::testing::with_monotonic_time(t0 + Duration::from_millis(frame * 16), || {
            row.frame_presented();
            let settle = crate::widget::layout::settle_widget_composition(false, |rebuild| {
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
    row.draw(&mut crate::draw::DrawCtx::new(
        &mut scene,
        Affine::IDENTITY,
        &mut TextRenderer::new(),
        &Theme::dark(),
        crate::presence::PresenceCtx::empty().draw_env(),
    ));
    (row, scene)
}

/// A window whose live picture is wanted is drawn as always and its
/// drawing kept as the picture, at its rect; a window not wanted keeps
/// none, a drawing that showed a secret is never kept, and wanting
/// nothing drops what was kept.
#[test]
fn a_wanted_window_keeps_its_drawing_as_a_live_picture() {
    crate::time::testing::with_monotonic_time(crate::time::now(), || {
        let (plain, _) = drawn_strip(3);
        let wanted = plain.groups[0].id;
        let other = plain.groups[1].id;
        crate::window_pictures::want(&[wanted]);
        let (row, scene) = drawn_strip(3);
        assert!(!scene.encoding().is_empty(), "the window is still drawn");
        let rect = crate::window_pictures::with_picture(wanted, |picture| {
            assert!(!picture.scene.encoding().is_empty());
            picture.rect
        });
        assert_eq!(rect, Some(row.groups[0].get_rect()));
        assert!(crate::window_pictures::with_picture(other, |_| ()).is_none());

        let secret = ApartDrawing {
            id: wanted,
            rect: Rect::new(0.0, 0.0, 100.0, 100.0),
            transform: Affine::IDENTITY,
            scene: Scene::new(),
            parts: None,
            shows_secret: true,
            seen: None,
            current: true,
        };
        secret.offer_picture();
        assert!(
            crate::window_pictures::with_picture(wanted, |_| ()).is_none(),
            "a secret is never shown small"
        );
        let _ = drawn_strip(3);
        assert!(crate::window_pictures::with_picture(wanted, |_| ()).is_some());
        crate::window_pictures::want(&[]);
        assert!(crate::window_pictures::with_picture(wanted, |_| ()).is_none());
    });
}

/// A shell that accumulated dozens of windows must encode only those
/// the row shows: vello sizes its GPU buffers from everything encoded,
/// clipped or not, and WebGPU's 128 MiB binding limit is finite.
#[test]
fn a_long_strip_encodes_only_the_groups_it_shows() {
    // Fresh curves are read in the instant they start: a descheduled
    // runner cannot age them.
    crate::time::testing::with_monotonic_time(crate::time::now(), || {
        let (few, few_scene) = drawn_strip(6);
        let (mut many, many_scene) = drawn_strip(40);
        let shown = many.onscreen_group_ids();
        assert!(
            !shown.is_empty() && shown.len() < 6,
            "fixture shows {shown:?}"
        );
        assert_eq!(few.onscreen_group_ids(), shown);
        assert!(many.culled_groups.len() >= 30, "{:?}", many.culled_groups);
        assert!(
            shown.iter().all(|id| !many.culled_groups.contains(id)),
            "an on-screen group was culled"
        );
        let (few_paths, many_paths) = (few_scene.encoding().n_paths, many_scene.encoding().n_paths);
        assert!(
            many_paths < few_paths * 2,
            "40 windows encoded {many_paths} paths against {few_paths} for 6"
        );

        let (consulted, skipped): (Vec<u64>, Vec<u64>) =
            many.groups.iter().map(|g| g.id).partition(|id| {
                let g = many.groups.iter().find(|g| g.id == *id).expect("group");
                many.paints_within_row(g.get_rect(), 0.0)
            });
        assert_eq!(skipped, many.culled_groups);
        assert!(consulted.len() >= shown.len());

        let offscreen = many.culled_groups[0];
        let group = many
            .groups
            .iter_mut()
            .find(|g| g.id == offscreen)
            .expect("culled group");
        group.start_spawn_animation();
        assert!(
            group.needs_animation_frame(),
            "fixture: the off-screen group is asking for frames"
        );
        assert!(
            !many.needs_animation_frame(),
            "an idle hosted app past the edge must not keep requesting frames"
        );
    });
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
            pull.x > 0.0 && pull.x < f64::from(crate::tokens::Spacing::BASE.s4),
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

    phase scroll_group_into_view_pans_or_eases_only_when_offscreen;
    {
        fn scroll_group_into_view_pans_to_reveal_offscreen_group() {
            let (mut row, _taffy, _root) =
                make_row_with_groups_with_taffy(&[&[1], &[2], &[3], &[4], &[5]]);
            row.set_rect_for_test(Rect::new(0.0, 0.0, 700.0, 600.0));
            row.set_content_w_for_test(row.content_width());
            let target_id = 500; // far-right group, off the viewport's edge
            let before = row.scroll_x();
            let changed = row.scroll_group_into_view(target_id);
            assert!(changed, "an off-screen group must trigger a scroll");
            assert!(
                row.scroll_x() > before,
                "revealing a right-edge group must increase scroll_x (was {before}, now {})",
                row.scroll_x(),
            );
        }

        fn scroll_group_into_view_animated_eases_to_the_same_target() {
            // One row panned by the teleporting variant (reference target),
            // one by the animated variant — must land on the same place.
            let build = || {
                let (mut row, _taffy, _root) =
                    make_row_with_groups_with_taffy(&[&[1], &[2], &[3], &[4], &[5]]);
                row.set_rect_for_test(Rect::new(0.0, 0.0, 700.0, 600.0));
                row.set_content_w_for_test(row.content_width());
                row
            };
            let mut plain = build();
            let mut anim = build();
            let target_id = 500; // far-right group, off the viewport's edge
            assert!(plain.scroll_group_into_view(target_id));
            let expected = plain.scroll_x();
            assert!(expected > 0.0);

            let t0 = crate::time::now();
            assert!(
                anim.scroll_group_into_view_animated(target_id, t0, MotionSource::Pointer),
                "an off-screen group must start an animated pan"
            );
            assert_eq!(
                anim.scroll_x(),
                0.0,
                "the pan is applied by ticks, not the call"
            );
            assert!(
                anim.strip_moving(t0),
                "a pan that has to ease still has to move"
            );
            let snap = crate::tokens::Motion::default().durations.base();
            anim.tick_snap_at(t0 + snap / 2);
            let mid = anim.scroll_x();
            assert!(
                mid > 0.0 && mid < expected,
                "halfway through the curve scroll_x must be between 0 and {expected}, got {mid}"
            );
            anim.tick_snap_at(t0 + Duration::from_secs(1));
            assert_eq!(
                anim.scroll_x(),
                expected,
                "the animated pan must settle on the same target the teleporting variant picks"
            );
            assert!(
                !anim.strip_moving(t0 + Duration::from_secs(1)),
                "a pan that landed has nowhere left to go"
            );
        }

        fn scroll_group_into_view_animated_is_a_noop_for_a_visible_group() {
            let (mut row, _taffy, _root) =
                make_row_with_groups_with_taffy(&[&[1], &[2], &[3], &[4], &[5]]);
            assert!(!row.scroll_group_into_view_animated(
                100,
                crate::time::now(),
                MotionSource::Pointer
            ));
            assert_eq!(row.scroll_x(), 0.0);
            assert!(
                !row.needs_animation_frame(),
                "a no-op must not leave an animation pumping frames"
            );
        }

        scroll_group_into_view_pans_to_reveal_offscreen_group();
        scroll_group_into_view_animated_eases_to_the_same_target();
        scroll_group_into_view_animated_is_a_noop_for_a_visible_group();
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

    phase presenter_pan_precision_moves_real_input_rects_without_rounding_state;
    {
        let mut row = strip_at(3, 800.0, 0.0, 1000);
        let initial = row.groups[0].get_rect().x0;
        row.set_scroll_x_for_test(120.5);
        relayout_strip(&mut row, 800.0);
        assert_eq!(row.groups[0].get_rect().x0, initial - 120.5);
        row.set_scroll_quantum(Some(1.0));
        relayout_strip(&mut row, 800.0);
        assert_eq!(row.groups[0].get_rect().x0, initial - 121.0);
        assert_eq!(row.scroll_x(), 120.5, "the gesture retains its precision");
        row.set_scroll_quantum(None);
        relayout_strip(&mut row, 800.0);
        assert_eq!(row.groups[0].get_rect().x0, initial - 120.5);
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
                row.tick_snap_at(crate::motion::frame_now() + Duration::from_millis(400));
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
            row.tick_snap_at(crate::motion::frame_now() + Duration::from_millis(400));
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

fn start_drag(row: &mut GroupRow, group_idx: usize, tab_id: u64) {
    use crate::dnd::DragPayload;
    row.groups[group_idx].bar_mut().pending_drag = Some(DragPayload::Tab {
        group: 0,
        tab: tab_id,
    });
}

fn fire_cursor_moved(row: &mut GroupRow, taffy: &TaffyTree<()>, root: NodeId, p: Point) {
    let event = WindowEvent::CursorMoved {
        device_id: winit::event::DeviceId::dummy(),
        position: winit::dpi::PhysicalPosition::new(p.x, p.y),
    };
    dispatch_at(row, taffy, root, &event);
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
                width: Dimension::length(crate::num::f32_of_f64(viewport_w)),
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
                width: AvailableSpace::Definite(crate::num::f32_of_f64(viewport_w)),
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
                    width: Dimension::length(crate::num::f32_of_f64(viewport_w)),
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
                    width: AvailableSpace::Definite(crate::num::f32_of_f64(viewport_w)),
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
                width: Dimension::length(crate::num::f32_of_f64(viewport_w)),
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
                width: AvailableSpace::Definite(crate::num::f32_of_f64(viewport_w)),
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

#[test]
fn a_device_pixel_quantum_puts_pan_and_canvas_shift_on_device_pixels() {
    let scale = 1.5;
    let mut row = strip_at(3, 800.0, 0.0, 1000);
    let initial = row.groups[0].get_rect().x0;
    let unaligned_pad = row.canvas_pad();
    row.set_scroll_quantum(Some(1.0 / scale));
    row.set_scroll_x_for_test(120.5);
    relayout_strip(&mut row, 800.0);
    let drawn = row.drawn_scroll_x();
    let device = drawn * scale;
    assert!(
        (device - device.round()).abs() < 1e-5,
        "the drawn pan {drawn} is {device} device pixels"
    );
    assert!(
        (drawn - 120.5).abs() <= 1.0 / 3.0,
        "the nearest step: {drawn}"
    );
    assert!((row.groups[0].get_rect().x0 - (initial - drawn)).abs() < 1e-9);
    let pad = row.canvas_pad();
    let pad_device = pad * scale;
    assert!(
        (pad_device - pad_device.round()).abs() < 1e-5 && pad >= unaligned_pad,
        "the canvas pad {pad} (was {unaligned_pad}) is {pad_device} device pixels"
    );
    let _scope = crate::retained_present::RetainedScene::begin(true);
    let shift = row.canvas_shift() * scale;
    assert!(
        (shift - shift.round()).abs() < 1e-5,
        "the canvas shift is {shift} device pixels"
    );
    assert_eq!(row.scroll_x(), 120.5, "the gesture retains its precision");
}

/// While a presenter pans a strip drawn in canvas coordinates, a group
/// wholly off screen replays its last drawing instead of being drawn
/// again, every group the screen shows is drawn, and the strip's drawing
/// stays byte for byte the same while only the pan moves. Held still, the
/// strip draws every group; drawn in screen space again, the row drops
/// the drawings.
#[test]
fn a_panned_strip_replays_groups_off_screen_and_draws_those_on_it() {
    fn draw(row: &mut GroupRow, text: &mut TextRenderer) -> Scene {
        let mut scene = Scene::new();
        row.draw(&mut crate::draw::DrawCtx::new(
            &mut scene,
            Affine::IDENTITY,
            text,
            &Theme::dark(),
            crate::presence::PresenceCtx::empty().draw_env(),
        ));
        scene
    }
    let text = &mut TextRenderer::new();
    let mut row = strip_at(8, 800.0, 0.0, 1000);
    relayout_strip(&mut row, 800.0);
    let scope = crate::retained_present::RetainedScene::begin(true);
    let first = draw(&mut row, text);
    assert!(
        row.culled_groups.is_empty(),
        "the first canvas draw draws every group: {:?}",
        row.culled_groups
    );
    let mut replayed_any = false;
    for scroll in [37.0, 1300.0, 2900.0, 0.0] {
        row.set_scroll_x_for_test(scroll);
        relayout_strip(&mut row, 800.0);
        let panned = draw(&mut row, text);
        let shown = row.onscreen_group_ids();
        assert!(!shown.is_empty(), "the screen shows a group at {scroll}");
        assert!(
            shown.iter().all(|id| !row.culled_groups.contains(id)),
            "at {scroll} a group on screen ({shown:?}) was replayed: {:?}",
            row.culled_groups
        );
        replayed_any |= !row.culled_groups.is_empty();
        assert!(
            first.encoding_eq(&panned),
            "a pan to {scroll} leaves the strip's canvas drawing as it was"
        );
    }
    assert!(replayed_any, "groups off screen replay their drawings");
    assert!(!row.apart_drawings.is_empty());
    let held = draw(&mut row, text);
    assert!(
        row.culled_groups.is_empty(),
        "a strip at rest replays nothing: {:?}",
        row.culled_groups
    );
    assert!(first.encoding_eq(&held));
    drop(scope);
    let _ = draw(&mut row, text);
    assert!(
        row.apart_drawings.is_empty(),
        "a screen-space draw drops the canvas drawings"
    );
}

/// On a frame that only moved laid-out content, a strip drawn in canvas
/// coordinates shows the groups on screen at rest by replaying their
/// drawings, and the strip's drawing stays byte for byte the same. A
/// focused group is drawn. A drawing replayed off screen through a frame
/// that may have changed its group is drawn again before it is shown.
#[test]
fn a_frame_that_only_moved_replays_groups_on_screen_at_rest() {
    use crate::retained_present::{PanFrame, PanReplay, take_replayed};
    fn draw(row: &mut GroupRow, text: &mut TextRenderer, frame: PanFrame) -> Scene {
        thread_local! {
            static FRAMES: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
        }
        let number = FRAMES.with(|frames| {
            frames.set(frames.get().wrapping_add(1));
            frames.get()
        });
        let _frame = PanReplay::begin(frame, number);
        let mut scene = Scene::new();
        row.draw(&mut crate::draw::DrawCtx::new(
            &mut scene,
            Affine::IDENTITY,
            text,
            &Theme::dark(),
            crate::presence::PresenceCtx::empty().draw_env(),
        ));
        row.mark_drawn();
        scene
    }
    let text = &mut TextRenderer::new();
    let mut row = strip_at(8, 800.0, 0.0, 1000);
    relayout_strip(&mut row, 800.0);
    let _scope = crate::retained_present::RetainedScene::begin(true);
    let first = draw(&mut row, text, PanFrame::Drawn);
    take_replayed();
    row.set_scroll_x_for_test(37.0);
    relayout_strip(&mut row, 800.0);
    let moved = draw(&mut row, text, PanFrame::Moved);
    let shown = row.onscreen_group_ids();
    assert!(!shown.is_empty());
    assert!(
        shown.iter().all(|id| row.replayed_groups.contains(id)),
        "groups on screen at rest replay: shown {shown:?}, replayed {:?}",
        row.replayed_groups
    );
    assert_eq!(take_replayed(), shown.len());
    assert!(
        first.encoding_eq(&moved),
        "the replay draws what drawing would"
    );
    let focused = shown[0];
    let index = row.groups.iter().position(|g| g.id == focused).unwrap();
    let _ = row.groups[index].set_focused(true);
    let _ = draw(&mut row, text, PanFrame::Drawn);
    let _ = draw(&mut row, text, PanFrame::Moved);
    assert!(
        !row.replayed_groups.contains(&focused),
        "a focused group draws"
    );
    let _ = row.groups[index].set_focused(false);
    let _ = draw(&mut row, text, PanFrame::Drawn);
    row.set_scroll_x_for_test(2900.0);
    relayout_strip(&mut row, 800.0);
    let _ = draw(&mut row, text, PanFrame::Drawn);
    row.set_scroll_x_for_test(37.0);
    relayout_strip(&mut row, 800.0);
    take_replayed();
    let _ = draw(&mut row, text, PanFrame::Moved);
    assert!(
        shown.iter().all(|id| !row.replayed_groups.contains(id)),
        "drawings replayed through a changing frame are drawn again: {:?}",
        row.replayed_groups
    );
}

/// On frames that only pan a strip drawn in screen space, a window at
/// rest shows its last drawing moved by the pan instead of drawing again,
/// and the frame renders as one that drew every window does, in every
/// theme and at fractional pans. A keyboard-focused window draws; a
/// frame that is not only a pan draws every window and drops the
/// drawings, so the next pan frame replays nothing; a pan frame that
/// draws every window anyway keeps them.
#[test]
fn a_pan_frame_replays_windows_at_rest_and_renders_as_a_drawn_frame() {
    use crate::offscreen::OffscreenRenderer;
    use crate::retained_present::{PanFrame, PanReplay, take_replayed};
    fn draw(row: &mut GroupRow, text: &mut TextRenderer, theme: &Theme, replay: bool) -> Scene {
        draw_as(
            row,
            text,
            theme,
            if replay {
                PanFrame::Replayed
            } else {
                PanFrame::Drawn
            },
        )
    }
    fn draw_as(
        row: &mut GroupRow,
        text: &mut TextRenderer,
        theme: &Theme,
        frame: PanFrame,
    ) -> Scene {
        thread_local! {
            static FRAMES: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
        }
        let number = FRAMES.with(|frames| {
            frames.set(frames.get().wrapping_add(1));
            frames.get()
        });
        let _frame = PanReplay::begin(frame, number);
        let mut scene = Scene::new();
        let presence = crate::presence::PresenceCtx::empty();
        let previous = presence.push_viewport(Rect::new(0.0, 0.0, 800.0, 600.0));
        row.draw(&mut crate::draw::DrawCtx::new(
            &mut scene,
            Affine::IDENTITY,
            text,
            theme,
            presence.draw_env(),
        ));
        presence.pop_viewport(previous);
        row.mark_drawn();
        scene
    }
    let text = &mut TextRenderer::new();
    let mut renderer = OffscreenRenderer::new(800, 600);
    for theme in [Theme::light(), Theme::dark(), Theme::high_contrast()] {
        let mut row = strip_at(5, 800.0, 0.0, 400);
        relayout_strip(&mut row, 800.0);
        take_replayed();
        let _ = draw(&mut row, text, &theme, true);
        assert_eq!(take_replayed(), 0, "the first pan frame draws every window");
        for scroll in [37.0, 210.5, 333.25, 0.0] {
            row.set_scroll_x_for_test(scroll);
            relayout_strip(&mut row, 800.0);
            let replayed = draw(&mut row, text, &theme, true);
            let windows = take_replayed();
            assert!(windows > 0, "a pan to {scroll} replays the windows at rest");
            assert_eq!(windows, row.replayed_groups_for_test().len());
            // The same pan drawn by a frame that is not only a pan.
            let drawn = draw(&mut row, text, &theme, false);
            assert_eq!(
                take_replayed(),
                0,
                "a frame that is not only a pan draws every window"
            );
            let replayed = renderer.render(&replayed, vello::peniko::Color::BLACK);
            let drawn = renderer.render(&drawn, vello::peniko::Color::BLACK);
            assert_eq!(
                OffscreenRenderer::pixels_differing_beyond_aa(&replayed, &drawn),
                0,
                "a pan to {scroll} replayed renders as drawn"
            );
            let _ = draw(&mut row, text, &theme, true);
            assert_eq!(
                take_replayed(),
                0,
                "drawings do not outlive a frame that is not only a pan"
            );
        }
        let shown = row.onscreen_group_ids();
        let focused = shown[0];
        let index = row
            .groups
            .iter()
            .position(|group| group.id == focused)
            .unwrap();
        let _ = row.groups[index].set_focused(true);
        let _ = draw(&mut row, text, &theme, false);
        let _ = draw(&mut row, text, &theme, true);
        row.set_scroll_x_for_test(120.0);
        relayout_strip(&mut row, 800.0);
        let _ = draw(&mut row, text, &theme, true);
        assert!(take_replayed() > 0);
        assert!(
            !row.replayed_groups_for_test().contains(&focused),
            "the focused window draws: {:?}",
            row.replayed_groups_for_test()
        );
        let _ = draw_as(&mut row, text, &theme, PanFrame::Kept);
        assert_eq!(take_replayed(), 0, "a kept frame draws every window");
        row.set_scroll_x_for_test(140.0);
        relayout_strip(&mut row, 800.0);
        let _ = draw(&mut row, text, &theme, true);
        assert!(
            take_replayed() > 0,
            "drawings outlive a pan frame that drew every window"
        );
    }
}

/// Each window group of a strip drawn in canvas coordinates is a damage
/// part of the strip's drawing: drawn again, panned (groups off screen
/// replaying their drawings) or held still, the strip shows no damage,
/// and a change to one group damages only where that group can paint.
#[test]
fn each_group_of_a_strip_drawn_in_canvas_coordinates_is_a_damage_part() {
    fn draw(
        row: &mut GroupRow,
        text: &mut TextRenderer,
    ) -> (Scene, crate::scene_parts::SectionParts) {
        let mut scene = Scene::new();
        let ((), parts) = crate::scene_parts::record(&mut scene, |scene| {
            row.draw(&mut crate::draw::DrawCtx::new(
                scene,
                Affine::IDENTITY,
                text,
                &Theme::dark(),
                crate::presence::PresenceCtx::empty().draw_env(),
            ));
        });
        (scene, parts.expect("the strip's parts are well nested"))
    }
    let text = &mut TextRenderer::new();
    let mut row = strip_at(8, 800.0, 0.0, 1000);
    relayout_strip(&mut row, 800.0);
    let _scope = crate::retained_present::RetainedScene::begin(true);
    let (first, first_parts) = draw(&mut row, text);
    let (again, again_parts) = draw(&mut row, text);
    assert_eq!(
        crate::scene_parts::damage(&first, &first_parts, &again, &again_parts),
        Ok(Vec::new())
    );
    row.set_scroll_x_for_test(1300.0);
    relayout_strip(&mut row, 800.0);
    let (panned, panned_parts) = draw(&mut row, text);
    assert!(
        !row.culled_groups.is_empty(),
        "groups off screen replay their drawings"
    );
    assert_eq!(
        crate::scene_parts::damage(&first, &first_parts, &panned, &panned_parts),
        Ok(Vec::new()),
        "a replayed group marks its part again"
    );
    let index = 5;
    let focused = row.groups[index].focused();
    let _ = row.groups[index].set_focused(!focused);
    let (changed, changed_parts) = draw(&mut row, text);
    let damage = crate::scene_parts::damage(&panned, &panned_parts, &changed, &changed_parts)
        .expect("a group bounds its change");
    assert!(!damage.is_empty(), "the group's focus edge changed");
    let shift = row.canvas_shift();
    let group = row.groups[index]
        .get_rect()
        .inflate(GROUP_SPRITE_REACH_PX, 0.0);
    for rect in damage {
        assert!(
            rect.x0 >= group.x0 + shift - 1e-6 && rect.x1 <= group.x1 + shift + 1e-6,
            "{rect:?} lies where group {index} paints ({group:?} shifted by {shift})"
        );
    }
}

#[test]
fn external_pixels_are_told_apart_from_carried_ones() {
    use vello::peniko::{Blob, ImageAlphaType, ImageData, ImageFormat};
    let image = |data: Vec<u8>| ImageData {
        data: Blob::new(std::sync::Arc::new(data)),
        format: ImageFormat::Rgba8,
        alpha_type: ImageAlphaType::Alpha,
        width: 2,
        height: 2,
    };
    let drawn = |image: &ImageData| {
        let mut scene = Scene::new();
        scene.draw_image(image, Affine::IDENTITY);
        scene
    };
    assert!(draws_external_pixels(&drawn(&image(Vec::new()))));
    assert!(!draws_external_pixels(&drawn(&image(vec![0; 16]))));
    assert!(!draws_external_pixels(&Scene::new()));
}

fn set_strip_permille(row: &mut GroupRow, permille: u32) {
    for g in &mut row.groups {
        g.set_width(WindowWidth::Fraction(permille));
    }
}

fn relayout_via_new_taffy_tree(row: &mut GroupRow) -> (TaffyTree<()>, NodeId) {
    use taffy::AvailableSpace;
    let theme = Theme::dark();
    let mut new_taffy: TaffyTree<()> = TaffyTree::new();
    let mut new_node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
    let new_root_child = row.build_layout(&mut new_taffy, &theme, &mut new_node_map);
    let new_root = new_taffy.parent_node(
        Style {
            size: Size {
                width: Dimension::length(2000.0),
                height: Dimension::length(600.0),
            },
            ..Default::default()
        },
        &[new_root_child],
    );
    new_taffy
        .compute_layout_with_measure(
            new_root,
            taffy::Size {
                width: AvailableSpace::Definite(2000.0),
                height: AvailableSpace::Definite(600.0),
            },
            |_, _, _, _, _| taffy::Size::ZERO,
        )
        .unwrap();
    row.apply_layout(&new_taffy, new_root_child, Point::ORIGIN);
    (new_taffy, new_root_child)
}

tilo_test_env::scenario! {
    fn a_window_edge_resizes_at_its_border;

    phase the_border_pool_tracks_the_gaps;
    {
        /// Pins the `build_layout` border policy so a refactor can't
        /// silently leak or under-allocate them.
        fn handle_pool_matches_inter_group_gap_count() {
            let row = make_row_with_groups(&[&[1]]);
            assert_eq!(row.handle_count(), 0, "single-group row needs no border");
            let row = make_row_with_groups(&[&[1], &[2]]);
            assert_eq!(row.handle_count(), 1, "two-group row has exactly one");
            let row = make_row_with_groups(&[&[1], &[2], &[3]]);
            assert_eq!(row.handle_count(), 2, "three-group row has exactly two");
        }

        fn handle_pool_grows_on_split_into_new_group() {
            let mut row = make_row_with_groups(&[&[1, 2]]);
            assert_eq!(row.handle_count(), 0, "precondition: one group, no border");
            assert!(row.split_tab_into_new_group(0, 2, 1));
            assert_eq!(row.groups.len(), 2);
            assert!(
                row.take_structure_dirty(),
                "split must flag structure_dirty"
            );
            let (row, _taffy, _root) = relayout_row_owned(row, 2000.0);
            assert_eq!(row.handle_count(), 1, "after split: groups.len() - 1");
            let r = row.handle_at(0).expect("border 0").get_rect();
            assert!(r.width() > 0.0 && r.height() > 0.0, "laid out: {r:?}");
        }

        handle_pool_matches_inter_group_gap_count();
        handle_pool_grows_on_split_into_new_group();
    }

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
        use crate::widget::cursor::in_tree;
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
        let escape = crate::synthetic_input::named_key(winit::keyboard::NamedKey::Escape);
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
    crate::rect_geometry!();
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
    crate::rect_geometry!();
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

    phase hidden_row_retains_session_collaboration_delivery;
    {
        let first = crate::history_mode::isolation_tests::native_effect_probe_parts(
            "hidden-session-a",
            "fixture.hidden-session-a",
        );
        let second = crate::history_mode::isolation_tests::native_effect_probe_parts(
            "hidden-session-b",
            "fixture.hidden-session-b",
        );
        let first_events = std::rc::Rc::clone(&first.collab_events);
        let second_events = std::rc::Rc::clone(&second.collab_events);
        let first_group = WindowGroup::new(
            100,
            vec![(TabDescriptor::new(1, "First"), TileContent::App(first.root))],
            1,
            WindowWidth::Normal,
        );
        let second_group = WindowGroup::new(
            200,
            vec![(
                TabDescriptor::new(2, "Second"),
                TileContent::App(second.root),
            )],
            2,
            WindowWidth::Normal,
        );
        let mut row = GroupRow::new(vec![first_group, second_group]);
        row.set_fullscreen(None, true);

        crate::widget::handle_collab_event(
            &mut row,
            &::collaboration::CollabEvent::FollowChanged { peer_id: None },
        );
        assert_eq!(first_events.get(), 1);
        assert_eq!(second_events.get(), 1);
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

/// A maximized window covers its strip: nothing of a neighbour paints
/// over it. While its frame grows the neighbours fade out under it (they
/// never cut out in one frame), and at rest it paints alone. The fade back
/// in on restore keeps the row dirty until a frame drew it at rest, so
/// a parent's subtree cache cannot keep a partway pose on screen.
#[test]
fn maximized_window_paints_alone_and_the_sibling_fade_redraws_to_rest() {
    fn encoded(row: &mut GroupRow) -> Vec<u32> {
        let mut scene = Scene::new();
        row.draw(&mut crate::draw::DrawCtx::new(
            &mut scene,
            Affine::IDENTITY,
            &mut TextRenderer::new(),
            &Theme::dark(),
            crate::presence::PresenceCtx::empty().draw_env(),
        ));
        row.mark_drawn();
        scene.encoding().draw_tags.iter().map(|tag| tag.0).collect()
    }
    let (mut row, _t, _r) = make_row_with_groups_with_taffy(&[&[1], &[2], &[3]]);
    row.groups[1].maximized = true;
    let (mut row, ..) = relayout_row_owned_with(row, 2000.0, &Theme::dark());

    row.advance_sibling_fade(Duration::from_secs(1));
    let settled = encoded(&mut row);
    assert!(row.hold_sibling_fade_at(Duration::ZERO));
    let fading = encoded(&mut row);
    row.clear_sibling_fade_hold();
    row.advance_sibling_fade(Duration::from_secs(1));
    let settled_again = encoded(&mut row);
    assert_eq!(
        settled, settled_again,
        "the maximized window draws the same at rest"
    );
    assert!(
        fading.len() > settled.len(),
        "the neighbours fade out under the maximizing window, not cut"
    );
    assert!(
        fading.ends_with(&settled),
        "a neighbour fading out must not paint over the maximized window"
    );

    row.groups[1].maximized = false;
    let (mut row, ..) = relayout_row_owned_with(row, 2000.0, &Theme::dark());
    let _ = encoded(&mut row);
    assert!(
        row.sibling_fade_in_flight() && row.needs_animation_frame(),
        "the neighbours fading back in redraw the row every frame"
    );
    row.advance_sibling_fade(Duration::from_secs(1));
    assert!(!row.sibling_fade_in_flight());
    assert!(
        row.sibling_fade_needs_frame() && row.needs_animation_frame(),
        "the frame with the neighbours at rest is still owed"
    );
    let _ = encoded(&mut row);
    assert!(
        !row.sibling_fade_needs_frame(),
        "once drawn at rest the fade asks for nothing more"
    );
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
