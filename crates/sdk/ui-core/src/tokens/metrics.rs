//! Density metrics: the control heights, hit targets, panel widths and icon sizes HIG
//! §9.6 fixes so two panels built by two people come out the same size. Spacing says how
//! far apart things sit; metrics say how big the things are. A custom theme's
//! `spacing_px` does **not** touch them: a denser gutter preference must not shrink a
//! pointer target.

/// Reference density (HIG §9.6), unscaled.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Metrics {
    pub bar_height: f32,
    /// A control living inside a 40 px bar (segmented switch, bar button).
    pub bar_control_height: f32,
    pub panel_row_height: f32,
    pub menu_row_height: f32,
    pub menu_min_width: f32,
    /// Room a scroll view keeps on its trailing edge for its scroll bar while its content
    /// overflows, so the bar never covers content: the thumb ([`Metrics::scroll_thumb`])
    /// plus `s1` on each side.
    pub scrollbar_gutter: f32,
    pub selector_height: f32,
    pub min_pointer_target: f32,
    pub panel_width_utility: f32,
    pub panel_width_list: f32,
    pub panel_width_sheet: f32,
    pub icon_sm: f32,
    /// The standard 20 px glyph that sits in a 32 px hit rect.
    pub icon_md: f32,
    pub icon_lg: f32,
    pub icon_label_gap: f32,
    /// Thickness of the rule under a toolbar and along a status bar.
    pub chrome_rule_px: f32,
    pub call_edge_px: f32,
    pub alert_edge_px: f32,
    pub sidebar_width: f32,
    pub drawer_width: f32,
    pub overview_width: f32,
    pub overview_max_width: f32,
    /// A generic "wide window" width for painted apps that pick a compact chrome by
    /// width: `sidebar_width` plus [`Metrics::content_min_width`].
    pub compact_breakpoint: f32,
    pub nav_row_height: f32,
    /// The one header row a `NavShell` draws across the top of the window: a
    /// `min_pointer_target` control in an `s3` inset (32 + 2 × 12).
    pub header_height: f32,
    pub rail_width: f32,
    pub rail_max_width: f32,
    pub inspector_width: f32,
    pub sidebar_min_width: f32,
    /// The narrowest a sidebar or panel works at beside a painted app's own content
    /// column, as a split of the app's window: a name beside its value, a destination's
    /// label whole.
    pub panel_min_width: f32,
    pub sidebar_max_width: f32,
    /// A generous content column: the width of a sheet ([`Metrics::panel_width_sheet`]).
    pub content_min_width: f32,
    /// Narrowest a detail column beside a list may become in a painted app's own split.
    pub detail_min_width: f32,
    /// Narrowest a Tilo app's window becomes: its compact layout, one screen at a time,
    /// at phone width. Resizing a window stops its edge here (`widget::edge_resize`), so
    /// every app's compact layout must fit it.
    pub window_min_width: f32,
    pub list_min_width: f32,
    /// The narrowest a reading column (a message, an article) still works at: about 45
    /// characters of body text.
    pub reading_min_width: f32,
    pub page_min_width: f32,
    pub grid_min_width: f32,
    pub board_lane_width: f32,
    /// How much wider than its threshold a window must grow before a layout that switched
    /// away switches back, so a window resized near a threshold does not flicker between
    /// two layouts.
    pub layout_hysteresis: f32,
    pub readable_width: f32,
    /// The line length of long-form reading text (documents, books, notes): about 66
    /// characters of the `Reading` role at the stock text size.
    pub reading_measure: f32,
    pub message_measure: f32,
    pub chooser_width: f32,
    /// The narrowest a text field stands where nothing else sets its width: a row that
    /// wraps or a run that hugs its items lays a field out at its own width, which is
    /// this, grown with the text.
    pub field_min_width: f32,
    pub wheel_line: f32,
    pub scroll_thumb: f32,
    /// How far a row must be swiped sideways before letting go commits its swipe action
    /// rather than springing back.
    pub swipe_commit: f32,
    pub row_single: f32,
    pub row_double: f32,
    /// A list row led by a 40 px avatar or icon.
    pub row_avatar: f32,
    /// A glyph in a row or a control: Lucide at 16.
    pub icon_control: f32,
    pub icon_bar: f32,
    pub avatar_inline: f32,
    pub avatar_row: f32,
    pub location_min_width: f32,
    /// A bar's actions move into its overflow menu, last first, while its location field
    /// would be narrower than this.
    pub location_comfortable_width: f32,
    pub note_wrap_width: f32,
    pub sheet_max_height: f32,
    /// A listing's row: a 40 px mark beside a title, a line and who is there.
    pub listing_row_height: f32,
    pub face_stack: f32,
    pub face_stack_step: f32,
    pub overview_panel_width: f32,
    pub overview_picture_min_width: f32,
    pub row_action_width: f32,
    pub row_arrow: f32,
    pub row_slider_width: f32,
    pub row_slider_min_width: f32,
    pub row_slider_valued_width: f32,
    pub meter_height: f32,
    pub chart_height: f32,
    pub aside_min_width: f32,
    pub figure_size: f32,
    pub picture_tile_min_width: f32,
    pub picture_tile_width: f32,
    pub click_slop: f32,
    pub drag_line_px: f32,
}

