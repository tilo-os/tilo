//! `StatusCluster`: the bar's status cluster as one control
//! (`docs/architecture/ui-inventory.md`, "Bar status cluster").

use vello::kurbo::{Point, Rect};
use vello::peniko::Color;

use crate::icons::{self, Icon, LUCIDE_DEFAULT_STROKE_PX};
use crate::paint::PaintCtx;
use crate::text_render::{TextRenderer, TextStyle, TextWeight};
use crate::theme::Theme;
use crate::tokens::TextRole;
use crate::widget::{BadgeFace, BarControlFace, ChipStyle};

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum StatusPart {
    /// The notification bell; `quiet` when fresh banners are held.
    Bell { unread: u32, quiet: bool },
    /// The volume, `silent` when it is muted.
    Volume { silent: bool },
    /// The network link, at `bars` of four when it is Wi-Fi (`0` otherwise).
    Link {
        kind: LinkKind,
        /// Signal strength on Wi-Fi, `0` off it.
        bars: u8,
    },
    /// The battery: a gauge, its percentage, and whether it is charging.
    Battery {
        /// How full it is, `0..=100`.
        percent: u8,
        charging: bool,
    },
    /// A glyph alone, in the cluster's dim ink.
    Glyph(Icon),
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum LinkKind {
    /// Nothing is connected.
    #[default]
    Offline,
    /// A cable.
    Wired,
    /// Wi-Fi.
    Wifi,
}

impl LinkKind {
    /// The glyph a surface draws for this kind of connection: the cable's
    /// port, Wi-Fi's arcs, or the crossed-out arcs for no connection.
    #[must_use]
    pub fn icon(self) -> Icon {
        match self {
            Self::Wired => Icon::EthernetPort,
            Self::Wifi => Icon::Wifi,
            Self::Offline => Icon::WifiOff,
        }
    }
}

/// The machine's link, as a surface draws it: what it is, what it is called,
/// and how strong its signal is.
#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct LinkStatus {
    pub kind: LinkKind,
    /// What the connection is called: the network's name on Wi-Fi, the
    /// profile's on a cable. `None` while nothing is connected.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// Signal strength from 0 to 4 on Wi-Fi, `0` on a cable and offline.
    pub signal: u8,
}

impl LinkStatus {
    /// The signal as the status cluster's link part counts it: `1..=4` on
    /// Wi-Fi, `0` where it has no bars to draw.
    #[must_use]
    pub fn bars(&self) -> u8 {
        match self.kind {
            LinkKind::Wifi => self.signal.clamp(1, 4),
            LinkKind::Offline | LinkKind::Wired => 0,
        }
    }
}

/// The cluster's one control: its parts, the state they share and how it
/// tells the degrees that matter.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct StatusCluster {
    /// How full the battery is, `0..=100`: the cluster's gauge and its wash.
    pub battery: Option<u8>,
    pub charging: bool,
}

impl StatusCluster {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    #[must_use]
    pub fn battery(mut self, percent: u8, charging: bool) -> Self {
        self.battery = Some(percent.min(100));
        self.charging = charging;
        self
    }

    /// Below this the whole cluster wears the danger wash: the person needs
    /// to notice the control, not just read the glyph.
    pub const CRITICAL: u8 = 5;
    pub const LOW: u8 = 15;
}

#[derive(Clone, Copy, Debug)]
pub struct StatusClusterFace<'a> {
    parts: &'a [StatusPart],
    battery: Option<u8>,
    charging: bool,
    open: bool,
    hovered: bool,
    pressed: bool,
    focused: bool,
    /// The control's height; `None` uses the theme's bar control height.
    height: Option<f64>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct StatusClusterLayout {
    pub cluster: Rect,
    /// Each part's glyph square, in the order they were given.
    pub parts: [Rect; MAX_PARTS],
    pub badge: Rect,
}

/// How far past its glyph's top right corner the bell's count is centred.
const BADGE_PAST_CORNER: f64 = 3.0;

