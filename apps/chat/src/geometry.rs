//! Pure geometry for the hand-painted chat chrome.
//!
//! The chat surface computes its rectangles by hand in `render_chat.rs`. The
//! helpers here hold the *rules* those rectangles follow — how a control band
//! is centred, how a title shares a row with a measured presence pill — as
//! plain functions of measured sizes, so the draw pass, the hit list and the
//! unit tests all consume one description instead of three copies of the same
//! arithmetic. Nothing in here measures text or touches the theme; callers
//! pass in the line boxes they measured with the style they will paint with.

use vello::kurbo::Rect;

/// A band of `content_h + 2 * pad` centred vertically inside `container`,
/// spanning the container's full width. When the padded content is taller
/// than the container the band is the container itself (the content owner
/// scrolls or grows; it never leaks past the box).
///
/// The composer uses this so a one-line draft sits on the same centre line as
/// the attach / emoji / send buttons instead of hugging the box's top edge.
pub(crate) fn centered_band(container: Rect, content_h: f64, pad: f64) -> Rect {
    let wanted = (content_h + 2.0 * pad).max(0.0);
    let h = wanted.min(container.height());
    let y0 = container.y0 + (container.height() - h) / 2.0;
    Rect::new(container.x0, y0, container.x1, y0 + h)
}

/// Top edge of a `line_h`-tall line box whose centre sits on `cy`. Text is
/// painted from the top of its line box, so this is the y every centred label
/// is drawn at — computed from the measured line box, never from the font size.
pub(crate) fn line_top(cy: f64, line_h: f64) -> f64 {
    cy - line_h / 2.0
}

/// Inputs for [`sidebar_header_layout`]: the sidebar rect, the row metrics,
/// and the sizes the caller measured with the styles it paints with.
pub(crate) struct SidebarHeaderSpec {
    /// The sidebar the header sits at the top of.
    pub side: Rect,
    /// Height of the title row.
    pub row_h: f64,
    /// Left inset of the title.
    pub inset_l: f64,
    /// Right inset of the presence pill.
    pub inset_r: f64,
    /// Minimum gap between the title's end and the pill.
    pub gap: f64,
    /// Measured line box of the title.
    pub title_h: f64,
    /// Measured `(width, height)` of the presence pill, when one is shown.
    pub pill: Option<(f64, f64)>,
    /// Below this remaining title width the pill drops onto its own row so
    /// the workspace name stays legible instead of becoming a stub.
    pub min_title_w: f64,
}

/// Where the sidebar header's title and presence pill land.
#[derive(Debug, PartialEq)]
pub(crate) struct SidebarHeaderLayout {
    /// The whole header band (grows by one pill row when stacked).
    pub header: Rect,
    /// Left edge of the title.
    pub title_x: f64,
    /// Top of the title's line box.
    pub title_y: f64,
    /// Width the title may occupy before it is elided.
    pub title_max_w: f64,
    /// The presence pill's rect, when shown.
    pub pill: Option<Rect>,
}

/// Lay out the workspace title beside its presence pill.
///
/// The pill is measured first and reserved in full; the title is then elided
/// into whatever remains, so the two can never paint over each other. When the
/// remainder would fall below `min_title_w` the pill stacks under the title,
/// left-aligned with it, and the returned header grows to include that row.
pub(crate) fn sidebar_header_layout(spec: &SidebarHeaderSpec) -> SidebarHeaderLayout {
    let side = spec.side;
    let row_cy = side.y0 + spec.row_h / 2.0;
    let title_x = side.x0 + spec.inset_l;
    let title_y = line_top(row_cy, spec.title_h);
    let right = side.x1 - spec.inset_r;

    let Some((pill_w, pill_h)) = spec.pill else {
        return SidebarHeaderLayout {
            header: Rect::new(side.x0, side.y0, side.x1, side.y0 + spec.row_h),
            title_x,
            title_y,
            title_max_w: (right - title_x).max(0.0),
            pill: None,
        };
    };

    let inline_title_w = right - pill_w - spec.gap - title_x;
    if inline_title_w >= spec.min_title_w {
        let pill = Rect::new(
            right - pill_w,
            row_cy - pill_h / 2.0,
            right,
            row_cy + pill_h / 2.0,
        );
        return SidebarHeaderLayout {
            header: Rect::new(side.x0, side.y0, side.x1, side.y0 + spec.row_h),
            title_x,
            title_y,
            title_max_w: inline_title_w,
            pill: Some(pill),
        };
    }

    // Stacked: the pill gets its own row directly under the title, sharing
    // the title's left edge, and the header band includes it.
    let pill_y0 = side.y0 + spec.row_h - spec.gap;
    let pill = Rect::new(title_x, pill_y0, title_x + pill_w, pill_y0 + pill_h);
    SidebarHeaderLayout {
        header: Rect::new(side.x0, side.y0, side.x1, pill.y1 + spec.gap),
        title_x,
        title_y,
        title_max_w: (right - title_x).max(0.0),
        pill: Some(pill),
    }
}