impl Metrics {
    /// HIG §9.6 values, unscaled.
    pub const BASE: Self = Self {
        bar_height: 40.0,
        bar_control_height: 28.0,
        panel_row_height: 44.0,
        menu_row_height: 32.0,
        menu_min_width: 160.0,
        scrollbar_gutter: 16.0,
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
        call_edge_px: 3.0,
        alert_edge_px: 7.0,
        sidebar_width: 280.0,
        drawer_width: 320.0,
        overview_width: 340.0,
        overview_max_width: 460.0,
        compact_breakpoint: 840.0,
        nav_row_height: 32.0,
        header_height: 56.0,
        rail_width: 76.0,
        rail_max_width: 160.0,
        inspector_width: 300.0,
        sidebar_min_width: 200.0,
        panel_min_width: 200.0,
        sidebar_max_width: 420.0,
        content_min_width: 560.0,
        detail_min_width: 360.0,
        window_min_width: 320.0,
        list_min_width: 320.0,
        reading_min_width: 360.0,
        page_min_width: 360.0,
        grid_min_width: 280.0,
        board_lane_width: 320.0,
        layout_hysteresis: 24.0,
        readable_width: 720.0,
        reading_measure: 640.0,
        message_measure: 560.0,
        chooser_width: 224.0,
        field_min_width: 160.0,
        wheel_line: 60.0,
        scroll_thumb: 8.0,
        swipe_commit: 88.0,
        row_single: super::Density::Comfortable.rows().single,
        row_double: super::Density::Comfortable.rows().double,
        row_avatar: super::Density::Comfortable.rows().avatar,
        icon_control: 16.0,
        icon_bar: 20.0,
        avatar_inline: 24.0,
        avatar_row: 40.0,
        location_min_width: 48.0,
        location_comfortable_width: 200.0,
        note_wrap_width: 300.0,
        sheet_max_height: 600.0,
        listing_row_height: 76.0,
        face_stack: 18.0,
        face_stack_step: 13.0,
        overview_panel_width: 480.0,
        overview_picture_min_width: 480.0,
        row_action_width: 84.0,
        row_arrow: 22.0,
        row_slider_width: 172.0,
        row_slider_min_width: 88.0,
        row_slider_valued_width: 184.0,
        meter_height: 6.0,
        chart_height: 168.0,
        aside_min_width: 240.0,
        figure_size: 280.0,
        picture_tile_min_width: 104.0,
        picture_tile_width: 168.0,
        click_slop: 4.0,
        drag_line_px: 2.0,
    };

    /// The factor a **text-sized** width or breakpoint grows by at a given accessibility
    /// text scale: `text_scale.max(1.0)`. A destination list, an overview column and the
    /// window width at which two panes still fit side by side are all sized by the text
    /// they hold, so they grow with the text — but never shrink below their authored 1×
    /// geometry, where the rows, hit targets and paddings were designed.
    #[must_use]
    pub fn text_growth(text_scale: f32) -> f32 {
        if text_scale.is_finite() {
            text_scale.max(1.0)
        } else {
            1.0
        }
    }

    /// The factor the glyph metrics (`icon_sm`, `icon_md`, `icon_lg`) grow by at a given
    /// accessibility text scale: the square root of [`Self::text_growth`].
    #[must_use]
    pub fn icon_growth(text_scale: f32) -> f32 {
        Self::text_growth(text_scale).sqrt()
    }

    /// These metrics with the glyph sizes multiplied by `factor`; see
    /// [`Self::icon_growth`].
    #[must_use]
    pub fn with_icons_scaled(mut self, factor: f32) -> Self {
        self.icon_sm *= factor;
        self.icon_md *= factor;
        self.icon_lg *= factor;
        self.icon_control *= factor;
        self.icon_bar *= factor;
        self
    }

