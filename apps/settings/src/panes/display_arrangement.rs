//! Drag-to-arrange canvas for the Displays pane.
//!
//! Each attached, enabled display is drawn at its size on the shared desktop
//! (mode, rotation and scale), numbered the way the compositor numbers it
//! when asked to identify screens. Dragging one snaps it to nearby edges,
//! pushes it clear of the others and keeps it touching at least one of
//! them ([`placement`]); the drop is sent as the output's exported `move`
//! action, so a drag is exactly what an agent or script can do. The
//! per-display position dropdowns stay the keyboard and assistive
//! alternative.

pub mod placement;

use std::collections::HashMap;

use taffy::{Dimension, NodeId, Size, Style, TaffyTree};
use ui::presence::PresenceCtx;
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::widget::{ActionEmitter, Widget};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect, RoundedRect, Stroke};
use vello::peniko::Fill;
use winit::event::{ElementState, MouseButton, WindowEvent};
use winit::keyboard::{Key, NamedKey};

use placement::Placement;

use crate::i18n::{t, t_args};

/// The canvas's width over its height; the desktop is scaled to fit inside
/// it. A drawing area sized for a desk of monitors.
const CANVAS_ASPECT: f32 = 3.2;
/// Inner margin between the canvas edge and the desktop's bounding box.
const CANVAS_PAD: f64 = ui::tokens::Spacing::BASE.s6 as f64;
/// Snap distance on screen (the `s3` step), converted to desktop units at
/// the canvas scale.
const SNAP_SCREEN_PX: f64 = ui::tokens::Spacing::BASE.s3 as f64;

/// One display as the canvas draws it.
#[derive(Clone, Debug, PartialEq)]
pub struct ArrangedDisplay {
    pub connector: String,
    pub label: String,
    /// The number Identify shows on this display.
    pub number: usize,
    pub placement: Placement,
    pub primary: bool,
    /// Mirrors sit on their target and follow it, so only extended displays
    /// can be dragged.
    pub movable: bool,
}

/// An in-progress drag. Owned by the pane, not the widget, so it survives
/// the composition being rebuilt between frames.
#[derive(Clone, Debug, PartialEq)]
pub struct ArrangementDrag {
    connector: String,
    /// Desktop-to-canvas mapping frozen at press time, so the canvas does
    /// not rescale under the pointer while the bounding box changes.
    mapping: Mapping,
    grab: (f64, f64),
    pressed_at: Point,
    active: bool,
    preview: Placement,
}

/// Uniform scale plus offset from desktop units to canvas pixels.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Mapping {
    scale: f64,
    origin: Point,
    desktop_x: i32,
    desktop_y: i32,
}

impl Mapping {
    fn fit(canvas: Rect, displays: &[ArrangedDisplay]) -> Self {
        let min_x = displays.iter().map(|d| d.placement.x).min().unwrap_or(0);
        let min_y = displays.iter().map(|d| d.placement.y).min().unwrap_or(0);
        let max_x = displays
            .iter()
            .map(|d| d.placement.right())
            .max()
            .unwrap_or(1920);
        let max_y = displays
            .iter()
            .map(|d| d.placement.bottom())
            .max()
            .unwrap_or(1080);
        let width = f64::from(max_x.saturating_sub(min_x).max(1));
        let height = f64::from(max_y.saturating_sub(min_y).max(1));
        let inner = canvas.inset(-CANVAS_PAD);
        let scale = (inner.width() / width)
            .min(inner.height() / height)
            .max(f64::EPSILON);
        let origin = Point::new(
            inner.center().x - width * scale / 2.0,
            inner.center().y - height * scale / 2.0,
        );
        Self {
            scale,
            origin,
            desktop_x: min_x,
            desktop_y: min_y,
        }
    }

    fn to_canvas(self, placement: Placement) -> Rect {
        let x0 = self.origin.x + f64::from(placement.x.saturating_sub(self.desktop_x)) * self.scale;
        let y0 = self.origin.y + f64::from(placement.y.saturating_sub(self.desktop_y)) * self.scale;
        Rect::new(
            x0,
            y0,
            x0 + f64::from(placement.width) * self.scale,
            y0 + f64::from(placement.height) * self.scale,
        )
    }

