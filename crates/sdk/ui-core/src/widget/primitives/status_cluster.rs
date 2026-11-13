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
pub enum StatusSlot {
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
    /// The signal as the status cluster's link slot counts it: `1..=4` on
    /// Wi-Fi, `0` where it has no bars to draw.
    #[must_use]
    pub fn bars(&self) -> u8 {
        match self.kind {
            LinkKind::Wifi => self.signal.clamp(1, 4),
            LinkKind::Offline | LinkKind::Wired => 0,
        }
    }
}

/// The cluster's one control: its slots, the state they share and how it
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
    slots: &'a [StatusSlot],
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
    /// Each slot's glyph square, in the order they were given.
    pub slots: [Rect; MAX_SLOTS],
    pub badge: Rect,
}

/// How far past its glyph's top right corner the bell's count is centred.
// ratchet-allow(pixel_const): an offset from a glyph's corner, a fraction of the glyph square
// rather than spacing
const BADGE_PAST_CORNER: f64 = 3.0;

/// The most slots a cluster takes: the bell, the volume, the link, the battery, and one glyph of
/// room.
pub const MAX_SLOTS: usize = 5;

impl<'a> StatusClusterFace<'a> {
    /// The cluster of `slots`, at the battery reading it shares.
    #[must_use]
    pub fn new(slots: &'a [StatusSlot], status: StatusCluster) -> Self {
        Self {
            slots,
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
    /// toolkit's default (the desktop's is 24) keeps one top and bottom
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

    fn slot(&self, theme: &Theme) -> f64 {
        self.height.unwrap_or_else(|| Self::height(theme))
    }

    fn pad(theme: &Theme) -> f64 {
        f64::from(theme.tokens.spacing.rhythm.tight)
    }

    /// A slot's glyph: the size every bar control's glyph is drawn at
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

    fn slots_shown(&self) -> usize {
        self.slots.len().min(MAX_SLOTS)
    }

    /// The room after the bell, when a slot follows it: as much as its widest count reaches past
    /// it, and a hairline more, so a count never stands on the glyph beside the bell. The same
    /// whether anything is unread, so news arriving moves nothing.
    fn bell_shoulder(&self, index: usize, text: &mut TextRenderer, theme: &Theme) -> f64 {
        if !matches!(self.slots.get(index), Some(StatusSlot::Bell { .. }))
            || index + 1 >= self.slots_shown()
        {
            return 0.0;
        }
        let (label_w, label_h) =
            text.measure_style(&badge_label(10), None, &Self::percentage_style(theme));
        let (width, _) = badge_size(label_w, label_h, theme);
        let (slot, glyph) = (self.slot(theme), Self::glyph(theme));
        let reach = (slot + glyph) / 2.0 + BADGE_PAST_CORNER + width / 2.0;
        let next_glyph = slot + (slot - glyph) / 2.0;
        (reach + f64::from(theme.tokens.spacing.s1) / 2.0 - next_glyph)
            .max(0.0)
            .ceil()
    }

    /// The cluster's width: its pad each side, a square per slot, and the battery's percentage
    /// where it has a reading.
    #[must_use]
    pub fn width(&self, text: &mut TextRenderer, theme: &Theme) -> f64 {
        let slots = self.slots_shown();
        let mut w = 2.0 * Self::pad(theme)
            + f64::from(u32::try_from(slots).unwrap_or(u32::MAX)) * self.slot(theme)
            + (0..slots)
                .map(|index| self.bell_shoulder(index, text, theme))
                .sum::<f64>();
        if let Some(percent) = self.battery {
            let label = format!("{percent}%");
            let (label_w, _) = text.measure_style(&label, None, &Self::percentage_style(theme));
            w += f64::from(theme.tokens.spacing.rhythm.tight) + f64::from(label_w);
        }
        w.ceil().max(self.slot(theme))
    }

    /// Whether the whole control should wear the danger wash: the battery is
    /// at or under [`StatusCluster::CRITICAL`].
    #[must_use]
    pub fn is_critical(&self) -> bool {
        self.battery
            .is_some_and(|percent| percent <= StatusCluster::CRITICAL)
    }

    /// Where the cluster at `rect` stands its slots and the bell's count, as
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
        let slot = self.slot(theme);
        let glyph = Self::glyph(theme);
        let cy = rect.center().y;
        let mut x = rect.x0 + Self::pad(theme);
        for (index, status) in self.slots.iter().take(MAX_SLOTS).enumerate() {
            let cell = Rect::new(x, cy - slot / 2.0, x + slot, cy + slot / 2.0);
            layout.slots[index] = cell;
            if let StatusSlot::Bell { unread, .. } = *status
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
            x += slot + self.bell_shoulder(index, text, theme);
        }
        layout
    }

    /// Paint the cluster in `rect` (its width from [`Self::width`]) standing
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
        for (index, status) in self.slots.iter().take(MAX_SLOTS).enumerate() {
            let cell = placed.slots[index];
            let square = Rect::from_center_size(cell.center(), (glyph, glyph));
            match *status {
                StatusSlot::Bell { unread, quiet } => {
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
                StatusSlot::Volume { silent } => {
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
                StatusSlot::Link { kind, bars } => {
                    self.paint_link(ctx, square, kind, bars, (dim, low));
                }
                StatusSlot::Battery { percent, charging } => {
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
                StatusSlot::Glyph(icon) => {
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
    use vello::kurbo::Affine;

    const BELL: StatusSlot = StatusSlot::Bell {
        unread: 3,
        quiet: false,
    };

    fn paint(slots: &[StatusSlot], status: StatusCluster) -> (StatusClusterLayout, usize) {
        let theme = Theme::dark();
        let mut text = TextRenderer::new();
        let face = StatusClusterFace::new(slots, status);
        let width = face.width(&mut text, &theme);
        let rect = Rect::new(0.0, 0.0, width, StatusClusterFace::height(&theme));
        let mut scene = vello::Scene::new();
        let layout = {
            let mut ctx = PaintCtx::new(&mut scene, Affine::IDENTITY, &mut text, &theme);
            face.paint(&mut ctx, rect, theme.tokens.colors.surfaces.base)
        };
        (layout, scene.encoding().draw_tags.len())
    }

    #[test]
    fn a_cluster_is_as_wide_as_its_slots_and_grows_for_the_percentage() {
        let theme = Theme::dark();
        let mut text = TextRenderer::new();
        let bell = StatusClusterFace::new(&[BELL], StatusCluster::new());
        let bell_and_battery = StatusClusterFace::new(
            &[
                BELL,
                StatusSlot::Battery {
                    percent: 82,
                    charging: true,
                },
            ],
            StatusCluster::new().battery(82, true),
        );
        assert!(
            bell_and_battery.width(&mut text, &theme) > bell.width(&mut text, &theme),
            "the percentage takes room past the gauge"
        );
    }

    #[test]
    fn the_critical_battery_wears_the_danger_wash_on_the_whole_control() {
        let slots = [StatusSlot::Battery {
            percent: 3,
            charging: false,
        }];
        assert!(
            StatusClusterFace::new(&slots, StatusCluster::new().battery(3, false)).is_critical()
        );
        assert!(
            !StatusClusterFace::new(&slots, StatusCluster::new().battery(40, false)).is_critical()
        );
    }

    #[test]
    fn the_badge_counts_nine_and_then_says_nine_plus() {
        assert_eq!(badge_label(7), "7");
        assert_eq!(badge_label(120), "9+");
    }

    fn paths(slots: &[StatusSlot]) -> vello::Scene {
        let theme = Theme::dark();
        let mut text = TextRenderer::new();
        let face = StatusClusterFace::new(slots, StatusCluster::new());
        let width = face.width(&mut text, &theme);
        let rect = Rect::new(0.0, 0.0, width, StatusClusterFace::height(&theme));
        let mut scene = vello::Scene::new();
        let mut ctx = PaintCtx::new(&mut scene, Affine::IDENTITY, &mut text, &theme);
        let _ = face.paint(&mut ctx, rect, theme.tokens.colors.surfaces.base);
        scene
    }

    /// The link slot draws what kind of connection the machine has: a cable's
    /// port, Wi-Fi's arcs, or the crossed-out arcs when nothing is connected.
    #[test]
    fn the_link_slot_draws_the_glyph_of_its_kind() {
        assert_eq!(LinkKind::Wired.icon(), Icon::EthernetPort);
        assert_eq!(LinkKind::Wifi.icon(), Icon::Wifi);
        assert_eq!(LinkKind::Offline.icon(), Icon::WifiOff);
        let of = |kind, bars| paths(&[StatusSlot::Link { kind, bars }]);
        let (wired, wifi, offline) = (
            of(LinkKind::Wired, 0),
            of(LinkKind::Wifi, 4),
            of(LinkKind::Offline, 0),
        );
        assert!(!wired.encoding_eq(&wifi), "a cable is not Wi-Fi");
        assert!(
            !wifi.encoding_eq(&offline),
            "nothing connected is not Wi-Fi"
        );
        assert!(!wired.encoding_eq(&offline), "a cable is not nothing");
    }

    /// A weak but connected radio shows one bar rather than reading exactly
    /// like an offline machine; a cable has no bars to show.
    #[test]
    fn a_connected_radio_always_shows_at_least_one_bar() {
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

    /// A count takes no width: nothing unread leaves no empty room beside the
    /// bell, and news arriving never moves what stands beside the cluster.
    #[test]
    fn a_count_takes_no_room() {
        let theme = Theme::dark();
        let mut text = TextRenderer::new();
        let mut width = |slots: &[StatusSlot]| {
            StatusClusterFace::new(slots, StatusCluster::new()).width(&mut text, &theme)
        };
        let resting = width(&[StatusSlot::Bell {
            unread: 0,
            quiet: false,
        }]);
        for unread in [1, 9, 42, 120] {
            assert_eq!(
                width(&[StatusSlot::Bell {
                    unread,
                    quiet: false
                }]),
                resting,
                "{unread} unread"
            );
        }
        let glyph = width(&[StatusSlot::Glyph(Icon::Bell)]);
        assert_eq!(resting, glyph, "the bell is as wide as any one glyph");
        let (layout, _) = paint(&[BELL], StatusCluster::new());
        assert!(
            layout.badge.overlaps(layout.slots[0]),
            "the count sits on the bell: {:?} on {:?}",
            layout.badge,
            layout.slots[0]
        );
    }

    /// The widest count stands clear of the glyph after the bell, and the room it keeps there is
    /// the same with nothing unread: the bar's volume is never covered.
    #[test]
    fn the_count_never_reaches_the_next_glyph() {
        let theme = Theme::dark();
        let glyph = StatusClusterFace::glyph(&theme);
        let volume = StatusSlot::Glyph(Icon::Volume2);
        for unread in [1, 9, 120] {
            let bell = StatusSlot::Bell {
                unread,
                quiet: false,
            };
            let (layout, _) = paint(&[bell, volume], StatusCluster::new());
            let next = Rect::from_center_size(layout.slots[1].center(), (glyph, glyph));
            assert!(
                layout.badge.x1 < next.x0,
                "{unread} unread: the count {:?} reaches the volume {next:?}",
                layout.badge
            );
        }
        let (quiet, _) = paint(
            &[
                StatusSlot::Bell {
                    unread: 0,
                    quiet: false,
                },
                volume,
            ],
            StatusCluster::new(),
        );
        let (busy, _) = paint(&[BELL, volume], StatusCluster::new());
        assert_eq!(quiet.slots[1], busy.slots[1], "news arriving moves nothing");
    }

    /// The bell is drawn at the glyph size, and in the stroke, of every other bar control, so
    /// it reads as one set with the people and invite controls beside it.
    #[test]
    fn the_bell_shares_the_bar_controls_glyph() {
        let theme = Theme::dark();
        assert_eq!(
            StatusClusterFace::glyph(&theme),
            f64::from(theme.tokens.metrics.icon_control)
        );
    }

    /// The count covers the bell's corner, not the bell: most of the glyph stays in view, and the
    /// count stays within the room the bar leaves around its controls.
    #[test]
    fn the_count_leaves_the_bell_in_view() {
        let theme = Theme::dark();
        let (layout, _) = paint(&[BELL], StatusCluster::new());
        let glyph = StatusClusterFace::glyph(&theme);
        let bell = Rect::from_center_size(layout.slots[0].center(), (glyph, glyph));
        let covered = layout.badge.intersect(bell);
        let share = covered.area() / bell.area();
        assert!(
            share <= 0.3,
            "the count covers {:.0}% of the bell: {:?} on {bell:?}",
            share * 100.0,
            layout.badge
        );
        let reach = f64::from(theme.tokens.spacing.s1);
        let control = StatusClusterFace::height(&theme);
        assert!(
            layout.badge.y0 >= -reach - 0.5 && layout.badge.y1 <= control + reach,
            "the count stays within the bar: {:?}",
            layout.badge
        );
    }

    #[test]
    fn a_bell_with_a_count_paints_its_badge() {
        let (layout, _) = paint(&[BELL], StatusCluster::new());
        assert!(layout.badge.width() > 0.0, "the count sits on the bell");
        let (quiet, _) = paint(
            &[StatusSlot::Bell {
                unread: 0,
                quiet: true,
            }],
            StatusCluster::new(),
        );
        assert!(quiet.badge.is_zero_area(), "nothing unread: no badge");
    }
}
