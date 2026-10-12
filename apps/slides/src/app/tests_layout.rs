//! The editor's chrome at every width and text size: the toolbar, the
//! slide rail and the inspector docking or folding into flaps, the inspector
//! scrolling when it overflows, and wheels and scrollbars over the rail and
//! the overview. View state only: nothing here edits the deck.
//!
//! The geometry is the editor's own: each case sizes it within one visit
//! ([`size`]), as a frame at that width does, and reads what it lays out.
#![allow(
    clippy::float_cmp,
    reason = "exact geometry and values the editor computes, not measurements"
)]

use winit::event::MouseScrollDelta;

use super::bench::{Bench, deck_of};
use super::editor_chrome::UiButton;
use super::*;
use ui::TextRole;
use ui::widget::{ButtonFace, ControlSize};

/// The editor over one slide with a text box, for the chrome around it.
fn layout_bench() -> Bench {
    let mut s = Slide::new(Layout::TitleContent);
    s.add(Element::text("hello", 96.0, 90.0, 1088.0, 100.0, 54.0));
    Bench::open(&deck_of(vec![s]), super::bench::WINDOW)
}

/// The editor over one slide whose single text box is selected.
fn selected_text_bench() -> Bench {
    let mut s = Slide::new(Layout::TitleContent);
    let id = s.add(Element::text("Hi", 100.0, 100.0, 400.0, 100.0, 40.0));
    let mut bench = Bench::open(&deck_of(vec![s]), super::bench::WINDOW);
    bench.with(|app| app.sel = Some(id));
    bench
}

/// `slide_count` slides of one text box each.
fn tall_deck(slide_count: usize) -> Deck {
    deck_of(
        (0..slide_count)
            .map(|_| {
                let mut s = Slide::new(Layout::TitleContent);
                s.add(Element::text("slide", 96.0, 90.0, 1000.0, 100.0, 40.0));
                s
            })
            .collect(),
    )
}

/// Lay the editor out at `w` × `h` from its first frame there: panels at
/// their default for the width, as a window opened at that size shows them.
fn size(app: &mut SlidesApp, w: f64, h: f64) {
    app.rect = Rect::new(0.0, 0.0, w, h);
    app.chrome_metrics = ChromeMetrics::fallback();
    (app.rail_drawer, app.inspector_drawer) = SlidesApp::new_drawers();
    app.flap_uninitialised = true;
    app.sync_flaps(Some(&Theme::dark()));
}

/// [`size`] at `w` × 800 with the chrome metrics measured at `text_scale`,
/// exactly as the first draw of a frame resolves them.
fn measure(app: &mut SlidesApp, w: f64, text_scale: f32) -> TextRenderer {
    size(app, w, 800.0);
    let theme = Theme::dark().with_text_scale(text_scale);
    let mut tr = TextRenderer::new();
    tr.set_text_scale(theme.text_scale);
    app.refresh_chrome_metrics(&mut tr, &theme);
    tr
}

/// Toolbar controls other than the rail hamburger, left to right.
fn toolbar_controls(chrome: &Chrome) -> Vec<&UiButton> {
    let mut controls: Vec<&UiButton> = chrome
        .buttons
        .iter()
        .enumerate()
        .filter(|(index, button)| {
            chrome.button_zone(*index) == super::editor_chrome::Zone::Content
                && !matches!(button.hit, Hit::ToggleRail)
                && button.rect.center().y < chrome.toolbar.y1
        })
        .map(|(_, button)| button)
        .collect();
    controls.sort_by(|a, b| a.rect.x0.total_cmp(&b.rect.x0));
    controls
}

