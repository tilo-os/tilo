//! Floating comments panel for a highlighted passage.
//!
//! The reader supplies annotations and the current composition id; this
//! component owns the overlay's open/anchor state, paints the panel, and keeps
//! its hit regions private so clicks cannot fall through to the page beneath.

use automerge::{AutoCommit, ObjId};
use ui::theme::Theme;
use uuid::Uuid;
use vello::kurbo::{Point, Rect};

use std::collections::HashMap;

use tilo_rich_text::RichTextView;
use ui::TextRole;
use ui::framework::Dispatcher;
use ui::icons::Icon;
use ui::paint::PaintCtx;
use ui::text_render::TextRenderer;
use ui::widget::{Button, ButtonFace, ChipFace, ChipLead, ControlSize};

use crate::input_controls::{InputEventCtx, NoteEditor};
use crate::model::{AnnoKind, Annotation, HiColor};
use crate::reader_theme::ReaderPalette;
use crate::resource::Ebook;

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
        state: &mut ui::widget::Edits<'_>,
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
    let note = a.note.clone();
    if note.trim().is_empty() {
        "Empty comment".to_string()
    } else {
        note
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
        let pad = f64::from(theme.tokens.spacing.s2);
        let floor = ControlSize::Small.height(theme);
        let empty_line = f64::from(theme.text(TextRole::Body).line_height_px());
        comments
            .iter()
            .map(|a| {
                let text_h = if editing == Some(a.id) {
                    editor_h.unwrap_or(3.0 * empty_line)
                } else if a.note.trim().is_empty() {
                    f64::from(
                        tr.measure_style(
                            &comment_display(a),
                            Some(text_w),
                            &theme.text(TextRole::Body),
                        )
                        .1,
                    )
                } else {
                    let view = self.views.entry(a.id).or_default();
                    view.show(&a.blocks);
                    view.layout(Rect::new(0.0, 0.0, f64::from(text_w), 0.0), theme, tr)
                };
                text_h.max(floor) + 2.0 * pad
            })
            .collect()
    }
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
    pub(crate) fn begin_editing(&mut self, id: Uuid, library: Option<&Dispatcher<Ebook>>) {
        self.editor = Some(NoteEditor::new(id, "Write a comment", library));
    }

    pub(crate) fn finish_editing(&mut self) {
        self.editor = None;
    }

    pub(crate) fn editing_id(&self) -> Option<Uuid> {
        self.editor.as_ref().map(NoteEditor::id)
    }

    /// Bring the comment being written up to date with the document.
    pub(crate) fn sync(&mut self, state: &AutoCommit, root: &ObjId) {
        if let Some(editor) = self.editor.as_mut() {
            editor.sync(state, root);
        }
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

    /// Card rect sized to its content, beside the highlight it belongs to
    /// (below it when there is room, else above), inside `area`.
    fn card_rect(&self, area: Rect, theme: &Theme, cw: f64, ch: f64) -> Rect {
        let (ax, atop, abot) = self.anchor.map_or_else(
            || {
                let c = area.center();
                (c.x, c.y, c.y)
            },
            |(x0, y0, x1, y1)| (f64::midpoint(x0, x1), y0, y1),
        );
        let margin = f64::from(theme.tokens.spacing.s3);
        let gap = f64::from(theme.tokens.spacing.rhythm.related);
        let x0 = (ax - cw / 2.0).clamp(
            area.x0 + margin,
            (area.x1 - margin - cw).max(area.x0 + margin),
        );
        let top = area.y0 + margin;
        let bottom = area.y1 - margin;
        let y0 = if abot + gap + ch <= bottom {
            abot + gap
        } else if atop - gap - ch >= top {
            atop - gap - ch
        } else {
            (bottom - ch).max(top)
        };
        Rect::new(x0, y0, x0 + cw, y0 + ch)
    }

    /// A small plain icon button inside the card, with its private hit.
    fn icon_button(
        &mut self,
        ctx: &mut PaintCtx<'_>,
        cursor: Point,
        rect: Rect,
        (icon, name): (Icon, &str),
        action: CommentAction,
    ) {
        let face = ButtonFace::icon_only(rect, icon, name)
            .control_size(ControlSize::Small)
            .hovered(rect.contains(cursor));
        Button::paint(ctx.scene, ctx.transform, ctx.text, ctx.theme, &face);
        self.push_hit(face.hit_rect(ctx.theme), action);
    }

    /// The card, its header (the highlight's chip and its delete control)
    /// and the passage. Returns the y below the passage.
    fn draw_card_and_header(
        &mut self,
        ctx: &mut PaintCtx<'_>,
        cursor: Point,
        layout: &CommentCard<'_>,
    ) -> f64 {
        let theme = ctx.theme;
        let &CommentCard {
            card,
            pad,
            color,
            group,
            ..
        } = layout;
        self.push_hit(card, CommentAction::Swallow);
        let (ix0, ix1) = (layout.ix0(), layout.ix1());
        let y = card.y0 + pad;
        let lead = ChipLead::Dot(ReaderPalette::highlight_swatch(color));
        let chip_w = ChipFace::intrinsic_width("Highlight", lead, false, ctx.text, theme);
        let line = layout.header_h;
        let chip_h = ChipFace::height(theme);
        let chip = Rect::new(
            ix0,
            y + (line - chip_h) / 2.0,
            ix0 + chip_w,
            y + f64::midpoint(line, chip_h),
        );
        ChipFace::new(chip, "Highlight").lead(lead).paint(
            ctx.scene,
            ctx.transform,
            ctx.text,
            theme,
        );
        let side = ControlSize::Small.height(theme);
        let delete = Rect::new(
            ix1 - side,
            y + (line - side) / 2.0,
            ix1,
            y + f64::midpoint(line, side),
        );
        self.icon_button(
            ctx,
            cursor,
            delete,
            (Icon::Trash2, "Delete highlight"),
            CommentAction::DeleteHighlight(group),
        );
        let y = y + line + layout.gap;
        let style = theme.text(TextRole::Body).with_italic(true);
        ctx.text(
            layout.excerpt,
            &style,
            theme.text_dim,
            Point::new(ix0, y),
            Some(layout.inner_w),
        );
        y + layout.excerpt_h + layout.gap
    }

    /// Each comment as a row (its text, Edit and Delete), rows separated by
    /// a hairline. Returns the y below the last row.
    fn draw_comment_rows(
        &mut self,
        ctx: &mut PaintCtx<'_>,
        cursor: Point,
        layout: &CommentCard<'_>,
        mut y: f64,
    ) -> f64 {
        let theme = ctx.theme;
        let &CommentCard {
            comments,
            row_heights,
            text_w,
            ..
        } = layout;
        let (ix0, ix1) = (layout.ix0(), layout.ix1());
        let pad = f64::from(theme.tokens.spacing.s2);
        let side = ControlSize::Small.height(theme);
        let gap = f64::from(theme.tokens.spacing.rhythm.tight);
        for (i, (annotation, height)) in comments.iter().zip(row_heights).enumerate() {
            let height = *height;
            if i > 0 {
                ui::surface::separator(
                    ctx.scene,
                    ctx.transform,
                    ix0,
                    ix1,
                    y,
                    0.0,
                    theme.tokens.colors.surfaces.content,
                );
            }
            let delete = Rect::new(ix1 - side, y + pad, ix1, y + pad + side);
            let edit = Rect::new(
                delete.x0 - gap - side,
                delete.y0,
                delete.x0 - gap,
                delete.y1,
            );
            let editing = self.editing_id() == Some(annotation.id);
            let text = Rect::new(ix0, y + pad, ix0 + f64::from(text_w), y + pad);
            if let (true, Some(editor)) = (editing, self.editor.as_mut()) {
                editor.layout(text, theme, ctx.text);
                editor.draw(ctx.scene, ctx.transform, ctx.text, theme);
            } else if annotation.note.trim().is_empty() {
                ctx.text_role(
                    &comment_display(annotation),
                    TextRole::Body,
                    theme.text_dim,
                    text.origin(),
                    Some(text_w),
                );
            } else {
                let view = self.views.entry(annotation.id).or_default();
                view.show(&annotation.blocks);
                view.layout(text, theme, ctx.text);
                view.draw(ctx.scene, ctx.transform, ctx.text, theme);
            }
            if !editing {
                self.icon_button(
                    ctx,
                    cursor,
                    edit,
                    (Icon::Pencil, "Edit comment"),
                    CommentAction::Edit(annotation.id),
                );
            }
            self.icon_button(
                ctx,
                cursor,
                delete,
                (Icon::Trash2, "Delete comment"),
                CommentAction::Delete(annotation.id),
            );
            y += height;
        }
        y
    }

    fn draw_add_button(
        &mut self,
        ctx: &mut PaintCtx<'_>,
        cursor: Point,
        layout: &CommentCard<'_>,
        y: f64,
    ) {
        let theme = ctx.theme;
        let size = ControlSize::Small;
        let face = ButtonFace::new(Rect::ZERO, "Add comment")
            .icon(Icon::Plus)
            .control_size(size);
        let width = face.intrinsic_width(ctx.text, theme);
        let add = Rect::new(
            layout.ix0(),
            y,
            layout.ix0() + width,
            y + size.height(theme),
        );
        let face = ButtonFace::new(add, "Add comment")
            .icon(Icon::Plus)
            .control_size(size)
            .hovered(add.contains(cursor));
        Button::paint(ctx.scene, ctx.transform, ctx.text, theme, &face);
        self.push_hit(face.hit_rect(theme), CommentAction::Add(layout.group));
    }

    /// Paint the comments card for the open highlight inside `area` (the
    /// reading column), over a full-window tap-away target.
    pub(crate) fn draw(
        &mut self,
        ctx: &mut PaintCtx<'_>,
        viewport: Rect,
        area: Rect,
        cursor: Point,
        annotations: &[Annotation],
        default_color: HiColor,
    ) {
        self.begin_frame();
        let Some(group) = self.group else { return };

        // Tap-away closes. It is deliberately behind the card and its controls.
        self.push_hit(viewport, CommentAction::Close);

        let (color, excerpt, comments) = gather_content(annotations, group, default_color);
        let base = ctx.theme;
        let theme = base.on_ground(ui::chrome::panel_ground(base));
        let spacing = &theme.tokens.spacing;
        let margin = f64::from(spacing.s3);
        let cw = f64::from(theme.tokens.metrics.panel_width_list).min(area.width() - 2.0 * margin);
        let pad = f64::from(spacing.rhythm.section);
        let gap = f64::from(spacing.rhythm.related);
        let inner_w = ui::num::f32_of_f64((cw - pad * 2.0).max(1.0));
        let controls = 2.0 * ControlSize::Small.height(&theme) + f64::from(spacing.rhythm.tight);
        let text_w = ui::num::f32_of_f64((f64::from(inner_w) - controls - gap).max(1.0));
        let excerpt_style = theme.text(TextRole::Body).with_italic(true);
        let excerpt_h = f64::from(
            ctx.text
                .measure_style(&excerpt, Some(inner_w), &excerpt_style)
                .1,
        );
        let row_heights = self.comment_row_heights(ctx.text, &theme, &comments, text_w);
        self.views
            .retain(|id, _| comments.iter().any(|comment| comment.id == *id));

        let header_h = ChipFace::height(&theme).max(ControlSize::Small.height(&theme));
        let add_h = ControlSize::Small.height(&theme);
        let comments_h = row_heights.iter().sum::<f64>();
        let ch = pad + header_h + gap + excerpt_h + gap + comments_h + gap + add_h + pad;

        let layout = CommentCard {
            card: self.card_rect(area, &theme, cw, ch),
            pad,
            header_h,
            gap,
            inner_w,
            text_w,
            excerpt: &excerpt,
            excerpt_h,
            color,
            group,
            comments: &comments,
            row_heights: &row_heights,
        };
        ui::widget::floating_surface::paint(ctx.scene, ctx.transform, layout.card, base);
        let mut panel = PaintCtx::new(ctx.scene, ctx.transform, ctx.text, &theme);
        let y = self.draw_card_and_header(&mut panel, cursor, &layout);
        let y = self.draw_comment_rows(&mut panel, cursor, &layout, y);
        self.draw_add_button(&mut panel, cursor, &layout, y + gap);
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
/// spacing, the passage, the group's colour and id, and the comments listed
/// in it with their measured row heights.
struct CommentCard<'a> {
    card: Rect,
    pad: f64,
    header_h: f64,
    gap: f64,
    inner_w: f32,
    text_w: f32,
    excerpt: &'a str,
    excerpt_h: f64,
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
    use vello::kurbo::Affine;
    use winit::event::{Modifiers, WindowEvent};
    use winit::keyboard::NamedKey;

    fn annotation(kind: AnnoKind, group: Uuid, note: &str) -> Annotation {
        let mut a = Annotation::new("book", kind, 0, 0).with_group(group);
        a.excerpt = "a highlighted sentence".into();
        a.note = note.into();
        a
    }

    /// Card controls win over the dismiss hit, close clears stale geometry, and the editor writes the comment as typed.
    #[test]
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
            let comment = annotation(AnnoKind::Note, Uuid::new_v4(), "");
            let id = comment.id;
            let mut doc = crate::resource::doc_with(&[], std::slice::from_ref(&comment));
            let library = crate::resource::library_dispatcher_for_test();
            let mut overlay = CommentOverlay::default();
            overlay.begin_editing(id, Some(&library));
            overlay.sync(&doc, &automerge::ROOT);
            let taffy = TaffyTree::new();
            let modifiers = Modifiers::default();
            let press = |overlay: &mut CommentOverlay, doc: &mut AutoCommit, key: WindowEvent| {
                let mut handled = false;
                overlay.handle_event(&mut InputEventCtx {
                    event: &key,
                    scale_factor: 1.0,
                    modifiers,
                    state: ui::widget::Edits::lend(doc),
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
                    ui::synthetic_input::char_key(ch)
                ));
            }
            let stored = crate::resource::read_doc(&doc)
                .annotations
                .values()
                .find(|mark| mark.id == id)
                .map(crate::model::Mark::annotation)
                .unwrap();
            assert_eq!(stored.note, "ok", "written as typed");
            assert!(press(
                &mut overlay,
                &mut doc,
                ui::synthetic_input::named_key(NamedKey::Enter)
            ));
            assert_eq!(overlay.editing_id(), None);
        }
    }
}
