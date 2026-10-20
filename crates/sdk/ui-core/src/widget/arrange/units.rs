//! Units, sizing and the width classes the layout primitives share.
//!
//! Nothing here knows pixels. Sizes that relate to content are in `rem`, the body text size after
//! the person's text-size setting, so a layout reflows when text is enlarged instead of clipping.
//! Gaps are [`Rhythm`] tokens. Directions are *logical* (leading and trailing), never left and
//! right, so a right-to-left layout is the same layout mirrored.

use crate::theme::Theme;
use crate::tokens::{Rhythm, TextRole};

/// One `rem` in logical pixels: the body text size, text scale included.
#[must_use]
pub fn rem_of(theme: &Theme) -> f32 {
    theme.tokens.typography.size(TextRole::Body).max(1.0)
}

/// A size in `rem` or in logical pixels (`dp`).
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Length {
    Rem(f32),
    Dp(f32),
}

impl Length {
    /// The size in logical pixels when one `rem` is `rem` pixels.
    #[must_use]
    pub fn resolve(self, rem: f32) -> f32 {
        match self {
            Self::Rem(value) => value * rem,
            Self::Dp(value) => value,
        }
    }
}

/// The direction inline content runs: leading to trailing is left to right for `Ltr`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Direction {
    #[default]
    Ltr,
    Rtl,
}

impl Direction {
    /// The flex direction of a row whose first child is at the leading edge.
    #[must_use]
    pub const fn row(self) -> taffy::FlexDirection {
        match self {
            Self::Ltr => taffy::FlexDirection::Row,
            Self::Rtl => taffy::FlexDirection::RowReverse,
        }
    }

    /// Whether the leading edge is on the right.
    #[must_use]
    pub const fn is_rtl(self) -> bool {
        matches!(self, Self::Rtl)
    }
}

/// The axis of a container's main direction.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Axis {
    /// Children run along the inline direction (a row).
    Inline,
    /// Children run along the block direction (a stack).
    Block,
}

/// Placement across a container's cross axis, or of one item within its cell.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Align {
    Start,
    Center,
    End,
    #[default]
    Stretch,
}

impl Align {
    #[must_use]
    pub const fn items(self) -> taffy::AlignItems {
        match self {
            Self::Start => taffy::AlignItems::FlexStart,
            Self::Center => taffy::AlignItems::Center,
            Self::End => taffy::AlignItems::FlexEnd,
            Self::Stretch => taffy::AlignItems::Stretch,
        }
    }

    #[must_use]
    pub const fn this(self) -> taffy::AlignSelf {
        self.items()
    }
}

/// How a row distributes its children along the main axis.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Distribute {
    #[default]
    Start,
    Center,
    End,
    Between,
}

impl Distribute {
    #[must_use]
    pub const fn justify(self) -> taffy::JustifyContent {
        match self {
            Self::Start => taffy::JustifyContent::FlexStart,
            Self::Center => taffy::JustifyContent::Center,
            Self::End => taffy::JustifyContent::FlexEnd,
            Self::Between => taffy::JustifyContent::SpaceBetween,
        }
    }
}

/// One of the nine logical positions of a layer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum At {
    TopLeading,
    Top,
    TopTrailing,
    Leading,
    Center,
    Trailing,
    BottomLeading,
    Bottom,
    BottomTrailing,
}

impl At {
    /// The inline and block alignment of the position, with the inline one in physical terms
    /// for `direction` (leading is the right edge in right-to-left).
    #[must_use]
    pub const fn aligns(self, direction: Direction) -> (taffy::AlignSelf, taffy::AlignSelf) {
        use taffy::AlignItems::{Center, FlexEnd as End, FlexStart as Start};
        let (inline, block) = match self {
            Self::TopLeading => (Start, Start),
            Self::Top => (Center, Start),
            Self::TopTrailing => (End, Start),
            Self::Leading => (Start, Center),
            Self::Center => (Center, Center),
            Self::Trailing => (End, Center),
            Self::BottomLeading => (Start, End),
            Self::Bottom => (Center, End),
            Self::BottomTrailing => (End, End),
        };
        let inline = match (direction, inline) {
            (Direction::Rtl, Start) => End,
            (Direction::Rtl, End) => Start,
            (_, other) => other,
        };
        (inline, block)
    }
}

/// How one axis of a child is sized: as big as its content (`hug`), or taking the remaining
/// space by weight (`fill`), within bounds.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Sizing {
    /// `Some(weight)` takes the remaining space of the container's main axis by that weight.
    pub fill: Option<f32>,
    pub min: Option<Length>,
    pub max: Option<Length>,
}

