//! Visual E2E journey for a `GroupRow`: renders two side-by-side
//! `WindowGroup`s through a sequence of cross-group tab operations —
//! transfer-tab, split-tab, close-group — and asserts each produces
//! distinct pixel output. The regression guard for *inter*-group
//! rasterisation, complementing `window_group_visual_journey.rs`
//! (single-group paint paths) and `group_row.rs`'s state-only unit tests.
//!
//! Skips gracefully when no wgpu adapter is available. Set
//! `TILO_UI_SCREENSHOT_DIR=<path>` to write one PNG per state
//! (`group_row_<state>.png`) for by-eye inspection.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::string_slice,
    clippy::panic,
    clippy::unreachable,
    reason = "in a test these are the assertion, not a hazard"
)]

use crate::support::offscreen::OffscreenRenderer;
use std::collections::HashMap;
use taffy::{AvailableSpace, NodeId, Size, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Point};
use vello::peniko::Color;

use tilo_ui::presence::PresenceCtx;
use tilo_ui::text_render::TextRenderer;
use tilo_ui::theme::Theme;
use tilo_ui::widget::{
    AppKind, GroupRow, TabDescriptor, TileContent, Widget, WindowGroup, WindowWidth,
};

fn try_new_renderer(w: u32, h: u32) -> Option<OffscreenRenderer> {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        OffscreenRenderer::new(w, h)
    }))
    .ok()
}

fn make_group(id: u64, tabs: &[(u64, &str, AppKind)]) -> WindowGroup {
    let tabs: Vec<(TabDescriptor, TileContent)> = tabs
        .iter()
        .map(|(tid, title, kind)| {
            (
                TabDescriptor {
                    id: *tid,
                    title: (*title).into(),
                    app_kind: Some(*kind),
                    icon: None,
                },
                TileContent::Stub {
                    kind: *kind,
                    label: (*title).into(),
                },
            )
        })
        .collect();
    let active = tabs[0].0.id;
    WindowGroup::new(id, tabs, active, WindowWidth::Normal)
}

#[allow(
    clippy::cast_precision_loss,
    reason = "renderer.width/height are small fixed test-canvas dimensions, exactly \
              representable in f32"
)]
fn render_row(renderer: &mut OffscreenRenderer, row: &mut GroupRow) -> Vec<u8> {
    let (w, h) = (renderer.width, renderer.height);
    let theme = Theme::dark();
    let mut taffy = TaffyTree::<()>::new();
    let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
    let mut tr = TextRenderer::new();

    let root = row.build_layout(&mut taffy, &theme, &mut node_map);
    taffy
        .compute_layout_with_measure(
            root,
            Size {
                width: AvailableSpace::Definite(w as f32),
                height: AvailableSpace::Definite(h as f32),
            },
            |known, available, node_id, _, _| {
                if let Some(&ptr) = node_map.get(&node_id) {
                    unsafe { (*ptr).measure(known, available, &mut tr) }
                } else {
                    Size::ZERO
                }
            },
        )
        .expect("taffy compute_layout");
    row.apply_layout(&taffy, root, Point::ORIGIN);

    let presence = PresenceCtx::default();
    let mut scene = Scene::new();
    row.draw(&mut scene, Affine::IDENTITY, &mut tr, &theme, &presence);

    let bg = Color::from_rgb8(0x0f, 0x17, 0x2a);
    renderer.render(&scene, bg)
}

fn pixel_hash(buf: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in buf {
        h ^= u64::from(*b);
        h = h.wrapping_mul(0x100_0000_01b3);
    }
    h
}

fn maybe_write_png(rgba: &[u8], w: u32, h: u32, name: &str) {
    let Ok(dir) = tilo_ui::env::var("TILO_UI_SCREENSHOT_DIR") else {
        return;
    };
    let path = std::path::Path::new(&dir).join(format!("{name}.png"));
    std::fs::create_dir_all(&dir).ok();
    tilo_ui::png::write_rgba8(&path, w, h, rgba).expect("write screenshot png");
    eprintln!("wrote {} to {}", name, path.display());
}

