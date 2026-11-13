//! The editor's chrome at every width and text size: the slide rail and the inspector docking or folding into flaps, the inspector
//! scrolling when it overflows, and wheels and scrollbars over the rail and
//! the overview. View state only: nothing here edits the deck.
//!
//! The geometry is the editor's own: each case sizes it within one visit
//! ([`size`]), as a frame at that width does, and reads what it lays out.
#![allow(
    clippy::float_cmp,
    reason = "exact geometry and values the editor computes, not measurements"
)]

use ui::winit::keyboard::ModifiersState;
use winit::event::MouseScrollDelta;

use super::bench::{Bench, deck_of};
use super::editor_chrome::UiButton;
use super::*;
use ui::TextRole;
use ui::widget::ControlSize;

fn layout_bench() -> Bench {
    let mut s = Slide::new(Layout::TitleContent);
    s.add(Element::text("hello", 96.0, 90.0, 1088.0, 100.0, 54.0));
    Bench::open(&deck_of(vec![s]), super::bench::WINDOW)
}

fn selected_text_bench() -> Bench {
    let mut s = Slide::new(Layout::TitleContent);
    let id = s.add(Element::text("Hi", 100.0, 100.0, 400.0, 100.0, 40.0));
    let mut bench = Bench::open(&deck_of(vec![s]), super::bench::WINDOW);
    bench.with(|app| app.sel = Some(id));
    bench
}

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

fn size(app: &mut SlidesApp, w: f64, h: f64) {
    app.rect = Rect::new(0.0, 0.0, w, h);
    app.chrome_metrics = ChromeMetrics::fallback();
    (app.rail_drawer, app.inspector_drawer) = SlidesApp::new_drawers();
    app.flap_uninitialised = true;
    app.sync_flaps(Some(&Theme::dark()));
}

fn measure(app: &mut SlidesApp, w: f64, text_scale: f32) -> TextRenderer {
    size(app, w, 800.0);
    let theme = Theme::dark().with_text_scale(text_scale);
    let mut tr = TextRenderer::new();
    tr.set_text_scale(theme.text_scale);
    app.refresh_chrome_metrics(&mut tr, &theme);
    tr
}