/// Toolbar/rail/inspector chrome density at 1x and 2x, narrow titles, large text and fitting pills/segments.
#[test]
#[expect(
    clippy::too_many_lines,
    reason = "cases merged from separate tests; each case stays self-contained"
)]
fn chrome_density_follows_measured_text_at_every_width() {
    // Case `title_yields_to_the_leftmost_placed_toolbar_control_at_480`.
    // At 480 px the insert tools all move behind the overflow button, which
    // is then pulled left of the nominal tool slot. The title must yield to
    // that placed control, not to the slot — otherwise the ellipsis paints
    // under the "⋯" button.
    {
        let mut bench = layout_bench();
        bench.with(|app| {
            let _tr = measure(app, 480.0, 1.0);
            let chrome = app.compute_chrome();
            let first = toolbar_controls(&chrome)
                .first()
                .map(|b| b.rect.x0)
                .expect("toolbar controls");
            assert!(
                chrome.title_rect.x1 <= first + 0.01,
                "title slot ends at {:.1} but a control starts at {first:.1}",
                chrome.title_rect.x1
            );
            let title = chrome
                .labels
                .iter()
                .find(|l| l.pos == chrome.title_pos)
                .expect("title label");
            let max_w = title.max_w.expect("title is elided at paint time");
            assert!(max_w > 0.0, "480 px still shows a title fragment");
            assert!(chrome.title_pos.x + max_w <= first - app.chrome_metrics.s3 + 0.01);
        });
    }

    // Case `one_x_chrome_density_is_unchanged`.
    // At 1× the measured chrome reproduces the classic density exactly:
    // 56 px toolbar, 34 px pills, 30 px rail controls, 28 px inspector pills.
    {
        let mut bench = layout_bench();
        bench.with(|app| {
            let _tr = measure(app, NARROW_BREAKPOINT + 400.0, 1.0);
            let chrome = app.compute_chrome();
            assert!((chrome.toolbar.height() - 56.0).abs() < 0.01, "toolbar");
            let present = chrome
                .buttons
                .iter()
                .find(|b| matches!(b.hit, Hit::Present))
                .expect("Present");
            assert!(
                (present.rect.height() - ControlSize::Regular.height(&Theme::dark())).abs() < 0.01,
                "toolbar pill"
            );
            let add = chrome
                .buttons
                .iter()
                .find(|b| matches!(b.hit, Hit::NewSlide) && chrome.rail.contains(b.rect.center()))
                .expect("rail Add");
            assert!(
                (add.rect.height() - ControlSize::Regular.height(&Theme::dark())).abs() < 0.01,
                "rail control"
            );
            let layout_pill = chrome
                .buttons
                .iter()
                .find(|b| {
                    matches!(b.hit, Hit::OpenLayoutMenu)
                        && matches!(b.kind, ButtonKind::LayoutPicker)
                })
                .expect("the shared slide layout picker");
            assert!(
                (layout_pill.rect.height() - ControlSize::Regular.height(&Theme::dark())).abs()
                    < 0.01,
                "inspector pill"
            );
        });
    }

    // Case `two_x_chrome_rows_follow_measured_text`.
    // At 2× every toolbar pill, rail control and inspector row follows the
    // measured text: labels fit their slots, pills share one centre line,
    // and no section caption is painted under the control below it.
    {
        let mut bench = layout_bench();
        bench.with(|app| {
            let mut tr = measure(app, NARROW_BREAKPOINT + 400.0, 2.0);
            // Select the text element so the inspector shows every section.
            app.sel = app
                .slide()
                .and_then(|s| s.elements.first().map(|e| e.id.clone()));
            let chrome = app.compute_chrome();
            let m = app.chrome_metrics.clone();
            assert!(m.lh_label > 30.0, "2x control line box {}", m.lh_label);
            assert!(
                chrome.toolbar.height() >= m.lh_heading + 2.0 * m.s2 - 0.01
                    && chrome.toolbar.height() > 56.0,
                "toolbar grew to its two-line title block"
            );

            let controls = toolbar_controls(&chrome);
            let centre = controls[0].rect.center().y;
            for b in &controls {
                assert!((b.rect.center().y - centre).abs() < 0.01, "one centre line");
                let theme = Theme::dark().with_text_scale(2.0);
                let size = if matches!(b.kind, ButtonKind::Tool { .. }) {
                    ControlSize::Small
                } else {
                    ControlSize::Regular
                };
                assert!(
                    b.rect.height() >= size.height(&theme) - 0.01,
                    "the shared control fits its measured line and vertical padding"
                );
                assert!(
                    b.rect.height()
                        >= f64::from(size.label_style(&theme).line_height_px())
                            + 2.0 * f64::from(theme.tokens.spacing.s1)
                            - 0.01,
                    "no control clips its label at 2x text size"
                );
                if let ButtonKind::Command { label, icon, .. } | ButtonKind::Tool { label, icon } =
                    &b.kind
                {
                    let face = ButtonFace::new(Rect::ZERO, label).control_size(size);
                    let face = match icon {
                        Some(icon) => face.icon(*icon),
                        None => face,
                    };
                    let width = face.intrinsic_width(&mut tr, &theme);
                    assert!(
                        b.rect.width() >= width - 0.01,
                        "'{label}' needs {width:.1} for its full shared face, pill is {:.1}",
                        b.rect.width()
                    );
                }
            }
            for pair in controls.windows(2) {
                assert!(
                    pair[0].rect.x1 <= pair[1].rect.x0 + 0.01,
                    "toolbar controls never overlap"
                );
            }

            // Inspector: captions keep clear of the control that follows them,
            // and every text control is at least its line box tall.
            let in_inspector = |r: Rect| chrome.inspector.contains(r.center());
            for l in chrome
                .labels
                .iter()
                .filter(|l| l.role == TextRole::Caption && chrome.inspector.contains(l.pos))
            {
                let caption = Rect::new(
                    l.pos.x,
                    l.pos.y,
                    l.pos.x + 1.0,
                    l.pos.y
                        + f64::from(Theme::dark().text(TextRole::Caption).line_height_px())
                            * m.growth,
                );
                for b in chrome.buttons.iter().filter(|b| in_inspector(b.rect)) {
                    assert!(
                        b.rect.y0 >= caption.y1 - 0.01 || b.rect.y1 <= caption.y0 + 0.01,
                        "'{}' caption at y {:.1}..{:.1} collides with a control at {:.1}..{:.1}",
                        l.text,
                        caption.y0,
                        caption.y1,
                        b.rect.y0,
                        b.rect.y1
                    );
                }
            }
            for b in chrome.buttons.iter().filter(|b| in_inspector(b.rect)) {
                match &b.kind {
                    ButtonKind::Command { label, .. } if !label.is_empty() => {
                        assert!(
                            b.rect.height() >= m.lh_label - 0.01,
                            "'{label}' row fits its text"
                        );
                    }
                    // Alignment segments wrap to two rows rather than clip their
                    // labels once a quarter-width cell is too narrow for them.
                    ButtonKind::Segment { label, .. } => {
                        let (w, _) =
                            tr.measure_style(label, None, &Theme::dark().text(TextRole::Label));
                        assert!(
                            b.rect.width() >= f64::from(w) + 2.0 * m.s2 - 0.01,
                            "segment '{label}' needs {w:.1}, cell is {:.1}",
                            b.rect.width()
                        );
                    }
                    _ => {}
                }
            }
            // The font-size input reserve fits two square steppers plus a
            // three-digit value at the scaled control size, and the B/I toggles
            // never paint over it (they wrap below when the row is too narrow).
            let font_input = chrome
                .font_input_rect
                .expect("text element shows the TYPE row");
            let (w, _) = tr.measure_style("999 pt", None, &Theme::dark().text(TextRole::Label));
            assert!(font_input.width() >= 2.0 * m.control_h + f64::from(w) + 2.0 * m.s2 - 0.01);
            // The speaker-notes box never inverts or folds back over the sections:
            // it is at least one control tall and starts below the last control.
            let notes = chrome.notes_box.expect("notes box");
            assert!(notes.height() >= m.control_h - 0.01, "notes box {notes:?}");
            let sections_bottom = chrome
                .buttons
                .iter()
                .filter(|b| in_inspector(b.rect) && !matches!(b.hit, Hit::EditNotes))
                .map(|b| b.rect.y1)
                .fold(0.0_f64, f64::max);
            assert!(
                notes.y0 >= sections_bottom - 0.01,
                "notes box starts under the sections"
            );
            let inspector_controls: Vec<Rect> = chrome
                .buttons
                .iter()
                .enumerate()
                .filter(|(index, b)| {
                    chrome.button_zone(*index) == super::editor_chrome::Zone::Inspector
                        && !matches!(b.hit, Hit::EditNotes)
                })
                .map(|(_, b)| b.rect)
                .chain([font_input, notes])
                .collect();
            for (i, a) in inspector_controls.iter().enumerate() {
                for b in inspector_controls.iter().skip(i + 1) {
                    let overlap = a.intersect(*b);
                    assert!(
                        overlap.width() <= 0.01 || overlap.height() <= 0.01,
                        "inspector controls overlap: {a:?} vs {b:?}"
                    );
                }
            }
            // Rail: the pinned action row grew with its labels.
            for b in chrome
                .buttons
                .iter()
                .filter(|b| chrome.rail.contains(b.rect.center()))
            {
                if let ButtonKind::Command { label, .. } = &b.kind {
                    assert!(b.rect.height() >= m.lh_label - 0.01, "rail '{label}' fits");
                }
            }
        });
    }

    // Case `phone_toolbar_elides_title_before_compact_controls`.
    {
        let mut bench = layout_bench();
        bench.with(|app| {
            size(app, 390.0, 800.0);
            let chrome = app.compute_chrome();
            let first_right_control = chrome
                .buttons
                .iter()
                .filter(|button| {
                    !matches!(button.hit, Hit::ToggleRail)
                        && button.rect.center().y < chrome.toolbar.y1
                })
                .map(|button| button.rect.x0)
                .min_by(f64::total_cmp)
                .unwrap();
            assert!(
                chrome.title_rect.x1 <= first_right_control,
                "the two-line title must not paint under compact toolbar controls"
            );
        });
    }

    // Case `large_text_chrome_keeps_hidden_tools_on_discoverable_routes`.
    {
        let mut bench = Bench::sample();
        bench.with(|app| {
            size(app, 1440.0, 960.0);
            app.text_scale = 2.0;
            let chrome = app.compute_chrome();
            assert!(
                chrome
                    .buttons
                    .iter()
                    .any(|button| matches!(button.hit, Hit::ToggleToolbarMenu))
            );
            assert!(
                chrome
                    .buttons
                    .iter()
                    .any(|button| matches!(button.hit, Hit::ToggleInspector))
            );
            assert!(
                chrome
                    .buttons
                    .iter()
                    .any(|button| matches!(button.hit, Hit::Present))
            );
            assert!(
                chrome.slide_dest.width() > 600.0,
                "large type keeps a practical authored-slide canvas"
            );
        });
    }

    // Case `deck_theme_pills_fit_their_full_labels_and_never_overlap`.
    {
        let mut bench = layout_bench();
        bench.with(|app| {
        // Docked inspector, nothing selected → the slide-level inspector shows
        // the five deck-theme pills. Each must be wide enough that its full name
        // fits (drawn from `dot.x1 + 6`), and pills on a row must not overlap —
        // this is the exact geometry `draw_button` paints, so the assertion is
        // on the real path.
        size(app, 1280.0, 800.0);
        app.sel = None;
        let chrome = app.compute_chrome();
        let pills: Vec<&UiButton> = chrome
            .buttons
            .iter()
            .filter(|b| {
                matches!(b.kind, ButtonKind::Theme { .. })
                    && chrome.inspector.contains(b.rect.center())
            })
            .collect();
        assert_eq!(
            pills.len(),
            PaletteKind::ALL.len(),
            "all five theme pills are present"
        );
        for b in &pills {
            if let ButtonKind::Theme { kind, .. } = &b.kind {
                let label_right = b.rect.x0 + 26.0 + app.chrome_metrics.width(kind.label(), editor_chrome::Measure::Button { icon: true }) - 26.0;
                assert!(
                    label_right <= b.rect.x1 + 0.5,
                    "'{}' pill is wide enough for its full label (label right {:.1} <= pill right {:.1})",
                    kind.label(),
                    label_right,
                    b.rect.x1
                );
            }
        }
        for (i, a) in pills.iter().enumerate() {
            for b in &pills[i + 1..] {
                let same_row = (a.rect.y0 - b.rect.y0).abs() < 1.0;
                if same_row {
                    assert!(
                        a.rect.x1 <= b.rect.x0 + 0.5 || b.rect.x1 <= a.rect.x0 + 0.5,
                        "deck-theme pills on the same row must not overlap"
                    );
                }
            }
        }
        });
    }

    // Case `text_alignment_segments_fill_the_shared_inspector_columns`.
    {
        let mut bench = selected_text_bench();
        bench.with(|app| {
            let _id = app.sel.clone().unwrap();
            size(app, 1280.0, 800.0);
            let chrome = app.compute_chrome();
            let segments: Vec<Rect> = chrome
                .buttons
                .iter()
                .filter_map(|button| {
                    matches!(button.kind, ButtonKind::Segment { .. }).then_some(button.rect)
                })
                .collect();

            assert_eq!(segments.len(), TextAlign::ALL.len());
            let inset = f64::from(Theme::dark().tokens.spacing.s3);
            let left = chrome.inspector.x0 + inset;
            let right = chrome.inspector.x1 - inset;
            assert_eq!(segments[0].x0, left);
            assert_eq!(segments.last().unwrap().x1, right);
            let width = segments[0].width();
            for pair in segments.windows(2) {
                if (pair[0].y0 - pair[1].y0).abs() < 0.5 {
                    assert_eq!(pair[0].x1, pair[1].x0, "segments meet without gaps");
                } else {
                    assert_eq!(pair[0].x1, right, "each wrapped row fills the right edge");
                    assert_eq!(pair[1].x0, left, "each wrapped row starts at the left edge");
                }
                assert_eq!(pair[1].width(), width, "segments divide the row evenly");
            }
        });
    }
}

