//! Floating comments panel for a highlighted passage.
//!
//! The reader supplies annotations and the current composition id; this
//! component owns the overlay's open/anchor state, paints the panel, and keeps
//! its hit regions private so clicks cannot fall through to the page beneath.

use automerge::{AutoCommit, ObjId};
use ui::theme::Theme;
use uuid::Uuid;
use vello::kurbo::{Affine, Point, Rect};

use std::collections::HashMap;
use ui::ColorCompat;

use tilo_rich_text::RichTextView;
use ui::paint::PaintCtx;
use ui::text_render::{TextRenderer, TextWeight};

use crate::input_controls::{InputEventCtx, NoteEditor};
use crate::model::{AnnoKind, Annotation, HiColor};
use crate::paint;
use crate::reader_theme::ReaderPalette;

const TOP_BAR: f64 = 54.0;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CommentAction {
    Close,
    Swallow,
    DeleteHighlight(Uuid),
    Add(Uuid),
    Edit(Uuid),
    Delete(Uuid),
}

#[derive(Clone)]
struct Hit {
    rect: Rect,
    action: CommentAction,
}

#[derive(Default)]
pub(crate) struct CommentOverlay {
    group: Option<Uuid>,
    anchor: Option<(f64, f64, f64, f64)>,
    hits: Vec<Hit>,
    /// The comment being written, in the SDK editor bound to it.
    editor: Option<NoteEditor>,
    /// Each comment as written, one read-only view per comment id.
    views: HashMap<Uuid, RichTextView>,
}

impl CommentOverlay {
    pub(crate) fn accessibility_nodes(
        &self,
        annotations: &[Annotation],
        focused: Option<&str>,
    ) -> Vec<ui::a11y::AccessNode> {
        use ui::a11y::{AccessNode, AccessState, Role};
        let Some(group) = self.group else {
            return Vec::new();
        };
        let (_, excerpt, comments) = gather_content(annotations, group, HiColor::Yellow);
        let mut controls = self
            .hits
            .iter()
            .filter_map(|hit| {
                let (target, label): (String, String) = match hit.action {
                    CommentAction::Close => {
                        ("books.comments.close".into(), "Close comments".into())
                    }
                    CommentAction::DeleteHighlight(id) => (
                        format!("books.comments.delete-highlight:{id}"),
                        "Delete highlight and comments".into(),
                    ),
                    CommentAction::Add(id) => {
                        (format!("books.comments.add:{id}"), "Add comment".into())
                    }
                    CommentAction::Edit(id) => {
                        (format!("books.comments.edit:{id}"), "Edit comment".into())
                    }
                    CommentAction::Delete(id) => (
                        format!("books.comments.delete:{id}"),
                        "Delete comment".into(),
                    ),
                    CommentAction::Swallow => return None,
                };
                Some(
                    AccessNode::new(Role::Button)
                        .with_name(label)
                        .with_bounds(hit.rect)
                        .with_state(
                            AccessState::new()
                                .focusable(true)
                                .focused(focused == Some(target.as_str())),
                        )
                        .with_action_target(target),
                )
            })
            .collect::<Vec<_>>();
        if let Some(editing) = self.editor.as_ref() {
            let mut editor = editing
                .accessibility()
                .unwrap_or_else(|| AccessNode::new(Role::MultilineTextInput));
            editor.name = Some("Comment text".into());
            controls.push(
                editor
                    .with_bounds(editing.rect())
                    .with_action_target(format!("books.comments.input:{}", editing.id())),
            );
        }
        controls.extend(comments.into_iter().map(|comment| {
            AccessNode::new(Role::ListItem)
                .with_name(comment_display(&comment))
                .with_hashed_key(comment.id)
        }));
        vec![
            AccessNode::new(Role::Dialog)
                .with_name("Highlight comments")
                .with_description(excerpt)
                .with_state(AccessState::new().modal(true))
                .with_children(controls),
        ]
    }

    pub(crate) fn semantic_action(&self, target: &str) -> Option<CommentAction> {
        self.hits.iter().rev().find_map(|hit| {
            let candidate = match hit.action {
                CommentAction::Close => "books.comments.close".into(),
                CommentAction::DeleteHighlight(id) => {
                    format!("books.comments.delete-highlight:{id}")
                }
                CommentAction::Add(id) => format!("books.comments.add:{id}"),
                CommentAction::Edit(id) => format!("books.comments.edit:{id}"),
                CommentAction::Delete(id) => format!("books.comments.delete:{id}"),
                CommentAction::Swallow => return None,
            };
            (candidate == target).then_some(hit.action)
        })
    }