/// Rail/inspector chrome density at 1x and 2x, narrow titles, large text and fitting pills/segments.
#[test]
#[expect(
    clippy::too_many_lines,
    reason = "cases merged from separate tests; each case stays self-contained"
)]
fn chrome_density_follows_measured_text_at_every_width() {
    {
        let mut bench = layout_bench();
        bench.with(|app| {
            let _tr = measure(app, NARROW_BREAKPOINT + 400.0, 1.0);
            let chrome = app.compute_chrome();
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

    {
        let mut bench = Bench::sample();
        bench.with(|app| {
            size(app, 1440.0, 960.0);
            app.text_scale = 2.0;
            let chrome = app.compute_chrome();
            let actions: Vec<String> = app
                .window_toolbar()
                .actions
                .into_iter()
                .map(|action| action.icon)
                .collect();
            for icon in ["plus", "panel-right", "play"] {
                assert!(
                    actions.iter().any(|action| action == icon),
                    "{icon} stays in the window bar: {actions:?}"
                );
            }
            assert!(
                chrome.slide_dest.width() > 600.0,
                "large type keeps a practical authored-slide canvas"
            );
        });
    }

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

#[test]
fn rail_docks_wide_and_overlays_narrow() {
    {
        let mut bench = layout_bench();
        bench.with(|app| {
            size(app, NARROW_BREAKPOINT + 200.0, 800.0);
            assert_eq!(app.rail_layout(), SplitLayout::Docked);
            assert!(app.rail_open(), "docked rail is open by default");
            let chrome = app.compute_chrome();
            assert_eq!(chrome.layout, SplitLayout::Docked);
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
            assert_eq!(
                app.window_toolbar().sidebar,
                Some(true),
                "the window bar's sidebar toggle shows the docked rail open"
            );
        });
    }

    {
        let mut bench = layout_bench();
        bench.with(|app| {
            size(app, NARROW_BREAKPOINT - 200.0, 800.0);
            assert_eq!(app.rail_layout(), SplitLayout::Overlay);
            assert!(!app.rail_open(), "overlay rail is closed by default");
            let chrome = app.compute_chrome();
            assert_eq!(chrome.layout, SplitLayout::Overlay);
            assert!(
                (chrome.slide_dest.x0 - app.rect.x0)
                    < (app.rail_drawer.resolved_docked_width() * 0.5),
                "narrow canvas reclaims the rail's column"
            );
            assert!(
                chrome.rail.x1 <= app.rect.x0 + 1.0,
                "closed flap is off-screen left"
            );
            assert_eq!(
                chrome.scrim.width(),
                0.0,
                "no scrim while the flap is closed"
            );
            assert_eq!(
                app.window_toolbar().sidebar,
                Some(false),
                "the window bar's sidebar toggle summons the rail"
            );
        });
    }

    {
        let mut bench = layout_bench();
        bench.with(|app| {
            size(app, NARROW_BREAKPOINT - 200.0, 800.0);
            app.rail_drawer.toggle();
            app.rail_drawer.snap(true);
            app.resolve_drawers();
            assert!(app.rail_open());
            let chrome = app.compute_chrome();
            assert_eq!(chrome.layout, SplitLayout::Overlay);
            let inset = f64::from(Theme::dark().tokens.spacing.s2);
            assert!(
                (chrome.rail.x0 - app.rect.x0 - inset).abs() < 1.0,
                "open drawer retains the shared floating inset"
            );
            assert!(
                chrome.scrim.width() > 0.0,
                "an open overlay flap dims the body"
            );
            assert_eq!(
                app.window_toolbar().sidebar,
                Some(true),
                "the window bar's sidebar toggle shows the open drawer"
            );
        });
    }

    {
        let mut bench = layout_bench();
        bench.with(|app| {
            size(app, NARROW_BREAKPOINT - 200.0, 800.0);
            assert!(!app.rail_open());
            app.toggle_rail_flap();
            assert!(app.rail_open(), "the toggle opens the collapsed flap");
            app.toggle_rail_flap();
            assert!(!app.rail_open(), "the toggle closes it again");
        });
    }
}

#[test]
fn inspector_docks_wide_and_overlays_narrow() {
    {
        let mut bench = layout_bench();
        bench.with(|app| {
            size(app, NARROW_BREAKPOINT + 200.0, 800.0);
            assert_eq!(app.inspector_layout(), SplitLayout::Docked);
            assert!(app.inspector_open(), "docked inspector is open by default");
            let chrome = app.compute_chrome();
            assert_eq!(chrome.inspector_layout, SplitLayout::Docked);
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

    {
        let mut bench = layout_bench();
        bench.with(|app| {
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
            let properties = app
                .window_toolbar()
                .actions
                .into_iter()
                .find(|action| action.icon == "panel-right")
                .expect("the window bar offers the properties' toggle");
            assert!(!properties.on, "the toggle shows the properties closed");
        });
    }

    {
        let mut bench = layout_bench();
        bench.with(|app| {
            size(app, INSPECTOR_BREAKPOINT - 120.0, 800.0);
            assert!(!app.inspector_open());
            app.toggle_inspector_flap();
            assert!(app.inspector_open(), "the toggle opens the collapsed flap");
            app.toggle_inspector_flap();
            assert!(!app.inspector_open(), "the toggle closes it again");
        });
    }
}

#[test]
fn compact_editor_drawers_and_breakpoint_reflow() {
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
            let toolbar = app.window_toolbar();
            assert_eq!(
                toolbar.sidebar,
                Some(false),
                "the slide list has its toggle"
            );
            assert!(
                toolbar
                    .actions
                    .iter()
                    .any(|action| action.icon == "panel-right"),
                "the properties have their toggle"
            );
        });
    }

    {
        let mut bench = layout_bench();
        bench.with(|app| {
            size(app, 820.0, 800.0);
            app.toggle_rail_flap();
            assert!(app.rail_open());
            assert!(!app.inspector_open());
            app.toggle_inspector_flap();
            assert!(app.inspector_open(), "properties drawer opens");
            assert!(
                !app.rail_open(),
                "opening properties closes the slide drawer immediately"
            );
        });
    }

    {
        let mut bench = layout_bench();
        bench.with(|app| {
            size(app, NARROW_BREAKPOINT - 200.0, 800.0);
            // User opens the flap while narrow.
            app.rail_drawer.resolve(app.rect);
            app.toggle_rail_flap();
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

#[test]
fn wheel_and_scrollbar_scroll_rail_and_overview() {
    {
        let mut bench = Bench::open(&tall_deck(16), super::bench::WINDOW);
        bench.with(|app| {
            size(app, 1280.0, 560.0);
            // The shared rail is the scroll authority: sync it to this frame, then
            // read the overflow it computed.
            app.sync_page_rail();
            let max_scroll = app.page_sidebar.max_scroll();
            assert!(max_scroll > 0.0, "a 16-slide rail overflows a 560px window");

            let rail = app.rail_rect();
            app.last_mouse = Point::new(rail.center().x, rail.center().y);
            let mut handled = false;
            app.on_wheel(
                MouseScrollDelta::LineDelta(0.0, -3.0),
                1.0,
                ModifiersState::empty(),
                &mut handled,
            );
            assert!(handled, "wheel over the rail is handled");
            assert!(app.rail_scroll > 0.0, "wheel over the rail scrolls it");

            for _ in 0..40 {
                let mut h = false;
                app.on_wheel(
                    MouseScrollDelta::LineDelta(0.0, -20.0),
                    1.0,
                    ModifiersState::empty(),
                    &mut h,
                );
            }
            assert!(
                (app.rail_scroll - max_scroll).abs() < 1.0,
                "rail scroll clamps to max"
            );

            // Pointer over the canvas → the rail does not move.
            app.rail_scroll = 0.0;
            app.last_mouse = Point::new(app.rect.x1 - 80.0, app.rect.center().y);
            let mut h2 = false;
            app.on_wheel(
                MouseScrollDelta::LineDelta(0.0, -3.0),
                1.0,
                ModifiersState::empty(),
                &mut h2,
            );
            assert_eq!(
                app.rail_scroll, 0.0,
                "wheel over the canvas leaves the rail alone"
            );
        });
    }

    {
        let mut bench = Bench::open(&tall_deck(12), super::bench::WINDOW);
        bench.with(|app| {
            app.bootstrapped = true;
            app.mode = Mode::Overview;
            app.rect = Rect::new(0.0, 0.0, 800.0, 600.0);
            let (_, _, max_scroll) = app.compute_overview();
            assert!(
                max_scroll > 0.0,
                "the 12-slide deck overflows the 800x600 overview"
            );

            let mut handled = false;
            app.on_wheel(
                MouseScrollDelta::LineDelta(0.0, -3.0),
                1.0,
                ModifiersState::empty(),
                &mut handled,
            );
            assert!(
                handled && app.overview.scroll() > 0.0,
                "wheel scrolls the overview"
            );

            for _ in 0..60 {
                let mut h = false;
                app.on_wheel(
                    MouseScrollDelta::LineDelta(0.0, -20.0),
                    1.0,
                    ModifiersState::empty(),
                    &mut h,
                );
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

#[test]
fn the_slide_zooms_and_pans_without_losing_its_edges() {
    let mut bench = layout_bench();
    bench.with(|app| {
        let _tr = measure(app, f64::from(super::bench::WINDOW.0), 1.0);
        let fitted = app.compute_chrome();
        assert!(app.slide_zoom.is_fit(), "the editor opens fitted");
        let pane = fitted.slide_dest;
        assert!(
            !app.pan_slide(0.0, 40.0),
            "a fitted slide has nowhere to move, so the wheel is not consumed"
        );

        app.zoom_slide(1.0);
        let zoomed = app.compute_chrome();
        assert!(!app.slide_zoom.is_fit(), "a step in pins the zoom");
        assert!(
            zoomed.scale > fitted.scale,
            "the slide is drawn larger: {:.3} against {:.3}",
            zoomed.scale,
            fitted.scale
        );
        assert!(zoomed.slide_dest.width() >= pane.width() - 0.5 || zoomed.scale > 1.0);

        // At a zoom the pane cannot hold, the wheel moves it, and no further
        // than its own overflow.
        app.zoom_slide_to(Some(2.0));
        assert!(app.pan_slide(0.0, 30.0), "a zoomed slide moves");
        assert!(app.slide_pan.1 > 0.0);
        assert!(app.pan_slide(0.0, 1.0e6), "the far pan runs to the edge");
        assert!(!app.pan_slide(0.0, 1.0e6), "at the edge it stops");
        let (avail_w, avail_h) = app.slide_pane.get();
        let SlideZoom::Pinned(scale) = app.slide_zoom else {
            panic!("a pinned zoom");
        };
        let reach_y = ((SLIDE_H * scale - avail_h) / 2.0).max(0.0);
        assert!(
            (app.slide_pan.1 - reach_y).abs() < 0.01,
            "the slide stops with its edge at the pane's: {:.1} against {:.1}",
            app.slide_pan.1,
            reach_y
        );
        let _ = avail_w;

        app.zoom_slide_to_fit();
        assert!(app.slide_zoom.is_fit());
        assert_eq!(app.slide_pan, (0.0, 0.0));
        assert_eq!(app.compute_chrome().scale, fitted.scale);
    });
}

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
    // scrolled off the top is culled rather than painted over the heading.
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
        for b in scrolled
            .buttons
            .iter()
            .filter(|b| in_span(b.rect.center().x))
        {
            assert!(
                SlidesApp::in_inspector(&scrolled, b.rect.center()),
                "{} at {:?} is outside the panel",
                app.semantic_target(&b.hit),
                b.rect
            );
        }
        assert!(
            !scrolled
                .buttons
                .iter()
                .any(|b| matches!(b.hit, Hit::SetFill(_))),
            "the FILL swatches scrolled out and are no longer hit targets"
        );
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
                let heading = Rect::new(
                    inspector.x0,
                    inspector.y0,
                    inspector.x1,
                    scrolled.inspector_body.y0,
                );
                assert!(
                    !app.font_input.get_rect().contains(heading.center()),
                    "a culled field must not keep a rect over the panel's heading"
                );
            }
        }
        for l in scrolled
            .labels
            .iter()
            .filter(|l| in_span(l.pos.x))
        {
            assert!(
                l.pos.y >= inspector.y0 - 0.01,
                "label '{}' leaked above the panel",
                l.text
            );
        }

        app.inspector_scroll.set_offset_y(0.0);
        app.last_mouse = inspector.center();
        let mut handled = false;
        app.on_wheel(MouseScrollDelta::LineDelta(0.0, -3.0), 1.0, ModifiersState::empty(), &mut handled);
        assert!(handled, "wheel over the inspector is consumed");
        assert!(app.inspector_scroll.offset_y() > 0.0, "and scrolls it");
        app.inspector_scroll.scroll_by(0.0, 1.0e6);
        let mut handled = false;
        app.on_wheel(MouseScrollDelta::LineDelta(0.0, -3.0), 1.0, ModifiersState::empty(), &mut handled);
        assert!(!handled, "a wheel past the end is not consumed");
        // Over the canvas the panel never scrolls: the wheel there is the
        // slide's own (it moves a zoomed slide, and does nothing at fit).
        app.inspector_scroll.set_offset_y(0.0);
        app.last_mouse = scrolled.slide_dest.center();
        let mut handled = false;
        app.on_wheel(MouseScrollDelta::LineDelta(0.0, -3.0), 1.0, ModifiersState::empty(), &mut handled);
        assert_eq!(app.inspector_scroll.offset_y(), 0.0);
        assert!(!handled, "a fitted slide has nowhere to move");
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
            app.on_wheel(
                MouseScrollDelta::LineDelta(0.0, -3.0),
                1.0,
                ModifiersState::empty(),
                &mut handled,
            );
            assert!(!handled && app.inspector_scroll.offset_y() == 0.0);
        });
    }
}

/// Every chrome label the app lays out resolves through the fluent bundle:
/// the measured label set carries real text, not keys, and the window bar's
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
                ButtonKind::Command { label, .. } => Some(label.clone()),
                _ => None,
            })
            .collect();
        assert!(faces.iter().all(|f| !f.starts_with("slides-")), "{faces:?}");
        let insert = app
            .window_toolbar()
            .actions
            .into_iter()
            .find(|action| action.name == "Insert")
            .expect("the window bar's Insert menu");
        let tools: Vec<String> = insert.items.into_iter().map(|item| item.name).collect();
        assert_eq!(tools, ["Text", "Rectangle", "Ellipse", "Line", "Image"]);
        for l in &chrome.labels {
            assert!(
                !l.text.starts_with("slides-"),
                "label carries a raw key: {}",
                l.text
            );
        }
    });
}

#[test]
fn a_compact_editor_shows_the_notes_under_the_slide() {
    let mut bench = layout_bench();
    bench.with(|app| {
        size(app, 420.0, 800.0);
        assert!(!app.inspector_open());
        let chrome = app.compute_chrome();
        let notes = chrome.notes_box.expect("the notes show under the slide");
        assert!(
            notes.y0 >= chrome.slide_dest.y1,
            "{notes:?} under {:?}",
            chrome.slide_dest
        );
        assert!(notes.y1 <= app.rect.y1 && notes.x0 >= app.rect.x0 && notes.x1 <= app.rect.x1);
        assert!(
            notes.height() >= 4.0 * app.chrome_metrics.lh_body,
            "the notes have room for a few lines: {notes:?}"
        );
        assert!(
            chrome.slide_dest.y0 - app.rect.y0 <= 2.0 * app.chrome_metrics.s6,
            "the slide sits at the top of the column: {:?}",
            chrome.slide_dest
        );
        assert!(
            chrome
                .labels
                .iter()
                .any(|l| l.text == tr("slides-speaker-notes")
                    && l.pos.y >= chrome.slide_dest.y1
                    && l.pos.y <= notes.y0),
            "the notes have their caption"
        );
        app.apply_hit(Hit::EditNotes);
        assert!(app.editing_notes, "the notes under the slide edit");
        app.editing_notes = false;

        app.toggle_inspector_flap();
        app.inspector_drawer.snap(true);
        app.resolve_drawers();
        let chrome = app.compute_chrome();
        let notes = chrome.notes_box.expect("the properties hold the notes");
        assert!(chrome.inspector.contains(notes.center()));
    });

    let mut bench = layout_bench();
    bench.with(|app| {
        size(app, 820.0, 500.0);
        assert!(!app.inspector_open());
        assert_eq!(app.compute_chrome().notes_box, None);
    });
}

#[test]
fn the_fill_and_stroke_offer_no_colour_as_the_none_swatch() {
    let unfilled = |hit: &Hit, fill: bool| match hit {
        Hit::SetFill(role) => fill && role == "none",
        Hit::SetStroke(role) => !fill && role == "none",
        _ => false,
    };
    for background in Background::ALL {
        let mut s = Slide::new(Layout::TitleContent);
        s.background = background;
        let shape = s.add(Element::shape(
            ElementKind::Rect,
            100.0,
            100.0,
            200.0,
            100.0,
        ));
        let text = s.add(Element::text("Hi", 100.0, 300.0, 400.0, 100.0, 40.0));
        let mut bench = Bench::open(&deck_of(vec![s]), super::bench::WINDOW);
        bench.with(|app| {
            let pal = app.palette();
            app.sel = Some(shape.clone());
            let chrome = app.compute_chrome();
            for fill in [true, false] {
                let button = chrome
                    .buttons
                    .iter()
                    .find(|b| unfilled(&b.hit, fill))
                    .unwrap_or_else(|| panic!("no colour is offered on {background:?}"));
                assert!(matches!(&button.kind, ButtonKind::Swatch { role, .. } if role == "none"));
                assert!(app.swatch_face(&pal, button.rect, "none").is_none());
            }
            assert!(!app.swatch_face(&pal, Rect::ZERO, "text").is_none());

            app.sel = Some(text.clone());
            let chrome = app.compute_chrome();
            assert!(
                !chrome.buttons.iter().any(|b| unfilled(&b.hit, true)),
                "text keeps its colour"
            );
        });
    }
}

#[test]
fn the_format_toggles_fill_their_row() {
    for width in [1280.0, 1100.0, 900.0] {
        let mut bench = selected_text_bench();
        bench.with(|app| {
            size(app, width, 800.0);
            if !app.inspector_open() {
                app.toggle_inspector_flap();
            }
            app.inspector_drawer.snap(true);
            app.resolve_drawers();
            let chrome = app.compute_chrome();
            let field = chrome.font_input_rect.expect("the size field shows");
            let toggles: Vec<Rect> = chrome
                .buttons
                .iter()
                .filter(|b| {
                    matches!(
                        b.hit,
                        Hit::ToggleBold | Hit::ToggleItalic | Hit::ToggleList(_)
                    )
                })
                .map(|b| b.rect)
                .collect();
            assert_eq!(toggles.len(), 4, "at {width}");
            let row_end = chrome
                .buttons
                .iter()
                .filter(|b| matches!(b.hit, Hit::SetAlign(_)))
                .map(|b| b.rect.x1)
                .fold(f64::MIN, f64::max);
            let last = toggles[3];
            assert!(
                (last.x1 - row_end).abs() < 0.5,
                "at {width}: {last:?} ends at {row_end}"
            );
            if toggles[0].y0 > field.y0 {
                assert!(
                    (toggles[0].x0 - field.x0).abs() < 0.5,
                    "at {width}: the wrapped row starts with the field: {toggles:?}"
                );
            }
        });
    }
}
