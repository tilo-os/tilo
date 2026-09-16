//! Floating comments panel for a highlighted passage.
//!
//! The reader supplies annotations and the current composition id; this
//! component owns the overlay's open/anchor state, paints the panel, and keeps
//! its hit regions private so clicks cannot fall through to the page beneath.

use automerge::{AutoCommit, ObjId};
use uuid::Uuid;
use vello::kurbo::{Affine, Point, Rect};
use vello::peniko::Color;

use ui::ColorCompat;
use ui::text_render::{TextRenderer, TextWeight};
use ui::widget::{TextInput, Widget};

use crate::input_controls::{InputEventCtx, InputPaintCtx};
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

pub(crate) enum CommentSemanticOutcome {
    Changed(Uuid, String),
    Focused,
}

#[derive(Clone)]
struct Hit {
    rect: Rect,
    action: CommentAction,
}

pub(crate) struct CommentOverlayView<'a> {
    pub viewport: Rect,
    pub cursor: Point,
    pub annotations: &'a [Annotation],
    pub default_color: HiColor,
}

struct CommentLayout {
    card: Rect,
    pad: f64,
    header_height: f64,
    inner_x0: f64,
    inner_x1: f64,
    inner_width: f32,
    text_width: f32,
    row_gap: f64,
    add_height: f64,
}

pub(crate) struct CommentOverlay {
    group: Option<Uuid>,
    anchor: Option<(f64, f64, f64, f64)>,
    hits: Vec<Hit>,
    editing: Option<Uuid>,
    editor: TextInput,
}