/// The thumbnail rail docks when wide and becomes a toggled overlay flap when narrow.
#[test]
fn rail_docks_wide_and_overlays_narrow() {
    // Case `wide_window_docks_the_rail_beside_the_canvas`.
    {
        let mut bench = layout_bench();
        bench.with(|app| {
            size(app, NARROW_BREAKPOINT + 200.0, 800.0);
            assert_eq!(app.rail_layout(), SplitLayout::Docked);
            assert!(app.rail_open(), "docked rail is open by default");
            let chrome = app.compute_chrome();
            assert_eq!(chrome.layout, SplitLayout::Docked);
            // The rail occupies the full left column and the canvas starts past it.
            assert!((chrome.rail.x0 - app.rect.x0).abs() < 1e-6);
            assert!(
                chrome.rail.width() > app.rail_drawer.resolved_docked_width() - 1.0,
                "docked rail is full width"
            );
            assert!(
                chrome.slide_dest.x0 > chrome.rail.x1 - 1.0,
                "canvas sits to the right of the docked rail"
            );
            assert_eq!(chrome.scrim.width(), 0.0, "no scrim while docked");
            // No hamburger when the rail is docked + fully open.
            assert!(
                chrome
                    .buttons
                    .iter()
                    .any(|b| matches!(b.hit, Hit::ToggleRail)),
                "the docked rail retains its shared hide-sidebar control"
            );
        });
    }

    // Case `narrow_window_collapses_rail_to_a_closed_overlay`.
    {
        let mut bench = layout_bench();
        bench.with(|app| {
            size(app, NARROW_BREAKPOINT - 200.0, 800.0);
            assert_eq!(app.rail_layout(), SplitLayout::Overlay);
            assert!(!app.rail_open(), "overlay rail is closed by default");
            let chrome = app.compute_chrome();
            assert_eq!(chrome.layout, SplitLayout::Overlay);
            // Canvas spans the full width (rail no longer insets it).
            assert!(
                (chrome.slide_dest.x0 - app.rect.x0)
                    < (app.rail_drawer.resolved_docked_width() * 0.5),
                "narrow canvas reclaims the rail's column"
            );
            // The closed flap is fully off the left edge, and no scrim shows.
            assert!(
                chrome.rail.x1 <= app.rect.x0 + 1.0,
                "closed flap is off-screen left"
            );
            assert_eq!(
                chrome.scrim.width(),
                0.0,
                "no scrim while the flap is closed"
            );
            // A hamburger is offered to summon it.
            assert!(
                chrome
                    .buttons
                    .iter()
                    .any(|b| matches!(b.hit, Hit::ToggleRail)),
                "narrow layout offers a hamburger to summon the rail"
            );
        });
    }

    // Case `opening_the_overlay_flap_reveals_the_rail_over_a_scrim`.
    {
        let mut bench = layout_bench();
        bench.with(|app| {
            size(app, NARROW_BREAKPOINT - 200.0, 800.0);
            // Summon via the production toggle path, then settle the animation.
            app.rail_drawer.toggle();
            app.rail_drawer.snap(true);
            // Resolve the retained drawer's rect after changing its reveal,
            // as the production frame does before computing the chrome.
            app.resolve_drawers();
            assert!(app.rail_open());
            let chrome = app.compute_chrome();
            assert_eq!(chrome.layout, SplitLayout::Overlay);
            // A fully revealed shared drawer floats at the theme's inset,
            // leaving its dismissal scrim accessible beside the rail.
            let inset = f64::from(Theme::dark().tokens.spacing.s2);
            assert!(
                (chrome.rail.x0 - app.rect.x0 - inset).abs() < 1.0,
                "open drawer retains the shared floating inset"
            );
            assert!(
                chrome.scrim.width() > 0.0,
                "an open overlay flap dims the body"
            );
            assert!(
                chrome.buttons.iter().any(|button| {
                    matches!(&button.kind, ButtonKind::Icon { name, .. } if name == &tr("slides-hide-slides"))
                        && matches!(button.hit, Hit::ToggleRail)
                }),
                "the open overlay has a named Hide slides control in its own header"
            );
        });
    }

    // Case `hamburger_hit_toggles_the_overlay_flap`.
    {
        let mut bench = layout_bench();
        bench.with(|app| {
            size(app, NARROW_BREAKPOINT - 200.0, 800.0);
            assert!(!app.rail_open());
            // Drive the real action dispatcher, not a private setter.
            app.apply_hit(Hit::ToggleRail);
            assert!(app.rail_open(), "ToggleRail opens the collapsed flap");
            app.apply_hit(Hit::ToggleRail);
            assert!(!app.rail_open(), "ToggleRail closes it again");
        });
    }
}

