//! A sort the person clicks and rows they add or remove move on the
//! standard list change (`motion::ListChange`), as a list's do: only rows
//! shown in the body move, rows coming from off-screen fade in at their
//! slot, and a key, a sync and reduced motion land.

use super::{DataTableCore, RowIdentity, TableModelChange};
use crate::presence::PresenceCtx;
use crate::text_render::TextRenderer;
use crate::theme::Theme;
use vello::Scene;
use vello::kurbo::Affine;

/// The table's rows on their way, and where the last frame showed them.
///
/// A view that builds its table anew on every change (a sort it keeps in
/// its own state rebuilds the view) would drop the motion a header click
/// started; the table leaves it, under its id, for the table the rebuild
/// puts in its place (`motion::keep`), which goes on from there.
#[derive(Default)]
pub(super) struct Moving {
    /// Shown rows on their way (`motion::ListChange`).
    pub(super) rows: crate::collection_transition::RowMotions<RowIdentity>,
    /// The rows the last frame showed and their tops: where a change the
    /// person makes moves them from.
    pub(super) drawn: Vec<(RowIdentity, f64)>,
    /// Laid out, so it took up what the table it replaces left, and leaves
    /// its own when dropped.
    bound: bool,
}

impl DataTableCore {
    /// At its first layout, go on from the rows the table with its id
    /// left when a rebuild dropped it.
    pub(super) fn take_up_row_motion(&mut self) {
        if std::mem::replace(&mut self.moving.bound, true) || self.test_id.is_empty() {
            return;
        }
        if let Some(kept) = crate::motion::take_kept::<Moving>(&self.test_id) {
            self.moving = Moving {
                bound: true,
                ..kept
            };
        }
    }

    /// The rows shown in the body now, each with its top: what a frame
    /// draws, and what a change moves from and to. Rows outside the body's
    /// viewport (the overscan) are not shown, so they never animate.
    pub(super) fn shown_rows(&self) -> Vec<(RowIdentity, f64)> {
        let (start, end) = self.visible_row_range();
        let body_h = self.body_viewport_h();
        (start..end)
            .filter_map(|index| {
                let top = self.viewport.item_view_top(index)?;
                let height = self.viewport.item_height(index)?;
                (top + height > 0.0 && top < body_h)
                    .then(|| Some((self.row_identity_at(index)?, top)))
                    .flatten()
            })
            .collect()
    }

    /// Before a structural change (`changes`, already applied to the rows
    /// or the model): note where the shown rows stood, so the next frame
    /// moves the survivors from there and fades arrivals in, as a list does
    /// (`motion::ListChange`). A change nobody just made by hand (a sync, a
    /// key, an agent), a reset and reduced motion land at once.
    pub(super) fn capture_row_motion(&mut self, changes: &[TableModelChange]) {
        let structural = changes.iter().any(|change| {
            matches!(
                change,
                TableModelChange::RowsInserted { .. }
                    | TableModelChange::RowsRemoved { .. }
                    | TableModelChange::RowsMoved { .. }
            )
        });
        let resets = changes
            .iter()
            .any(|change| matches!(change, TableModelChange::Reset));
        let inserts = changes
            .iter()
            .any(|change| matches!(change, TableModelChange::RowsInserted { .. }));
        let source = crate::collection_transition::RowMotions::<RowIdentity>::caused_by_hand(
            crate::motion::frame_now(),
            self.reduce_motion,
        )
        .filter(|_| structural && !resets && !self.moving.drawn.is_empty());
        self.moving
            .rows
            .capture(self.moving.drawn.iter().copied(), inserts, source);
    }

    /// Before a sort, or a keyed table's rows replaced by its host: note
    /// where the shown rows stand. Every row shown after it that was not
    /// shown before came from off-screen (or is new) and fades in at its
    /// slot.
    pub(super) fn capture_reorder_motion(&mut self) {
        let source = crate::collection_transition::RowMotions::<RowIdentity>::caused_by_hand(
            crate::motion::frame_now(),
            self.reduce_motion,
        )
        .filter(|_| !self.moving.drawn.is_empty());
        self.moving
            .rows
            .capture(self.moving.drawn.iter().copied(), true, source);
    }

    /// Start the motion of a change made since the last frame, from where
    /// the rows were drawn to where they are now, then draw the rows and
    /// their cell controls where they are on their way.
    pub(super) fn draw_moving_rows(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) -> Vec<crate::widget::paint_damage::PaintPart> {
        // A sort or change the person made since the last frame starts its
        // motion from where the rows were drawn to where they are now.
        let now = crate::motion::frame_now();
        let shown = self.shown_rows();
        self.moving
            .rows
            .start(shown.iter().copied(), theme.motion(), now);
        self.moving.drawn = shown;
        let offset = self.viewport.offset();
        #[allow(
            clippy::arithmetic_side_effects,
            reason = "kurbo Affine composition is floating-point matrix math; it cannot overflow or panic"
        )]
        let scrolled = transform * Affine::translate((0.0, -offset)) * transform.inverse();
        let parts = self.draw_rows(transform, text_renderer, theme);
        for part in &parts {
            scene.append(part.scene(), Some(scrolled));
        }
        for cell in &mut self.mounted_cells {
            // A cell control travels and fades with its row.
            let (dy, alpha) = self.moving.rows.pose(&cell.row_key, now);
            let cell_transform = transform * Affine::translate((0.0, dy));
            let faded = alpha < 1.0;
            if faded {
                scene.push_layer(
                    vello::peniko::Fill::NonZero,
                    vello::peniko::BlendMode::default(),
                    alpha,
                    cell_transform,
                    &cell.widget.get_rect(),
                );
            }
            cell.widget
                .draw(scene, cell_transform, text_renderer, theme, presence);
            if faded {
                scene.pop_layer();
            }
        }
        parts
    }
}

/// A table a rebuild drops leaves its rows' motion for the one put in its
/// place.
impl Drop for DataTableCore {
    fn drop(&mut self) {
        if self.moving.bound && !self.test_id.is_empty() {
            crate::motion::keep(self.test_id.clone(), None, std::mem::take(&mut self.moving));
        }
    }
}