    /// An assistive technology's action on the comment being written.
    /// Returns whether it was the editor's.
    pub(crate) fn perform_editor_semantic_action(
        &mut self,
        target: &str,
        action: &ui::a11y::Action,
        state: &mut AutoCommit,
        root: &ObjId,
    ) -> bool {
        let Some(editor) = self.editor.as_mut() else {
            return false;
        };
        target == format!("books.comments.input:{}", editor.id())
            && editor.perform_action(action, state, root)
    }
}

/// Highlight color, quoted excerpt, and note comments belonging to `group`.
fn gather_content(
    annotations: &[Annotation],
    group: Uuid,
    default_color: HiColor,
) -> (HiColor, String, Vec<Annotation>) {
    let (color, excerpt) = {
        let mut rows: Vec<&Annotation> = annotations
            .iter()
            .filter(|a| a.kind == AnnoKind::Highlight && a.group == group)
            .collect();
        rows.sort_by_key(|a| (a.block, a.start));
        let color = rows.first().map_or(default_color, |a| a.color);
        let excerpt = rows
            .iter()
            .map(|a| a.excerpt.trim())
            .filter(|s| !s.is_empty())
            .collect::<Vec<_>>()
            .join(" ");
        (color, excerpt)
    };
    let excerpt = if excerpt.is_empty() {
        "Highlighted passage".to_string()
    } else {
        format!("\u{201C}{excerpt}\u{201D}")
    };
    let mut comments: Vec<Annotation> = annotations
        .iter()
        .filter(|a| a.kind == AnnoKind::Note && a.group == group)
        .cloned()
        .collect();
    comments.sort_by_key(|a| a.created);
    (color, excerpt, comments)
}

/// Text shown for a note row when its shared editor is not active.
fn comment_display(a: &Annotation) -> String {
    if a.note.trim().is_empty() {
        "Empty comment".to_string()
    } else {
        a.note.clone()
    }
}

impl CommentOverlay {
    /// Height of each comment row, in reading order: the comment as
    /// written (measured with the view that draws it), or the editor while
    /// it is being written.
    fn comment_row_heights(
        &mut self,
        tr: &mut TextRenderer,
        theme: &Theme,
        comments: &[Annotation],
        text_w: f32,
    ) -> Vec<f64> {
        let editing = self.editing_id();
        let editor_h = self.editor.as_ref().map(|editor| editor.rect().height());
        comments
            .iter()
            .map(|a| {
                if editing == Some(a.id) {
                    return editor_h.map_or(76.0, |h| (h + 12.0).max(76.0));
                }
                let text_h = if a.note.trim().is_empty() {
                    f64::from(tr.measure(&comment_display(a), 13.0, Some(text_w)).1)
                } else {
                    let view = self.views.entry(a.id).or_insert_with(comment_view);
                    view.show(&a.blocks);
                    view.layout(Rect::new(0.0, 0.0, f64::from(text_w), 0.0), theme, tr)
                };
                text_h.max(18.0) + 16.0
            })
            .collect()
    }
}

/// A read-only view of one comment, in comment type.
fn comment_view() -> RichTextView {
    let mut view = RichTextView::new();
    view.editor_mut().set_body_size(13.0);
    view
}

impl CommentOverlay {
    pub(crate) fn open(&mut self, group: Uuid) {
        self.group = Some(group);
    }

    pub(crate) fn close(&mut self) {
        self.group = None;
        self.anchor = None;
        self.hits.clear();
        self.finish_editing();
    }

    pub(crate) fn group(&self) -> Option<Uuid> {
        self.group
    }

    pub(crate) fn is_open(&self) -> bool {
        self.group.is_some()
    }

    /// Write comment `id` in the SDK editor; it shows the comment on the
    /// next sync.
    pub(crate) fn begin_editing(&mut self, id: Uuid) {
        self.editor = Some(NoteEditor::new(id, 13.0, "Write a comment…"));
    }

    pub(crate) fn finish_editing(&mut self) {
        self.editor = None;
    }

    pub(crate) fn editing_id(&self) -> Option<Uuid> {
        self.editor.as_ref().map(NoteEditor::id)
    }