impl Sizing {
    #[must_use]
    pub const fn hug() -> Self {
        Self {
            fill: None,
            min: None,
            max: None,
        }
    }

    #[must_use]
    pub const fn fill(weight: f32) -> Self {
        Self {
            fill: Some(weight),
            min: None,
            max: None,
        }
    }

    #[must_use]
    pub const fn min(mut self, min: Length) -> Self {
        self.min = Some(min);
        self
    }

    #[must_use]
    pub const fn max(mut self, max: Length) -> Self {
        self.max = Some(max);
        self
    }
}

/// How a child is sized in each axis, and its aspect ratio.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Fit {
    pub width: Sizing,
    pub height: Sizing,
    /// Width divided by height.
    pub aspect: Option<f32>,
    /// Placement across the container's cross axis, when it is not the container's own.
    pub align: Option<Align>,
}

impl Fit {
    #[must_use]
    pub const fn hug() -> Self {
        Self {
            width: Sizing::hug(),
            height: Sizing::hug(),
            aspect: None,
            align: None,
        }
    }

    /// Fill the space on both axes, by `weight` along the container's main axis.
    #[must_use]
    pub const fn fill(weight: f32) -> Self {
        Self {
            width: Sizing::fill(weight),
            height: Sizing::fill(weight),
            aspect: None,
            align: None,
        }
    }

    /// Set the sizing along the *inline* axis.
    #[must_use]
    pub const fn width(mut self, sizing: Sizing) -> Self {
        self.width = sizing;
        self
    }

    /// Set the sizing along the *block* axis.
    #[must_use]
    pub const fn height(mut self, sizing: Sizing) -> Self {
        self.height = sizing;
        self
    }

    #[must_use]
    pub const fn aspect(mut self, ratio: f32) -> Self {
        self.aspect = Some(ratio);
        self
    }

    #[must_use]
    pub const fn align(mut self, align: Align) -> Self {
        self.align = Some(align);
        self
    }

    /// The bounds and weights of this fit written onto a wrapper style of a container running
    /// along `main`, with `rem` logical pixels to a `rem`.
    pub fn apply(&self, style: &mut taffy::Style, main: Axis, rem: f32) {
        use taffy::style_helpers::length;
        let (along, across) = match main {
            Axis::Inline => (self.width, self.height),
            Axis::Block => (self.height, self.width),
        };
        if let Some(weight) = along.fill {
            style.flex_grow = weight.max(0.0);
            style.flex_shrink = 1.0;
            style.flex_basis = length(0.0_f32);
        }
        let bounds = |sizing: Sizing| {
            (
                sizing.min.map(|min| min.resolve(rem)),
                sizing.max.map(|max| max.resolve(rem)),
            )
        };
        let (width, height) = (bounds(self.width), bounds(self.height));
        if let Some(min) = width.0 {
            style.min_size.width = length(min);
        }
        if let Some(max) = width.1 {
            style.max_size.width = length(max);
        }
        if let Some(min) = height.0 {
            style.min_size.height = length(min);
        }
        if let Some(max) = height.1 {
            style.max_size.height = length(max);
        }
        if across.fill.is_some() {
            style.align_self = Some(taffy::AlignSelf::Stretch);
        }
        if let Some(ratio) = self.aspect {
            style.aspect_ratio = Some(ratio);
        }
        if let Some(align) = self.align {
            style.align_self = Some(align.this());
        }
    }
}

/// The width classes of a view's container (spec 4): in `rem`, so enlarged text moves a window to
/// a narrower class.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum WidthClass {
    Compact,
    Medium,
    Expanded,
    Wide,
}

/// Where the class boundaries lie, in `rem`.
pub const COMPACT_BELOW: f32 = 30.0;
pub const MEDIUM_BELOW: f32 = 52.0;
pub const EXPANDED_BELOW: f32 = 75.0;
/// Half the width of the band around a boundary inside which a class holds, in `rem`.
pub const HYSTERESIS: f32 = 0.5;

impl WidthClass {
    /// The class of `width_rem`, with no history.
    #[must_use]
    pub fn of(width_rem: f32) -> Self {
        if width_rem < COMPACT_BELOW {
            Self::Compact
        } else if width_rem < MEDIUM_BELOW {
            Self::Medium
        } else if width_rem < EXPANDED_BELOW {
            Self::Expanded
        } else {
            Self::Wide
        }
    }

    /// The class of `width_rem` given the class it was in: it holds inside a small band around
    /// each boundary, so a window resized exactly at one does not flicker between layouts.
    #[must_use]
    pub fn with_history(width_rem: f32, previous: Option<Self>) -> Self {
        let Some(previous) = previous else {
            return Self::of(width_rem);
        };
        let (low, high) = previous.band();
        if width_rem >= low - HYSTERESIS && width_rem < high + HYSTERESIS {
            previous
        } else {
            Self::of(width_rem)
        }
    }

