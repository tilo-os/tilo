//! `space`: the constraint a view's container offers it (layout.md, "Responding to space").
//!
//! Widths and heights are in `rem`, so a larger text size moves a window to a narrower class.
//! A view reads `space` through `space_i` and `space_f`; the read recorder remembers which
//! values it read, so a view re-runs only when one of those values changes, not on every
//! pixel of a resize.

/// Class boundaries in rem.
const COMPACT_BELOW: f64 = 30.0;
const MEDIUM_BELOW: f64 = 52.0;
const EXPANDED_BELOW: f64 = 75.0;
/// Width over height beyond which a space is ultrawide.
const ULTRAWIDE_RATIO: f64 = 2.2;

/// What one rem is in logical pixels until a window says what it drew its text at: the
/// toolkit's body size, as every app of the shipped set draws it.
pub const DEFAULT_REM: f64 = 16.0;

/// How far past a class boundary a size has to go before its class changes, in rem: a window
/// resized right at a boundary keeps one class instead of flickering between two.
pub const HYSTERESIS_REM: f64 = 0.5;
/// The same for the aspect ratio an ultrawide space is told by.
const HYSTERESIS_RATIO: f64 = 0.05;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Space {
    /// Available width in rem.
    pub width: f64,
    /// Available height in rem.
    pub height: f64,
    /// What one rem is in logical pixels: the person's body text size, which a view computes a
    /// place in pixels from (a card kept where the person dropped it).
    pub rem: f64,
    /// 0 compact, 1 medium, 2 expanded, 3 wide.
    class: i64,
    /// 0 portrait, 1 landscape, 2 ultrawide.
    orientation: i64,
}

impl Default for Space {
    /// A medium landscape space, for a view nobody has laid out yet.
    fn default() -> Self {
        Self::from_rem(40.0, 25.0)
    }
}

fn width_class_of(width: f64) -> i64 {
    if width < COMPACT_BELOW {
        0
    } else if width < MEDIUM_BELOW {
        1
    } else if width < EXPANDED_BELOW {
        2
    } else {
        3
    }
}

fn orientation_of(width: f64, height: f64) -> i64 {
    if height > 0.0 && width / height > ULTRAWIDE_RATIO {
        2
    } else {
        i64::from(width >= height)
    }
}

impl Space {
    /// A space of `width` by `height` rem, its classes read straight from its size.
    pub fn from_rem(width: f64, height: f64) -> Self {
        Self {
            width,
            height,
            rem: DEFAULT_REM,
            class: width_class_of(width),
            orientation: orientation_of(width, height),
        }
    }

    /// This space with its rem this many logical pixels: what the window drew its text at.
    #[must_use]
    pub fn with_rem(mut self, rem: f64) -> Self {
        if rem.is_finite() && rem > 0.0 {
            self.rem = rem;
        }
        self
    }

    /// The space a container of `width` by `height` rem offers after this one: a class changes
    /// only once the size is [`HYSTERESIS_REM`] past the boundary it crosses.
    #[must_use]
    pub fn followed(&self, width: f64, height: f64) -> Self {
        let plain = width_class_of(width);
        let class = match plain.cmp(&self.class) {
            std::cmp::Ordering::Greater => self.class.max(width_class_of(width - HYSTERESIS_REM)),
            std::cmp::Ordering::Less => self.class.min(width_class_of(width + HYSTERESIS_REM)),
            std::cmp::Ordering::Equal => plain,
        };
        let ratio = if height > 0.0 { width / height } else { 0.0 };
        let shifted = |by: f64| orientation_of(ratio + by, 1.0);
        let plain = orientation_of(width, height);
        let orientation = match plain.cmp(&self.orientation) {
            _ if height <= 0.0 => plain,
            std::cmp::Ordering::Greater => self.orientation.max(shifted(-HYSTERESIS_RATIO)),
            std::cmp::Ordering::Less => self.orientation.min(shifted(HYSTERESIS_RATIO)),
            std::cmp::Ordering::Equal => plain,
        };
        Self {
            width,
            height,
            rem: self.rem,
            class,
            orientation,
        }
    }

