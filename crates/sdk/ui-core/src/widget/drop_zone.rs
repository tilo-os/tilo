//! `DropZone`: where files can be dropped (an import target, an image to
//! flash, a book to add).
//!
//! At rest the zone is a dashed edge derived from its ground around a
//! centred icon, title and an optional line saying what it accepts. Only
//! while a drag hovers it does it take colour: your colour on the edge and
//! the text-selection tint inside, the same as every drop indicator. An
//! optional action ("Choose files") offers the same route without dragging.
//!
//! [`DropZoneFace`] paints the same geometry for immediate-mode callers.

use std::collections::HashMap;
use std::path::PathBuf;

use taffy::style_helpers::{auto, length, percent};
use taffy::{NodeId, Size, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect, RoundedRect, Stroke};
use vello::peniko::Fill;
use winit::event::WindowEvent;

use crate::dnd::DragPayload;
use crate::icons::Icon;
use crate::paint::PaintCtx;
use crate::presence::PresenceCtx;
use crate::text_render::{ParagraphAlign, TextRenderer, TextStyle};
use crate::theme::Theme;
use crate::tokens::TextRole;
use crate::widget::{Button, TaffyTreeExt, Widget};

/// The resolved geometry of a drop zone in `theme`.
#[derive(Clone, Debug)]
struct ZoneMetrics {
    title: TextStyle,
    message: TextStyle,
    icon: f64,
    pad: f64,
    gap: f64,
    text_gap: f64,
    radius: f64,
    dash: f64,
    rule: f64,
}

impl ZoneMetrics {
    fn new(theme: &Theme) -> Self {
        let spacing = &theme.tokens.spacing;
        Self {
            title: theme
                .text(TextRole::Subheading)
                .with_align(ParagraphAlign::Center),
            message: theme
                .text(TextRole::Caption)
                .with_align(ParagraphAlign::Center),
            icon: f64::from(theme.tokens.metrics.icon_lg),
            pad: f64::from(spacing.s6),
            gap: f64::from(spacing.s3),
            text_gap: f64::from(spacing.rhythm.tight),
            radius: f64::from(theme.tokens.radii.tile),
            dash: f64::from(spacing.s1),
            rule: f64::from(theme.tokens.metrics.chrome_rule_px),
        }
    }

    fn text_height(
        &self,
        text: &mut TextRenderer,
        width: f64,
        title: &str,
        message: Option<&str>,
    ) -> f64 {
        let wrap = Some(crate::num::f32_of_f64(width.max(1.0)));
        let mut h = f64::from(text.measure_style(title, wrap, &self.title).1);
        if let Some(message) = message {
            h += self.text_gap + f64::from(text.measure_style(message, wrap, &self.message).1);
        }
        h
    }
}

/// Immediate-mode drop zone.
#[derive(Clone, Copy, Debug)]
pub struct DropZoneFace<'a> {
    title: &'a str,
    message: Option<&'a str>,
    icon: Icon,
    drag_over: bool,
}

impl<'a> DropZoneFace<'a> {
    #[must_use]
    pub fn new(title: &'a str) -> Self {
        Self {
            title,
            message: None,
            icon: Icon::Download,
            drag_over: false,
        }
    }

    /// What the zone accepts ("PNG, JPEG or WebP up to 50 MB").
    #[must_use]
    pub fn message(mut self, message: &'a str) -> Self {
        self.message = Some(message);
        self
    }

    #[must_use]
    pub fn icon(mut self, icon: Icon) -> Self {
        self.icon = icon;
        self
    }

    /// A drag is hovering over the zone.
    #[must_use]
    pub fn drag_over(mut self, over: bool) -> Self {
        self.drag_over = over;
        self
    }

    /// The zone's height at `width`, around its content and `extra` (the
    /// height of an action the caller places under the text, or 0).
    pub fn height(&self, text: &mut TextRenderer, theme: &Theme, width: f64, extra: f64) -> f64 {
        let m = ZoneMetrics::new(theme);
        let inner = width - 2.0 * m.pad;
        let mut h =
            2.0 * m.pad + m.icon + m.gap + m.text_height(text, inner, self.title, self.message);
        if extra > 0.0 {
            h += m.gap + extra;
        }
        h.ceil()
    }