    /// The `rem` range the class covers.
    #[must_use]
    pub const fn band(self) -> (f32, f32) {
        match self {
            Self::Compact => (f32::NEG_INFINITY, COMPACT_BELOW),
            Self::Medium => (COMPACT_BELOW, MEDIUM_BELOW),
            Self::Expanded => (MEDIUM_BELOW, EXPANDED_BELOW),
            Self::Wide => (EXPANDED_BELOW, f32::INFINITY),
        }
    }

    /// The page gutter of the class: more room around the content as there is more room.
    #[must_use]
    pub const fn gutter(self) -> Rhythm {
        match self {
            Self::Compact => Rhythm::Sibling,
            Self::Medium => Rhythm::Section,
            Self::Expanded | Self::Wide => Rhythm::Group,
        }
    }
}

/// The shape of the space a view is offered.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Orientation {
    Portrait,
    Landscape,
    Ultrawide,
}

/// The aspect ratio above which a space is ultrawide.
pub const ULTRAWIDE_RATIO: f32 = 2.2;

impl Orientation {
    #[must_use]
    pub fn of(width: f32, height: f32) -> Self {
        if height > 0.0 && width / height > ULTRAWIDE_RATIO {
            Self::Ultrawide
        } else if width > height {
            Self::Landscape
        } else {
            Self::Portrait
        }
    }
}

/// What a view reads as `space`: the constraint its container offers it (spec 4).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Space {
    /// Offered width and height in `rem`.
    pub width: f32,
    pub height: f32,
    pub width_class: WidthClass,
    pub orientation: Orientation,
}

impl Space {
    /// The space offered `width` by `height` logical pixels at `rem` pixels to a `rem`, given the
    /// class the view was last evaluated in.
    #[must_use]
    pub fn offered(width: f32, height: f32, rem: f32, previous: Option<WidthClass>) -> Self {
        let rem = rem.max(1.0);
        let (width_rem, height_rem) = (width / rem, height / rem);
        Self {
            width: width_rem,
            height: height_rem,
            width_class: WidthClass::with_history(width_rem, previous),
            orientation: Orientation::of(width, height),
        }
    }

    #[must_use]
    pub fn compact(&self) -> bool {
        self.width_class == WidthClass::Compact
    }

    #[must_use]
    pub fn wide(&self) -> bool {
        self.width_class >= WidthClass::Expanded
    }
}

/// The number of equal columns of at least `min` that fit `width` with `gap` between them: as
/// many as fit, and one at least.
#[must_use]
pub fn columns_for(width: f32, min: f32, gap: f32) -> usize {
    if !(width.is_finite() && min.is_finite()) || width <= 0.0 {
        return 1;
    }
    let unit = (min + gap).max(1.0);
    let fits = ((width + gap) / unit).floor();
    if fits < 1.0 {
        1
    } else {
        #[allow(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "a positive floor far below usize::MAX"
        )]
        {
            fits.min(1024.0) as usize
        }
    }
}

/// How a split arranges its panes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Arrangement {
    SideBySide,
    Collapsed,
}

/// Side by side when both panes' minimums and the gap between them fit `width`.
#[must_use]
pub fn arrangement_for(width: f32, primary_min: f32, secondary_min: f32, gap: f32) -> Arrangement {
    if width >= primary_min + secondary_min + gap {
        Arrangement::SideBySide
    } else {
        Arrangement::Collapsed
    }
}

