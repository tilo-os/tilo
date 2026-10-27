//! The bar's thumb: where it stands and how long it is for the content's
//! length and offset, and how a drag on it or a press on its track scrolls.

use super::tests::{both, mouse, vertical};
use super::*;
use crate::synthetic_input::cursor_moved;
use crate::testing::dispatch;

#[test]
fn thumb_geometry_tracks_ratio_offset_minimum_and_visibility() {
    {
        let short = vertical(600.0).vertical_thumb().expect("bar shown at 2x");
        let tall = vertical(1200.0).vertical_thumb().expect("bar shown at 4x");
        assert!(
            short.height() > tall.height(),
            "thumb must shrink as content grows: 2x={}, 4x={}",
            short.height(),
            tall.height(),
        );
        let track_len = 300.0 - 2.0 * BAR_MARGIN;
        let expected = 0.5 * track_len;
        assert!(
            (short.height() - expected).abs() < 1.0,
            "2x content thumb should be ≈half the track ({expected}), got {}",
            short.height(),
        );
    }

    {
        let thumb = vertical(100_000.0).vertical_thumb().expect("bar shown");
        assert!(
            thumb.height() >= MIN_THUMB_LEN - 1e-6,
            "thumb {} below min {}",
            thumb.height(),
            MIN_THUMB_LEN,
        );
    }

    {
        let s = vertical(150.0);
        assert!(s.thumb_rect(ScrollAxis::Vertical).is_none());
        assert!(s.vertical_track().is_none());
        let s = vertical(300.0);
        assert!(s.thumb_rect(ScrollAxis::Vertical).is_none());
    }

    {
        let mut s = vertical(1200.0);
        s = s.with_scrollbar(false);
        assert!(s.thumb_rect(ScrollAxis::Vertical).is_none());
    }

    {
        let mut s = vertical(1200.0);
        let top = s.vertical_thumb().unwrap();
        let track = s.vertical_track().unwrap();
        assert!(
            (top.y0 - track.y0).abs() < 1e-6,
            "thumb starts at track top"
        );

        s.scroll_by(0.0, 100_000.0); // clamp to max
        let bottom = s.vertical_thumb().unwrap();
        assert!(
            (bottom.y1 - track.y1).abs() < 1e-3,
            "fully scrolled thumb should reach track bottom: {} vs {}",
            bottom.y1,
            track.y1,
        );
    }
}

#[test]
fn thumb_drags_and_track_presses_scroll_by_offset_and_page() {
    {
        let mut s = vertical(1200.0); // max_scroll = 900
        let thumb = s.vertical_thumb().unwrap();
        let bar_x = s.rect.x1 - BAR_MARGIN - BAR_THICKNESS / 2.0;
        let mid_y = f64::midpoint(thumb.y0, thumb.y1);

        dispatch(&mut s, &cursor_moved(bar_x, mid_y));
        assert!(
            dispatch(&mut s, &mouse(ElementState::Pressed)),
            "press on thumb handled"
        );
        assert!(s.vbar.is_dragging(), "press on thumb begins a drag");
        assert!(s.wants_pointer_capture(), "drag requests pointer capture");
        assert_eq!(s.offset_y(), 0.0);

        dispatch(&mut s, &cursor_moved(bar_x, mid_y + 100.0));
        assert!(s.offset_y() > 0.0, "drag down increases offset");
        assert!(s.offset_y() <= 900.0, "offset stays clamped to max");
        let after_down = s.offset_y();

        dispatch(&mut s, &cursor_moved(bar_x, mid_y - 100.0));
        assert!(s.offset_y() < after_down, "drag up decreases offset");

        dispatch(&mut s, &mouse(ElementState::Released));
        assert!(!s.vbar.is_dragging(), "release ends the drag");
    }

    {
        let mut s = vertical(1200.0);
        let thumb = s.vertical_thumb().unwrap();
        let bar_x = s.rect.x1 - BAR_MARGIN - BAR_THICKNESS / 2.0;
        let below_y = thumb.y1 + 5.0;
        dispatch(&mut s, &cursor_moved(bar_x, below_y));
        assert!(
            dispatch(&mut s, &mouse(ElementState::Pressed)),
            "track click handled"
        );
        assert!(!s.vbar.is_dragging(), "track click does not start a drag");
        let paged = s.offset_y();
        assert!(paged > 0.0, "click below thumb pages down: {paged}");
        let page_frac = PAGE_FRACTION;
        assert!(
            (paged - 300.0 * page_frac).abs() < 1.0,
            "page step should be ≈viewport*{page_frac}, got {paged}",
        );
    }

    {
        let mut s = both(1200.0, 1200.0);
        let track = s.horizontal_track().expect("h-bar shown");
        let thumb = s.thumb_rect(ScrollAxis::Horizontal).expect("h-thumb");
        let mid_x = f64::midpoint(thumb.x0, thumb.x1);
        let bar_y = track.y0 + track.height() / 2.0;
        dispatch(&mut s, &cursor_moved(mid_x, bar_y));
        assert!(
            dispatch(&mut s, &mouse(ElementState::Pressed)),
            "press on h-thumb handled"
        );
        assert!(s.hbar.is_dragging(), "h-thumb press begins a drag");
        dispatch(&mut s, &cursor_moved(mid_x + 80.0, bar_y));
        assert!(
            s.offset_x() > 0.0,
            "drag right increases offset_x: {}",
            s.offset_x()
        );
        dispatch(&mut s, &mouse(ElementState::Released));
        assert!(!s.hbar.is_dragging(), "release ends the h-drag");
    }

    {
        let mut s = vertical(3000.0); // viewport 300, max_scroll 2700
        let bar_x = s.rect.x1 - BAR_MARGIN - BAR_THICKNESS / 2.0;
        let thumb = s.vertical_thumb().unwrap();
        let click_y = thumb.y1 + (s.rect.y1 - thumb.y1) * 0.8;
        dispatch(&mut s, &cursor_moved(bar_x, click_y));
        assert!(
            dispatch(&mut s, &mouse(ElementState::Pressed)),
            "track press handled"
        );
        assert!(
            s.hold.is_some(),
            "a track press below the thumb arms a hold"
        );
        assert!(!s.needs_animation_frame(), "a repeat is a wait, not motion");
        assert_eq!(
            s.next_wake_deadline(),
            s.hold.map(|h| h.next_at),
            "a held track pins the bar and wakes for its next page"
        );
        let after_first = s.offset_y();
        assert!(after_first > 0.0, "first page moved down: {after_first}");
        assert!(
            s.wants_pointer_capture(),
            "a live hold captures the pointer"
        );

        let mut prev = after_first;
        let mut steps = 0;
        for _ in 0..40 {
            let due = s.hold.map(|h| h.next_at).unwrap();
            s.advance_hold(due + Duration::from_millis(1));
            if s.offset_y() > prev {
                steps += 1;
                prev = s.offset_y();
            } else {
                break; // thumb reached the click point → paging stopped
            }
        }
        assert!(
            steps >= 2,
            "the hold auto-repeats past the first page ({steps} extra steps)"
        );

        let thumb_now = s.vertical_thumb().unwrap();
        assert!(
            thumb_now.y1 >= click_y - BAR_THICKNESS && thumb_now.y0 <= click_y + 1.0,
            "paging stopped at the click, thumb=({},{}) click_y={click_y}",
            thumb_now.y0,
            thumb_now.y1,
        );

        assert!(
            dispatch(&mut s, &mouse(ElementState::Released)),
            "release handled"
        );
        assert!(s.hold.is_none(), "release ends the hold");
    }
}
