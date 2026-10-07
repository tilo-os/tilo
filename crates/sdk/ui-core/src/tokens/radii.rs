//! Corner radii, by what the corner belongs to (`docs/design-language.md`,
//! "Shape").
//!
//! Radii are **concentric**: an inner radius is its container's radius
//! minus the inset between them, never below [`Radii::MIN`]. A thing that
//! sits inside another rounded thing asks [`Radii::concentric`] for its
//! radius instead of picking a token, so its corners run parallel to its
//! container's at every inset and every scale.

#[derive(Clone, Debug, PartialEq)]
pub struct Radii {
    /// A window tile.
    pub tile: f32,
    /// A floating panel: menu, popover, palette, sheet, quick settings.
    pub panel: f32,
    /// A control: button, field, segmented track, a quick-settings tile.
    pub control: f32,
    /// An item inside a control or a list: a selected row, a segment thumb.
    pub item: f32,
    /// The smallest corner anything gets: a progress track, a badge, a
    /// sparkline clip, a caret. Also the concentric floor.
    pub detail: f32,
    /// A pill: chips, presence pills, tags, avatars — things pill-shaped by
    /// meaning. Buttons and tabs are not pills. A "make it a pill" sentinel,
    /// not a real length; drawing code clamps it to half the short side.
    pub full: f32,
}

impl Radii {
    /// The concentric floor at 1×: no corner inside a container shrinks
    /// below this.
    pub const MIN: f32 = 4.0;

    /// The stock window radius.
    pub const WINDOW: f32 = 14.0;

    /// The whole scale from a theme's one shape role, the window tile's
    /// radius. At the stock 14 it is the design language's table (panel 18,
    /// control 8, item 6, detail 4); every other radius keeps its proportion
    /// to the window, and the item stays concentric inside its control.
    #[must_use]
    pub fn from_window_radius(window: f32) -> Self {
        let window = window.max(0.0);
        let k = window / Self::WINDOW;
        let detail = (Self::MIN * k).min(Self::MIN);
        let control = 8.0 * k;
        let item = (control - 2.0 * k).max(detail.min(control));
        Self {
            tile: window,
            panel: 18.0 * k,
            control,
            item,
            detail,
            full: 9999.0,
        }
    }

    /// The radius of something inset `inset` inside a container whose
    /// corner is `outer`: `outer - inset`, never below [`Self::detail`].
    /// A full-pill container yields a full pill.
    #[must_use]
    pub fn concentric(&self, outer: f32, inset: f32) -> f32 {
        if outer >= self.full {
            return self.full;
        }
        (outer - inset.max(0.0)).max(self.detail)
    }
}

impl Default for Radii {
    fn default() -> Self {
        Self::from_window_radius(Self::WINDOW)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn radii_follow_the_shape_table_and_nest_concentrically() {
        // phase: the_scale_is_the_design_language_table
        {
            let r = Radii::default();
            assert!((r.tile - 14.0).abs() < f32::EPSILON);
            assert!((r.panel - 18.0).abs() < f32::EPSILON);
            assert!((r.control - 8.0).abs() < f32::EPSILON);
            assert!((r.item - 6.0).abs() < f32::EPSILON);
            assert!((r.detail - 4.0).abs() < f32::EPSILON);
        }
        // phase: an_inset_child_takes_the_outer_radius_minus_the_inset
        {
            let r = Radii::default();
            assert!((r.concentric(r.panel, 8.0) - 10.0).abs() < f32::EPSILON);
            assert!((r.concentric(r.control, 2.0) - r.item).abs() < f32::EPSILON);
            assert!((r.concentric(r.tile, 4.0) - 10.0).abs() < f32::EPSILON);
        }
        // phase: one_window_radius_derives_the_scale
        {
            let round = Radii::from_window_radius(28.0);
            assert!((round.panel - 36.0).abs() < 1e-4);
            assert!((round.control - 16.0).abs() < 1e-4);
            assert!((round.item - 12.0).abs() < 1e-4);
            assert!((round.detail - Radii::MIN).abs() < 1e-4);
            let square = Radii::from_window_radius(0.0);
            assert_eq!(
                [
                    square.tile,
                    square.panel,
                    square.control,
                    square.item,
                    square.detail
                ],
                [0.0; 5]
            );
            let tight = Radii::from_window_radius(7.0);
            assert!(tight.item <= tight.control && tight.detail <= tight.item);
        }
        // phase: concentric_radii_never_drop_below_the_floor
        {
            let r = Radii::default();
            assert!((r.concentric(r.control, 16.0) - Radii::MIN).abs() < f32::EPSILON);
            assert!((r.concentric(r.item, 6.0) - Radii::MIN).abs() < f32::EPSILON);
            assert!((r.concentric(r.full, 6.0) - r.full).abs() < f32::EPSILON);
            assert!((r.concentric(r.panel, -3.0) - r.panel).abs() < f32::EPSILON);
        }
    }
}
