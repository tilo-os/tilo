//! The folder comparison's listing.
//!
//! One row per file that differs between the two roots: what happened to it
//! (added on the left, removed, modified), its path in the tree, and how its
//! size changed. Choosing a row opens those two files as an ordinary
//! comparison, which is where the work happens; this listing is only the
//! index.
//!
//! The listing says when the walk stopped at its bound, so a very large tree
//! is never presented as a complete answer.

use vello::kurbo::{Point, Rect};
use vello::peniko::Fill;

use ui::chrome::{SelectionState, paint_row_selection};
use ui::paint::PaintCtx;
use ui::tokens::TextRole;
use ui::widget::{BadgeFace, ListRowFace, RowLeading, badge_column_width};

use crate::folder::{FolderComparison, FolderEntry, FolderKind};
use crate::i18n::{count, t, t_args};

/// A row's height at the current text scale.
pub fn row_height(text: &mut ui::text_render::TextRenderer, theme: &ui::theme::Theme) -> f64 {
    f64::from(
        text.measure_style("Ag", None, &theme.text(TextRole::Body))
            .1,
    ) + 2.0 * f64::from(theme.tokens.spacing.s2)
}

/// A byte count as the system writes it (`1.5 kB`): the folder listing is
/// read with everything else on the desktop.
fn format_bytes(bytes: u64) -> String {
    ui::format::size(bytes)
}

/// What a row says happened, beside its path.
fn entry_meta(entry: &FolderEntry) -> String {
    let left = format_bytes(entry.left_len);
    let right = format_bytes(entry.right_len);
    match entry.kind {
        FolderKind::Added => t_args("folder-meta-only-here", &[("size", left.into())]),
        FolderKind::Removed => t_args("folder-meta-only-other", &[("size", right.into())]),
        FolderKind::Modified if entry.by_length => t_args(
            "folder-meta-size",
            &[("left", left.into()), ("right", right.into())],
        ),
        FolderKind::Modified => t("folder-meta-contents"),
    }
}

/// What one paint produced.
pub struct FolderDraw {
    /// Every row that was painted, with the entry it shows.
    pub rows: Vec<(usize, Rect)>,
    /// Height of everything, for clamping the scroll.
    pub content_height: f64,
    /// The band the rows were painted in.
    pub viewport: Rect,
}

/// Paint the listing into `area`.
pub fn paint(
    ctx: &mut PaintCtx<'_>,
    area: Rect,
    comparison: &FolderComparison,
    selected: usize,
    scroll: f64,
    pointer: Option<Point>,
) -> FolderDraw {
    let theme = ctx.theme;
    let s = &theme.tokens.spacing;
    let row_h = row_height(ctx.text, theme);
    let band_h = f64::from(
        ctx.text
            .measure_style("Ag", None, &theme.text(TextRole::Subheading))
            .1,
    )
    .max(BadgeFace::height(theme))
        + f64::from(s.s3);
    let (added, removed, modified) = comparison.counts();
    let summary = t_args(
        "folder-summary",
        &[
            ("left", comparison.left_label.as_str().into()),
            ("right", comparison.right_label.as_str().into()),
            ("added", count(added)),
            ("removed", count(removed)),
            ("modified", count(modified)),
        ],
    );
    let cut = comparison.truncated.then(|| t("folder-truncated"));

    let viewport = Rect::new(area.x0, area.y0 + band_h, area.x1, area.y1);
    let content_height = comparison.entries.len() as f64 * row_h + f64::from(s.s2);
    let top = viewport.y0 - scroll;
    ctx.scene.push_layer(
        Fill::NonZero,
        vello::peniko::BlendMode::default(),
        1.0,
        ctx.transform,
        &viewport,
    );
    let ground = theme.tokens.colors.surfaces.content;
    let pointer = pointer.filter(|p| viewport.contains(*p));
    let mut rows = Vec::new();
    for index in 0..comparison.entries.len() {
        let row = Rect::new(
            viewport.x0,
            top + index as f64 * row_h,
            viewport.x1,
            top + (index + 1) as f64 * row_h,
        );
        if row.y1 < viewport.y0 || row.y0 > viewport.y1 {
            continue;
        }
        let hovered = pointer.is_some_and(|p| row.contains(p));
        paint_row_selection(
            ctx.scene,
            ctx.transform,
            row,
            SelectionState::new(index == selected, hovered, false),
            ground,
            theme,
        );
        rows.push((index, row));
    }
    // Rows are drawn after the selection band so the text sits over it.
    let badges: Vec<&str> = comparison
        .entries
        .iter()
        .map(|entry| entry.kind.tag())
        .collect();
    let badge_w = badge_column_width(ctx.text, theme, &badges);
    let meta: Vec<String> = comparison.entries.iter().map(entry_meta).collect();
    for (index, row) in &rows {
        let entry = &comparison.entries[*index];
        let mut face = ListRowFace::new(&entry.path)
            .leading(RowLeading::Badge(entry.kind.tag()))
            .leading_column(badge_w)
            .meta(&meta[*index])
            .selected(*index == selected);
        if index + 1 < comparison.entries.len() {
            face = face.separator(false);
        }
        face.paint(ctx, *row);
    }
    ctx.scene.pop_layer();

    // The band above the rows: which two folders, and how much differs.
    let label_style = theme.text(TextRole::Subheading);
    ctx.text(
        &summary,
        &label_style,
        theme.text_primary,
        Point::new(area.x0 + f64::from(s.s2), area.y0 + band_h / 2.0),
        None,
    );
    if let Some(cut) = cut {
        let width = f64::from(ctx.text.measure_style(&cut, None, &label_style).0);
        ctx.text(
            &cut,
            &label_style,
            theme.tokens.colors.semantic.warning,
            Point::new(area.x1 - f64::from(s.s2) - width, area.y0 + band_h / 2.0),
            None,
        );
    }
    FolderDraw {
        rows,
        content_height,
        viewport,
    }
}
