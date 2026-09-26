#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::string_slice,
    clippy::panic,
    clippy::unreachable,
    reason = "in a test these are the assertion, not a hazard"
)]

use tilo_ui::widget::{
    AppKind, TabDescriptor, TileContent, WIDTH_MAX_PERMILLE, WindowGroup, WindowWidth,
    max_group_px, min_group_px,
};

/// Display widths this suite pins the width model against. The bug being
/// covered here only shows up across resolutions: a fixed pixel width is
/// a *different fraction* of every one of these, so a single-resolution
/// test cannot see it.
const DISPLAYS: [(&str, f64); 3] = [
    ("1600x900", 1600.0),
    ("2560x1440", 2560.0),
    ("3840x2160", 3840.0),
];

fn group(width: WindowWidth, usable: f64) -> WindowGroup {
    let mut g = WindowGroup::new(
        1,
        vec![(
            TabDescriptor::new(1, "T1"),
            TileContent::Stub {
                kind: AppKind::Terminal,
                label: "Terminal".into(),
            },
        )],
        1,
        width,
    );
    g.set_strip_usable_px(usable);
    g
}

#[test]
fn width_tiers_are_ordered_shares_of_every_display() {
    // Case `named_tiers_are_the_same_share_of_every_display`.
    // Every named tier is the *same share* of the strip on every display, not a
    // flat pixel count (which would be a different fraction of each one).
    {
        for (name, usable) in DISPLAYS {
            assert_eq!(
                WindowWidth::Narrow.resolve(usable),
                (usable * 0.333).round(),
                "Narrow must be a third of {name}",
            );
            assert_eq!(
                WindowWidth::Normal.resolve(usable),
                (usable * 0.667).round(),
                "Normal must be two thirds of {name}",
            );
            assert_eq!(
                WindowWidth::Wide.resolve(usable),
                usable,
                "Wide must fill {name}",
            );
        }
    }

    // Case `tiers_are_ordered_and_bounded_on_every_display`.
    // The tiers step upward and stay inside the strip on every display —
    // the property that makes cycling a window through them useful.
    {
        for (name, usable) in DISPLAYS {
            let narrow = WindowWidth::Narrow.resolve(usable);
            let normal = WindowWidth::Normal.resolve(usable);
            let wide = WindowWidth::Wide.resolve(usable);
            assert!(
                narrow < normal && normal < wide,
                "tiers must step up on {name}: {narrow} / {normal} / {wide}",
            );
            assert!(
                wide <= usable,
                "no tier may exceed the strip on {name}: {wide} > {usable}",
            );
        }
    }

    // Case `a_launched_window_is_a_dominant_share_of_every_display`.
    // A launched app defaults to `Normal`, which must be a dominant share of
    // the screen on every display.
    {
        for (name, usable) in DISPLAYS {
            let px = group(WindowWidth::Normal, usable).resolved_width_px();
            let share = px / usable;
            assert!(
                (0.6..=0.7).contains(&share),
                "a launched window must take 60-70% of {name}; got {px} px of \
             {usable} ({share:.2})",
            );
        }
    }

    // Case `the_widest_setting_reaches_the_edge_of_every_display`.
    // The widest setting must actually reach the screen edge, on any display —
    // including a large one, where a pixel-clamped width could never fill it.
    {
        for (name, usable) in DISPLAYS {
            let widest = WindowWidth::Fraction(WIDTH_MAX_PERMILLE).resolve(usable);
            assert_eq!(widest, usable, "the widest width must span {name} exactly");
            assert_eq!(
                max_group_px(usable),
                usable,
                "the resize ceiling on {name} must be the display, not a constant",
            );
        }
    }
}

#[test]
#[allow(
    clippy::cast_possible_truncation,
    reason = "pixel widths are bounded by the test's display sizes (at most 3840px), well \
within f32's exact-integer range"
)]
fn manual_resizes_track_the_pointer_and_round_trip_as_shares() {
    // Case `a_manual_resize_round_trips_across_displays_as_a_share`.
    // A manual resize is recorded as the fraction it landed on, so replaying
    // it onto a different display gives the same share, not the same pixel count.
    {
        // Drag the right edge of a Normal group on the 1600 px display from
        // its 1067 px out to 1200 px — three quarters of that display.
        let mut g = group(WindowWidth::Normal, 1600.0);
        let start = g.resolved_width_px();
        assert_eq!(start, 1067.0, "Normal on a 1600 px strip");
        g.begin_resize(start as f32);
        g.update_resize(1200.0);
        g.end_resize();

        let stored = g.width();
        assert_eq!(
            stored,
            WindowWidth::Fraction(750),
            "the resize stores the share of the display, not 1200 px",
        );
        assert_eq!(
            g.take_pending_width_change(),
            Some(750),
            "and hands the shell that same share to persist",
        );

        // The share holds across displays; the pixel count scales.
        for (name, usable) in DISPLAYS {
            let restored = group(stored, usable);
            assert_eq!(
                restored.resolved_width_px(),
                (usable * 0.75).round(),
                "the stored width must still be three quarters of {name}",
            );
        }
    }

    // Case `the_resize_floor_scales_with_the_display_but_never_goes_unusable`.
    // The resize floor is a fraction too, with an absolute pixel backstop so
    // a narrow display cannot yield an unusable sliver.
    {
        // 15 % of 3840 is 576 px — the fraction dominates on a large display.
        assert_eq!(min_group_px(3840.0), 576.0);
        // 15 % of 1200 is 180 px, below the absolute floor, so the floor wins.
        assert_eq!(min_group_px(1200.0), 240.0);
        // A drag far past the floor pins there rather than collapsing.
        let mut g = group(WindowWidth::Normal, 1600.0);
        g.begin_resize(g.resolved_width_px() as f32);
        g.update_resize(-50_000.0);
        assert_eq!(g.resolved_width_px(), min_group_px(1600.0));
    }

    // Case `window_group_drag_right_edge_resizes`.
    // The right-edge drag still tracks the pointer 1:1 in pixels — the
    // fraction is a storage representation, not a snapping grid.
    {
        let mut g = group(WindowWidth::Normal, 1200.0);
        assert_eq!(g.resolved_width_px(), 800.0);
        g.begin_resize(800.0);
        g.update_resize(950.0);
        g.end_resize();
        assert_eq!(g.resolved_width_px(), 950.0);

        g.begin_resize(950.0);
        g.update_resize(200.0);
        g.end_resize();
        assert_eq!(
            g.resolved_width_px(),
            min_group_px(1200.0),
            "a drag past the floor pins at the display's minimum",
        );
    }
}
