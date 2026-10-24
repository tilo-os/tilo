//! `SignalMeter`: how strong a radio signal is (a Wi-Fi network, a mobile
//! network), as four bars of rising height.
//!
//! The bars the level reaches are filled in the accent ink; the rest are the
//! muted track a progress bar's unfilled part is, so the meter reads on every
//! material and in high contrast. The meter names itself by its level
//! ("Strong signal"), the name a screen reader announces and the tooltip a
//! list row shows for its leading column.
//!
//! [`SignalMeterFace`] paints the same meter into a rect for a host that
//! paints immediately (a list row's leading column, the quick settings
//! panel); [`SignalMeter`] is the retained widget, one icon square in size.

use std::collections::HashMap;

use taffy::{NodeId, Size, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Rect, RoundedRect};
use vello::peniko::Fill;

use crate::presence::PresenceCtx;
use crate::text_render::TextRenderer;
use crate::theme::Theme;
use crate::widget::{TaffyTreeExt, Widget};

/// How many bars a meter has.
pub const SIGNAL_BARS: u8 = 4;

/// How strong a signal is: the level the meter fills to.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum SignalLevel {
    /// The strength is not known; no bar is filled.
    #[default]
    Unknown,
    Weak,
    Fair,
    Good,
    Strong,
}

impl SignalLevel {
    /// The level of a strength bucket `0..=4` (what `NetworkManager`'s
    /// percentage is bucketed to): 0 is unknown, 4 and above strong.
    #[must_use]
    pub const fn from_bars(bars: u8) -> Self {
        match bars {
            0 => Self::Unknown,
            1 => Self::Weak,
            2 => Self::Fair,
            3 => Self::Good,
            _ => Self::Strong,
        }
    }

    /// How many of the [`SIGNAL_BARS`] bars it fills.
    #[must_use]
    pub const fn filled(self) -> u8 {
        match self {
            Self::Unknown => 0,
            Self::Weak => 1,
            Self::Fair => 2,
            Self::Good => 3,
            Self::Strong => 4,
        }
    }

    /// The meter's name: "Strong signal", in the interface language.
    #[must_use]
    pub fn name(self) -> String {
        crate::i18n!(match self {
            Self::Unknown => "signal-meter-unknown",
            Self::Weak => "signal-meter-weak",
            Self::Fair => "signal-meter-fair",
            Self::Good => "signal-meter-good",
            Self::Strong => "signal-meter-strong",
        })
    }
}

/// The meter painted into a rect: four bars standing on its bottom edge,
/// centred in the largest square the rect holds.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SignalMeterFace {
    level: SignalLevel,
}

impl SignalMeterFace {
    #[must_use]
    pub const fn new(level: SignalLevel) -> Self {
        Self { level }
    }

    /// The four bars in `rect`, shortest first: equal widths and gaps, the
    /// tallest the square's full height, each a quarter shorter than the
    /// next.
    #[must_use]
    pub fn bars(rect: Rect) -> [Rect; SIGNAL_BARS as usize] {
        let side = rect.width().min(rect.height()).max(0.0);
        let square = Rect::from_center_size(rect.center(), (side, side));
        // Three gaps of half a bar between four bars: 4w + 3(w/2) = side.
        let width = side / 5.5;
        let gap = width / 2.0;
        std::array::from_fn(|index| {
            #[allow(
                clippy::cast_precision_loss,
                reason = "an index below four is exact in f64"
            )]
            let at = index as f64;
            let height = side * (at + 1.0) / f64::from(SIGNAL_BARS);
            let x0 = square.x0 + at * (width + gap);
            Rect::new(x0, square.y1 - height, x0 + width, square.y1)
        })
    }

    /// Paint the meter into `rect`.
    pub fn paint(&self, scene: &mut Scene, transform: Affine, theme: &Theme, rect: Rect) {
        if rect.width() <= 0.0 || rect.height() <= 0.0 {
            return;
        }
        let colors = &theme.tokens.colors.intent;
        for (index, bar) in Self::bars(rect).into_iter().enumerate() {
            let lit = index < usize::from(self.level.filled());
            let color = if lit {
                colors.accent_ink
            } else {
                colors.selection
            };
            let radius = (bar.width() / 2.0).min(f64::from(theme.tokens.radii.detail));
            scene.fill(
                Fill::NonZero,
                transform,
                color,
                None,
                &RoundedRect::from_rect(bar, radius),
            );
        }
    }
}

/// The retained meter: one icon square in a row, named by its level.
pub struct SignalMeter {
    level: SignalLevel,
    rect: Rect,
}

