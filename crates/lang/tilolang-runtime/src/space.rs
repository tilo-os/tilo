//! `space`: the constraint a view's container offers it (layout design, section 4).
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

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Space {
    /// Available width in rem.
    pub width: f64,
    /// Available height in rem.
    pub height: f64,
}

impl Default for Space {
    /// A medium landscape space, for a view nobody has laid out yet.
    fn default() -> Self {
        Self {
            width: 40.0,
            height: 25.0,
        }
    }
}

impl Space {
    pub fn from_rem(width: f64, height: f64) -> Self {
        Self { width, height }
    }

    /// 0 compact, 1 medium, 2 expanded, 3 wide.
    pub fn width_class(&self) -> i64 {
        if self.width < COMPACT_BELOW {
            0
        } else if self.width < MEDIUM_BELOW {
            1
        } else if self.width < EXPANDED_BELOW {
            2
        } else {
            3
        }
    }

    /// 0 portrait, 1 landscape, 2 ultrawide.
    pub fn orientation(&self) -> i64 {
        if self.height > 0.0 && self.width / self.height > ULTRAWIDE_RATIO {
            2
        } else if self.width < self.height {
            0
        } else {
            1
        }
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
            _ => return None,
        })
    }

    /// Whether the field is a size (read with `space_f`) rather than a class or flag.
    pub fn is_size(name: &str) -> bool {
        matches!(name, "width" | "height")
    }
}

#[allow(clippy::cast_precision_loss, reason = "values are small class indexes")]
fn int_as_f64(v: i64) -> f64 {
    v as f64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classes_follow_the_documented_boundaries() {
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
}
