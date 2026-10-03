//! Density metrics: the control heights, hit targets, panel widths and icon
//! sizes HIG §9.6 fixes so two panels built by two people come out the same
//! size. Spacing says how far apart things sit; metrics say how big the things
//! are.
//!
//! Like [`super::Spacing`] these are logical pixels multiplied by
//! [`crate::Theme::with_ui_scale`]. A custom theme's `spacing_px` does **not**
//! touch them: a denser gutter preference must not shrink a pointer target.
//!
//! [`Metrics::BASE`] is the unscaled reference, readable from a `const`
//! context so a shell constant that has to be compile-time (`const PAD`, a
//! default breakpoint) names the token instead of restating its number.

/// Reference density (HIG §9.6), unscaled. Read live values from
/// `theme.tokens.metrics`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Metrics {
    /// A top bar, toolbar or status strip. Also the default height of the
    /// shared `Button` and `Dropdown` controls.
    pub bar_height: f32,
    /// A control living inside a 40 px bar (segmented switch, bar button).
    pub bar_control_height: f32,
    /// One row of a Panel — a settings toggle row, a roster entry.
    pub panel_row_height: f32,
    /// A compact selector (chip, tab pill, dropdown trigger in a Panel).
    pub selector_height: f32,
    /// No pointer target is smaller than this, whatever glyph it shows.
    pub min_pointer_target: f32,
    /// A utility Panel such as quick settings.
    pub panel_width_utility: f32,
    /// A roster or list Panel.
    pub panel_width_list: f32,
    /// A sheet carrying a form.
    pub panel_width_sheet: f32,
    /// Compact glyph: tab leading icon, inline status mark.
    pub icon_sm: f32,
    /// The standard 20 px glyph that sits in a 32 px hit rect.
    pub icon_md: f32,
    /// Large glyph: empty-state mark, list leading icon.
    pub icon_lg: f32,
    /// Between an icon and the label it belongs to (`Button`, `Tabs`).
    pub icon_label_gap: f32,
    /// Thickness of the rule under a toolbar and along a status bar.
    /// `1` is the hairline. `0` draws no rule. A heavier value is a
    /// stronger chrome edge. `ui_scale` multiplies it; a theme's spacing
    /// preference does not.
    pub chrome_rule_px: f32,
    /// A genuine navigation sidebar (HIG §1a) when docked.
    pub sidebar_width: f32,
    /// An overlay drawer; equal to [`Metrics::panel_width_utility`] so a
    /// docked utility panel and a slid-in drawer are one width.
    pub drawer_width: f32,
    /// The overview column of an overview + detail pair (HIG §1b).
    pub overview_width: f32,
    /// At or above this window width a sidebar docks beside its content;
    /// below it the shell switches to separate screens or an overlay.
    /// `sidebar_width` plus a content column that still reads
    /// ([`Metrics::content_min_width`]).
    pub compact_breakpoint: f32,
    /// One row of a navigation list (`NavList`, `paint_nav_row`) at the
    /// default text scale; it grows with the body line like any shell
    /// control that carries one line of text.
    pub nav_row_height: f32,
    /// The one header row a `NavShell` draws across the top of the window:
    /// a `min_pointer_target` control in an `s3` inset (32 + 2 × 12).
    pub header_height: f32,
    /// A compact icon-over-label navigation rail (`NavList::rail`).
    pub rail_width: f32,
    /// The inspector column at the trailing edge of a window.
    pub inspector_width: f32,
    /// Narrowest a person can drag a docked sidebar; dragging it below this
    /// hides it.
    pub sidebar_min_width: f32,
    /// Widest a person can drag a docked sidebar.
    pub sidebar_max_width: f32,
    /// Narrowest the content column of a window may become before the
    /// sidebar collapses into a drawer: `compact_breakpoint − sidebar_width`.
    pub content_min_width: f32,
    /// Narrowest the detail column beside an overview may become before the
    /// two become push screens.
    pub detail_min_width: f32,
}

impl Metrics {
    /// HIG §9.6 values, unscaled.
    pub const BASE: Self = Self {
        bar_height: 40.0,
        bar_control_height: 28.0,
        panel_row_height: 44.0,
        selector_height: 28.0,
        min_pointer_target: 32.0,
        panel_width_utility: 320.0,
        panel_width_list: 360.0,
        panel_width_sheet: 560.0,
        icon_sm: 14.0,
        icon_md: 20.0,
        icon_lg: 24.0,
        icon_label_gap: 6.0,
        chrome_rule_px: 1.0,
        sidebar_width: 280.0,
        drawer_width: 320.0,
        overview_width: 340.0,
        compact_breakpoint: 720.0,
        nav_row_height: 32.0,
        header_height: 56.0,
        rail_width: 76.0,
        inspector_width: 300.0,
        sidebar_min_width: 200.0,
        sidebar_max_width: 420.0,
        content_min_width: 440.0,
        detail_min_width: 360.0,
    };

    /// The factor a **text-sized** width or breakpoint grows by at a given
    /// accessibility text scale: `text_scale.max(1.0)`.
    ///
    /// A destination list, an overview column and the window width at which
    /// two panes still fit side by side are all sized by the text they hold,
    /// so they grow with the text — but never shrink below their authored
    /// 1× geometry, where the rows, hit targets and paddings were designed.
    /// This is the one rule the responsive shells (`NavShell`,
    /// `SidebarDrawer`) apply to their inputs
    /// (`crate::Theme::responsive_scale`), so an app never restates it.
    /// A non-finite scale counts as 1×.
    #[must_use]
    pub fn text_growth(text_scale: f32) -> f32 {
        if text_scale.is_finite() {
            text_scale.max(1.0)
        } else {
            1.0
        }
    }

    /// Every metric multiplied by `factor`. Used by `ui_scale`; never by a
    /// custom theme's spacing preference.
    #[must_use]
    pub fn scaled(&self, factor: f32) -> Self {
        Self {
            bar_height: self.bar_height * factor,
            bar_control_height: self.bar_control_height * factor,
            panel_row_height: self.panel_row_height * factor,
            selector_height: self.selector_height * factor,
            min_pointer_target: self.min_pointer_target * factor,
            panel_width_utility: self.panel_width_utility * factor,
            panel_width_list: self.panel_width_list * factor,
            panel_width_sheet: self.panel_width_sheet * factor,
            icon_sm: self.icon_sm * factor,
            icon_md: self.icon_md * factor,
            icon_lg: self.icon_lg * factor,
            icon_label_gap: self.icon_label_gap * factor,
            chrome_rule_px: self.chrome_rule_px * factor,
            sidebar_width: self.sidebar_width * factor,
            drawer_width: self.drawer_width * factor,
            overview_width: self.overview_width * factor,
            compact_breakpoint: self.compact_breakpoint * factor,
            nav_row_height: self.nav_row_height * factor,
            header_height: self.header_height * factor,
            rail_width: self.rail_width * factor,
            inspector_width: self.inspector_width * factor,
            sidebar_min_width: self.sidebar_min_width * factor,
            sidebar_max_width: self.sidebar_max_width * factor,
            content_min_width: self.content_min_width * factor,
            detail_min_width: self.detail_min_width * factor,
        }
    }
}

impl Default for Metrics {
    fn default() -> Self {
        Self::BASE
    }
}