/// The most parts a cluster takes: the bell, the volume, the link, the battery, and one glyph of
/// width.
pub const MAX_PARTS: usize = 5;

impl<'a> StatusClusterFace<'a> {
    /// The cluster of `parts`, at the battery reading it shares.
    #[must_use]
    pub fn new(parts: &'a [StatusPart], status: StatusCluster) -> Self {
        Self {
            parts,
            battery: status.battery,
            charging: status.charging,
            open: false,
            hovered: false,
            pressed: false,
            focused: false,
            height: None,
        }
    }

    /// Paint and measure at `height` rather than the theme's bar control
    /// height: a bar whose own controls are a little shorter than the
    /// toolkit's default (the shell's is 24) keeps one top and bottom
    /// edge across every control.
    #[must_use]
    pub fn at_height(mut self, height: f64) -> Self {
        self.height = Some(height.max(1.0));
        self
    }

    #[must_use]
    pub fn open(mut self, open: bool) -> Self {
        self.open = open;
        self
    }

    #[must_use]
    pub fn hovered(mut self, hovered: bool) -> Self {
        self.hovered = hovered;
        self
    }

    #[must_use]
    pub fn pressed(mut self, pressed: bool) -> Self {
        self.pressed = pressed;
        self
    }

    /// Keyboard focus; the ring shows only while focus is visible.
    #[must_use]
    pub fn focused(mut self, focused: bool) -> Self {
        self.focused = focused;
        self
    }

    #[must_use]
    pub fn height(theme: &Theme) -> f64 {
        BarControlFace::height(theme)
    }

    fn part(&self, theme: &Theme) -> f64 {
        self.height.unwrap_or_else(|| Self::height(theme))
    }

    fn pad(theme: &Theme) -> f64 {
        f64::from(theme.tokens.spacing.rhythm.tight)
    }

    /// A part's glyph: the size every bar control's glyph is drawn at
    /// ([`crate::widget::BarChipFace`]), so the bell and the people beside it
    /// read as one set.
    fn glyph(theme: &Theme) -> f64 {
        f64::from(theme.tokens.metrics.icon_control)
    }

    fn percentage_style(theme: &Theme) -> TextStyle {
        theme
            .text(TextRole::Caption)
            .with_weight(TextWeight::Semibold)
    }

    fn parts_shown(&self) -> usize {
        self.parts.len().min(MAX_PARTS)
    }

    /// The width after the bell, when a part follows it: as much as its widest count reaches past
    /// it, and a hairline more, so a count never stands on the glyph beside the bell. The same
    /// whether anything is unread, so news arriving moves nothing.
    fn bell_shoulder(&self, index: usize, text: &mut TextRenderer, theme: &Theme) -> f64 {
        if !matches!(self.parts.get(index), Some(StatusPart::Bell { .. }))
            || index + 1 >= self.parts_shown()
        {
            return 0.0;
        }
        let (label_w, label_h) =
            text.measure_style(&badge_label(10), None, &Self::percentage_style(theme));
        let (width, _) = badge_size(label_w, label_h, theme);
        let (part, glyph) = (self.part(theme), Self::glyph(theme));
        let reach = (part + glyph) / 2.0 + BADGE_PAST_CORNER + width / 2.0;
        let next_glyph = part + (part - glyph) / 2.0;
        (reach + f64::from(theme.tokens.spacing.s1) / 2.0 - next_glyph)
            .max(0.0)
            .ceil()
    }

    /// The cluster's width: its pad each side, a square per part, and the battery's percentage
    /// where it has a reading.
    #[must_use]
    pub fn width(&self, text: &mut TextRenderer, theme: &Theme) -> f64 {
        let parts = self.parts_shown();
        let mut w = 2.0 * Self::pad(theme)
            + f64::from(u32::try_from(parts).unwrap_or(u32::MAX)) * self.part(theme)
            + (0..parts)
                .map(|index| self.bell_shoulder(index, text, theme))
                .sum::<f64>();
        if let Some(percent) = self.battery {
            let label = format!("{percent}%");
            let (label_w, _) = text.measure_style(&label, None, &Self::percentage_style(theme));
            w += f64::from(theme.tokens.spacing.rhythm.tight) + f64::from(label_w);
        }
        w.ceil().max(self.part(theme))
    }

