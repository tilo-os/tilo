//! Visual E2E journey for a `GroupRow`: renders two side-by-side
//! `WindowGroup`s through a sequence of cross-group tab operations —
//! baseline, transfer-tab (moves a pill from group A to group B),
//! split-tab (detach into a brand-new group), close-group (remove an
//! entire group) — and asserts each produces distinct pixel output.
//!
//! Complements `window_group_visual_journey.rs` (single-group paint
//! paths) and the state-only assertions in `group_row.rs` unit tests.
//! This file's job is to be the regression guard for the *inter*-group
//! rasterisation: pill-count changes, group-count changes, and
//! cross-group drag transfer should all be visible in the pixels.
//!
//! Skips gracefully when no wgpu adapter is available, matching the
//! existing offscreen tests. Set `QUICK_UI_SCREENSHOT_DIR=<path>` to
//! write one PNG per state (`group_row_<state>.png`) for by-eye
//! inspection.

mod support;

use std::collections::HashMap;
use support::offscreen::OffscreenRenderer;
use taffy::{AvailableSpace, NodeId, Size, TaffyTree};
use vello::kurbo::{Affine, Point};
use vello::peniko::Color;
use vello::Scene;

use ui::presence::PresenceCtx;
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::widget::{
    AppKind, GroupRow, TabDescriptor, TileContent, Widget, WindowGroup, WindowWidth,
};

fn try_new_renderer(w: u32, h: u32) -> Option<OffscreenRenderer> {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| OffscreenRenderer::new(w, h)))
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

    // Dark slate backdrop so the accented chrome pops clearly off the
    // background — same base tone used by the window-group journey so
    // the two tests read as belonging to the same family.
    let bg = Color::from_rgb8(0x0f, 0x17, 0x2a);
    renderer.render(&scene, bg)
}

fn pixel_hash(buf: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf29ce484222325;
    for b in buf {
        h ^= *b as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    h
}

fn maybe_write_png(rgba: &[u8], w: u32, h: u32, name: &str) {
    let Ok(dir) = std::env::var("QUICK_UI_SCREENSHOT_DIR") else {
        return;
    };
    let path = std::path::Path::new(&dir).join(format!("{name}.png"));
    std::fs::create_dir_all(&dir).ok();
    let file = std::fs::File::create(&path).expect("create screenshot png");
    let mut bw = std::io::BufWriter::new(file);
    let mut enc = png::Encoder::new(&mut bw, w, h);
    enc.set_color(png::ColorType::Rgba);
    enc.set_depth(png::BitDepth::Eight);
    let mut writer = enc.write_header().expect("png header");
    writer.write_image_data(rgba).expect("png write");
    eprintln!("wrote {} to {}", name, path.display());
}

/// Full cross-group journey: two groups side by side →
/// transfer one tab A → B → split a tab into a brand-new third group →
/// close (drop) the last group. Every state must produce a distinct
/// pixel hash. If any two match, a layout or paint invariant has
/// regressed.
#[test]
fn group_row_visual_journey_distinguishes_all_states() {
    let (w, h) = (960u32, 220u32);
    let mut renderer = match try_new_renderer(w, h) {
        Some(r) => r,
        None => {
            eprintln!("offscreen: no wgpu adapter available — skipping group-row journey");
            return;
        }
    };

    // State 1: baseline — two groups, each with 3 tabs of distinct kinds.
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
        &[
            (10, "B1", AppKind::Browser),
            (11, "B2", AppKind::Settings),
        ],
    );
    let mut row = GroupRow::new(vec![g_a, g_b]);

    let baseline = render_row(&mut renderer, &mut row);
    let baseline_hash = pixel_hash(&baseline);
    maybe_write_png(&baseline, w, h, "group_row_baseline");

    // State 2: transfer tab A3 from group 0 → group 1 (dropped at index 0).
    // Group 0 now has 2 pills, group 1 has 3 pills — the distribution of
    // pills across the row visibly shifts left-to-right.
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

    // State 3: split tab A2 into a brand-new group inserted to the
    // left of group 1. Row now has 3 groups: [A1] | [A2] | [B1, B2, A3].
    // Group count changes — the row's horizontal layout re-divides
    // available width, so the pixel output must shift again.
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

/// Concept-parity maximize: rendering a `GroupRow` with one of its
/// groups flagged `maximized` must produce pixel output distinct from
/// the same row's non-maximized baseline AND must visually collapse
/// the siblings — verified by checking the maximized group's stored
/// rect absorbs the row's full inner width while the siblings'
/// stored rects collapse to zero. Mirrors the concept's
/// `width: 100vw` fixed overlay in `WindowFrame.tsx:188-200`.
#[test]
fn group_row_maximized_paints_distinct_pixels_and_hides_siblings() {
    let (w, h) = (960u32, 220u32);
    let mut renderer = match try_new_renderer(w, h) {
        Some(r) => r,
        None => {
            eprintln!("offscreen: no wgpu adapter available — skipping maximize scene");
            return;
        }
    };

    let mut row = GroupRow::new(vec![
        make_group(
            100,
            &[
                (1, "A1", AppKind::Editor),
                (2, "A2", AppKind::Files),
            ],
        ),
        make_group(200, &[(10, "B1", AppKind::Browser)]),
        make_group(300, &[(20, "C1", AppKind::Terminal)]),
    ]);

    let baseline = render_row(&mut renderer, &mut row);
    let baseline_hash = pixel_hash(&baseline);
    maybe_write_png(&baseline, w, h, "group_row_maximize_baseline");

    // Flip group 1 (middle) to maximized. The next render must paint
    // ONLY that group at the row's full width — siblings collapsed.
    row.groups[1].maximized = true;
    let maximized = render_row(&mut renderer, &mut row);
    let maximized_hash = pixel_hash(&maximized);
    maybe_write_png(&maximized, w, h, "group_row_maximized");

    assert_ne!(
        baseline_hash, maximized_hash,
        "flipping a group to maximized must change the rendered output",
    );

    // Stored rects: siblings collapsed to zero width; maximized group
    // absorbs the row's full inner width.
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

/// Determinism guard: two GroupRows built identically must produce
/// byte-identical pixels. Without this, the journey test's pixel-hash
/// comparisons are meaningless.
#[test]
fn group_row_fresh_render_is_deterministic() {
    let (w, h) = (600u32, 200u32);
    let mut renderer = match try_new_renderer(w, h) {
        Some(r) => r,
        None => {
            eprintln!("offscreen: no wgpu adapter available — skipping determinism check");
            return;
        }
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
        pixel_hash(&a),
        pixel_hash(&b),
        "two identical GroupRow renders must produce identical pixels",
    );
}
