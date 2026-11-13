//! How the shared components answer the pointer: the hover and press response, as a theme
//! role (`docs/design-language.md`, "Lifts"; `docs/architecture/theming.md`, the
//! `response` role). A pointed-at control lifts off its ground; a pressed one presses
//! into it. An app never picks one: it says a control is hovered or pressed, and the
//! theme's response draws it.

use serde::{Deserialize, Serialize};

/// One state's response: the four levers a theme may move.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResponseStep {
    /// How far the fill steps toward the next surface (`1` one surface step,
    /// [`crate::tokens::Colors::lift_on`]).
    pub lift: f32,
    /// How much of the accent washes into the lift, `0..=0.25`.
    pub tint: f32,
    /// The control's scale about its own centre; below `1` it presses in.
    pub scale: f32,
    /// The lit inner edge's opacity, `0..=0.5`; `0` draws none.
    pub glow: f32,
}

impl ResponseStep {
    /// How far a lift may step, in surface steps: never none, because a theme cannot
    /// remove the hover and press lift (`docs/design-language.md`, "Theming").
    pub const LIFT_ADMITTED: std::ops::RangeInclusive<f32> = 0.25..=2.0;
    /// How much accent a lift may carry.
    pub const TINT_ADMITTED: std::ops::RangeInclusive<f32> = 0.0..=0.25;
    /// How far a control may scale about its centre.
    pub const SCALE_ADMITTED: std::ops::RangeInclusive<f32> = 0.7..=1.15;
    /// How strongly the lit inner edge may draw.
    pub const GLOW_ADMITTED: std::ops::RangeInclusive<f32> = 0.0..=0.5;

    /// The stock hover lift, half a surface step.
    pub const HOVER_LIFT: f32 = 0.5;
    pub const PRESS_SCALE: f32 = 0.97;

    /// The stock idle response: no lift, no tint, no scale, no glow.
    pub const REST: Self = Self {
        lift: 0.0,
        tint: 0.0,
        scale: 1.0,
        glow: 0.0,
    };

    /// Every lever finite and inside its admitted band.
    pub fn validate(&self) -> Result<(), &'static str> {
        let within = |value: f32, band: &std::ops::RangeInclusive<f32>| {
            value.is_finite() && band.contains(&value)
        };
        if !within(self.lift, &Self::LIFT_ADMITTED) {
            return Err("response lift outside 0.25..=2");
        }
        if !within(self.tint, &Self::TINT_ADMITTED) {
            return Err("response tint outside 0..=0.25");
        }
        if !within(self.scale, &Self::SCALE_ADMITTED) {
            return Err("response scale outside 0.7..=1.15");
        }
        if !within(self.glow, &Self::GLOW_ADMITTED) {
            return Err("response glow outside 0..=0.5");
        }
        Ok(())
    }

    /// This step between the rest and `self` at `t` (`0` at rest, `1` arrived), as the
    /// part motion's progress moves it.
    #[must_use]
    pub fn at(&self, t: f32) -> Self {
        let t = t.clamp(0.0, 1.0);
        let between = |from: f32, to: f32| from + (to - from) * t;
        Self {
            lift: between(Self::REST.lift, self.lift),
            tint: between(Self::REST.tint, self.tint),
            scale: between(Self::REST.scale, self.scale),
            glow: between(Self::REST.glow, self.glow),
        }
    }

    /// Whether nothing moves for this step: the rest state.
    #[must_use]
    pub fn is_rest(&self) -> bool {
        self.lift.abs() <= f32::EPSILON
            && self.tint.abs() <= f32::EPSILON
            && (self.scale - 1.0).abs() <= f32::EPSILON
            && self.glow.abs() <= f32::EPSILON
    }
}

impl Default for ResponseStep {
    fn default() -> Self {
        Self::REST
    }
}

/// The hover and press response of the shared components.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Response {
    pub hover: ResponseStep,
    pub press: ResponseStep,
}