impl SignalMeter {
    #[must_use]
    pub const fn new(level: SignalLevel) -> Self {
        Self {
            level,
            rect: Rect::ZERO,
        }
    }

    /// The level it shows.
    #[must_use]
    pub const fn level(&self) -> SignalLevel {
        self.level
    }
}

impl Widget for SignalMeter {
    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let edge = theme.tokens.metrics.icon_md;
        let style = Style {
            size: Size::from_lengths(edge, edge),
            flex_shrink: 0.0,
            ..Default::default()
        };
        taffy.widget_leaf(style, node_map, self)
    }

    crate::rect_geometry!();

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        _text_renderer: &mut TextRenderer,
        theme: &Theme,
        _presence: &PresenceCtx,
    ) {
        SignalMeterFace::new(self.level).paint(scene, transform, theme, self.rect);
    }

    fn name(&self) -> &'static str {
        "SignalMeter"
    }

    fn is_layout_dirty(&self) -> bool {
        self.rect == Rect::ZERO
    }

    fn accessibility(&self) -> Option<crate::a11y::AccessNode> {
        use crate::a11y::{AccessNode, Role};
        Some(
            AccessNode::new(Role::Image)
                .with_name(self.level.name())
                .with_value(format!("{}/{SIGNAL_BARS}", self.level.filled())),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use vello_encoding::DrawTag;

    #[test]
    fn a_strength_bucket_fills_as_many_bars_and_names_its_level() {
        let levels: Vec<(u8, &str)> = (0..=5)
            .map(|bars| {
                let level = SignalLevel::from_bars(bars);
                (level.filled(), level_key(level))
            })
            .collect();
        assert_eq!(
            levels,
            [
                (0, "unknown"),
                (1, "weak"),
                (2, "fair"),
                (3, "good"),
                (4, "strong"),
                (4, "strong"),
            ]
        );
        assert_eq!(SignalLevel::Strong.name(), "Strong signal");
        assert_eq!(SignalLevel::Weak.name(), "Weak signal");
    }

    fn level_key(level: SignalLevel) -> &'static str {
        match level {
            SignalLevel::Unknown => "unknown",
            SignalLevel::Weak => "weak",
            SignalLevel::Fair => "fair",
            SignalLevel::Good => "good",
            SignalLevel::Strong => "strong",
        }
    }

    #[test]
    fn the_bars_rise_left_to_right_on_one_baseline_inside_the_square() {
        let rect = Rect::new(10.0, 10.0, 40.0, 30.0);
        let bars = SignalMeterFace::bars(rect);
        let square = Rect::from_center_size(rect.center(), (20.0, 20.0));
        for pair in bars.windows(2) {
            let [shorter, taller] = pair else { continue };
            assert!(taller.height() > shorter.height());
            assert!(taller.x0 > shorter.x1, "bars do not touch");
            assert!((taller.width() - shorter.width()).abs() < 1e-9);
        }
        for bar in &bars {
            assert!((bar.y1 - square.y1).abs() < 1e-9, "one baseline");
            assert!(bar.x0 >= square.x0 - 1e-9 && bar.x1 <= square.x1 + 1e-9);
        }
        assert!(
            (bars[3].height() - 20.0).abs() < 1e-9,
            "the tallest fills it"
        );
    }

    /// Lit bars are the accent ink, the others the muted track: the colours
    /// of the four fills follow the level.
    #[test]
    fn the_level_lights_its_bars_in_the_accent_and_leaves_the_rest_as_track() {
        let theme = Theme::dark();
        for level in [SignalLevel::Unknown, SignalLevel::Fair, SignalLevel::Strong] {
            let mut scene = Scene::new();
            SignalMeterFace::new(level).paint(
                &mut scene,
                Affine::IDENTITY,
                &theme,
                Rect::new(0.0, 0.0, 20.0, 20.0),
            );
            let fills = scene
                .encoding()
                .draw_tags
                .iter()
                .filter(|tag| **tag == DrawTag::COLOR)
                .count();
            assert_eq!(fills, usize::from(SIGNAL_BARS), "every bar is drawn");
        }
        assert_ne!(
            theme.tokens.colors.intent.accent_ink, theme.tokens.colors.intent.selection,
            "lit and unlit bars differ"
        );
    }

    #[test]
    fn the_retained_meter_is_named_by_its_level() {
        let meter = SignalMeter::new(SignalLevel::Good);
        let node = meter.accessibility().expect("the meter is announced");
        assert_eq!(node.name.as_deref(), Some("Good signal"));
        assert_eq!(node.value.as_deref(), Some("3/4"));
    }
}
