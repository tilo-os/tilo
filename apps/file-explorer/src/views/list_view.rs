//! List-mode content built on the toolkit's `DataTable<Entry>` primitive.
//!
//! Columns: icon | Name | Size | Modified. Each column registers a
//! text-capable cell renderer (the toolkit's `with_text_render`), so the
//! list finally draws real data instead of blank rows. The table is
//! selectable and reports row/header clicks back through the command sink.

use crate::command::{push, CommandSink, FileCommand};
use crate::model::{format_modified, human_size, Entry};
use ui::icons::{draw_icon, icon_for_path, LUCIDE_DEFAULT_STROKE_PX};
use ui::widget::data_table::{ColumnDef, ColumnWidth, DataTable};
use ui::widget::Widget;
use ui::SelectionModel;
use vello::kurbo::{Affine, Rect};
use vello::peniko::Color;

/// Column keys in left-to-right display order (AUDIT.md M1).
pub const COLUMN_KEYS: &[&str] = &["icon", "name", "size", "modified"];

const ICON_PX: f64 = 16.0;
const CELL_PAD_X: f64 = 8.0;

fn cell_text_color(theme: &ui::theme::Theme, selected: bool) -> Color {
    if selected {
        Color::WHITE
    } else {
        theme.tokens.colors.text_primary
    }
}

/// Draw left-aligned, vertically-centred text inside `cell`.
fn draw_cell_text(
    text: &str,
    scene: &mut vello::Scene,
    tr: &mut ui::text_render::TextRenderer,
    cell: Rect,
    theme: &ui::theme::Theme,
    selected: bool,
) {
    let size = theme.tokens.typography.scale.sm;
    let y = cell.y0 + (cell.height() - size as f64) / 2.0;
    tr.draw_text(
        scene,
        text,
        size,
        cell_text_color(theme, selected),
        Affine::translate((cell.x0 + CELL_PAD_X, y)),
        None,
    );
}

/// Build the list-view's `ColumnDef<Entry>` set, each with a renderer.
pub fn build_columns() -> Vec<ColumnDef<Entry>> {
    vec![
        ColumnDef::new("icon", "", ColumnWidth::Fixed(34.0)).with_text_render(Box::new(
            |entry: &Entry, scene, _tr, cell, theme, selected| {
                let iy = cell.y0 + (cell.height() - ICON_PX) / 2.0;
                let ix = cell.x0 + (cell.width() - ICON_PX) / 2.0;
                let color = cell_text_color(theme, selected);
                draw_icon(
                    scene,
                    Affine::IDENTITY,
                    Rect::new(ix, iy, ix + ICON_PX, iy + ICON_PX),
                    icon_for_path(&entry.path, entry.is_dir),
                    color,
                    LUCIDE_DEFAULT_STROKE_PX,
                );
            },
        )),
        ColumnDef::new("name", "Name", ColumnWidth::Flex(1.0))
            .sortable(true)
            .with_text_render(Box::new(|entry: &Entry, scene, tr, cell, theme, selected| {
                let label = if entry.is_conflict() {
                    format!("{}  ⚠ conflict", entry.name)
                } else {
                    entry.name.clone()
                };
                draw_cell_text(&label, scene, tr, cell, theme, selected);
            })),
        ColumnDef::new("size", "Size", ColumnWidth::Fixed(90.0))
            .sortable(true)
            .with_text_render(Box::new(|entry: &Entry, scene, tr, cell, theme, selected| {
                draw_cell_text(&human_size(entry), scene, tr, cell, theme, selected);
            })),
        ColumnDef::new("modified", "Modified", ColumnWidth::Fixed(150.0))
            .sortable(true)
            .with_text_render(Box::new(|entry: &Entry, scene, tr, cell, theme, selected| {
                draw_cell_text(&format_modified(entry.modified), scene, tr, cell, theme, selected);
            })),
    ]
}

/// Construct the `DataTable<Entry>` used by list-mode, bound to the live
/// selection + command sink.
pub fn build_list(entries: &[Entry], selection: &SelectionModel, sink: &CommandSink) -> Box<dyn Widget> {
    let mut table: DataTable<Entry> = DataTable::new(build_columns()).selectable(true);
    table.set_rows(entries.to_vec());
    table.set_selection(selection.clone());

    let click_sink = sink.clone();
    table = table.on_row_click(move |i, ctrl, shift| {
        push(&click_sink, FileCommand::Click { index: i, ctrl, shift });
    });
    let header_sink = sink.clone();
    table = table.on_header_click(move |col| {
        push(&header_sink, FileCommand::SortColumn(col));
    });
    let activate_sink = sink.clone();
    table = table.on_activate(move |i| {
        push(&activate_sink, FileCommand::Activate(i));
    });
    Box::new(table)
}