impl Response {
    /// The design language's response: a hover lifts half a surface step, a press a whole
    /// one and sinks to 0.97 with a lit edge (the stock glow's `0.2` is the chrome's
    /// 51-of-255 inner hairline).
    pub const STOCK: Self = Self {
        hover: ResponseStep {
            lift: ResponseStep::HOVER_LIFT,
            tint: 0.0,
            scale: 1.0,
            glow: 0.0,
        },
        press: ResponseStep {
            lift: 1.0,
            tint: 0.0,
            scale: ResponseStep::PRESS_SCALE,
            glow: 0.2,
        },
    };

    /// The response of a theme that holds still (`motion.curve: none`, as block does):
    /// the same lifts, nothing scaling and no lit edge, because nothing there moves.
    pub const STILL: Self = Self {
        hover: ResponseStep {
            lift: ResponseStep::HOVER_LIFT,
            tint: 0.0,
            scale: 1.0,
            glow: 0.0,
        },
        press: ResponseStep {
            lift: 1.0,
            tint: 0.0,
            scale: 1.0,
            glow: 0.0,
        },
    };

    /// The step for the pointer states `hovered` and `pressed`; a press answers with the
    /// press step whatever else is true.
    #[must_use]
    pub fn step(&self, hovered: bool, pressed: bool) -> ResponseStep {
        if pressed {
            self.press
        } else if hovered {
            self.hover
        } else {
            ResponseStep::REST
        }
    }

    /// [`Self::step`] `progress` of the way there, as a part's motion runs.
    #[must_use]
    pub fn moment(&self, hovered: bool, pressed: bool, progress: f32) -> ResponseStep {
        self.step(hovered, pressed).at(progress)
    }

    pub fn validate(&self) -> Result<(), &'static str> {
        self.hover.validate()?;
        self.press.validate()
    }
}

impl Default for Response {
    fn default() -> Self {
        Self::STOCK
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_stock_response_keeps_the_design_languages_lifts() {
        let stock = Response::STOCK;
        assert_eq!(stock.validate(), Ok(()));
        let hover = stock.step(true, false);
        assert!((hover.lift - 0.5).abs() <= f32::EPSILON);
        assert!((hover.scale - 1.0).abs() <= f32::EPSILON);
        let press = stock.step(true, true);
        assert!((press.lift - 1.0).abs() <= f32::EPSILON);
        assert!((press.scale - 0.97).abs() <= 1e-6);
        assert!(press.glow > 0.0);
        assert_eq!(stock.step(false, false), ResponseStep::REST);
        assert!(ResponseStep::REST.is_rest());
        assert!(!press.is_rest());
    }

    #[test]
    fn a_moment_interpolates_from_rest_and_is_clamped() {
        let step = ResponseStep {
            lift: 1.0,
            tint: 0.2,
            scale: 0.9,
            glow: 0.4,
        };
        let half = step.at(0.5);
        assert!((half.lift - 0.5).abs() <= f32::EPSILON);
        assert!((half.tint - 0.1).abs() <= f32::EPSILON);
        assert!((half.scale - 0.95).abs() <= f32::EPSILON);
        assert!((half.glow - 0.2).abs() <= f32::EPSILON);
        assert_eq!(step.at(0.0), ResponseStep::REST);
        assert_eq!(step.at(1.0), step);
        assert_eq!(step.at(4.0), step, "a spring past its target holds still");
    }

    #[test]
    fn out_of_band_levers_are_refused() {
        for bad in [
            ResponseStep::REST,
            ResponseStep {
                lift: -0.1,
                ..ResponseStep::REST
            },
            ResponseStep {
                lift: 1.0,
                tint: 0.3,
                ..ResponseStep::REST
            },
            ResponseStep {
                lift: 1.0,
                scale: 0.6,
                ..ResponseStep::REST
            },
            ResponseStep {
                lift: 1.0,
                glow: 0.6,
                ..ResponseStep::REST
            },
            ResponseStep {
                lift: f32::NAN,
                ..ResponseStep::REST
            },
        ] {
            assert!(bad.validate().is_err(), "{bad:?}");
        }
    }
}