/// The content width of a page: at most `measure` and at most what is left of `width` after a
/// gutter on each side.
#[must_use]
pub fn page_content_width(width: f32, measure: Option<f32>, gutter: f32) -> f32 {
    let room = (width - 2.0 * gutter).max(0.0);
    measure.map_or(room, |measure| room.min(measure))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lengths_resolve_against_the_text_size() {
        assert!((Length::Rem(11.0).resolve(16.0) - 176.0).abs() < f32::EPSILON);
        // 200 % text: the same rem is twice the pixels, so fewer columns fit.
        assert!((Length::Rem(11.0).resolve(32.0) - 352.0).abs() < f32::EPSILON);
        assert!((Length::Dp(5.0).resolve(32.0) - 5.0).abs() < f32::EPSILON);
    }

    #[test]
    fn a_grid_has_as_many_columns_as_fit_and_always_one() {
        let min = 11.0 * 16.0;
        assert_eq!(columns_for(320.0, min, 16.0), 1, "a phone");
        assert_eq!(columns_for(2000.0, min, 16.0), 10);
        assert_eq!(columns_for(1000.0, min, 16.0), 5);
        // Exactly enough for three columns and their gaps.
        assert_eq!(columns_for(3.0 * min + 2.0 * 16.0, min, 16.0), 3);
        assert_eq!(columns_for(3.0 * min + 2.0 * 16.0 - 1.0, min, 16.0), 2);
        assert_eq!(columns_for(0.0, min, 16.0), 1);
        // Doubling the text size halves the columns.
        assert_eq!(columns_for(1000.0, 2.0 * min, 16.0), 2);
    }

    #[test]
    fn classes_follow_the_rem_boundaries() {
        assert_eq!(WidthClass::of(29.9), WidthClass::Compact);
        assert_eq!(WidthClass::of(30.0), WidthClass::Medium);
        assert_eq!(WidthClass::of(51.9), WidthClass::Medium);
        assert_eq!(WidthClass::of(52.0), WidthClass::Expanded);
        assert_eq!(WidthClass::of(75.0), WidthClass::Wide);
        // The same window at 1.5 times the text size is a narrower class.
        let window = 700.0;
        assert_eq!(WidthClass::of(window / 16.0), WidthClass::Medium);
        assert_eq!(WidthClass::of(window / 24.0), WidthClass::Compact);
    }

    #[test]
    fn a_class_holds_through_a_small_band_around_its_boundary() {
        let compact = Some(WidthClass::Compact);
        assert_eq!(WidthClass::with_history(30.2, compact), WidthClass::Compact);
        assert_eq!(WidthClass::with_history(30.6, compact), WidthClass::Medium);
        let medium = Some(WidthClass::Medium);
        assert_eq!(WidthClass::with_history(29.8, medium), WidthClass::Medium);
        assert_eq!(WidthClass::with_history(29.4, medium), WidthClass::Compact);
        assert_eq!(WidthClass::with_history(30.0, None), WidthClass::Medium);
    }

    #[test]
    fn orientation_separates_portrait_landscape_and_ultrawide() {
        assert_eq!(Orientation::of(390.0, 844.0), Orientation::Portrait);
        assert_eq!(Orientation::of(1280.0, 800.0), Orientation::Landscape);
        assert_eq!(Orientation::of(3440.0, 1440.0), Orientation::Ultrawide);
        assert_eq!(Orientation::of(1080.0, 1920.0), Orientation::Portrait);
    }

    #[test]
    fn a_split_collapses_when_both_minimums_no_longer_fit() {
        let (primary, secondary, gap) = (18.0 * 16.0, 22.0 * 16.0, 16.0);
        let needed = primary + secondary + gap;
        assert_eq!(
            arrangement_for(needed, primary, secondary, gap),
            Arrangement::SideBySide
        );
        assert_eq!(
            arrangement_for(needed - 0.5, primary, secondary, gap),
            Arrangement::Collapsed
        );
        // At 150 % text the same window collapses.
        assert_eq!(
            arrangement_for(needed, 1.5 * primary, 1.5 * secondary, gap),
            Arrangement::Collapsed
        );
    }

    #[test]
    fn a_page_is_bounded_to_its_measure_and_keeps_its_gutters() {
        let measure = Some(40.0 * 16.0);
        assert!((page_content_width(1920.0, measure, 24.0) - 640.0).abs() < f32::EPSILON);
        assert!((page_content_width(390.0, measure, 16.0) - 358.0).abs() < f32::EPSILON);
        assert!((page_content_width(1920.0, None, 24.0) - 1872.0).abs() < f32::EPSILON);
        assert!(page_content_width(10.0, measure, 16.0).abs() < f32::EPSILON);
    }

    #[test]
    fn leading_is_the_right_edge_in_right_to_left() {
        use taffy::AlignItems::{FlexEnd as End, FlexStart as Start};
        let (inline, _) = At::TopLeading.aligns(Direction::Ltr);
        assert_eq!(inline, Start);
        let (inline, _) = At::TopLeading.aligns(Direction::Rtl);
        assert_eq!(inline, End);
        let (inline, block) = At::BottomTrailing.aligns(Direction::Rtl);
        assert_eq!((inline, block), (Start, End));
        assert_eq!(Direction::Rtl.row(), taffy::FlexDirection::RowReverse);
    }

    #[test]
    fn space_reads_the_offer_in_rem() {
        let space = Space::offered(480.0, 800.0, 16.0, None);
        assert!((space.width - 30.0).abs() < f32::EPSILON);
        assert!(!space.compact());
        assert_eq!(space.orientation, Orientation::Portrait);
        assert!(Space::offered(479.0, 800.0, 16.0, None).compact());
    }
}