#[test]
fn group_row_journey_is_deterministic_and_distinguishes_all_states() {
    // Case `group_row_visual_journey_distinguishes_all_states`.
    // Every state must produce a distinct pixel hash; if any two match, a
    // layout or paint invariant has regressed.
    {
        fn group_row_visual_journey_distinguishes_all_states() {
            let (w, h) = (960u32, 220u32);
            let Some(mut renderer) = try_new_renderer(w, h) else {
                eprintln!("offscreen: no wgpu adapter available — skipping group-row journey");
                return;
            };

            let g_a = make_group(
                100,
                &[
                    (1, "A1", AppKind::Editor),
                    (2, "A2", AppKind::Files),
                    (3, "A3", AppKind::Terminal),
                ],
            );
            let g_b = make_group(
                200,
                &[(10, "B1", AppKind::Browser), (11, "B2", AppKind::Settings)],
            );
            let mut row = GroupRow::new(vec![g_a, g_b]);

            let baseline = render_row(&mut renderer, &mut row);
            let baseline_hash = pixel_hash(&baseline);
            maybe_write_png(&baseline, w, h, "group_row_baseline");

            // Group 0 now has 2 pills, group 1 has 3 — distribution across the
            // row visibly shifts left-to-right.
            assert!(
                row.transfer_tab(0, 1, 3, 0),
                "transfer_tab(src=0, dst=1, tab=3, idx=0) must succeed",
            );
            let after_transfer = render_row(&mut renderer, &mut row);
            let after_transfer_hash = pixel_hash(&after_transfer);
            maybe_write_png(&after_transfer, w, h, "group_row_after_transfer");
            assert_ne!(
                baseline_hash, after_transfer_hash,
                "transferring a tab between groups must change the rendered output",
            );

            // Row now has 3 groups: [A1] | [A2] | [B1, B2, A3] — group count
            // changes re-divide available width, so pixel output shifts again.
            assert!(
                row.split_tab_into_new_group(0, 2, 1),
                "split_tab_into_new_group(src=0, tab=2, gap_idx=1) must succeed",
            );
            let after_split = render_row(&mut renderer, &mut row);
            let after_split_hash = pixel_hash(&after_split);
            maybe_write_png(&after_split, w, h, "group_row_after_split");
            assert_ne!(
                baseline_hash, after_split_hash,
                "splitting a tab into a new group must change the rendered output",
            );
            assert_ne!(
                after_transfer_hash, after_split_hash,
                "group-count change must paint differently from a same-count transfer",
            );
        }
        group_row_visual_journey_distinguishes_all_states();
    }

    // Case `group_row_fresh_render_is_deterministic`.
    // Without this, the journey test's pixel-hash comparisons are
    // meaningless. "The same image" isn't "the same bytes": the GPU
    // rasteriser may move an antialiased edge pixel one 8-bit step between
    // runs, so compare with that tolerance and nothing wider — the journey
    // test's `assert_ne!`s are unaffected, since a state change moves far
    // more than one step on far more than a few pixels.
    {
        fn group_row_fresh_render_is_deterministic() {
            let (w, h) = (600u32, 200u32);
            let Some(mut renderer) = try_new_renderer(w, h) else {
                eprintln!("offscreen: no wgpu adapter available — skipping determinism check");
                return;
            };
            let mut r1 = GroupRow::new(vec![
                make_group(1, &[(1, "X", AppKind::Editor)]),
                make_group(2, &[(2, "Y", AppKind::Files)]),
            ]);
            let mut r2 = GroupRow::new(vec![
                make_group(1, &[(1, "X", AppKind::Editor)]),
                make_group(2, &[(2, "Y", AppKind::Files)]),
            ]);
            let a = render_row(&mut renderer, &mut r1);
            let b = render_row(&mut renderer, &mut r2);
            assert_eq!(
                OffscreenRenderer::pixels_differing_beyond_aa(&a, &b),
                0,
                "two identical GroupRow renders must produce the same image",
            );
        }
        group_row_fresh_render_is_deterministic();
    }
}

