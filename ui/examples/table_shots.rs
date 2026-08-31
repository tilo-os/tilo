//! Offscreen shots for the editable [`ui::widget::table::Table`].
//!
//! Renders four frames through the widget's real five-pass pipeline and its
//! real event handler (no private-field pokes):
//!   1. `table-base.png`          — a seeded 3-column table with a header band
//!      and the always-on insert ticks in the
//!      gutters.
//!   2. `table-insert-col.png`    — hovering a *middle* column boundary shows
//!      the accent insert pill + full-height guide.
//!   3. `table-delete-row.png`    — hovering a row's delete handle tints the
//!      row danger-red and shows the "×" handle.
//!   4. `table-after-insert.png`  — after clicking that middle boundary, a new
//!      empty column has appeared between the
//!      originals (asserted on the model too).
//!
//! Run:  `cargo run -p ui --example ui-shots -- table_shots`
//! Out:  `$TABLE_SHOTS_OUT` (default `/tmp/table_shots`)

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use automerge::{AutoCommit, ObjId, ObjType, ROOT, transaction::Transactable};
use taffy::{AvailableSpace, NodeId, Size, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Point};
use winit::dpi::PhysicalPosition;
use winit::event::{ElementState, Modifiers, MouseButton, WindowEvent};

use ui::offscreen::OffscreenRenderer;
use ui::presence::PresenceCtx;
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::widget::Widget;
use ui::widget::table::{self, Table};

const MARGIN: f64 = 28.0;
const LEFT_GUTTER: f64 = 24.0;
const TOP_GUTTER: f64 = 24.0;
const DEFAULT_COL_W: f64 = 152.0;
const ROW_H: f64 = 40.0;

/// Rounds a canvas dimension up to the nearest whole pixel. Every caller
/// passes a positive, finite table extent, so `v` is always in
/// `[0.0, u32::MAX]` — the `.clamp` here makes the truncating/sign-losing
/// cast provably safe regardless.
#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "v is clamped into [0.0, u32::MAX] on the line above, so the cast can never truncate or flip sign"
)]
fn px_dim(v: f64) -> u32 {
    v.ceil().clamp(0.0, f64::from(u32::MAX)) as u32
}

fn seed(doc: &mut AutoCommit, t: &ObjId) {
    // 4 rows (incl. header) × 3 columns.
    for _ in 0..3 {
        table::insert_column(doc, t, usize::MAX);
    }
    for _ in 0..4 {
        table::insert_row(doc, t, usize::MAX);
    }
    table::set_header(doc, t, true);
    let data = [
        ["Name", "Role", "Status"],
        ["Ada", "Owner", "Online"],
        ["Grace", "Editor", "Away"],
        ["Linus", "Viewer", "Offline"],
    ];
    for (r, row) in data.iter().enumerate() {
        for (c, cell) in row.iter().enumerate() {
            table::set_cell_text(doc, t, r, c, cell);
        }
    }
}

/// `sync` → `build_layout` → `compute_layout` → `apply_layout` at the
/// table's natural size, positioned at `(MARGIN, MARGIN)`.
fn frame(
    table: &mut Table,
    doc: &mut AutoCommit,
    t: &ObjId,
) -> (TaffyTree<()>, NodeId, (f64, f64)) {
    table.sync(doc, t);
    let theme = Theme::dark();
    let mut taffy: TaffyTree<()> = TaffyTree::new();
    let mut nm: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
    let mut tr = TextRenderer::new();
    let node = table.build_layout(&mut taffy, &theme, &mut nm);
    // The table node carries an explicit size; max-content lets it resolve to it.
    let layout_result = taffy.compute_layout_with_measure(
        node,
        Size {
            width: AvailableSpace::MaxContent,
            height: AvailableSpace::MaxContent,
        },
        |known, available, node_id, _, _| {
            nm.get(&node_id).map_or(Size::ZERO, |&p| unsafe {
                (*p).measure(known, available, &mut tr)
            })
        },
    );
    if let Err(e) = layout_result {
        panic!("layout failed: {e}");
    }
    table.apply_layout(&taffy, node, Point::new(MARGIN, MARGIN));
    let r = table.get_rect();
    (taffy, node, (r.width(), r.height()))
}