#[cfg(test)]
#[allow(clippy::float_cmp, reason = "tests assert exact authored geometry")]
mod tests {
    use super::*;

    fn approx(a: f64, b: f64) -> bool {
        (a - b).abs() < 1e-6
    }

    #[test]
    fn centered_band_and_line_top_centre_inside_their_container() {
        // Phase: centered_band_shares_the_container_centre.
        {
            let box_ = Rect::new(0.0, 100.0, 400.0, 176.0);
            let band = centered_band(box_, 19.0, 8.0);
            assert!(approx(band.height(), 35.0));
            assert!(approx(band.center().y, box_.center().y));
            assert_eq!((band.x0, band.x1), (box_.x0, box_.x1));
        }
        // Phase: centered_band_never_exceeds_its_container.
        {
            let box_ = Rect::new(0.0, 0.0, 400.0, 60.0);
            let band = centered_band(box_, 200.0, 8.0);
            assert_eq!(band, box_);
        }
        // Phase: line_top_centres_the_measured_line_box.
        {
            assert!(approx(line_top(26.0, 20.0), 16.0));
        }
    }

    fn spec(side_w: f64, pill: Option<(f64, f64)>) -> SidebarHeaderSpec {
        SidebarHeaderSpec {
            side: Rect::new(68.0, 0.0, 68.0 + side_w, 800.0),
            row_h: 52.0,
            inset_l: 16.0,
            inset_r: 12.0,
            gap: 8.0,
            title_h: 20.0,
            pill,
            min_title_w: 72.0,
        }
    }

    #[test]
    fn sidebar_header_reserves_the_pill_before_budgeting_the_title() {
        // Phase: pill_is_reserved_before_the_title_is_budgeted.
        {
            let l = sidebar_header_layout(&spec(248.0, Some((96.0, 18.0))));
            let pill = l.pill.expect("pill shown");
            // Pill hugs the right inset and sits on the row's centre line.
            assert!(approx(pill.x1, 68.0 + 248.0 - 12.0));
            assert!(approx(pill.center().y, 26.0));
            // The title stops a full gap before the pill; they cannot overlap.
            assert!(approx(l.title_x + l.title_max_w + 8.0, pill.x0));
            // Title and pill share one centre line.
            assert!(approx(l.title_y + 10.0, pill.center().y));
            assert!(approx(l.header.height(), 52.0));
        }
        // Phase: a_wide_pill_on_a_narrow_sidebar_stacks_under_the_title.
        {
            let l = sidebar_header_layout(&spec(180.0, Some((120.0, 18.0))));
            let pill = l.pill.expect("pill shown");
            assert!(
                approx(pill.x0, l.title_x),
                "stacked pill shares the title's left edge"
            );
            assert!(
                pill.y0 >= l.title_y + 20.0,
                "pill sits below the title line"
            );
            assert!(l.header.y1 >= pill.y1, "header band includes the pill row");
            assert!(approx(l.title_max_w, 180.0 - 16.0 - 12.0));
        }
        // Phase: without_a_pill_the_title_owns_the_row.
        {
            let l = sidebar_header_layout(&spec(248.0, None));
            assert!(l.pill.is_none());
            assert!(approx(l.title_max_w, 248.0 - 16.0 - 12.0));
            assert!(approx(l.header.height(), 52.0));
        }
    }
}