    /// Bring the comment being written up to date with the document.
    pub(crate) fn sync(&mut self, state: &mut AutoCommit, root: &ObjId) {
        if let Some(editor) = self.editor.as_mut() {
            editor.sync(state, root);
        }
    }

    /// The undo boundary the comment's last edit opened, for the host.
    pub(crate) fn take_committed(&mut self) -> Option<Vec<automerge::ChangeHash>> {
        self.editor.as_mut().and_then(NoteEditor::take_committed)
    }

    /// Whether a typed word keeps its undo step open.
    pub(crate) fn keeps_step_open(&self) -> bool {
        self.editor
            .as_ref()
            .is_some_and(NoteEditor::keeps_step_open)
    }

    /// Route native editing events to the comment being written. Returns
    /// whether the person finished it (Enter or Escape), which closes the
    /// editor; the text is already in the document.
    pub(crate) fn handle_event(&mut self, ctx: &mut InputEventCtx<'_>) -> bool {
        let Some(editor) = self.editor.as_mut() else {
            return false;
        };
        let finished = editor.handle_event(ctx).is_some();
        if finished {
            self.finish_editing();
        }
        finished
    }

    pub(crate) fn set_anchor(&mut self, anchor: Option<(f64, f64, f64, f64)>) {
        self.anchor = anchor;
    }

    pub(crate) fn begin_frame(&mut self) {
        self.hits.clear();
    }

    /// Return the topmost overlay action at `point`. The full-window dismiss
    /// hit is recorded first and the card/actions afterward, mirroring paint
    /// order and making the panel modal without leaking its geometry outward.
    pub(crate) fn hit_test(&self, point: Point) -> Option<CommentAction> {
        self.hits
            .iter()
            .rev()
            .find(|hit| hit.rect.contains(point))
            .map(|hit| hit.action)
    }

    fn push_hit(&mut self, rect: Rect, action: CommentAction) {
        self.hits.push(Hit { rect, action });
    }

    /// Card rect sized to its content, anchored beside `self.anchor` (or the
    /// viewport center) and clamped inside `viewport`.
    fn card_rect(&self, viewport: Rect, cw: f64, ch: f64) -> Rect {
        let (ax, atop, abot) = self.anchor.map_or_else(
            || {
                let c = viewport.center();
                (c.x, c.y, c.y)
            },
            |(x0, y0, x1, y1)| (f64::midpoint(x0, x1), y0, y1),
        );
        let x0 = (ax - cw / 2.0).clamp(
            viewport.x0 + 12.0,
            (viewport.x1 - 12.0 - cw).max(viewport.x0 + 12.0),
        );
        let top = viewport.y0 + TOP_BAR + 8.0;
        let bottom = viewport.y1 - 16.0;
        let y0 = if abot + 12.0 + ch <= bottom {
            abot + 12.0
        } else if atop - 12.0 - ch >= top {
            atop - 12.0 - ch
        } else {
            (bottom - ch).max(top)
        };
        Rect::new(x0, y0, x0 + cw, y0 + ch)
    }

    /// Draws the card shadow/background/border and the "Highlight" header row
    /// (color swatch, label, delete-highlight trash); records their hits.
    /// Returns the y-coordinate immediately below the header.
    fn draw_card_and_header(
        &mut self,
        ctx: &mut PaintCtx<'_>,
        cursor: Point,
        layout: &CommentCard<'_>,
    ) -> f64 {
        let (t, theme) = (ctx.transform, ctx.theme);
        let PaintCtx {
            scene, text: tr, ..
        } = ctx;
        let &CommentCard {
            card,
            pad,
            header_h,
            color,
            group,
            ..
        } = layout;
        let colors = &theme.tokens.colors;
        // A floating Panel: elevation, ground and the derived Panel border.
        // The focus ring belongs to the control being edited, not the card.
        crate::chrome::panel(scene, t, theme, card);
        self.push_hit(card, CommentAction::Swallow);

        let ix0 = card.x0 + pad;
        let ix1 = card.x1 - pad;
        let y = card.y0 + pad;
        paint::circle(
            scene,
            t,
            Point::new(ix0 + 6.0, y + 8.0),
            6.0,
            ReaderPalette::highlight_swatch(color),
        );
        tr.draw_text_weighted(
            scene,
            "Highlight",
            13.5,
            colors.text_primary,
            t * Affine::translate((ix0 + 20.0, y + 1.0)),
            None,
            TextWeight::Semibold,
        );
        let trash = Rect::new(ix1 - 24.0, y - 2.0, ix1, y + 22.0);
        let hovered = trash.contains(cursor);
        if hovered {
            paint::fill_round(scene, t, trash, 6.0, theme.bg_hover);
        }
        paint::trash(
            scene,
            t,
            trash,
            if hovered {
                colors.semantic.danger
            } else {
                colors.text_dim
            },
            1.5,
        );
        self.push_hit(trash, CommentAction::DeleteHighlight(group));
        y + header_h
    }

