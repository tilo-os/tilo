//! Floating map controls shared by every Maps surface.
//!
//! This component owns the zoom/location/traffic stack's UI state, geometry,
//! drawing, hit-testing, and tooltip dwell. Geographic effects remain in the
//! app and are requested through [`MapControlAction`].

use ui::time::Instant;

use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use vello::peniko::Color;

use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::tokens::spacing::Spacing;
use ui::widget::{Button, ButtonFace, TooltipHost};

use crate::draw;

pub(super) const SIZE: f64 = 40.0;
/// Inset of the control column from the map's edge — the theme's `s4`, the
/// same margin every other floating map surface keeps.
const MARGIN: f64 = Spacing::BASE.s4 as f64;
/// The zoom pair reads as one group: its two buttons sit an `s2` apart
/// (HIG §7b "related rows"), never sharing an edge — nothing touches.
const PAIR_GAP: f64 = Spacing::BASE.s2 as f64;
/// Locate and Traffic are siblings of the zoom group: `s3` between them.
const GROUP_GAP: f64 = Spacing::BASE.s3 as f64;

/// Height of the whole control column below its top margin: four controls,
/// the zoom pair gap and two sibling gaps. The app lays the navigation card
/// and the ⋯ overflow button out around this.
pub(super) const fn column_height() -> f64 {
    4.0 * SIZE + PAIR_GAP + 2.0 * GROUP_GAP
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum MapControlAction {
    ZoomIn,
    ZoomOut,
    Locate,
    ToggleTraffic,
}

#[derive(Clone, Copy)]
struct Layout {
    zoom_in: Rect,
    zoom_out: Rect,
    locate: Rect,
    traffic: Rect,
}

impl Layout {
    fn new(viewport: Rect) -> Self {
        let x1 = viewport.x1 - MARGIN;
        let y0 = viewport.y0 + MARGIN;
        let zoom_in = Rect::new(x1 - SIZE, y0, x1, y0 + SIZE);
        let zoom_out = Rect::new(
            zoom_in.x0,
            zoom_in.y1 + PAIR_GAP,
            zoom_in.x1,
            zoom_in.y1 + PAIR_GAP + SIZE,
        );
        let locate = Rect::new(
            zoom_out.x0,
            zoom_out.y1 + GROUP_GAP,
            zoom_out.x1,
            zoom_out.y1 + GROUP_GAP + SIZE,
        );
        let traffic = Rect::new(
            locate.x0,
            locate.y1 + GROUP_GAP,
            locate.x1,
            locate.y1 + GROUP_GAP + SIZE,
        );
        Self {
            zoom_in,
            zoom_out,
            locate,
            traffic,
        }
    }

    fn rect(self, action: MapControlAction) -> Rect {
        match action {
            MapControlAction::ZoomIn => self.zoom_in,
            MapControlAction::ZoomOut => self.zoom_out,
            MapControlAction::Locate => self.locate,
            MapControlAction::ToggleTraffic => self.traffic,
        }
    }
}

pub(super) struct MapControls {
    viewport: Rect,
    traffic_enabled: bool,
    hovered: Option<MapControlAction>,
    tooltip: TooltipHost,
}

impl Default for MapControls {
    fn default() -> Self {
        Self {
            viewport: Rect::ZERO,
            traffic_enabled: false,
            hovered: None,
            tooltip: TooltipHost::new(),
        }
    }
}

impl MapControls {
    pub(super) fn set_viewport(&mut self, viewport: Rect) {
        self.viewport = viewport;
    }

    pub(super) fn traffic_enabled(&self) -> bool {
        self.traffic_enabled
    }

    pub(super) fn set_traffic_enabled(&mut self, enabled: bool) {
        self.traffic_enabled = enabled;
    }

    pub(super) fn hit_test(&self, point: Point) -> Option<MapControlAction> {
        let layout = Layout::new(self.viewport);
        [
            MapControlAction::ZoomIn,
            MapControlAction::ZoomOut,
            MapControlAction::Locate,
            MapControlAction::ToggleTraffic,
        ]
        .into_iter()
        .find(|action| layout.rect(*action).contains(point))
    }

    /// Retarget both immediate hover feedback and the shared delayed tooltip.
    /// Extra targets let the app's narrow-menu and overflow buttons participate
    /// without duplicating a second tooltip controller.
    pub(super) fn on_pointer(
        &mut self,
        point: Point,
        suppressed: bool,
        extra_targets: impl IntoIterator<Item = (Rect, &'static str)>,
        now: Instant,
    ) -> bool {
        let old_hover = self.hovered;
        self.hovered = (!suppressed).then(|| self.hit_test(point)).flatten();
        let own_target = self.hovered.map(|action| {
            let label = match action {
                MapControlAction::ZoomIn => "Zoom in",
                MapControlAction::ZoomOut => "Zoom out",
                MapControlAction::Locate => "My location",
                MapControlAction::ToggleTraffic => "Traffic",
            };
            (Layout::new(self.viewport).rect(action), label.to_string())
        });
        let target = if suppressed {
            None
        } else {
            own_target.or_else(|| {
                extra_targets
                    .into_iter()
                    .find(|(rect, _)| rect.contains(point))
                    .map(|(rect, label)| (rect, label.to_string()))
            })
        };
        let tooltip_changed = self.tooltip.on_pointer(target, now);
        old_hover != self.hovered || tooltip_changed
    }

    pub(super) fn dismiss_tooltip(&mut self) {
        self.tooltip.dismiss();
    }

    pub(super) fn needs_frame(&self) -> bool {
        self.tooltip.needs_frame()
    }

    pub(super) fn draw(
        &self,
        scene: &mut Scene,
        transform: Affine,
        text: &mut TextRenderer,
        theme: &Theme,
    ) {
        let layout = Layout::new(self.viewport);
        for (action, label) in [
            (MapControlAction::ZoomIn, "+"),
            (MapControlAction::ZoomOut, "–"),
        ] {
            Button::paint(
                scene,
                transform,
                text,
                theme,
                &ButtonFace::new(layout.rect(action), label)
                    .radius(10.0)
                    .text_size(22.0)
                    .hovered(self.hovered == Some(action))
                    .accessible_name(if action == MapControlAction::ZoomIn {
                        "Zoom in"
                    } else {
                        "Zoom out"
                    }),
            );
        }

        let colors = &theme.tokens.colors;
        // Locate and Traffic are the same Neutral button face as the zoom
        // pair (one rest edge, one hover, one radius for the whole column);
        // their pictograms paint on top of the face.
        let locate = layout.locate;
        Button::paint(
            scene,
            transform,
            text,
            theme,
            &ButtonFace::new(locate, "")
                .radius(10.0)
                .hovered(self.hovered == Some(MapControlAction::Locate))
                .accessible_name("My location"),
        );
        let center = locate.center();
        draw::stroke_circle(scene, transform, center, 6.0, 2.0, theme.accent_primary);
        draw::fill_circle(scene, transform, center, 2.0, theme.accent_primary);
        for (dx, dy) in [(0.0, -9.0), (0.0, 9.0), (-9.0, 0.0), (9.0, 0.0)] {
            draw::line(
                scene,
                transform,
                Point::new(center.x + dx * 0.7, center.y + dy * 0.7),
                Point::new(center.x + dx, center.y + dy),
                2.0,
                theme.accent_primary,
            );
        }

        let traffic = layout.traffic;
        let traffic_hovered = self.hovered == Some(MapControlAction::ToggleTraffic);
        Button::paint(
            scene,
            transform,
            text,
            theme,
            &ButtonFace::new(traffic, "")
                .radius(10.0)
                .hovered(traffic_hovered)
                .accessible_name("Traffic"),
        );
        let (traffic_bg, _, _) = ButtonFace::new(traffic, "")
            .hovered(traffic_hovered)
            .colors(theme);
        if self.traffic_enabled {
            // The overlay is *live* while enabled — accent is the right
            // vocabulary for that (HIG §9.4), as a ring over the rest face.
            draw::stroke_rrect(scene, transform, traffic, 10.0, 1.0, theme.accent_primary);
        }
        let center = traffic.center();
        let dot = |color: Color| {
            if self.traffic_enabled {
                color
            } else {
                draw::mix(color, traffic_bg, 0.55)
            }
        };
        for (dy, color) in [
            (-8.0, colors.semantic.danger),
            (0.0, colors.semantic.warning),
            (8.0, colors.semantic.success),
        ] {
            draw::fill_circle(
                scene,
                transform,
                Point::new(center.x, center.y + dy),
                3.2,
                dot(color),
            );
        }
    }

    /// Paint the delayed tooltip after the app's other floating surfaces.
    pub(super) fn draw_tooltip(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text: &mut TextRenderer,
        theme: &Theme,
    ) {
        self.tooltip.tick(ui::time::now());
        if let Some((label, anchor)) = self.tooltip.visible() {
            ui::widget::tooltip::draw_hover_tooltip(
                scene,
                transform,
                text,
                theme,
                anchor,
                label,
                self.viewport,
            );
        }
    }

    pub(super) fn rect(&self, action: MapControlAction) -> Rect {
        Layout::new(self.viewport).rect(action)
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    fn controls() -> MapControls {
        let mut controls = MapControls::default();
        controls.set_viewport(Rect::new(0.0, 0.0, 1280.0, 800.0));
        controls
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn map_controls_own_hit_testing_hover_gaps_and_traffic_state() {
        // Phase `every_drawn_control_hit_tests_to_one_semantic_action`.
        {
            let controls = controls();
            for action in [
                MapControlAction::ZoomIn,
                MapControlAction::ZoomOut,
                MapControlAction::Locate,
                MapControlAction::ToggleTraffic,
            ] {
                assert_eq!(
                    controls.hit_test(controls.rect(action).center()),
                    Some(action)
                );
            }
            assert_eq!(controls.hit_test(Point::new(640.0, 400.0)), None);
        }

        // Phase `hover_and_tooltip_state_are_owned_by_the_component`.
        {
            let mut controls = controls();
            let zoom = controls.rect(MapControlAction::ZoomIn);
            let t0 = ui::time::now();
            assert!(controls.on_pointer(zoom.center(), false, [], t0));
            assert_eq!(controls.hovered, Some(MapControlAction::ZoomIn));
            assert!(controls.tooltip.visible().is_none());
            controls.tooltip.tick(t0 + Duration::from_millis(600));
            assert_eq!(controls.tooltip.visible(), Some(("Zoom in", zoom)));
            assert!(controls.on_pointer(zoom.center(), true, [], t0));
            assert_eq!(controls.hovered, None);
        }

        // Phase `controls_keep_token_gaps_and_never_touch`.
        // HIG §7 "nothing touches": the zoom pair sits an `s2` apart, the
        // groups an `s3`, and the column the app lays out around is exactly
        // the four faces plus those gaps — no control shares an edge.
        {
            let controls = controls();
            let sp = Spacing::BASE;
            let zoom_in = controls.rect(MapControlAction::ZoomIn);
            let zoom_out = controls.rect(MapControlAction::ZoomOut);
            let locate = controls.rect(MapControlAction::Locate);
            let traffic = controls.rect(MapControlAction::ToggleTraffic);
            assert_eq!(zoom_out.y0 - zoom_in.y1, f64::from(sp.s2));
            assert_eq!(locate.y0 - zoom_out.y1, f64::from(sp.s3));
            assert_eq!(traffic.y0 - locate.y1, f64::from(sp.s3));
            assert_eq!(zoom_in.y0, f64::from(sp.s4));
            assert_eq!(traffic.y1 - zoom_in.y0, column_height());
            let rects = [zoom_in, zoom_out, locate, traffic];
            for (i, a) in rects.iter().enumerate() {
                for b in &rects[i + 1..] {
                    assert!(
                        b.y0 - a.y1 >= f64::from(sp.s1),
                        "{a:?} and {b:?} touch or overlap"
                    );
                }
            }
        }

        // Phase `traffic_visual_state_is_not_duplicated_in_the_app`.
        {
            let mut controls = controls();
            assert!(!controls.traffic_enabled());
            controls.set_traffic_enabled(true);
            assert!(controls.traffic_enabled());
        }
    }
}