    fn to_desktop(self, point: Point) -> (f64, f64) {
        (
            (point.x - self.origin.x) / self.scale + f64::from(self.desktop_x),
            (point.y - self.origin.y) / self.scale + f64::from(self.desktop_y),
        )
    }

    fn snap_threshold(self) -> i32 {
        round_to_i32(SNAP_SCREEN_PX / self.scale)
    }
}

#[allow(
    clippy::cast_possible_truncation,
    reason = "clamped to the i32 range before the cast"
)]
fn round_to_i32(value: f64) -> i32 {
    value
        .round()
        .clamp(f64::from(i32::MIN), f64::from(i32::MAX)) as i32
}

#[allow(clippy::cast_possible_truncation, reason = "canvas pixels fit f32")]
fn px(value: f64) -> f32 {
    value as f32
}

pub struct DisplayArrangement {
    displays: Vec<ArrangedDisplay>,
    moves: Vec<(String, ActionEmitter<(i32, i32)>)>,
    drag: ui::LocalState<Option<ArrangementDrag>>,
    cursor: Option<Point>,
    rect: Rect,
}

impl DisplayArrangement {
    pub fn new(
        displays: Vec<ArrangedDisplay>,
        moves: Vec<(String, tilo_actions::ActionBinding<(i32, i32)>)>,
        drag: ui::LocalState<Option<ArrangementDrag>>,
    ) -> Self {
        Self {
            displays,
            moves: moves
                .into_iter()
                .map(|(connector, binding)| (connector, ActionEmitter::new(binding)))
                .collect(),
            drag,
            cursor: None,
            rect: Rect::ZERO,
        }
    }

    fn mapping(&self) -> Mapping {
        self.drag
            .with(|drag| drag.as_ref().map(|drag| drag.mapping))
            .unwrap_or_else(|| Mapping::fit(self.rect, &self.displays))
    }

    /// Where each display is drawn this frame: the live preview for the one
    /// being dragged, its stored placement for the rest.
    fn drawn(&self) -> Vec<(ArrangedDisplay, bool)> {
        let drag = self.drag.with(Clone::clone);
        self.displays
            .iter()
            .map(|display| match &drag {
                Some(drag) if drag.active && drag.connector == display.connector => (
                    ArrangedDisplay {
                        placement: drag.preview,
                        ..display.clone()
                    },
                    true,
                ),
                _ => (display.clone(), false),
            })
            .collect()
    }

    fn others(&self, connector: &str) -> Vec<Placement> {
        self.displays
            .iter()
            .filter(|display| display.movable && display.connector != connector)
            .map(|display| display.placement)
            .collect()
    }

    fn press(&self, point: Point) -> bool {
        let mapping = self.mapping();
        // Topmost first: the last drawn display wins where rectangles meet.
        let Some(display) = self
            .displays
            .iter()
            .rev()
            .find(|display| display.movable && mapping.to_canvas(display.placement).contains(point))
            .cloned()
        else {
            return false;
        };
        let (dx, dy) = mapping.to_desktop(point);
        self.drag.set(Some(ArrangementDrag {
            connector: display.connector,
            mapping,
            grab: (
                dx - f64::from(display.placement.x),
                dy - f64::from(display.placement.y),
            ),
            pressed_at: point,
            active: false,
            preview: display.placement,
        }));
        true
    }

    fn drag_to(&self, point: Point) -> bool {
        let Some(mut drag) = self.drag.with(Clone::clone) else {
            return false;
        };
        if !drag.active
            && (point - drag.pressed_at).hypot() < ui::widget::drag::REORDER_THRESHOLD_PX
        {
            return true;
        }
        drag.active = true;
        let (dx, dy) = drag.mapping.to_desktop(point);
        let moved = drag.preview.at(
            round_to_i32(dx - drag.grab.0),
            round_to_i32(dy - drag.grab.1),
        );
        drag.preview = placement::snap(
            moved,
            &self.others(&drag.connector),
            drag.mapping.snap_threshold(),
        );
        self.drag.set(Some(drag));
        true
    }