    /// Draws each comment row (text, delete/edit affordances) and records
    /// their hits. Returns the y-coordinate immediately below the last row.
    fn draw_comment_rows(
        &mut self,
        ctx: &mut PaintCtx<'_>,
        cursor: Point,
        layout: &CommentCard<'_>,
        mut y: f64,
    ) -> f64 {
        use crate::chrome::rhythm;
        let (t, theme) = (ctx.transform, ctx.theme);
        let PaintCtx {
            scene, text: tr, ..
        } = ctx;
        let &CommentCard {
            comments,
            row_heights,
            text_w,
            gap,
            ..
        } = layout;
        let (ix0, ix1) = (layout.ix0(), layout.ix1());
        let colors = &theme.tokens.colors;
        // The two row controls are full pointer targets, a sibling step from
        // each other and a tight step in from the row's edge.
        let control = 24.0;
        for (annotation, height) in comments.iter().zip(row_heights) {
            let height = *height;
            let row = Rect::new(ix0, y, ix1, y + height);
            // A row on the panel: one tier up, with its derived edge.
            crate::chrome::card_on_raised(scene, t, theme, row, 8.0);
            let delete = Rect::new(
                ix1 - rhythm::TIGHT - control,
                y + rhythm::TIGHT,
                ix1 - rhythm::TIGHT,
                y + rhythm::TIGHT + control,
            );
            let edit = Rect::new(
                delete.x0 - rhythm::SIBLING - control,
                delete.y0,
                delete.x0 - rhythm::SIBLING,
                delete.y1,
            );
            let editing = self.editing_id() == Some(annotation.id);
            if editing {
                paint::stroke_round(scene, t, row, 8.0, theme.border_focus, 1.4);
            }
            if let (true, Some(editor)) = (editing, self.editor.as_mut()) {
                editor.layout(
                    Rect::new(ix0 + 8.0, y + 6.0, edit.x0 - rhythm::RELATED, y + 6.0),
                    theme,
                    tr,
                );
                editor.draw(scene, t, tr, theme);
            } else if annotation.note.trim().is_empty() {
                tr.draw_text(
                    scene,
                    &comment_display(annotation),
                    13.0,
                    colors.text_dim,
                    t * Affine::translate((ix0 + 10.0, y + 8.0)),
                    Some(text_w),
                );
            } else {
                let view = self.views.entry(annotation.id).or_insert_with(comment_view);
                view.show(&annotation.blocks);
                let x0 = ix0 + 10.0;
                view.layout(
                    Rect::new(x0, y + 8.0, x0 + f64::from(text_w), y + 8.0),
                    theme,
                    tr,
                );
                view.draw(scene, t, tr, theme);
            }
            let delete_hover = delete.contains(cursor);
            paint::close(
                scene,
                t,
                delete,
                if delete_hover {
                    colors.semantic.danger
                } else {
                    colors.text_dim.with_alpha_factor(0.7)
                },
                1.4,
            );
            self.push_hit(
                crate::chrome::pointer_target(delete, theme),
                CommentAction::Delete(annotation.id),
            );
            paint::pencil(
                scene,
                t,
                edit,
                if edit.contains(cursor) {
                    colors.text_primary
                } else {
                    colors.text_dim.with_alpha_factor(0.7)
                },
                1.4,
            );
            if !editing {
                self.push_hit(
                    crate::chrome::pointer_target(edit, theme),
                    CommentAction::Edit(annotation.id),
                );
            }
            y += height + gap;
        }
        y
    }