impl Default for CommentOverlay {
    fn default() -> Self {
        Self {
            group: None,
            anchor: None,
            hits: Vec::new(),
            editing: None,
            editor: TextInput::local()
                .with_id("ebook_highlight_comment")
                .with_font_size(13.0)
                .with_plain_chrome()
                .multiline(true)
                .with_enter_submits(true)
                .with_placeholder("Write a comment…"),
        }
    }
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
        if let Some(editing) = self.editing {
            let mut editor = self
                .editor
                .accessibility()
                .unwrap_or_else(|| AccessNode::new(Role::MultilineTextInput));
            editor.name = Some("Comment text".into());
            controls.push(
                editor
                    .with_bounds(self.editor.get_rect())
                    .with_action_target(format!("books.comments.input:{editing}")),
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

    pub(crate) fn perform_editor_semantic_action(
        &mut self,
        target: &str,
        action: &ui::a11y::Action,
        state: &mut AutoCommit,
        root: &ObjId,
    ) -> Option<CommentSemanticOutcome> {
        let id = self.editing?;
        if target != format!("books.comments.input:{id}") {
            return None;
        }
        if !self.editor.perform_action(action, state, root) {
            return None;
        }
        self.editor
            .take_dirty()
            .map_or(Some(CommentSemanticOutcome::Focused), |text| {
                Some(CommentSemanticOutcome::Changed(id, text))
            })
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

/// Wrapped height of each comment row, in reading order.
fn comment_row_heights(
    tr: &mut TextRenderer,
    comments: &[Annotation],
    editing: Option<Uuid>,
    text_w: f32,
) -> Vec<f64> {
    comments
        .iter()
        .map(|a| {
            if editing == Some(a.id) {
                return 76.0;
            }
            f64::from(tr.measure(&comment_display(a), 13.0, Some(text_w)).1).max(18.0) + 16.0
        })
        .collect()
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

    pub(crate) fn begin_editing(&mut self, id: Uuid, text: &str) {
        self.editing = Some(id);
        self.editor.set_text(text);
        self.editor.set_focus(true);
    }

    pub(crate) fn finish_editing(&mut self) {
        self.editing = None;
        self.editor.set_focus(false);
    }

    pub(crate) fn editing_id(&self) -> Option<Uuid> {
        self.editing
    }

    /// Route native editing events through the shared multiline input. The
    /// overlay owns transient editing state and emits only domain-ready text.
    pub(crate) fn handle_event(
        &mut self,
        ctx: &mut InputEventCtx<'_>,
    ) -> (Option<(Uuid, String)>, bool) {
        let Some(id) = self.editing else {
            return (None, false);
        };
        let mut focus_target = None;
        self.editor.handle_event(
            ctx.event,
            ctx.scale_factor,
            &ctx.modifiers,
            ctx.state,
            ctx.root,
            ctx.handled,
            &mut focus_target,
            ctx.node_id,
            ctx.taffy,
        );
        if focus_target.is_some() {
            self.editor.set_focus(true);
        }
        let changed = self.editor.take_dirty().map(|text| (id, text));
        let submitted = self.editor.take_submit();
        if submitted {
            self.finish_editing();
        }
        (changed, submitted)
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
        ctx: &mut InputPaintCtx<'_>,
        cursor: Point,
        layout: &CommentLayout,
        color: HiColor,
        group: Uuid,
    ) -> f64 {
        let colors = &ctx.theme.tokens.colors;
        let shadow = Rect::new(
            layout.card.x0 - 1.0,
            layout.card.y0 + 4.0,
            layout.card.x1 + 1.0,
            layout.card.y1 + 7.0,
        );
        paint::fill_round(
            ctx.scene,
            ctx.transform,
            shadow,
            12.0,
            Color::from_rgba8(0, 0, 0, 80),
        );
        paint::fill_round(
            ctx.scene,
            ctx.transform,
            layout.card,
            f64::from(ctx.theme.tokens.radii.lg),
            colors.surfaces.raised,
        );
        paint::stroke_round(
            ctx.scene,
            ctx.transform,
            layout.card,
            f64::from(ctx.theme.tokens.radii.lg),
            ctx.theme.border_focus,
            1.4,
        );
        self.push_hit(layout.card, CommentAction::Swallow);

        let y = layout.card.y0 + layout.pad;
        paint::circle(
            ctx.scene,
            ctx.transform,
            Point::new(layout.inner_x0 + 6.0, y + 8.0),
            6.0,
            ReaderPalette::highlight_swatch(color),
        );
        ctx.text.draw_text_weighted(
            ctx.scene,
            "Highlight",
            13.5,
            colors.text_primary,
            ctx.transform * Affine::translate((layout.inner_x0 + 20.0, y + 1.0)),
            None,
            TextWeight::Semibold,
        );
        let trash = Rect::new(layout.inner_x1 - 24.0, y - 2.0, layout.inner_x1, y + 22.0);
        let hovered = trash.contains(cursor);
        if hovered {
            paint::fill_round(ctx.scene, ctx.transform, trash, 6.0, ctx.theme.bg_hover);
        }
        paint::trash(
            ctx.scene,
            ctx.transform,
            trash,
            if hovered {
                colors.semantic.danger
            } else {
                colors.text_dim
            },
            1.5,
        );
        self.push_hit(trash, CommentAction::DeleteHighlight(group));
        y + layout.header_height
    }

    /// Draws each comment row (text, delete/edit affordances) and records
    /// their hits. Returns the y-coordinate immediately below the last row.
    fn draw_comment_rows(
        &mut self,
        ctx: &mut InputPaintCtx<'_>,
        cursor: Point,
        layout: &CommentLayout,
        mut y: f64,
        comments: &[Annotation],
        row_heights: &[f64],
    ) -> f64 {
        let colors = &ctx.theme.tokens.colors;
        for (annotation, height) in comments.iter().zip(row_heights) {
            let height = *height;
            let row = Rect::new(layout.inner_x0, y, layout.inner_x1, y + height);
            paint::fill_round(ctx.scene, ctx.transform, row, 8.0, colors.surfaces.overlay);
            let editing = self.editing == Some(annotation.id);
            if editing {
                paint::stroke_round(
                    ctx.scene,
                    ctx.transform,
                    row,
                    8.0,
                    ctx.theme.border_focus,
                    1.4,
                );
            }
            if editing {
                self.editor.apply_layout_external(Rect::new(
                    layout.inner_x0 + 8.0,
                    y + 6.0,
                    layout.inner_x1 - 52.0,
                    y + height - 6.0,
                ));
                self.editor.draw(
                    ctx.scene,
                    ctx.transform,
                    ctx.text,
                    ctx.theme,
                    &ui::presence::PresenceCtx::empty(),
                );
            } else {
                let text = comment_display(annotation);
                ctx.text.draw_text(
                    ctx.scene,
                    &text,
                    13.0,
                    if annotation.note.trim().is_empty() {
                        colors.text_dim
                    } else {
                        colors.text_primary
                    },
                    ctx.transform * Affine::translate((layout.inner_x0 + 10.0, y + 8.0)),
                    Some(layout.text_width),
                );
            }
            let delete = Rect::new(
                layout.inner_x1 - 24.0,
                y + 4.0,
                layout.inner_x1 - 2.0,
                y + 26.0,
            );
            let delete_hover = delete.contains(cursor);
            paint::close(
                ctx.scene,
                ctx.transform,
                delete,
                if delete_hover {
                    colors.semantic.danger
                } else {
                    colors.text_dim.with_alpha_factor(0.7)
                },
                1.4,
            );
            self.push_hit(delete, CommentAction::Delete(annotation.id));
            let edit = Rect::new(
                layout.inner_x1 - 48.0,
                y + 4.0,
                layout.inner_x1 - 26.0,
                y + 26.0,
            );
            paint::pencil(
                ctx.scene,
                ctx.transform,
                edit,
                if edit.contains(cursor) {
                    colors.text_primary
                } else {
                    colors.text_dim.with_alpha_factor(0.7)
                },
                1.4,
            );
            if !editing {
                self.push_hit(edit, CommentAction::Edit(annotation.id));
            }
            y += height + layout.row_gap;
        }
        y
    }

    /// Draws the "Add comment" button and records its hit.
    fn draw_add_button(
        &mut self,
        ctx: &mut InputPaintCtx<'_>,
        cursor: Point,
        layout: &CommentLayout,
        y: f64,
        group: Uuid,
    ) {
        let add = Rect::new(
            layout.inner_x0,
            y,
            layout.inner_x1,
            y + layout.add_height - 4.0,
        );
        let add_hover = add.contains(cursor);
        paint::fill_round(
            ctx.scene,
            ctx.transform,
            add,
            8.0,
            if add_hover {
                ctx.theme.accent_primary
            } else {
                ctx.theme.accent_primary.with_alpha_factor(0.14)
            },
        );
        ctx.text.draw_text_weighted(
            ctx.scene,
            "＋  Add comment",
            12.5,
            if add_hover {
                Color::from_rgb8(248, 248, 252)
            } else {
                ctx.theme.accent_primary
            },
            ctx.transform * Affine::translate((layout.inner_x0 + 12.0, y + 8.0)),
            None,
            TextWeight::Semibold,
        );
        self.push_hit(add, CommentAction::Add(group));
    }

    pub(crate) fn draw(&mut self, ctx: &mut InputPaintCtx<'_>, view: &CommentOverlayView<'_>) {
        self.begin_frame();
        let Some(group) = self.group else { return };

        // Tap-away closes. It is deliberately behind the card and its controls.
        self.push_hit(view.viewport, CommentAction::Close);

        let (color, excerpt, comments) =
            gather_content(view.annotations, group, view.default_color);

        let cw = 360.0_f64.min(view.viewport.width() - 24.0);
        let pad = 16.0;
        let inner_w = (cw - pad * 2.0) as f32;
        let text_w = inner_w - 52.0;
        let (_, excerpt_h) = ctx.text.measure(&excerpt, 12.0, Some(inner_w));
        let row_heights = comment_row_heights(ctx.text, &comments, self.editing, text_w);

        let header_h = 28.0;
        let gap = 8.0;
        let add_h = 34.0;
        let comments_h = row_heights.iter().sum::<f64>() + gap * comments.len().max(1) as f64;
        let ch = pad + header_h + 8.0 + f64::from(excerpt_h) + 12.0 + comments_h + add_h + pad;

        let card = self.card_rect(view.viewport, cw, ch);
        let layout = CommentLayout {
            card,
            pad,
            header_height: header_h,
            inner_x0: card.x0 + pad,
            inner_x1: card.x1 - pad,
            inner_width: inner_w,
            text_width: text_w,
            row_gap: gap,
            add_height: add_h,
        };

        let y = self.draw_card_and_header(ctx, view.cursor, &layout, color, group);

        ctx.text.draw_text(
            ctx.scene,
            &excerpt,
            12.0,
            ctx.theme.tokens.colors.text_dim,
            ctx.transform * Affine::translate((layout.inner_x0, y)),
            Some(layout.inner_width),
        );
        let y = y + f64::from(excerpt_h) + 12.0;

        let y = self.draw_comment_rows(ctx, view.cursor, &layout, y, &comments, &row_heights);

        self.draw_add_button(ctx, view.cursor, &layout, y, group);
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

    #[test]
    fn card_controls_win_over_the_full_window_dismiss_hit() {
        let group = Uuid::new_v4();
        let comment = annotation(AnnoKind::Note, group, "Useful thought");
        let annotations = [annotation(AnnoKind::Highlight, group, ""), comment.clone()];
        let mut overlay = CommentOverlay::default();
        overlay.open(group);
        overlay.set_anchor(Some((200.0, 180.0, 300.0, 200.0)));
        let mut scene = Scene::new();
        let mut text = TextRenderer::new();
        overlay.draw(
            &mut InputPaintCtx {
                scene: &mut scene,
                transform: Affine::IDENTITY,
                text: &mut text,
                theme: &Theme::dark(),
            },
            &CommentOverlayView {
                viewport: Rect::new(0.0, 0.0, 800.0, 700.0),
                cursor: Point::ZERO,
                annotations: &annotations,
                default_color: HiColor::Yellow,
            },
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

    #[test]
    fn close_clears_geometry_and_prevents_stale_hits() {
        let group = Uuid::new_v4();
        let annotations = [annotation(AnnoKind::Highlight, group, "")];
        let mut overlay = CommentOverlay::default();
        overlay.open(group);
        let mut scene = Scene::new();
        let mut text = TextRenderer::new();
        overlay.draw(
            &mut InputPaintCtx {
                scene: &mut scene,
                transform: Affine::IDENTITY,
                text: &mut text,
                theme: &Theme::dark(),
            },
            &CommentOverlayView {
                viewport: Rect::new(0.0, 0.0, 600.0, 500.0),
                cursor: Point::ZERO,
                annotations: &annotations,
                default_color: HiColor::Yellow,
            },
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

    #[test]
    fn comment_editor_uses_shared_selection_editing_and_submit() {
        let id = Uuid::new_v4();
        let mut overlay = CommentOverlay::default();
        overlay.begin_editing(id, "hello");
        overlay.editor.select_all();
        let mut doc = AutoCommit::new();
        let taffy = TaffyTree::new();
        let modifiers = Modifiers::default();
        let mut handled = false;

        let (changed, submitted) = overlay.handle_event(&mut InputEventCtx {
            event: &key(Key::Character("world".into())),
            scale_factor: 1.0,
            modifiers,
            state: &mut doc,
            root: &automerge::ROOT,
            node_id: NodeId::new(0),
            taffy: &taffy,
            handled: &mut handled,
        });
        assert_eq!(changed, Some((id, "world".into())));
        assert!(!submitted);

        let (_, submitted) = overlay.handle_event(&mut InputEventCtx {
            event: &key(Key::Named(NamedKey::Enter)),
            scale_factor: 1.0,
            modifiers,
            state: &mut doc,
            root: &automerge::ROOT,
            node_id: NodeId::new(0),
            taffy: &taffy,
            handled: &mut handled,
        });
        assert!(submitted);
        assert_eq!(overlay.editing_id(), None);
    }
}
