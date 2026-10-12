//! Shared direct-manipulation crop editor for scanner, Photos, and image tools.

use crate::widget::ModelPath;
use std::collections::HashMap;
use std::sync::Arc;

use taffy::{NodeId, Size, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect, RoundedRect, Stroke};
use vello::peniko::Fill;
use winit::event::{ElementState, MouseButton, TouchPhase, WindowEvent};
use winit::keyboard::{Key, NamedKey};

use crate::ColorCompat;
use crate::LocalState;
use crate::a11y::{AccessNode, AccessState, Action, Role};
use crate::presence::PresenceCtx;
use crate::text_render::TextRenderer;
use crate::theme::Theme;
use crate::widget::{Image, ImageFit, TaffyTreeExt, Widget};

/// Normalized source-image crop edges. Values are always clamped to `0..=1`
/// with at least 5% width and height.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CropSelection {
    pub left: f64,
    pub top: f64,
    pub right: f64,
    pub bottom: f64,
}

impl Default for CropSelection {
    fn default() -> Self {
        Self {
            left: 0.0,
            top: 0.0,
            right: 1.0,
            bottom: 1.0,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Edge {
    TopLeft,
    TopRight,
    BottomRight,
    BottomLeft,
}

#[derive(Clone, Copy)]
struct Drag {
    edge: Edge,
    touch_id: Option<u64>,
}

impl Drag {
    fn owns_touch(self, id: u64) -> bool {
        self.touch_id == Some(id)
    }
}

pub struct ImageCropEditor {
    image: Image,
    image_size: (u32, u32),
    selection: LocalState<CropSelection>,
    rect: Rect,
    pointer: Point,
    drag: Option<Drag>,
    focused_edge: Edge,
    focused: bool,
}

impl ImageCropEditor {
    pub fn new(
        rgba: Arc<[u8]>,
        width: u32,
        height: u32,
        selection: LocalState<CropSelection>,
    ) -> Self {
        Self {
            image: Image::from_rgba(rgba, width, height)
                .with_fit(ImageFit::Contain)
                .with_alt("Page being cropped")
                .filling(),
            image_size: (width, height),
            selection,
            rect: Rect::ZERO,
            pointer: Point::ZERO,
            drag: None,
            focused_edge: Edge::TopLeft,
            focused: false,
        }
    }

    /// Paint into an immediate-mode host while retaining the same interaction
    /// geometry as the normal [`Widget`] layout path.
    pub fn paint_at(&mut self, scene: &mut Scene, transform: Affine, theme: &Theme, rect: Rect) {
        self.rect = rect;
        self.image.paint_at(scene, transform, rect);
        self.paint_crop_overlay(scene, transform, theme);
    }

    /// Route input for an immediate-mode host that uses [`Self::paint_at`].
    /// Returns whether the event belongs to the crop editor. Pointer contact
    /// focuses it so subsequent arrow keys nudge the active corner.
    pub fn handle_event_at(&mut self, event: &WindowEvent, scale: f64) -> bool {
        match event {
            WindowEvent::CursorMoved { position, .. } => {
                self.pointer = Point::new(position.x / scale, position.y / scale);
                if let Some(drag) = self.drag
                    && drag.touch_id.is_none()
                {
                    self.update_edge(drag.edge, self.pointer);
                    return true;
                }
            }
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            } => {
                if let Some(edge) = self.edge_at(self.pointer) {
                    self.focused_edge = edge;
                    self.drag = Some(Drag {
                        edge,
                        touch_id: None,
                    });
                    self.focused = true;
                    return true;
                }
            }
            WindowEvent::MouseInput {
                state: ElementState::Released,
                button: MouseButton::Left,
                ..
            } if self.drag.is_some_and(|drag| drag.touch_id.is_none()) => {
                self.drag = None;
                return true;
            }
            WindowEvent::Touch(touch) => {
                let point = Point::new(touch.location.x / scale, touch.location.y / scale);
                match touch.phase {
                    TouchPhase::Started => {
                        if self.drag.is_none()
                            && let Some(edge) = self.edge_at(point)
                        {
                            self.focused_edge = edge;
                            self.drag = Some(Drag {
                                edge,
                                touch_id: Some(touch.id),
                            });
                            self.focused = true;
                            return true;
                        }
                    }
                    TouchPhase::Moved => {
                        if let Some(drag) = self.drag
                            && drag.owns_touch(touch.id)
                        {
                            self.update_edge(drag.edge, point);
                            return true;
                        }
                    }
                    TouchPhase::Ended | TouchPhase::Cancelled => {
                        if self.drag.is_some_and(|drag| drag.owns_touch(touch.id)) {
                            self.drag = None;
                            return true;
                        }
                    }
                }
            }
            WindowEvent::KeyboardInput { event, .. }
                if event.state == ElementState::Pressed
                    && self.focused
                    && matches!(event.logical_key, Key::Named(_)) =>
            {
                if let Key::Named(key) = event.logical_key
                    && self.nudge(key, false)
                {
                    return true;
                }
            }
            _ => {}
        }
        false
    }

    fn image_rect(&self) -> Rect {
        let (width, height) = self.image_size;
        if width == 0 || height == 0 {
            return self.rect;
        }
        let scale =
            (self.rect.width() / f64::from(width)).min(self.rect.height() / f64::from(height));
        let w = f64::from(width) * scale;
        let h = f64::from(height) * scale;
        Rect::from_center_size(self.rect.center(), (w, h))
    }

    fn selection_rect(&self) -> Rect {
        let image = self.image_rect();
        let selection = self.selection.get();
        Rect::new(
            image.x0 + image.width() * selection.left,
            image.y0 + image.height() * selection.top,
            image.x0 + image.width() * selection.right,
            image.y0 + image.height() * selection.bottom,
        )
    }

    fn handles(&self) -> [(Edge, Point); 4] {
        let selection = self.selection_rect();
        [
            (Edge::TopLeft, Point::new(selection.x0, selection.y0)),
            (Edge::TopRight, Point::new(selection.x1, selection.y0)),
            (Edge::BottomRight, Point::new(selection.x1, selection.y1)),
            (Edge::BottomLeft, Point::new(selection.x0, selection.y1)),
        ]
    }

    fn edge_at(&self, point: Point) -> Option<Edge> {
        self.handles().into_iter().find_map(|(edge, center)| {
            Rect::from_center_size(center, (44.0, 44.0))
                .contains(point)
                .then_some(edge)
        })
    }

    fn update_edge(&self, edge: Edge, point: Point) {
        let image = self.image_rect();
        if image.width() <= 0.0 || image.height() <= 0.0 {
            return;
        }
        let x = ((point.x - image.x0) / image.width()).clamp(0.0, 1.0);
        let y = ((point.y - image.y0) / image.height()).clamp(0.0, 1.0);
        self.selection.update(|selection| match edge {
            Edge::TopLeft => {
                selection.left = x.min(selection.right - 0.05);
                selection.top = y.min(selection.bottom - 0.05);
            }
            Edge::TopRight => {
                selection.right = x.max(selection.left + 0.05);
                selection.top = y.min(selection.bottom - 0.05);
            }
            Edge::BottomRight => {
                selection.right = x.max(selection.left + 0.05);
                selection.bottom = y.max(selection.top + 0.05);
            }
            Edge::BottomLeft => {
                selection.left = x.min(selection.right - 0.05);
                selection.bottom = y.max(selection.top + 0.05);
            }
        });
    }

    fn nudge(&self, key: NamedKey, large: bool) -> bool {
        let step = if large { 0.05 } else { 0.01 };
        let rect = self.selection_rect();
        let mut point = self
            .handles()
            .into_iter()
            .find(|(edge, _)| *edge == self.focused_edge)
            .map_or_else(|| rect.center(), |(_, point)| point);
        match key {
            NamedKey::ArrowLeft => point.x -= self.image_rect().width() * step,
            NamedKey::ArrowRight => point.x += self.image_rect().width() * step,
            NamedKey::ArrowUp => point.y -= self.image_rect().height() * step,
            NamedKey::ArrowDown => point.y += self.image_rect().height() * step,
            _ => return false,
        }
        self.update_edge(self.focused_edge, point);
        true
    }

    fn adjust_boundary(&self, target: &str, delta: f64) -> bool {
        let before = self.selection.get();
        self.selection.update(|selection| match target {
            "crop.left" => {
                selection.left = (selection.left + delta).clamp(0.0, selection.right - 0.05);
            }
            "crop.top" => {
                selection.top = (selection.top + delta).clamp(0.0, selection.bottom - 0.05);
            }
            "crop.right" => {
                selection.right = (selection.right + delta).clamp(selection.left + 0.05, 1.0);
            }
            "crop.bottom" => {
                selection.bottom = (selection.bottom + delta).clamp(selection.top + 0.05, 1.0);
            }
            _ => {}
        });
        self.selection.get() != before
    }

    fn paint_crop_overlay(&self, scene: &mut Scene, transform: Affine, theme: &Theme) {
        let image = self.image_rect();
        let crop = self.selection_rect();
        let shade = theme
            .tokens
            .colors
            .surfaces
            .scrim
            .with_alpha_factor(130.0 / 255.0);
        for outside in [
            Rect::new(image.x0, image.y0, image.x1, crop.y0),
            Rect::new(image.x0, crop.y1, image.x1, image.y1),
            Rect::new(image.x0, crop.y0, crop.x0, crop.y1),
            Rect::new(crop.x1, crop.y0, image.x1, crop.y1),
        ] {
            scene.fill(Fill::NonZero, transform, shade, None, &outside);
        }
        scene.stroke(
            &Stroke::new(2.0),
            transform,
            theme.tokens.colors.intent.inverted,
            None,
            &crop,
        );
        for (edge, center) in self.handles() {
            let radius = if self.focused_edge == edge { 7.0 } else { 6.0 };
            let handle = RoundedRect::from_rect(
                Rect::from_center_size(center, (radius * 2.0, radius * 2.0)),
                radius,
            );
            scene.fill(
                Fill::NonZero,
                transform,
                theme.tokens.colors.surfaces.base,
                None,
                &handle,
            );
            scene.stroke(
                &Stroke::new(2.0),
                transform,
                theme.tokens.colors.intent.inverted,
                None,
                &handle,
            );
        }
    }
}

impl Widget for ImageCropEditor {
    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        _theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let style = Style {
            size: Size {
                width: taffy::Dimension::percent(1.0),
                height: taffy::Dimension::percent(1.0),
            },
            flex_grow: 1.0,
            ..Default::default()
        };
        taffy.widget_leaf(style, node_map, self)
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        self.rect = crate::widget::layout_rect(taffy, node, origin);
        self.image.apply_layout(taffy, node, origin);
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        self.image.draw(scene, transform, text, theme, presence);
        self.paint_crop_overlay(scene, transform, theme);
    }

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut crate::widget::EventCtx<'_>) {
        if self.handle_event_at(event, ctx.scale_factor) {
            if self.drag.is_some() {
                *ctx.focus_target = Some(ctx.node_id);
            }
            *ctx.handled = true;
        }
    }

