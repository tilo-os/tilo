#![allow(
    clippy::exit,
    reason = "migration stack lint debt is scoped to this target"
)]

//! Offscreen shots for the `DataTable` primitive's built-in draggable
//! scrollbar.
//!
//! A 40-row table is put into internal-virtualization mode (a 300-px body
//! viewport over ~960 px of rows) so the built-in vertical scrollbar is
//! live, then two frames are rendered:
//!   1. `data_table-top.png`    — `scroll_y = 0`: the thumb sits at the top.
//!   2. `data_table-bottom.png` — `scroll_y = max`: the thumb sits at the
//!      bottom, proving it tracks the offset.
//!
//! The example also *asserts* the thumb rect matches the offset (top thumb
//! at the track top, bottom thumb at the track bottom) so it doubles as a
//! self-checking geometry probe — a green run means "the thumb is where the
//! offset says".
//!
//! Run:  `cargo run -p ui-examples --example ui-shots -- data_table`
//! Out:  `$DATA_TABLE_SHOTS_OUT` (default `/tmp/data_table_shots`)

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use taffy::{AvailableSpace, Dimension, NodeId, Size, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use vello::peniko::{BlendMode, Fill};

use tilo_ui::offscreen::OffscreenRenderer;
use tilo_ui::presence::PresenceCtx;
use tilo_ui::text_render::TextRenderer;
use tilo_ui::theme::Theme;
use tilo_ui::widget::Widget;
use tilo_ui::widget::data_table::{ColumnDef, ColumnWidth, DataTable};

#[derive(Clone)]
struct Proc {
    name: String,
    cpu: String,
    mem: String,
}

fn columns() -> Vec<ColumnDef<Proc>> {
    vec![
        ColumnDef::new("name", "Process", ColumnWidth::Flex(1.0))
            .sortable(true)
            .with_text_render(Box::new(|r: &Proc, scene, tr, rect, theme, _sel| {
                tr.draw_text(
                    scene,
                    &r.name,
                    14.0,
                    theme.text_primary,
                    Affine::translate((rect.x0 + 8.0, rect.y0 + 4.0)),
                    None,
                );
            })),
        ColumnDef::new("cpu", "CPU%", ColumnWidth::Fixed(90.0)).with_text_render(Box::new(
            |r: &Proc, scene, tr, rect, theme, _sel| {
                tr.draw_text(
                    scene,
                    &r.cpu,
                    14.0,
                    theme.text_dim,
                    Affine::translate((rect.x0 + 8.0, rect.y0 + 4.0)),
                    None,
                );
            },
        )),
        ColumnDef::new("mem", "Mem", ColumnWidth::Fixed(90.0)).with_text_render(Box::new(
            |r: &Proc, scene, tr, rect, theme, _sel| {
                tr.draw_text(
                    scene,
                    &r.mem,
                    14.0,
                    theme.text_dim,
                    Affine::translate((rect.x0 + 8.0, rect.y0 + 4.0)),
                    None,
                );
            },
        )),
    ]
}

fn rows() -> Vec<Proc> {
    (0..40usize)
        .map(|i| Proc {
            name: format!("service-daemon-{i:02}"),
            cpu: format!("{}.{}%", i % 9, i % 10),
            mem: format!("{} MB", 40usize.saturating_add(i.saturating_mul(7))),
        })
        .collect()
}

const WIDTH: u32 = 520;
const BODY_VP: f32 = 300.0;
const ROW_H: f32 = 24.0;
const HEADER_H: f32 = 28.0;

/// Lay the table out full-height inside a fixed-width root, then switch it
/// into internal-virtualization mode with a `BODY_VP` body viewport.
fn build() -> (DataTable<Proc>, TaffyTree<()>, NodeId) {
    let mut table = DataTable::new(columns());
    table.set_rows(rows());

    let theme = shot_theme();
    let mut taffy = TaffyTree::<()>::new();
    let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
    let mut tr = TextRenderer::new();
    let tnode = table.build_layout(&mut taffy, &theme, &mut node_map);
    // Row count is a small fixed fixture size (40), far below f32's
    // exact-integer range; WIDTH is a small fixed pixel constant (520).
    #[allow(
        clippy::cast_precision_loss,
        reason = "row count and WIDTH are small fixed fixture/pixel constants, far below f32's exact-integer range"
    )]
    let (rows_len_f32, width_f32) = (rows().len() as f32, WIDTH as f32);
    let full_h = HEADER_H + ROW_H * rows_len_f32;
    let root = taffy
        .new_with_children(
            Style {
                display: taffy::Display::Flex,
                flex_direction: taffy::FlexDirection::Column,
                size: Size {
                    width: Dimension::length(width_f32),
                    height: Dimension::length(full_h),
                },
                ..Default::default()
            },
            &[tnode],
        )
        .unwrap_or(tnode);
    let _ = taffy.compute_layout_with_measure(
        root,
        Size {
            width: AvailableSpace::Definite(width_f32),
            height: AvailableSpace::Definite(full_h),
        },
        |known, available, node_id, _, _| {
            if let Some(&ptr) = node_map.get(&node_id) {
                unsafe { (*ptr).measure(known, available, &mut tr) }
            } else {
                Size::ZERO
            }
        },
    );
    table.apply_layout(&taffy, tnode, Point::ZERO);
    table.set_viewport(BODY_VP, ROW_H);
    (table, taffy, tnode)
}

