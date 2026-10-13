//! Headless frame cost of the desktop's top bar.
//!
//! Lays the bar out at 1280 px and measures three things over many frames:
//! the first (cold) draw, a draw with nothing changed (the cached scene is
//! replayed), and a draw after the centre widget changes (the scene is
//! re-encoded). `update` on an unchanged chrome is measured too, since the
//! frame loop calls it every frame.
//!
//! Run with:
//! ```
//! cargo run -p tilo-desktop --release --example topbar_frame_cost
//! ```
//! Iterations override: `TILO_BENCH_ITERS=2000`.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::print_stdout,
    clippy::cast_precision_loss,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    reason = "a benchmark reports to stdout and treats a failed setup as fatal"
)]

use std::collections::HashMap;
use std::time::{Duration, Instant};

use automerge::AutoCommit;
use taffy::{NodeId, Size, Style, TaffyTree};
use tilo_desktop::topbar::{TOPBAR_HEIGHT_PX, TopBarChrome};
use tilo_desktop_model::{Collaborator, DesktopTree, Workspace, WorkspaceId};
use ui::presence::PresenceCtx;
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::widget::Widget;
use vello::Scene;
use vello::kurbo::{Affine, Point};

fn tree() -> DesktopTree {
    let mut workspace = Workspace::new(WorkspaceId(1), "Design review");
    workspace.collaborators = vec![
        Collaborator::new("ada", "Ada").with_session(7),
        Collaborator::new("lin", "Lin").with_session(9),
        Collaborator::new("sam", "Sam").with_session(11),
        Collaborator::new("kim", "Kim").with_session(13),
    ];
    DesktopTree {
        workspaces: vec![workspace, Workspace::new(WorkspaceId(2), "Home")],
        active: WorkspaceId(1),
        ..DesktopTree::default()
    }
}

fn lay_out(bar: &mut TopBarChrome, theme: &Theme, width: f32) {
    let mut taffy: TaffyTree<()> = TaffyTree::new();
    let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
    let node = bar.build_layout(&mut taffy, theme, &mut node_map);
    let viewport = taffy
        .new_with_children(
            Style {
                size: Size::from_lengths(width, TOPBAR_HEIGHT_PX),
                ..Default::default()
            },
            &[node],
        )
        .unwrap();
    taffy
        .compute_layout_with_measure(
            viewport,
            Size {
                width: taffy::AvailableSpace::Definite(width),
                height: taffy::AvailableSpace::Definite(TOPBAR_HEIGHT_PX),
            },
            |_, _, _, _, _| taffy::Size::ZERO,
        )
        .unwrap();
    bar.apply_layout(&taffy, node, Point::ORIGIN);
}

fn draw(bar: &mut TopBarChrome, text: &mut TextRenderer, theme: &Theme) -> Duration {
    let mut scene = Scene::new();
    let start = Instant::now();
    bar.draw(
        &mut scene,
        Affine::IDENTITY,
        text,
        theme,
        &PresenceCtx::default(),
    );
    start.elapsed()
}

fn report(name: &str, samples: &[Duration]) {
    let mut sorted = samples.to_vec();
    sorted.sort_unstable();
    let total: Duration = sorted.iter().sum();
    let mean = total.as_secs_f64() * 1e6 / sorted.len() as f64;
    let p50 = sorted[sorted.len() / 2].as_secs_f64() * 1e6;
    let p99 = sorted[sorted.len() * 99 / 100].as_secs_f64() * 1e6;
    println!("{name:<28} mean {mean:>9.2} µs   p50 {p50:>9.2} µs   p99 {p99:>9.2} µs");
}

fn main() {
    let iters: usize = std::env::var("TILO_BENCH_ITERS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(500);
    let theme = Theme::default();
    let mut text = TextRenderer::new();
    let tree = tree();

    let mut cold = Vec::with_capacity(iters);
    for _ in 0..iters.min(100) {
        let mut bar = TopBarChrome::from_tree(&tree);
        lay_out(&mut bar, &theme, 1280.0);
        cold.push(draw(&mut bar, &mut text, &theme));
    }

    let mut bar = TopBarChrome::from_tree(&tree);
    lay_out(&mut bar, &theme, 1280.0);
    draw(&mut bar, &mut text, &theme);
    let mut rest = Vec::with_capacity(iters);
    let mut updates = Vec::with_capacity(iters);
    let doc = AutoCommit::new();
    for _ in 0..iters {
        let start = Instant::now();
        bar.update(&doc, &automerge::ROOT);
        updates.push(start.elapsed());
        rest.push(draw(&mut bar, &mut text, &theme));
    }

    let mut relayout = Vec::with_capacity(iters);
    for i in 0..iters {
        // Alternating widths invalidate the cached scene every frame.
        let width = if i % 2 == 0 { 1280.0 } else { 1279.0 };
        lay_out(&mut bar, &theme, width);
        relayout.push(draw(&mut bar, &mut text, &theme));
    }

    println!("top bar, 1280 px, {iters} frames");
    report("cold draw", &cold);
    report("draw, nothing changed", &rest);
    report("draw after resize", &relayout);
    report("update, nothing changed", &updates);
}
