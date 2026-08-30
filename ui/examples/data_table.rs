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
//! Run:  cargo run -p ui --example ui-shots -- data_table
//! Out:  $DATA_TABLE_SHOTS_OUT (default /tmp/data_table_shots)

use std::collections::HashMap;
use std::path::PathBuf;

use taffy::{AvailableSpace, Dimension, NodeId, Size, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use vello::peniko::{BlendMode, Fill};

use ui::offscreen::OffscreenRenderer;
use ui::presence::PresenceCtx;
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::widget::Widget;
use ui::widget::data_table::{ColumnDef, ColumnWidth, DataTable};

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
    (0..40)
        .map(|i| Proc {
            name: format!("service-daemon-{i:02}"),
            cpu: format!("{}.{}%", i % 9, i % 10),
            mem: format!("{} MB", 40 + i * 7),
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

    let theme = Theme::dark();
    let mut taffy = TaffyTree::<()>::new();
    let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
    let mut tr = TextRenderer::new();
    let tnode = table.build_layout(&mut taffy, &theme, &mut node_map);
    let full_h = HEADER_H + ROW_H * rows().len() as f32;
    let root = taffy
        .new_with_children(
            Style {
                display: taffy::Display::Flex,
                flex_direction: taffy::FlexDirection::Column,
                size: Size {
                    width: Dimension::length(WIDTH as f32),
                    height: Dimension::length(full_h),
                },
                ..Default::default()
            },
            &[tnode],
        )
        .unwrap();
    taffy
        .compute_layout_with_measure(
            root,
            Size {
                width: AvailableSpace::Definite(WIDTH as f32),
                height: AvailableSpace::Definite(full_h),
            },
            |known, available, node_id, _, _| {
                if let Some(&ptr) = node_map.get(&node_id) {
                    unsafe { (*ptr).measure(known, available, &mut tr) }
                } else {
                    Size::ZERO
                }
            },
        )
        .unwrap();
    table.apply_layout(&taffy, tnode, Point::ZERO);
    table.set_viewport(BODY_VP, ROW_H);
    (table, taffy, tnode)
}

fn shoot(table: &mut DataTable<Proc>, scroll_y: f32, out: PathBuf) -> Rect {
    table.set_scroll_y(scroll_y);
    let theme = Theme::dark();
    let mut tr = TextRenderer::new();
    let presence = PresenceCtx::default();

    // The visible region is the header + body viewport; the renderer canvas
    // clips everything below it, which is exactly the virtualized viewport.
    let h = (HEADER_H + BODY_VP).ceil() as u32;
    let clip = Rect::new(0.0, 0.0, WIDTH as f64, h as f64);

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
    r.render_png(&scene, theme.bg_app, &out);
    println!("wrote {}", out.display());
    table
        .scrollbar_thumb()
        .expect("bar live in internal-scroll mode")
}

pub fn run(_args: &[String]) {
    let dir = std::env::var("DATA_TABLE_SHOTS_OUT")
        .unwrap_or_else(|_| "/tmp/data_table_shots".to_string());
    std::fs::create_dir_all(&dir).unwrap();
    let dir = PathBuf::from(dir);

    let (mut table, _taffy, _node) = build();
    let max = table.max_scroll_y();
    println!("max_scroll_y = {max}");

    let top = shoot(&mut table, 0.0, dir.join("data_table-top.png"));
    let bottom = shoot(&mut table, max, dir.join("data_table-bottom.png"));

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
    let track_top = HEADER_H as f64 + 2.0; // rect.y0(0) + HEADER + BAR_MARGIN
    let track_bottom = HEADER_H as f64 + BODY_VP as f64 - 2.0;
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