    fn release(&mut self) -> bool {
        let Some(drag) = self.drag.with(Clone::clone) else {
            return false;
        };
        self.drag.set(None);
        if !drag.active {
            return true;
        }
        let settled = placement::settle(
            drag.preview,
            &self.others(&drag.connector),
            drag.mapping.snap_threshold(),
        );
        let unchanged = self
            .displays
            .iter()
            .any(|display| display.connector == drag.connector && display.placement == settled);
        if !unchanged
            && let Some((_, emitter)) = self
                .moves
                .iter_mut()
                .find(|(connector, _)| *connector == drag.connector)
        {
            let _ = emitter.emit((settled.x, settled.y));
        }
        if let Some(display) = self
            .displays
            .iter_mut()
            .find(|display| display.connector == drag.connector)
        {
            display.placement = settled;
        }
        true
    }

    fn draw_display(
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        rect: Rect,
        display: &ArrangedDisplay,
        dragging: bool,
    ) {
        let colors = &theme.tokens.colors;
        let unit = f64::from(theme.tokens.spacing.s1);
        let ground = colors.surfaces.raised;
        let radius = f64::from(theme.tokens.radii.item);
        let shape = RoundedRect::from_rect(rect, radius);
        let tier = ui::chrome::selection_tier_on(
            theme,
            ui::chrome::SelectionState::new(display.primary, dragging, false),
            ground,
        );
        let fill = if display.movable {
            colors.lift_on(ground, if dragging { 1.0 } else { 0.5 })
        } else {
            ground
        };
        scene.fill(Fill::NonZero, transform, fill, None, &shape);
        scene.stroke(
            &Stroke::new(if dragging { unit / 2.0 } else { unit / 4.0 }),
            transform,
            if dragging {
                theme.border_focus
            } else {
                tier.border.unwrap_or_else(|| ui::surface::edge_on(fill))
            },
            None,
            &ui::surface::edge_outline(rect, radius),
        );
        // The number Identify shows, and the display's name under it.
        let number = display.number.to_string();
        let number_style = theme.text(ui::TextRole::Title).strong();
        let label_style = theme.text(ui::TextRole::Caption);
        let (nw, nh) = text_renderer.measure_style(&number, None, &number_style);
        let label = text_renderer.elide_to_width_style(
            &display.label,
            px(rect.width() - 4.0 * unit),
            &label_style,
        );
        let (lw, lh) = text_renderer.measure_style(&label, None, &label_style);
        let block = f64::from(nh) + f64::from(lh);
        let top = rect.center().y - block / 2.0;
        let mut ctx = ui::paint::PaintCtx::new(scene, transform, text_renderer, theme);
        ctx.text(
            &number,
            &number_style,
            colors.text_primary,
            Point::new(rect.center().x - f64::from(nw) / 2.0, top),
            None,
        );
        ctx.text(
            &label,
            &label_style,
            colors.text_dim,
            Point::new(rect.center().x - f64::from(lw) / 2.0, top + f64::from(nh)),
            None,
        );
        if display.primary {
            // Primary marker: a small bar along the top edge, the place the
            // top bar lives on that display.
            let bar = Rect::new(
                rect.x0 + 2.0 * unit,
                rect.y0 + unit,
                rect.x1 - 2.0 * unit,
                rect.y0 + 1.5 * unit,
            );
            scene.fill(
                Fill::NonZero,
                transform,
                colors.intent.inverted,
                None,
                &RoundedRect::from_rect(bar, f64::from(theme.tokens.radii.detail)),
            );
        }
    }
}