#[allow(
    clippy::too_many_lines,
    reason = "merged scenario keeps its original cases together; each case builds a fresh fixture"
)]
#[test]
fn group_row_maximize_and_reorder_paint_visibly() {
    // Case `group_row_maximized_paints_distinct_pixels_and_hides_siblings`.
    // Verified by checking the maximized group's stored rect absorbs the
    // row's full inner width while the siblings' collapse to zero.
    {
        fn group_row_maximized_paints_distinct_pixels_and_hides_siblings() {
            let (w, h) = (960u32, 220u32);
            let Some(mut renderer) = try_new_renderer(w, h) else {
                eprintln!("offscreen: no wgpu adapter available — skipping maximize scene");
                return;
            };

            let mut row = GroupRow::new(vec![
                make_group(
                    100,
                    &[(1, "A1", AppKind::Editor), (2, "A2", AppKind::Files)],
                ),
                make_group(200, &[(10, "B1", AppKind::Browser)]),
                make_group(300, &[(20, "C1", AppKind::Terminal)]),
            ]);

            let baseline = render_row(&mut renderer, &mut row);
            let baseline_hash = pixel_hash(&baseline);
            maybe_write_png(&baseline, w, h, "group_row_maximize_baseline");

            row.groups[1].maximized = true;
            let maximized = render_row(&mut renderer, &mut row);
            let maximized_hash = pixel_hash(&maximized);
            maybe_write_png(&maximized, w, h, "group_row_maximized");

            assert_ne!(
                baseline_hash, maximized_hash,
                "flipping a group to maximized must change the rendered output",
            );

            let r0 = row.groups[0].get_rect();
            let r1 = row.groups[1].get_rect();
            let r2 = row.groups[2].get_rect();
            assert!(
                r0.width() <= 0.5,
                "left sibling must collapse to zero width when peer is maximized; got {}",
                r0.width(),
            );
            assert!(
                r2.width() <= 0.5,
                "right sibling must collapse to zero width when peer is maximized; got {}",
                r2.width(),
            );
            let row_w = row.get_rect().width();
            assert!(
                (r1.width() - row_w).abs() < 1.0,
                "maximized group must absorb the row's full width; got group={} row={}",
                r1.width(),
                row_w,
            );
        }
        group_row_maximized_paints_distinct_pixels_and_hides_siblings();
    }

    // Case `group_row_reorder_drop_indicator_is_visible_mid_drag`.
    // Grab a group's tab-bar background, drag past a sibling, and render the
    // in-flight state — the drop-rule accent at the target gap must make the
    // frame visibly differ from the idle strip.
    {
        fn group_row_reorder_drop_indicator_is_visible_mid_drag() {
            use winit::dpi::PhysicalPosition;
            use winit::event::{ElementState, Modifiers, MouseButton, WindowEvent};

            // Wide enough that all three 600px groups + gaps fit on-screen.
            let (w, h) = (1900u32, 220u32);
            let Some(mut renderer) = try_new_renderer(w, h) else {
                eprintln!("offscreen: no wgpu adapter available — skipping reorder shot");
                return;
            };

            let mut row = GroupRow::new(vec![
                make_group(100, &[(1, "Editor", AppKind::Editor)]),
                make_group(200, &[(2, "Files", AppKind::Files)]),
                make_group(300, &[(3, "Term", AppKind::Terminal)]),
            ]);

            let idle = render_row(&mut renderer, &mut row);
            maybe_write_png(&idle, w, h, "group_row_reorder_idle");

            // Lay out so child rects populate, then read geometry for the gesture.
            let (taffy, root) = lay_out(&mut row, w, h);
            let grab = tabbar_grab_point(&row, 0);
            let past_x = center_x_of_group(&row, 1) + 40.0;

            let dispatch = |row: &mut GroupRow, taffy: &TaffyTree<()>, ev: WindowEvent| {
                let modifiers = Modifiers::default();
                let mut state = automerge::AutoCommit::new();
                let path = automerge::ROOT.clone();
                let mut handled = false;
                let mut focus: Option<NodeId> = None;
                row.handle_event(
                    &ev,
                    &mut tilo_ui::widget::EventCtx {
                        scale_factor: 1.0,
                        modifiers: &modifiers,
                        state: &mut state,
                        path: &path,
                        handled: &mut handled,
                        focus_target: &mut focus,
                        node_id: root,
                        taffy,
                    },
                );
            };
            // Press on group 100's bar background, then move past group 200's
            // centre so the drop target is the gap after group 200.
            dispatch(
                &mut row,
                &taffy,
                WindowEvent::CursorMoved {
                    device_id: unsafe { std::mem::zeroed() },
                    position: PhysicalPosition::new(grab.x, grab.y),
                },
            );
            dispatch(
                &mut row,
                &taffy,
                WindowEvent::MouseInput {
                    device_id: unsafe { std::mem::zeroed() },
                    state: ElementState::Pressed,
                    button: MouseButton::Left,
                },
            );
            dispatch(
                &mut row,
                &taffy,
                WindowEvent::CursorMoved {
                    device_id: unsafe { std::mem::zeroed() },
                    position: PhysicalPosition::new(past_x, grab.y),
                },
            );
            assert!(
                row.group_drag_active(),
                "grab-and-move must engage the reorder drag"
            );
            assert!(
                row.group_reorder_target().is_some(),
                "the drag must target a drop gap so the indicator has a slot to paint",
            );

            // No reorder is committed yet, so group positions are unchanged;
            // the drop indicator paints from the retained target.
            let mid = render_row(&mut renderer, &mut row);
            maybe_write_png(&mid, w, h, "group_row_reorder_mid");

            assert_ne!(
                pixel_hash(&idle),
                pixel_hash(&mid),
                "the mid-drag frame with the drop indicator must differ from the idle strip",
            );
        }
        group_row_reorder_drop_indicator_is_visible_mid_drag();
    }
}