fn shoot(table: &mut DataTable<Proc>, scroll_y: f32, out: &Path) -> Option<Rect> {
    table.set_scroll_y(scroll_y);
    let theme = shot_theme();
    let mut tr = TextRenderer::new();
    let presence = PresenceCtx::default();

    // The visible region is the header + body viewport; the renderer canvas
    // clips everything below it, which is exactly the virtualized viewport.
    // HEADER_H/BODY_VP are small fixed positive pixel constants, so the
    // rounded sum always fits u32 losslessly.
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "HEADER_H + BODY_VP is a small fixed positive pixel constant"
    )]
    let h = (HEADER_H + BODY_VP).ceil() as u32;
    let clip = Rect::new(0.0, 0.0, f64::from(WIDTH), f64::from(h));

    let mut scene = Scene::new();
    scene.push_layer(
        Fill::NonZero,
        BlendMode::default(),
        1.0,
        Affine::IDENTITY,
        &clip,
    );
    table.draw(&mut scene, Affine::IDENTITY, &mut tr, &theme, &presence);
    scene.pop_layer();

    let mut r = OffscreenRenderer::new(WIDTH, h);
    r.render_png(&scene, theme.bg_app, out);
    println!("wrote {}", out.display());
    table.scrollbar_thumb()
}

pub fn run(_args: &[String]) {
    let dir = std::env::var("DATA_TABLE_SHOTS_OUT")
        .unwrap_or_else(|_| "/tmp/data_table_shots".to_string());
    if let Err(e) = std::fs::create_dir_all(&dir) {
        eprintln!("data_table: failed to create output dir {dir}: {e}");
        return;
    }
    let dir = PathBuf::from(dir);

    let (mut table, _taffy, _node) = build();
    let max = table.max_scroll_y();
    println!("max_scroll_y = {max}");

    let Some(top) = shoot(&mut table, 0.0, &dir.join("data_table-top.png")) else {
        eprintln!("data_table: table has no scrollbar thumb in internal-scroll mode");
        return;
    };
    let Some(bottom) = shoot(&mut table, max, &dir.join("data_table-bottom.png")) else {
        eprintln!("data_table: table has no scrollbar thumb in internal-scroll mode");
        return;
    };

    // The thumb must reflect the offset: at 0 it sits at the track top; at
    // max it sits at the bottom (and strictly lower than at the top).
    println!("thumb @top    = y[{:.1}..{:.1}]", top.y0, top.y1);
    println!("thumb @bottom = y[{:.1}..{:.1}]", bottom.y0, bottom.y1);
    assert!(
        bottom.y0 > top.y0,
        "thumb must move DOWN as scroll_y grows: top.y0={:.1}, bottom.y0={:.1}",
        top.y0,
        bottom.y0,
    );
    let track_top = f64::from(HEADER_H) + 2.0; // rect.y0(0) + HEADER + BAR_MARGIN
    let track_bottom = f64::from(HEADER_H) + f64::from(BODY_VP) - 2.0;
    assert!(
        (top.y0 - track_top).abs() < 1.0,
        "top thumb at track top: {}",
        top.y0
    );
    assert!(
        (bottom.y1 - track_bottom).abs() < 1.0,
        "bottom thumb at track bottom: {}",
        bottom.y1,
    );
    println!("OK: thumb tracks the offset");
}

/// `TILO_SHOTS_THEME` picks the palette (`dark` by default) and
/// `TILO_SHOTS_TEXT_SCALE` the text scale — the switches every app's `--shots`
/// honours through `tilo_ui::launch::shot_theme`, whose `LaunchArgs` an example
/// cannot construct. An unknown theme name is an error, never a silent dark
/// shot: a capture that cannot observe its subject must fail.
fn shot_theme() -> Theme {
    tilo_ui::launch::shot_theme_from_env().unwrap_or_else(|error| {
        eprintln!("ui-shots: {error}");
        std::process::exit(2)
    })
}