impl Widget for DisplayArrangement {
    ui::provides!(dyn ui::widget::controls::ActionControls);

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        _theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        #[allow(
            clippy::unwrap_used,
            reason = "taffy 0.10's new_leaf always inserts and returns Ok; see ui::widget::TaffyTreeExt's doc comment"
        )]
        let node = taffy
            .new_leaf(Style {
                size: Size {
                    width: Dimension::percent(1.0),
                    height: Dimension::auto(),
                },
                aspect_ratio: Some(CANVAS_ASPECT),
                flex_shrink: 0.0,
                ..Default::default()
            })
            .unwrap();
        node_map.insert(node, std::ptr::from_mut::<dyn Widget>(self));
        node
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        let l = taffy.layout(node).copied().unwrap_or_default();
        self.rect = Rect::new(
            origin.x + f64::from(l.location.x),
            origin.y + f64::from(l.location.y),
            origin.x + f64::from(l.location.x) + f64::from(l.size.width),
            origin.y + f64::from(l.location.y) + f64::from(l.size.height),
        );
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        _presence: &PresenceCtx,
    ) {
        let colors = &theme.tokens.colors;
        let radius = f64::from(theme.tokens.radii.control);
        let ground = colors.surfaces.raised;
        scene.fill(
            Fill::NonZero,
            transform,
            ground,
            None,
            &RoundedRect::from_rect(self.rect, radius),
        );
        let mapping = self.mapping();
        let mut drawn = self.drawn();
        // The dragged display paints last so it rides above the others.
        drawn.sort_by_key(|(_, dragging)| *dragging);
        for (display, dragging) in &drawn {
            let rect = mapping.to_canvas(display.placement);
            Self::draw_display(
                scene,
                transform,
                text_renderer,
                theme,
                rect,
                display,
                *dragging,
            );
        }
        if drawn.is_empty() {
            let message = t("display-arrangement-empty");
            let style = theme.text(ui::TextRole::Body);
            let (w, h) = text_renderer.measure_style(&message, None, &style);
            let mut ctx = ui::paint::PaintCtx::new(scene, transform, text_renderer, theme);
            ctx.text(
                &message,
                &style,
                colors.text_dim,
                Point::new(
                    self.rect.center().x - f64::from(w) / 2.0,
                    self.rect.center().y - f64::from(h) / 2.0,
                ),
                None,
            );
        }
    }

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut ui::widget::EventCtx<'_>) {
        match event {
            WindowEvent::CursorMoved { position, .. } => {
                let point =
                    Point::new(position.x / ctx.scale_factor, position.y / ctx.scale_factor);
                self.cursor = Some(point);
                if self.drag_to(point) {
                    *ctx.handled = true;
                }
            }
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            } => {
                if let Some(point) = self.cursor
                    && self.rect.contains(point)
                    && self.press(point)
                {
                    *ctx.handled = true;
                }
            }
            WindowEvent::MouseInput {
                state: ElementState::Released,
                button: MouseButton::Left,
                ..
            } => {
                if self.release() {
                    *ctx.handled = true;
                }
            }
            WindowEvent::KeyboardInput { event, .. }
                if event.state == ElementState::Pressed
                    && event.logical_key == Key::Named(NamedKey::Escape)
                    && self.drag.with(Option::is_some) =>
            {
                self.drag.set(None);
                *ctx.handled = true;
            }
            _ => {}
        }
    }

    fn accessibility(&self) -> Option<ui::a11y::AccessNode> {
        use ui::a11y::{AccessNode, Role};
        let mapping = self.mapping();
        let mut group = AccessNode::new(Role::Group)
            .with_name(t("display-arrangement-name"))
            .with_description(t("display-arrangement-description"))
            .with_bounds(self.rect);
        for display in &self.displays {
            let key = if display.primary {
                "display-arrangement-item-primary"
            } else {
                "display-arrangement-item"
            };
            group.children.push(
                AccessNode::new(Role::Image)
                    .with_name(t_args(
                        key,
                        &[
                            (
                                "number",
                                ui::i18n::ArgValue::Text(display.number.to_string()),
                            ),
                            ("name", ui::i18n::ArgValue::Text(display.label.clone())),
                        ],
                    ))
                    .with_description(t_args(
                        "display-arrangement-item-position",
                        &[
                            (
                                "x",
                                ui::i18n::ArgValue::Text(display.placement.x.to_string()),
                            ),
                            (
                                "y",
                                ui::i18n::ArgValue::Text(display.placement.y.to_string()),
                            ),
                            (
                                "width",
                                ui::i18n::ArgValue::Text(display.placement.width.to_string()),
                            ),
                            (
                                "height",
                                ui::i18n::ArgValue::Text(display.placement.height.to_string()),
                            ),
                        ],
                    ))
                    .with_bounds(mapping.to_canvas(display.placement)),
            );
        }
        Some(group)
    }

    fn name(&self) -> &'static str {
        "DisplayArrangement"
    }

    fn test_id(&self) -> Option<&str> {
        Some("display.arrangement")
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }
}