fn dispatch(
    table: &mut Table,
    taffy: &TaffyTree<()>,
    node: NodeId,
    doc: &mut AutoCommit,
    t: &ObjId,
    ev: &WindowEvent,
) {
    let mut handled = false;
    let mut focus_target = None;
    let mods = Modifiers::default();
    table.handle_event(
        ev,
        1.0,
        &mods,
        doc,
        t,
        &mut handled,
        &mut focus_target,
        node,
        taffy,
    );
}

fn cursor(x: f64, y: f64) -> WindowEvent {
    WindowEvent::CursorMoved {
        device_id: unsafe { std::mem::zeroed() },
        position: PhysicalPosition::new(x, y),
    }
}
fn press() -> WindowEvent {
    WindowEvent::MouseInput {
        device_id: unsafe { std::mem::zeroed() },
        state: ElementState::Pressed,
        button: MouseButton::Left,
    }
}

fn shoot(table: &mut Table, size: (f64, f64), out: &Path) {
    let theme = Theme::dark();
    let mut tr = TextRenderer::new();
    let presence = PresenceCtx::default();
    let cw = px_dim(size.0 + MARGIN * 2.0);
    let ch = px_dim(size.1 + MARGIN * 2.0);
    let mut scene = Scene::new();
    table.draw(&mut scene, Affine::IDENTITY, &mut tr, &theme, &presence);
    let mut r = OffscreenRenderer::new(cw, ch);
    r.render_png(&scene, theme.bg_app, out);
    println!("wrote {}", out.display());
}

pub fn run(_args: &[String]) {
    let dir = std::env::var("TABLE_SHOTS_OUT").unwrap_or_else(|_| "/tmp/table_shots".to_string());
    if let Err(e) = std::fs::create_dir_all(&dir) {
        eprintln!("failed to create output dir {dir}: {e}");
        return;
    }
    let dir = PathBuf::from(dir);

    let mut doc = AutoCommit::new();
    let t = match doc.put_object(&ROOT, "t", ObjType::Map) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("failed to create table object: {e}");
            return;
        }
    };
    seed(&mut doc, &t);

    let mut table = Table::new("demo").with_header();

    // 1. base
    let (taffy, node, size) = frame(&mut table, &mut doc, &t);
    shoot(&mut table, size, &dir.join("table-base.png"));

    // Middle column boundary (between col 0 and col 1), in the top gutter.
    let bx = MARGIN + LEFT_GUTTER + DEFAULT_COL_W;
    let by = MARGIN + TOP_GUTTER / 2.0;

    // 2. hover the middle column-insert pill
    dispatch(&mut table, &taffy, node, &mut doc, &t, &cursor(bx, by));
    shoot(&mut table, size, &dir.join("table-insert-col.png"));

    // 3. hover a row's delete handle (row 2, a data row)
    let dy = MARGIN + TOP_GUTTER + 2.5 * ROW_H;
    dispatch(
        &mut table,
        &taffy,
        node,
        &mut doc,
        &t,
        &cursor(MARGIN + LEFT_GUTTER / 2.0, dy),
    );
    shoot(&mut table, size, &dir.join("table-delete-row.png"));

    // 4. click the middle boundary → insert a column, then relayout + shoot.
    let cols_before = table::dims(&doc, &t).1;
    dispatch(&mut table, &taffy, node, &mut doc, &t, &cursor(bx, by));
    dispatch(&mut table, &taffy, node, &mut doc, &t, &press());
    let cols_after = table::dims(&doc, &t).1;
    assert_eq!(
        cols_after,
        cols_before.saturating_add(1),
        "middle click inserted a column"
    );

    let (_taffy2, _node2, size2) = frame(&mut table, &mut doc, &t);
    shoot(&mut table, size2, &dir.join("table-after-insert.png"));

    // The freshly inserted middle column is empty; its neighbours are intact.
    assert_eq!(table::cell_text(&doc, &t, 1, 0), "Ada");
    assert_eq!(
        table::cell_text(&doc, &t, 1, 1),
        "",
        "new middle column is empty"
    );
    assert_eq!(table::cell_text(&doc, &t, 1, 2), "Owner");
    println!("OK: middle column inserted between originals; content preserved");
}