    fn accessibility(&self) -> Option<AccessNode> {
        let selection = self.selection.get();
        let boundary = |name: &str, target: &str, value: f64| {
            AccessNode::new(Role::Slider)
                .with_name(name)
                .with_value(format!("{:.0}%", value * 100.0))
                .with_action_target(target)
                .with_state(AccessState::new().focusable(true))
        };
        Some(
            AccessNode::new(Role::Group)
                .with_name("Crop page")
                .with_description(format!(
                    "From {:.0}% across and {:.0}% down to {:.0}% across and {:.0}% down",
                    selection.left * 100.0,
                    selection.top * 100.0,
                    selection.right * 100.0,
                    selection.bottom * 100.0
                ))
                .with_state(AccessState::new().focusable(true).focused(self.focused))
                .with_children(vec![
                    boundary("Left crop edge", "crop.left", selection.left),
                    boundary("Top crop edge", "crop.top", selection.top),
                    boundary("Right crop edge", "crop.right", selection.right),
                    boundary("Bottom crop edge", "crop.bottom", selection.bottom),
                ]),
        )
    }

    fn perform_semantic_action(
        &mut self,
        target: &str,
        action: &Action,
        _state: &mut crate::widget::Edits<'_>,
        _path: &ModelPath,
    ) -> bool {
        match action {
            Action::Increment => self.adjust_boundary(target, 0.01),
            Action::Decrement => self.adjust_boundary(target, -0.01),
            Action::SetValue(value) => {
                let Ok(mut parsed) = value.trim().trim_end_matches('%').parse::<f64>() else {
                    return false;
                };
                if !parsed.is_finite() {
                    return false;
                }
                if parsed > 1.0 {
                    parsed /= 100.0;
                }
                let current = match target {
                    "crop.left" => self.selection.get().left,
                    "crop.top" => self.selection.get().top,
                    "crop.right" => self.selection.get().right,
                    "crop.bottom" => self.selection.get().bottom,
                    _ => return false,
                };
                self.adjust_boundary(target, parsed.clamp(0.0, 1.0) - current)
            }
            _ => false,
        }
    }