    /// Whether the whole control should wear the danger wash: the battery is
    /// at or under [`StatusCluster::CRITICAL`].
    #[must_use]
    pub fn is_critical(&self) -> bool {
        self.battery
            .is_some_and(|percent| percent <= StatusCluster::CRITICAL)
    }

    /// Where the cluster at `rect` stands its parts and the bell's count, as
    /// [`Self::paint`] paints them: what a host places its targets by
    /// before anything is painted.
    #[must_use]
    pub fn layout(
        &self,
        text: &mut TextRenderer,
        theme: &Theme,
        rect: Rect,
    ) -> StatusClusterLayout {
        let mut layout = StatusClusterLayout {
            cluster: rect,
            ..StatusClusterLayout::default()
        };
        if rect.width() <= 0.0 || rect.height() <= 0.0 {
            return StatusClusterLayout::default();
        }
        let part = self.part(theme);
        let glyph = Self::glyph(theme);
        let cy = rect.center().y;
        let mut x = rect.x0 + Self::pad(theme);
        for (index, status) in self.parts.iter().take(MAX_PARTS).enumerate() {
            let cell = Rect::new(x, cy - part / 2.0, x + part, cy + part / 2.0);
            layout.parts[index] = cell;
            if let StatusPart::Bell { unread, .. } = *status
                && unread > 0
            {
                let square = Rect::from_center_size(cell.center(), (glyph, glyph));
                let label = badge_label(unread);
                let (label_w, label_h) =
                    text.measure_style(&label, None, &Self::percentage_style(theme));
                let (width, height) = badge_size(label_w, label_h, theme);
                // The count sits on the bell's top right corner, as an application icon's count
                // does: it covers only the corner, so the bell still reads as one, and it takes
                // none of the bar's width.
                let reach = f64::from(theme.tokens.spacing.s1);
                let centre_y = square.y0.max(rect.y0 - reach + height / 2.0);
                let centre_x = (square.x1 + BADGE_PAST_CORNER).min(rect.x1 + reach - width / 2.0);
                layout.badge =
                    Rect::from_center_size(Point::new(centre_x, centre_y), (width, height));
            }
            x += part + self.bell_shoulder(index, text, theme);
        }
        layout
    }

