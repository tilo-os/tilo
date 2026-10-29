//! How wide a widget's open card may be, and how its content changes with
//! the width.
//!
//! Every widget's card is resizable between its own narrowest and widest
//! ([`Sizing`]), and lays its content out in one of three stages by width
//! ([`Stage`]): compact, regular and large. Around each breakpoint there
//! is a band ([`BLEND`]) in which the two neighbouring stages cross-fade
//! while the card's height moves smoothly from one stage's to the other's,
//! so dragging the resize handle through a breakpoint reflows the content
//! continuously: nothing jumps, at any width.

#![allow(
    clippy::arithmetic_side_effects,
    reason = "widths are finite logical pixels"
)]

use super::WidgetId;

/// The width band around a breakpoint across which two stages cross-fade:
/// wide enough that a card twice as tall in the next stage still grows
/// only a few pixels for each pixel of drag.
pub const BLEND: f64 = 2.0 * ui::tokens::Spacing::BASE.s8 as f64;

/// One of a card's layouts by width.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Stage {
    /// The essentials on one or two lines.
    Compact,
    /// The card as it opens by default.
    Regular,
    /// Room for more: larger artwork, larger readouts, more rows.
    Large,
}

/// A widget's widths: its narrowest, the width it opens at until resized,
/// its widest, and where compact turns regular and regular turns large.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Sizing {
    pub min: f64,
    pub default: f64,
    pub max: f64,
    pub regular_at: f64,
    pub large_at: f64,
}

impl Sizing {
    /// `width` kept between the narrowest and the widest.
    #[must_use]
    pub fn clamp(self, width: f64) -> f64 {
        width.clamp(self.min, self.max)
    }
}

/// `id`'s widths.
#[must_use]
pub fn sizing(id: WidgetId) -> Sizing {
    let card = f64::from(ui::tokens::Metrics::BASE.panel_width_utility);
    match id {
        WidgetId::Music => Sizing {
            min: 240.0,
            default: card,
            max: 520.0,
            regular_at: 280.0,
            large_at: 420.0,
        },
        WidgetId::Team | WidgetId::Calendar => Sizing {
            min: 220.0,
            default: card,
            max: 480.0,
            regular_at: 260.0,
            large_at: 400.0,
        },
        WidgetId::SystemMonitor => Sizing {
            min: 200.0,
            default: card,
            max: 460.0,
            regular_at: 250.0,
            large_at: 380.0,
        },
    }
}

/// How far into its large stage a card `width` wide is: 0 where the large
/// stage starts fading in, 1 at the widest. What grows with it (the music
/// card's artwork) starts from nothing, so the stage arrives without a
/// jump.
#[must_use]
pub fn into_large(id: WidgetId, width: f64) -> f64 {
    let sizing = sizing(id);
    let from = sizing.large_at - BLEND / 2.0;
    ((width - from) / (sizing.max - from).max(1.0)).clamp(0.0, 1.0)
}

/// The stages a card `width` wide shows, and how far from the first to the
/// second it is (`0..=1`, eased): one stage alone (`(s, s, 0.0)`) outside
/// the blend bands, two cross-fading inside one.
#[must_use]
pub fn stages(id: WidgetId, width: f64) -> (Stage, Stage, f64) {
    let sizing = sizing(id);
    let half = BLEND / 2.0;
    for (at, lo, hi) in [
        (sizing.regular_at, Stage::Compact, Stage::Regular),
        (sizing.large_at, Stage::Regular, Stage::Large),
    ] {
        if (width - at).abs() < half {
            let t = (width - (at - half)) / BLEND;
            return (lo, hi, smoothstep(t));
        }
    }
    let stage = if width < sizing.regular_at {
        Stage::Compact
    } else if width < sizing.large_at {
        Stage::Regular
    } else {
        Stage::Large
    };
    (stage, stage, 0.0)
}

/// The stage whose controls take a press at `width`: the one more shown.
#[must_use]
pub fn dominant(id: WidgetId, width: f64) -> Stage {
    let (lo, hi, t) = stages(id, width);
    if t < 0.5 { lo } else { hi }
}

/// The card's height at `width`, given each stage's height there:
/// continuous across the breakpoints.
#[must_use]
pub fn height(id: WidgetId, width: f64, stage_height: impl Fn(Stage, f64) -> f64) -> f64 {
    let (lo, hi, t) = stages(id, width);
    if lo == hi {
        return stage_height(lo, width);
    }
    let a = stage_height(lo, width);
    let b = stage_height(hi, width);
    a + (b - a) * t
}

fn smoothstep(t: f64) -> f64 {
    let t = t.clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Heights that jump at each breakpoint, as content does.
    fn stepped(stage: Stage, width: f64) -> f64 {
        match stage {
            Stage::Compact => 72.0,
            Stage::Regular => 140.0 + width * 0.1,
            Stage::Large => 150.0 + width * 0.2,
        }
    }

    #[test]
    fn resizing_through_every_breakpoint_is_continuous() {
        for id in WidgetId::ALL {
            let sizing = sizing(id);
            assert!(sizing.min < sizing.regular_at && sizing.regular_at < sizing.large_at);
            assert!(sizing.large_at < sizing.max);
            assert!((sizing.min..=sizing.max).contains(&sizing.default));
            let mut last = height(id, sizing.min, stepped);
            let mut seen = std::collections::BTreeSet::new();
            let widths = std::iter::successors(Some(sizing.min), |width| Some(width + 0.25));
            for width in widths.take_while(|width| *width <= sizing.max) {
                let h = height(id, width, stepped);
                // A quarter-pixel step never moves the height more than a
                // couple of pixels: no jump anywhere.
                assert!(
                    (h - last).abs() < 2.5,
                    "{id:?} jumps at {width}: {last} -> {h}"
                );
                let (lo, hi, t) = stages(id, width);
                assert!((0.0..=1.0).contains(&t));
                seen.insert(lo);
                seen.insert(hi);
                last = h;
            }
            assert_eq!(seen.len(), 3, "{id:?} passes all three stages");
        }
    }

    #[test]
    fn outside_the_bands_one_stage_shows_alone() {
        let sizing = sizing(WidgetId::Music);
        assert_eq!(
            stages(WidgetId::Music, sizing.min),
            (Stage::Compact, Stage::Compact, 0.0)
        );
        assert_eq!(stages(WidgetId::Music, sizing.default).0, Stage::Regular);
        assert_eq!(
            stages(WidgetId::Music, sizing.max),
            (Stage::Large, Stage::Large, 0.0)
        );
        let (lo, hi, t) = stages(WidgetId::Music, sizing.large_at);
        assert_eq!((lo, hi), (Stage::Regular, Stage::Large));
        assert!((t - 0.5).abs() < 1e-9, "half way at the breakpoint");
    }
}
