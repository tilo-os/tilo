//! Floating map controls shared by every Maps surface.
//!
//! This component owns the zoom/location/traffic stack's UI state, geometry,
//! drawing, hit-testing, and tooltip dwell. Geographic effects remain in the
//! app and are requested through [`MapControlAction`].

use ui::time::Instant;

use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};

use ui::icons::Icon;
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::widget::{Button, ButtonFace, ButtonRole, ControlSize, TooltipHost};

/// The geometry of the control column, read from the theme: icon buttons of
/// the Regular control height, the zoom pair an `s2` apart (one group), the
/// locate and traffic buttons an `s3` from it and from each other, and the
/// whole column an `s4` from the map's edges like every floating map surface.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct ColumnMetrics {
    pub size: f64,
    pub margin: f64,
    pub pair_gap: f64,
    pub group_gap: f64,
}

impl ColumnMetrics {
    pub(super) fn of(theme: &Theme) -> Self {
        let sp = &theme.tokens.spacing;
        Self {
            size: ButtonFace::height(ControlSize::Regular, theme),
            margin: f64::from(sp.s4),
            pair_gap: f64::from(sp.s2),
            group_gap: f64::from(sp.s3),
        }
    }

    /// Height of the whole column below its top margin: four controls, the
    /// zoom pair gap and two sibling gaps. The app lays the navigation card
    /// and the overflow button out around this.
    pub(super) fn column_height(self) -> f64 {
        4.0 * self.size + self.pair_gap + 2.0 * self.group_gap
    }
}

impl Default for ColumnMetrics {
    fn default() -> Self {
        Self::of(&Theme::dark())
    }
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
    fn new(viewport: Rect, m: ColumnMetrics) -> Self {
        let x1 = viewport.x1 - m.margin;
        let y0 = viewport.y0 + m.margin;
        let below = |r: Rect, gap: f64| Rect::new(r.x0, r.y1 + gap, r.x1, r.y1 + gap + m.size);
        let zoom_in = Rect::new(x1 - m.size, y0, x1, y0 + m.size);
        let zoom_out = below(zoom_in, m.pair_gap);
        let locate = below(zoom_out, m.group_gap);
        let traffic = below(locate, m.group_gap);
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

impl MapControlAction {
    const ALL: [Self; 4] = [
        Self::ZoomIn,
        Self::ZoomOut,
        Self::Locate,
        Self::ToggleTraffic,
    ];

    /// What the control is called: its accessible name and its tooltip.
    pub(super) fn label(self) -> &'static str {
        match self {
            Self::ZoomIn => "Zoom in",
            Self::ZoomOut => "Zoom out",
            Self::Locate => "My location",
            Self::ToggleTraffic => "Traffic",
        }
    }

    fn icon(self) -> Icon {
        match self {
            Self::ZoomIn => Icon::Plus,
            Self::ZoomOut => Icon::Minus,
            Self::Locate => Icon::MousePointer2,
            Self::ToggleTraffic => Icon::Layers,
        }
    }
}

pub(super) struct MapControls {
    viewport: Rect,
    metrics: ColumnMetrics,
    traffic_enabled: bool,
    hovered: Option<MapControlAction>,
    tooltip: TooltipHost,
}

impl Default for MapControls {
    fn default() -> Self {
        Self {
            viewport: Rect::ZERO,
            metrics: ColumnMetrics::default(),
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

    pub(super) fn set_metrics(&mut self, metrics: ColumnMetrics) {
        self.metrics = metrics;
    }

    pub(super) fn metrics(&self) -> ColumnMetrics {
        self.metrics
    }

    fn layout(&self) -> Layout {
        Layout::new(self.viewport, self.metrics)
    }

    pub(super) fn traffic_enabled(&self) -> bool {
        self.traffic_enabled
    }

    pub(super) fn set_traffic_enabled(&mut self, enabled: bool) {
        self.traffic_enabled = enabled;
    }

    pub(super) fn hit_test(&self, point: Point) -> Option<MapControlAction> {
        let layout = self.layout();
        MapControlAction::ALL
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
        let own_target = self
            .hovered
            .map(|action| (self.layout().rect(action), action.label().to_string()));
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

    /// Paint the column: icon buttons on their raised face so they read over
    /// any basemap tone. The traffic toggle shows its on state as the
    /// pressed lift.
    pub(super) fn draw(
        &self,
        scene: &mut Scene,
        transform: Affine,
        text: &mut TextRenderer,
        theme: &Theme,
    ) {
        let layout = self.layout();
        for action in MapControlAction::ALL {
            let on = action == MapControlAction::ToggleTraffic && self.traffic_enabled;
            Button::paint(
                scene,
                transform,
                text,
                theme,
                &ButtonFace::icon_only(layout.rect(action), action.icon(), action.label())
                    .role(ButtonRole::Secondary)
                    .hovered(self.hovered == Some(action))
                    .pressed(on),
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
        self.layout().rect(action)
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
            let sp = Theme::dark().tokens.spacing;
            let zoom_in = controls.rect(MapControlAction::ZoomIn);
            let zoom_out = controls.rect(MapControlAction::ZoomOut);
            let locate = controls.rect(MapControlAction::Locate);
            let traffic = controls.rect(MapControlAction::ToggleTraffic);
            assert_eq!(zoom_out.y0 - zoom_in.y1, f64::from(sp.s2));
            assert_eq!(locate.y0 - zoom_out.y1, f64::from(sp.s3));
            assert_eq!(traffic.y0 - locate.y1, f64::from(sp.s3));
            assert_eq!(zoom_in.y0, f64::from(sp.s4));
            assert_eq!(traffic.y1 - zoom_in.y0, controls.metrics().column_height());
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