/// The inspector docks when wide and becomes a toggled overlay flap when narrow.
#[test]
fn inspector_docks_wide_and_overlays_narrow() {
    // Case `wide_window_docks_the_inspector`.
    {
        let mut bench = layout_bench();
        bench.with(|app| {
            size(app, NARROW_BREAKPOINT + 200.0, 800.0);
            assert_eq!(app.inspector_layout(), SplitLayout::Docked);
            assert!(app.inspector_open(), "docked inspector is open by default");
            let chrome = app.compute_chrome();
            assert_eq!(chrome.inspector_layout, SplitLayout::Docked);
            // Docked inspector is a full-width right column and the canvas ends at
            // its left edge.
            assert!(
                chrome.inspector.width() > app.inspector_drawer.resolved_docked_width() - 1.0,
                "docked inspector is full width"
            );
            assert!(
                chrome.slide_dest.x1 < chrome.inspector.x0 + 1.0,
                "canvas sits to the left of the docked inspector"
            );
        });
    }

    // Case `narrow_window_collapses_inspector_to_a_closed_overlay`.
    {
        let mut bench = layout_bench();
        bench.with(|app| {
            // Below the inspector breakpoint the inspector becomes a closed flap and
            // the canvas reclaims its column (spanning to the right edge).
            size(app, INSPECTOR_BREAKPOINT - 120.0, 800.0);
            assert_eq!(app.inspector_layout(), SplitLayout::Overlay);
            assert!(
                !app.inspector_open(),
                "overlay inspector is closed by default"
            );
            let chrome = app.compute_chrome();
            assert!(
                chrome.inspector.area() == 0.0,
                "closed inspector has no exposed panel geometry"
            );
            // A toggle is offered to summon it.
            assert!(
                chrome
                    .buttons
                    .iter()
                    .any(|b| matches!(b.hit, Hit::ToggleInspector)),
                "narrow layout offers an inspector toggle"
            );
        });
    }

    // Case `inspector_toggle_hit_opens_the_overlay_flap`.
    {
        let mut bench = layout_bench();
        bench.with(|app| {
            size(app, INSPECTOR_BREAKPOINT - 120.0, 800.0);
            assert!(!app.inspector_open());
            // Drive the real action dispatcher, not a private setter.
            app.apply_hit(Hit::ToggleInspector);
            assert!(
                app.inspector_open(),
                "ToggleInspector opens the collapsed flap"
            );
            app.apply_hit(Hit::ToggleInspector);
            assert!(!app.inspector_open(), "ToggleInspector closes it again");
        });
    }
}