    /// Paint the zone into `rect`; returns where an action of `extra`
    /// height goes under the text (empty when `extra` is 0).
    pub fn paint(&self, ctx: &mut PaintCtx<'_>, rect: Rect, extra: f64) -> Rect {
        if rect.width() <= 0.0 || rect.height() <= 0.0 {
            return Rect::ZERO;
        }
        let theme = ctx.theme;
        let colors = &theme.tokens.colors;
        let m = ZoneMetrics::new(theme);
        let ground = colors.surfaces.content;
        let shape = RoundedRect::from_rect(rect.inset(-m.rule), m.radius);
        let (edge, width) = if self.drag_over {
            ctx.scene.fill(
                Fill::NonZero,
                ctx.transform,
                colors.intent.selection_tint,
                None,
                &shape,
            );
            (colors.intent.you, m.rule * 2.0)
        } else {
            (crate::surface::edge_on(ground), m.rule)
        };
        ctx.scene.stroke(
            &Stroke::new(width).with_dashes(0.0, [m.dash * 2.0, m.dash]),
            ctx.transform,
            edge,
            None,
            &shape,
        );

        let inner = (rect.width() - 2.0 * m.pad).max(1.0);
        let text_h = m.text_height(ctx.text, inner, self.title, self.message);
        let mut block = m.icon + m.gap + text_h;
        if extra > 0.0 {
            block += m.gap + extra;
        }
        let mut y = rect.center().y - block / 2.0;
        let icon_ink = if self.drag_over {
            colors.intent.you
        } else {
            theme.text_tertiary
        };
        self.icon.draw(
            ctx.scene,
            ctx.transform,
            Rect::from_center_size(
                Point::new(rect.center().x, y + m.icon / 2.0),
                (m.icon, m.icon),
            ),
            icon_ink,
        );
        y += m.icon + m.gap;
        let wrap = Some(crate::num::f32_of_f64(inner));
        let x = rect.x0 + m.pad;
        ctx.text(
            self.title,
            &m.title,
            theme.text_primary,
            Point::new(x, y),
            wrap,
        );
        y += f64::from(ctx.text.measure_style(self.title, wrap, &m.title).1);
        if let Some(message) = self.message {
            y += m.text_gap;
            ctx.text(message, &m.message, theme.text_dim, Point::new(x, y), wrap);
            y += f64::from(ctx.text.measure_style(message, wrap, &m.message).1);
        }
        if extra > 0.0 {
            y += m.gap;
            Rect::new(rect.x0 + m.pad, y, rect.x1 - m.pad, y + extra)
        } else {
            Rect::ZERO
        }
    }
}

/// A retained drop zone. See the module docs.
pub struct DropZone {
    id: String,
    title: String,
    message: Option<String>,
    icon: Icon,
    action: Option<Button>,
    action_node: Option<NodeId>,
    on_files: Option<Box<dyn FnMut(Vec<PathBuf>)>>,
    drag_over: bool,
    cursor: Point,
    metrics: Option<ZoneMetrics>,
    rect: Rect,
    dirty: bool,
}

impl DropZone {
    pub fn new(id: impl Into<String>, title: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            title: title.into(),
            message: None,
            icon: Icon::Download,
            action: None,
            action_node: None,
            on_files: None,
            drag_over: false,
            cursor: Point::ZERO,
            metrics: None,
            rect: Rect::ZERO,
            dirty: true,
        }
    }

    /// See [`DropZoneFace::message`].
    #[must_use]
    pub fn message(mut self, message: impl Into<String>) -> Self {
        self.message = Some(message.into());
        self
    }

    #[must_use]
    pub fn icon(mut self, icon: Icon) -> Self {
        self.icon = icon;
        self
    }

    /// The same route without dragging ("Choose files"), under the text.
    #[must_use]
    pub fn with_action(mut self, button: Button) -> Self {
        self.action = Some(button);
        self
    }

    /// Receive the dropped files.
    #[must_use]
    pub fn on_files(mut self, f: impl FnMut(Vec<PathBuf>) + 'static) -> Self {
        self.on_files = Some(Box::new(f));
        self
    }

    /// For hosts with their own drag tracking: whether a drag hovers.
    pub fn set_drag_over(&mut self, over: bool) {
        if self.drag_over != over {
            self.drag_over = over;
            self.dirty = true;
        }
    }

    pub fn is_drag_over(&self) -> bool {
        self.drag_over
    }

    fn face(&self) -> DropZoneFace<'_> {
        let mut face = DropZoneFace::new(&self.title)
            .icon(self.icon)
            .drag_over(self.drag_over);
        if let Some(message) = &self.message {
            face = face.message(message);
        }
        face
    }

    fn deliver(&mut self, files: Vec<PathBuf>) {
        if let Some(on_files) = &mut self.on_files {
            on_files(files);
        }
    }
}