    /// 0 compact, 1 medium, 2 expanded, 3 wide.
    pub fn width_class(&self) -> i64 {
        self.class
    }

    /// 0 portrait, 1 landscape, 2 ultrawide.
    pub fn orientation(&self) -> i64 {
        self.orientation
    }

    /// The value of a field by name as a number: classes and enums as their index, booleans
    /// as 0 or 1, sizes in rem. `None` for an unknown field.
    pub fn field(&self, name: &str) -> Option<f64> {
        Some(match name {
            "width_class" => int_as_f64(self.width_class()),
            "compact" => int_as_f64(i64::from(self.width_class() == 0)),
            "wide" => int_as_f64(i64::from(self.width_class() == 3)),
            "orientation" => int_as_f64(self.orientation()),
            "width" => self.width,
            "height" => self.height,
            "rem" => self.rem,
            _ => return None,
        })
    }

    /// Whether the field is a size (read with `space_f`) rather than a class or flag.
    pub fn is_size(name: &str) -> bool {
        matches!(name, "width" | "height" | "rem")
    }
}

#[allow(
    clippy::cast_precision_loss,
    reason = "every caller passes a space class index, 0 to 3"
)]
fn int_as_f64(v: i64) -> f64 {
    v as f64
}

#[cfg(test)]
mod tests {
    use super::*;

    tilo_test_env::scenario! {
        fn a_space_class_follows_its_boundaries;

        phase classes_follow_the_documented_boundaries;
        {
            let class = |w| Space::from_rem(w, 20.0).width_class();
            assert_eq!(
                [class(10.0), class(30.0), class(52.0), class(75.0)],
                [0, 1, 2, 3]
            );
            assert_eq!(Space::from_rem(100.0, 30.0).orientation(), 2);
            assert_eq!(Space::from_rem(20.0, 30.0).orientation(), 0);
            assert_eq!(Space::from_rem(40.0, 30.0).orientation(), 1);
            assert_eq!(Space::from_rem(20.0, 30.0).field("compact"), Some(1.0));
            assert_eq!(Space::from_rem(20.0, 30.0).field("nope"), None);
        }

        phase a_space_says_what_a_rem_is_in_pixels;
        {
            let space = Space::from_rem(40.0, 30.0);
            assert_eq!(
                space.field("rem"),
                Some(DEFAULT_REM),
                "until a window says, a rem is the body size every app draws"
            );
            assert!(Space::is_size("rem"), "a view computes a place in pixels from it");
            let told = space.with_rem(20.0);
            assert_eq!(told.field("rem"), Some(20.0));
            assert_eq!(
                told.followed(41.0, 30.0).rem,
                20.0,
                "a size that moves keeps the rem it was drawn at"
            );
            assert_eq!(
                space.with_rem(0.0).field("rem"),
                Some(DEFAULT_REM),
                "no size is no rem, and the body size stands"
            );
        }

        phase a_class_changes_only_past_its_boundary_by_the_hysteresis;
        {
            let medium = Space::from_rem(31.0, 40.0);
            // Just under the compact boundary: still medium; well under it: compact.
            assert_eq!(medium.followed(29.8, 40.0).width_class(), 1);
            let compact = medium.followed(29.0, 40.0);
            assert_eq!(compact.width_class(), 0);
            // Back just over it: still compact, until past it by the hysteresis.
            assert_eq!(compact.followed(30.2, 40.0).width_class(), 0);
            assert_eq!(compact.followed(30.6, 40.0).width_class(), 1);
            // A jump over several boundaries lands in the class it reaches.
            assert_eq!(compact.followed(100.0, 40.0).width_class(), 3);
            // Landscape to portrait right at square keeps landscape.
            let landscape = Space::from_rem(50.0, 40.0);
            assert_eq!(landscape.followed(39.0, 40.0).orientation(), 1);
            assert_eq!(landscape.followed(30.0, 40.0).orientation(), 0);
        }
    }
}
