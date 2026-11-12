//! The list's frame, painted: the rows the viewport holds, the bar beside them, a row's
//! menu and the way back to what is new, drawn where they stand.

use vello::Scene;
use vello::kurbo::{Affine, Rect, RoundedRect};

use crate::paint::PaintCtx;
use crate::presence::PresenceCtx;
use crate::text_render::TextRenderer;
use crate::theme::Theme;

use super::remount::row_kept;
use super::shown::ListShown;
use super::{ListModel, RowSelection, VirtualList};
use crate::widget::DrawChild;

impl<M: ListModel> VirtualList<M> {
    /// Paint the frame: the rows in the viewport, the scrollbar beside them, a row's menu
    /// and the way back to what is new.
    pub(super) fn paint(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        self.painted_lifts.clear();
        self.painted_chevrons.clear();
        self.corners = crate::widget::scrollbar::SurfaceCorners::seen(theme, transform);
        self.reduce_motion = theme.reduce_motion;
        if self.reduce_motion {
            self.scroll.momentum.stop();
        }
        let now = crate::motion::frame_now();
        self.arrivals.mark_drawn(now);
        self.start_row_motion(self.feel.over(theme.motion()));
        self.motion = self.feel.over(theme.motion());
        self.scroll.momentum.set_view(
            self.viewport.viewport(),
            self.scroll_metrics().overflows(),
            self.motion,
        );
        // After a long reveal the rows fade in where they landed.
        let opacity = self.scroll.to.opacity(crate::motion::frame_now());
        // How far the rows the last presented frame showed moved on screen:
        // the frame stands that much further along than that one did.
        let moved = self.moved_since_presented();
        let presented = self.shown.presented().map(|shown| shown.position);
        self.shown.note(ListShown {
            offset: self.viewport.offset(),
            position: presented
                .zip(moved)
                .map_or(self.viewport.offset(), |(position, moved)| position - moved),
            moved_known: moved.is_some(),
            rect: self.rect,
            transform: crate::scroll_copy::on_screen(transform),
            beside_bar: self.rows_beside_bar,
            chrome: self.chrome_fingerprint(),
            in_motion: !self.row_motion.is_empty() || opacity < 1.0,
            first_shown: self.first_shown_row(),
            overlay: self.scrollbar_track(),
            places: self.places_near(),
            headings: self.headings_drawn(),
        });
        scene.push_layer(
            vello::peniko::Fill::NonZero,
            vello::peniko::BlendMode::default(),
            opacity,
            transform,
            &self.rect,
        );
        let accent = theme.tokens.colors.intent.accent_ink;
        // Past an end, fingers pull the rows through the rubber band.
        let frame_transform = transform;
        #[allow(
            clippy::arithmetic_side_effects,
            reason = "kurbo Affine composition is floating-point matrix math"
        )]
        let transform = transform * Affine::translate((0.0, -self.give()));
        self.push_row_selection();
        // Only the rows near the box: the list clips the others away.
        let near = self.placed_near();
        let boxes: Vec<(Option<Rect>, bool)> = self
            .mounted
            .iter()
            .enumerate()
            .map(|(at, row)| {
                if !near.contains(&at) {
                    return (None, false);
                }
                let growing = self.growth.iter().any(|(key, _)| key == &row.key);
                (self.row_box(row.index), growing)
            })
            .collect();
        let selections: Vec<RowSelection> = self
            .mounted
            .iter()
            .map(|row| self.row_selection(row.index))
            .collect();
        // A row moving after a change is drawn where it is on its way, and
        // an arriving one at its opacity (`motion::ListChange`).
        let now = crate::time::now();
        let poses: Vec<(f64, f32)> = self
            .mounted
            .iter()
            .map(|row| self.row_motion.pose(&row.key, now))
            .collect();
        let pending_shape = self.pending_shape();
        // The tree's chevrons are the list's own, over the rows of any view: one place folds.
        let tree = self.tree.clone();
        let chevrons = self.chevrons();
        // The row that grew into its item keeps its chevron on its own line, where it starts.
        let open = self.open.clone();
        let line_height = self.row_line;
        let guide_color = crate::surface::edge_on(theme.tokens.colors.surfaces.content);
        let _kept = crate::draw_cull::keep_scope(scene, self.draw_keep(opacity, now));
        let focused = self.focused_key.as_ref().filter(|_| self.is_focused);
        for (((row, (row_rect, growing)), selection), (dy, alpha)) in self
            .mounted
            .iter_mut()
            .zip(boxes)
            .zip(selections)
            .zip(poses)
        {
            let Some(row_rect) = row_rect else {
                continue;
            };
            // The lift the row's selection takes: around what the row shows, as a `ListRow`
            // paints it, and what an audit holds it to. A row whose view keeps no content box
            // of its own takes the box every other thing the list draws for its rows takes.
            let lift = (selection.selected && !self.delegate_selection).then(|| {
                let content =
                    crate::widget::row_content_box(row.widget.as_ref()).unwrap_or(row_rect);
                let painted = super::lift::selection_around(row_rect, content, self.lift);
                self.painted_lifts.push(crate::widget::RowLift {
                    rect: painted,
                    content,
                    bleed: self.lift,
                });
                painted
            });
            // A row the frame keeps on screen, unchanged, is not drawn
            // again: its selection, its ring and its widget are as they were.
            if focused != Some(&row.key)
                && !growing
                && row_kept(scene, transform, row.widget.as_ref(), lift)
            {
                continue;
            }
            let row_rect = row_rect + vello::kurbo::Vec2::new(0.0, dy);
            let row_transform = transform * Affine::translate((0.0, dy));
            // Each row is an element of its own on a page shown as HTML,
            // kept by the row's key as rows scroll in and out.
            let row_key = crate::scene_parts::begin_item(
                scene,
                crate::scene_parts::item_key("ui.list-row", &row.key),
                transform * Affine::translate((row_rect.x0, row_rect.y0)),
            );
            let faded = alpha < 1.0;
            if faded {
                scene.push_layer(
                    vello::peniko::Fill::NonZero,
                    vello::peniko::BlendMode::default(),
                    alpha,
                    transform,
                    &row_rect.inflate(4.0, 4.0),
                );
            }
            // The same selection a ListRow paints, so a list of ListRows
            // reads identically to a painted one; a run of selected rows is
            // one lift. A delegate that owns its selection paints its own.
            if let Some(lift) = lift {
                let lift = lift + vello::kurbo::Vec2::new(0.0, dy);
                crate::chrome::paint_row_selection_joined(
                    scene,
                    transform,
                    lift,
                    crate::chrome::SelectionState::active(selection.selected),
                    theme.tokens.colors.surfaces.content,
                    theme,
                    selection.join(),
                );
            }
            if self.is_focused && self.focused_key.as_ref() == Some(&row.key) {
                let radius = f64::from(theme.tokens.radii.item);
                crate::widget::paint_focus_ring(
                    scene,
                    transform,
                    row_rect.inset(-2.0),
                    (radius - 2.0).max(0.0),
                    theme,
                );
            }
            if let Some(place) = self
                .row_presence
                .as_ref()
                .and_then(|place| place(&self.model, row.index, &row.key))
            {
                // Mist is painted in device space.
                let device = transform.transform_rect_bbox(row_rect);
                for user in presence.peers_on(&place) {
                    presence.draw_peer_mist(scene, theme, user, device);
                }
            }
            let slot = row.widget.get_rect() + vello::kurbo::Vec2::new(0.0, dy);
            // A row arriving at a height of its own is drawn as far as it
            // stands there: the rest of its item shows as the row reaches it.
            let clipped = growing.then(|| {
                let shown = self
                    .viewport
                    .item_height(row.index)
                    .unwrap_or(slot.height());
                Rect::from_points(
                    slot.origin(),
                    vello::kurbo::Point::new(slot.x1, slot.y0 + shown.min(slot.height())),
                )
            });
            if let Some(clip) = clipped {
                scene.push_layer(
                    vello::peniko::Fill::NonZero,
                    vello::peniko::BlendMode::default(),
                    1.0,
                    transform,
                    &clip,
                );
            }
            self.arrivals.paint(
                &row.key,
                pending_shape,
                &mut PaintCtx::new(scene, transform, text_renderer, theme),
                slot,
                now,
                &mut |ctx| {
                    row.widget
                        .draw_child(ctx.scene, row_transform, ctx.text, theme, presence);
                },
            );
            if let Some(marks) = tree.as_ref() {
                // The chevron and the guides hang on the row's own box: the one its text, its
                // leading control and the lift stand in. A finger's row takes a taller slot than
                // its box (a collection row a touch can hit), so centring the chevron on the slot
                // would drop it below the words it folds.
                let shown = row.widget.get_rect();
                let shown = if shown.height() > 0.0 {
                    shown
                } else {
                    row_rect
                };
                let line = Rect::new(row_rect.x0, shown.y0, row_rect.x1, shown.y1);
                let line = if open.as_ref() == Some(&row.key) && line.height() > line_height {
                    Rect::new(line.x0, line.y0, line.x1, line.y0 + line_height)
                } else {
                    line
                };
                if self.indent_guides {
                    chevrons.paint_guides(
                        marks,
                        scene,
                        transform,
                        &row.key,
                        line,
                        crate::ColorCompat::with_alpha_factor(guide_color, alpha),
                    );
                }
                // A row standing open folds its branch away: the document it grew into lists
                // the rows, so its chevron reads folded.
                let folded = marks.is_folded(&row.key) || open.as_ref() == Some(&row.key);
                chevrons.paint(marks, scene, transform, theme, &row.key, line, folded);
                if let Some(rect) = chevrons.rect(marks, theme, &row.key, line, folded) {
                    self.painted_chevrons
                        .push(crate::widget::virtual_list::RowChevron { rect: rect.0, line });
                }
            }
            if clipped.is_some() {
                scene.pop_layer();
            }
            if faded {
                scene.pop_layer();
            }
            crate::scene_parts::end_widget(scene, row_key);
        }
        // The headings over the groups, over the rows they hold.
        self.draw_bands(scene, transform, text_renderer, theme);
        // A row renamed in place: the field over its label, kept on the row as it moves.
        self.place_rename();
        if let Some(field) = self.rename.as_mut() {
            field
                .input
                .draw_child(scene, transform, text_renderer, theme, presence);
        }
        if let Some(preview) = self.reorder.preview() {
            scene.fill(
                vello::peniko::Fill::NonZero,
                transform,
                accent,
                None,
                &preview,
            );
        }
        scene.pop_layer();
        let transform = frame_transform;

        if let Some(bar) = &self.overlay_bar {
            bar.note_offset(self.viewport.offset());
            bar.paint(scene, transform, self.rect, self.scroll_metrics(), theme);
        } else if let Some(track) = self.scrollbar_track() {
            scene.fill(
                vello::peniko::Fill::NonZero,
                transform,
                crate::widget::scrollbar::track_color(theme),
                None,
                &RoundedRect::from_rect(track, track.width() / 2.0),
            );
            self.vbar
                .draw(scene, transform, track, self.scroll_metrics(), theme, false);
        }
        // The way back, over the foot of a transcript the reader has moved away from: to the
        // first unread row, or to what is being said now.
        if let Some(pill) = self.jump_face() {
            pill.paint(&mut PaintCtx::new(scene, transform, text_renderer, theme));
        }
    }
}