    fn name(&self) -> &'static str {
        "ImageCropEditor"
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }

    fn is_focusable(&self) -> bool {
        true
    }

    fn set_focus(&mut self, focused: bool) {
        self.focused = focused;
        if !focused {
            self.drag = None;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use automerge::AutoCommit;

    #[test]
    fn crop_handles_normalize_and_are_touch_and_assistive_targets() {
        fn direct_manipulation_is_normalized_and_never_inverts_the_crop() {
            let state = LocalState::new(CropSelection::default());
            let mut editor = ImageCropEditor::new(Arc::from(vec![255; 400]), 10, 10, state.clone());
            editor.rect = Rect::new(0.0, 0.0, 200.0, 200.0);
            editor.update_edge(Edge::TopLeft, Point::new(80.0, 60.0));
            let crop = state.get();
            assert_eq!((crop.left, crop.top), (0.4, 0.3));
            editor.update_edge(Edge::TopLeft, Point::new(500.0, 500.0));
            let crop = state.get();
            assert!(crop.left <= crop.right - 0.05);
            assert!(crop.top <= crop.bottom - 0.05);
        }

        fn corner_targets_are_touch_sized() {
            let state = LocalState::new(CropSelection::default());
            let mut editor = ImageCropEditor::new(Arc::from(vec![255; 400]), 10, 10, state);
            editor.rect = Rect::new(0.0, 0.0, 200.0, 200.0);
            assert_eq!(editor.edge_at(Point::new(21.0, 21.0)), Some(Edge::TopLeft));
            assert_eq!(editor.edge_at(Point::new(23.0, 23.0)), None);
        }

        fn assistive_edges_are_individually_actionable_and_bounded() {
            let state = LocalState::new(CropSelection::default());
            let mut editor = ImageCropEditor::new(Arc::from(vec![255; 400]), 10, 10, state.clone());
            let semantics = editor.accessibility().unwrap();
            assert_eq!(semantics.children.len(), 4);
            let mut document = AutoCommit::new();
            let root = automerge::ROOT;
            assert!(editor.perform_semantic_action(
                "crop.left",
                &Action::SetValue("40%".into()),
                &mut crate::widget::Edits::lend(&mut document),
                &root,
            ));
            assert_eq!(state.get().left, 0.4);
            assert!(editor.perform_semantic_action(
                "crop.right",
                &Action::Decrement,
                &mut crate::widget::Edits::lend(&mut document),
                &root,
            ));
            assert!(state.get().right < 1.0);
        }

        fn touch_drag_has_exclusive_finger_ownership() {
            let drag = Drag {
                edge: Edge::BottomRight,
                touch_id: Some(42),
            };
            assert!(drag.owns_touch(42));
            assert!(!drag.owns_touch(7));
            assert!(
                !Drag {
                    edge: Edge::BottomRight,
                    touch_id: None,
                }
                .owns_touch(42)
            );
        }

        direct_manipulation_is_normalized_and_never_inverts_the_crop();
        corner_targets_are_touch_sized();
        assistive_edges_are_individually_actionable_and_bounded();
        touch_drag_has_exclusive_finger_ownership();
    }
}