    /// Draws the "Add comment" button and records its hit.
    fn draw_add_button(
        &mut self,
        ctx: &mut PaintCtx<'_>,
        cursor: Point,
        layout: &CommentCard<'_>,
        y: f64,
    ) {
        let (t, theme) = (ctx.transform, ctx.theme);
        let PaintCtx {
            scene, text: tr, ..
        } = ctx;
        let add = Rect::new(layout.ix0(), y, layout.ix1(), y + layout.add_h - 4.0);
        // The shared Neutral face (HIG §8): raised, derived edge, themed hover.
        ui::widget::Button::paint(
            scene,
            t,
            tr,
            theme,
            &ui::widget::ButtonFace::new(add, "Add comment")
                .icon(ui::icons::Icon::Plus)
                .radius(8.0)
                .text_size(12.5)
                .weight(TextWeight::Semibold)
                .hovered(add.contains(cursor)),
        );
        self.push_hit(add, CommentAction::Add(layout.group));
    }

    pub(crate) fn draw(
        &mut self,
        ctx: &mut PaintCtx<'_>,
        viewport: Rect,
        cursor: Point,
        annotations: &[Annotation],
        default_color: HiColor,
    ) {
        self.begin_frame();
        let Some(group) = self.group else { return };

        // Tap-away closes. It is deliberately behind the card and its controls.
        self.push_hit(viewport, CommentAction::Close);

        let (color, excerpt, comments) = gather_content(annotations, group, default_color);

        let cw = 360.0_f64.min(viewport.width() - 24.0);
        let pad = 16.0;
        let inner_w = (cw - pad * 2.0) as f32;
        let text_w = inner_w - 52.0;
        let (_, excerpt_h) = ctx.text.measure(&excerpt, 12.0, Some(inner_w));
        let row_heights = self.comment_row_heights(ctx.text, ctx.theme, &comments, text_w);
        self.views
            .retain(|id, _| comments.iter().any(|comment| comment.id == *id));

        let header_h = 28.0;
        let gap = 8.0;
        let add_h = 34.0;
        let comments_h = row_heights.iter().sum::<f64>() + gap * comments.len().max(1) as f64;
        let ch = pad + header_h + 8.0 + f64::from(excerpt_h) + 12.0 + comments_h + add_h + pad;

        let layout = CommentCard {
            card: self.card_rect(viewport, cw, ch),
            pad,
            header_h,
            gap,
            add_h,
            text_w,
            color,
            group,
            comments: &comments,
            row_heights: &row_heights,
        };

        let y = self.draw_card_and_header(ctx, cursor, &layout);

        ctx.text.draw_text(
            ctx.scene,
            &excerpt,
            12.0,
            ctx.theme.tokens.colors.text_dim,
            ctx.transform * Affine::translate((layout.ix0(), y)),
            Some(inner_w),
        );
        let y = y + f64::from(excerpt_h) + 12.0;

        let y = self.draw_comment_rows(ctx, cursor, &layout, y);

        self.draw_add_button(ctx, cursor, &layout, y);
    }

    #[cfg(test)]
    pub(crate) fn action_center(&self, pred: impl Fn(CommentAction) -> bool) -> Option<Point> {
        self.hits
            .iter()
            .rev()
            .find(|h| pred(h.action))
            .map(|h| h.rect.center())
    }
}

/// The measured comments card for one highlight group: its rect and
/// spacing, the group's colour and id, and the comments listed in it with
/// their measured row heights.
struct CommentCard<'a> {
    card: Rect,
    pad: f64,
    header_h: f64,
    gap: f64,
    add_h: f64,
    text_w: f32,
    color: HiColor,
    group: Uuid,
    comments: &'a [Annotation],
    row_heights: &'a [f64],
}