/// Compact drawers, their exclusivity, and reflow across the breakpoint.
#[test]
fn compact_editor_drawers_and_breakpoint_reflow() {
    // Case `compact_editor_keeps_the_canvas_wide_with_both_panels_as_drawers`.
    {
        let mut bench = layout_bench();
        bench.with(|app| {
            size(app, 820.0, 800.0);
            let chrome = app.compute_chrome();
            assert_eq!(chrome.layout, SplitLayout::Overlay);
            assert_eq!(chrome.inspector_layout, SplitLayout::Overlay);
            assert!(
                chrome.slide_dest.width() > 700.0,
                "closed drawers leave a useful slide canvas at compact laptop widths"
            );
            assert!(
                chrome
                    .buttons
                    .iter()
                    .any(|button| matches!(button.hit, Hit::ToggleRail))
                    && chrome
                        .buttons
                        .iter()
                        .any(|button| matches!(button.hit, Hit::ToggleInspector)),
                "both drawers retain explicit toolbar controls"
            );
        });
    }

    // Case `compact_drawers_are_mutually_exclusive`.
    {
        let mut bench = layout_bench();
        bench.with(|app| {
            size(app, 820.0, 800.0);
            app.apply_hit(Hit::ToggleRail);
            assert!(app.rail_open());
            assert!(!app.inspector_open());
            app.apply_hit(Hit::ToggleInspector);
            assert!(app.inspector_open(), "properties drawer opens");
            assert!(
                !app.rail_open(),
                "opening properties closes the slide drawer immediately"
            );
        });
    }

    // Case `resizing_across_the_breakpoint_returns_to_the_layout_default`.
    {
        let mut bench = layout_bench();
        bench.with(|app| {
            size(app, NARROW_BREAKPOINT - 200.0, 800.0);
            // User opens the flap while narrow.
            app.rail_drawer.resolve(app.rect);
            app.apply_hit(Hit::ToggleRail);
            assert!(app.rail_open());
            // Widen past the breakpoint: the stale preference drops and the rail
            // adopts the docked default (open). Then narrow again → closed default.
            app.rect = Rect::new(0.0, 0.0, NARROW_BREAKPOINT + 200.0, 800.0);
            app.rail_drawer.resolve(app.rect);
            assert_eq!(app.rail_layout(), SplitLayout::Docked);
            assert!(app.rail_open(), "docked default is open");
            app.rect = Rect::new(0.0, 0.0, NARROW_BREAKPOINT - 200.0, 800.0);
            app.rail_drawer.resolve(app.rect);
            assert!(
                !app.rail_open(),
                "overlay default is closed after re-narrowing"
            );
        });
    }

    // Case `same_instance_reflows_both_panels_wide_then_narrow`.
    {
        let mut bench = layout_bench();
        bench.with(|app| {
            // The breakpoint MUST be re-derived from the *current* width every
            // frame, not cached at construction — resize a single instance wide,
            // then narrow, and confirm both panels flip layout.
            size(app, 1280.0, 800.0);
            assert_eq!(app.rail_layout(), SplitLayout::Docked);
            assert_eq!(app.inspector_layout(), SplitLayout::Docked);
            let wide = app.compute_chrome();
            assert_eq!(wide.layout, SplitLayout::Docked);
            assert_eq!(wide.inspector_layout, SplitLayout::Docked);

            // Simulate a resize down to a phone-ish width and run the per-frame
            // flap sync (the real path) — both panels must become overlay flaps and
            // the canvas must span the full width.
            app.rect = Rect::new(0.0, 0.0, 560.0, 720.0);
            app.sync_flaps(Some(&Theme::dark()));
            assert_eq!(
                app.rail_layout(),
                SplitLayout::Overlay,
                "rail reflows to overlay"
            );
            assert_eq!(
                app.inspector_layout(),
                SplitLayout::Overlay,
                "inspector reflows to overlay"
            );
            let narrow = app.compute_chrome();
            assert_eq!(narrow.layout, SplitLayout::Overlay);
            assert_eq!(narrow.inspector_layout, SplitLayout::Overlay);
            assert!(
                narrow.slide_dest.x1 > 560.0 - app.rail_drawer.resolved_docked_width(),
                "canvas reclaims the collapsed panels' width when narrow"
            );
        });
    }
}