impl ui::widget::controls::ActionControls for DisplayArrangement {
    fn take_control_invocation(&mut self) -> Option<tilo_actions::ControlInvocation> {
        self.moves
            .iter_mut()
            .find_map(|(_, emitter)| emitter.take())
    }
    fn refresh_control_action_availability(
        &mut self,
        allowed: &std::collections::HashSet<tilo_actions::ActionAddress>,
    ) {
        for (_, emitter) in &mut self.moves {
            emitter.refresh_availability(allowed);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn displays() -> Vec<ArrangedDisplay> {
        vec![
            ArrangedDisplay {
                connector: "eDP-1".into(),
                label: "Built-in display".into(),
                number: 1,
                placement: Placement::new(0, 0, 1920, 1080),
                primary: true,
                movable: true,
            },
            ArrangedDisplay {
                connector: "HDMI-A-1".into(),
                label: "Desk monitor".into(),
                number: 2,
                placement: Placement::new(1920, 0, 1920, 1080),
                primary: false,
                movable: true,
            },
        ]
    }

    fn canvas() -> DisplayArrangement {
        let moves = displays()
            .iter()
            .filter_map(|display| {
                crate::actions::display_output_move_binding(display.connector.clone())
                    .map(|binding| (display.connector.clone(), binding))
            })
            .collect();
        let mut canvas = DisplayArrangement::new(displays(), moves, ui::LocalState::new(None));
        canvas.rect = Rect::new(0.0, 0.0, 600.0, 220.0);
        canvas
    }

    #[test]
    fn dragging_a_display_below_the_other_emits_its_settled_position() {
        let mut canvas = canvas();
        let mapping = canvas.mapping();
        let monitor = mapping.to_canvas(displays()[1].placement);
        let panel = mapping.to_canvas(displays()[0].placement);
        assert!(canvas.press(monitor.center()));
        // Drop it roughly under the panel, a little off every edge.
        let target = Point::new(
            panel.center().x + 3.0,
            panel.y1 + monitor.height() / 2.0 + 4.0,
        );
        assert!(canvas.drag_to(target));
        assert!(canvas.release());

        let invocation = ui::widget::controls::take_control_invocation(&mut canvas)
            .expect("the drop is sent as the display's move action");
        assert_eq!(invocation.address.action, "move");
        let x = invocation.arguments["x"].as_i64().unwrap();
        let y = invocation.arguments["y"].as_i64().unwrap();
        assert_eq!((x, y), (0, 1080), "snapped flush under the panel");
        assert!(ui::widget::controls::take_control_invocation(&mut canvas).is_none());
    }

    #[test]
    fn a_click_without_movement_changes_nothing() {
        let mut canvas = canvas();
        let monitor = canvas.mapping().to_canvas(displays()[1].placement);
        assert!(canvas.press(monitor.center()));
        assert!(canvas.drag_to(monitor.center() + (1.0, 1.0)));
        assert!(canvas.release());
        assert!(ui::widget::controls::take_control_invocation(&mut canvas).is_none());
    }

    #[test]
    fn every_display_is_described_for_assistive_technology() {
        let node = canvas().accessibility().unwrap();
        let names: Vec<String> = node
            .children
            .iter()
            .filter_map(|child| child.name.clone())
            .collect();
        assert_eq!(
            names,
            [
                "Display 1: Built-in display, primary",
                "Display 2: Desk monitor"
            ]
        );
    }
}