impl Widget for DropZone {
    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        self.metrics = Some(ZoneMetrics::new(theme));
        self.action_node = self
            .action
            .as_mut()
            .map(|button| button.build_layout(taffy, theme, node_map));
        let style = Style {
            size: Size {
                width: percent(1.0_f32),
                height: auto(),
            },
            min_size: Size {
                width: length(0.0_f32),
                height: auto(),
            },
            flex_shrink: 0.0,
            ..Default::default()
        };
        let node = match self.action_node {
            Some(child) => {
                // The button is placed absolutely under the text in
                // `adjust_layout_with_measure`.
                if let Ok(current) = taffy.style(child) {
                    let mut child_style = current.clone();
                    child_style.position = taffy::Position::Absolute;
                    let _ = taffy.set_style(child, child_style);
                }
                taffy.parent_node(style, &[child])
            }
            None => taffy.leaf_node(style),
        };
        node_map.insert(node, std::ptr::from_mut::<dyn Widget>(self));
        node
    }

    fn adjust_layout_with_measure(
        &mut self,
        taffy: &mut TaffyTree<()>,
        node: NodeId,
        text: &mut TextRenderer,
    ) -> bool {
        let (Some(m), Ok(layout)) = (self.metrics.clone(), taffy.layout(node).copied()) else {
            return false;
        };
        let width = f64::from(layout.size.width);
        let action_h = self
            .action_node
            .and_then(|n| taffy.layout(n).ok())
            .map_or(0.0, |l| f64::from(l.size.height));
        let action_w = self
            .action_node
            .and_then(|n| taffy.layout(n).ok())
            .map_or(0.0, |l| f64::from(l.size.width));
        let mut changed = false;
        let face = self.face();
        let inner = width - 2.0 * m.pad;
        let mut h =
            2.0 * m.pad + m.icon + m.gap + m.text_height(text, inner, face.title, face.message);
        if action_h > 0.0 {
            h += m.gap + action_h;
        }
        let h = crate::num::f32_of_f64(h.ceil());
        if let Ok(current) = taffy.style(node)
            && current.size.height != length(h)
        {
            let mut style = current.clone();
            style.size.height = length(h);
            changed |= taffy.set_style(node, style).is_ok();
        }
        if let Some(child) = self.action_node
            && let Ok(current) = taffy.style(child)
        {
            let top = f64::from(h) - m.pad - action_h;
            let left = (width - action_w) / 2.0;
            let inset = taffy::Rect {
                left: length(crate::num::f32_of_f64(left)),
                top: length(crate::num::f32_of_f64(top)),
                right: auto(),
                bottom: auto(),
            };
            if current.inset != inset {
                let mut style = current.clone();
                style.inset = inset;
                changed |= taffy.set_style(child, style).is_ok();
            }
        }
        changed
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        let rect = crate::widget::layout_rect(taffy, node, origin);
        if rect != self.rect {
            self.dirty = true;
        }
        self.rect = rect;
        if let (Some(button), Some(child)) = (&mut self.action, self.action_node) {
            button.apply_layout(taffy, child, rect.origin());
        }
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        if presence.should_cull(self.rect) {
            return;
        }
        let action_h = self.action.as_ref().map_or(0.0, |b| b.get_rect().height());
        self.face().paint(
            &mut PaintCtx::new(scene, transform, text_renderer, theme),
            self.rect,
            action_h,
        );
        if let Some(button) = &mut self.action {
            button.draw(scene, transform, text_renderer, theme, presence);
        }
    }

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut crate::widget::EventCtx<'_>) {
        if let (Some(button), Some(child)) = (&mut self.action, self.action_node) {
            button.handle_event(event, &mut ctx.reborrow(ctx.path, child));
            if *ctx.handled {
                return;
            }
        }
        match event {
            WindowEvent::CursorMoved { position, .. } => {
                self.cursor =
                    Point::new(position.x / ctx.scale_factor, position.y / ctx.scale_factor);
            }
            WindowEvent::HoveredFile(_) => {
                self.set_drag_over(self.rect.contains(self.cursor));
            }
            WindowEvent::HoveredFileCancelled => self.set_drag_over(false),
            WindowEvent::DroppedFile(path) => {
                if self.drag_over || self.rect.contains(self.cursor) {
                    self.deliver(vec![path.clone()]);
                    *ctx.handled = true;
                }
                self.set_drag_over(false);
            }
            _ => {}
        }
    }

    fn accepts_drop(&self, payload: &DragPayload) -> bool {
        matches!(payload, DragPayload::File { .. })
    }

    fn on_drop(
        &mut self,
        payload: DragPayload,
        _state: &mut crate::widget::Edits<'_>,
        _path: &crate::widget::ModelPath,
    ) {
        if let DragPayload::File { path } = payload {
            self.deliver(vec![PathBuf::from(path)]);
        }
        self.set_drag_over(false);
    }

    fn children(&self) -> Vec<&dyn Widget> {
        self.action
            .as_ref()
            .map(|b| b as &dyn Widget)
            .into_iter()
            .collect()
    }

    fn for_each_child(&self, visitor: &mut dyn FnMut(&dyn Widget)) {
        if let Some(button) = &self.action {
            visitor(button);
        }
    }

    fn children_mut(&mut self) -> Vec<&mut (dyn Widget + '_)> {
        self.action
            .as_mut()
            .map(|b| b as &mut (dyn Widget + '_))
            .into_iter()
            .collect()
    }

    fn needs_redraw(&self) -> bool {
        self.dirty || self.action.as_ref().is_some_and(Widget::needs_redraw)
    }

    fn mark_drawn(&mut self) {
        self.dirty = false;
        if let Some(button) = &mut self.action {
            button.mark_drawn();
        }
    }

    fn is_layout_dirty(&self) -> bool {
        self.rect == Rect::ZERO
    }

    fn name(&self) -> &'static str {
        "DropZone"
    }

    fn test_id(&self) -> Option<&str> {
        Some(&self.id)
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }

    fn accessibility(&self) -> Option<crate::a11y::AccessNode> {
        use crate::a11y::{AccessNode, Role};
        let mut node = AccessNode::new(Role::Group).with_name(self.title.clone());
        if let Some(message) = &self.message {
            node = node.with_description(message.clone());
        }
        Some(node)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::rc::Rc;

    #[test]
    fn a_drag_over_takes_your_colour_and_rest_is_quiet() {
        let theme = Theme::dark();
        let mut text = TextRenderer::new();
        let rect = Rect::new(0.0, 0.0, 400.0, 200.0);
        let paint = |face: DropZoneFace<'_>, text: &mut TextRenderer| {
            let mut scene = Scene::new();
            face.paint(
                &mut PaintCtx::new(&mut scene, Affine::IDENTITY, text, &theme),
                rect,
                0.0,
            );
            scene.encoding().draw_tags.len()
        };
        let face = DropZoneFace::new("Drop an image to flash").message("ISO or IMG");
        let rest = paint(face, &mut text);
        let over = paint(face.drag_over(true), &mut text);
        assert_eq!(over, rest + 1, "only the tint fill is added");
        let height = face.height(&mut text, &theme, 400.0, 0.0);
        let with_action = face.height(&mut text, &theme, 400.0, 32.0);
        assert!(with_action > height + 32.0);
        assert!(face.height(&mut text, &Theme::dark().with_text_scale(2.0), 400.0, 0.0) > height);
    }

    #[test]
    fn dropped_files_reach_the_callback() {
        let got = Rc::new(RefCell::new(Vec::new()));
        let sink = got.clone();
        let mut zone = DropZone::new("import", "Drop books here")
            .on_files(move |files| sink.borrow_mut().extend(files));
        assert!(zone.accepts_drop(&DragPayload::File {
            path: "/home/ada/notes.epub".into()
        }));
        assert!(!zone.accepts_drop(&DragPayload::Text {
            value: "hello".into()
        }));
        let mut doc = automerge::AutoCommit::new();
        zone.on_drop(
            DragPayload::File {
                path: "/home/ada/notes.epub".into(),
            },
            &mut crate::widget::Edits::lend(&mut doc),
            &automerge::ROOT,
        );
        assert_eq!(
            got.borrow().as_slice(),
            [PathBuf::from("/home/ada/notes.epub")]
        );
    }
}