/// Wheel scrolling of rail and overview, and the overview scrollbar drag.
#[test]
fn wheel_and_scrollbar_scroll_rail_and_overview() {
    // Case `wheel_scrolls_the_thumbnail_rail_only_over_the_rail`.
    {
        let mut bench = Bench::open(&tall_deck(16), super::bench::WINDOW);
        bench.with(|app| {
            size(app, 1280.0, 560.0);
            // The shared rail is the scroll authority: sync it to this frame, then
            // read the overflow it computed.
            app.sync_page_rail();
            let max_scroll = app.page_sidebar.max_scroll();
            assert!(max_scroll > 0.0, "a 16-slide rail overflows a 560px window");

            // Pointer over the rail → wheel scrolls it (down).
            let rail = app.rail_rect();
            app.last_mouse = Point::new(rail.center().x, rail.center().y);
            let mut handled = false;
            app.on_wheel(MouseScrollDelta::LineDelta(0.0, -3.0), 1.0, &mut handled);
            assert!(handled, "wheel over the rail is handled");
            assert!(app.rail_scroll > 0.0, "wheel over the rail scrolls it");

            // Overscroll clamps to the maximum, so every thumbnail stays reachable.
            for _ in 0..40 {
                let mut h = false;
                app.on_wheel(MouseScrollDelta::LineDelta(0.0, -20.0), 1.0, &mut h);
            }
            assert!(
                (app.rail_scroll - max_scroll).abs() < 1.0,
                "rail scroll clamps to max"
            );

            // Pointer over the canvas → the rail does not move.
            app.rail_scroll = 0.0;
            app.last_mouse = Point::new(app.rect.x1 - 80.0, app.rect.center().y);
            let mut h2 = false;
            app.on_wheel(MouseScrollDelta::LineDelta(0.0, -3.0), 1.0, &mut h2);
            assert_eq!(
                app.rail_scroll, 0.0,
                "wheel over the canvas leaves the rail alone"
            );
        });
    }

    // Case `wheel_scrolls_overview_and_last_slide_becomes_reachable`.
    {
        let mut bench = Bench::open(&tall_deck(12), super::bench::WINDOW);
        bench.with(|app| {
            app.bootstrapped = true;
            app.mode = Mode::Overview;
            // 800x600 is exactly the size the prior review reported cutting off slide 5.
            app.rect = Rect::new(0.0, 0.0, 800.0, 600.0);
            let (_, _, max_scroll) = app.compute_overview();
            assert!(
                max_scroll > 0.0,
                "the 12-slide deck overflows the 800x600 overview"
            );

            let mut handled = false;
            app.on_wheel(MouseScrollDelta::LineDelta(0.0, -3.0), 1.0, &mut handled);
            assert!(
                handled && app.overview.scroll() > 0.0,
                "wheel scrolls the overview"
            );

            // Scroll to the very end and confirm the last slide's card is on-screen.
            for _ in 0..60 {
                let mut h = false;
                app.on_wheel(MouseScrollDelta::LineDelta(0.0, -20.0), 1.0, &mut h);
            }
            assert!(
                (app.overview.scroll() - max_scroll).abs() < 1.0,
                "overview scroll clamps to max"
            );
            let (area, thumbs, _) = app.compute_overview();
            let last = thumbs.last().expect("a slide").1;
            assert!(
                last.y0 < area.y1 && last.y1 > area.y0,
                "the last slide is inside the viewport once scrolled to the end"
            );
        });
    }

    // Case `overview_scrollbar_thumb_drag_scrolls_the_grid`.
    {
        let mut bench = Bench::open(&tall_deck(12), super::bench::WINDOW);
        bench.with(|app| {
            app.bootstrapped = true;
            app.mode = Mode::Overview;
            app.rect = Rect::new(0.0, 0.0, 800.0, 600.0);
            let (area, _, max_scroll) = app.compute_overview();
            let (_, thumb) = app
                .overview
                .scrollbar_rects(area, 0.0, max_scroll)
                .expect("overflow ⇒ scrollbar");
            // Start a thumb drag (the real on_press path constructs this), then move.
            app.drag = Drag::ScrollOverview {
                start_y: thumb.center().y,
                orig_scroll: 0.0,
            };
            app.drag_move(Point::new(thumb.center().x, thumb.center().y + 120.0));
            assert!(
                app.overview.scroll() > 0.0,
                "dragging the thumb down scrolls the grid"
            );
        });
    }
}