impl CommentCard<'_> {
    /// Left edge of the card's padded content.
    fn ix0(&self) -> f64 {
        self.card.x0 + self.pad
    }

    /// Right edge of the card's padded content.
    fn ix1(&self) -> f64 {
        self.card.x1 - self.pad
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use taffy::{NodeId, TaffyTree};
    use ui::theme::Theme;
    use vello::Scene;
    use winit::event::{ElementState, KeyEvent, Modifiers, WindowEvent};
    use winit::keyboard::{Key, NamedKey, PhysicalKey};

    fn key(logical_key: Key) -> WindowEvent {
        let text = match &logical_key {
            Key::Character(text) => Some(text.clone()),
            _ => None,
        };
        let mut event: KeyEvent = unsafe { std::mem::zeroed() };
        event.physical_key =
            PhysicalKey::Unidentified(winit::keyboard::NativeKeyCode::Unidentified);
        event.logical_key = logical_key;
        event.location = winit::keyboard::KeyLocation::Standard;
        event.state = ElementState::Pressed;
        event.repeat = false;
        event.text = text;
        WindowEvent::KeyboardInput {
            device_id: winit::event::DeviceId::dummy(),
            event,
            is_synthetic: false,
        }
    }

    fn annotation(kind: AnnoKind, group: Uuid, note: &str) -> Annotation {
        let mut a = Annotation::new("book", kind, 0, 0).with_group(group);
        a.excerpt = "a highlighted sentence".into();
        a.note = note.into();
        a
    }

    /// Card controls win over the dismiss hit, close clears stale geometry, and the editor writes the comment as typed.
    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "merged scenario test: each labelled phase is one original case"
    )]
    fn comment_overlay_hit_testing_and_editing() {
        // --- phase: card_controls_win_over_the_full_window_dismiss_hit ---
        {
            let group = Uuid::new_v4();
            let comment = annotation(AnnoKind::Note, group, "Useful thought");
            let annotations = [annotation(AnnoKind::Highlight, group, ""), comment.clone()];
            let mut overlay = CommentOverlay::default();
            overlay.open(group);
            overlay.set_anchor(Some((200.0, 180.0, 300.0, 200.0)));
            let mut scene = Scene::new();
            let mut text = TextRenderer::new();
            overlay.draw(
                &mut PaintCtx::new(&mut scene, Affine::IDENTITY, &mut text, &Theme::dark()),
                Rect::new(0.0, 0.0, 800.0, 700.0),
                Point::ZERO,
                &annotations,
                HiColor::Yellow,
            );

            let edit = overlay
                .action_center(|a| a == CommentAction::Edit(comment.id))
                .unwrap();
            assert_eq!(
                overlay.hit_test(edit),
                Some(CommentAction::Edit(comment.id))
            );
            assert_eq!(
                overlay.hit_test(Point::new(4.0, 690.0)),
                Some(CommentAction::Close)
            );
        }

        // --- phase: close_clears_geometry_and_prevents_stale_hits ---
        {
            let group = Uuid::new_v4();
            let annotations = [annotation(AnnoKind::Highlight, group, "")];
            let mut overlay = CommentOverlay::default();
            overlay.open(group);
            let mut scene = Scene::new();
            let mut text = TextRenderer::new();
            overlay.draw(
                &mut PaintCtx::new(&mut scene, Affine::IDENTITY, &mut text, &Theme::dark()),
                Rect::new(0.0, 0.0, 600.0, 500.0),
                Point::ZERO,
                &annotations,
                HiColor::Yellow,
            );
            assert!(overlay.hit_test(Point::ZERO).is_some());
            overlay.begin_frame();
            assert!(
                overlay.hit_test(Point::ZERO).is_none(),
                "a frame that does not draw the overlay cannot retain modal hits"
            );
            overlay.close();
            assert!(!overlay.is_open());
            assert!(overlay.hit_test(Point::ZERO).is_none());
        }

        // --- phase: comment_editor_writes_the_comment_as_typed_and_enter_finishes ---
        {
            let mut doc = AutoCommit::new();
            let comment = annotation(AnnoKind::Note, Uuid::new_v4(), "");
            let id = comment.id;
            crate::doc::BooksDoc::upsert_annotation(&mut doc, &comment);
            doc.commit();
            let mut overlay = CommentOverlay::default();
            overlay.begin_editing(id);
            overlay.sync(&mut doc, &automerge::ROOT);
            let taffy = TaffyTree::new();
            let modifiers = Modifiers::default();
            let press = |overlay: &mut CommentOverlay, doc: &mut AutoCommit, key: WindowEvent| {
                let mut handled = false;
                overlay.handle_event(&mut InputEventCtx {
                    event: &key,
                    scale_factor: 1.0,
                    modifiers,
                    state: doc,
                    root: &automerge::ROOT,
                    node_id: NodeId::new(0),
                    taffy: &taffy,
                    handled: &mut handled,
                })
            };
            for ch in ["o", "k"] {
                assert!(!press(
                    &mut overlay,
                    &mut doc,
                    key(Key::Character(ch.into()))
                ));
            }
            let stored = crate::doc::BooksDoc::list_annotations(&doc)
                .into_iter()
                .find(|annotation| annotation.id == id)
                .unwrap();
            assert_eq!(stored.note, "ok", "written as typed");
            assert!(press(
                &mut overlay,
                &mut doc,
                key(Key::Named(NamedKey::Enter))
            ));
            assert_eq!(overlay.editing_id(), None);
        }
    }
}
