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

pub use settings_store::display_placement as placement;

use crate as ui;
use crate::widget::TaffyTreeExt;

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

use crate::i18n::{ArgValue, EmbeddedCatalog};

static CATALOG: EmbeddedCatalog = catalogue();

const fn catalogue() -> EmbeddedCatalog {
    let catalog = EmbeddedCatalog::new(
        "en-US",
        &[
            ("en-US", include_str!("../../locales/en-US.ftl")),
            ("de-DE", include_str!("../../locales/de-DE.ftl")),
        ],
        "display-arrangement-name",
    )
    .with_system_locale();
    #[cfg(test)]
    let catalog = catalog.with_pinned_locale("en-US");
    catalog
}

fn t(key: &str) -> String {
    CATALOG.t(key)
}
fn t_args(key: &str, args: &[(&str, ArgValue)]) -> String {
    CATALOG.t_args(key, args)
}

/// One display as the canvas draws it.
#[derive(Clone, Debug, PartialEq)]
pub struct ArrangedDisplay {
    pub connector: String,
    /// Screens in one device move together without changing its native seat layout.
    pub movement_group: Option<String>,
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
    snap_radius: f64,
}

impl Mapping {
    fn fit(canvas: Rect, displays: &[ArrangedDisplay], padding: f64, snap_radius: f64) -> Self {
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
        let inner = canvas.inset(-padding);
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
            snap_radius,
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
        round_to_i32(self.snap_radius / self.scale)
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
    padding: f64,
    snap_radius: f64,
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
            padding: f64::from(ui::tokens::Spacing::BASE.s6),
            snap_radius: f64::from(ui::tokens::Metrics::BASE.icon_sm),
            rect: Rect::ZERO,
        }
    }

    fn mapping(&self) -> Mapping {
        self.drag
            .with(|drag| drag.as_ref().map(|drag| drag.mapping))
            .unwrap_or_else(|| {
                Mapping::fit(self.rect, &self.displays, self.padding, self.snap_radius)
            })
    }

    /// Where each display is drawn this frame: the live preview for the one
    /// being dragged, its stored placement for the rest.
    fn moves_together(&self, connector: &str, display: &ArrangedDisplay) -> bool {
        display.connector == connector
            || self
                .displays
                .iter()
                .find(|item| item.connector == connector)
                .is_some_and(|item| {
                    item.movement_group.is_some() && item.movement_group == display.movement_group
                })
    }

    fn moved_placement(
        &self,
        connector: &str,
        destination: Placement,
        display: &ArrangedDisplay,
    ) -> Placement {
        let Some(selected) = self
            .displays
            .iter()
            .find(|item| item.connector == connector)
        else {
            return display.placement;
        };
        display.placement.at(
            display
                .placement
                .x
                .saturating_add(destination.x.saturating_sub(selected.placement.x)),
            display
                .placement
                .y
                .saturating_add(destination.y.saturating_sub(selected.placement.y)),
        )
    }

    fn drawn(&self) -> Vec<(ArrangedDisplay, bool)> {
        let drag = self.drag.with(Clone::clone);
        self.displays
            .iter()
            .map(|display| match &drag {
                Some(drag) if drag.active && self.moves_together(&drag.connector, display) => (
                    ArrangedDisplay {
                        placement: self.moved_placement(&drag.connector, drag.preview, display),
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
            .filter(|display| !self.moves_together(connector, display))
            .map(|display| display.placement)
            .collect()
    }

    fn group_bounds(&self, connector: &str, destination: Placement) -> Placement {
        let screens = self
            .displays
            .iter()
            .filter(|display| self.moves_together(connector, display))
            .map(|display| self.moved_placement(connector, destination, display))
            .collect::<Vec<_>>();
        let x = screens
            .iter()
            .map(|rect| rect.x)
            .min()
            .unwrap_or(destination.x);
        let y = screens
            .iter()
            .map(|rect| rect.y)
            .min()
            .unwrap_or(destination.y);
        let right = screens
            .iter()
            .map(|rect| rect.right())
            .max()
            .unwrap_or(destination.right());
        let bottom = screens
            .iter()
            .map(|rect| rect.bottom())
            .max()
            .unwrap_or(destination.bottom());
        Placement::new(x, y, right.saturating_sub(x), bottom.saturating_sub(y))
    }

    fn align_group(
        &self,
        connector: &str,
        destination: Placement,
        settled: Placement,
    ) -> Placement {
        let bounds = self.group_bounds(connector, destination);
        destination.at(
            destination
                .x
                .saturating_add(settled.x.saturating_sub(bounds.x)),
            destination
                .y
                .saturating_add(settled.y.saturating_sub(bounds.y)),
        )
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
        let snapped = placement::snap(
            self.group_bounds(&drag.connector, moved),
            &self.others(&drag.connector),
            drag.mapping.snap_threshold(),
        );
        drag.preview = self.align_group(&drag.connector, moved, snapped);
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
        let bounds = placement::settle(
            self.group_bounds(&drag.connector, drag.preview),
            &self.others(&drag.connector),
            drag.mapping.snap_threshold(),
        );
        let settled = self.align_group(&drag.connector, drag.preview, bounds);
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
        let positions = self
            .displays
            .iter()
            .map(|display| {
                if self.moves_together(&drag.connector, display) {
                    self.moved_placement(&drag.connector, settled, display)
                } else {
                    display.placement
                }
            })
            .collect::<Vec<_>>();
        for (display, position) in self.displays.iter_mut().zip(positions) {
            display.placement = position;
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

        let radius = f64::from(theme.tokens.radii.control);
        let shape = RoundedRect::from_rect(rect, radius);
        let tier = ui::chrome::selection_tier(
            theme,
            ui::chrome::SelectionState::new(display.primary, dragging, false),
        );
        let fill = if display.movable {
            colors.lift(if dragging { 0.7 } else { 0.45 })
        } else {
            colors.lift(0.2)
        };
        scene.fill(Fill::NonZero, transform, fill, None, &shape);
        scene.stroke(
            &Stroke::new(if dragging { 2.0 } else { 1.0 }),
            transform,
            if dragging {
                theme.border_focus
            } else {
                tier.border.unwrap_or_else(|| ui::surface::edge_on(fill))
            },
            None,
            &ui::surface::edge_outline(rect, radius),
        );
        let number = display.number.to_string();
        let number_style = theme.text(ui::TextRole::Title).strong().numeric();
        let (nw, nh) = text_renderer.measure_style(&number, None, &number_style);
        text_renderer.draw_style(
            scene,
            &number,
            colors.text_primary,
            transform.pre_translate(
                (
                    rect.center().x - f64::from(nw) / 2.0,
                    rect.center().y - f64::from(nh) / 2.0 - f64::from(theme.tokens.spacing.s1),
                )
                    .into(),
            ),
            None,
            &number_style,
        );
        let caption_style = theme.text(ui::TextRole::Caption);
        let label = text_renderer.elide_to_width_style(
            &display.label,
            px(rect.width() - f64::from(theme.tokens.spacing.s3)),
            &caption_style,
        );
        let (lw, _) = text_renderer.measure_style(&label, None, &caption_style);
        text_renderer.draw_style(
            scene,
            &label,
            colors.text_dim,
            transform.pre_translate(
                (
                    rect.center().x - f64::from(lw) / 2.0,
                    rect.center().y + f64::from(nh) / 2.0 - 4.0,
                )
                    .into(),
            ),
            None,
            &caption_style,
        );
        if display.primary {
            // Primary marker: a small bar along the top edge, the place the
            // top bar lives on that display.
            let bar = Rect::new(rect.x0 + 6.0, rect.y0 + 5.0, rect.x1 - 6.0, rect.y0 + 8.0);
            scene.fill(
                Fill::NonZero,
                transform,
                theme.tokens.colors.text_primary,
                None,
                &RoundedRect::from_rect(bar, f64::from(theme.tokens.radii.control)),
            );
        }
    }
}

impl Widget for DisplayArrangement {
    ui::provides!(dyn ui::widget::controls::ActionControls);

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        self.padding = f64::from(theme.tokens.spacing.s6);
        self.snap_radius = f64::from(theme.tokens.metrics.icon_sm);
        let style = Style {
            size: Size {
                width: Dimension::percent(1.0),
                height: Dimension::length(theme.tokens.metrics.panel_width_utility * 11.0 / 16.0),
            },
            flex_shrink: 0.0,
            ..Default::default()
        };
        taffy.widget_leaf(style, node_map, self)
    }

    /// Value, hover and drag change the paint, not this leaf's intrinsic size.
    /// Theme, viewport and input-capability changes rebuild the host layout.
    fn is_layout_dirty(&self) -> bool {
        self.rect == Rect::ZERO
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        self.rect = crate::widget::layout_rect(taffy, node, origin);
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
        let radius = f64::from(theme.tokens.radii.panel);
        let ground = colors.lift(0.1);
        ui::surface::card(scene, transform, self.rect, radius, ground);
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
            text_renderer.draw_style(
                scene,
                &message,
                colors.text_dim,
                transform.pre_translate(
                    (
                        self.rect.center().x - f64::from(w) / 2.0,
                        self.rect.center().y - f64::from(h) / 2.0,
                    )
                        .into(),
                ),
                None,
                &style,
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

    #[derive(Clone, Debug, tilo_resource::Effect)]
    #[effect(operation = "test.display")]
    enum TestEffect {
        #[effect(returns, row)]
        MoveDisplay { display: String, x: i32, y: i32 },
    }

    fn displays() -> Vec<ArrangedDisplay> {
        vec![
            ArrangedDisplay {
                connector: "eDP-1".into(),
                movement_group: None,
                label: "Built-in display".into(),
                number: 1,
                placement: Placement::new(0, 0, 1920, 1080),
                primary: true,
                movable: true,
            },
            ArrangedDisplay {
                connector: "HDMI-A-1".into(),
                movement_group: None,
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
            .map(|display| {
                use tilo_resource::Effect as _;
                let spec = TestEffect::declarations()
                    .into_iter()
                    .find(|decl| decl.id == "move-display")
                    .expect("moving a display is an effect")
                    .spec("test.display");
                let resource =
                    ui::tilo_actions::Resource::new("settings.root", "settings.root", "Settings")
                        .with_action(spec.clone());
                let connector = display.connector.clone();
                let binding = ui::tilo_actions::ActionBinding::new(
                    &resource,
                    &spec,
                    move |(x, y): (i32, i32)| {
                        TestEffect::MOVE_DISPLAY.arguments(&(connector.clone(), x, y))
                    },
                )
                .expect("the move effect is exported");
                (display.connector.clone(), binding)
            })
            .collect();
        let mut canvas = DisplayArrangement::new(displays(), moves, ui::LocalState::new(None));
        canvas.rect = Rect::new(0.0, 0.0, 600.0, 220.0);
        canvas
    }

    #[test]
    fn retained_arrangement_hover_preserves_geometry_without_remeasurement() {
        let mut host =
            ui::headless::FrameHarness::new(ui::State::new(), Box::new(canvas()), (800, 600));
        host.profile_frame();
        assert!(!host.last_relayout().measured);
        let rect = host
            .targets()
            .get("display.arrangement")
            .copied()
            .expect("the actual arrangement is mounted");
        host.send(&WindowEvent::CursorMoved {
            device_id: winit::event::DeviceId::dummy(),
            position: winit::dpi::PhysicalPosition::new(rect.center().x, rect.center().y),
        });
        assert!(!host.last_relayout().measured);
        host.verify_retained_geometry().unwrap();
        host.resize((1200, 720)).unwrap();
        assert!(host.last_relayout().rebuilt);
        assert!(
            host.targets()
                .get("display.arrangement")
                .copied()
                .expect("the actual arrangement is mounted")
                .width()
                > rect.width()
        );
        host.verify_retained_geometry().unwrap();
        host.set_theme(ui::Theme::light());
        assert!(host.last_relayout().rebuilt);
        host.verify_retained_geometry().unwrap();
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
        assert_eq!(invocation.address.action, "move-display");
        let x = invocation.arguments["x"].as_i64().unwrap();
        let y = invocation.arguments["y"].as_i64().unwrap();
        assert_eq!((x, y), (0, 1080), "snapped flush under the panel");
        assert!(ui::widget::controls::take_control_invocation(&mut canvas).is_none());
    }

    #[test]
    fn device_screens_preview_and_drop_together_preserving_native_seat_geometry() {
        let mut canvas = canvas();
        canvas.displays[0].movable = false;
        canvas.displays[1].movement_group = Some("peer".into());
        canvas.displays.push(ArrangedDisplay {
            connector: "peer/DP-2".into(),
            movement_group: Some("peer".into()),
            label: "Second peer screen".into(),
            number: 3,
            placement: Placement::new(1920, 1080, 1280, 720),
            primary: false,
            movable: true,
        });
        let mapping = canvas.mapping();
        let first = mapping.to_canvas(canvas.displays[1].placement);
        let panel = mapping.to_canvas(canvas.displays[0].placement);
        assert!(canvas.press(first.center()));
        assert!(canvas.drag_to(Point::new(
            panel.center().x,
            panel.y1 + first.height() / 2.0 + 4.0
        )));
        let drawn = canvas.drawn();
        assert_eq!(drawn.iter().filter(|(_, dragging)| *dragging).count(), 2);
        assert_eq!(drawn[2].0.placement.x, drawn[1].0.placement.x);
        assert_eq!(drawn[2].0.placement.y - drawn[1].0.placement.y, 1080);
        assert!(canvas.release());
        assert_eq!(
            canvas.displays[2].placement.x,
            canvas.displays[1].placement.x
        );
        assert_eq!(
            canvas.displays[2].placement.y - canvas.displays[1].placement.y,
            1080
        );
        assert_eq!(canvas.displays[2].placement.width, 1280);
        let invocation = ui::widget::controls::take_control_invocation(&mut canvas).unwrap();
        assert_eq!(invocation.address.action, "move-display");
        assert!(ui::widget::controls::take_control_invocation(&mut canvas).is_none());
    }

    #[test]
    fn immovable_local_screen_still_constrains_peer_drag() {
        let mut canvas = canvas();
        canvas.displays[0].movable = false;
        let mapping = canvas.mapping();
        let local = mapping.to_canvas(canvas.displays[0].placement);
        let peer = mapping.to_canvas(canvas.displays[1].placement);
        assert!(!canvas.press(local.center()));
        assert!(canvas.press(peer.center()));
        assert!(canvas.drag_to(local.center()));
        assert!(canvas.release());
        assert!(
            !canvas.displays[0]
                .placement
                .overlaps(canvas.displays[1].placement)
        );
        assert!(ui::widget::controls::take_control_invocation(&mut canvas).is_some());
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