/// The 2x inspector scrolls instead of overflowing; a fitting 1x inspector stays inert.
#[test]
#[expect(
    clippy::too_many_lines,
    reason = "cases merged from separate tests; each case stays self-contained"
)]
fn inspector_scrolls_only_when_it_overflows() {
    // Case `two_x_inspector_scrolls_instead_of_running_past_the_viewport`.
    // The inspector in the 2× narrow-inspector scenario overflows, and every
    // control the pointer can see is inside the panel; scrolling to the end
    // brings the last section and the notes box into the viewport, while what
    // scrolled off the top is culled rather than painted over the toolbar.
    {
        let mut bench = layout_bench();
        bench.with(|app| {
        let _tr = measure(app, NARROW_BREAKPOINT + 400.0, 2.0);
        app.sel = app
            .slide()
            .and_then(|s| s.elements.first().map(|e| e.id.clone()));
        let chrome = app.compute_chrome();
        let inspector = chrome.inspector;
        assert!(
            inspector.width() > 100.0,
            "inspector is docked: {inspector:?}"
        );
        assert!(
            chrome.inspector_content_h > inspector.height() + 40.0,
            "the scenario overflows the panel: content {:.0} vs viewport {:.0}",
            chrome.inspector_content_h,
            inspector.height()
        );
        let in_span = |x: f64| x >= inspector.x0 - 0.5 && x <= inspector.x1 + 0.5;
        let inside_y = |y: f64| y >= inspector.y0 - 0.01 && y <= inspector.y1 + 0.01;
        // Unscrolled: the notes box is beyond the viewport (that was the bug),
        // and nothing kept from the panel sits outside it.
        let notes = chrome.notes_box.expect("notes box");
        assert!(
            notes.y1 > inspector.y1,
            "notes start below the fold before scrolling"
        );
        for b in chrome
            .buttons
            .iter()
            .filter(|b| in_span(b.rect.center().x) && b.rect.center().y >= inspector.y0)
        {
            assert!(
                b.rect.center().y <= inspector.y1 + 0.01,
                "{} was kept although its centre is below the panel",
                app.semantic_target(&b.hit)
            );
        }

        // Hand the scroll model this frame's extent (as `draw` does) and scroll
        // to the end.
        app.inspector_scroll.set_viewport_rect(chrome.inspector_body);
        app.inspector_scroll
            .set_content_size(inspector.width() as f32, chrome.inspector_content_h as f32);
        assert!(app.inspector_scroll.max_offset_y() > 0.0);
        app.inspector_scroll.scroll_by(0.0, 1.0e6);
        let scrolled = app.compute_chrome();
        let notes = scrolled.notes_box.expect("notes box after scrolling");
        assert!(
            notes.y1 <= inspector.y1 + 0.01 && notes.y0 >= inspector.y0 - 0.01,
            "notes box {notes:?} is fully inside the panel {inspector:?} after scrolling to the end"
        );
        let arrange = scrolled
            .buttons
            .iter()
            .find(|b| matches!(b.hit, Hit::BringFront))
            .expect("ARRANGE row is reachable after scrolling");
        assert!(
            arrange.rect.y0 >= inspector.y0 - 0.01 && arrange.rect.y1 <= inspector.y1 + 0.01,
            "arrange row {:?} inside {inspector:?}",
            arrange.rect
        );
        // The FILL swatches scrolled off the top and were culled: no panel item
        // has its centre above the panel, so none can be painted over the toolbar
        // by the base-chrome pass (which paints everything `in_inspector` rejects).
        for b in scrolled
            .buttons
            .iter()
            .filter(|b| in_span(b.rect.center().x))
        {
            let toolbar_control = b.rect.center().y < scrolled.toolbar.y1;
            assert!(
                toolbar_control || SlidesApp::in_inspector(&scrolled, b.rect.center()),
                "{} at {:?} is neither toolbar chrome nor inside the panel",
                app.semantic_target(&b.hit),
                b.rect
            );
            if toolbar_control {
                assert!(
                    b.rect.y0 >= scrolled.toolbar.y0 - 0.01
                        && b.rect.y1 <= scrolled.toolbar.y1 + 0.01,
                    "a scrolled-off panel control {} at {:?} leaked into the toolbar band",
                    app.semantic_target(&b.hit),
                    b.rect
                );
            }
        }
        assert!(
            !scrolled
                .buttons
                .iter()
                .any(|b| matches!(b.hit, Hit::SetFill(_))),
            "the FILL swatches scrolled out and are no longer hit targets"
        );
        // The embedded font-size field follows the same rule as the buttons: it
        // had a slot unscrolled, and once its TYPE row leaves the panel it has
        // none — so `position_inspector_widgets` parks it instead of leaving a
        // stale rect in the toolbar band.
        assert!(
            chrome.font_input_rect.is_some(),
            "TYPE row is present unscrolled"
        );
        match scrolled.font_input_rect {
            Some(fi) => assert!(
                inside_y(fi.center().y),
                "kept field {fi:?} is inside the panel"
            ),
            None => {
                app.position_inspector_widgets(&scrolled);
                assert!(
                    !app.font_input
                        .get_rect()
                        .contains(scrolled.toolbar.center()),
                    "a culled field must not keep a rect under the toolbar"
                );
            }
        }
        for l in scrolled
            .labels
            .iter()
            .filter(|l| in_span(l.pos.x) && l.pos.y >= scrolled.toolbar.y1)
        {
            assert!(
                l.pos.y >= inspector.y0 - 0.01,
                "label '{}' leaked above the panel",
                l.text
            );
        }

        // The wheel over the panel scrolls it and is consumed; at the end of the
        // content it changes nothing and is left alone.
        app.inspector_scroll.set_offset_y(0.0);
        app.last_mouse = inspector.center();
        let mut handled = false;
        app.on_wheel(MouseScrollDelta::LineDelta(0.0, -3.0), 1.0, &mut handled);
        assert!(handled, "wheel over the inspector is consumed");
        assert!(app.inspector_scroll.offset_y() > 0.0, "and scrolls it");
        app.inspector_scroll.scroll_by(0.0, 1.0e6);
        let mut handled = false;
        app.on_wheel(MouseScrollDelta::LineDelta(0.0, -3.0), 1.0, &mut handled);
        assert!(!handled, "a wheel past the end is not consumed");
        // Over the canvas the panel never scrolls.
        app.inspector_scroll.set_offset_y(0.0);
        app.last_mouse = scrolled.slide_dest.center();
        let mut handled = false;
        app.on_wheel(MouseScrollDelta::LineDelta(0.0, -3.0), 1.0, &mut handled);
        assert!(!handled && app.inspector_scroll.offset_y() == 0.0);
        });
    }

    // Case `one_x_inspector_that_fits_neither_shifts_nor_scrolls`.
    // A 1× inspector that fits keeps every item where it was and ignores the
    // wheel — the scroll model is inert until there is overflow.
    {
        let mut bench = layout_bench();
        bench.with(|app| {
            let _tr = measure(app, NARROW_BREAKPOINT + 400.0, 1.0);
            app.sel = app
                .slide()
                .and_then(|s| s.elements.first().map(|e| e.id.clone()));
            let chrome = app.compute_chrome();
            assert!(
                chrome.inspector_content_h <= chrome.inspector.height() + 0.01,
                "1x content {:.0} fits the {:.0} panel",
                chrome.inspector_content_h,
                chrome.inspector.height()
            );
            assert!(
                chrome
                    .buttons
                    .iter()
                    .any(|b| matches!(b.hit, Hit::SetFill(_)))
                    && chrome
                        .buttons
                        .iter()
                        .any(|b| matches!(b.hit, Hit::BringFront)),
                "every section is present unscrolled"
            );
            app.inspector_scroll.set_viewport_rect(chrome.inspector);
            app.inspector_scroll.set_content_size(
                chrome.inspector.width() as f32,
                chrome.inspector_content_h as f32,
            );
            app.last_mouse = chrome.inspector.center();
            let mut handled = false;
            app.on_wheel(MouseScrollDelta::LineDelta(0.0, -3.0), 1.0, &mut handled);
            assert!(!handled && app.inspector_scroll.offset_y() == 0.0);
        });
    }
}

/// Every chrome label the app lays out resolves through the fluent bundle:
/// the measured label set carries real text, not keys, and the toolbar's
/// insert tools read as words.
#[test]
fn chrome_labels_are_fluent_resolved_text() {
    layout_bench().with(|app| {
        let _tr = measure(app, 1400.0, 1.0);
        for (text, _) in app.chrome_label_set() {
            assert!(
                !text.starts_with("slides-"),
                "chrome label set carries a raw key: {text}"
            );
            assert!(!text.is_empty());
        }
        let chrome = app.compute_chrome();
        let faces: Vec<String> = chrome
            .buttons
            .iter()
            .filter_map(|b| match &b.kind {
                ButtonKind::Command { label, .. } | ButtonKind::Tool { label, .. } => {
                    Some(label.clone())
                }
                _ => None,
            })
            .collect();
        assert!(faces.iter().any(|f| f == "Present"), "{faces:?}");
        assert!(faces.iter().any(|f| f == "Rectangle"), "{faces:?}");
        assert!(faces.iter().all(|f| !f.starts_with("slides-")), "{faces:?}");
        for l in &chrome.labels {
            assert!(
                !l.text.starts_with("slides-"),
                "label carries a raw key: {}",
                l.text
            );
        }
    });
}