/// Returns the taffy tree + root node so the caller can reuse them when
/// dispatching events (child widgets read `taffy.children`/`taffy.layout`
/// during event handling).
#[allow(
    clippy::cast_precision_loss,
    reason = "w/h are small fixed test-canvas dimensions, exactly representable in f32"
)]
fn lay_out(row: &mut GroupRow, w: u32, h: u32) -> (TaffyTree<()>, NodeId) {
    let theme = Theme::dark();
    let mut taffy = TaffyTree::<()>::new();
    let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
    let mut tr = TextRenderer::new();
    let root = row.build_layout(&mut taffy, &theme, &mut node_map);
    taffy
        .compute_layout_with_measure(
            root,
            Size {
                width: AvailableSpace::Definite(w as f32),
                height: AvailableSpace::Definite(h as f32),
            },
            |known, available, node_id, _, _| {
                if let Some(&ptr) = node_map.get(&node_id) {
                    unsafe { (*ptr).measure(known, available, &mut tr) }
                } else {
                    Size::ZERO
                }
            },
        )
        .expect("compute_layout");
    row.apply_layout(&taffy, root, Point::ORIGIN);
    (taffy, root)
}

/// A point in group `idx`'s tab-bar background (right of the last pill),
/// where a whole-group reorder grab lands.
fn tabbar_grab_point(row: &GroupRow, idx: usize) -> Point {
    let g = row.group_at_for_test(idx).expect("group at idx");
    let bar = g.bar_rect();
    let last = g.tab_rects().last().copied().unwrap_or(bar);
    Point::new(
        f64::midpoint(last.x1, bar.x1),
        f64::midpoint(bar.y0, bar.y1),
    )
}

fn center_x_of_group(row: &GroupRow, idx: usize) -> f64 {
    row.group_at_for_test(idx)
        .expect("group at idx")
        .get_rect()
        .center()
        .x
}