    /// Paint the cluster in `rect` (its width from [`Self::width`]) drawn
    /// on `ground`, and hand back where its parts landed.
    #[must_use]
    pub fn paint(&self, ctx: &mut PaintCtx<'_>, rect: Rect, ground: Color) -> StatusClusterLayout {
        let theme = ctx.theme;
        let colors = &theme.tokens.colors;
        if rect.width() <= 0.0 || rect.height() <= 0.0 {
            return StatusClusterLayout::default();
        }
        let control = BarControlFace::new("Status")
            .open(self.open)
            .pressed(self.pressed)
            .hovered(self.hovered)
            .focused(self.focused);
        let radius = f64::from(theme.tokens.radii.control).min(rect.height() / 2.0);
        let shape = vello::kurbo::RoundedRect::from_rect(rect, radius);
        if let Some(fill) = control.fill(theme, ground) {
            ctx.scene.fill(
                vello::peniko::Fill::NonZero,
                ctx.transform,
                fill,
                None,
                &shape,
            );
        }
        if self.is_critical() {
            ctx.scene.fill(
                vello::peniko::Fill::NonZero,
                ctx.transform,
                colors.semantic.danger.multiply_alpha(0.18),
                None,
                &shape,
            );
        }
        let lit = self.open || self.hovered || self.pressed;
        let dim = colors.ink_on(ground, colors.text_dim, 3.0);
        let bright = colors.ink_on(ground, colors.text_primary, 4.5);
        let low = colors.semantic.warning;
        let danger = colors.semantic.danger;
        let glyph = Self::glyph(theme);
        let cy = rect.center().y;
        let placed = self.layout(ctx.text, theme, rect);
        for (index, status) in self.parts.iter().take(MAX_PARTS).enumerate() {
            let cell = placed.parts[index];
            let square = Rect::from_center_size(cell.center(), (glyph, glyph));
            match *status {
                StatusPart::Bell { unread, quiet } => {
                    let ink = if unread == 0 && !quiet { dim } else { bright };
                    let icon = if quiet { Icon::BellOff } else { Icon::Bell };
                    icon.draw_with_stroke(
                        ctx.scene,
                        ctx.transform,
                        square,
                        ink,
                        LUCIDE_DEFAULT_STROKE_PX,
                    );
                    if unread > 0 {
                        let label = badge_label(unread);
                        let style = Self::percentage_style(theme);
                        let (label_w, label_h) = ctx.text.measure_style(&label, None, &style);
                        let badge = placed.badge;
                        let height = badge.height();
                        ctx.scene.fill(
                            vello::peniko::Fill::NonZero,
                            ctx.transform,
                            danger,
                            None,
                            &vello::kurbo::RoundedRect::from_rect(
                                badge,
                                crate::chrome::ChromeRole::Selector.radius(theme, height),
                            ),
                        );
                        let ink = colors.ink_on(danger, bright, 4.5);
                        ctx.text.draw_style(
                            ctx.scene,
                            &label,
                            ink,
                            ctx.transform.pre_translate(
                                (
                                    badge.x0 + (badge.width() - f64::from(label_w)) / 2.0,
                                    badge.y0 + (badge.height() - f64::from(label_h)) / 2.0,
                                )
                                    .into(),
                            ),
                            None,
                            &style,
                        );
                    }
                }
                StatusPart::Volume { silent } => {
                    let icon = if silent { Icon::VolumeX } else { Icon::Volume2 };
                    let ink = if silent { bright } else { dim };
                    icon.draw_with_stroke(
                        ctx.scene,
                        ctx.transform,
                        square,
                        ink,
                        LUCIDE_DEFAULT_STROKE_PX,
                    );
                }
                StatusPart::Link { kind, bars } => {
                    self.paint_link(ctx, square, kind, bars, (dim, low));
                }
                StatusPart::Battery { percent, charging } => {
                    let percent = self.battery.unwrap_or(percent);
                    paint_battery_gauge(
                        ctx.scene,
                        ctx.transform,
                        square,
                        percent,
                        self.charging || charging,
                        battery_ink(
                            percent,
                            charging,
                            (low, danger, colors.semantic.success, dim),
                        ),
                        colors,
                    );
                    let style = Self::percentage_style(theme);
                    let label = format!("{percent}%");
                    let (label_w, label_h) = ctx.text.measure_style(&label, None, &style);
                    let label_x = cell.x1 + f64::from(theme.tokens.spacing.rhythm.tight);
                    let ink = if percent <= StatusCluster::LOW {
                        low
                    } else {
                        bright
                    };
                    ctx.text.draw_style(
                        ctx.scene,
                        &label,
                        ink,
                        ctx.transform
                            .pre_translate((label_x, cy - f64::from(label_h) / 2.0).into()),
                        None,
                        &style,
                    );
                    let _ = label_w;
                }
                StatusPart::Glyph(icon) => {
                    icon.draw_with_stroke(
                        ctx.scene,
                        ctx.transform,
                        square,
                        dim,
                        LUCIDE_DEFAULT_STROKE_PX,
                    );
                }
            }
        }
        if self.focused {
            crate::widget::paint_focus_ring(
                ctx.scene,
                ctx.transform,
                rect,
                f64::from(theme.tokens.radii.control),
                theme,
            );
        }
        let _ = lit;
        placed
    }