    /// Every metric multiplied by `factor`. Used by `ui_scale`; never by a custom theme's
    /// spacing preference.
    #[must_use]
    pub fn scaled(&self, factor: f32) -> Self {
        Self {
            bar_height: self.bar_height * factor,
            bar_control_height: self.bar_control_height * factor,
            panel_row_height: self.panel_row_height * factor,
            menu_row_height: self.menu_row_height * factor,
            menu_min_width: self.menu_min_width * factor,
            scrollbar_gutter: self.scrollbar_gutter * factor,
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
            call_edge_px: self.call_edge_px * factor,
            alert_edge_px: self.alert_edge_px * factor,
            sidebar_width: self.sidebar_width * factor,
            drawer_width: self.drawer_width * factor,
            overview_width: self.overview_width * factor,
            overview_max_width: self.overview_max_width * factor,
            compact_breakpoint: self.compact_breakpoint * factor,
            nav_row_height: self.nav_row_height * factor,
            header_height: self.header_height * factor,
            rail_width: self.rail_width * factor,
            rail_max_width: self.rail_max_width * factor,
            inspector_width: self.inspector_width * factor,
            sidebar_min_width: self.sidebar_min_width * factor,
            panel_min_width: self.panel_min_width * factor,
            sidebar_max_width: self.sidebar_max_width * factor,
            content_min_width: self.content_min_width * factor,
            detail_min_width: self.detail_min_width * factor,
            window_min_width: self.window_min_width * factor,
            list_min_width: self.list_min_width * factor,
            reading_min_width: self.reading_min_width * factor,
            page_min_width: self.page_min_width * factor,
            grid_min_width: self.grid_min_width * factor,
            board_lane_width: self.board_lane_width * factor,
            layout_hysteresis: self.layout_hysteresis * factor,
            readable_width: self.readable_width * factor,
            reading_measure: self.reading_measure * factor,
            message_measure: self.message_measure * factor,
            chooser_width: self.chooser_width * factor,
            field_min_width: self.field_min_width * factor,
            wheel_line: self.wheel_line * factor,
            scroll_thumb: self.scroll_thumb * factor,
            swipe_commit: self.swipe_commit * factor,
            row_single: self.row_single * factor,
            row_double: self.row_double * factor,
            row_avatar: self.row_avatar * factor,
            icon_control: self.icon_control * factor,
            icon_bar: self.icon_bar * factor,
            avatar_inline: self.avatar_inline * factor,
            avatar_row: self.avatar_row * factor,
            location_min_width: self.location_min_width * factor,
            location_comfortable_width: self.location_comfortable_width * factor,
            note_wrap_width: self.note_wrap_width * factor,
            sheet_max_height: self.sheet_max_height * factor,
            listing_row_height: self.listing_row_height * factor,
            face_stack: self.face_stack * factor,
            face_stack_step: self.face_stack_step * factor,
            overview_panel_width: self.overview_panel_width * factor,
            overview_picture_min_width: self.overview_picture_min_width * factor,
            row_action_width: self.row_action_width * factor,
            row_arrow: self.row_arrow * factor,
            row_slider_width: self.row_slider_width * factor,
            row_slider_min_width: self.row_slider_min_width * factor,
            row_slider_valued_width: self.row_slider_valued_width * factor,
            meter_height: self.meter_height * factor,
            chart_height: self.chart_height * factor,
            aside_min_width: self.aside_min_width * factor,
            figure_size: self.figure_size * factor,
            picture_tile_min_width: self.picture_tile_min_width * factor,
            picture_tile_width: self.picture_tile_width * factor,
            click_slop: self.click_slop * factor,
            drag_line_px: self.drag_line_px * factor,
        }
    }
}

impl Default for Metrics {
    fn default() -> Self {
        Self::BASE
    }
}

#[cfg(test)]
mod tests {
    use super::Metrics;

    #[test]
    fn readable_and_scroll_metrics_follow_ui_scale() {
        let base = Metrics::BASE;
        assert!((base.readable_width - 720.0).abs() < f32::EPSILON);
        let scaled = base.scaled(1.5);
        for (one, scaled) in [
            (base.readable_width, scaled.readable_width),
            (base.reading_measure, scaled.reading_measure),
            (base.message_measure, scaled.message_measure),
            (base.chooser_width, scaled.chooser_width),
            (base.field_min_width, scaled.field_min_width),
            (base.wheel_line, scaled.wheel_line),
            (base.scroll_thumb, scaled.scroll_thumb),
            (base.swipe_commit, scaled.swipe_commit),
            (base.menu_min_width, scaled.menu_min_width),
        ] {
            assert!((one * 1.5 - scaled).abs() < 1e-3);
        }
        let theme = crate::Theme::dark().with_ui_scale(2.0);
        assert!((theme.tokens.metrics.readable_width - 1440.0).abs() < 1e-3);
        assert!(base.reading_measure <= base.readable_width);
        assert!(base.chooser_width < base.content_min_width);
    }
}
