//! `DropZone`: where files can be dropped (an import target, an image to flash, a book to
//! add).

use std::collections::HashMap;
use std::path::PathBuf;

use taffy::style_helpers::{auto, length, percent};
use taffy::{NodeId, Size, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect, RoundedRect, Stroke};
use vello::peniko::Fill;
use winit::event::WindowEvent;

use crate::icons::Icon;
use crate::paint::PaintCtx;

use crate::text_render::{ParagraphAlign, TextRenderer, TextStyle};
use crate::theme::Theme;
use crate::tokens::TextRole;
use crate::widget::DrawChild;
use crate::widget::{Button, TaffyTreeExt, Widget};

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

#[path = "drop_zone_component.rs"]
mod component;
pub use component::DropZoneProps;

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

    #[must_use]
    pub fn drag_over(mut self, over: bool) -> Self {
        self.drag_over = over;
        self
    }

    /// The zone's height at `width`, around its content and `extra` (the height of an
    /// action the caller places under the text, or 0).
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

    /// Paint the zone into `rect`; returns where an action of `extra` height goes under the
    /// text (empty when `extra` is 0).
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
            (colors.intent.accent_ink, m.rule * 2.0)
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
            colors.intent.accent_ink
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

/// A retained drop zone.
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
            action_node: None,
            on_files: None,
            action: None,
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
        self.set_on_files(f);
        self
    }

    pub fn set_on_files(&mut self, f: impl FnMut(Vec<PathBuf>) + 'static) {
        self.on_files = Some(Box::new(f));
    }

    /// For hosts with their own drag tracking: whether a drag hovers.
    pub fn set_drag_over(&mut self, over: bool) {
        if self.drag_over != over {
            self.drag_over = over;
            self.dirty = true;
        }
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
    crate::described!();

    crate::provides!(dyn crate::widget::controls::ActionControls);

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let metrics = ZoneMetrics::new(theme);
        let pad = crate::num::f32_of_f64(metrics.pad);
        self.metrics = Some(metrics);
        self.action_node = self
            .action
            .as_mut()
            .map(|button| button.build_layout(taffy, theme, node_map));
        // Its icon and text stand above an action, which is centred under them: the action's top
        // margin is the height they take at the zone's width, set as the solve lays the zone out.
        let style = Style {
            display: taffy::Display::Flex,
            flex_direction: taffy::FlexDirection::Column,
            align_items: Some(taffy::AlignItems::Center),
            size: Size {
                width: percent(1.0_f32),
                height: auto(),
            },
            min_size: Size {
                width: length(0.0_f32),
                height: auto(),
            },
            padding: taffy::Rect {
                left: length(0.0_f32),
                right: length(0.0_f32),
                top: length(0.0_f32),
                bottom: length(pad),
            },
            flex_shrink: 0.0,
            ..Default::default()
        };
        let node = match self.action_node {
            Some(child) => {
                let node = taffy.parent_node(style, &[child]);
                let _ = taffy.set_restyled_for_width(node, true);
                node
            }
            None => taffy.leaf_node(style),
        };
        node_map.insert(node, std::ptr::from_mut::<dyn Widget>(self));
        node
    }

    /// A zone without an action is as tall as its icon and text at its width.
    fn measure(
        &mut self,
        known: Size<Option<f32>>,
        available: Size<taffy::AvailableSpace>,
        text: &mut TextRenderer,
    ) -> Size<f32> {
        let Some(m) = self.metrics.clone() else {
            return Size::ZERO;
        };
        let face = self.face();
        let width = known
            .width
            .map(f64::from)
            .unwrap_or_else(|| match available.width {
                taffy::AvailableSpace::Definite(width) => f64::from(width),
                _ => f64::from(text.measure_style(face.title, None, &m.title).0) + 2.0 * m.pad,
            });
        let inner = width - 2.0 * m.pad;
        let height =
            2.0 * m.pad + m.icon + m.gap + m.text_height(text, inner, face.title, face.message);
        Size {
            width: crate::num::f32_of_f64(width),
            height: crate::num::f32_of_f64(height.ceil()),
        }
    }

    /// An action stands under the icon and text, as tall as they are at the zone's width.
    fn style_for_width(
        &mut self,
        styles: &mut taffy::Restyle<'_>,
        width: taffy::AvailableSpace,
        text: &mut TextRenderer,
    ) {
        let (Some(m), Some(child)) = (self.metrics.clone(), self.action_node) else {
            return;
        };
        let face = self.face();
        let inner = match width {
            taffy::AvailableSpace::Definite(width) => f64::from(width) - 2.0 * m.pad,
            _ => f64::from(text.measure_style(face.title, None, &m.title).0),
        };
        let above = m.pad + m.icon + m.gap + m.text_height(text, inner, face.title, face.message);
        let mut style = styles.style(child).clone();
        style.margin.top = length(crate::num::f32_of_f64((above + m.gap).ceil()));
        styles.set_style(child, style);
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

    fn draw(&mut self, ctx: &mut crate::draw::DrawCtx<'_>) {
        #[allow(unused_variables, unused_mut, reason = "a draw binds only the parts it uses")]
        let (scene, transform, text_renderer, theme, mut env) = ctx.parts();
        if env.should_cull(self.rect) {
            return;
        }
        let action_h = self.action.as_ref().map_or(0.0, |b| b.get_rect().height());
        self.face().paint(
            &mut PaintCtx::new(scene, transform, text_renderer, theme),
            self.rect,
            action_h,
        );
        if let Some(button) = &mut self.action {
            button.draw_child(scene, transform, text_renderer, theme,&env);
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
                self.cursor = ctx.logical(position);
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

impl crate::widget::controls::ActionControls for DropZone {
    fn control_action_bindings(&self) -> Vec<crate::widget::ControlBindingMetadata> {
        let mut bindings = Vec::new();
        for child in Widget::children(self) {
            bindings.extend(crate::widget::controls::control_action_bindings(child));
        }
        bindings
    }

    fn attach_controls(
        &mut self,
        domain: &crate::ControlDomain,
        attachment: crate::control_domain::DomainAttachment,
    ) {
        for child in Widget::children_mut(self) {
            crate::widget::controls::attach_controls(child, domain, attachment);
        }
    }

    fn take_control_invocation(&mut self) -> Option<crate::tilo_actions::ControlInvocation> {
        Widget::children_mut(self)
            .into_iter()
            .find_map(crate::widget::controls::take_control_invocation)
    }

    fn refresh_control_action_availability(
        &mut self,
        allowed: &std::collections::HashSet<crate::tilo_actions::ActionAddress>,
    ) {
        for child in Widget::children_mut(self) {
            crate::widget::controls::refresh_control_action_availability(child, allowed);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::rc::Rc;

    tilo_test_env::scenario! {
        /// A drop zone.
        /// Each phase was a test of its own and builds its own fixture.
        fn a_drop_zone_shows_settles_and_reports_a_drop;

        phase a_drag_over_takes_your_colour_and_rest_is_quiet;
        {
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

        phase a_zone_with_a_button_settles_with_the_button_centred_under_its_text;
        {
            let zone = DropZone::new("picture", "Drop a picture")
                .with_action(Button::new("picture-choose", "Choose"));
            let mut harness = crate::headless::FrameHarness::new_with_theme(
                crate::State::new(),
                Box::new(zone),
                (320, 568),
                Theme::light(),
            );
            assert_eq!(harness.take_skip_report(), None, "the layout settles");
            fn find<'a>(
                node: &'a crate::headless::InspectionNode,
                name: &str,
            ) -> Option<&'a crate::headless::InspectionNode> {
                if node.widget == name {
                    return Some(node);
                }
                node.children.iter().find_map(|child| find(child, name))
            }
            let inspection = harness.inspection();
            let zone = find(&inspection.root, "DropZone").expect("the zone").rect;
            let button = find(&inspection.root, "Button").expect("its button").rect;
            let (zone_mid, button_mid) = (zone[0] + zone[2] / 2.0, button[0] + button[2] / 2.0);
            assert!(
                (zone_mid - button_mid).abs() <= 1.0,
                "the button {button:?} is centred in the zone {zone:?}"
            );
        }

        phase dropped_files_reach_the_callback;
        {
            let got = Rc::new(RefCell::new(Vec::new()));
            let sink = got.clone();
            let zone = DropZone::new("import", "Drop books here")
                .on_files(move |files| sink.borrow_mut().extend(files));
            let mut harness =
                crate::headless::FrameHarness::new(crate::State::new(), Box::new(zone), (400, 300));
            let [x, y, w, h] = harness.inspection().root.rect;
            harness.click_at(x + w / 2.0, y + h / 2.0);
            harness.send(&WindowEvent::DroppedFile("/home/ada/notes.epub".into()));
            assert_eq!(
                got.borrow().as_slice(),
                [PathBuf::from("/home/ada/notes.epub")]
            );
        }
    }
}