    fn paint_link(
        &self,
        ctx: &mut PaintCtx<'_>,
        square: Rect,
        kind: LinkKind,
        bars: u8,
        (dim, warning): (Color, Color),
    ) {
        match kind {
            LinkKind::Wired => Icon::EthernetPort.draw_with_stroke(
                ctx.scene,
                ctx.transform,
                square,
                dim,
                LUCIDE_DEFAULT_STROKE_PX,
            ),
            LinkKind::Offline => Icon::WifiOff.draw_with_stroke(
                ctx.scene,
                ctx.transform,
                square,
                warning,
                LUCIDE_DEFAULT_STROKE_PX,
            ),
            LinkKind::Wifi => {
                let faint = dim.multiply_alpha(0.25);
                icons::draw_icon_layered(
                    ctx.scene,
                    ctx.transform,
                    square,
                    Icon::Wifi,
                    LUCIDE_DEFAULT_STROKE_PX,
                    |i| {
                        let needed = match i {
                            0 => 1,
                            1 => 4,
                            2 => 3,
                            _ => 2,
                        };
                        if bars >= needed { dim } else { faint }
                    },
                );
            }
        }
    }

    /// Where a cluster of `width` stands with its leading edge at `x` on a
    /// bar whose middle line is `cy`.
    #[must_use]
    pub fn place(x: f64, cy: f64, width: f64, theme: &Theme) -> Rect {
        Self::place_at(x, cy, width, Self::height(theme))
    }

    /// [`Self::place`] at `height` rather than the theme's bar control height.
    #[must_use]
    pub fn place_at(x: f64, cy: f64, width: f64, height: f64) -> Rect {
        Rect::new(x, cy - height / 2.0, x + width, cy + height / 2.0)
    }
}

/// The bell's count as it reads: `9+` once it needs a second digit, since
/// two digits would crowd the bar.
#[must_use]
pub fn badge_label(count: u32) -> String {
    if count > 9 {
        "9+".to_string()
    } else {
        count.to_string()
    }
}

/// A count badge's size: `s1` of padding above and below the line, `s1`
/// either side, never shorter than `s4` nor narrower than it is tall.
#[must_use]
pub fn badge_size(label_w: f32, label_h: f32, theme: &Theme) -> (f64, f64) {
    let s1 = f64::from(theme.tokens.spacing.s1);
    let s4 = f64::from(theme.tokens.spacing.s4);
    let h = (f64::from(label_h) + s1).max(s4).round();
    let w = (f64::from(label_w) + 2.0 * s1).max(h).round();
    (w, h)
}

fn battery_ink(
    percent: u8,
    charging: bool,
    (low, danger, success, dim): (Color, Color, Color, Color),
) -> Color {
    if percent <= StatusCluster::CRITICAL {
        danger
    } else if percent <= StatusCluster::LOW {
        low
    } else if charging {
        success
    } else {
        dim
    }
}

/// The battery gauge: lucide's `battery` body and its terminal nub, with a
/// charge fill inset one stroke inside the body, and a bolt while charging.
fn paint_battery_gauge(
    scene: &mut vello::Scene,
    transform: vello::kurbo::Affine,
    rect: Rect,
    percent: u8,
    charging: bool,
    hue: Color,
    colors: &crate::tokens::colors::Colors,
) {
    // lucide's battery is a 16×12 body at x=2, y=6 in a 24 box, with the
    // nub from x=22. Keep those ratios so the gauge lines up optically with
    // the neighbouring stroked glyphs.
    let s = rect.width().min(rect.height()) / 24.0;
    let ox = rect.x0 + (rect.width() - 24.0 * s) / 2.0;
    let oy = rect.y0 + (rect.height() - 24.0 * s) / 2.0;
    let px = |x: f64| ox + x * s;
    let py = |y: f64| oy + y * s;
    let stroke_w = LUCIDE_DEFAULT_STROKE_PX.max(1.0);
    let body = vello::kurbo::RoundedRect::new(px(2.0), py(6.0), px(18.0), py(18.0), 2.0 * s);
    scene.stroke(
        &vello::kurbo::Stroke::new(stroke_w),
        transform,
        colors.text_dim,
        None,
        &body,
    );
    scene.stroke(
        &vello::kurbo::Stroke::new(stroke_w).with_caps(vello::kurbo::Cap::Round),
        transform,
        colors.text_dim,
        None,
        &vello::kurbo::Line::new(
            Point::new(px(21.0), py(10.0)),
            Point::new(px(21.0), py(14.0)),
        ),
    );
    let inset = stroke_w + 0.5;
    let x0 = px(2.0) + inset;
    let x1 = px(18.0) - inset;
    let frac = (f64::from(percent) / 100.0).clamp(0.0, 1.0);
    if frac > 0.0 {
        // A hairline at 1 % would disappear; give any nonzero charge at
        // least a visible sliver, so "nearly empty" and "empty" differ.
        let w = ((x1 - x0) * frac).max(1.5);
        let fill = vello::kurbo::RoundedRect::new(
            x0,
            py(6.0) + inset,
            (x0 + w).min(x1),
            py(18.0) - inset,
            1.0 * s,
        );
        scene.fill(vello::peniko::Fill::NonZero, transform, hue, None, &fill);
    }
    if charging {
        let mut bolt = vello::kurbo::BezPath::new();
        bolt.move_to((px(11.5), py(7.5)));
        bolt.line_to((px(8.0), py(12.6)));
        bolt.line_to((px(11.0), py(12.6)));
        bolt.line_to((px(9.5), py(16.5)));
        bolt.line_to((px(13.5), py(11.0)));
        bolt.line_to((px(10.5), py(11.0)));
        bolt.close_path();
        scene.fill(
            vello::peniko::Fill::NonZero,
            transform,
            colors.text_primary,
            None,
            &bolt,
        );
        scene.stroke(
            &vello::kurbo::Stroke::new(0.75),
            transform,
            colors.surfaces.raised,
            None,
            &bolt,
        );
    }
}

/// The unread count on a running application's own icon: the badge a
/// sidebar's unread count wears, filled in the person's colour.
pub fn paint_unread_badge(
    scene: &mut vello::Scene,
    transform: vello::kurbo::Affine,
    theme: &Theme,
    text: &mut TextRenderer,
    icon: Rect,
    count: u32,
) {
    let label = count.to_string();
    let height = BadgeFace::height(theme);
    let width = BadgeFace::intrinsic_width(&label, text, theme);
    let centre = Point::new(icon.x1 + 2.0, icon.y0 + 2.0);
    BadgeFace::new(Rect::from_center_size(centre, (width, height)), &label)
        .style(ChipStyle::Live)
        .paint(scene, transform, text, theme);
}

#[cfg(test)]
mod tests {
    use super::*;

    tilo_test_env::scenario! {
        fn a_status_cluster_draws_its_parts_glyphs_and_counts;



        phase the_badge_counts_nine_and_then_says_nine_plus;
        {
            assert_eq!(badge_label(7), "7");
            assert_eq!(badge_label(120), "9+");
        }


        // A weak but connected radio shows one bar rather than reading exactly
        // like an offline machine; a cable has no bars to show.
        phase a_connected_radio_always_shows_at_least_one_bar;
        {
            let status = |kind, signal| LinkStatus {
                kind,
                name: None,
                signal,
            };
            assert_eq!(status(LinkKind::Wifi, 0).bars(), 1);
            assert_eq!(status(LinkKind::Wifi, 3).bars(), 3);
            assert_eq!(status(LinkKind::Wifi, 9).bars(), 4);
            assert_eq!(status(LinkKind::Wired, 4).bars(), 0);
            assert_eq!(status(LinkKind::Offline, 0).bars(), 0);
        }





    }
}
