use super::*;
use automerge::transaction::Transactable;
use std::fmt::Write as _;
use ui::headless::Framed;
use ui::permissions::PermissionContext;

/// Minimal, valid 2×3 RGB PNG shared by image layout, accessibility, portal,
/// and inspector tests.
const TEST_PNG: &[u8] = &[
    137, 80, 78, 71, 13, 10, 26, 10, 0, 0, 0, 13, 73, 72, 68, 82, 0, 0, 0, 2, 0, 0, 0, 3, 8, 2, 0,
    0, 0, 54, 136, 73, 214, 0, 0, 0, 17, 73, 68, 65, 84, 120, 156, 99, 248, 207, 192, 240, 159, 1,
    140, 17, 20, 0, 68, 208, 5, 251, 176, 88, 73, 172, 0, 0, 0, 0, 73, 69, 78, 68, 174, 66, 96,
    130,
];

/// A cropped image source expands behind its stable frame, a flip mirrors around the frame centre,
/// and a comment thread keeps its reply controls inside the panel.
#[test]
fn images_and_comment_threads_stay_inside_their_frames() {
    // Phase: cropped_image_source_expands_behind_the_stable_frame.
    {
        let frame = Rect::new(10.0, 20.0, 110.0, 70.0);
        let source = cropped_image_source(frame, (0.1, 0.2, 0.3, 0.1));

        assert!((source.x0 - (-6.666_666_666_666_668)).abs() < 1e-9);
        assert!((source.y0 - 5.714_285_714_285_713).abs() < 1e-9);
        assert!((source.x1 - 160.0).abs() < 1e-9);
        assert!((source.y1 - 77.142_857_142_857_14).abs() < 1e-9);
        assert_eq!(cropped_image_source(frame, (0.0, 0.0, 0.0, 0.0)), frame);
    }

    // Phase: image_flip_transform_mirrors_around_the_frame_center.
    {
        let frame = Rect::new(10.0, 20.0, 110.0, 70.0);
        let horizontal = image_paint_transform(
            Affine::IDENTITY,
            frame,
            ImagePaintOptions {
                flip_horizontal: true,
                ..ImagePaintOptions::default()
            },
        );
        let vertical = image_paint_transform(
            Affine::IDENTITY,
            frame,
            ImagePaintOptions {
                flip_vertical: true,
                ..ImagePaintOptions::default()
            },
        );

        assert_eq!(
            horizontal * Point::new(frame.x0, frame.y0),
            Point::new(frame.x1, frame.y0)
        );
        assert_eq!(
            vertical * Point::new(frame.x0, frame.y0),
            Point::new(frame.x0, frame.y1)
        );
        let center = Point::new(frame.center().x, frame.center().y);
        assert_eq!(horizontal * center, center);
        assert_eq!(vertical * center, center);
    }

    // Phase: comment_thread_layout_keeps_reply_controls_inside_the_panel.
    {
        let host = Rect::new(0.0, 0.0, 300.0, 480.0);
        let layout = CommentThreadLayout::for_host(host, 1.0);

        assert_eq!(layout.panel, Rect::new(12.0, 82.0, 288.0, 442.0));
        for control in [
            layout.reply_field,
            layout.cancel_button,
            layout.submit_button,
        ] {
            assert!(layout.panel.contains(Point::new(control.x0, control.y0)));
            assert!(layout.panel.contains(Point::new(control.x1, control.y1)));
        }
        assert!(layout.cancel_button.x1 < layout.submit_button.x0);
        assert!(layout.reply_field.y1 < layout.submit_button.y0);
    }
}

/// The native input snapshot reads the real caret text without mutation; plain body edits retain
/// untouched views, fall back for other changes and refresh a shifted virtual window; native document
/// edits carry writer provenance.
#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one scenario test: each phase is a former standalone test kept verbatim as a scoped block"
)]
fn native_edits_keep_views_provenance_and_the_real_caret_text() {
    // Phase: native_input_snapshot_reads_real_caret_text_without_mutation.
    {
        let mut state = ui::State::new();
        let mut wp = WordProcessorWidget::new();
        wp.sync(&mut state.doc, &state.root);
        wp.model_mut().insert_str(&mut state.doc, "fixture é");
        wp.scroll_y = 144.0;
        wp.max_scroll = 2000.0;
        let heads = state.doc.get_heads();
        let caret = wp.model().caret().clone();
        let dirty = wp.lifecycle.dirty;
        let snapshot = wp.native_input_snapshot(&state.doc, &state.root).unwrap();
        assert_eq!(snapshot["offset_y"], 144.0);
        assert_eq!(snapshot["max_scroll"], 2000.0);
        assert_eq!(snapshot["caret_text"], "fixture é");
        assert_eq!(snapshot["caret_offset"], 9);
        assert_eq!(snapshot["caret_block"], caret.block);
        assert_eq!(snapshot["caret_region"], "body");
        assert_eq!(snapshot["has_selection"], false);
        assert_eq!(state.doc.get_heads(), heads);
        assert_eq!(wp.model().caret(), &caret);
        assert_eq!(wp.lifecycle.dirty, dirty);
        assert_eq!(wp.scroll_y, 144.0);
        wp.inner.region = Region::Header;
        let header = wp.native_input_snapshot(&state.doc, &state.root).unwrap();
        assert_eq!(header["caret_region"], "header");
        assert!(
            header["caret_text"].is_null(),
            "running regions must not materialize text for diagnostics"
        );
    }

    // Phase: plain_body_edits_retain_untouched_views_and_fall_back_for_other_changes.
    {
        let mut state = ui::State::new();
        let mut wp = WordProcessorWidget::new();
        wp.sync(&mut state.doc, &state.root);
        wp.inner.insert_str(&mut state.doc, "Untouched paragraph");
        wp.inner.enter(&mut state.doc);
        wp.inner.insert_str(&mut state.doc, "Edited paragraph");
        wp.refresh_blocks(&mut state.doc);
        let untouched = wp.blocks[0].text.as_ptr();
        for (key, text) in [
            (Key::Character("é".into()), Some("é")),
            (Key::Named(NamedKey::Backspace), None),
        ] {
            let mut handled = false;
            wp.handle_key_press(&mut state.doc, &key, text, false, false, &mut handled);
            assert!(handled);
            assert_eq!(
                wp.blocks[0].text.as_ptr(),
                untouched,
                "text-only edits must retain unrelated block views"
            );
            let c = wp.inner.containers.as_ref().unwrap();
            assert_eq!(
                wp.blocks,
                tilo_doc::model::materialize(&state.doc, c).blocks
            );
        }
        // A mutation outside the known local edit invalidates the optimization.
        let first = wp.blocks[0].id.clone();
        let caret = wp.inner.caret().clone();
        wp.inner.set_caret(Caret {
            block: first,
            offset: 0,
        });
        wp.inner.insert_str(&mut state.doc, "External ");
        wp.inner.set_caret(caret);
        wp.handle_key_press(
            &mut state.doc,
            &Key::Character("x".into()),
            Some("x"),
            false,
            false,
            &mut false,
        );
        assert!(wp.blocks[0].text.starts_with("External "));
        wp.handle_key_press(
            &mut state.doc,
            &Key::Named(NamedKey::Enter),
            None,
            false,
            false,
            &mut false,
        );
        let c = wp.inner.containers.as_ref().unwrap();
        assert_eq!(
            wp.blocks,
            tilo_doc::model::materialize(&state.doc, c).blocks
        );
        assert_eq!(wp.blocks.len(), 3);
        wp.handle_key_press(
            &mut state.doc,
            &Key::Named(NamedKey::Backspace),
            None,
            false,
            false,
            &mut false,
        );
        assert_eq!(wp.blocks.len(), 2, "structural backspace merges blocks");
        wp.inner.set_caret(Caret {
            block: wp.blocks[0].id.clone(),
            offset: 0,
        });
        wp.inner.set_anchor();
        wp.inner.set_caret(Caret {
            block: wp.blocks[1].id.clone(),
            offset: 2,
        });
        wp.handle_key_press(
            &mut state.doc,
            &Key::Character("x".into()),
            Some("x"),
            false,
            false,
            &mut false,
        );
        let c = wp.inner.containers.as_ref().unwrap();
        assert_eq!(
            wp.blocks,
            tilo_doc::model::materialize(&state.doc, c).blocks
        );
        assert_eq!(
            wp.blocks.len(),
            1,
            "cross-block replacement uses full refresh"
        );
    }

    // Phase: plain_body_edit_refreshes_a_shifted_virtual_window.
    {
        let mut state = large_body();
        let mut wp = WordProcessorWidget::new();
        wp.sync(&mut state.doc, &state.root);
        let old_start = wp.body_window_start;
        // The caret is still cached, but the requested scroll window has moved.
        assert!(wp.blocks.iter().any(|b| b.id == wp.inner.caret.block));
        wp.scroll_y = num::f64_of_usize(VIEWPORT_BLOCK_WINDOW * 2) * ESTIMATED_BLOCK_EXTENT;
        wp.handle_key_press(
            &mut state.doc,
            &Key::Character("x".into()),
            Some("x"),
            false,
            false,
            &mut false,
        );
        assert_ne!(wp.body_window_start, old_start);
        let (start, end) = wp.body_window_bounds();
        let c = wp.inner.containers.as_ref().unwrap();
        assert_eq!(
            wp.blocks,
            tilo_doc::model::materialize_blocks(
                &state.doc,
                c,
                &wp.body_order
                    .ids(start..end)
                    .map(str::to_owned)
                    .collect::<Vec<_>>()
            )
        );
    }

    // Phase: native_document_edit_has_writer_provenance.
    {
        let mut wp = WordProcessorWidget::new();
        wp.lifecycle.needs_commit = true;

        let context = ui::undo::take_commit_context(&mut wp).expect("native edit provenance");
        assert_eq!(context.action_label, "Edit document");
        assert_eq!(
            context.app.as_ref().map(|app| app.id.as_str()),
            Some("org.tilo.Writer")
        );
        assert_eq!(
            context
                .resource
                .as_ref()
                .map(|resource| resource.kind.as_str()),
            Some("tilo.rich-text")
        );
    }
}

/// Resource navigation uses stable ranges and refuses stale ones, and each open file is its own record.
#[test]
fn resources_navigate_by_stable_ranges_and_files_are_records() {
    // Phase: resource_navigation_uses_stable_ranges_and_refuses_stale_ones.
    {
        let mut state = ui::State::new();
        let mut wp = WordProcessorWidget::new();
        wp.sync(&mut state.doc, &state.root);
        wp.model_mut().insert_str(&mut state.doc, "findable text");
        wp.sync(&mut state.doc, &state.root);
        let block = wp.model().caret().block.clone();

        wp.navigate_to_range(&state.doc, &block, 0, 8)
            .expect("canonical range exists");
        assert_eq!(wp.model().caret().offset, 8);
        assert!(wp.model().has_selection());
        let before = wp.model().caret().clone();
        assert!(wp.navigate_to_range(&state.doc, &block, 0, 99).is_err());
        assert_eq!(
            wp.model().caret(),
            &before,
            "stale navigation changes nothing"
        );
    }

    // Phase: each_open_file_is_its_own_record.
    // Two windows on two files are two records, each identified by its file's
    // path — what the broker routes on, and what stops two people editing
    // unrelated documents in one room from merging.
    {
        let mut report = WordProcessorWidget::new();
        assert!(
            ui::record::capability::record_intent(&report).is_none(),
            "an untitled document names no record"
        );
        let report_path = std::env::temp_dir().join("report.odt");
        let minutes_path = std::env::temp_dir().join("minutes.odt");
        report.set_file(report_path.clone(), WpFormat::Markdown);

        let mut minutes = WordProcessorWidget::new();
        minutes.set_file(minutes_path, WpFormat::Markdown);

        let a =
            ui::record::capability::record_intent(&report).expect("an open file names its record");
        let b = ui::record::capability::record_intent(&minutes).expect("and so does the other");
        assert_eq!(a.id, ui::RecordId::for_file(&report_path));
        assert_eq!(a.kind, ui::RecordKind::new("document"));
        assert_ne!(a.id, b.id, "two files are never one record");
        assert_eq!(
            a.id.file_path().as_deref(),
            Some(report_path.as_path()),
            "a file record's identity is the path itself, not a handle to it"
        );
    }
}

/// Mount `wp` over `state` in a [`FrameHarness`](ui::headless::FrameHarness)
/// and run its first frame at `w`×`h` under the dark theme.
pub(super) fn mount(
    state: ui::State,
    wp: WordProcessorWidget,
    w: u32,
    h: u32,
) -> Framed<WordProcessorWidget> {
    mount_themed(state, wp, w, h, &Theme::dark())
}

/// Like [`mount`] but runs the first frame under `theme`.
pub(super) fn mount_themed(
    state: ui::State,
    wp: WordProcessorWidget,
    w: u32,
    h: u32,
    theme: &Theme,
) -> Framed<WordProcessorWidget> {
    Framed::new(state, wp, (w, h), theme.clone())
}

/// Run one frame at `w`×`h` under the dark theme that recomposes the whole
/// layout tree, and hand back the widget and its document for what follows.
pub(super) fn pipeline_draw(
    framed: &mut Framed<WordProcessorWidget>,
    w: u32,
    h: u32,
) -> (&mut WordProcessorWidget, &mut ui::State) {
    pipeline_draw_themed(framed, w, h, &Theme::dark());
    Framed::split(framed)
}

/// Like [`pipeline_draw`] but renders under an explicit [`Theme`] so tests
/// can exercise the real five-pass draw path under light/dark/high-contrast
/// palettes. Returns the widget's resolved chrome palette for inspection.
pub(super) fn pipeline_draw_themed(
    framed: &mut Framed<WordProcessorWidget>,
    w: u32,
    h: u32,
    theme: &Theme,
) -> ChromeColors {
    Framed::harness_mut(framed)
        .relayout_themed((w, h), theme.clone())
        .expect("the frame settles");
    // `draw` resolves the chrome palette from `theme` into `wp.chrome`.
    framed.chrome
}

/// Build a small populated doc (heading + paragraph) so a draw exercises the
/// toolbar, panels, page, body ink, selection, and bars in one pass.
fn populated() -> (ui::State, WordProcessorWidget) {
    let mut state = ui::State::new();
    let mut wp = WordProcessorWidget::new();
    wp.sync(&mut state.doc, &state.root);
    wp.model_mut().insert_str(&mut state.doc, "Title");
    wp.model_mut().enter(&mut state.doc);
    wp.model_mut().insert_str(&mut state.doc, "Body text here");
    // Open the find bar + outline pane so their chrome is drawn too.
    wp.find.open(None, &wp.blocks);
    wp.outline.toggle();
    (state, wp)
}

/// Chrome colours are sourced from theme tokens and change with the theme, paper stays light for
/// readable ink in every theme, and the draw path reads the theme without panicking in any mode.
#[test]
fn chrome_follows_the_theme_while_paper_stays_readable() {
    // Phase: chrome_colors_are_sourced_from_theme_tokens.
    // `ChromeColors::for_theme` is the single production mapping `draw` uses.
    // Assert every chrome slot is *sourced from the theme token* (not a literal)
    // and that swapping the theme actually changes the sampled chrome surfaces —
    // proving the chrome follows the OS palette.
    {
        for theme in [Theme::dark(), Theme::light(), Theme::high_contrast()] {
            let cc = ChromeColors::for_theme(&theme);
            let c = &theme.tokens.colors;
            assert_eq!(cc.canvas, c.surfaces.base, "canvas = surfaces.base");
            assert_eq!(cc.bar, c.surfaces.raised, "bar = surfaces.raised");
            // Edges are derived against the bar they sit on (HIG §9.5), so
            // they register on every preset — the declared `border_standard`
            // is three shades from the dark `raised` and vanished there.
            assert_eq!(
                cc.border,
                ui::surface::edge_on(c.surfaces.raised),
                "border = edge_on(bar)"
            );
            assert_eq!(cc.chip, c.surfaces.raised, "chip face = surfaces.raised");
            for (name, edge, ground) in [
                ("border", cc.border, cc.bar),
                ("chip edge", ui::surface::edge_on(cc.chip), cc.chip),
            ] {
                let moved = ui::surface::edge_delta(edge, ground);
                assert!(
                    moved >= ui::tokens::colors::HAIRLINE_MIN_DELTA,
                    "{name} on {} moved luminance only {moved:.4}",
                    theme.name
                );
            }
            // The scrollbar thumb is a control, not a hairline: it clears the
            // non-text contrast floor against the desk on every preset.
            let thumb_contrast = ui::a11y::contrast_ratio(cc.scrollbar, cc.canvas);
            assert!(
                thumb_contrast >= ui::tokens::colors::NON_TEXT_MIN_CONTRAST,
                "scrollbar thumb on {} measures {thumb_contrast:.2}:1",
                theme.name
            );
            assert_eq!(cc.text, c.text_primary, "text = text_primary");
            assert_eq!(cc.text_dim, c.text_dim, "text_dim = text_dim");
            assert_eq!(cc.accent, theme.accent_primary, "accent = accent_primary");
        }
    }

    // Phase: swapping_theme_changes_sampled_chrome.
    {
        let dark = ChromeColors::for_theme(&Theme::dark());
        let light = ChromeColors::for_theme(&Theme::light());
        // The desk/canvas and bar surfaces must differ between light and dark —
        // if the chrome were hard-coded these would be identical.
        assert_ne!(
            (dark.canvas.r(), dark.canvas.g(), dark.canvas.b()),
            (light.canvas.r(), light.canvas.g(), light.canvas.b()),
            "canvas tracks the theme",
        );
        assert_ne!(
            (dark.text.r(), dark.text.g(), dark.text.b()),
            (light.text.r(), light.text.g(), light.text.b()),
            "chrome text tracks the theme",
        );
    }

    // Phase: paper_stays_light_for_readable_ink_in_every_theme.
    // The paper sheet is deliberately a *light* tone in every theme so the
    // dark-authored document ink stays readable (see the `PAPER_*` convention).
    {
        for theme in [Theme::dark(), Theme::light(), Theme::high_contrast()] {
            let cc = ChromeColors::for_theme(&theme);
            assert!(
                luminance(cc.paper) >= 0.85,
                "paper must stay light (got luminance {:.2}) so ink contrasts",
                luminance(cc.paper),
            );
            // The default paper ink is dark — confirm it actually contrasts.
            assert!(
                luminance(cc.paper) - luminance(PAPER_INK) > 0.5,
                "paper/ink contrast must be ample",
            );
        }
    }

    // Phase: draw_path_reads_theme_and_does_not_panic_in_any_mode.
    // The whole five-pass draw path must run (and stash the themed chrome on the
    // widget) without panicking under light, dark, AND high-contrast palettes.
    {
        for theme in [Theme::dark(), Theme::light(), Theme::high_contrast()] {
            let (state, wp) = populated();
            let cc = mount_themed(state, wp, 1100, 760, &theme).chrome;
            // The widget's live chrome must equal the theme's mapping — i.e. the
            // production `draw` resolved it from the passed-in `theme`.
            assert_eq!(cc.canvas, theme.tokens.colors.surfaces.base);
            assert_eq!(cc.bar, theme.tokens.colors.surfaces.raised);
            assert_eq!(cc.accent, theme.accent_primary);
        }
    }
}

/// Draw lays out all blocks, a large body uses a bounded viewport cache while find stays complete, and
/// scrolling retains overlap but discards transient block previews.
#[test]
fn drawing_lays_out_every_block_within_a_bounded_viewport() {
    // Phase: draw_lays_out_all_blocks.
    {
        let mut state = ui::State::new();
        let mut wp = WordProcessorWidget::new();
        wp.sync(&mut state.doc, &state.root);
        wp.model_mut().insert_str(&mut state.doc, "Hello");
        wp.model_mut().enter(&mut state.doc);
        wp.model_mut().insert_str(&mut state.doc, "World");
        let mut framed = mount(state, wp, 1000, 700);
        let (wp, _) = Framed::split(&mut framed);
        assert_eq!(wp.laid.len(), 2, "draw laid out both paragraphs");
    }

    // Phase: large_body_uses_a_bounded_viewport_cache_while_find_remains_complete.
    {
        let state = large_body();
        let wp = WordProcessorWidget::new();
        let mut framed = mount(state, wp, 1000, 700);
        let (wp, state) = Framed::split(&mut framed);

        assert_eq!(wp.body_order.len(), VIEWPORT_BLOCK_WINDOW * 4);
        assert!(
            wp.blocks.len() <= VIEWPORT_BLOCK_WINDOW,
            "offscreen rich block views stay bounded: {}",
            wp.blocks.len()
        );
        assert!(
            wp.layout_cache.len() <= VIEWPORT_BLOCK_WINDOW,
            "offscreen shaped layouts stay bounded: {}",
            wp.layout_cache.len()
        );
        // Find gets a short-lived complete view rather than silently searching
        // only the visible cache. Its result can target the final imported block.
        let all = wp.search_blocks(&state.doc);
        assert_eq!(all.len(), VIEWPORT_BLOCK_WINDOW * 4);
        assert!(all.last().is_some_and(|b| b.text.ends_with("767")));
        drop(all);

        wp.scroll_y = num::f64_of_usize(VIEWPORT_BLOCK_WINDOW * 3) * ESTIMATED_BLOCK_EXTENT;
        let (wp, _) = pipeline_draw(&mut framed, 1000, 700);
        assert!(wp.body_window_start > 0, "scrolling loads a later window");
        assert!(wp.blocks.len() <= VIEWPORT_BLOCK_WINDOW);
        assert!(wp.layout_cache.len() <= VIEWPORT_BLOCK_WINDOW);
    }

    // Phase: scrolling_retains_overlap_but_discards_transient_block_previews.
    {
        let mut state = large_body();
        let mut wp = WordProcessorWidget::new();
        wp.sync(&mut state.doc, &state.root);
        let retained_id = wp.blocks[100].id.clone();
        let retained_text = wp.blocks[100].text.as_ptr();
        for focus in [60, 80, 70, 48] {
            wp.scroll_y = num::f64_of_usize(focus) * ESTIMATED_BLOCK_EXTENT;
            wp.refresh_blocks(&mut state.doc);
            assert_eq!(
                wp.blocks
                    .iter()
                    .find(|b| b.id == retained_id)
                    .unwrap()
                    .text
                    .as_ptr(),
                retained_text,
                "overlapping unchanged views should be moved, not rebuilt"
            );
            let (start, end) = wp.body_window_bounds();
            assert_eq!(
                wp.blocks,
                tilo_doc::model::materialize_blocks(
                    &state.doc,
                    wp.inner.containers.as_ref().unwrap(),
                    &wp.body_order
                        .ids(start..end)
                        .map(str::to_owned)
                        .collect::<Vec<_>>()
                )
            );
        }
        wp.inner.set_caret(Caret {
            block: retained_id,
            offset: 0,
        });
        wp.set_caret_block_indent_view(123.0);
        wp.set_caret_block_first_line_view(42.0);
        wp.scroll_y = 70.0 * ESTIMATED_BLOCK_EXTENT;
        wp.refresh_blocks(&mut state.doc);
        let (start, end) = wp.body_window_bounds();
        assert_eq!(
            wp.blocks,
            tilo_doc::model::materialize_blocks(
                &state.doc,
                wp.inner.containers.as_ref().unwrap(),
                &wp.body_order
                    .ids(start..end)
                    .map(str::to_owned)
                    .collect::<Vec<_>>()
            ),
            "scrolling must discard uncommitted previews even without a live drag"
        );
    }
}

#[test]
fn deep_scrolling_keeps_visible_text_loaded() {
    let state = large_body();
    let mut wp = WordProcessorWidget::new();
    wp.view.auto_fit_page = false;
    let mut framed = mount(state, wp, 1000, 700);
    for zoom in [0.5, 1.0, 2.0] {
        framed.zoom.set(zoom);
        pipeline_draw(&mut framed, 1000, 700);
        for fraction in [0.75, 0.5, 0.25, 0.0] {
            framed.scroll_y = framed.max_scroll * fraction;
            let (wp, _) = pipeline_draw(&mut framed, 1000, 700);
            assert!(
                wp.laid.iter().any(|l| {
                    let top =
                        wp.zoom.anchor().y + (l.y_top - wp.zoom.anchor().y) * wp.zoom.factor();
                    top + l.height * wp.zoom.factor() > wp.zoom_viewport.y0
                        && top < wp.zoom_viewport.y1
                }),
                "zoom {zoom}, scroll fraction {fraction} must retain visible text (window {}, scroll {})",
                wp.body_window_start,
                wp.scroll_y
            );
        }
    }
}

#[test]
fn returning_to_recent_text_reuses_shapes_and_refreshes_edited_words() {
    let state = large_body();
    let wp = WordProcessorWidget::new();
    let mut framed = mount(state, wp, 1000, 700);
    let (wp, _) = Framed::split(&mut framed);
    let first = wp.body_order.id_at(0).unwrap().to_owned();
    let cold_shapes = wp.dbg_layout_misses;
    assert!(cold_shapes >= VIEWPORT_BLOCK_WINDOW);
    let distant_scroll = num::f64_of_usize(VIEWPORT_BLOCK_WINDOW * 3) * ESTIMATED_BLOCK_EXTENT;
    wp.scroll_y = distant_scroll;
    let (wp, _) = pipeline_draw(&mut framed, 1000, 700);
    wp.scroll_y = 0.0;
    let (wp, _) = pipeline_draw(&mut framed, 1000, 700);
    assert_eq!(
        wp.dbg_layout_misses, 0,
        "returning to the previous viewport should reuse its text shaping"
    );
    assert!(wp.dbg_layout_hits >= cold_shapes);
    assert!(wp.layout_cache.len() <= VIEWPORT_BLOCK_WINDOW);
    assert!(wp.recent_layout_cache.len() <= VIEWPORT_BLOCK_WINDOW);
    wp.scroll_y = distant_scroll;
    let (wp, state) = pipeline_draw(&mut framed, 1000, 700);
    with_doc(state, |dm| {
        dm.splice_text(&first, 0, 0, "Updated ");
    });
    wp.scroll_y = 0.0;
    let (wp, _) = pipeline_draw(&mut framed, 1000, 700);
    assert_eq!(
        wp.dbg_layout_misses, 1,
        "changed offscreen text must be reshaped"
    );
    assert!(wp.blocks.first().unwrap().text.starts_with("Updated "));
    assert!(
        wp.laid
            .iter()
            .any(|l| l.id == first && l.text.starts_with("Updated "))
    );
    // A third distinct viewport must not evict the immediately previous one.
    for position in [distant_scroll / 2.0, distant_scroll, distant_scroll / 2.0] {
        framed.scroll_y = position;
        let (wp, _) = pipeline_draw(&mut framed, 1000, 700);
        assert!(wp.layout_cache.len() + wp.recent_layout_cache.len() <= 2 * VIEWPORT_BLOCK_WINDOW);
    }
    assert_eq!(framed.dbg_layout_misses, 0);
}

#[test]
fn scrolling_to_top_restores_first_blocks_without_moving_deep_caret() {
    let state = large_body();
    let wp = WordProcessorWidget::new();
    let mut framed = mount(state, wp, 1000, 700);
    let (wp, _) = Framed::split(&mut framed);
    let first = wp.body_order.id_at(0).unwrap().to_owned();
    let deep = wp
        .body_order
        .id_at(VIEWPORT_BLOCK_WINDOW * 3)
        .unwrap()
        .to_owned();
    wp.inner.set_caret(Caret {
        block: deep.clone(),
        offset: 0,
    });
    wp.scroll_y = num::f64_of_usize(VIEWPORT_BLOCK_WINDOW * 3) * ESTIMATED_BLOCK_EXTENT;
    let (wp, _) = pipeline_draw(&mut framed, 1000, 700);
    let mut handled = false;
    wp.handle_mouse_wheel(
        &MouseScrollDelta::LineDelta(0.0, 100_000.0),
        1.0,
        false,
        &mut handled,
    );
    assert!(handled);
    assert_eq!(wp.scroll_y, 0.0);
    let (wp, _) = pipeline_draw(&mut framed, 1000, 700);
    assert_eq!(
        wp.inner.caret.block, deep,
        "scrolling must not relocate the insertion point"
    );
    assert_eq!(
        wp.body_window_start, 0,
        "top viewport must load the beginning, not the distant caret"
    );
    assert_eq!(
        wp.blocks.first().map(|b| b.id.as_str()),
        Some(first.as_str())
    );
    assert!(
        wp.laid
            .iter()
            .any(|l| l.id == first && l.y_top >= 0.0 && l.y_top < 700.0)
    );
}

/// Select `[start, end)` chars of the (single) caret block.
fn select_range(wp: &mut WordProcessorWidget, start: usize, end: usize) {
    let block = wp.model().caret.block.clone();
    wp.model_mut().set_caret(Caret {
        block: block.clone(),
        offset: start,
    });
    wp.model_mut().set_anchor();
    wp.model_mut().set_caret(Caret { block, offset: end });
}

/// The `EncodedAction` a resource's immediate `ActionSpec` carries.
fn spec_encoded(res: &tilo_actions::Resource, id: &str) -> tilo_actions::EncodedAction {
    let spec = res
        .actions
        .iter()
        .find(|a| a.id == id)
        .unwrap_or_else(|| panic!("spec {id}"));
    match &spec.input {
        tilo_actions::ActionInput::Immediate(enc) => enc.clone(),
        tilo_actions::ActionInput::Prompted { .. } | tilo_actions::ActionInput::Deferred { .. } => {
            panic!("{id} is immediate")
        }
    }
}

/// The toolbar's insert-table matches the palette spec; panel align dispatches the action and arms one
/// undo boundary; find and replace all works; the styles panel toggles off; the link button requests the
/// canonical argument flow; the compact toolbar and More cover the full toolbar.
#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one scenario test: each phase is a former standalone test kept verbatim as a scoped block"
)]
fn toolbar_and_palette_actions_dispatch_through_their_specs() {
    // Phase: toolbar_insert_table_matches_palette_spec.
    // One source of truth on the **live widget path**: the Insert-Table toolbar
    // button dispatches the byte-identical `EncodedAction` the Ctrl+K palette's
    // `insert-table` spec builds for the same document.
    {
        let mut state = ui::State::new();
        let mut wp = WordProcessorWidget::new();
        wp.sync(&mut state.doc, &state.root);
        wp.model_mut().insert_str(&mut state.doc, "Intro");
        wp.refresh_blocks(&mut state.doc);

        // What the palette would dispatch, resolved before the button fires.
        let resources = ui::resources::resources(&wp, &state.doc);
        let from_palette = spec_encoded(&resources[0], "insert-table");

        // The toolbar button drives the same operation.
        wp.apply_tool(&mut state.doc, ToolAction::InsertTable);
        let from_toolbar = wp
            .last_dispatched_for_test()
            .cloned()
            .expect("the toolbar dispatched an action");

        assert_eq!(from_toolbar.type_id, from_palette.type_id);
        assert_eq!(
            from_toolbar.payload, from_palette.payload,
            "toolbar affordance and palette spec build the same EncodedAction"
        );
        // And the affordance really created the table (a 2x2 = four cells).
        wp.refresh_blocks(&mut state.doc);
        assert_eq!(
            wp.blocks.iter().filter(|b| b.kind == "cell").count(),
            4,
            "the toolbar affordance inserted the table"
        );
    }

    // Phase: panel_align_dispatches_action_and_arms_one_undo_boundary.
    // The styles panel's paragraph-align affordance dispatches an action, arms
    // exactly one undo boundary for the host, and matches the palette spec.
    {
        let mut state = ui::State::new();
        let mut wp = WordProcessorWidget::new();
        wp.sync(&mut state.doc, &state.root);
        wp.model_mut().insert_str(&mut state.doc, "Body text");
        wp.refresh_blocks(&mut state.doc);
        let before = state.doc.get_heads();

        wp.apply_panel_hit(&mut state.doc, PanelHit::ParaAlign("center"));
        let dispatched = wp
            .last_dispatched_for_test()
            .cloned()
            .expect("the panel dispatched an action");

        // The affordance mutated the document and armed one undo bracket at the
        // pre-affordance heads (what the host records as a single undo boundary).
        assert_ne!(
            state.doc.get_heads(),
            before,
            "the affordance mutated the document"
        );
        let bracket = ui::undo::take_committed_action(&mut wp).expect("an undo bracket was armed");
        assert_eq!(
            bracket, before,
            "the boundary brackets the pre-affordance heads"
        );
        assert!(
            ui::undo::take_committed_action(&mut wp).is_none(),
            "the bracket is rising-edge (cleared on read)"
        );

        // It is the same EncodedAction the palette's align-center spec builds.
        let resources = ui::resources::resources(&wp, &state.doc);
        let from_palette = spec_encoded(&resources[0], "align-center");
        assert_eq!(
            dispatched.payload, from_palette.payload,
            "panel align and palette spec build the same EncodedAction"
        );
        // The block really carries the centre alignment.
        wp.refresh_blocks(&mut state.doc);
        let aligned = wp
            .blocks
            .iter()
            .any(|b| matches!(b.attrs.get("align"), Some(AttrVal::Str(s)) if s == "center"));
        assert!(aligned, "the affordance set the paragraph alignment");
    }

    // Phase: find_and_replace_all_works.
    {
        let mut state = ui::State::new();
        let mut wp = WordProcessorWidget::new();
        wp.sync(&mut state.doc, &state.root);
        wp.model_mut()
            .insert_str(&mut state.doc, "hello world hello there");
        wp.refresh_blocks(&mut state.doc);

        wp.find.set_queries_for_tests("hello", "hi");
        wp.find.open(None, &wp.blocks);
        assert_eq!(
            wp.find.matches().len(),
            2,
            "case-insensitive search finds both"
        );

        wp.replace_all(&mut state.doc);
        wp.refresh_blocks(&mut state.doc);
        assert_eq!(wp.blocks[0].text, "hi world hi there");
        assert!(
            wp.find.matches().is_empty(),
            "no matches remain after replace-all"
        );
    }

    // Phase: styles_panel_toggles_off.
    {
        let mut state = ui::State::new();
        let mut wp = WordProcessorWidget::new();
        assert!(
            !wp.view.styles_panel_visible,
            "the first-launch writing surface is uncluttered"
        );
        wp.apply_tool(&mut state.doc, ToolAction::ToggleStyles);
        assert!(
            wp.view.styles_panel_visible,
            "toolbar toggle reveals the panel"
        );
        wp.apply_tool(&mut state.doc, ToolAction::ToggleStyles);
        assert!(
            !wp.view.styles_panel_visible,
            "toolbar toggle dismisses the panel"
        );
    }

    // Phase: selected_text_link_button_requests_the_canonical_argument_flow.
    {
        let mut state = ui::State::new();
        let mut wp = WordProcessorWidget::new();
        wp.sync(&mut state.doc, &state.root);
        wp.model_mut().insert_str(&mut state.doc, "Read this");
        wp.sync(&mut state.doc, &state.root);

        assert!(
            wp.primary_toolbar_specs()
                .iter()
                .all(|(action, _)| *action != ToolAction::Hyperlink),
            "an empty selection cannot author a link"
        );
        select_range(&mut wp, 0, 4);
        assert!(
            wp.primary_toolbar_specs()
                .iter()
                .any(|(action, _)| *action == ToolAction::Hyperlink),
            "a non-empty selection exposes the Link affordance"
        );

        wp.apply_tool(&mut state.doc, ToolAction::Hyperlink);
        assert_eq!(
            Widget::take_action_palette_request(&mut wp),
            Some(tilo_actions::ActionAddress::new(
                "word-processor.document",
                "set-hyperlink"
            ))
        );
        assert!(
            Widget::take_action_palette_request(&mut wp).is_none(),
            "the host request is edge-triggered"
        );
    }

    // Phase: compact_toolbar_and_more_cover_the_full_toolbar.
    // Responsive composition may move commands, but it must never remove one:
    // the visible compact row plus its More menu covers every action exposed by
    // the full desktop toolbar.
    {
        let mut state = ui::State::new();
        let mut wp = WordProcessorWidget::new();
        wp.sync(&mut state.doc, &state.root);
        wp.model_mut().insert_str(&mut state.doc, "coverage");

        let full = wp
            .toolbar_button_specs(false)
            .into_iter()
            .map(|(action, _)| action)
            .collect::<Vec<_>>();
        let mut compact = wp
            .toolbar_button_specs(true)
            .into_iter()
            .map(|(action, _)| action)
            .filter(|action| *action != ToolAction::More)
            .collect::<Vec<_>>();
        wp.open_toolbar_overflow();
        compact.extend(wp.context_actions.iter().filter_map(|entry| match entry {
            Some(CtxAction::Tool(action)) => Some(*action),
            _ => None,
        }));

        for action in full {
            assert!(
                compact.contains(&action),
                "compact toolbar and More menu lost {action:?}"
            );
        }
    }
}

/// Highlight marks yield a background span, super marks a raised baseline, and 2x line spacing a
/// taller layout.
#[test]
fn character_marks_shape_their_spans() {
    // Phase: highlight_mark_yields_background_span.
    {
        let mut state = ui::State::new();
        let mut wp = WordProcessorWidget::new();
        wp.sync(&mut state.doc, &state.root);
        wp.model_mut().insert_str(&mut state.doc, "hello world");
        select_range(&mut wp, 0, 5);
        wp.model_mut().set_highlight(&mut state.doc, "#fff35c");
        wp.refresh_blocks(&mut state.doc);
        let b = wp
            .blocks
            .iter()
            .find(|b| b.text == "hello world")
            .expect("block present");
        let (runs, _, _) = block_runs(b);
        // The first run ("hello") carries the highlight as a background band.
        let first = &runs[0];
        assert_eq!(first.text, "hello");
        assert_eq!(
            first.background,
            Some([0xff, 0xf3, 0x5c]),
            "highlight maps to RichSpan.background"
        );
        // The tail run (" world") has no highlight.
        assert!(
            runs.iter()
                .any(|r| r.text.contains("world") && r.background.is_none()),
            "unhighlighted tail has no band"
        );
    }

    // Phase: super_mark_yields_super_baseline.
    {
        let mut state = ui::State::new();
        let mut wp = WordProcessorWidget::new();
        wp.sync(&mut state.doc, &state.root);
        wp.model_mut().insert_str(&mut state.doc, "x2");
        select_range(&mut wp, 1, 2); // the "2"
        wp.model_mut().toggle_super(&mut state.doc);
        wp.refresh_blocks(&mut state.doc);
        let b = wp
            .blocks
            .iter()
            .find(|b| b.text == "x2")
            .expect("block present");
        let (runs, _, _) = block_runs(b);
        let sup = runs.iter().find(|r| r.text == "2").expect("the '2' run");
        assert_eq!(
            sup.baseline,
            ui::richtext::BaselineShift::Super,
            "super mark maps to BaselineShift::Super"
        );
        let base = runs.iter().find(|r| r.text == "x").expect("the 'x' run");
        assert_eq!(
            base.baseline,
            ui::richtext::BaselineShift::None,
            "unmarked run stays on the baseline"
        );
    }

    // Phase: line_spacing_2x_shapes_a_taller_layout.
    {
        // A wrapped multi-line paragraph: single vs double spacing on the SAME
        // production path (`block_line_spacing` → `RichTextLayout::shape`'s
        // line_height_mult). Double spacing must produce a strictly taller layout.
        let mut state = ui::State::new();
        let mut wp = WordProcessorWidget::new();
        wp.sync(&mut state.doc, &state.root);
        let long = "Lorem ipsum dolor sit amet consectetur adipiscing elit sed do eiusmod tempor incididunt ut labore et dolore magna aliqua";
        wp.model_mut().insert_str(&mut state.doc, long);

        // Single-spaced layout via the production draw pass.
        let mut framed = mount(state, wp, 700, 700);
        let (wp, state) = Framed::split(&mut framed);
        let single_h = wp
            .laid
            .iter()
            .find(|l| l.text == long)
            .map(|l| l.height)
            .expect("laid block");
        assert_eq!(
            block_line_spacing(wp.blocks.iter().find(|b| b.text == long).unwrap()),
            1.0,
            "default 1.0"
        );

        // Now set double spacing and redraw on the same path.
        wp.model_mut().set_line_spacing(&mut state.doc, 2.0);
        wp.refresh_blocks(&mut state.doc);
        assert_eq!(
            block_line_spacing(wp.blocks.iter().find(|b| b.text == long).unwrap()),
            2.0,
            "line_spacing=2.0 read from the block attr the layout pass uses"
        );
        let (wp, _) = pipeline_draw(&mut framed, 700, 700);
        let double_h = wp
            .laid
            .iter()
            .find(|l| l.text == long)
            .map(|l| l.height)
            .expect("laid block");
        assert!(
            double_h > single_h * 1.5,
            "double line spacing yields a much taller layout: single={single_h}, double={double_h}"
        );
    }
}

/// The layout cache reuses unchanged blocks and reshapes after an edit, and draw renders headings and
/// bold.
#[test]
fn headings_draw_bold_and_unchanged_blocks_reuse_their_layout() {
    // Phase: layout_cache_reuses_unchanged_blocks_and_reshapes_after_edit.
    {
        let mut state = ui::State::new();
        let mut wp = WordProcessorWidget::new();
        wp.sync(&mut state.doc, &state.root);
        wp.model_mut()
            .insert_str(&mut state.doc, "First paragraph here");
        wp.model_mut().enter(&mut state.doc);
        wp.model_mut()
            .insert_str(&mut state.doc, "Second paragraph here");

        // Cold frame: the cache is empty, so every block is shaped (a miss).
        let mut framed = mount(state, wp, 1000, 700);
        let (wp, _) = Framed::split(&mut framed);
        assert_eq!(wp.dbg_layout_hits, 0, "cold draw cannot reuse anything");
        assert_eq!(
            wp.dbg_layout_misses, 2,
            "both paragraphs shaped on the first frame"
        );
        assert_eq!(wp.layout_cache.len(), 2, "both blocks cached");

        // Warm frame, no edits: every block's key matches, so all are reused and
        // nothing is re-shaped — this is the per-frame O(n) shaping we eliminated.
        let (wp, state) = pipeline_draw(&mut framed, 1000, 700);
        assert_eq!(
            wp.dbg_layout_hits, 2,
            "unchanged re-draw reuses every cached layout"
        );
        assert_eq!(
            wp.dbg_layout_misses, 0,
            "no re-shaping on an unchanged frame"
        );

        // Edit one block (caret is in the 2nd paragraph): only that block's key
        // changes, so only it re-shapes; the untouched block is still reused.
        wp.model_mut().insert_str(&mut state.doc, "!");
        let (wp, _) = pipeline_draw(&mut framed, 1000, 700);
        assert_eq!(
            wp.dbg_layout_misses, 1,
            "only the edited block is re-shaped"
        );
        assert_eq!(wp.dbg_layout_hits, 1, "the untouched block is still reused");

        // A margin change alters the wrap width (the page is fixed-width, so the
        // window size does not — only the margins do). Wrap width is part of the
        // key, so every block must re-shape at the new measure.
        wp.margins = Edges::uniform(140.0);
        let (wp, _) = pipeline_draw(&mut framed, 1000, 700);
        assert_eq!(
            wp.dbg_layout_misses, 2,
            "a wrap-width change re-shapes every block"
        );
        assert_eq!(
            wp.dbg_layout_hits, 0,
            "no block can be reused at a new wrap width"
        );
    }

    // Phase: draw_renders_headings_and_bold.
    {
        use tilo_doc_codecs::{FormatProvider, MarkdownCodec};
        let mut state = ui::State::new();
        let c = tilo_doc::model::init(&mut state.doc, &state.root);
        let mut ids = tilo_doc::IdGen::new(state.doc.get_actor().to_hex_string());
        {
            let mut dm = tilo_doc::DocMut {
                doc: &mut state.doc,
                c: &c,
                ids: &mut ids,
            };
            MarkdownCodec
                .import(
                    b"# Title\n\nHello **bold** world\n",
                    &mut dm,
                    &mut tilo_content::ContentInputs::new(),
                )
                .unwrap();
        }
        let wp = WordProcessorWidget::new();
        let mut framed = mount(state, wp, 1000, 700);
        let (wp, _) = Framed::split(&mut framed);
        assert!(wp.laid.len() >= 2);
        assert!(wp.blocks.iter().any(|b| b.kind == "heading"));
        assert!(
            wp.blocks
                .iter()
                .any(|b| b.marks.iter().any(|m| m.name == "strong"))
        );
    }
}

/// The styles panel renders hits and picks a style, editing a style updates every instance, and typing
/// into a numeric field commits.
#[test]
fn the_styles_panel_picks_edits_and_commits_styles() {
    // Phase: styles_panel_renders_hits_and_picks_a_style.
    {
        let mut state = ui::State::new();
        let mut wp = WordProcessorWidget::new();
        wp.sync(&mut state.doc, &state.root);
        wp.model_mut().insert_str(&mut state.doc, "A paragraph");
        wp.view.styles_panel_visible = true;
        let mut framed = mount(state, wp, 1000, 700);
        let (wp, state) = Framed::split(&mut framed);

        // An explicitly-open panel renders clickable targets.
        assert!(wp.view.styles_panel_visible);
        assert!(
            !wp.panel_hits.is_empty(),
            "panel rendered clickable targets"
        );
        assert_eq!(wp.focused_style_key().as_deref(), Some("body"));

        // Picking the Quote style makes the caret block adopt it (component-instance).
        wp.apply_panel_hit(&mut state.doc, PanelHit::Pick("quote".into()));
        wp.refresh_blocks(&mut state.doc);
        assert_eq!(
            wp.focused_style_key().as_deref(),
            Some("quote"),
            "block adopted the picked style"
        );
    }

    // Phase: editing_a_style_updates_every_instance.
    {
        let mut state = ui::State::new();
        let mut wp = WordProcessorWidget::new();
        wp.sync(&mut state.doc, &state.root);
        wp.model_mut().insert_str(&mut state.doc, "first");
        wp.model_mut().enter(&mut state.doc);
        wp.model_mut().insert_str(&mut state.doc, "second");
        let mut framed = mount(state, wp, 1000, 700);
        let (wp, state) = Framed::split(&mut framed);

        let before = wp.model().stylesheet(&state.doc).get("body").size;
        // Bump the body style's size via the panel stepper.
        wp.apply_panel_hit(&mut state.doc, PanelHit::NumDelta(NumField::Size, 5.0));
        let after = wp.model().stylesheet(&state.doc).get("body").size;
        assert_eq!(
            after,
            before + 5.0,
            "edited style size persisted to the document"
        );

        // Both paragraphs are Body, so both re-render at the new size.
        let (wp, _) = pipeline_draw(&mut framed, 1000, 700);
        assert_eq!(wp.laid.len(), 2);
        assert!(
            wp.laid.iter().all(|l| (l.size - after).abs() < 0.01),
            "every Body block follows the style edit"
        );
    }

    // Phase: typing_into_a_numeric_field_commits.
    {
        let mut state = ui::State::new();
        let mut wp = WordProcessorWidget::new();
        wp.sync(&mut state.doc, &state.root);
        wp.model_mut().insert_str(&mut state.doc, "x");
        let mut framed = mount(state, wp, 1000, 700);
        let (wp, state) = Framed::split(&mut framed);

        // Click-to-type: focus the Size field, type "40".
        wp.editing_field = Some(NumField::Size);
        wp.edit_buf = "40".into();
        wp.apply_edit_buf_live();
        // Live preview mutated the widget sheet (not yet the document).
        assert_eq!(wp.sheet.get("body").size, 40.0);
        wp.commit_num_edit(&mut state.doc);
        assert!(wp.editing_field.is_none(), "Enter blurs the field");
        assert_eq!(
            wp.model().stylesheet(&state.doc).get("body").size,
            40.0,
            "typed value persisted"
        );
    }
}

/// New style from this duplicates and applies; a custom style can be deleted but a builtin never is;
/// font size lives in exactly one place.
#[test]
fn custom_styles_are_created_deleted_and_sized_in_one_place() {
    // Phase: new_style_from_this_duplicates_and_applies.
    {
        let mut state = ui::State::new();
        let mut wp = WordProcessorWidget::new();
        wp.sync(&mut state.doc, &state.root);
        wp.model_mut().insert_str(&mut state.doc, "para");
        wp.set_styles_panel_open(true);
        let mut framed = mount(state, wp, 1000, 700);
        let (wp, state) = Framed::split(&mut framed);

        let n_before = wp.sheet.order().len();
        wp.apply_panel_hit(&mut state.doc, PanelHit::NewStyle);
        let sheet = wp.model().stylesheet(&state.doc);
        assert_eq!(
            sheet.order().len(),
            n_before + 1,
            "a new style id was added"
        );
        wp.refresh_blocks(&mut state.doc);
        let key = wp.focused_style_key().unwrap();
        assert!(
            key.starts_with("custom"),
            "block adopted the duplicated style, got {key}"
        );
    }

    // Phase: deleting_a_custom_style_removes_it.
    // Deleting a custom style removes it from the document's sheet and reassigns
    // the block that used it (so no dangling style ref remains).
    {
        let (mut framed, custom_id) = doc_with_custom_style();
        let (wp, state) = Framed::split(&mut framed);
        assert!(
            wp.sheet.styles.contains_key(&custom_id),
            "custom style present before delete"
        );

        // Apply the typed outcome produced by the extracted menu component.
        wp.dispatch_style_menu(StyleMenuAction::Delete(custom_id.clone()), &mut state.doc);
        wp.sync(&mut state.doc, &state.root);

        assert!(
            !wp.sheet.styles.contains_key(&custom_id),
            "custom style removed from the sheet"
        );
        assert!(
            !tilo_doc::model::style_ids(&state.doc, &wp.model().containers.clone().unwrap())
                .iter()
                .any(|s| s == &custom_id),
            "custom style removed from the document"
        );
        // The block no longer references the deleted style.
        let c = wp.model().containers.clone().unwrap();
        let dangling = tilo_doc::model::materialize(&state.doc, &c).blocks.iter().any(
            |b| matches!(b.attrs.get("style"), Some(tilo_doc::AttrVal::Str(s)) if s == &custom_id),
        );
        assert!(!dangling, "no block still references the deleted style");
    }

    // Phase: builtin_style_is_never_deleted.
    // A built-in style cannot be deleted through the model even if asked.
    {
        let (mut state, mut wp) = bound();
        wp.model_mut().insert_str(&mut state.doc, "x");
        assert!(
            !wp.model_mut().delete_style(&mut state.doc, "body"),
            "built-in delete is refused"
        );
        wp.sync(&mut state.doc, &state.root);
        assert!(
            wp.sheet.styles.contains_key("body"),
            "Body survives a delete attempt"
        );
    }

    // Phase: font_size_lives_in_exactly_one_place.
    // The inspector owns the paragraph style's default size, while A−/A+ are
    // selection-scoped character formatting. Keeping those two scopes distinct
    // avoids a duplicated *style* control without hiding direct formatting.
    {
        let (mut state, mut wp) = bound();
        wp.model_mut().insert_str(&mut state.doc, "Body text");
        wp.view.styles_panel_visible = true;
        let mut framed = mount(state, wp, 1800, 700);
        let (wp, _) = Framed::split(&mut framed);

        let panel_size_fields = wp
            .panel_hits
            .iter()
            .filter(|(_, h)| matches!(h, PanelHit::Num(NumField::Size)))
            .count();
        assert_eq!(
            panel_size_fields, 1,
            "the inspector is the single home for the style default"
        );

        // The desktop toolbar exposes selection-scoped A−/A+ controls; the
        // dedicated direct-formatting test above proves they do not edit Style.
        assert!(
            wp.toolbar
                .iter()
                .any(|(a, _)| matches!(a, ToolAction::FontSizeDec)),
            "A− remains available for direct character formatting"
        );
        assert!(
            wp.toolbar
                .iter()
                .any(|(a, _)| matches!(a, ToolAction::FontSizeInc)),
            "A+ remains available for direct character formatting"
        );
    }
}

// ---- toolbar ⇄ panel reorganization: paragraph controls belong to the
// side panel; the toolbar is direct character formatting only (no dupes) ----

/// The toolbar drops paragraph controls and keeps character formatting, the compact overflow keeps
/// every direct colour action, and compact palette clear is pointer, keyboard and @ equivalent.
#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one scenario test: each phase is a former standalone test kept verbatim as a scoped block"
)]
fn the_toolbar_keeps_character_formatting_and_every_colour_action() {
    // Phase: toolbar_drops_paragraph_controls_and_keeps_character_formatting.
    // The toolbar carries only direct character formatting + insert/view
    // controls; paragraph-style presets live in the side panel's style picker.
    // Asserted on `ToolAction` variants — those variants no longer exist, so a
    // regression re-adding them fails to compile against the allowed-set below.
    {
        let mut state = ui::State::new();
        let mut wp = WordProcessorWidget::new();
        wp.sync(&mut state.doc, &state.root);
        wp.model_mut().insert_str(&mut state.doc, "para");
        // Use a desktop width at which the complete command set is genuinely
        // visible. At medium widths the compact toolbar intentionally moves
        // lower-frequency commands into More (covered below).
        let mut framed = mount(state, wp, 1800, 760);
        let (wp, _) = Framed::split(&mut framed);

        let actions: Vec<ToolAction> = wp.toolbar.iter().map(|(a, _)| *a).collect();
        // Character formatting + the direct font-size nudges are present.
        for kept in [
            ToolAction::Bold,
            ToolAction::Italic,
            ToolAction::Underline,
            ToolAction::Strike,
            ToolAction::Super,
            ToolAction::Sub,
            ToolAction::FontCycle,
            ToolAction::FontSizeInc,
            ToolAction::FontSizeDec,
        ] {
            assert!(
                actions.contains(&kept),
                "{kept:?} stays on the toolbar; toolbar={actions:?}"
            );
        }
        // Insert / view structural actions stay.
        assert!(
            actions.contains(&ToolAction::InsertTable),
            "table insert stays on the toolbar"
        );
        assert!(
            actions.contains(&ToolAction::ToggleStyles),
            "styles-panel toggle stays on the toolbar"
        );

        // Every toolbar action is direct character formatting or an insert/view
        // action — NONE switches the paragraph style/kind. (Alignment + line
        // spacing AND the Body/Heading/List style picker live in the side panel;
        // see `panel_owns_paragraph_alignment_and_the_style_editor` and
        // `panel_style_picker_switches_paragraph_kind`.)
        let is_char_or_structural = |a: &ToolAction| {
            matches!(
                a,
                ToolAction::Bold
                    | ToolAction::Italic
                    | ToolAction::Underline
                    | ToolAction::Strike
                    | ToolAction::Super
                    | ToolAction::Sub
                    | ToolAction::Hyperlink
                    | ToolAction::FontCycle
                    | ToolAction::FontSizeInc
                    | ToolAction::FontSizeDec
                    | ToolAction::TextColor(_)
                    | ToolAction::Highlight(_)
                    | ToolAction::InsertImage
                    | ToolAction::InsertTable
                    | ToolAction::TableRowAdd
                    | ToolAction::TableRowDel
                    | ToolAction::TableColAdd
                    | ToolAction::TableColDel
                    | ToolAction::TableDelete
                    | ToolAction::ToggleStyles
                    | ToolAction::ToggleRuler
                    | ToolAction::ToggleOutline
                    | ToolAction::ToggleHistory
                    | ToolAction::InsertToc
                    | ToolAction::NewMenu
                    | ToolAction::More
            )
        };
        for a in &actions {
            assert!(
                is_char_or_structural(a),
                "unexpected paragraph-level control on the toolbar: {a:?}"
            );
        }
    }

    // Phase: compact_toolbar_overflow_keeps_every_direct_colour_action.
    // Compact mode keeps More concise: two labelled entries open the shared
    // visual swatch grid, whose selections still use canonical actions.
    {
        let mut state = ui::State::new();
        let mut wp = WordProcessorWidget::new();
        wp.sync(&mut state.doc, &state.root);
        wp.model_mut().insert_str(&mut state.doc, "colour me");
        let mut framed = mount(state, wp, 1280, 760);
        let (wp, state) = Framed::split(&mut framed);
        assert!(wp.use_compact_toolbar(1280.0));

        wp.apply_tool(&mut state.doc, ToolAction::More);
        assert!(
            wp.context_actions
                .iter()
                .any(|action| { *action == Some(CtxAction::CompactPalette(PaletteKind::Text)) })
        );
        assert!(
            wp.context_actions.iter().any(|action| {
                *action == Some(CtxAction::CompactPalette(PaletteKind::Highlight))
            })
        );

        wp.open_compact_swatch_palette(PaletteKind::Text);
        assert!(wp.compact_swatch_palette.rect().width() > 0.0);
        let swatch = wp
            .compact_swatch_palette
            .swatch_rect(3)
            .expect("shared picker lays out each swatch")
            .center();
        left_click(wp, state, swatch);
        wp.refresh_blocks(&mut state.doc);
        assert_eq!(wp.current_text_color(), Some(STYLE_PALETTE[3]));
    }

    // Phase: compact_palette_clear_is_pointer_keyboard_and_at_equivalent.
    // The compact palette's clear footer is a real, localized semantic button
    // with the same typed action route for pointer, keyboard, and AT input.
    {
        let mut state = ui::State::new();
        let mut wp = WordProcessorWidget::new();
        wp.sync(&mut state.doc, &state.root);
        wp.model_mut().insert_str(&mut state.doc, "colour me");
        let mut framed = mount(state, wp, 760, 800);
        let (wp, state) = Framed::split(&mut framed);

        // Pointer clear for text colour.
        wp.apply_tool(
            &mut state.doc,
            ToolAction::TextColor(Some(STYLE_PALETTE[2])),
        );
        wp.open_compact_swatch_palette(PaletteKind::Text);
        let pointer_clear = wp.compact_swatch_palette.clear_rect().center();
        left_click(wp, state, pointer_clear);
        assert_eq!(wp.current_text_color(), None);

        // Keyboard clear for text colour. Tab moves focus from the shared
        // picker to the visible footer, then Space activates the exact action.
        wp.apply_tool(
            &mut state.doc,
            ToolAction::TextColor(Some(STYLE_PALETTE[3])),
        );
        wp.open_compact_swatch_palette(PaletteKind::Text);
        let expected_clear_bounds = wp.compact_swatch_palette.clear_rect();
        press_named(wp, state, NamedKey::Tab);
        assert_compact_clear_footer(wp, &t("picker-text-color"), expected_clear_bounds, true);
        press_named(wp, state, NamedKey::Space);
        assert_eq!(wp.current_text_color(), None);

        // AT clear for text colour routes through the same ToolAction helper.
        wp.apply_tool(
            &mut state.doc,
            ToolAction::TextColor(Some(STYLE_PALETTE[4])),
        );
        wp.open_compact_swatch_palette(PaletteKind::Text);
        assert!(Widget::perform_action_at(
            wp,
            &ui::a11y::Action::Focus,
            &[ui::a11y::hash_key(&"compact-swatch:clear")],
            &mut state.doc,
            &state.root,
        ));
        assert_compact_clear_footer(
            wp,
            &t("picker-text-color"),
            wp.compact_swatch_palette.clear_rect(),
            true,
        );
        assert!(Widget::perform_action_at(
            wp,
            &ui::a11y::Action::Activate,
            &[ui::a11y::hash_key(&"compact-swatch:clear")],
            &mut state.doc,
            &state.root,
        ));
        assert_eq!(wp.current_text_color(), None);

        // Repeat the three modalities for highlight, whose clear action is a
        // distinct document mutation but shares the same palette plumbing.
        wp.apply_tool(
            &mut state.doc,
            ToolAction::Highlight(Some(HIGHLIGHT_PALETTE[2])),
        );
        wp.open_compact_swatch_palette(PaletteKind::Highlight);
        let highlight_pointer_clear = wp.compact_swatch_palette.clear_rect().center();
        left_click(wp, state, highlight_pointer_clear);
        assert_eq!(wp.current_highlight_color(), None);

        wp.apply_tool(
            &mut state.doc,
            ToolAction::Highlight(Some(HIGHLIGHT_PALETTE[3])),
        );
        wp.open_compact_swatch_palette(PaletteKind::Highlight);
        press_named(wp, state, NamedKey::Tab);
        press_named(wp, state, NamedKey::Enter);
        assert_eq!(wp.current_highlight_color(), None);

        wp.apply_tool(
            &mut state.doc,
            ToolAction::Highlight(Some(HIGHLIGHT_PALETTE[4])),
        );
        wp.open_compact_swatch_palette(PaletteKind::Highlight);
        assert!(Widget::perform_action_at(
            wp,
            &ui::a11y::Action::Activate,
            &[ui::a11y::hash_key(&"compact-swatch:clear")],
            &mut state.doc,
            &state.root,
        ));
        assert_eq!(wp.current_highlight_color(), None);
    }
}

fn assert_compact_clear_footer(
    wp: &WordProcessorWidget,
    title: &str,
    expected_bounds: Rect,
    focused: bool,
) {
    let tree = wp.accessibility_tree();
    let palette = tree
        .children
        .iter()
        .find(|child| child.node.name.as_deref() == Some(title))
        .expect("open palette is a named accessibility group");
    let clear = palette
        .node
        .children
        .iter()
        .find(|child| child.action_target.as_deref() == Some("compact-swatch:clear"))
        .expect("localized clear footer is exported");
    assert_eq!(clear.role, ui::a11y::Role::Button);
    assert_eq!(clear.bounds, Some(expected_bounds.into()));
    assert_eq!(clear.state.focused, focused);
}

/// Alignment and line spacing apply to the paragraph via the panel, and the panel owns paragraph
/// alignment and the style editor.
#[test]
fn the_panel_applies_paragraph_alignment_and_spacing() {
    // Phase: alignment_applies_to_the_paragraph_via_the_panel.
    // Alignment now applies to the current paragraph through the side panel's
    // "This paragraph" section — an instance `align` attr on the caret block,
    // read back on the production layout path — and does NOT edit the style.
    {
        let mut state = ui::State::new();
        let mut wp = WordProcessorWidget::new();
        wp.sync(&mut state.doc, &state.root);
        wp.model_mut().insert_str(&mut state.doc, "centre me");
        let mut framed = mount(state, wp, 1000, 700);
        let (wp, state) = Framed::split(&mut framed);
        assert_eq!(
            wp.current_align(),
            "left",
            "paragraphs are left-aligned by default"
        );

        // Apply Center on the real dispatch path the panel registers.
        wp.apply_panel_hit(&mut state.doc, PanelHit::ParaAlign("center"));
        assert_eq!(
            wp.current_align(),
            "center",
            "the current paragraph is now centred"
        );
        // It is a per-paragraph (instance) attr, not an edit of the Body style.
        assert_eq!(
            wp.model().stylesheet(&state.doc).get("body").align,
            "left",
            "the Body style default alignment is untouched — the panel edited the paragraph, not the preset"
        );
    }

    // Phase: line_spacing_applies_to_the_paragraph_via_the_panel.
    // Line spacing likewise cycles the current paragraph from the panel (1.0 →
    // 1.15), on the same `caret_line_spacing` path the layout pass reads.
    {
        let mut state = ui::State::new();
        let mut wp = WordProcessorWidget::new();
        wp.sync(&mut state.doc, &state.root);
        wp.model_mut().insert_str(&mut state.doc, "spaced");
        let mut framed = mount(state, wp, 1000, 700);
        let (wp, state) = Framed::split(&mut framed);
        assert!(
            (wp.model().caret_line_spacing(&state.doc) - 1.0).abs() < 1e-9,
            "single spacing by default"
        );

        wp.apply_panel_hit(&mut state.doc, PanelHit::ParaLineSpacing);
        assert!(
            (wp.model().caret_line_spacing(&state.doc) - 1.15).abs() < 1e-9,
            "cycled to the next preset"
        );
    }

    // Phase: panel_owns_paragraph_alignment_and_the_style_editor.
    // The panel registers the paragraph-alignment controls (all four) + line
    // spacing every frame AND still exposes the named-style editor — the two
    // clearly-labelled halves ("this paragraph" vs "this style"), so alignment
    // is not duplicated between the toolbar and the panel.
    {
        let mut state = ui::State::new();
        let mut wp = WordProcessorWidget::new();
        wp.sync(&mut state.doc, &state.root);
        wp.model_mut().insert_str(&mut state.doc, "para");
        wp.set_styles_panel_open(true);
        let mut framed = mount(state, wp, 1000, 700);
        let (wp, _) = Framed::split(&mut framed);

        let has = |want: &PanelHit| wp.panel_hits.iter().any(|(_, h)| h == want);
        for a in ["left", "center", "right", "justify"] {
            assert!(
                has(&PanelHit::ParaAlign(a)),
                "the panel owns paragraph alignment `{a}`"
            );
        }
        assert!(
            has(&PanelHit::ParaLineSpacing),
            "the panel owns paragraph line spacing"
        );
        // The named-style editor is still present in the same panel.
        assert!(
            has(&PanelHit::ToggleBold),
            "the style editor stays (weight toggle present)"
        );
        assert!(
            wp.panel_hits
                .iter()
                .any(|(_, h)| matches!(h, PanelHit::Num(NumField::Size))),
            "the style Size field stays"
        );
    }
}

/// The panel style picker switches paragraph kind, and font size and bold apply to the selection from
/// the toolbar.
#[test]
fn paragraph_kind_font_size_and_bold_apply_from_panel_and_toolbar() {
    // Phase: panel_style_picker_switches_paragraph_kind.
    // The panel's style picker is the authoritative paragraph-style control.
    // Picking a built-in style switches the block *kind* too — "Heading 1"
    // yields a real heading that shows up in the outline, "Body" reverts to a
    // paragraph. Runs the production `apply_panel_hit` → `Pick` dispatch,
    // asserting on the materialized block kind, not a helper.
    {
        let mut state = ui::State::new();
        let mut wp = WordProcessorWidget::new();
        wp.sync(&mut state.doc, &state.root);
        wp.model_mut()
            .insert_str(&mut state.doc, "make me a heading");
        let mut framed = mount(state, wp, 1000, 700);
        let (wp, state) = Framed::split(&mut framed);

        let block_kind = |wp: &WordProcessorWidget, st: &ui::State| -> (String, Option<i64>) {
            let c = wp.model().containers.clone().unwrap();
            let b = tilo_doc::model::materialize(&st.doc, &c)
                .blocks
                .into_iter()
                .next()
                .unwrap();
            let level = match b.attrs.get("level") {
                Some(AttrVal::Int(n)) => Some(*n),
                _ => None,
            };
            (b.kind, level)
        };

        // A fresh paragraph starts as `paragraph` and is absent from the outline.
        assert_eq!(block_kind(wp, state).0, "paragraph");
        assert!(wp.outline_entries().is_empty(), "no headings yet");

        // Pick "Heading 1" from the panel → a REAL heading (kind + level), and it
        // now appears in the document outline / TOC source.
        wp.apply_panel_hit(&mut state.doc, PanelHit::Pick("heading1".into()));
        let (wp, state) = pipeline_draw(&mut framed, 1000, 700);
        assert_eq!(
            block_kind(wp, state),
            ("heading".into(), Some(1)),
            "picker made it a level-1 heading"
        );
        assert_eq!(
            wp.outline_entries().len(),
            1,
            "the heading is now in the outline"
        );

        // Pick "List Item" → a real list item (not a heading anymore).
        wp.apply_panel_hit(&mut state.doc, PanelHit::Pick("list".into()));
        assert_eq!(
            block_kind(wp, state).0,
            "list_item",
            "picker made it a list item"
        );
        assert!(
            wp.outline_entries().is_empty(),
            "a list item is not a heading"
        );

        // Pick "Body" → back to a plain paragraph.
        wp.apply_panel_hit(&mut state.doc, PanelHit::Pick("body".into()));
        assert_eq!(
            block_kind(wp, state).0,
            "paragraph",
            "picker restored the paragraph kind"
        );
    }

    // Phase: font_size_applies_to_the_selection_from_the_toolbar.
    // Direct font size is character formatting on the selection — the toolbar's
    // A+ writes a per-run `size` mark, distinct from (and leaving untouched) the
    // style's default size in the panel.
    {
        let mut state = ui::State::new();
        let mut wp = WordProcessorWidget::new();
        wp.sync(&mut state.doc, &state.root);
        wp.model_mut().insert_str(&mut state.doc, "Hello");
        let mut framed = mount(state, wp, 1000, 700);
        let (wp, state) = Framed::split(&mut framed);

        let style_size = wp.model().stylesheet(&state.doc).get("body").size;
        let base = wp.current_font_size();
        assert!(
            (base - style_size).abs() < 0.01,
            "with no run mark the effective size is the style default"
        );

        wp.apply_tool(&mut state.doc, ToolAction::FontSizeInc);
        wp.refresh_blocks(&mut state.doc);
        let up = wp.current_font_size();
        assert!(up > base, "A+ enlarges the selection: {base} -> {up}");
        // The nudge is a per-run character mark, not an edit of the Body style.
        assert!(
            (wp.model().stylesheet(&state.doc).get("body").size - style_size).abs() < 0.01,
            "the Body style default size is untouched — direct formatting only"
        );
    }

    // Phase: bold_still_applies_to_the_selection_from_the_toolbar.
    // Character formatting from the toolbar still applies to the selection
    // (Bold over a select-all range), unchanged by the reorganization.
    {
        let mut state = ui::State::new();
        let mut wp = WordProcessorWidget::new();
        wp.sync(&mut state.doc, &state.root);
        wp.model_mut().insert_str(&mut state.doc, "bold me");
        wp.model_mut().select_all(&state.doc);
        let mut framed = mount(state, wp, 1000, 700);
        let (wp, state) = Framed::split(&mut framed);
        assert!(!wp.mark_active_in_selection("strong"), "not bold yet");

        wp.apply_tool(&mut state.doc, ToolAction::Bold);
        wp.refresh_blocks(&mut state.doc);
        assert!(
            wp.mark_active_in_selection("strong"),
            "the whole selection is now bold"
        );
    }
}

/// Build a doc directly in `state` from a closure over a `DocMut`.
fn with_doc(state: &mut ui::State, f: impl FnOnce(&mut tilo_doc::DocMut)) {
    let c = tilo_doc::model::init(&mut state.doc, &state.root);
    let mut ids = tilo_doc::IdGen::new(state.doc.get_actor().to_hex_string());
    let mut dm = tilo_doc::DocMut {
        doc: &mut state.doc,
        c: &c,
        ids: &mut ids,
    };
    f(&mut dm);
}

/// The saved form of [`build_large_body`], checked in because building it one
/// operation at a time costs seconds of automerge work in every test process
/// that needs it, while loading it is immediate.
const LARGE_BODY: &[u8] = include_bytes!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/tests/fixtures/large-body.automerge"
));

/// A document whose body holds `VIEWPORT_BLOCK_WINDOW * 4` paragraphs reading
/// `Paragraph 0` to `Paragraph 767`, enough for the viewport window to move.
fn large_body() -> ui::State {
    ui::State::load(LARGE_BODY).expect(
        "the large-body fixture loads; regenerate it with UPDATE_WP_LARGE_BODY=1 \
         cargo test -p word-processor --lib large_body_fixture",
    )
}

/// How the large-body fixture is made: the same edits a person's typing
/// would produce, block by block.
fn build_large_body() -> ui::State {
    let mut state = ui::State::new();
    with_doc(&mut state, |dm| {
        let mut after = None;
        for i in 0..(VIEWPORT_BLOCK_WINDOW * 4) {
            let id = dm.insert_block(after.as_deref(), "paragraph");
            dm.splice_text(&id, 0, 0, &format!("Paragraph {i}"));
            after = Some(id);
        }
    });
    state
}

#[test]
fn large_body_fixture_holds_the_paragraphs_its_tests_expect() {
    if std::env::var_os("UPDATE_WP_LARGE_BODY").is_some() {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/large-body.automerge");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, build_large_body().doc.save()).unwrap();
        eprintln!("updated {}", path.display());
        return;
    }
    let mut state = large_body();
    let mut wp = WordProcessorWidget::new();
    wp.sync(&mut state.doc, &state.root);
    let blocks = wp.search_blocks(&state.doc);
    assert_eq!(blocks.len(), VIEWPORT_BLOCK_WINDOW * 4);
    for (i, block) in blocks.iter().enumerate() {
        assert_eq!(block.kind, "paragraph", "block {i}");
        assert_eq!(block.text, format!("Paragraph {i}"), "block {i}");
    }
}

/// Nested list items indent and pick their glyph by level, ordered items number per level, and
/// multi-digit markers grow away from the paragraph text.
#[test]
fn list_markers_indent_number_and_grow_by_level() {
    // Phase: nested_list_items_indent_and_pick_glyph_by_level.
    {
        let mut state = ui::State::new();
        with_doc(&mut state, |dm| {
            let a = dm.insert_block(None, "list_item");
            dm.splice_text(&a, 0, 0, "a");
            let b = dm.insert_block(Some(&a), "list_item");
            dm.splice_text(&b, 0, 0, "b");
            dm.set_attr(&b, "level", 1_i64);
            let cc = dm.insert_block(Some(&b), "list_item");
            dm.splice_text(&cc, 0, 0, "c");
            dm.set_attr(&cc, "level", 2_i64);
        });
        let wp = WordProcessorWidget::new();
        let mut framed = mount(state, wp, 1000, 900);
        let (wp, _) = Framed::split(&mut framed);

        let la = wp.laid.iter().find(|l| l.text == "a").unwrap();
        let lb = wp.laid.iter().find(|l| l.text == "b").unwrap();
        let lc = wp.laid.iter().find(|l| l.text == "c").unwrap();
        assert!(
            lb.text_x > la.text_x && lc.text_x > lb.text_x,
            "deeper levels indent further"
        );
        // Unordered glyphs cycle • ◦ ▪ by depth.
        assert_eq!(la.bullet.as_deref(), Some("•"));
        assert_eq!(lb.bullet.as_deref(), Some("◦"));
        assert_eq!(lc.bullet.as_deref(), Some("▪"));
    }

    // Phase: ordered_list_items_number_per_level.
    {
        let mut state = ui::State::new();
        with_doc(&mut state, |dm| {
            let mut prev: Option<String> = None;
            for (txt, lvl) in [
                ("one", 0i64),
                ("two", 0),
                ("two-a", 1),
                ("two-b", 1),
                ("three", 0),
            ] {
                let id = dm.insert_block(prev.as_deref(), "list_item");
                dm.splice_text(&id, 0, 0, txt);
                dm.set_attr(&id, "ordered", true);
                if lvl > 0 {
                    dm.set_attr(&id, "level", lvl);
                }
                prev = Some(id);
            }
        });
        let wp = WordProcessorWidget::new();
        let mut framed = mount(state, wp, 1000, 900);
        let (wp, _) = Framed::split(&mut framed);
        let num = |t: &str| {
            wp.laid
                .iter()
                .find(|l| l.text == t)
                .and_then(|l| l.bullet.clone())
        };
        assert_eq!(num("one").as_deref(), Some("1."));
        assert_eq!(num("two").as_deref(), Some("2."));
        // The nested ordered level restarts its own counter.
        assert_eq!(num("two-a").as_deref(), Some("1."));
        assert_eq!(num("two-b").as_deref(), Some("2."));
        // Returning to level 0 continues the outer numbering.
        assert_eq!(num("three").as_deref(), Some("3."));
    }

    // Phase: multi_digit_list_markers_grow_away_from_paragraph_text.
    {
        let text_x = 180.0;
        let one = list_marker_x(text_x, 13.0);
        let two = list_marker_x(text_x, 21.0);
        assert!(two < one, "the extra digit is allocated to the left gutter");
        assert_eq!(one + 13.0 + LIST_MARKER_TEXT_GAP, text_x);
        assert_eq!(two + 21.0 + LIST_MARKER_TEXT_GAP, text_x);
    }
}

/// A long document paginates across discrete pages, a single paragraph splits at a line boundary across
/// pages, and a manual page break starts its block on a fresh page.
#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one scenario test: each phase is a former standalone test kept verbatim as a scoped block"
)]
fn long_documents_paginate_across_discrete_pages() {
    // Phase: long_document_paginates_across_discrete_pages.
    {
        // Geometry mirrors `draw`: a fresh widget keeps the default uniform 72px
        // margins, find bar closed (find_h = 0), so canvas top = the toolbar's
        // actual (possibly multi-row) height. The toolbar wraps on a narrow
        // window, so derive `page_top` from the same `toolbar_height` the
        // production `draw` uses rather than hard-coding a single-row height.
        let mar = 72.0;
        let letter_h = MAX_PAGE_W * LETTER_RATIO;
        let page_pitch = letter_h + PAGE_GAP;

        let mut state = ui::State::new();
        let mut wp = WordProcessorWidget::new();
        wp.sync(&mut state.doc, &state.root);
        // Enough paragraphs to overflow a single US-Letter page.
        for i in 0..120 {
            wp.model_mut().insert_str(
                &mut state.doc,
                &format!("Paragraph number {i} carries enough words to fill a line."),
            );
            wp.model_mut().enter(&mut state.doc);
        }
        let mut framed = mount(state, wp, 1000, 700);
        let (wp, _) = Framed::split(&mut framed);

        // The canvas top sits below the (wrapped) toolbar AND the ruler band
        // (on by default) — the real offset the pagination cursor starts from.
        let ruler_h = if wp.view.ruler_visible { RULER_H } else { 0.0 };
        let page_top = wp.toolbar_height(1000.0) + ruler_h + PAGE_TOP;
        let pct = |i: usize| page_top + num::f64_of_usize(i) * page_pitch + mar; // page content top
        let pcb = |i: usize| page_top + num::f64_of_usize(i) * page_pitch + letter_h - mar; // bottom margin
        let page_of = |y: f64| num::i64_of_f64(((y - page_top) / page_pitch).floor());

        // The content spans more than one page.
        let max_page = wp.laid.iter().map(|l| page_of(l.y_top)).max().unwrap();
        assert!(
            max_page >= 1,
            "a long document must occupy more than one page (got {max_page})"
        );

        // No laid block crosses a page boundary: each sits wholly within its page's
        // text band, so a page-end never bisects a line of text.
        for l in &wp.laid {
            let i = num::usize_of_i64(page_of(l.y_top).max(0));
            assert!(
                l.y_top >= pct(i) - 1.0,
                "block starts at/below its page's top margin"
            );
            assert!(
                l.y_top + l.height <= pcb(i) + 1.0,
                "block ends above its page's bottom margin (no straddle)"
            );
        }

        // The first block that lands on page 2 starts exactly at the page-2 top
        // margin, and the block before it (in document order) is still on page 1.
        let idx = wp
            .laid
            .iter()
            .position(|l| page_of(l.y_top) == 1)
            .expect("some block lands on page 2");
        assert!(idx > 0, "page 2 is preceded by page-1 content");
        assert!(
            (wp.laid[idx].y_top - pct(1)).abs() < 1.0,
            "the first page-2 block hugs the page-2 top margin"
        );
        assert_eq!(
            page_of(wp.laid[idx - 1].y_top),
            0,
            "the block before it is on page 1"
        );
    }

    // Phase: single_paragraph_splits_at_line_boundary_across_pages.
    // A SINGLE paragraph taller than one page is split at a line boundary across
    // pages (intra-block pagination): the lines that fit stay on page 1, the rest
    // flow to page 2. The block stays one logical block (one id, one cache entry);
    // only its visual rendering is split. Asserts on the production layout
    // geometry (`wp.laid` slices), not a helper.
    {
        let mar = 72.0;
        let letter_h = MAX_PAGE_W * LETTER_RATIO;
        let page_pitch = letter_h + PAGE_GAP;

        let mut state = ui::State::new();
        let mut wp = WordProcessorWidget::new();
        wp.sync(&mut state.doc, &state.root);
        // ONE paragraph with enough words to wrap to well over a page of lines.
        let mut para = String::new();
        // 400 words can fit within the page with the host font metrics.
        for i in 0..800 {
            write!(para, "word{i} ").unwrap();
        }
        let para = para.trim_end().to_string();
        wp.model_mut().insert_str(&mut state.doc, &para);
        let mut framed = mount(state, wp, 1000, 700);
        let (wp, _) = Framed::split(&mut framed);

        let ruler_h = if wp.view.ruler_visible { RULER_H } else { 0.0 };
        let page_top = wp.toolbar_height(1000.0) + ruler_h + PAGE_TOP;
        let pct = |i: usize| page_top + num::f64_of_usize(i) * page_pitch + mar; // page content top
        let pcb = |i: usize| page_top + num::f64_of_usize(i) * page_pitch + letter_h - mar; // bottom margin
        let page_of = |y: f64| num::i64_of_f64(((y - page_top) / page_pitch).floor());

        // The one paragraph produced multiple laid slices, all sharing its id.
        let bid = wp.blocks[0].id.clone();
        let slices: Vec<&Laid> = wp.laid.iter().filter(|l| l.id == bid).collect();
        assert!(
            slices.len() >= 2,
            "a too-tall paragraph splits into >=2 page slices (got {}, shaped height {}, page content height {})",
            slices.len(),
            wp.layout_cache[&bid].1.height(),
            letter_h - 2.0 * mar
        );
        // One logical block, one layout cache entry — only the rendering is split.
        assert_eq!(
            wp.layout_cache.len(),
            1,
            "the split block keeps exactly one cache entry"
        );
        assert!(
            wp.layout_cache.contains_key(&bid),
            "the cache entry is keyed by the single block id"
        );

        // The first slice is on page 1 and its last line ends ABOVE the page-1
        // bottom text margin — the page boundary never bisects a line.
        let s0 = slices[0];
        assert_eq!(page_of(s0.y_top), 0, "the first slice is on page 1");
        assert!(
            s0.y_top + s0.height <= pcb(0) + 1.0,
            "page-1 slice ends above the bottom margin (no straddle): bottom={} margin={}",
            s0.y_top + s0.height,
            pcb(0)
        );

        // The second slice's first carried line starts at the page-2 TOP margin.
        let s1 = slices[1];
        assert_eq!(page_of(s1.y_top), 1, "the carried-over slice is on page 2");
        assert!(
            (s1.y_top - pct(1)).abs() < 1.0,
            "the first carried line hugs the page-2 top margin: top={} margin={}",
            s1.y_top,
            pct(1)
        );

        // The carried slice maps the layout origin ABOVE its band (so an internal
        // line N draws at `layout_y + line_top`), and clips to its own band.
        assert!(
            s1.layout_y < s1.y_top,
            "the carried slice's layout origin sits above its page-2 band"
        );
        assert!(
            s1.clip.is_some(),
            "a split slice clips its drawing to its band"
        );
        assert!(
            s0.clip.is_some(),
            "the first slice of a split block also clips its band"
        );
    }

    // Phase: manual_page_break_starts_block_on_a_fresh_page.
    {
        // Geometry mirrors `draw` (default 72px margins, find bar closed).
        let mar = 72.0;
        let letter_h = MAX_PAGE_W * LETTER_RATIO;
        let page_pitch = letter_h + PAGE_GAP;

        let mut state = ui::State::new();
        let mut wp = WordProcessorWidget::new();
        wp.sync(&mut state.doc, &state.root);
        // Two short paragraphs that both fit on page 1, then force a break so the
        // third starts on a fresh page even though it would otherwise fit.
        wp.model_mut()
            .insert_str(&mut state.doc, "First paragraph on page one.");
        wp.model_mut().enter(&mut state.doc);
        wp.model_mut()
            .insert_str(&mut state.doc, "Second paragraph still on page one.");
        // Ctrl+Enter behaviour: split + mark the new block to break before.
        wp.model_mut().insert_page_break(&mut state.doc);
        wp.model_mut()
            .insert_str(&mut state.doc, "This paragraph is forced onto page two.");
        let mut framed = mount(state, wp, 1000, 700);
        let (wp, _) = Framed::split(&mut framed);

        let ruler_h = if wp.view.ruler_visible { RULER_H } else { 0.0 };
        let page_top = wp.toolbar_height(1000.0) + ruler_h + PAGE_TOP;
        let pct = |i: usize| page_top + num::f64_of_usize(i) * page_pitch + mar; // page content top
        let page_of = |y: f64| num::i64_of_f64(((y - page_top) / page_pitch).floor());

        let broken = wp
            .laid
            .iter()
            .find(|l| l.text.starts_with("This paragraph is forced"))
            .expect("the broken block is laid out");
        assert_eq!(
            page_of(broken.y_top),
            1,
            "the page-break block lands on page 2"
        );
        assert!(
            (broken.y_top - pct(1)).abs() < 1.0,
            "it hugs the page-2 top margin (laid at a page top)"
        );

        // The two earlier paragraphs are still on page 1 (the break was manual, not
        // an overflow): proving the attr — not a full page of text — caused it.
        let first = wp
            .laid
            .iter()
            .find(|l| l.text.starts_with("First paragraph"))
            .unwrap();
        let second = wp
            .laid
            .iter()
            .find(|l| l.text.starts_with("Second paragraph"))
            .unwrap();
        assert_eq!(
            page_of(first.y_top),
            0,
            "the first paragraph stays on page 1"
        );
        assert_eq!(
            page_of(second.y_top),
            0,
            "the second paragraph stays on page 1"
        );
    }
}

/// Caret hit-testing on a line that MOVED to page 2 by intra-block pagination
/// returns the right offset: a pointer over the carried line resolves to the
/// byte that the production layout places at that on-screen position. Drives
/// the production `caret_from_pointer` path, asserts on its result.
#[test]
fn caret_hit_on_a_carried_line_returns_the_right_offset() {
    let mut state = ui::State::new();
    let mut wp = WordProcessorWidget::new();
    wp.sync(&mut state.doc, &state.root);
    let mut para = String::new();
    // 400 words can fit within the page with the host font metrics.
    for i in 0..800 {
        write!(para, "word{i} ").unwrap();
    }
    let para = para.trim_end().to_string();
    wp.model_mut().insert_str(&mut state.doc, &para);
    let mut framed = mount(state, wp, 1000, 700);
    let (wp, _) = Framed::split(&mut framed);

    let bid = wp.blocks[0].id.clone();
    // The carried-over slice (the first one on page 2 for this block).
    let s1 = wp
        .laid
        .iter()
        .filter(|l| l.id == bid)
        .find(|l| l.page == 1)
        .cloned()
        .map(|l| (l.text_x, l.layout_y, l.y_top, l.height))
        .expect("the paragraph spills onto page 2");
    let (text_x, layout_y, band_top, band_h) = s1;

    // Pick a byte whose shaped position falls inside the page-2 band — i.e. a
    // character that was carried over. Walk the layout's caret geometry to find
    // the first byte whose on-screen y lands in slice 2's band.
    let (key_byte, gx, gy) = {
        let (_, layout) = wp.layout_cache.get(&bid).expect("cached layout");
        let mut found = None;
        let mut byte = 0usize;
        while byte <= para.len() {
            let (cx, cy, _ch) = layout.caret_geometry(byte);
            let screen_y = layout_y + f64::from(cy);
            if screen_y >= band_top - 0.5 && screen_y < band_top + band_h {
                // A non-leading byte on this line (so click x is unambiguous).
                found = Some((byte, f64::from(cx), f64::from(cy)));
                break;
            }
            // Advance to the next char boundary.
            byte += 1;
            while byte < para.len() && !para.is_char_boundary(byte) {
                byte += 1;
            }
        }
        found.expect("some byte renders on the page-2 carried band")
    };
    let expected_char = byte_to_char(&para, key_byte);

    // Click a few px to the right of that glyph's left edge, on its line — a
    // point unambiguously inside one character — using the slice's on-screen
    // mapping (`text_x` for x, `layout_y + glyph_top` for y).
    let click = Point::new(text_x + gx + 2.0, layout_y + gy + 4.0);
    // The click must really be inside the page-2 band (sanity on the geometry).
    assert!(
        click.y >= band_top - 0.5 && click.y < band_top + band_h,
        "click lands in the carried band"
    );
    wp.caret_from_pointer(click, false);

    // The resolved caret is in the same (single) block, at the carried char.
    assert_eq!(
        wp.inner.caret.block, bid,
        "hit resolves to the one logical block"
    );
    let got = num::i64_of_usize(wp.inner.caret.offset);
    assert!(
        (got - num::i64_of_usize(expected_char)).abs() <= 1,
        "carried-line hit resolves to the right offset: got {got}, expected ~{expected_char}"
    );
}

/// The footer page-number field resolves per page and page margins persist across reload.
#[test]
fn page_fields_and_margins_resolve_per_page_and_persist() {
    // Phase: footer_page_number_field_resolves_per_page.
    {
        // A footer "Page <field>" must render "Page 1" on page 1 and "Page 2" on
        // page 2 — the field placeholder substitutes the rendered page number.
        let mut state = ui::State::new();
        let mut wp = WordProcessorWidget::new();
        wp.sync(&mut state.doc, &state.root);
        // Build a footer with a page-number field via the production model API.
        let c = wp.model().containers.clone().unwrap();
        let mut ids = tilo_doc::IdGen::new(state.doc.get_actor().to_hex_string());
        {
            let mut dm = tilo_doc::DocMut {
                doc: &mut state.doc,
                c: &c,
                ids: &mut ids,
            };
            let f = dm.insert_block_in(tilo_doc::model::Region::Footer, None, "paragraph");
            dm.splice_text_in(tilo_doc::model::Region::Footer, &f, 0, 0, "Page ");
            dm.insert_page_number_field(tilo_doc::model::Region::Footer, &f, 5);
        }
        // Body long enough to span two pages.
        for i in 0..120 {
            wp.model_mut().insert_str(
                &mut state.doc,
                &format!("Paragraph number {i} carries enough words to fill a line."),
            );
            wp.model_mut().enter(&mut state.doc);
        }
        let mut framed = mount(state, wp, 1000, 700);
        let (wp, _) = Framed::split(&mut framed);

        // The footer is loaded from the snapshot and spans >1 page.
        assert_eq!(
            wp.footer.len(),
            1,
            "the footer block is materialized into the widget"
        );
        let ruler_h = if wp.view.ruler_visible { RULER_H } else { 0.0 };
        let max_page = wp
            .laid
            .iter()
            .map(|l| {
                ((l.y_top + wp.scroll_y - (wp.toolbar_height(1000.0) + ruler_h + PAGE_TOP))
                    / (MAX_PAGE_W * LETTER_RATIO + PAGE_GAP))
                    .floor() as i64
            })
            .max()
            .unwrap();
        assert!(
            max_page >= 1,
            "the document spans more than one page (got {max_page})"
        );

        // The production substitution path renders the page number per page: the
        // joined run text for the footer block is "Page 1" on page 1, "Page 2" on
        // page 2 — the placeholder is gone and the digits are present.
        let join = |page_no: usize| {
            running_head_runs(&wp.footer[0], page_no)
                .iter()
                .map(|r| r.text.clone())
                .collect::<String>()
        };
        assert_eq!(join(1), "Page 1", "page 1 footer resolves to 'Page 1'");
        assert_eq!(join(2), "Page 2", "page 2 footer resolves to 'Page 2'");
        assert!(
            !join(2).contains(tilo_doc::schema::FIELD_PLACEHOLDER),
            "the field placeholder is substituted, not rendered literally"
        );
    }

    // Phase: page_margins_persist_across_reload.
    {
        let mut state = ui::State::new();
        let mut wp = WordProcessorWidget::new();
        wp.sync(&mut state.doc, &state.root);
        // A document with no recorded page setup uses the default margins.
        assert!(
            (wp.margins.left - 72.0).abs() < 1e-6,
            "fresh doc uses default margins"
        );

        // Edit margins (as a margin-guide drag would) and persist via the same
        // production path the drag-release handler uses.
        wp.margins = Edges {
            top: 33.0,
            right: 44.0,
            bottom: 55.0,
            left: 66.0,
        };
        wp.persist_margins(&mut state.doc);

        // A fresh widget bound to the *same document* reloads the persisted page
        // setup on sync — proving margins now live in the document, not the widget.
        let mut wp2 = WordProcessorWidget::new();
        wp2.sync(&mut state.doc, &state.root);
        assert!(
            (wp2.margins.top - 33.0).abs() < 1e-6,
            "top margin survived reload"
        );
        assert!(
            (wp2.margins.right - 44.0).abs() < 1e-6,
            "right margin survived reload"
        );
        assert!(
            (wp2.margins.bottom - 55.0).abs() < 1e-6,
            "bottom margin survived reload"
        );
        assert!(
            (wp2.margins.left - 66.0).abs() < 1e-6,
            "left margin survived reload"
        );
    }
}

/// Header / footer materialized text via the production snapshot path.
fn header_text(wp: &WordProcessorWidget, state: &ui::State) -> Vec<String> {
    let c = wp.model().containers.clone().unwrap();
    tilo_doc::model::materialize_region(&state.doc, &c, tilo_doc::model::Region::Header)
        .into_iter()
        .map(|b| b.text)
        .collect()
}
fn body_text(wp: &WordProcessorWidget, state: &ui::State) -> Vec<String> {
    let c = wp.model().containers.clone().unwrap();
    tilo_doc::model::materialize(&state.doc, &c)
        .blocks
        .into_iter()
        .map(|b| b.text)
        .collect()
}

/// Drive a real named-key press (e.g. Escape) through `handle_event`.
fn press_named(wp: &mut WordProcessorWidget, state: &mut ui::State, key: NamedKey) {
    press_named_with_modifiers(wp, state, key, Modifiers::default());
}

fn press_named_with_modifiers(
    wp: &mut WordProcessorWidget,
    state: &mut ui::State,
    key: NamedKey,
    mods: Modifiers,
) {
    use winit::keyboard::{NativeKeyCode, PhysicalKey};
    let taffy: TaffyTree<()> = TaffyTree::new();
    let node = NodeId::new(0);
    let mut handled = false;
    let mut focus: Option<NodeId> = None;
    let mut ev: KeyEvent = unsafe { std::mem::zeroed() };
    ev.physical_key = PhysicalKey::Unidentified(NativeKeyCode::Unidentified);
    ev.logical_key = Key::Named(key);
    ev.text = None;
    ev.location = winit::keyboard::KeyLocation::Standard;
    ev.state = ElementState::Pressed;
    ev.repeat = false;
    wp.handle_event(
        &WindowEvent::KeyboardInput {
            device_id: unsafe { std::mem::zeroed() },
            event: ev,
            is_synthetic: false,
        },
        &mut ui::widget::EventCtx {
            scale_factor: 1.0,
            modifiers: &mods,
            state: &mut state.doc,
            path: &state.root,
            handled: &mut handled,
            focus_target: &mut focus,
            node_id: node,
            taffy: &taffy,
        },
    );
}

fn pinch(
    wp: &mut WordProcessorWidget,
    state: &mut ui::State,
    delta: f64,
    phase: winit::event::TouchPhase,
) -> bool {
    let mut handled = false;
    wp.handle_event(
        &WindowEvent::PinchGesture {
            device_id: unsafe { std::mem::zeroed() },
            delta,
            phase,
        },
        &mut ui::widget::EventCtx {
            scale_factor: 1.0,
            modifiers: &Modifiers::default(),
            state: &mut state.doc,
            path: &state.root,
            handled: &mut handled,
            focus_target: &mut None,
            node_id: NodeId::new(0),
            taffy: &TaffyTree::new(),
        },
    );
    handled
}

/// Mouse selection across wrapped line boundaries keeps its column and between paragraphs stays near
/// the pointer.
#[test]
fn mouse_selection_follows_the_pointer_across_lines_and_paragraphs() {
    // Phase: mouse_selection_across_wrapped_line_boundaries_keeps_its_column.
    {
        let (mut state, mut wp) = bound();
        wp.inner.insert_str(
            &mut state.doc,
            &"A paragraph with several words across visual lines. ".repeat(12),
        );
        let mut framed = mount(state, wp, 1000, 700);
        let (wp, _) = Framed::split(&mut framed);
        let laid = wp.laid[0].clone();
        let bands = wp.layout_cache[&laid.id].1.logical_line_bands(0);
        assert!(bands.len() > 2);
        for (top, _) in bands.iter().skip(1).take(3) {
            for delta in [-0.1, 0.1] {
                wp.caret_from_pointer(
                    Point::new(laid.text_x + 100.0, laid.layout_y + f64::from(*top) + delta),
                    true,
                );
                assert_eq!(wp.inner.caret.block, laid.id);
                let byte = char_to_byte(&laid.text, wp.inner.caret.offset);
                let (x, _, _) = wp.layout_cache[&laid.id].1.caret_geometry(byte);
                assert!(
                    (f64::from(x) - 100.0).abs() < 15.0,
                    "crossing a visual line boundary must retain the pointed column, got {x}"
                );
            }
        }
    }

    // Phase: mouse_selection_between_paragraphs_stays_near_the_pointer.
    {
        let (mut state, mut wp) = bound();
        for i in 0..3 {
            if i != 0 {
                wp.inner.enter(&mut state.doc);
            }
            wp.inner.insert_str(&mut state.doc, "abcdefghijk");
        }
        let mut framed = mount(state, wp, 1000, 700);
        for zoom in [0.5, 1.0, 2.0] {
            framed.set_zoom(zoom);
            let (wp, _) = pipeline_draw(&mut framed, 1000, 700);
            let first = wp.laid[0].clone();
            let second = wp.laid[1].clone();
            let gap_start = first.y_top + first.height;
            assert!(
                second.y_top > gap_start,
                "fixture needs real paragraph spacing"
            );
            let x = first.text_x + f64::from(wp.layout_cache[&first.id].1.caret_geometry(4).0);
            for (fraction, block) in [(0.25, &first.id), (0.75, &second.id)] {
                wp.inner.set_caret(Caret {
                    block: first.id.clone(),
                    offset: 1,
                });
                wp.inner.clear_anchor();
                let point = Point::new(x, gap_start + (second.y_top - gap_start) * fraction);
                // Exercise the same screen/content transform as mouse dragging.
                let screen = wp.content_transform(Affine::IDENTITY) * point;
                wp.drag.press(screen, false);
                let mut handled = false;
                wp.handle_cursor_moved(
                    screen.x,
                    screen.y,
                    1.0,
                    wp.toolbar_height(1000.0),
                    &mut handled,
                );
                wp.drag.release();
                assert!(handled);
                assert_eq!(
                    &wp.inner.caret.block, block,
                    "gap hit must choose a neighbouring paragraph at {zoom}x"
                );
                assert_eq!(
                    wp.inner.caret.offset, 4,
                    "gap hit must preserve horizontal position"
                );
                assert_eq!(wp.inner.selection_anchor.as_ref().unwrap().offset, 1);
            }
        }
    }
}

/// Trackpad pinch zooms smoothly keeping the reading position, and respects overlays, bounds and
/// invalid deltas.
#[test]
fn trackpad_pinch_zooms_smoothly_within_its_bounds() {
    // Phase: trackpad_pinch_zooms_smoothly_and_keeps_the_reading_position.
    {
        use winit::event::TouchPhase;
        let (state, mut wp) = populated();
        wp.set_zoom(1.0);
        wp.scroll_y = 100.0;
        let mut framed = mount(state, wp, 1000, 700);
        let (wp, state) = Framed::split(&mut framed);
        wp.pointer = Point::new(500.0, 300.0);
        let before = wp.scroll_y + (wp.pointer.y - wp.zoom.anchor().y) / wp.zoom.factor();
        let heads = state.doc.get_heads();
        assert!(pinch(wp, state, 0.025, TouchPhase::Moved));
        assert!((wp.zoom.factor() - 1.025).abs() < 1e-9);
        let (wp, state) = pipeline_draw(&mut framed, 1000, 700);
        let after = wp.scroll_y + (wp.pointer.y - wp.zoom.anchor().y) / wp.zoom.factor();
        assert!((before - after).abs() < 1e-6);
        assert!(!wp.view.auto_fit_page);
        assert_eq!(state.doc.get_heads(), heads);
        assert_ne!(wp.save_status, DocumentSaveStatus::Modified);
        assert!(pinch(wp, state, -0.05, TouchPhase::Moved));
        assert!(wp.zoom.factor() < 1.025);
    }

    // Phase: trackpad_pinch_respects_overlays_bounds_and_invalid_deltas.
    {
        use winit::event::TouchPhase;
        let (state, mut wp) = populated();
        wp.set_zoom(1.0);
        let mut framed = mount(state, wp, 1000, 700);
        let (wp, state) = Framed::split(&mut framed);
        wp.pointer = Point::new(500.0, 300.0);
        for delta in [f64::NAN, f64::INFINITY] {
            pinch(wp, state, delta, TouchPhase::Moved);
            assert_eq!(wp.zoom.factor(), 1.0);
        }
        pinch(wp, state, 0.5, TouchPhase::Cancelled);
        assert_eq!(wp.zoom.factor(), 1.0);
        wp.open_template_picker();
        assert!(pinch(wp, state, 0.5, TouchPhase::Moved));
        assert_eq!(wp.zoom.factor(), 1.0);
        wp.template_picker.close();
        wp.pointer = Point::new(20.0, 20.0);
        assert!(!pinch(wp, state, 0.5, TouchPhase::Moved));
        assert_eq!(wp.zoom.factor(), 1.0);
        wp.pointer = Point::new(500.0, 300.0);
        pinch(wp, state, 20.0, TouchPhase::Moved);
        assert_eq!(wp.zoom.factor(), ZOOM_MAX);
        pinch(wp, state, -2.0, TouchPhase::Moved);
        assert_eq!(wp.zoom.factor(), ZOOM_MIN);
    }
}

/// Native text shortcuts select words and lines after menu dismissal, and Option-Right crosses
/// trailing spaces forward and keeps adjacent Unicode words.
#[test]
fn native_text_shortcuts_move_by_words_and_lines() {
    // Phase: native_text_shortcuts_select_words_and_lines_after_menu_dismissal.
    {
        use winit::keyboard::ModifiersState;
        for (mods, expected) in [
            (ModifiersState::ALT | ModifiersState::SHIFT, "café"),
            (ModifiersState::SUPER | ModifiersState::SHIFT, "Hello café"),
            (ModifiersState::CONTROL | ModifiersState::SHIFT, "café"),
        ] {
            let (mut state, mut wp) = bound();
            wp.inner.insert_str(&mut state.doc, "Hello café");
            let mut framed = mount(state, wp, 1000, 700);
            let (wp, state) = Framed::split(&mut framed);
            wp.lifecycle.focused = true;
            wp.open_template_picker();
            press_named(wp, state, NamedKey::Escape);
            press_named_with_modifiers(wp, state, NamedKey::ArrowLeft, mods.into());
            assert_eq!(wp.inner.selection_text(&state.doc), expected);
            assert_eq!(wp.inner.current_text(&state.doc), "Hello café");
            assert!(
                !(wp.save_status == DocumentSaveStatus::Modified),
                "selection must not modify the document"
            );
        }
    }

    // Phase: native_text_shortcuts_option_right_crosses_trailing_spaces_forward.
    {
        use winit::keyboard::ModifiersState;
        for (text, selected) in [("hello   ", "  "), ("hello!!!", "!!")] {
            let (mut state, mut wp) = bound();
            wp.inner.insert_str(&mut state.doc, text);
            let mut framed = mount(state, wp, 1000, 700);
            let (wp, state) = Framed::split(&mut framed);
            wp.inner.caret.offset = 6;
            wp.lifecycle.focused = true;
            press_named_with_modifiers(
                wp,
                state,
                NamedKey::ArrowRight,
                (ModifiersState::ALT | ModifiersState::SHIFT).into(),
            );
            assert_eq!(wp.inner.caret.offset, 8);
            assert_eq!(wp.inner.selection_text(&state.doc), selected);
        }
    }

    // Phase: native_text_shortcuts_option_right_keeps_adjacent_unicode_words.
    {
        use winit::keyboard::ModifiersState;
        let (mut state, mut wp) = bound();
        wp.inner.insert_str(&mut state.doc, "東京京都大阪");
        let mut framed = mount(state, wp, 1000, 700);
        let (wp, state) = Framed::split(&mut framed);
        wp.inner.caret.offset = 2;
        wp.lifecycle.focused = true;
        press_named_with_modifiers(
            wp,
            state,
            NamedKey::ArrowRight,
            (ModifiersState::ALT | ModifiersState::SHIFT).into(),
        );
        assert_eq!(wp.inner.selection_text(&state.doc), "京都");
    }
}

/// Native text shortcuts select then delete through the host, and Command-Right stops at the visual
/// line in every region.
#[test]
fn native_text_shortcuts_delete_and_stop_at_visual_lines() {
    // Phase: native_text_shortcuts_select_then_delete_through_the_host.
    {
        for (modifier, expected) in [("alt", "Hello "), ("super", "")] {
            let (mut state, mut wp) = bound();
            wp.inner.insert_str(&mut state.doc, "Hello café");
            let containers = wp.inner.containers.clone().unwrap();
            wp.lifecycle.focused = true;
            let mut driver = ui::headless::Driver::new(state, Box::new(wp), (1000, 700));
            driver.set_modifiers(&[modifier.into(), "shift".into()]);
            driver.key("ArrowLeft");
            driver.set_modifiers(&[]);
            driver.key("Backspace");
            assert_eq!(
                tilo_doc::model::materialize(&driver.state_mut().doc, &containers).blocks[0].text,
                expected
            );
        }
    }

    // Phase: native_text_shortcuts_command_right_stops_at_the_visual_line_in_every_region.
    {
        use winit::keyboard::ModifiersState;
        for region in [Region::Body, Region::Header, Region::Footer] {
            let (mut state, mut wp) = bound();
            wp.inner.focus_region(&mut state.doc, region);
            let text = "A long line of text with several words. ".repeat(20);
            wp.inner.insert_str(&mut state.doc, &text);
            wp.inner.caret.offset = 2;
            wp.lifecycle.focused = true;
            let mut framed = mount(state, wp, 1000, 700);
            let (wp, state) = Framed::split(&mut framed);
            press_named_with_modifiers(
                wp,
                state,
                NamedKey::ArrowRight,
                (ModifiersState::SUPER | ModifiersState::SHIFT).into(),
            );
            assert!(
                wp.inner.caret.offset > 2 && wp.inner.caret.offset < text.chars().count(),
                "Command+Right must stop at a wrapped visual line, not the entire paragraph: {region:?}"
            );
            assert!(wp.inner.has_selection());
        }
    }
}

/// End-to-end through the **production** event path: click into the header
/// band → the edit context switches to Header → typed characters land in the
/// header container (asserted via `materialize_region`), not the body. Then a
/// click back in the body returns the context to Body, and typing edits the
/// body again.
#[test]
fn clicking_header_band_routes_typing_into_the_header() {
    let (mut state, mut wp) = bound();
    // Put some content in the body first (proves it is not what gets edited).
    wp.model_mut().insert_str(&mut state.doc, "body text");
    let mut framed = mount(state, wp, 1000, 700);
    let (wp, state) = Framed::split(&mut framed);
    assert_eq!(
        wp.model().region(),
        Region::Body,
        "starts in the body context"
    );

    // The cached header-band rect is content space; map its centre to a screen
    // point the way the production pointer→content mapping inverts (identity at
    // zoom 1.0, but go through the helper to be exact).
    let hb = wp.head_band_rect;
    assert!(hb.height() > 0.0, "header band rect was cached by draw");
    let centre_content = Point::new(f64::midpoint(hb.x0, hb.x1), f64::midpoint(hb.y0, hb.y1));
    let screen = Point::new(
        wp.content_x_to_screen(centre_content.x),
        wp.zoom.anchor().y + (centre_content.y - wp.zoom.anchor().y) * wp.zoom.factor(),
    );

    left_click(wp, state, screen);
    assert_eq!(
        wp.model().region(),
        Region::Header,
        "clicking the header band focuses the Header context"
    );

    // Type into the header — production keyboard path.
    for ch in "My Title".chars() {
        type_char(wp, state, ch);
    }
    let (wp, state) = pipeline_draw(&mut framed, 1000, 700);

    // Assert on the materialized HEADER snapshot (the production path).
    assert_eq!(
        header_text(wp, state),
        vec!["My Title".to_string()],
        "typed text landed in the header container"
    );
    assert_eq!(
        body_text(wp, state),
        vec!["body text".to_string()],
        "the body is untouched"
    );
    // The band laid out the header block, so a caret can render in it.
    assert!(
        wp.region_laid.iter().any(|r| r.text == "My Title"),
        "the header band is laid out for the active context"
    );

    // Escape returns to the body; typing then edits the body again.
    press_named(wp, state, NamedKey::Escape);
    assert_eq!(
        wp.model().region(),
        Region::Body,
        "Escape exits header editing"
    );
    let (wp, state) = pipeline_draw(&mut framed, 1000, 700);
    wp.model_mut().move_end(&state.doc);
    for ch in "!".chars() {
        type_char(wp, state, ch);
    }
    assert_eq!(
        body_text(wp, state),
        vec!["body text!".to_string()],
        "after Escape, typing edits the body"
    );
    assert_eq!(
        header_text(wp, state),
        vec!["My Title".to_string()],
        "the header keeps its content"
    );
}

/// Rasterize one frame while the header is the active edit context and write a
/// PNG (so the header-edit affordance — band wash + dashed outline + caret —
/// can be eyeballed). Renders through the real five-pass draw; asserts a
/// non-trivial image was produced. Output path is overridable via
/// `$TILO_WP_HEADER_SHOT` for manual inspection.
#[test]
fn header_edit_mode_renders_a_frame() {
    let (mut state, mut wp) = bound();
    wp.model_mut().insert_str(
        &mut state.doc,
        "The body paragraph recedes while the header is edited.",
    );
    // Focus the header and give it content through the production model API.
    wp.model_mut().focus_region(&mut state.doc, Region::Header);
    for ch in "Draft — Confidential".chars() {
        wp.model_mut().insert_str(&mut state.doc, &ch.to_string());
    }
    let (w, h) = (1000u32, 700u32);
    let wp = mount(state, wp, w, h);
    // The header band laid out for the active context (so its caret + chrome
    // are part of this frame).
    assert!(
        wp.region_laid.iter().any(|r| r.text.contains("Draft")),
        "header band laid for the active context"
    );

    let out = std::env::var("TILO_WP_HEADER_SHOT").unwrap_or_else(|_| {
        format!(
            "{}/header-edit-mode-{}.png",
            std::env::temp_dir().display(),
            std::process::id()
        )
    });
    ui::offscreen::OffscreenRenderer::new(w, h).render_png(
        Framed::harness(&wp).scene(),
        Color::from_rgb8(0x20, 0x22, 0x28),
        std::path::Path::new(&out),
    );
    let meta = std::fs::metadata(&out).expect("a PNG frame was written");
    assert!(
        meta.len() > 2000,
        "the rendered frame is a non-trivial PNG ({} bytes at {out})",
        meta.len()
    );
}

/// An inserted image lays out and draws, and an explicit image size is honoured.
#[test]
fn inserted_images_lay_out_draw_and_honour_their_size() {
    // Phase: inserted_image_lays_out_and_draws.
    // Inserting an image via the model and running the full draw pipeline lays
    // out an `image` block and does not panic (decode → layout → draw).
    {
        let mut state = ui::State::new();
        let mut wp = WordProcessorWidget::new();
        wp.sync(&mut state.doc, &state.root);
        wp.model_mut().insert_str(&mut state.doc, "caption above");
        wp.model_mut().insert_image_bytes(&mut state.doc, TEST_PNG);
        // Re-sync so the widget's block/blob caches see the new image block.
        wp.sync(&mut state.doc, &state.root);
        let mut framed = mount(state, wp, 1000, 700);
        let (wp, _) = Framed::split(&mut framed);
        assert!(
            wp.laid.iter().any(|l| l.kind == "image"),
            "an image block was laid out"
        );
    }

    // Phase: explicit_image_size_is_honored.
    // An image block carrying explicit `width`/`height` attrs renders at that size
    // (the requested width, clamped to the content column) — not auto-fit to the
    // intrinsic decoded size. Asserts on the production `Laid` geometry.
    {
        let mut state = ui::State::new();
        with_doc(&mut state, |dm| {
            let hash = tilo_content::hash_bytes(TEST_PNG);
            dm.reference_content(&hash, &tilo_content::sniff(TEST_PNG));
            let p = dm.insert_block(None, "paragraph");
            dm.splice_text(&p, 0, 0, "caption");
            let img = dm.insert_block(Some(&p), "image");
            dm.set_attr(&img, "blob", hash.as_str());
            // Request a 200×120 display size — far larger than the 2×3 intrinsic.
            dm.set_attr(&img, "width", 200_i64);
            dm.set_attr(&img, "height", 120_i64);
        });
        let wp = WordProcessorWidget::new();
        wp.model().content_store().put(TEST_PNG).unwrap();
        let mut framed = mount(state, wp, 1100, 900);
        let (wp, _) = Framed::split(&mut framed);

        let img = wp
            .laid
            .iter()
            .find(|l| l.kind == "image")
            .expect("image laid out");
        assert!(
            (img.width - 200.0).abs() < 0.5,
            "image uses its explicit width, got {}",
            img.width
        );
        assert!(
            (img.height - 120.0).abs() < 0.5,
            "image uses its explicit height, got {}",
            img.height
        );
    }
}

/// A right-floated image narrows the following paragraph and a left-floated one offsets it right.
#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one scenario test: each phase is a former standalone test kept verbatim as a scoped block"
)]
fn floated_images_narrow_and_offset_the_following_paragraph() {
    // Phase: right_floated_image_narrows_following_paragraph.
    // A `paragraph`-anchored, `wrap=right` floated image pins to the RIGHT edge of
    // the content column, and the paragraph that follows is laid at a reduced width
    // (beside the image) keeping its left x-origin. Asserts on production geometry.
    {
        let mut state = ui::State::new();
        with_doc(&mut state, |dm| {
            // A reference full-width paragraph (above the float) to read the content
            // column geometry from, then the floated image, then the wrapped text.
            let r = dm.insert_block(None, "paragraph");
            dm.splice_text(&r, 0, 0, "reference paragraph at full content width");
            let hash = tilo_content::hash_bytes(TEST_PNG);
            dm.reference_content(&hash, &tilo_content::sniff(TEST_PNG));
            let img = dm.insert_block(Some(&r), "image");
            dm.set_attr(&img, "blob", hash.as_str());
            dm.set_attr(&img, "anchor", "paragraph");
            dm.set_attr(&img, "wrap", "right");
            dm.set_attr(&img, "width", 200_i64);
            dm.set_attr(&img, "height", 160_i64);
            let beside = dm.insert_block(Some(&img), "paragraph");
            dm.splice_text(
                &beside,
                0,
                0,
                "this paragraph should flow beside the floated image to its left",
            );
        });
        let wp = WordProcessorWidget::new();
        wp.model().content_store().put(TEST_PNG).unwrap();
        let mut framed = mount(state, wp, 1100, 900);
        let (wp, _) = Framed::split(&mut framed);

        let reference = wp
            .laid
            .iter()
            .find(|l| l.text.starts_with("reference"))
            .expect("ref laid");
        let img = wp
            .laid
            .iter()
            .find(|l| l.kind == "image")
            .expect("image laid out");
        let beside = wp
            .laid
            .iter()
            .find(|l| l.text.starts_with("this paragraph"))
            .expect("wrapped para laid");

        // Content column: the reference paragraph spans it fully.
        let content_left = reference.text_x;
        let content_right = reference.text_x + reference.width;

        // The image sits at the RIGHT edge of the content column.
        let img_right = img.text_x + img.width;
        assert!(
            (img_right - content_right).abs() < 1.0,
            "right-floated image hugs the content right edge: img_right={img_right} content_right={content_right}"
        );
        assert!(
            img.text_x > content_left + 1.0,
            "the image is offset toward the right, not at the left edge"
        );

        // The wrapped paragraph keeps its LEFT origin (right float → text stays left)
        // and is narrowed to clear the image.
        assert!(
            (beside.text_x - content_left).abs() < 0.5,
            "right-float text keeps its left x-origin: {} vs {}",
            beside.text_x,
            content_left
        );
        assert!(
            beside.width < reference.width - 100.0,
            "wrapped paragraph is narrowed beside the image: beside={} reference={}",
            beside.width,
            reference.width
        );
        // Specifically: reduced by ~ image width + gutter.
        assert!(
            beside.width <= content_right - content_left - 200.0 + 0.5,
            "narrowed by at least the image width"
        );
    }

    // Phase: left_floated_image_offsets_following_paragraph_right.
    // A `wrap=left` floated image pins to the LEFT edge and pushes the following
    // paragraph's text origin to the RIGHT (beside the image), still narrowed.
    {
        let mut state = ui::State::new();
        with_doc(&mut state, |dm| {
            let r = dm.insert_block(None, "paragraph");
            dm.splice_text(&r, 0, 0, "reference paragraph at full content width");
            let hash = tilo_content::hash_bytes(TEST_PNG);
            dm.reference_content(&hash, &tilo_content::sniff(TEST_PNG));
            let img = dm.insert_block(Some(&r), "image");
            dm.set_attr(&img, "blob", hash.as_str());
            dm.set_attr(&img, "anchor", "paragraph");
            dm.set_attr(&img, "wrap", "left");
            dm.set_attr(&img, "width", 200_i64);
            dm.set_attr(&img, "height", 160_i64);
            let beside = dm.insert_block(Some(&img), "paragraph");
            dm.splice_text(
                &beside,
                0,
                0,
                "this paragraph should flow to the right of the floated image",
            );
        });
        let wp = WordProcessorWidget::new();
        wp.model().content_store().put(TEST_PNG).unwrap();
        let mut framed = mount(state, wp, 1100, 900);
        let (wp, _) = Framed::split(&mut framed);

        let reference = wp
            .laid
            .iter()
            .find(|l| l.text.starts_with("reference"))
            .expect("ref laid");
        let img = wp
            .laid
            .iter()
            .find(|l| l.kind == "image")
            .expect("image laid out");
        let beside = wp
            .laid
            .iter()
            .find(|l| l.text.starts_with("this paragraph"))
            .expect("wrapped para laid");

        let content_left = reference.text_x;
        // The image sits at the LEFT edge.
        assert!(
            (img.text_x - content_left).abs() < 1.0,
            "left-floated image hugs the content left edge"
        );
        // The wrapped paragraph's origin is pushed right (beside the image) and is narrowed.
        assert!(
            beside.text_x > content_left + 199.0,
            "left-float text is offset right past the image: {} vs {}",
            beside.text_x,
            content_left
        );
        assert!(
            beside.width < reference.width - 100.0,
            "wrapped paragraph is narrowed beside the image"
        );
        // The narrowed text still ends within the content column (no overflow off the right edge).
        assert!(
            beside.text_x + beside.width <= content_left + reference.width + 0.5,
            "narrowed text fits within the content column"
        );
    }
}

// ---- Zoom ---------------------------------------------------------------

/// Zoom scales the content transform about the anchor, a click maps to the same caret under zoom, and
/// zoom clamps and resets.
#[test]
fn zoom_scales_about_the_anchor_and_keeps_clicks_on_the_same_caret() {
    // Phase: zoom_scales_the_content_transform_about_the_anchor.
    {
        let mut state = ui::State::new();
        let mut wp = WordProcessorWidget::new();
        wp.sync(&mut state.doc, &state.root);
        wp.model_mut().insert_str(&mut state.doc, "zoom me");
        let mut framed = mount(state, wp, 1000, 700);
        let (wp, _) = Framed::split(&mut framed);

        // At 100% the content transform is the identity scale (no magnification).
        let base = Affine::IDENTITY;
        let id_ctf = wp.content_transform(base);
        assert!(
            (id_ctf.as_coeffs()[0] - 1.0).abs() < 1e-9,
            "100% zoom does not scale"
        );

        // Zoom in: the content transform now scales by `zoom` about the anchor,
        // so a point at the anchor is fixed and points below it move down.
        assert!(wp.set_zoom(2.0));
        let ctf = wp.content_transform(base);
        let coeffs = ctf.as_coeffs();
        assert!(
            (coeffs[0] - 2.0).abs() < 1e-9 && (coeffs[3] - 2.0).abs() < 1e-9,
            "zoom doubles the scale"
        );
        // The anchor maps to itself.
        let a = wp.zoom.anchor();
        let mapped_anchor = ctf * a;
        assert!(
            (mapped_anchor.x - a.x).abs() < 1e-6 && (mapped_anchor.y - a.y).abs() < 1e-6,
            "the anchor is fixed under zoom"
        );
        // A point one content-unit below the anchor lands two screen-units below.
        let p = Point::new(a.x, a.y + 100.0);
        let mp = ctf * p;
        assert!(
            (mp.y - (a.y + 200.0)).abs() < 1e-6,
            "content below the anchor is magnified downward"
        );
    }

    // Phase: click_maps_to_the_same_caret_under_zoom.
    {
        let mut state = ui::State::new();
        let mut wp = WordProcessorWidget::new();
        wp.sync(&mut state.doc, &state.root);
        wp.model_mut().insert_str(&mut state.doc, "first paragraph");
        wp.model_mut().enter(&mut state.doc);
        wp.model_mut()
            .insert_str(&mut state.doc, "second paragraph");
        let mut framed = mount(state, wp, 1000, 700);
        let (wp, _) = Framed::split(&mut framed);

        // A content-space point inside the second block (its `Laid` rect lives in
        // unscaled content coordinates regardless of zoom).
        let l2 = wp
            .laid
            .iter()
            .find(|l| l.text == "second paragraph")
            .expect("2nd block laid out");
        let target_block = l2.id.clone();
        let content_pt = Point::new(l2.text_x + 30.0, l2.y_top + l2.height / 2.0);

        // Sanity: clicking that content point lands the caret in the 2nd block.
        wp.caret_from_pointer(content_pt, false);
        assert_eq!(
            wp.inner.caret.block, target_block,
            "baseline content-space click hits the 2nd block"
        );
        let off_unzoomed = wp.inner.caret.offset;

        // Now zoom to 175%. The screen position of that same content point is the
        // content transform applied to it. Feeding the screen point back through
        // the production inverse (`pointer_to_content`) must recover the click,
        // so `handle_event`'s caret hit-test lands on the same block + offset.
        assert!(wp.set_zoom(1.75));
        let (wp, _) = pipeline_draw(&mut framed, 1000, 700); // recompute `zoom_anchor`
        let screen_pt = wp.content_transform(Affine::IDENTITY) * content_pt;
        let recovered = wp.pointer_to_content(screen_pt);
        assert!(
            (recovered.x - content_pt.x).abs() < 1e-6 && (recovered.y - content_pt.y).abs() < 1e-6,
            "inverse recovers the content point"
        );
        wp.caret_from_pointer(recovered, false);
        assert_eq!(
            wp.inner.caret.block, target_block,
            "zoomed click still hits the 2nd block"
        );
        assert_eq!(
            wp.inner.caret.offset, off_unzoomed,
            "zoomed click lands on the same caret offset"
        );
    }

    // Phase: zoom_clamps_and_resets.
    {
        let mut wp = WordProcessorWidget::new();
        assert!(
            (wp.zoom.factor() - 1.0).abs() < 1e-9,
            "default zoom is 100%"
        );
        // Clamp at the bounds.
        for _ in 0..100 {
            wp.zoom_by(-ZOOM_STEP);
        }
        assert!(
            (wp.zoom.factor() - ZOOM_MIN).abs() < 1e-6,
            "zoom clamps to the minimum"
        );
        for _ in 0..100 {
            wp.zoom_by(ZOOM_STEP);
        }
        assert!(
            (wp.zoom.factor() - ZOOM_MAX).abs() < 1e-6,
            "zoom clamps to the maximum"
        );
        wp.set_zoom(1.0);
        assert!(
            (wp.zoom.factor() - 1.0).abs() < 1e-9,
            "reset returns to 100%"
        );
    }
}

/// The toolbar zoom stepper drives zoom from its new home, accessibility exposes and activates it, and
/// zooming in grows the scrollable range.
#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one scenario test: each phase is a former standalone test kept verbatim as a scoped block"
)]
fn the_zoom_stepper_drives_zoom_and_the_scroll_range() {
    // Phase: toolbar_zoom_stepper_drives_zoom_from_its_new_home.
    // The zoom stepper lives right-aligned in the toolbar. Driving its buttons
    // through the real event handler must zoom out/in/reset, proving
    // the component's hit-testing fires correctly from the toolbar band.
    {
        let mut state = ui::State::new();
        let mut wp = WordProcessorWidget::new();
        wp.sync(&mut state.doc, &state.root);
        let mut framed = mount(state, wp, 1000, 700);
        let (wp, state) = Framed::split(&mut framed);

        // Every zoom button sits inside the toolbar band, not a bottom strip.
        let tb_h = wp.toolbar_height(1000.0);
        let rects = wp.zoom.hit_rects();
        assert_eq!(rects.len(), 3, "the −/%/+ stepper registered three targets");
        for r in &rects {
            assert!(
                r.y1 <= tb_h + 1.0,
                "zoom button is in the toolbar (rect={r:?}, toolbar_h={tb_h})"
            );
            assert!(
                r.x1 <= wp.rect.x1,
                "zoom button is within the window (rect={r:?})"
            );
        }

        // Component order is −, percentage/reset, +.
        let center = |wp: &WordProcessorWidget, index: usize| -> Point {
            let rects = wp.zoom.hit_rects();
            let r = &rects[index];
            Point::new(f64::midpoint(r.x0, r.x1), f64::midpoint(r.y0, r.y1))
        };

        // Click + : zoom in one step.
        let p = center(wp, 2);
        left_click(wp, state, p);
        assert!(
            (wp.zoom.factor() - (1.0 + ZOOM_STEP)).abs() < 1e-6,
            "clicking + zooms in one step: {}",
            wp.zoom.factor()
        );

        // Click − : back to 100%.
        let (wp, state) = pipeline_draw(&mut framed, 1000, 700);
        let p = center(wp, 0);
        left_click(wp, state, p);
        assert!(
            (wp.zoom.factor() - 1.0).abs() < 1e-6,
            "clicking − zooms out one step: {}",
            wp.zoom.factor()
        );

        // Click the percentage chip: reset to 100% from any zoom.
        wp.set_zoom(1.6);
        let (wp, state) = pipeline_draw(&mut framed, 1000, 700);
        let p = center(wp, 1);
        left_click(wp, state, p);
        assert!(
            (wp.zoom.factor() - 1.0).abs() < 1e-9,
            "clicking the % chip resets to 100%: {}",
            wp.zoom.factor()
        );
    }

    // Phase: accessibility_exposes_and_activates_the_zoom_stepper.
    // Zoom's custom-painted controls remain first-class semantic buttons. Their
    // targets are the controls' live paint targets, and activation enters the
    // same component method as a pointer press.
    {
        crate::i18n::set_locale("en-US");
        let (state, wp) = populated();
        let mut framed = mount(state, wp, 1100, 760);
        let (wp, state) = Framed::split(&mut framed);

        let tree = wp.accessibility_tree();
        let toolbar = tree
            .children
            .iter()
            .find(|child| child.node.role == ui::a11y::Role::Toolbar)
            .expect("drawn document has a toolbar");
        let zoom_in = toolbar
            .children
            .iter()
            .find(|child| child.node.action_target.as_deref() == Some(ZoomControl::TARGET_IN))
            .expect("zoom in is exposed to assistive technology");
        assert_eq!(zoom_in.node.name.as_deref(), Some("Zoom in"));
        assert_eq!(zoom_in.node.value.as_deref(), Some("100%"));
        assert!(zoom_in.node.bounds.is_some());

        assert!(Widget::perform_action_at(
            wp,
            &ui::a11y::Action::Activate,
            &[ui::a11y::hash_key(&ZoomControl::TARGET_IN)],
            &mut state.doc,
            &state.root,
        ));
        assert!(
            (wp.zoom.factor() - (1.0 + ZOOM_STEP)).abs() < 1e-6,
            "semantic activation uses the same +10% step as the toolbar"
        );

        assert!(Widget::perform_action_at(
            wp,
            &ui::a11y::Action::Activate,
            &[ui::a11y::hash_key(&ZoomControl::TARGET_RESET)],
            &mut state.doc,
            &state.root,
        ));
        assert_eq!(wp.zoom.factor(), 1.0, "semantic reset returns to 100%");
    }

    // Phase: zoom_in_grows_the_scrollable_range.
    {
        let mut state = ui::State::new();
        let mut wp = WordProcessorWidget::new();
        wp.sync(&mut state.doc, &state.root);
        for i in 0..40 {
            wp.model_mut().insert_str(
                &mut state.doc,
                &format!("Paragraph {i} with several words to fill the line."),
            );
            wp.model_mut().enter(&mut state.doc);
        }
        let mut framed = mount(state, wp, 1000, 700);
        let (wp, _) = Framed::split(&mut framed);
        let scroll_1x = wp.max_scroll;
        assert!(scroll_1x > 0.0, "the document already overflows at 100%");
        wp.set_zoom(2.0);
        let (wp, _) = pipeline_draw(&mut framed, 1000, 700);
        assert!(
            wp.max_scroll > scroll_1x,
            "zooming in enlarges the content, so there is more to scroll"
        );
    }
}

// ---- Responsive toolbar -------------------------------------------------

/// The toolbar packs into one row when it fits, wraps to more rows when items exceed the width, and the
/// real toolbar wraps on a narrow window and sets the canvas offset.
#[test]
fn the_toolbar_packs_or_wraps_to_its_width() {
    // Phase: toolbar_packs_into_one_row_when_it_fits.
    {
        // A generous width fits every control on a single row.
        let widths = vec![34.0, 30.0, 32.0, 30.0, 42.0, 42.0, 58.0];
        let starts = vec![false, false, false, false, true, false, false];
        let rows = pack_toolbar_rows(&widths, &starts, 1400.0, 1.0);
        assert_eq!(rows.len(), 1, "all items fit one row at 1400px");
        assert_eq!(rows[0], (0, widths.len()));
    }

    // Phase: toolbar_wraps_to_more_than_one_row_when_items_exceed_width.
    {
        // Items whose total far exceeds the available width must wrap.
        let widths = vec![100.0; 12]; // 12 × 100 = 1200px of buttons + gaps + pads
        let starts = vec![false; 12];
        let rows = pack_toolbar_rows(&widths, &starts, 400.0, 1.0);
        assert!(
            rows.len() > 1,
            "items exceeding the width wrap to >1 row (got {})",
            rows.len()
        );
        // Every item is assigned exactly once, in order, with no gaps.
        assert_eq!(rows.first().unwrap().0, 0);
        assert_eq!(rows.last().unwrap().1, widths.len());
        for w in rows.windows(2) {
            assert_eq!(w[0].1, w[1].0, "row ranges are contiguous");
        }
        // No row's packed width exceeds the usable width (mirrors `draw`).
        let usable = 400.0 - TOOLBAR_MODEL.pad - TOOLBAR_MODEL.right_pad;
        for (s, e) in &rows {
            let mut x = 0.0;
            for i in *s..*e {
                x +=
                    widths[i] + TOOLBAR_MODEL.gap + if starts[i] { TOOLBAR_MODEL.sep } else { 0.0 };
            }
            // A row is allowed to overflow only if it holds a single over-wide item.
            assert!(
                x <= usable + 1.0 || e - s == 1,
                "row [{s},{e}) overflows usable width"
            );
        }
    }

    // Phase: real_toolbar_wraps_on_a_narrow_window_and_sets_canvas_offset.
    {
        let mut state = ui::State::new();
        let mut wp = WordProcessorWidget::new();
        wp.sync(&mut state.doc, &state.root);
        wp.model_mut().insert_str(&mut state.doc, "hi");
        // At narrow sizes we deliberately use the focused single-row toolbar
        // plus an explicit More menu rather than a wrapped ribbon. This keeps
        // the writing canvas stable and prevents orphaned controls.
        let narrow = wp.toolbar_height(760.0);
        let wide = wp.toolbar_height(1800.0);
        assert!(
            wp.use_compact_toolbar(760.0),
            "760px selects the compact toolbar rather than wrapping a ribbon"
        );
        assert!(
            !wp.use_compact_toolbar(1800.0),
            "a wide window exposes the complete desktop toolbar"
        );
        assert_eq!(
            narrow, TOOLBAR_H,
            "the compact toolbar reserves exactly one writing row"
        );
        assert!(
            (wide - TOOLBAR_H).abs() < 1e-9,
            "the complete desktop toolbar still fits a single row when shown"
        );
        let mut framed = mount(state, wp, 760, 700);
        let (wp, _) = Framed::split(&mut framed);
        assert!(
            wp.page_rect.y0 >= narrow + PAGE_TOP,
            "the page is laid below the single-row compact toolbar"
        );
    }
}

/// Enlarged toolbar and layout controls fit their labels, and a 2x phone keeps the toolbar to two rows
/// with the document readable.
#[test]
fn enlarged_and_phone_layouts_keep_controls_fitting() {
    // Phase: enlarged_toolbar_controls_fit_their_labels.
    {
        let (state, wp) = populated();
        let mut theme = Theme::dark();
        theme.text_scale = 2.0;
        let wp = mount_themed(state, wp, 480, 600, &theme);
        let mut tr = TextRenderer::new();
        tr.set_text_scale(2.0);
        let specs = wp.toolbar_button_specs(true);
        for ((_, rect), (_, item)) in wp.toolbar.iter().zip(specs) {
            let (width, height) = tr.measure(&item.label, 15.0, None);
            assert!(
                rect.width() >= f64::from(width) + 12.0,
                "{} needs {width}, got {rect:?}",
                item.label
            );
            assert!(
                rect.height() >= f64::from(height),
                "{} needs {height}, got {rect:?}",
                item.label
            );
            assert_eq!(rect.intersect(wp.rect), *rect);
        }
    }

    // Phase: enlarged_layout_controls_fit_their_labels.
    {
        let (state, wp) = populated();
        let mut theme = Theme::dark();
        theme.text_scale = 2.0;
        let wp = mount_themed(state, wp, 480, 600, &theme);
        let mut tr = TextRenderer::new();
        tr.set_text_scale(2.0);
        for ((_, rect), label) in wp
            .layout_chips
            .iter()
            .zip([t("layout-margins"), t("layout-spacing")])
        {
            let (width, height) = tr.measure(&label, 13.0, None);
            assert!(
                rect.width() >= f64::from(width) + 16.0,
                "label needs {width}, got {rect:?}"
            );
            assert!(
                rect.height() >= f64::from(height),
                "label needs {height}, got {rect:?}"
            );
            assert_eq!(rect.intersect(wp.rect), *rect);
        }
    }

    // Phase: two_x_phone_keeps_toolbar_to_two_rows_and_document_readable.
    {
        let mut state = ui::State::new();
        let mut wp = WordProcessorWidget::new();
        wp.sync(&mut state.doc, &state.root);
        wp.model_mut()
            .insert_str(&mut state.doc, "A readable large-text document");
        let theme = Theme::dark().with_text_scale(2.0);
        let wp = mount_themed(state, wp, 390, 844, &theme);

        let row_height = ui::chrome::toolbar::metrics(2.0).row_height;
        assert_eq!(
            wp.toolbar_height(390.0),
            row_height * 2.0,
            "phone-sized 200% text reserves exactly one action row plus zoom"
        );
        let mut tr = TextRenderer::new();
        tr.set_text_scale(theme.text_scale);
        let specs = wp.responsive_toolbar_specs(390.0);
        assert_eq!(wp.toolbar.len(), specs.len());
        for ((action, rect), (expected_action, item)) in wp.toolbar.iter().zip(specs) {
            assert_eq!(*action, expected_action);
            let (width, height) = tr.measure_weighted(
                &item.label,
                15.0,
                None,
                ui::chrome::toolbar::ToolbarMetrics::label_weight(item.cue),
            );
            assert!(rect.width() >= f64::from(width) + 12.0);
            assert!(rect.height() >= f64::from(height));
            assert_eq!(rect.intersect(wp.rect), *rect);
            assert!(
                rect.y1 <= row_height,
                "actions stay in their reserved first row"
            );
        }
        assert_eq!(
            wp.zoom.factor(),
            2.0,
            "the editing surface follows 200% text"
        );
        assert!(
            wp.page_rect.width() * wp.zoom.factor() <= 390.0,
            "the readable 200% paper still fits the viewport"
        );
        assert_eq!(
            wp.layout_chips.len(),
            2,
            "the bounded bottom row exposes only complete layout controls"
        );
        assert!(
            wp.layout_chips.iter().all(|(_, rect)| rect.x1 <= 390.0),
            "no bottom action is half-visible at the right edge"
        );
    }
}

/// Document status stays in the bottom chrome at narrow and wide zoomed widths, and a narrow first open
/// fits the page until the user zooms.
#[test]
fn narrow_and_zoomed_windows_keep_status_and_page_fitting() {
    // Phase: document_status_stays_in_bottom_chrome_at_narrow_and_wide_zoomed_widths.
    {
        let (state, wp) = populated();
        let mut framed = mount(state, wp, 700, 900);
        for (width, zoom) in [(700_u32, 0.81), (1100, 1.0)] {
            framed.view.auto_fit_page = false;
            framed.zoom.set(zoom);
            let (wp, _) = pipeline_draw(&mut framed, width, 900);
            let bar = Rect::new(0.0, 868.0, f64::from(width), 900.0);
            let label = wp.status_count_label();
            let rect = WordProcessorWidget::layout_bar_status_rect(
                bar,
                210.0,
                f64::from(TextRenderer::new().measure(&label, 12.5, None).0),
                12.0,
            )
            .expect("the count fits after the layout controls");
            assert!(
                rect.y0 >= bar.y0 && rect.y1 <= bar.y1 && rect.x1 <= bar.x1,
                "{width}px status is entirely inside the bottom chrome: {rect:?}"
            );
            assert!(
                rect.y0 >= wp.rect.y1 - LAYOUT_BAR_H,
                "{width}px status cannot overlap the page even at {zoom:.0}% zoom"
            );
        }
    }

    // Phase: narrow_first_open_fits_the_page_until_the_user_zooms.
    {
        let mut state = ui::State::new();
        let mut wp = WordProcessorWidget::new();
        wp.sync(&mut state.doc, &state.root);
        wp.model_mut()
            .insert_str(&mut state.doc, "Readable narrow document");
        let mut framed = mount(state, wp, 640, 800);
        let (wp, _) = Framed::split(&mut framed);
        assert!(
            wp.zoom.factor() < 1.0,
            "narrow canvas automatically fits paper"
        );

        let ctf = wp.content_transform(Affine::IDENTITY);
        let left = ctf * Point::new(wp.page_rect.x0, wp.page_rect.y0);
        let right = ctf * Point::new(wp.page_rect.x1, wp.page_rect.y0);
        assert!(
            left.x >= wp.rect.x0 - 1.0,
            "left paper edge remains visible"
        );
        assert!(
            right.x <= wp.rect.x1 + 1.0,
            "right paper edge remains visible"
        );

        wp.set_zoom(1.0);
        let (wp, _) = pipeline_draw(&mut framed, 640, 800);
        assert_eq!(wp.zoom.factor(), 1.0, "explicit zoom disables auto-fit");
    }
}

/// Toolbar swatches never overlap the buttons, and the document canvas is wider than the styles panel
/// at every tile width.
#[test]
fn chrome_never_crowds_the_document_canvas() {
    // Phase: toolbar_swatches_never_overlap_the_buttons.
    // The swatch strips are right-aligned on the last toolbar row, painted
    // outside the chrome button packer, which reserves their width as a
    // phantom item. At a width where buttons alone would fill one row, the
    // last button (TOC) must not overlap the swatches — proving the
    // reservation works on the real draw path, not just in the width model.
    {
        let mut state = ui::State::new();
        let mut wp = WordProcessorWidget::new();
        wp.sync(&mut state.doc, &state.root);
        wp.model_mut().insert_str(&mut state.doc, "hi");
        // At a full desktop width, buttons and swatches share one row. The
        // compact toolbar intentionally moves swatches into More, so testing
        // this at 1200px would exercise the wrong responsive mode.
        let mut framed = mount(state, wp, 1800, 760);
        let (wp, _) = Framed::split(&mut framed);

        let rect_of =
            |a: fn(&ToolAction) -> bool| wp.toolbar.iter().find(|(x, _)| a(x)).map(|(_, r)| *r);
        let toc = rect_of(|a| matches!(a, ToolAction::InsertToc)).expect("TOC button laid out");
        // The neutral text-colour swatch is the left-most colour swatch.
        let swatch = wp
            .toolbar
            .iter()
            .filter(|(a, _)| matches!(a, ToolAction::TextColor(_)))
            .map(|(_, r)| *r)
            .min_by(|a, b| a.x0.partial_cmp(&b.x0).unwrap())
            .expect("colour swatches laid out");

        let disjoint_x = toc.x1 <= swatch.x0 + 0.5;
        let different_rows = toc.y1 <= swatch.y0 + 0.5 || swatch.y1 <= toc.y0 + 0.5;
        assert!(
            disjoint_x || different_rows,
            "TOC button {toc:?} must not overlap the colour swatches {swatch:?}"
        );
    }

    // Phase: the_document_canvas_is_wider_than_the_styles_panel_at_every_tile_width.
    // A document editor shows the document. The contextual styles panel
    // is an aid to that, not a co-equal half of the window: at every
    // width the shell can hand this app, the canvas left for the
    // document must be wider than the panel.
    //
    // Regression guard for a frame in which the panel took roughly half of a
    // tiled word-processor window, so
    // the thing the app exists to show was the smaller half of it.
    //
    // Measured on the *canvas*, not on `page_rect`: the page sheet is a
    // fixed nominal width that stays ~816px however narrow the window
    // gets (it is clipped and scrolled, not shrunk), so comparing it
    // against the panel would pass at every width and prove nothing.
    // `panel_rect` and `rect` are both written by the production draw
    // pass, and `panel_rect.x0` *is* the canvas's right edge.
    {
        // Widths a one-, two- and three-column strip hands an app on
        // ordinary displays — a 1280-wide screen split three ways is the
        // narrow end, and is where the panel currently wins.
        for (w, h) in [(1400_u32, 900_u32), (960, 800), (640, 700), (426, 700)] {
            let (state, mut wp) = bound();
            wp.view.styles_panel_visible = true;
            let mut framed = mount(state, wp, w, h);
            let (wp, _) = Framed::split(&mut framed);
            assert!(
                wp.view.styles_panel_visible,
                "precondition: the explicitly-open panel is the one under test",
            );
            assert!(
                !wp.outline.is_open(),
                "precondition: the left-hand outline pane is closed, so the \
                 canvas is bounded only by the panel under test",
            );
            let panel = wp.panel_rect.width();
            let canvas = if wp.styles_panel_overlay() {
                wp.rect.width()
            } else {
                wp.rect.width() - panel
            };
            assert!(
                canvas > panel,
                "at {w}x{h} the canvas left for the document ({canvas:.0}px) \
                 must be wider than the styles panel ({panel:.0}px) — an \
                 editor whose subject is the smaller half of its own window \
                 is showing the wrong thing",
            );
        }
    }
}

// ---- spell-check squiggles --------------------------------------------

/// A misspelled word yields a squiggle at its x range, the toggle suppresses the pass, autocorrect is
/// reversible at a space boundary, a German session does not judge German words against English, and
/// the spell cache reuses unchanged blocks and rechecks after an edit.
#[test]
fn spellcheck_marks_misspellings_per_language_and_caches_by_block() {
    // Phase: misspelled_word_yields_a_squiggle_span_at_its_x_range.
    {
        let mut state = ui::State::new();
        let mut wp = WordProcessorWidget::new();
        wp.sync(&mut state.doc, &state.root);
        // "teh" is a classic misspelling flagged by the builtin dictionary; the
        // surrounding words are valid, so exactly one span is expected.
        wp.model_mut()
            .insert_str(&mut state.doc, "teh quick brown fox");
        let mut framed = mount(state, wp, 1000, 700);
        let (wp, _) = Framed::split(&mut framed);

        let l = wp
            .laid
            .iter()
            .find(|l| l.text == "teh quick brown fox")
            .expect("paragraph laid out");
        let id = l.id.clone();
        let text = l.text.clone();

        // The production spell path computes the misspelled byte spans for this
        // block (the same call `draw` makes to feed `draw_squiggle`).
        let spans = wp.block_spell_spans(&id, &text);
        assert_eq!(
            spans,
            vec![(0usize, 3usize)],
            "exactly ‘teh’ at bytes [0,3) is flagged"
        );

        // The span maps to a concrete, positive on-screen x-range via the cached
        // shaped layout — exactly the geometry `draw` passes to `draw_squiggle`.
        let (_, layout) = wp
            .layout_cache
            .get(&id)
            .expect("block layout cached after draw");
        let rects = layout.selection_rects(0, 3);
        assert!(
            !rects.is_empty(),
            "the misspelled span resolves to at least one rect"
        );
        let (x0, _, x1, _) = rects[0];
        assert!(
            x1 > x0,
            "the squiggle covers a non-empty width: x0={x0} x1={x1}"
        );
        // ‘teh’ sits at the very start of the line, so its rect hugs the left edge.
        assert!(
            x0 < 4.0,
            "the first word's squiggle starts at the line's left edge: x0={x0}"
        );
    }

    // Phase: spellcheck_toggle_off_suppresses_the_squiggle_pass.
    {
        let mut state = ui::State::new();
        let mut wp = WordProcessorWidget::new();
        wp.sync(&mut state.doc, &state.root);
        wp.model_mut().insert_str(&mut state.doc, "teh");
        assert!(wp.spellcheck_enabled(), "spell-check defaults ON");
        wp.toggle_spellcheck();
        assert!(!wp.spellcheck_enabled(), "toggle turns it OFF");
        // With the toggle off, a draw must not even build the dictionary (the lazy
        // OnceLock stays untouched for this widget — the per-widget handle is None).
        let mut framed = mount(state, wp, 1000, 700);
        let (wp, _) = Framed::split(&mut framed);
        assert!(
            wp.dict.is_none(),
            "spell-check off skips dictionary construction"
        );
    }

    // Phase: document_autocorrect_is_reversible_at_a_space_boundary.
    {
        let mut state = ui::State::new();
        let mut wp = WordProcessorWidget::new();
        wp.sync(&mut state.doc, &state.root);
        wp.model_mut().insert_str(&mut state.doc, "teh");

        let correction = wp
            .autocorrect_before_boundary(&mut state.doc)
            .expect("‘teh’ is a high-confidence correction");
        wp.model_mut().insert_str(&mut state.doc, " ");
        wp.last_autocorrection = Some(DocumentAutocorrection {
            boundary_end: wp.inner.caret.offset,
            ..correction
        });
        assert_eq!(wp.inner.current_text(&state.doc), "the ");

        assert!(wp.undo_document_autocorrection(&mut state.doc));
        assert_eq!(wp.inner.current_text(&state.doc), "teh");
    }

    // Phase: a_german_session_does_not_judge_german_words_against_english.
    {
        // The embedded list is English; a German session asks the system Hunspell
        // `de_DE` dictionary instead, and flags nothing when that dictionary is
        // absent. Either way a correct German word must never carry a squiggle.
        let mut wp = WordProcessorWidget::new();
        let english = wp.block_spell_spans("b", "Straße Mädchen");
        assert!(
            !english.is_empty(),
            "precondition: the English checker flags the German words, got {english:?}"
        );
        wp.set_spell_language("de_DE");
        assert_eq!(
            wp.block_spell_spans("b", "Straße Mädchen"),
            Vec::<(usize, usize)>::new(),
            "German words are correct in a German session"
        );
    }

    // Phase: spell_cache_reuses_unchanged_blocks_and_rechecks_after_edit.
    {
        let mut state = ui::State::new();
        let mut wp = WordProcessorWidget::new();
        wp.sync(&mut state.doc, &state.root);
        wp.model_mut().insert_str(&mut state.doc, "wrold");
        let id = wp.blocks[0].id.clone();

        // First check flags ‘wrold’.
        let spans = wp.block_spell_spans(&id, "wrold");
        assert_eq!(spans, vec![(0, 5)], "‘wrold’ flagged");
        // A second check with identical text reuses the cache (same result).
        assert_eq!(wp.block_spell_spans(&id, "wrold"), vec![(0, 5)]);
        // Correcting the text re-checks (the key is the text) → no misspelling.
        assert!(
            wp.block_spell_spans(&id, "world").is_empty(),
            "the corrected text re-checks clean"
        );
    }
}

/// Multi-block typing dispatches one replace-selection action, the shared clipboard preserves rich and
/// plain paste, typed text keeps its capitals, and caret navigation does not mark the document modified.
#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one scenario test: each phase is a former standalone test kept verbatim as a scoped block"
)]
fn typing_and_clipboard_edit_the_document_through_one_action() {
    const CHILD: &str = "TILO_WRITER_CLIPBOARD_TEST_CHILD";
    #[derive(Default)]
    struct TestClipboard(std::sync::Mutex<Option<String>>);
    impl ui::clipboard::ClipboardBackend for TestClipboard {
        fn set_text(&self, _: ui::clipboard::ClipboardTarget, text: &str) -> bool {
            *self.0.lock().unwrap() = Some(text.to_string());
            true
        }

        fn text(&self, _: ui::clipboard::ClipboardTarget) -> Option<String> {
            self.0.lock().unwrap().clone()
        }

        fn name(&self) -> &'static str {
            "writer-clipboard-test"
        }
    }
    if std::env::var_os(CHILD).is_none() {
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "widget::tests::typing_and_clipboard_edit_the_document_through_one_action",
                "--nocapture",
            ])
            .env(CHILD, "1")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "isolated clipboard test failed:
{}
{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        return;
    }

    // Phase: multi_block_typing_dispatches_one_replace_selection_action.
    {
        let mut state = ui::State::new();
        let mut wp = WordProcessorWidget::new();
        wp.sync(&mut state.doc, &state.root);
        wp.model_mut().insert_str(&mut state.doc, "alpha");
        wp.model_mut().enter(&mut state.doc);
        wp.model_mut().insert_str(&mut state.doc, "beta");
        let ids = tilo_doc::model::blocks_in_order(
            &state.doc,
            &tilo_doc::model::open(&state.doc, &state.root).unwrap(),
        );
        wp.model_mut().set_caret(Caret {
            block: ids[0].clone(),
            offset: 2,
        });
        wp.model_mut().set_anchor();
        wp.model_mut().set_caret(Caret {
            block: ids[1].clone(),
            offset: 2,
        });

        wp.insert_text_via_action(&mut state.doc, "X");

        let dispatched = wp.last_dispatched_for_test().expect("action dispatched");
        let action: crate::actions::RichTextAction =
            serde_json::from_slice(&dispatched.payload).unwrap();
        assert!(matches!(
            action,
            crate::actions::RichTextAction::ReplaceSelection { .. }
        ));
        let blocks = tilo_doc::model::materialize(
            &state.doc,
            &tilo_doc::model::open(&state.doc, &state.root).unwrap(),
        )
        .blocks;
        assert_eq!(blocks.len(), 1);
        assert_eq!(blocks[0].text, "alXta");
    }

    // Phase: shared_clipboard_preserves_rich_paste_and_plain_paste_semantics.
    {
        let services = ui::host::HostServices::null()
            .with_clipboard(std::sync::Arc::new(TestClipboard::default()));
        let _host = ui::host::enter(&services);
        let mut state = ui::State::new();
        let mut wp = WordProcessorWidget::new();
        wp.sync(&mut state.doc, &state.root);

        // Copy without a selection remains a no-op.
        clipboard_copy("sentinel");
        wp.clipboard_copy(&state.doc);
        assert_eq!(clipboard_paste().as_deref(), Some("sentinel"));

        wp.inner.insert_str(&mut state.doc, "hello");

        // Copy a bold selection. The system-facing clipboard carries plain text;
        // the widget's matching sidecar carries the qdoc marks.
        wp.inner.caret.offset = 0;
        wp.inner.set_anchor();
        wp.inner.caret.offset = 5;
        wp.inner.toggle_mark(&mut state.doc, "strong");
        wp.inner.caret.offset = 0;
        wp.inner.clear_anchor();
        wp.inner.set_anchor();
        wp.inner.caret.offset = 5;
        wp.clipboard_copy(&state.doc);
        assert_eq!(clipboard_paste().as_deref(), Some("hello"));

        // A normal paste of the unchanged clipboard finds the sidecar and keeps
        // the mark.
        wp.inner.clear_anchor();
        wp.inner.move_end(&state.doc);
        wp.clipboard_paste(&mut state.doc);
        let containers = wp.inner.containers.clone().expect("bound document");
        let block = wp.inner.caret.block.clone();
        assert_eq!(wp.inner.current_text(&state.doc), "hellohello");
        assert!(tilo_doc::model::has_mark_over(
            &state.doc,
            &containers,
            &block,
            5,
            10,
            "strong"
        ));

        // Paste-as-plain reads through the same shared backend but deliberately
        // ignores that matching rich sidecar. Use a fresh paragraph so the
        // destination's own active character style cannot affect the assertion.
        wp.inner.enter(&mut state.doc);
        wp.clipboard_paste_plain(&mut state.doc);
        let block = wp.inner.caret.block.clone();
        assert_eq!(wp.inner.current_text(&state.doc), "hello");
        assert!(!tilo_doc::model::has_mark_over(
            &state.doc,
            &containers,
            &block,
            0,
            5,
            "strong"
        ));

        // Replacing the clipboard from another app invalidates the sidecar key,
        // so an ordinary paste also falls back to plain text.
        clipboard_copy("external");
        wp.clipboard_paste(&mut state.doc);
        assert_eq!(wp.inner.current_text(&state.doc), "helloexternal");
        assert!(!tilo_doc::model::has_mark_over(
            &state.doc,
            &containers,
            &block,
            5,
            13,
            "strong"
        ));
    }

    // Phase: typed_text_lands_in_the_document_with_its_capitals.
    // Type a mixed-case string through the production `handle_event`
    // keyboard path, then read the document back through the production
    // `materialize` path.
    //
    // This guards two *distinct* failures that a rendered frame showed
    // and no test caught. The first is the document coming back empty
    // after a scripted typing run — nothing the driver sent ever reached
    // the CRDT, which made every downstream "the peer sees what was
    // typed" claim vacuously true over an empty string. The second is
    // the document coming back lowercased: a key source that resolves a
    // character to a bare keycode loses the shift, so `Hello World`
    // lands as `hello world`. Asserting an exact mixed-case string
    // catches both; asserting merely "non-empty" catches only the first.
    {
        const TYPED: &str = "Hello World";

        let (mut state, mut wp) = bound();
        assert_eq!(
            body_text(&wp, &state).concat(),
            "",
            "a fresh document starts empty, so the assertion below is about \
             what typing added and nothing else",
        );

        for ch in TYPED.chars() {
            type_char(&mut wp, &mut state, ch);
        }

        assert_eq!(
            body_text(&wp, &state).concat(),
            TYPED,
            "the document holds exactly the characters that were typed, \
             capitals included",
        );
    }

    // Phase: caret_navigation_is_handled_without_marking_the_document_modified.
    {
        let (mut state, mut wp) = bound();
        wp.model_mut().insert_str(&mut state.doc, "move");
        wp.lifecycle.focused = true;
        wp.save_status = DocumentSaveStatus::Clean;
        wp.lifecycle.needs_commit = false;
        let before = wp.model().caret().offset;
        let mut handled = false;

        wp.handle_key_press(
            &mut state.doc,
            &Key::Named(NamedKey::ArrowLeft),
            None,
            false,
            false,
            &mut handled,
        );

        assert!(handled);
        assert_eq!(wp.model().caret().offset, before - 1);
        assert!(
            wp.save_status == DocumentSaveStatus::Clean,
            "caret motion is not a content edit"
        );
        assert!(
            wp.lifecycle.needs_commit,
            "caret presence still needs publishing"
        );
    }
}

// ---- right-click context menu -----------------------------------------

/// A content-space point at the very start of the first laid block's text
/// (so a right-click lands on its first word).
fn first_block_start_point(wp: &WordProcessorWidget) -> Point {
    let l = wp.laid.first().expect("at least one laid block");
    Point::new(l.text_x + 1.0, l.y_top + l.height / 2.0)
}

/// Right-click preserves selected text and clipboard actions, open menus capture editing keys and
/// keyboard activation, and context actions stay in the viewport.
#[test]
fn context_menus_preserve_the_selection_and_capture_editing_keys() {
    // Phase: menu_ux_right_click_preserves_selected_text_and_clipboard_actions.
    {
        let (mut state, mut wp) = bound();
        wp.model_mut().insert_str(&mut state.doc, "Hello world");
        let mut framed = mount(state, wp, 1000, 700);
        let (wp, state) = Framed::split(&mut framed);
        wp.inner.select_all(&state.doc);
        let selected = wp.inner.selection_text(&state.doc);
        let caret = wp.inner.caret.clone();
        let anchor = wp.inner.selection_anchor.clone();
        let point = first_block_start_point(wp);
        wp.open_context_menu(&state.doc, point, [300.0, 300.0]);
        assert_eq!(wp.inner.selection_text(&state.doc), selected);
        assert_eq!(wp.inner.caret, caret);
        assert_eq!(wp.inner.selection_anchor, anchor);
        assert!(wp.context_actions.contains(&Some(CtxAction::Copy)));
        assert!(wp.context_actions.contains(&Some(CtxAction::Cut)));
    }

    // Phase: menu_ux_open_menus_capture_editing_keys_and_keyboard_activation.
    {
        for style_menu in [false, true] {
            let (mut state, mut wp) = bound();
            wp.model_mut().insert_str(&mut state.doc, "Keep my draft");
            let mut framed = mount(state, wp, 1000, 700);
            let (wp, state) = Framed::split(&mut framed);
            wp.lifecycle.focused = true;
            wp.inner.select_all(&state.doc);
            let style_count = wp.sheet.order().len();
            if style_menu {
                wp.style_menu.open("body", Point::new(300.0, 300.0));
            } else {
                wp.open_toolbar_overflow();
            }
            press_named(wp, state, NamedKey::Backspace);
            assert_eq!(
                first_block(wp, state).map(|b| b.text),
                Some("Keep my draft".into())
            );
            press_named(wp, state, NamedKey::ArrowDown);
            press_named(wp, state, NamedKey::Enter);
            assert!(!wp.context_menu.is_open());
            assert!(!wp.style_menu.is_open());
            if style_menu {
                assert_eq!(
                    wp.sheet.order().len(),
                    style_count + 1,
                    "Enter duplicates the style"
                );
            } else {
                assert!(
                    wp.mark_active_in_selection("strike"),
                    "Enter applies the chosen formatting"
                );
            }
            assert_eq!(
                first_block(wp, state).map(|b| b.text),
                Some("Keep my draft".into())
            );
        }
    }

    // Phase: menu_ux_context_actions_stay_in_the_viewport.
    {
        let (mut state, mut wp) = bound();
        wp.model_mut().insert_str(&mut state.doc, "Hello world");
        let mut framed = mount(state, wp, 600, 400);
        let (wp, state) = Framed::split(&mut framed);
        let point = first_block_start_point(wp);
        wp.open_context_menu(&state.doc, point, [598.0, 398.0]);
        let mut scene = Scene::new();
        wp.draw_overlay(
            &mut scene,
            Affine::IDENTITY,
            &mut TextRenderer::new(),
            &Theme::dark().with_text_scale(2.0),
            &PresenceCtx::empty(),
        );
        for (index, action) in wp.context_actions.iter().enumerate() {
            if action.is_some() {
                let row = wp
                    .context_menu
                    .item_rect(index)
                    .expect("menu row remains reachable");
                assert!(
                    row.x0 >= 0.0 && row.y0 >= 0.0 && row.x1 <= 600.0 && row.y1 <= 400.0,
                    "row outside viewport: {row:?}"
                );
            }
        }
    }
}

/// Host undo cannot edit behind an open menu, and right-click at the selection end keeps the selection.
#[test]
fn host_undo_and_right_click_leave_an_open_menus_selection_alone() {
    // Phase: menu_ux_host_undo_cannot_edit_behind_an_open_menu.
    {
        for menu_kind in 0..3 {
            let (mut state, mut wp) = bound();
            state.commit_action(None, None);
            wp.inner.insert_str(&mut state.doc, "Keep this edit");
            state.commit_action(None, None);
            let containers = wp.inner.containers.clone().unwrap();
            let mut framed = mount(state, wp, 1000, 700);
            match menu_kind {
                0 => framed.open_template_picker(),
                1 => framed.open_toolbar_overflow(),
                _ => framed.style_menu.open("body", Point::new(300.0, 300.0)),
            }
            let driver = Framed::harness_mut(&mut framed);
            driver.relayout();
            // Ctrl+Z is the host undo binding under these conventions; on a macOS
            // build host the default conventions bind undo to Cmd instead.
            driver.set_platform_conventions(ui::PlatformConventions::new(ui::Platform::Linux));
            driver.set_modifiers(&["ctrl".into()]);
            driver.key("z");
            assert_eq!(
                tilo_doc::model::materialize(&driver.state_mut().doc, &containers).blocks[0].text,
                "Keep this edit",
                "host undo must be captured while a menu is open"
            );
            driver.set_modifiers(&[]);
            driver.key("Escape");
            driver.set_modifiers(&["ctrl".into()]);
            driver.key("z");
            assert_eq!(
                tilo_doc::model::materialize(&driver.state_mut().doc, &containers).blocks[0].text,
                "",
                "ordinary undo resumes after dismissal"
            );
        }
    }

    // Phase: menu_ux_right_click_at_the_selection_end_keeps_the_selection.
    {
        let (mut state, mut wp) = bound();
        wp.inner.insert_str(&mut state.doc, "Hello world");
        let mut framed = mount(state, wp, 1000, 700);
        let (wp, state) = Framed::split(&mut framed);
        wp.inner.select_all(&state.doc);
        let mut point = first_block_start_point(wp);
        point.x += 400.0; // hit testing clamps the text's right edge to its end offset
        wp.open_context_menu(&state.doc, point, [500.0, 300.0]);
        assert_eq!(wp.inner.selection_text(&state.doc), "Hello world");
    }
}

/// Right-click on a misspelled word offers add-to-dictionary and clears the squiggle, a suggestion
/// replaces the word, and right-click with no selection selects the word with Copy in the menu.
#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one scenario test: each phase is a former standalone test kept verbatim as a scoped block"
)]
fn right_click_spelling_offers_dictionary_suggestions_and_copy() {
    // Phase: right_click_on_misspelled_word_offers_add_to_dictionary_and_clears_squiggle.
    {
        // Assertions below are literal English menu strings; pin the locale so
        // the test doesn't depend on the host's `LANG` (nextest runs each test
        // in its own process, so this can't leak into a sibling test).
        crate::i18n::set_locale("en-US");
        // Persist added words to a throwaway file rather than the real
        // `~/.config/tilo/userdict.txt`.
        let tmp = std::env::temp_dir().join(format!("tilo-userdict-test-{}", std::process::id()));
        let userdict = tmp.join("userdict.txt");

        let mut state = ui::State::new();
        let mut wp = WordProcessorWidget::new();
        wp.proofing.user_dictionary = Some(userdict.clone());
        wp.sync(&mut state.doc, &state.root);
        // "qwordz" is not a real word → flagged. Followed by a valid word.
        wp.model_mut().insert_str(&mut state.doc, "qwordz quick");
        let mut framed = mount(state, wp, 1000, 700);
        let (wp, state) = Framed::split(&mut framed);
        let id = wp.blocks[0].id.clone();
        assert_eq!(
            wp.block_spell_spans(&id, "qwordz quick"),
            vec![(0usize, 6usize)],
            "precondition: ‘qwordz’ is flagged"
        );

        // Right-click on the first word.
        let pt = first_block_start_point(wp);
        wp.open_context_menu(
            &state.doc,
            pt,
            [num::f32_of_f64(pt.x), num::f32_of_f64(pt.y)],
        );
        let labels = wp.context_menu_labels();
        assert!(
            labels.iter().any(|l| l == "Add to dictionary"),
            "misspelled word offers ‘Add to dictionary’, got {labels:?}"
        );

        // Invoke "Add to dictionary".
        let add_idx = wp
            .context_menu
            .items()
            .iter()
            .position(|i| i.label() == Some("Add to dictionary"))
            .unwrap();
        assert_eq!(
            wp.context_actions[add_idx],
            Some(CtxAction::AddToDictionary("qwordz".to_string()))
        );
        wp.dispatch_context_action(add_idx, &mut state.doc);
        assert!(!wp.context_menu.is_open(), "menu closed after dispatch");

        // Re-check: the squiggle is gone (the cache was invalidated and the word
        // is now in the user dictionary).
        assert!(
            wp.block_spell_spans(&id, "qwordz quick").is_empty(),
            "after Add to dictionary, ‘qwordz’ is no longer flagged"
        );
        assert!(
            wp.dictionary().contains("qwordz"),
            "the word is recorded in the (owned) dictionary"
        );
        assert_eq!(
            std::fs::read_to_string(&userdict).ok().as_deref(),
            Some("qwordz"),
            "the added word persists to the person's word list"
        );

        // A widget opened later starts with the persisted word already known.
        let mut reopened = WordProcessorWidget::new();
        reopened.proofing.user_dictionary = Some(userdict);
        assert!(
            reopened.block_spell_spans("b", "qwordz quick").is_empty(),
            "a persisted word is not flagged in a new session"
        );
        let _ = std::fs::remove_dir_all(&tmp);
    }

    // Phase: right_click_suggestion_replaces_the_word.
    {
        let mut state = ui::State::new();
        let mut wp = WordProcessorWidget::new();
        wp.sync(&mut state.doc, &state.root);
        // "teh" suggests "the".
        wp.model_mut().insert_str(&mut state.doc, "teh end");
        let mut framed = mount(state, wp, 1000, 700);
        let (wp, state) = Framed::split(&mut framed);

        let pt = first_block_start_point(wp);
        wp.open_context_menu(
            &state.doc,
            pt,
            [num::f32_of_f64(pt.x), num::f32_of_f64(pt.y)],
        );
        // Find a suggestion row whose action replaces [0,3) with "the".
        let the_idx = wp.context_actions.iter().position(|a| {
            matches!(a, Some(CtxAction::ApplySuggestion { replacement, .. }) if replacement == "the")
        });
        let Some(idx) = the_idx else {
            panic!(
                "expected a ‘the’ suggestion row, labels: {:?}",
                wp.context_menu_labels()
            );
        };
        wp.dispatch_context_action(idx, &mut state.doc);
        // The replacement is one replayable envelope through the executor, not a
        // direct CRDT write.
        let dispatched = wp
            .last_dispatched_for_test()
            .expect("the suggestion dispatched an action");
        assert_eq!(
            dispatched.type_id,
            crate::actions::WORD_PROCESSOR_ACTION_TYPE
        );
        assert!(matches!(
            serde_json::from_slice::<crate::actions::RichTextAction>(&dispatched.payload),
            Ok(crate::actions::RichTextAction::SpliceText { at: 0, del: 3, ref ins, .. }) if ins == "the"
        ));
        // The block text now reads "the end".
        let id = wp.blocks[0].id.clone();
        let text = wp.model().block_texts(&state.doc);
        assert_eq!(
            text[0], "the end",
            "suggestion replaced ‘teh’ with ‘the’ (block {id})"
        );
    }

    // Phase: right_click_with_no_selection_selects_word_and_menu_has_copy.
    {
        // Assertions below are literal English menu strings; pin the locale so
        // the test doesn't depend on the host's `LANG` (nextest runs each test
        // in its own process, so this can't leak into a sibling test).
        crate::i18n::set_locale("en-US");
        let mut state = ui::State::new();
        let mut wp = WordProcessorWidget::new();
        wp.sync(&mut state.doc, &state.root);
        wp.model_mut().insert_str(&mut state.doc, "hello world");
        let mut framed = mount(state, wp, 1000, 700);
        let (wp, state) = Framed::split(&mut framed);
        assert!(
            !wp.model().has_selection(),
            "precondition: nothing selected"
        );

        let pt = first_block_start_point(wp);
        wp.open_context_menu(
            &state.doc,
            pt,
            [num::f32_of_f64(pt.x), num::f32_of_f64(pt.y)],
        );
        // The clicked-word fallback selected "hello".
        assert!(
            wp.model().has_selection(),
            "a word was selected by the fallback"
        );
        assert_eq!(wp.model().selection_text(&state.doc), "hello");
        // With a selection, the menu has Cut + Copy.
        let labels = wp.context_menu_labels();
        assert!(labels.contains(&"Copy".to_string()), "got {labels:?}");
        assert!(labels.contains(&"Cut".to_string()), "got {labels:?}");
        assert!(labels.contains(&"Select All".to_string()), "got {labels:?}");

        // item_at + dispatch maps the Copy row to the Copy action.
        let copy_idx = wp
            .context_menu
            .items()
            .iter()
            .position(|i| i.label() == Some("Copy"))
            .unwrap();
        assert_eq!(wp.context_actions[copy_idx], Some(CtxAction::Copy));
    }
}

// ---- footnote rendering -----------------------------------------------

/// Author a doc with one body paragraph that references a single footnote
/// (placeholder + `note-ref` mark), whose content is `note_body`. Returns the
/// constructed `ui::State`.
fn author_doc_with_footnote(body: &str, ref_idx: usize, note_body: &str) -> ui::State {
    let mut state = ui::State::new();
    let c = tilo_doc::model::init(&mut state.doc, &state.root);
    let mut ids = tilo_doc::IdGen::new(state.doc.get_actor().to_hex_string());
    {
        let mut dm = tilo_doc::DocMut {
            doc: &mut state.doc,
            c: &c,
            ids: &mut ids,
        };
        let p = dm.insert_block(None, "paragraph");
        dm.splice_text(&p, 0, 0, body);
        // Place a footnote reference marker at `ref_idx` and fill the note body.
        let note_id = dm.insert_footnote(&p, ref_idx);
        let nb = dm
            .insert_note_block(&note_id, None, "paragraph")
            .expect("note block");
        dm.splice_note_text(&note_id, &nb, 0, 0, note_body);
    }
    state.doc.commit();
    state
}

/// A footnote renders its superscript marker and note text in the page area, and note numbering follows
/// document order, not note id.
#[test]
fn footnotes_render_in_the_page_area_numbered_in_document_order() {
    // Phase: footnote_renders_superscript_marker_and_note_text_in_page_area.
    {
        // Body "Hello world" with a footnote reference after "Hello" (idx 5).
        let state = author_doc_with_footnote("Hello world", 5, "A clarifying footnote.");
        let wp = WordProcessorWidget::new();
        let mut framed = mount(state, wp, 1000, 900);
        let (wp, _) = Framed::split(&mut framed);

        // The note materialized into the widget as a footnote.
        assert_eq!(wp.notes.len(), 1, "one note materialized");
        assert_eq!(
            wp.notes[0].kind,
            tilo_doc::schema::NOTE_KIND_FOOTNOTE,
            "it is a footnote"
        );

        // The body block carries exactly one note-ref marker (the production scan
        // `draw` uses to place the superscript), and document-order numbering
        // assigns it citation number 1.
        let body = wp
            .blocks
            .iter()
            .find(|b| b.text.contains("Hello"))
            .expect("body block");
        let refs = block_note_refs(body);
        assert_eq!(refs.len(), 1, "one note-ref marker in the body");
        let (ref_ci, note_id) = &refs[0];
        assert_eq!(
            *ref_ci, 5,
            "marker sits at the reference index (the placeholder char)"
        );
        let nums = note_numbering(&wp.blocks);
        assert_eq!(
            nums.get(note_id.as_str()),
            Some(&1),
            "first-referenced note is numbered 1"
        );

        // The footnote text that the page note-area renders ("N. <text>") is laid
        // — assert the note text is present (not the rendered pixels).
        let text = note_text(&wp.notes[0]);
        assert_eq!(
            text, "A clarifying footnote.",
            "the footnote body text is materialized"
        );
        let rendered_line = format!("{}. {}", nums[note_id.as_str()], text);
        assert_eq!(
            rendered_line, "1. A clarifying footnote.",
            "the page note-area line is well-formed"
        );
    }

    // Phase: note_numbering_follows_document_order_not_note_id.
    {
        // Two footnote references in one paragraph; the FIRST reference (by source
        // position) must be number 1 regardless of the notes' id sort order.
        let mut state = ui::State::new();
        let c = tilo_doc::model::init(&mut state.doc, &state.root);
        let mut ids = tilo_doc::IdGen::new(state.doc.get_actor().to_hex_string());
        let (n_first, n_second);
        {
            let mut dm = tilo_doc::DocMut {
                doc: &mut state.doc,
                c: &c,
                ids: &mut ids,
            };
            let p = dm.insert_block(None, "paragraph");
            dm.splice_text(&p, 0, 0, "alpha beta");
            // Insert the LATER reference first (at idx 10, end) then the EARLIER one
            // (at idx 5), so document order != insertion order.
            n_second = dm.insert_footnote(&p, 10);
            n_first = dm.insert_footnote(&p, 5);
            let b2 = dm.insert_note_block(&n_second, None, "paragraph").unwrap();
            dm.splice_note_text(&n_second, &b2, 0, 0, "second");
            let b1 = dm.insert_note_block(&n_first, None, "paragraph").unwrap();
            dm.splice_note_text(&n_first, &b1, 0, 0, "first");
        }
        state.doc.commit();

        let wp = WordProcessorWidget::new();
        let mut framed = mount(state, wp, 1000, 900);
        let (wp, _) = Framed::split(&mut framed);

        let nums = note_numbering(&wp.blocks);
        assert_eq!(
            nums.get(n_first.as_str()),
            Some(&1),
            "the earlier marker is #1 by document order"
        );
        assert_eq!(
            nums.get(n_second.as_str()),
            Some(&2),
            "the later marker is #2"
        );
    }
}

/// Build a single-paragraph document with `text`, returning the drawn widget
/// plus its `ui::State` and the paragraph's block id. The caller mutates the
/// doc (adding comments/changes) via a `DocMut`, then re-draws and inspects.
fn doc_with_paragraph(
    text: &str,
) -> (
    WordProcessorWidget,
    ui::State,
    tilo_doc::Containers,
    tilo_doc::IdGen,
    String,
) {
    let mut state = ui::State::new();
    let c = tilo_doc::model::init(&mut state.doc, &state.root);
    let mut ids = tilo_doc::IdGen::new(state.doc.get_actor().to_hex_string());
    let bid;
    {
        let mut dm = tilo_doc::DocMut {
            doc: &mut state.doc,
            c: &c,
            ids: &mut ids,
        };
        bid = dm.insert_block(None, "paragraph");
        dm.splice_text(&bid, 0, 0, text);
    }
    state.doc.commit();
    let wp = WordProcessorWidget::new();
    (wp, state, c, ids, bid)
}

/// A comment range computes a highlight rect, the review reply affordance opens the durable thread, and
/// deleting a comment removes it and clears the highlight.
#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one scenario test: each phase is a former standalone test kept verbatim as a scoped block"
)]
fn comments_highlight_open_threads_and_delete_cleanly() {
    // Phase: comment_range_computes_a_highlight_rect.
    {
        // "hello world" with a comment over the word "world" (chars 6..11).
        let (wp, mut state, c, mut ids, bid) = doc_with_paragraph("hello world");
        {
            let mut dm = tilo_doc::DocMut {
                doc: &mut state.doc,
                c: &c,
                ids: &mut ids,
            };
            dm.add_comment(&bid, 6, 11, "Ada", "Reword this");
        }
        state.doc.commit();
        let mut framed = mount(state, wp, 1000, 900);
        let (wp, _) = Framed::split(&mut framed);

        // The comment is materialized and anchored to the body range.
        assert_eq!(
            wp.review.comments().len(),
            1,
            "the comment is materialized into the widget"
        );
        assert_eq!(wp.review.comments()[0].author, "Ada");

        // The production helper recovers the commented byte range from the block's
        // `comment` mark (this is exactly what the draw pass calls).
        let b = wp
            .blocks
            .iter()
            .find(|b| b.id == bid)
            .expect("block present");
        let ranges = block_comment_ranges(b);
        assert_eq!(ranges.len(), 1, "one comment range over the block");
        let (bs, be, cid) = &ranges[0];
        assert_eq!((*bs, *be), (6, 11), "byte range covers 'world'");
        assert_eq!(
            cid,
            &wp.review.comments()[0].id,
            "range carries the comment id"
        );

        // The draw pass maps that byte range → on-screen highlight rects via the
        // shaped layout's `selection_rects`. Assert a non-empty rect with width.
        let (_, lay) = wp.layout_cache.get(&bid).expect("shaped layout cached");
        let rects = lay.selection_rects(*bs, *be);
        assert!(
            !rects.is_empty(),
            "the commented range computes a highlight rect"
        );
        assert!(
            rects[0].2 > rects[0].0,
            "the highlight rect has positive width"
        );
    }

    // Phase: review_reply_affordance_opens_the_durable_thread.
    {
        let (wp, mut state, c, mut ids, bid) = doc_with_paragraph("hello world");
        let comment;
        {
            let mut dm = tilo_doc::model::DocMut {
                doc: &mut state.doc,
                c: &c,
                ids: &mut ids,
            };
            comment = dm.add_comment(&bid, 0, 5, "Ada", "Could this be clearer?");
            let reply = dm.add_comment_reply(&comment, "Bea", "2026-09-07", "I will revise it.");
            assert!(reply.is_some(), "reply attaches to the stable parent id");
        }
        state.doc.commit();
        let mut framed = mount(state, wp, 1000, 700);
        let (wp, state) = Framed::split(&mut framed);
        assert_eq!(wp.review.comments()[0].replies.len(), 1);

        wp.dispatch_review_action(
            ReviewAction::OpenCommentThread(comment.clone()),
            &mut state.doc,
        );
        assert_eq!(wp.thread_comment.as_deref(), Some(comment.as_str()));
        assert!(
            wp.thread_snapshot.is_none(),
            "interactive panel reads current review data"
        );

        // The reply field is the SDK editor: plain Enter starts a paragraph,
        // Ctrl+Enter sends the reply as written, and the field clears.
        let (wp, state) = pipeline_draw(&mut framed, 1000, 700);
        {
            // The thread panel paints in the top layer, which lays out its field.
            let mut tr = TextRenderer::new();
            wp.draw_overlay(
                &mut Scene::new(),
                Affine::IDENTITY,
                &mut tr,
                &Theme::dark(),
                &PresenceCtx::empty(),
            );
        }
        for ch in "Done".chars() {
            type_char(wp, state, ch);
        }
        press_named(wp, state, NamedKey::Enter);
        for ch in "Thanks".chars() {
            type_char(wp, state, ch);
        }
        assert_eq!(
            wp.thread_reply.widget().source().plain_text(),
            "Done\nThanks"
        );
        press_named_with_modifiers(
            wp,
            state,
            NamedKey::Enter,
            Modifiers::from(winit::keyboard::ModifiersState::CONTROL),
        );
        let after = snapshot(state, &c);
        let thread = after.comments.iter().find(|cm| cm.id == comment).unwrap();
        assert_eq!(thread.replies.len(), 2, "the reply was sent");
        let reply = thread
            .replies
            .iter()
            .find(|reply| reply.author != "Bea")
            .unwrap();
        let sent: Vec<&str> = reply
            .blocks
            .iter()
            .map(|block| block.text.as_str())
            .collect();
        assert_eq!(sent, ["Done", "Thanks"], "sent as its paragraphs");
        assert!(
            wp.thread_reply.widget().source().is_empty(),
            "the field clears"
        );
        assert_eq!(wp.thread_comment.as_deref(), Some(comment.as_str()));

        // Escape in the field closes the thread.
        press_named(wp, state, NamedKey::Escape);
        assert!(wp.thread_comment.is_none());
    }

    // Phase: deleting_comment_removes_it_and_clears_the_highlight.
    {
        // "hello world" with a comment over "world" (chars 6..11).
        let (wp, mut state, c, mut ids, bid) = doc_with_paragraph("hello world");
        let cid = {
            let mut dm = tilo_doc::DocMut {
                doc: &mut state.doc,
                c: &c,
                ids: &mut ids,
            };
            dm.add_comment(&bid, 6, 11, "Ada", "Reword this")
        };
        state.doc.commit();
        let mut framed = mount(state, wp, 1000, 900);
        let (wp, state) = Framed::split(&mut framed);

        // Before: the comment is in the snapshot and its highlight range is present.
        let before = snapshot(state, &c);
        assert!(
            before.comments.iter().any(|cm| cm.id == cid),
            "the comment exists before resolve"
        );
        let b = before.blocks.iter().find(|b| b.id == bid).unwrap();
        assert_eq!(
            block_comment_ranges(b).len(),
            1,
            "the highlight range exists before resolve"
        );

        // Resolve (delete) via the production review-dispatch path.
        wp.dispatch_review_action(ReviewAction::ResolveComment(cid.clone()), &mut state.doc);

        // After: gone from `comments`, and the anchoring `comment` mark (the
        // highlight band) is cleared from the block. Body text is untouched.
        let after = snapshot(state, &c);
        let b = after.blocks.iter().find(|b| b.id == bid).unwrap();
        assert!(
            !after.comments.iter().any(|cm| cm.id == cid),
            "the comment is removed from the snapshot after resolve"
        );
        assert!(
            block_comment_ranges(b).is_empty(),
            "the comment highlight is cleared after resolve"
        );
        assert_eq!(
            b.text, "hello world",
            "resolving a comment leaves the commented text intact"
        );
    }
}

/// Tracked inserts underline in colour and deletes strike, and accepting an insertion clears its mark
/// while accepting a deletion removes the struck text.
#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one scenario test: each phase is a former standalone test kept verbatim as a scoped block"
)]
fn tracked_changes_render_and_accept_individually() {
    // Phase: tracked_insert_underlines_colored_and_delete_strikes.
    {
        // "keep gone": flag "keep" (0..4) as a tracked insertion and "gone" (5..9)
        // as a tracked deletion, via the production tracked-change helpers.
        let (wp, mut state, c, mut ids, bid) = doc_with_paragraph("keep gone");
        let (ins_id, del_id);
        {
            let mut dm = tilo_doc::DocMut {
                doc: &mut state.doc,
                c: &c,
                ids: &mut ids,
            };
            // "keep" is already in the block; mark it as a tracked insertion.
            ins_id = dm.insert_change(tilo_doc::ChangeKind::Ins, "Ada", "");
            dm.add_change_mark(tilo_doc::ChangeKind::Ins, &bid, 0, 4, &ins_id);
            // "gone" stays in the doc but is flagged for deletion.
            del_id = dm.track_delete(&bid, 5, 9, "Ada", "");
        }
        state.doc.commit();
        let mut framed = mount(state, wp, 1000, 900);
        let (wp, _) = Framed::split(&mut framed);

        // Both changes materialize with their kinds.
        assert_eq!(
            wp.review.changes().len(),
            2,
            "both tracked changes materialized"
        );
        let ins = wp
            .review
            .changes()
            .iter()
            .find(|ch| ch.id == ins_id)
            .expect("ins change");
        let del = wp
            .review
            .changes()
            .iter()
            .find(|ch| ch.id == del_id)
            .expect("del change");
        assert_eq!(ins.kind, tilo_doc::ChangeKind::Ins);
        assert_eq!(del.kind, tilo_doc::ChangeKind::Del);

        let b = wp
            .blocks
            .iter()
            .find(|b| b.id == bid)
            .expect("block present");

        // Decoration ranges (the production draw path): ins → underline, del → strike.
        let (ins_ranges, del_ranges) = block_change_decos(b);
        assert_eq!(
            ins_ranges,
            vec![(0, 4)],
            "the insertion range underlines 'keep'"
        );
        assert_eq!(
            del_ranges,
            vec![(5, 9)],
            "the deletion range strikes 'gone'"
        );

        // Both decoration ranges resolve to real on-screen rects via the layout.
        let (_, lay) = wp.layout_cache.get(&bid).expect("shaped layout cached");
        assert!(
            !lay.selection_rects(0, 4).is_empty(),
            "insertion underline has geometry"
        );
        assert!(
            !lay.selection_rects(5, 9).is_empty(),
            "deletion strike has geometry"
        );

        // Glyph tinting: `block_runs_markup` colours the ins run green and the del
        // run red (the standard review styling). With markup OFF those runs are
        // plain (no review colour).
        let (runs_on, _, _) = block_runs_markup(b, true);
        assert!(
            runs_on
                .iter()
                .any(|r| r.text.contains("keep") && r.color == Some(INSERTION_RGB)),
            "the inserted run is tinted with the insertion review colour"
        );
        assert!(
            runs_on
                .iter()
                .any(|r| r.text.contains("gone") && r.color == Some(DELETION_RGB)),
            "the deleted run is tinted with the deletion review colour"
        );
        let (runs_off, _, _) = block_runs_markup(b, false);
        assert!(
            runs_off
                .iter()
                .all(|r| r.color != Some(INSERTION_RGB) && r.color != Some(DELETION_RGB)),
            "with markup hidden, tracked-change runs render as plain text"
        );
    }

    // Phase: accepting_tracked_insertion_clears_its_ins_mark.
    {
        // "keep gone": flag "keep" (0..4) as a tracked insertion.
        let (wp, mut state, c, mut ids, bid) = doc_with_paragraph("keep gone");
        let ins_id = {
            let mut dm = tilo_doc::DocMut {
                doc: &mut state.doc,
                c: &c,
                ids: &mut ids,
            };
            let id = dm.insert_change(tilo_doc::ChangeKind::Ins, "Ada", "");
            dm.add_change_mark(tilo_doc::ChangeKind::Ins, &bid, 0, 4, &id);
            id
        };
        state.doc.commit();
        let mut framed = mount(state, wp, 1000, 900);
        let (wp, state) = Framed::split(&mut framed);

        // Before: the change is reported in the materialized snapshot, and the
        // block carries a decoration range over "keep".
        let before = snapshot(state, &c);
        assert!(
            before.changes.iter().any(|ch| ch.id == ins_id),
            "the insertion is a tracked change before accept"
        );
        let b = before.blocks.iter().find(|b| b.id == bid).unwrap();
        assert_eq!(
            block_change_decos(b).0,
            vec![(0, 4)],
            "the insertion underlines 'keep' before accept"
        );

        // Accept via the production review-dispatch path (the same call the
        // margin ✓ button + context menu use).
        wp.dispatch_review_action(ReviewAction::AcceptChange(ins_id.clone()), &mut state.doc);

        // After: the text is kept, but the `ins` mark + change record are gone —
        // the run is no longer reported as a tracked change anywhere.
        let after = snapshot(state, &c);
        let b = after.blocks.iter().find(|b| b.id == bid).unwrap();
        assert_eq!(
            b.text, "keep gone",
            "accepting an insertion keeps the inserted text"
        );
        assert!(
            !after.changes.iter().any(|ch| ch.id == ins_id),
            "the change record is dropped after accept"
        );
        assert!(
            block_change_decos(b).0.is_empty(),
            "no `ins` decoration remains after accept"
        );
        assert!(
            !b.marks.iter().any(|m| m.name == tilo_doc::schema::MARK_INS),
            "the `ins` mark is cleared from the block after accept"
        );
    }

    // Phase: accepting_tracked_deletion_removes_the_struck_text.
    {
        // "keep gone": flag "gone" (5..9) as a tracked deletion (retained-but-struck).
        // qdoc semantics: ACCEPTING a deletion applies it (removes the struck text);
        // REJECTING keeps the text and drops the `del` mark. We assert both arms
        // on the materialized snapshot (the production read path).
        let (wp, mut state, c, mut ids, bid) = doc_with_paragraph("keep gone");
        let del_id = {
            let mut dm = tilo_doc::DocMut {
                doc: &mut state.doc,
                c: &c,
                ids: &mut ids,
            };
            dm.track_delete(&bid, 5, 9, "Ada", "")
        };
        state.doc.commit();
        let mut framed = mount(state, wp, 1000, 900);
        let (wp, state) = Framed::split(&mut framed);

        // Before: the struck text is still present + reported as a change.
        let before = snapshot(state, &c);
        assert!(
            before.changes.iter().any(|ch| ch.id == del_id),
            "the deletion is a tracked change before accept"
        );
        assert_eq!(
            before.blocks.iter().find(|b| b.id == bid).unwrap().text,
            "keep gone"
        );

        // Accept the deletion: the struck text is removed.
        wp.dispatch_review_action(ReviewAction::AcceptChange(del_id.clone()), &mut state.doc);

        let after = snapshot(state, &c);
        let b = after.blocks.iter().find(|b| b.id == bid).unwrap();
        assert_eq!(
            b.text, "keep ",
            "accepting a deletion removes the struck text"
        );
        assert!(
            !after.changes.iter().any(|ch| ch.id == del_id),
            "the change record is dropped after accept"
        );
        assert!(
            block_change_decos(b).1.is_empty(),
            "no `del` strike decoration remains after accept"
        );
    }
}

/// Re-materialize the document straight from the CRDT (the production
/// snapshot path) so a test asserts on what every reader sees — not on the
/// widget's cached copy.
fn snapshot(state: &ui::State, c: &tilo_doc::Containers) -> tilo_doc::snapshot::DocSnapshot {
    tilo_doc::model::materialize(&state.doc, c)
}

/// Rejecting a tracked deletion keeps the text and drops the mark, and accept all clears every tracked
/// change.
#[test]
fn tracked_changes_reject_and_accept_all() {
    // Phase: rejecting_tracked_deletion_keeps_the_text_and_drops_the_mark.
    {
        // The inverse arm: rejecting a tracked deletion keeps the text (the author
        // chose NOT to delete) and clears the `del` strike.
        let (wp, mut state, c, mut ids, bid) = doc_with_paragraph("keep gone");
        let del_id = {
            let mut dm = tilo_doc::DocMut {
                doc: &mut state.doc,
                c: &c,
                ids: &mut ids,
            };
            dm.track_delete(&bid, 5, 9, "Ada", "")
        };
        state.doc.commit();
        let mut framed = mount(state, wp, 1000, 900);
        let (wp, state) = Framed::split(&mut framed);

        wp.dispatch_review_action(ReviewAction::RejectChange(del_id.clone()), &mut state.doc);

        let after = snapshot(state, &c);
        let b = after.blocks.iter().find(|b| b.id == bid).unwrap();
        assert_eq!(b.text, "keep gone", "rejecting a deletion keeps the text");
        assert!(
            !after.changes.iter().any(|ch| ch.id == del_id),
            "the change record is dropped after reject"
        );
        assert!(
            block_change_decos(b).1.is_empty(),
            "the `del` strike decoration is cleared after reject"
        );
    }

    // Phase: accept_all_clears_every_tracked_change.
    {
        // Two changes: an insertion over "keep" and a deletion over "gone".
        let (wp, mut state, c, mut ids, bid) = doc_with_paragraph("keep gone");
        {
            let mut dm = tilo_doc::DocMut {
                doc: &mut state.doc,
                c: &c,
                ids: &mut ids,
            };
            let ins = dm.insert_change(tilo_doc::ChangeKind::Ins, "Ada", "");
            dm.add_change_mark(tilo_doc::ChangeKind::Ins, &bid, 0, 4, &ins);
            dm.track_delete(&bid, 5, 9, "Ada", "");
        }
        state.doc.commit();
        let mut framed = mount(state, wp, 1000, 900);
        let (wp, state) = Framed::split(&mut framed);
        assert_eq!(
            snapshot(state, &c).changes.len(),
            2,
            "two tracked changes before Accept All"
        );

        wp.dispatch_review_action(ReviewAction::AcceptAllChanges, &mut state.doc);

        // Every change is gone; accepting the insertion kept "keep" and accepting
        // the deletion removed "gone".
        let after = snapshot(state, &c);
        assert!(
            after.changes.is_empty(),
            "no tracked changes remain after Accept All"
        );
        let b = after.blocks.iter().find(|b| b.id == bid).unwrap();
        assert_eq!(
            b.text, "keep ",
            "Accept All keeps insertions and applies deletions"
        );
        assert!(
            b.marks
                .iter()
                .all(|m| m.name != tilo_doc::schema::MARK_INS
                    && m.name != tilo_doc::schema::MARK_DEL)
        );
    }
}

/// Margin card buttons register review hits and the markup toggle hides comment ranges and changes.
#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one scenario test: each phase is a former standalone test kept verbatim as a scoped block"
)]
fn review_markup_registers_card_hits_and_toggles_off() {
    // Phase: margin_card_buttons_register_review_hits.
    {
        // A drawn frame with one comment + one tracked change populates clickable
        // affordances in `review_hits` (the production hit list the left-click
        // handler dispatches through).
        let (wp, mut state, c, mut ids, bid) = doc_with_paragraph("keep gone");
        let (cid, ins_id);
        {
            let mut dm = tilo_doc::DocMut {
                doc: &mut state.doc,
                c: &c,
                ids: &mut ids,
            };
            cid = dm.add_comment(&bid, 0, 4, "Ada", "look");
            ins_id = dm.insert_change(tilo_doc::ChangeKind::Ins, "Ada", "");
            dm.add_change_mark(tilo_doc::ChangeKind::Ins, &bid, 5, 9, &ins_id);
        }
        state.doc.commit();
        let mut framed = mount(state, wp, 1200, 900);
        let (wp, state) = Framed::split(&mut framed);

        let has = |pred: &dyn Fn(&ReviewAction) -> bool| {
            wp.review
                .hits()
                .iter()
                .any(|(r, a)| pred(a) && r.width() > 0.0 && r.height() > 0.0)
        };
        assert!(
            has(&|a| matches!(a, ReviewAction::ResolveComment(id) if id == &cid)),
            "a Resolve affordance for the comment"
        );
        assert!(
            has(&|a| matches!(a, ReviewAction::OpenCommentThread(id) if id == &cid)),
            "a Reply affordance opens the durable comment thread"
        );
        assert!(
            has(&|a| matches!(a, ReviewAction::AcceptChange(id) if id == &ins_id)),
            "an Accept ✓ affordance for the change"
        );
        assert!(
            has(&|a| matches!(a, ReviewAction::RejectChange(id) if id == &ins_id)),
            "a Reject ✗ affordance for the change"
        );

        let tree = wp.accessibility_tree();
        let review = tree
            .children
            .iter()
            .find(|child| {
                child.node.role == ui::a11y::Role::Group
                    && child.node.name.as_deref() == Some(t("a11y-group-review").as_str())
            })
            .expect("painted review controls are exported as one semantic group");
        assert_eq!(
            review.children.len(),
            4,
            "comment cards expose Reply and Resolve; change cards expose Accept and Reject"
        );
        assert!(review.children.iter().all(|child| {
            child.node.role == ui::a11y::Role::Button
                && child.node.bounds.is_some()
                && child.node.action_target.is_some()
                && child.node.state.focusable
        }));

        let resolve_target = review_action_target(&ReviewAction::ResolveComment(cid.clone()));
        let root = state.root.clone();
        assert!(Widget::perform_action_at(
            wp,
            &ui::a11y::Action::Activate,
            &[ui::a11y::hash_key(&resolve_target)],
            &mut state.doc,
            &root,
        ));
        assert!(
            snapshot(state, &c)
                .comments
                .iter()
                .all(|comment| comment.id != cid),
            "screen-reader activation resolves through the typed document action"
        );
    }

    // Phase: markup_toggle_hides_comment_ranges_and_changes.
    {
        let (wp, mut state, c, mut ids, bid) = doc_with_paragraph("hello world");
        {
            let mut dm = tilo_doc::DocMut {
                doc: &mut state.doc,
                c: &c,
                ids: &mut ids,
            };
            dm.add_comment(&bid, 0, 5, "Ada", "note");
            let ci = dm.insert_change(tilo_doc::ChangeKind::Ins, "Ada", "");
            dm.add_change_mark(tilo_doc::ChangeKind::Ins, &bid, 6, 11, &ci);
        }
        state.doc.commit();

        // Default ON: a comment marker is collected for the right-margin card.
        assert!(wp.markup_enabled(), "markup defaults ON");
        let mut framed = mount(state, wp, 1000, 900);
        let (wp, _) = Framed::split(&mut framed);
        let b = wp.blocks.iter().find(|b| b.id == bid).unwrap().clone();
        assert_eq!(
            block_comment_ranges(&b).len(),
            1,
            "comment range present in the data"
        );

        // Toggle OFF: the markup-gated runs lose their review colour, proving the
        // draw path suppresses tracked-change styling.
        wp.toggle_markup();
        assert!(!wp.markup_enabled(), "toggle flips the flag");
        let (runs_off, _, _) = block_runs_markup(&b, wp.review.is_visible());
        assert!(
            runs_off.iter().all(|r| r.color != Some(INSERTION_RGB)),
            "no review colour is applied while markup is hidden"
        );
    }
}

// ---- outline / navigation pane + table of contents ----------------------

/// Build a document with several headings (some on later pages) interleaved
/// with body paragraphs, returning `(wp, state)` already drawn at `w×h`.
fn doc_with_headings(w: u32, h: u32) -> Framed<WordProcessorWidget> {
    let mut state = ui::State::new();
    let mut wp = WordProcessorWidget::new();
    wp.sync(&mut state.doc, &state.root);
    let add_heading = |wp: &mut WordProcessorWidget, st: &mut ui::State, text: &str, level: i64| {
        wp.model_mut().insert_str(&mut st.doc, text);
        wp.model_mut()
            .set_paragraph_style(&mut st.doc, "heading", Some(level), None);
        wp.model_mut().enter(&mut st.doc);
        wp.model_mut()
            .set_paragraph_style(&mut st.doc, "paragraph", None, None);
    };
    add_heading(&mut wp, &mut state, "Introduction", 1);
    for i in 0..30 {
        wp.model_mut().insert_str(
            &mut state.doc,
            &format!("Intro body line {i} with enough words to fill the line."),
        );
        wp.model_mut().enter(&mut state.doc);
    }
    add_heading(&mut wp, &mut state, "Background", 2);
    for i in 0..30 {
        wp.model_mut().insert_str(
            &mut state.doc,
            &format!("Background body line {i} with enough words to fill it."),
        );
        wp.model_mut().enter(&mut state.doc);
    }
    add_heading(&mut wp, &mut state, "Conclusion", 1);
    wp.model_mut().insert_str(&mut state.doc, "The end.");
    mount(state, wp, w, h)
}

/// The outline has one entry per heading with levels in order, and generating a TOC inserts one marked
/// block per heading.
#[test]
fn the_outline_and_toc_follow_the_headings() {
    // Phase: outline_has_one_entry_per_heading_with_levels_in_order.
    {
        let wp = doc_with_headings(1000, 700);
        let entries = wp.outline_entries();
        assert_eq!(
            entries.iter().map(|e| e.text.as_str()).collect::<Vec<_>>(),
            vec!["Introduction", "Background", "Conclusion"],
            "one entry per heading in document order"
        );
        assert_eq!(
            entries.iter().map(|e| e.level).collect::<Vec<_>>(),
            vec![1, 2, 1],
            "levels preserved"
        );
    }

    // Phase: generate_toc_inserts_one_marked_block_per_heading.
    {
        let mut framed = doc_with_headings(1000, 700);
        let (wp, state) = Framed::split(&mut framed);
        let heading_count = wp.outline_entries().len();
        wp.apply_tool(&mut state.doc, ToolAction::InsertToc);

        let c = wp.model().containers.clone().unwrap();
        let snap = tilo_doc::model::materialize(&state.doc, &c);
        let toc: Vec<_> = snap
            .blocks
            .iter()
            .filter(|b| crate::word_processor::block_is_toc(b))
            .collect();
        assert_eq!(
            toc.len(),
            heading_count,
            "one toc-marked block per heading (one line each)"
        );
        // The TOC sits at the document top, in heading order, each line carrying
        // the heading text.
        assert!(
            snap.blocks[0].text.contains("Introduction"),
            "TOC begins at the top with the first heading"
        );
        assert!(
            toc.iter().any(|b| b.text.contains("Conclusion")),
            "every heading is listed"
        );
        assert!(wp.lifecycle.needs_commit, "TOC generation is an undo point");
    }
}

/// Drive a real left-click at `pt` through the production `handle_event`.
pub(super) fn left_click(wp: &mut WordProcessorWidget, state: &mut ui::State, pt: Point) {
    let taffy: TaffyTree<()> = TaffyTree::new();
    let node = NodeId::new(0);
    let mods = Modifiers::default();
    let mut handled = false;
    let mut focus: Option<NodeId> = None;
    // Move the pointer, then press.
    wp.handle_event(
        &WindowEvent::CursorMoved {
            device_id: unsafe { std::mem::zeroed() },
            position: (pt.x, pt.y).into(),
        },
        &mut ui::widget::EventCtx {
            scale_factor: 1.0,
            modifiers: &mods,
            state: &mut state.doc,
            path: &state.root,
            handled: &mut handled,
            focus_target: &mut focus,
            node_id: node,
            taffy: &taffy,
        },
    );
    wp.handle_event(
        &WindowEvent::MouseInput {
            device_id: unsafe { std::mem::zeroed() },
            state: ElementState::Pressed,
            button: MouseButton::Left,
        },
        &mut ui::widget::EventCtx {
            scale_factor: 1.0,
            modifiers: &mods,
            state: &mut state.doc,
            path: &state.root,
            handled: &mut handled,
            focus_target: &mut focus,
            node_id: node,
            taffy: &taffy,
        },
    );
}

#[test]
fn clicking_an_outline_entry_scrolls_the_heading_into_view() {
    let mut framed = doc_with_headings(1000, 700);
    let (wp, state) = Framed::split(&mut framed);
    // Open the outline pane and redraw so its clickable entries are built.
    wp.apply_tool(&mut state.doc, ToolAction::ToggleOutline);
    assert!(wp.outline.is_open(), "outline toggled on");
    let (wp, state) = pipeline_draw(&mut framed, 1000, 700);

    // The last heading ("Conclusion") is far down; it should be off-screen at
    // the top of the document.
    let entries = wp.outline_entries();
    let (entry_index, concl) = entries
        .iter()
        .enumerate()
        .find(|(_, heading)| heading.text == "Conclusion")
        .expect("conclusion heading");
    assert_eq!(wp.scroll_y, 0.0, "starts scrolled to the top");

    // The pane lays entries immediately below its header. Click the centre
    // of the selected row through the production event path rather than
    // reaching into the component's private hit map.
    let row_y =
        wp.toolbar_height(1000.0) + 14.0 + 18.0 + num::f64_of_usize(entry_index) * 24.0 + 12.0;
    let pt = Point::new(30.0, row_y);
    left_click(wp, state, pt);
    assert!(
        wp.scroll_y > 0.0,
        "clicking a far-down heading scrolls the document"
    );

    // After redrawing at the new scroll, the heading is laid within the canvas.
    let (wp, _) = pipeline_draw(&mut framed, 1000, 700);
    let canvas_top = wp.toolbar_height(1000.0);
    // The bottom status strip is gone (its readout floats as a bubble); the
    // canvas now runs down to the top of the layout bar.
    let canvas_bottom = wp.rect.y1 - LAYOUT_BAR_H;
    let laid = wp
        .laid
        .iter()
        .find(|l| l.id == concl.id)
        .expect("conclusion is laid out after scrolling to it");
    assert!(
        laid.y_top >= canvas_top && laid.y_top <= canvas_bottom,
        "the clicked heading is visible in the canvas (y_top={}, canvas=[{canvas_top},{canvas_bottom}])",
        laid.y_top
    );
}

/// Regenerating the TOC replaces rather than duplicates, and the TOC action sets a confirmation toast.
#[test]
fn regenerating_the_toc_replaces_it_and_confirms() {
    // Phase: regenerating_toc_replaces_rather_than_duplicating.
    {
        let mut framed = doc_with_headings(1000, 700);
        let (wp, state) = Framed::split(&mut framed);
        let heading_count = wp.outline_entries().len();
        wp.apply_tool(&mut state.doc, ToolAction::InsertToc);
        let (wp, state) = pipeline_draw(&mut framed, 1000, 700);
        // Regenerate.
        wp.apply_tool(&mut state.doc, ToolAction::InsertToc);

        let c = wp.model().containers.clone().unwrap();
        let snap = tilo_doc::model::materialize(&state.doc, &c);
        let toc_count = snap
            .blocks
            .iter()
            .filter(|b| crate::word_processor::block_is_toc(b))
            .count();
        assert_eq!(
            toc_count, heading_count,
            "regenerating replaces the prior TOC (no duplicate)"
        );
        // The headings themselves are untouched (still derive the same outline).
        assert_eq!(
            wp.outline_entries().len(),
            heading_count,
            "headings unchanged by TOC regeneration"
        );
    }

    // Phase: toc_action_sets_a_confirmation_toast.
    // Generating a TOC leaves a visible confirmation toast (the button used to
    // give no feedback at all).
    {
        let mut framed = doc_with_headings(1000, 700);
        let (wp, state) = Framed::split(&mut framed);
        assert!(wp.toast.is_none(), "no toast before the action");
        wp.apply_tool(&mut state.doc, ToolAction::InsertToc);
        assert!(
            wp.toast.is_some(),
            "generating a TOC surfaces a confirmation toast"
        );
        // And it actually produced TOC lines.
        let c = wp.model().containers.clone().unwrap();
        let toc = tilo_doc::model::materialize(&state.doc, &c)
            .blocks
            .iter()
            .filter(|b| crate::word_processor::block_is_toc(b))
            .count();
        assert!(toc > 0, "the TOC action inserts visible TOC lines");
    }
}

// ---- new-from-template picker -----------------------------------------

/// The materialized first block of the document in `state`.
fn first_block(wp: &WordProcessorWidget, state: &ui::State) -> Option<BlockView> {
    let c = wp.model().containers.clone()?;
    tilo_doc::model::materialize(&state.doc, &c)
        .blocks
        .into_iter()
        .next()
}

/// A blank, bound widget over a fresh `ui::State`.
fn bound() -> (ui::State, WordProcessorWidget) {
    let mut state = ui::State::new();
    let mut wp = WordProcessorWidget::new();
    wp.sync(&mut state.doc, &state.root);
    (state, wp)
}

#[test]
fn hosted_lookup_returns_a_published_child_not_the_document_root() {
    let (state, mut wp) = bound();
    wp.mark_resource_hosted_for_test();
    assert!(ui::resources::resource(&wp, &state.doc, "word-processor.document").is_none());
    let child_id = wp
        .publish_resources(&state.doc)
        .into_iter()
        .find_map(|root| {
            root.children
                .first()
                .map(|child| child.id.as_str().to_owned())
        })
        .expect("the hosted document still publishes a child");
    assert!(
        ui::resources::resource(&wp, &state.doc, &child_id).is_some(),
        "a hosted child stays the published row"
    );
    let hosted = ui::resources::resources(&wp, &state.doc);
    assert!(
        hosted
            .iter()
            .all(|resource| resource.id.as_str() != "word-processor.document"),
        "a hosted window does not publish a second document root"
    );
    let caret = hosted
        .iter()
        .find(|resource| resource.id.as_str() == "word-processor.caret")
        .expect("caret edits stay addressable");
    assert!(
        caret.actions.iter().any(|action| action.id == "type-text"),
        "typing is a caret action, not only a keypress"
    );
    assert!(
        caret
            .actions
            .iter()
            .any(|action| action.id == "place-image"),
        "placing an image is a caret action, not only a toolbar click"
    );
    assert!(
        caret
            .actions
            .iter()
            .any(|action| action.id == "insert-table"),
        "inserting a table is a caret action"
    );
    assert!(
        caret.actions.iter().any(|action| action.id == "move-caret"),
        "moving the caret is a view action"
    );
}

#[test]
fn hosted_hyperlink_opens_on_the_caret_not_the_document_root() {
    let (_state, mut wp) = bound();
    assert_eq!(wp.hyperlink_palette_resource(), "word-processor.document");
    wp.mark_resource_hosted_for_test();
    assert_eq!(wp.hyperlink_palette_resource(), "word-processor.caret");
}

#[test]
fn hosted_selection_publishes_set_hyperlink_on_the_caret() {
    let (mut state, mut wp) = bound();
    wp.inner.insert_str(&mut state.doc, "hello");
    wp.sync(&mut state.doc, &state.root);
    wp.inner.caret.offset = 0;
    wp.inner.set_anchor();
    wp.inner.caret.offset = 5;
    wp.mark_resource_hosted_for_test();
    let hosted = ui::resources::resources(&wp, &state.doc);
    let caret = hosted
        .iter()
        .find(|resource| resource.id.as_str() == "word-processor.caret")
        .expect("caret actions stay addressable");
    assert!(
        caret
            .actions
            .iter()
            .any(|action| action.id == "set-hyperlink"),
        "a hosted selection publishes set-hyperlink on the caret"
    );
}

/// The New menu button is in the toolbar, template menu hover paints feedback in both themes, and the
/// keyboard selects without editing the document.
#[test]
fn the_new_menu_paints_feedback_and_selects_by_keyboard() {
    // Phase: new_menu_button_is_in_the_toolbar.
    // The "New ▾" toolbar button is present and registered as a click target.
    {
        let (state, wp) = bound();
        let mut framed = mount(state, wp, 1200, 700);
        let (wp, _) = Framed::split(&mut framed);
        assert!(
            wp.toolbar.iter().any(|(a, _)| *a == ToolAction::NewMenu),
            "the New ▾ button is a registered toolbar action"
        );
    }

    // Phase: template_menu_hover_paints_feedback_in_both_themes.
    {
        for theme in [Theme::dark(), Theme::light()] {
            let (state, wp) = bound();
            let mut framed = mount_themed(state, wp, 1200, 700, &theme);
            let (wp, state) = Framed::split(&mut framed);
            wp.apply_tool(&mut state.doc, ToolAction::NewMenu);
            let mut tr = TextRenderer::new();
            let mut before = Scene::new();
            wp.draw_overlay(
                &mut before,
                Affine::IDENTITY,
                &mut tr,
                &theme,
                &PresenceCtx::empty(),
            );
            let point = Point::new(
                wp.toolbar.first().unwrap().1.x0 + 10.0,
                wp.rect.y0 + wp.toolbar_height(wp.rect.width()) + 6.0 + 28.0 * 2.0 + 10.0,
            );
            let mut handled = false;
            wp.lifecycle.dirty = false;
            wp.handle_event(
                &WindowEvent::CursorMoved {
                    device_id: unsafe { std::mem::zeroed() },
                    position: (point.x * 2.0, point.y * 2.0).into(),
                },
                &mut ui::widget::EventCtx {
                    scale_factor: 2.0,
                    modifiers: &Modifiers::default(),
                    state: &mut state.doc,
                    path: &state.root,
                    handled: &mut handled,
                    focus_target: &mut None,
                    node_id: NodeId::new(0),
                    taffy: &TaffyTree::new(),
                },
            );
            let mut after = Scene::new();
            wp.draw_overlay(
                &mut after,
                Affine::IDENTITY,
                &mut tr,
                &theme,
                &PresenceCtx::empty(),
            );
            assert!(
                after.encoding().path_tags.len() > before.encoding().path_tags.len(),
                "hovering a template must paint a row highlight"
            );
            assert!(wp.lifecycle.dirty, "hover must request a repaint");
            if let Some(dir) = std::env::var_os("TILO_WP_REVIEW_SHOTS") {
                render_review_png(
                    &mut framed,
                    1200,
                    700,
                    std::path::Path::new(&dir),
                    "template-hover-dark",
                );
            }
        }
    }

    // Phase: template_menu_keyboard_selects_without_editing_the_document.
    {
        let (mut state, mut wp) = bound();
        wp.model_mut().insert_str(&mut state.doc, "Draft");
        wp.refresh_blocks(&mut state.doc);
        let mut framed = mount(state, wp, 1200, 700);
        let (wp, state) = Framed::split(&mut framed);
        wp.lifecycle.focused = true;
        wp.apply_tool(&mut state.doc, ToolAction::NewMenu);
        press_named(wp, state, NamedKey::Backspace);
        assert_eq!(first_block(wp, state).map(|b| b.text), Some("Draft".into()));
        for _ in 0..3 {
            press_named(wp, state, NamedKey::ArrowDown);
        }
        press_named(wp, state, NamedKey::Enter);
        assert!(
            !wp.template_picker.is_open(),
            "Enter chooses and closes the menu"
        );
        assert_eq!(
            first_block(wp, state).map(|b| b.text),
            Some("Report Title".into())
        );
    }
}

/// Pointer selection and dismissal use the overlay, a new template starts untitled without overwriting
/// the old file, and selecting a template replaces the document content.
#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one scenario test: each phase is a former standalone test kept verbatim as a scoped block"
)]
fn templates_replace_the_document_only_when_chosen() {
    // Phase: template_menu_pointer_selection_and_dismissal_use_the_overlay.
    {
        let (state, wp) = bound();
        let mut framed = mount(state, wp, 1200, 700);
        let (wp, state) = Framed::split(&mut framed);
        wp.apply_tool(&mut state.doc, ToolAction::NewMenu);
        let report = wp
            .template_picker
            .item_center(crate::Template::Report)
            .expect("Report has a live menu row");
        left_click(wp, state, report);
        assert!(!wp.template_picker.is_open());
        assert_eq!(
            first_block(wp, state).map(|b| b.text),
            Some("Report Title".into())
        );

        wp.apply_tool(&mut state.doc, ToolAction::NewMenu);
        assert_eq!(
            wp.template_picker.menu_mut().hovered(),
            None,
            "reopening clears the old highlight"
        );
        wp.lifecycle.focused = false;
        press_named(wp, state, NamedKey::Escape);
        assert!(
            !wp.template_picker.is_open(),
            "Escape works even without editor focus"
        );

        wp.apply_tool(&mut state.doc, ToolAction::NewMenu);
        let caret = wp.inner.caret.clone();
        left_click(wp, state, Point::new(900.0, 500.0));
        assert!(!wp.template_picker.is_open());
        assert_eq!(
            wp.inner.caret, caret,
            "dismissal must not click the document beneath"
        );
    }

    // Phase: file_ux_new_template_starts_untitled_without_overwriting_the_old_file.
    {
        let (mut state, mut wp) = bound();
        let old_path =
            std::env::temp_dir().join(format!("wp-template-original-{}.md", std::process::id()));
        std::fs::write(&old_path, "Original document").unwrap();
        wp.try_open_document_in_place(&mut state.doc, &old_path)
            .unwrap();
        wp.apply_template(&mut state.doc, crate::Template::Report);
        assert!(
            wp.path.is_none(),
            "New must ask for a destination on its first Save"
        );
        assert_eq!(wp.format, Some(WpFormat::Markdown));
        assert_eq!(wp.save_status, DocumentSaveStatus::Modified);
        assert_eq!(
            std::fs::read_to_string(&old_path).unwrap(),
            "Original document"
        );
        let _ = std::fs::remove_file(old_path);
    }

    // Phase: selecting_a_template_replaces_the_document_content.
    // Selecting from the "New ▾" picker replaces the document content with that
    // template's first heading through the production pointer-routing path.
    {
        let (mut state, mut wp) = bound();
        // Author some pre-existing content so we can prove it's actually replaced.
        wp.model_mut()
            .insert_str(&mut state.doc, "Pre-existing rough draft");
        wp.refresh_blocks(&mut state.doc);
        assert_eq!(
            first_block(&wp, &state).map(|b| b.text),
            Some("Pre-existing rough draft".to_string()),
            "the document starts with the user's own text"
        );

        // Draw once to establish the real toolbar anchor, then open the picker
        // via the toolbar action.
        let mut framed = mount(state, wp, 1200, 700);
        let (wp, state) = Framed::split(&mut framed);
        wp.apply_tool(&mut state.doc, ToolAction::NewMenu);
        assert!(wp.template_picker.is_open(), "the New ▾ picker opens");

        // Select "Report" (its first heading is a known, stable string) using
        // the shared menu's current layout rather than copied row metrics.
        wp.pointer = wp
            .template_picker
            .item_center(crate::Template::Report)
            .expect("Report row is laid out while the picker is open");
        let mut handled = false;
        assert!(wp.handle_left_press_menu_routing(&mut state.doc, &mut handled));
        assert!(handled);
        assert!(
            !wp.template_picker.is_open(),
            "the picker closes after a selection"
        );

        let first = first_block(wp, state).expect("the templated document has a first block");
        assert_eq!(
            first.kind, "heading",
            "the report template opens with a heading"
        );
        assert_eq!(
            first.text, "Report Title",
            "content is replaced by the template's first heading"
        );
        // The old draft text is gone — the body is the template, not an append.
        let c = wp.model().containers.clone().unwrap();
        let all_text: String = tilo_doc::model::materialize(&state.doc, &c)
            .blocks
            .iter()
            .map(|b| b.text.clone())
            .collect();
        assert!(
            !all_text.contains("rough draft"),
            "the prior content was replaced, not appended"
        );
        assert!(
            wp.save_status == DocumentSaveStatus::Modified,
            "a template swap marks the document modified"
        );
    }
}

// ---- autosave / crash-recovery ----------------------------------------

fn temp_dir(tag: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "editor-wp-recovery-{tag}-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    dir
}

/// Drive a real keypress (single typed character) through the production
/// `handle_event` keyboard path. winit's `KeyEvent` has a private
/// `platform_specific` field, so (per the shell's `e2e_shell_flows` pattern)
/// zero-init the whole struct and overwrite the public fields we read.
fn type_char(wp: &mut WordProcessorWidget, state: &mut ui::State, ch: char) {
    use winit::keyboard::{NativeKeyCode, PhysicalKey, SmolStr};
    let taffy: TaffyTree<()> = TaffyTree::new();
    let node = NodeId::new(0);
    let mods = Modifiers::default();
    let mut handled = false;
    let mut focus: Option<NodeId> = None;
    // SAFETY: identical to the shell's KeyEvent synthesis — all public fields
    // tolerate the zero bit pattern (Option → None, enums → first variant),
    // and we overwrite the ones `handle_event` reads (`logical_key`/`text`/
    // `state`). The private `platform_specific` field is plain/zero-sized.
    let mut ev: KeyEvent = unsafe { std::mem::zeroed() };
    ev.physical_key = PhysicalKey::Unidentified(NativeKeyCode::Unidentified);
    ev.logical_key = Key::Character(SmolStr::new(ch.to_string()));
    ev.text = Some(SmolStr::new(ch.to_string()));
    ev.location = winit::keyboard::KeyLocation::Standard;
    ev.state = ElementState::Pressed;
    ev.repeat = false;
    wp.handle_event(
        &WindowEvent::KeyboardInput {
            device_id: unsafe { std::mem::zeroed() },
            event: ev,
            is_synthetic: false,
        },
        &mut ui::widget::EventCtx {
            scale_factor: 1.0,
            modifiers: &mods,
            state: &mut state.doc,
            path: &state.root,
            handled: &mut handled,
            focus_target: &mut focus,
            node_id: node,
            taffy: &taffy,
        },
    );
}

/// Recovery reuses fresh complete views and falls back for stale ones, autosave writes recovery and save
/// clears it, check-for-recovery surfaces the banner and restore loads it, discard removes the file and
/// banner, and the recovery path is stable and per document.
#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one scenario test: each phase is a former standalone test kept verbatim as a scoped block"
)]
fn recovery_autosaves_restores_and_discards_per_document() {
    // Phase: recovery_reuses_fresh_complete_views_and_falls_back_for_stale_or_partial_views.
    {
        for case in ["fresh", "stale", "partial"] {
            let dir = temp_dir(&format!("recovery-views-{case}"));
            // A partial view needs a body longer than the viewport window.
            let (mut state, mut wp) = if case == "partial" {
                let mut state = large_body();
                let mut wp = WordProcessorWidget::new();
                wp.sync(&mut state.doc, &state.root);
                (state, wp)
            } else {
                bound()
            };
            wp.set_recovery_dir(dir.clone());
            wp.inner.insert_str(&mut state.doc, "Unicode café recovery");
            wp.refresh_blocks(&mut state.doc);
            if case == "stale" {
                wp.inner.insert_str(&mut state.doc, " latest pending edit");
            } else if case == "partial" {
                assert!(wp.blocks.len() < wp.body_order.len());
            }
            let c = wp.inner.containers.as_ref().unwrap();
            let expected = WpFormat::Markdown
                .export(
                    tilo_doc::DocRef { doc: &state.doc, c },
                    &tilo_content::NoContent,
                )
                .unwrap();
            let scans = tilo_doc::model::ordered_keys_scan_count();
            for _ in 0..AUTOSAVE_EDIT_THRESHOLD {
                wp.note_edit_for_autosave(&mut state.doc);
            }
            assert_eq!(
                std::fs::read(wp.recovery_path()).unwrap(),
                expected,
                "{case}"
            );
            if case == "fresh" {
                assert_eq!(
                    tilo_doc::model::ordered_keys_scan_count(),
                    scans,
                    "fresh complete views must avoid full document materialization"
                );
            }
            let _ = std::fs::remove_dir_all(dir);
        }
    }

    // Phase: autosave_writes_recovery_then_save_clears_it.
    // Autosave: typing past the edit threshold (through the production keyboard
    // path) writes a recovery copy to the widget's recovery dir. A real save then
    // clears it. Asserts on file existence (the production path), not pixels.
    {
        let dir = temp_dir("autosave");
        let (mut state, mut wp) = bound();
        wp.set_recovery_dir(dir.clone());
        // Point the document at a real (temp) save target so `save` succeeds and
        // `recovery_path` is keyed off a stable document path.
        let doc_path = dir.join("doc.md");
        wp.set_file(doc_path.clone(), WpFormat::Markdown);

        let recovery_path = wp.recovery_path();
        assert!(!recovery_path.exists(), "no recovery file before any edits");

        // Type more than AUTOSAVE_EDIT_THRESHOLD characters through `handle_event`.
        for _ in 0..AUTOSAVE_EDIT_THRESHOLD {
            type_char(&mut wp, &mut state, 'x');
        }
        let c = wp.inner.containers.as_ref().unwrap();
        assert_eq!(
            std::fs::read(&recovery_path).unwrap(),
            WpFormat::Markdown
                .export(
                    tilo_doc::DocRef { doc: &state.doc, c },
                    &tilo_content::NoContent
                )
                .unwrap(),
            "the snapshot includes the edit that reached the threshold"
        );
        for _ in 0..2 {
            type_char(&mut wp, &mut state, 'x');
        }
        assert!(
            recovery_path.exists(),
            "a recovery copy is written once the edit threshold is crossed ({})",
            recovery_path.display()
        );
        // The recovery copy is a faithful Markdown snapshot (re-imports our text).
        let recovered = std::fs::read(&recovery_path).expect("read recovery copy");
        assert!(
            String::from_utf8_lossy(&recovered).contains('x'),
            "the recovery copy holds the typed content"
        );

        // A real save supersedes and removes the recovery copy.
        wp.save(&state.doc);
        assert!(doc_path.exists(), "the document saved to its real path");
        assert!(
            !recovery_path.exists(),
            "a successful save clears the recovery copy"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    // Phase: check_for_recovery_surfaces_banner_and_restore_loads_it.
    // `check_for_recovery` surfaces the banner (Restore/Discard) when a recovery
    // copy exists for the open document; Restore re-imports its content in place.
    {
        let dir = temp_dir("restore");
        let (state, mut wp) = bound();
        wp.set_recovery_dir(dir.clone());
        let doc_path = dir.join("notes.md");
        wp.set_file(doc_path, WpFormat::Markdown);

        // Stage a recovery copy on disk (as a previous, crashed session would).
        let rp = wp.recovery_path();
        std::fs::create_dir_all(rp.parent().unwrap()).unwrap();
        std::fs::write(&rp, b"# Recovered Heading\n\nUnsaved work\n").unwrap();

        assert!(
            wp.check_for_recovery(),
            "a present recovery copy stages an offer"
        );
        assert!(wp.recovery.is_active(), "the recovery banner is now active");

        // Draw so the banner's Restore/Discard buttons are registered, then click
        // Restore through the production handler.
        let mut framed = mount(state, wp, 1200, 700);
        let (wp, state) = Framed::split(&mut framed);
        let rrect = wp
            .recovery
            .restore_button_rect()
            .expect("Restore button is drawn");
        left_click(
            wp,
            state,
            Point::new(
                f64::midpoint(rrect.x0, rrect.x1),
                f64::midpoint(rrect.y0, rrect.y1),
            ),
        );

        assert!(
            !wp.recovery.is_active(),
            "the banner is dismissed after Restore"
        );
        assert_eq!(
            first_block(wp, state).map(|b| b.text),
            Some("Recovered Heading".to_string()),
            "Restore re-imported the recovery copy into the live document"
        );
        assert!(!rp.exists(), "the recovery file is consumed by Restore");

        let _ = std::fs::remove_dir_all(&dir);
    }

    // Phase: discard_recovery_removes_the_file_and_banner.
    // Discard dismisses the banner and deletes the recovery file without touching
    // the document.
    {
        let dir = temp_dir("discard");
        let (mut state, mut wp) = bound();
        wp.set_recovery_dir(dir.clone());
        wp.set_file(dir.join("d.md"), WpFormat::Markdown);
        wp.model_mut().insert_str(&mut state.doc, "Keep me");
        wp.refresh_blocks(&mut state.doc);

        let rp = wp.recovery_path();
        std::fs::create_dir_all(rp.parent().unwrap()).unwrap();
        std::fs::write(&rp, b"# Stale\n").unwrap();
        assert!(wp.check_for_recovery());

        let mut framed = mount(state, wp, 1200, 700);
        let (wp, state) = Framed::split(&mut framed);
        let drect = wp
            .recovery
            .discard_button_rect()
            .expect("Discard button is drawn");
        left_click(
            wp,
            state,
            Point::new(
                f64::midpoint(drect.x0, drect.x1),
                f64::midpoint(drect.y0, drect.y1),
            ),
        );

        assert!(!wp.recovery.is_active(), "Discard dismisses the banner");
        assert!(!rp.exists(), "Discard deletes the recovery file");
        assert_eq!(
            first_block(wp, state).map(|b| b.text),
            Some("Keep me".to_string()),
            "Discard leaves the open document untouched"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    // Phase: widget_recovery_path_is_stable_and_per_document.
    // The recovery path is stable for a given document and distinct per document.
    {
        let dir = temp_dir("stable");
        let mut a = WordProcessorWidget::new();
        a.set_recovery_dir(dir.clone());
        a.set_file(std::path::PathBuf::from("/docs/a.md"), WpFormat::Markdown);
        let pa1 = a.recovery_path();
        let pa2 = a.recovery_path();
        assert_eq!(pa1, pa2, "stable across calls for the same document");
        assert!(
            pa1.starts_with(&dir),
            "honours the widget's recovery dir override"
        );

        let mut b = WordProcessorWidget::new();
        b.set_recovery_dir(dir);
        b.set_file(std::path::PathBuf::from("/docs/b.md"), WpFormat::Markdown);
        assert_ne!(
            pa1,
            b.recovery_path(),
            "different documents get different recovery files"
        );
    }
}

// ---- horizontal ruler ----------------------------------------------------

/// Drive a real Tab keypress through the production `handle_event` keyboard
/// path (no `text`, just the named key).
fn press_tab(wp: &mut WordProcessorWidget, state: &mut ui::State) {
    use winit::keyboard::{NativeKeyCode, PhysicalKey};
    let taffy: TaffyTree<()> = TaffyTree::new();
    let node = NodeId::new(0);
    let mods = Modifiers::default();
    let mut handled = false;
    let mut focus: Option<NodeId> = None;
    let mut ev: KeyEvent = unsafe { std::mem::zeroed() };
    ev.physical_key = PhysicalKey::Unidentified(NativeKeyCode::Unidentified);
    ev.logical_key = Key::Named(NamedKey::Tab);
    ev.text = None;
    ev.location = winit::keyboard::KeyLocation::Standard;
    ev.state = ElementState::Pressed;
    ev.repeat = false;
    wp.handle_event(
        &WindowEvent::KeyboardInput {
            device_id: unsafe { std::mem::zeroed() },
            event: ev,
            is_synthetic: false,
        },
        &mut ui::widget::EventCtx {
            scale_factor: 1.0,
            modifiers: &mods,
            state: &mut state.doc,
            path: &state.root,
            handled: &mut handled,
            focus_target: &mut focus,
            node_id: node,
            taffy: &taffy,
        },
    );
}

/// The ruler defaults on and toggles, its markers track margins, indent and zoom, and dragging the left
/// margin marker updates the left margin.
#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one scenario test: each phase is a former standalone test kept verbatim as a scoped block"
)]
fn the_ruler_tracks_margins_indent_and_zoom() {
    // Phase: ruler_defaults_on_and_toggles.
    // The ruler is on by default and toggles off/on via the toolbar action.
    {
        let (mut state, mut wp) = bound();
        assert!(wp.view.ruler_visible, "ruler defaults ON (discoverable)");
        wp.apply_tool(&mut state.doc, ToolAction::ToggleRuler);
        assert!(!wp.view.ruler_visible, "toolbar toggle hides the ruler");
        wp.apply_tool(&mut state.doc, ToolAction::ToggleRuler);
        assert!(wp.view.ruler_visible, "toggles back on");
    }

    // Phase: ruler_marker_positions_track_margins_indent_and_zoom.
    // The ruler computes correct marker x-positions from the margins + the caret
    // paragraph's indent, and those positions scale with zoom about the anchor.
    {
        let (mut state, mut wp) = bound();
        wp.model_mut().insert_str(&mut state.doc, "para");
        // Set a known left/hanging indent + first-line indent on the caret block.
        let block = wp.model().caret().block.clone();
        wp.model_mut()
            .set_block_indent(&mut state.doc, &block, 40.0);
        wp.model_mut()
            .set_block_first_line_indent(&mut state.doc, &block, 18.0);
        let mut framed = mount(state, wp, 1200, 760);
        let (wp, _) = Framed::split(&mut framed);

        let page_x = wp.page_rect.x0;
        let page_w = wp.page_rect.width();
        assert!(
            (page_w - MAX_PAGE_W).abs() < 1e-6,
            "page is the fixed US-Letter width"
        );

        // At zoom 1.0 content-x == screen-x (anchor scale is identity).
        assert!((wp.zoom.factor() - 1.0).abs() < 1e-6, "default zoom");
        let markers = wp.ruler_markers(page_x, page_w);
        let mx = |m: RulerMarker| {
            markers
                .iter()
                .find(|(k, _)| *k == m)
                .map(|(_, x)| *x)
                .unwrap()
        };

        let m = wp.margins;
        assert!(
            (mx(RulerMarker::MarginLeft) - (page_x + m.left)).abs() < 1e-6,
            "left margin marker at the left text edge"
        );
        assert!(
            (mx(RulerMarker::MarginRight) - (page_x + page_w - m.right)).abs() < 1e-6,
            "right margin marker at the right text edge"
        );
        assert!(
            (mx(RulerMarker::IndentLeft) - (page_x + m.left + 40.0)).abs() < 1e-6,
            "left-indent marker offset by the block indent"
        );
        assert!(
            (mx(RulerMarker::IndentFirst) - (page_x + m.left + 40.0 + 18.0)).abs() < 1e-6,
            "first-line marker offset by indent + first-line indent"
        );

        // Zooming in moves the markers further from the anchor by the zoom factor.
        wp.set_zoom(2.0);
        let (wp, _) = pipeline_draw(&mut framed, 1200, 760);
        let anchor = wp.zoom.anchor().x;
        let zoomed = wp.ruler_markers(wp.page_rect.x0, wp.page_rect.width());
        let zx = |m: RulerMarker| {
            zoomed
                .iter()
                .find(|(k, _)| *k == m)
                .map(|(_, x)| *x)
                .unwrap()
        };
        // The left-margin marker's screen position = anchor + (content - anchor)*zoom.
        let content_left = wp.page_rect.x0 + wp.margins.left;
        let expect = anchor + (content_left - anchor) * 2.0;
        assert!(
            (zx(RulerMarker::MarginLeft) - expect).abs() < 1e-6,
            "marker scales with zoom about the anchor"
        );
    }

    // Phase: dragging_left_margin_marker_updates_margins_left.
    // Dragging the left-margin marker on the ruler updates `self.margins.left`
    // (with a live preview) and persists it through the production path.
    {
        let (mut state, mut wp) = bound();
        wp.model_mut().insert_str(&mut state.doc, "para");
        let mut framed = mount(state, wp, 1200, 760);
        let (wp, state) = Framed::split(&mut framed);

        let page_x = wp.page_rect.x0;
        let page_w = wp.page_rect.width();
        let before = wp.margins.left;

        // Grab the left-margin marker (press on its screen x within the band).
        let markers = wp.ruler_markers(page_x, page_w);
        let marker_x = markers
            .iter()
            .find(|(k, _)| *k == RulerMarker::MarginLeft)
            .map(|(_, x)| *x)
            .unwrap();
        wp.pointer = Point::new(
            marker_x,
            f64::midpoint(wp.ruler.rect().y0, wp.ruler.rect().y1),
        );
        wp.ruler_press(&mut state.doc);
        assert_eq!(
            wp.ruler.drag_marker(),
            Some(RulerMarker::MarginLeft),
            "grabbed the left-margin marker"
        );

        // Drag it right (toward the page center): a wider left margin.
        let target_content_x = page_x + 120.0; // 120 px from the page's left edge
        wp.pointer = Point::new(wp.content_x_to_screen(target_content_x), wp.pointer.y);
        wp.ruler_drag_move();
        assert!(
            (wp.margins.left - 120.0).abs() < 1.0,
            "live preview updated margins.left to ~120 (got {})",
            wp.margins.left
        );
        assert_ne!(wp.margins.left, before, "the left margin actually changed");

        // Release persists into the document (a fresh widget reloads the value).
        wp.ruler_drag_release(&mut state.doc);
        assert!(!wp.ruler.is_dragging(), "drag ended on release");
        let mut wp2 = WordProcessorWidget::new();
        wp2.sync(&mut state.doc, &state.root);
        assert!(
            (wp2.margins.left - wp.margins.left).abs() < 1e-6,
            "the dragged left margin persisted to the document"
        );
    }
}

/// Dragging the indent marker sets the block indent and reflows, and clicking the ruler adds a tab stop
/// that Tab advances to.
#[test]
fn ruler_drags_and_clicks_set_indents_and_tab_stops() {
    // Phase: dragging_indent_marker_sets_block_indent_and_reflows.
    // Dragging the left-indent marker sets the caret block's `indent` attr, which
    // reflows the paragraph's text-left (`text_x`) by that amount.
    {
        let (mut state, mut wp) = bound();
        wp.model_mut().insert_str(&mut state.doc, "indent me");
        let mut framed = mount(state, wp, 1200, 760);
        let (wp, state) = Framed::split(&mut framed);
        let block = wp.model().caret().block.clone();
        let x_before = wp.laid.iter().find(|l| l.id == block).unwrap().text_x;

        let page_x = wp.page_rect.x0;
        let page_w = wp.page_rect.width();
        let markers = wp.ruler_markers(page_x, page_w);
        let mxl = markers
            .iter()
            .find(|(k, _)| *k == RulerMarker::IndentLeft)
            .map(|(_, x)| *x)
            .unwrap();
        // The hanging/left-indent wedge lives in the lower third of the band.
        wp.pointer = Point::new(mxl, wp.ruler.rect().y1 - 2.0);
        wp.ruler_press(&mut state.doc);
        assert_eq!(wp.ruler.drag_marker(), Some(RulerMarker::IndentLeft));

        // Drag the indent marker 60 px right of the left text margin.
        let target = page_x + wp.margins.left + 60.0;
        wp.pointer = Point::new(wp.content_x_to_screen(target), wp.pointer.y);
        wp.ruler_drag_move();
        wp.ruler_drag_release(&mut state.doc);

        // The block's `indent` attr was persisted (~60 px).
        let stored = wp.model().block_indent(&state.doc, &block);
        assert!(
            (stored - 60.0).abs() < 1.0,
            "indent persisted to the block attr (got {stored})"
        );

        // A redraw reflows the paragraph's text-left by the new indent.
        let (wp, _) = pipeline_draw(&mut framed, 1200, 760);
        let x_after = wp.laid.iter().find(|l| l.id == block).unwrap().text_x;
        assert!(
            (x_after - x_before - 60.0).abs() < 1.0,
            "text reflowed right by the indent (before {x_before}, after {x_after})"
        );
    }

    // Phase: clicking_ruler_adds_tab_stop_and_tab_advances_to_it.
    // Clicking the ruler in the page span adds a tab stop on the caret paragraph;
    // pressing Tab then advances the caret to it (instead of inserting spaces).
    {
        let (mut state, mut wp) = bound();
        wp.model_mut().insert_str(&mut state.doc, "ab");
        let mut framed = mount(state, wp, 1200, 760);
        let (wp, state) = Framed::split(&mut framed);
        let block = wp.model().caret().block.clone();
        assert!(
            wp.model().block_tab_stops(&state.doc, &block).is_empty(),
            "no tab stops initially"
        );

        // Click on the ruler, well inside the text region (no marker there).
        let page_x = wp.page_rect.x0;
        let tab_content_x = page_x + wp.margins.left + 200.0;
        wp.pointer = Point::new(
            wp.content_x_to_screen(tab_content_x),
            f64::midpoint(wp.ruler.rect().y0, wp.ruler.rect().y1),
        );
        wp.ruler_press(&mut state.doc);
        let stops = wp.model().block_tab_stops(&state.doc, &block);
        assert_eq!(stops.len(), 1, "a tab stop was stored on the block");
        assert!(
            (stops[0] - 200.0).abs() < 2.0,
            "the stop is ~200 px from the left text margin (got {})",
            stops[0]
        );

        // Caret at end of "ab". Tab advances to the stop (records a tab char).
        wp.model_mut().move_end(&state.doc);
        let off_before = wp.model().caret().offset;
        press_tab(wp, state);
        let off_after = wp.model().caret().offset;
        assert_eq!(
            off_after,
            off_before + 1,
            "Tab advanced the caret by one (to the tab stop), not four spaces"
        );
        // The advance is recorded as a single tab char (not spaces).
        assert!(
            wp.model().current_text(&state.doc).contains('\t'),
            "a tab character records the advance"
        );
        assert!(
            !wp.model().current_text(&state.doc).contains("    "),
            "Tab did not fall back to inserting spaces"
        );
    }
}

/// Tab without stops inserts spaces, and dragging a tab marker off removes it.
#[test]
fn tab_stops_insert_spaces_without_stops_and_drag_off_to_remove() {
    // Phase: tab_without_stops_inserts_spaces.
    // With no tab stops, Tab keeps the legacy behaviour (inserts spaces) so the
    // key is never a dead no-op.
    {
        let (mut state, mut wp) = bound();
        wp.model_mut().insert_str(&mut state.doc, "x");
        let mut framed = mount(state, wp, 1200, 760);
        let (wp, state) = Framed::split(&mut framed);
        press_tab(wp, state);
        assert!(
            wp.model().current_text(&state.doc).contains("    "),
            "no stops → Tab inserts spaces as before"
        );
    }

    // Phase: dragging_tab_marker_off_removes_it.
    // Dragging a tab marker below the ruler band removes the stop ("drag off").
    {
        let (mut state, mut wp) = bound();
        wp.model_mut().insert_str(&mut state.doc, "para");
        let block = wp.model().caret().block.clone();
        wp.model_mut()
            .set_block_tab_stops(&mut state.doc, &block, &[150.0]);
        let mut framed = mount(state, wp, 1200, 760);
        let (wp, state) = Framed::split(&mut framed);

        let page_x = wp.page_rect.x0;
        let page_w = wp.page_rect.width();
        let markers = wp.ruler_markers(page_x, page_w);
        let tx = markers
            .iter()
            .find(|(k, _)| matches!(k, RulerMarker::Tab(0)))
            .map(|(_, x)| *x)
            .unwrap();
        wp.pointer = Point::new(tx, f64::midpoint(wp.ruler.rect().y0, wp.ruler.rect().y1));
        wp.ruler_press(&mut state.doc);
        assert!(
            matches!(wp.ruler.drag_marker(), Some(RulerMarker::Tab(0))),
            "grabbed the tab marker"
        );

        // Drag below the band → flagged for removal, then release removes it.
        wp.pointer = Point::new(tx, wp.ruler.rect().y1 + 40.0);
        wp.ruler_drag_move();
        assert!(
            wp.ruler.dropping_tab(),
            "dragging below the band flags drop-off"
        );
        wp.ruler_drag_release(&mut state.doc);
        assert!(
            wp.model().block_tab_stops(&state.doc, &block).is_empty(),
            "the tab stop was removed"
        );
    }
}

// ---- Accessibility ------------------------------------------------------

/// The accessibility root is a multiline document with heading, paragraph, image-with-alt and
/// table-cell nodes.
#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one scenario test: each phase is a former standalone test kept verbatim as a scoped block"
)]
fn the_accessibility_tree_mirrors_the_document_structure() {
    // Phase: accessibility_root_is_a_multiline_document.
    // The accessibility tree's root is a focusable, multiline [`Role::Document`].
    {
        use ui::a11y::Role;
        let mut state = ui::State::new();
        let mut wp = WordProcessorWidget::new();
        wp.sync(&mut state.doc, &state.root);

        let tree = wp.accessibility_tree();
        assert_eq!(tree.node.role, Role::Document);
        assert!(tree.node.state.multiline, "the document is multiline");
        assert!(tree.node.state.focusable, "the document is focusable");

        // The document's `focused` flag is derived from the widget's focus state,
        // not stored separately, so it tracks `set_focus`.
        Widget::set_focus(&mut wp, false);
        assert!(
            !wp.accessibility_tree().node.state.focused,
            "blurred editor → not focused"
        );
        Widget::set_focus(&mut wp, true);
        assert!(
            wp.accessibility_tree().node.state.focused,
            "focused editor → focused"
        );

        // The trait method returns the root *and* its synthetic subtree; this is
        // the exact host path consumed by AccessKit/AT-SPI.
        let trait_root = Widget::accessibility(&wp).expect("editor exposes a document node");
        assert_eq!(trait_root.role, Role::Document);
        assert_eq!(trait_root.name.as_deref(), Some("Document editor"));
        assert_eq!(
            trait_root.children.len(),
            tree.children.len(),
            "Widget::accessibility() exports every private tree child"
        );
    }

    // Phase: accessibility_tree_has_heading_and_paragraph_nodes.
    // A heading + a paragraph yield a heading node (correct level + text) and a
    // paragraph node carrying the paragraph text, under the document root.
    {
        use ui::a11y::Role;
        let mut state = ui::State::new();
        let mut wp = WordProcessorWidget::new();
        wp.sync(&mut state.doc, &state.root);

        // First block: a level-1 heading "Title".
        wp.model_mut().insert_str(&mut state.doc, "Title");
        wp.model_mut()
            .set_paragraph_style(&mut state.doc, "heading", Some(1), None);
        wp.model_mut().enter(&mut state.doc);
        // Second block: a body paragraph.
        wp.model_mut()
            .set_paragraph_style(&mut state.doc, "paragraph", None, None);
        wp.model_mut().insert_str(&mut state.doc, "Body text here");
        wp.sync(&mut state.doc, &state.root);

        let tree = wp.accessibility_tree();

        let heading = tree
            .children
            .iter()
            .find(|c| matches!(c.node.role, Role::Heading { .. }))
            .expect("a heading node is present");
        assert_eq!(
            heading.node.role,
            Role::Heading { level: 1 },
            "level-1 heading"
        );
        assert_eq!(
            heading.node.name.as_deref(),
            Some("Title"),
            "heading carries its text"
        );

        let paragraph = tree
            .children
            .iter()
            .find(|c| c.node.role == Role::Paragraph)
            .expect("a paragraph node is present");
        assert_eq!(
            paragraph.node.value.as_deref(),
            Some("Body text here"),
            "paragraph carries its text",
        );
    }

    // Phase: accessibility_image_node_carries_alt.
    // An image block whose `alt` attr is set (as the ODT/DOCX importers do)
    // yields a [`Role::Image`] node carrying that alt text. Asserted on the
    // production node-builder (`block_access_node`) with a directly-built
    // [`BlockView`], since setting an arbitrary block attr is not on the model's
    // public surface.
    {
        use std::collections::BTreeMap;
        use tilo_doc::snapshot::{AttrVal, BlockView};
        use ui::a11y::Role;

        let mut attrs = BTreeMap::new();
        attrs.insert("blob".to_string(), AttrVal::Str("deadbeef".into()));
        attrs.insert("alt".to_string(), AttrVal::Str("a tiny dot".into()));
        let img = BlockView {
            id: "img1".into(),
            kind: "image".into(),
            text: String::new(),
            marks: Vec::new(),
            attrs,
            children: Vec::new(),
        };

        let node = WordProcessorWidget::block_access_node(&img).expect("image yields a node");
        assert_eq!(node.role, Role::Image);
        assert_eq!(
            node.name.as_deref(),
            Some("a tiny dot"),
            "image node carries its alt text"
        );

        // With no `alt` attr the block's own text (a caption) is the fallback name.
        let mut captioned = img;
        captioned.attrs.remove("alt");
        captioned.text = "Figure 1".into();
        let cap_node = WordProcessorWidget::block_access_node(&captioned).unwrap();
        assert_eq!(
            cap_node.name.as_deref(),
            Some("Figure 1"),
            "caption text is the alt fallback"
        );

        // And a genuinely-inserted image surfaces as an Image node in the full tree.
        let mut state = ui::State::new();
        let mut wp = WordProcessorWidget::new();
        wp.sync(&mut state.doc, &state.root);
        wp.model_mut().insert_str(&mut state.doc, "before");
        wp.model_mut().insert_image_bytes(&mut state.doc, TEST_PNG);
        wp.sync(&mut state.doc, &state.root);
        let tree = wp.accessibility_tree();
        assert!(
            tree.children.iter().any(|c| c.node.role == Role::Image),
            "an inserted image appears as an image node in the document tree",
        );
    }

    // Phase: accessibility_tree_groups_table_cells_under_a_table.
    // A table's cells are surfaced as [`Role::Cell`] children of a single
    // [`Role::Table`] node (not as loose top-level blocks).
    {
        use ui::a11y::Role;
        let mut state = ui::State::new();
        let mut wp = WordProcessorWidget::new();
        wp.sync(&mut state.doc, &state.root);
        wp.model_mut().insert_table(&mut state.doc, 2, 2);
        wp.sync(&mut state.doc, &state.root);

        let tree = wp.accessibility_tree();
        let table = tree
            .children
            .iter()
            .find(|c| c.node.role == Role::Table)
            .expect("a table node is present");
        assert_eq!(table.children.len(), 4, "the 2×2 table has four cell nodes");
        assert!(
            table.children.iter().all(|c| c.node.role == Role::Cell),
            "every table child is a cell",
        );
        // No loose cell nodes leaked to the document root.
        assert!(
            !tree.children.iter().any(|c| c.node.role == Role::Cell),
            "cells are not top-level document children",
        );
    }
}

/// The accessibility tree exposes toolbar buttons and activation uses the real toolbar path.
#[test]
fn accessibility_exposes_and_activates_real_toolbar_buttons() {
    // Phase: accessibility_tree_exposes_toolbar_buttons.
    // Once a frame has been drawn (the toolbar is laid out), the tree carries a
    // [`Role::Toolbar`] group of button nodes with spelled-out accessible names.
    {
        use ui::a11y::Role;

        // Assertions below are literal English button names; pin the locale so
        // the test doesn't depend on the host's `LANG` (nextest runs each test
        // in its own process, so this can't leak into a sibling test).
        crate::i18n::set_locale("en-US");
        let (state, wp) = populated();
        let mut framed = mount(state, wp, 1100, 760);
        let (wp, state) = Framed::split(&mut framed);
        // Re-sync so the tree reflects the drawn toolbar.
        wp.sync(&mut state.doc, &state.root);

        let tree = wp.accessibility_tree();
        let toolbar = tree
            .children
            .iter()
            .find(|c| c.node.role == Role::Toolbar)
            .expect("a toolbar group is present after a draw");
        assert!(!toolbar.children.is_empty(), "the toolbar has button nodes");
        // The buttons read with human names, not the terse visible glyphs.
        assert!(
            toolbar
                .children
                .iter()
                .filter_map(|c| c.node.name.as_deref())
                .any(|n| n == "Bold"),
            "the Bold button is named for a screen reader"
        );
        assert!(
            toolbar
                .children
                .iter()
                .any(|c| matches!(c.node.role, Role::Button | Role::ToggleButton)),
            "the toolbar contains actionable button nodes",
        );

        let exported = Widget::accessibility(&*wp).expect("exported semantics");
        let exported_toolbar = exported
            .children
            .iter()
            .find(|node| node.role == Role::Toolbar)
            .expect("toolbar reaches the host tree");
        assert!(exported_toolbar.children.iter().all(|node| {
            node.bounds.is_some() && node.action_target.is_some() && node.state.focusable
        }));
    }

    // Phase: accessibility_activation_uses_the_real_toolbar_path.
    {
        let (state, wp) = populated();
        let mut framed = mount(state, wp, 1100, 760);
        let (wp, state) = Framed::split(&mut framed);
        assert!(!wp.view.styles_panel_visible);
        let target = "word-processor.toolbar:toggle-styles".to_string();
        assert!(Widget::perform_action_at(
            wp,
            &ui::a11y::Action::Activate,
            &[ui::a11y::hash_key(&target)],
            &mut state.doc,
            &state.root,
        ));
        assert!(
            wp.view.styles_panel_visible,
            "AT activation reaches the same view toggle as a pointer click"
        );
    }
}

// ---- collaboration: remote presence rendering --------------------------

use ui::permissions::UserId;
use ui::presence::SelectionRange;

/// The id of the first laid-out block (after a draw) — the block a remote
/// peer's caret/selection will be keyed to.
fn first_block_id(wp: &WordProcessorWidget) -> String {
    wp.laid
        .first()
        .expect("at least one block was laid out")
        .id
        .clone()
}

/// A populated, focused widget that has been drawn once (so `self.laid` +
/// `self.layout_cache` are live and `remote_presence_overlays` is
/// meaningful).
fn drawn_focused() -> Framed<WordProcessorWidget> {
    let (state, mut wp) = populated();
    wp.set_focus(true);
    mount(state, wp, 1100, 760)
}

/// Remote presence is a no-op without peers, and a peer's caret is labelled with their name or, unnamed,
/// their id.
#[test]
fn remote_carets_are_labelled_by_peer_name_or_id() {
    // Phase: remote_presence_is_noop_without_peers.
    // An empty `PresenceCtx` (single-user / no broker) yields no remote
    // caret/selection overlays — proving the collab render path is a strict
    // no-op when there are no peers, so single-user editing is unchanged.
    {
        let wp = drawn_focused();
        let presence = PresenceCtx::empty();
        let (carets, selections) = wp.remote_presence_overlays(&presence);
        assert!(carets.is_empty(), "no remote carets without peers");
        assert!(selections.is_empty(), "no remote selections without peers");
    }

    // Phase: a_named_peers_caret_is_labelled_with_their_name.
    // The pill over a peer's caret shows who is editing, not which machine —
    // the room's assigned name for the peer's transport id, never the raw id
    // itself (which reads as noise like `peer@5537458103208628978`).
    {
        let wp = drawn_focused();
        let block_id = first_block_id(&wp);
        let peer = "peer@5537458103208628978";

        let mut presence = PresenceCtx::empty();
        presence.set_remote_caret(peer.into(), block_presence_key(false, &block_id), 3);
        presence.set_display_name(ui::permissions::UserId(peer.into()), "bob");

        let (carets, _) = wp.remote_presence_overlays(&presence);
        assert_eq!(carets.len(), 1);
        assert_eq!(carets[0].label, "bob");
        assert_eq!(
            carets[0].user.0, peer,
            "the id stays the identity — it is what the caret is coloured by",
        );
    }

    // Phase: an_unnamed_peers_caret_is_labelled_with_its_id.
    // And a peer nobody named keeps its id on the pill: not blank, and
    // not a person who does not exist. An unlabelled caret is honest;
    // an invented name is a claim about who is in the room.
    {
        let wp = drawn_focused();
        let block_id = first_block_id(&wp);
        let peer = "peer@5537458103208628978";

        let mut presence = PresenceCtx::empty();
        presence.set_remote_caret(peer.into(), block_presence_key(false, &block_id), 3);

        let (carets, _) = wp.remote_presence_overlays(&presence);
        assert_eq!(carets.len(), 1);
        assert_eq!(carets[0].label, peer);
        assert!(!carets[0].label.trim().is_empty());
    }
}

/// A remote caret resolves to an on-screen rect, a remote selection to highlight rects, and an empty
/// selection to none.
#[test]
fn remote_carets_and_selections_resolve_to_screen_rects() {
    // Phase: remote_caret_is_resolved_to_on_screen_rect.
    // A `PresenceCtx` carrying a remote collaborator's caret (under the same
    // per-block key the widget broadcasts the local caret under) yields exactly
    // one remote-caret overlay, positioned inside the laid block — proving
    // `draw` consumes presence and maps a remote byte offset onto on-screen
    // caret geometry via the production `caret_geometry` path.
    {
        let wp = drawn_focused();
        let block_id = first_block_id(&wp);

        let mut presence = PresenceCtx::empty();
        // Place "alice"'s caret 3 bytes into the first block.
        presence.set_remote_caret("alice".into(), block_presence_key(false, &block_id), 3);

        let (carets, selections) = wp.remote_presence_overlays(&presence);
        assert_eq!(carets.len(), 1, "one remote caret resolved");
        assert!(
            selections.is_empty(),
            "a bare caret yields no selection rects"
        );
        let c = &carets[0];
        assert_eq!(c.user, UserId("alice".into()));
        // The caret rect must be a real, non-degenerate vertical bar (the
        // `CARET_WIDTH`-wide, line-tall geometry produced by `caret_geometry`),
        // and it must land within the block's laid rectangle.
        let block = wp.laid.iter().find(|l| l.id == block_id).unwrap();
        assert!(
            (c.rect.width() - CARET_WIDTH).abs() < 1e-6,
            "caret is CARET_WIDTH wide: {:?}",
            c.rect
        );
        assert!(
            c.rect.height() > 1.0,
            "caret has a real height: {:?}",
            c.rect
        );
        assert!(
            c.rect.x0 >= block.text_x - 0.5,
            "caret sits at/after the block's text edge"
        );
        assert!(
            c.rect.y0 >= block.y_top - 0.5,
            "caret sits within the block vertically"
        );
    }

    // Phase: remote_selection_is_resolved_to_highlight_rects.
    // A remote selection range yields one-or-more non-degenerate highlight
    // rectangles (a single-line selection is one rect), mapped through the same
    // `selection_rects` geometry the local selection uses.
    {
        let wp = drawn_focused();
        let block_id = first_block_id(&wp);

        let mut presence = PresenceCtx::empty();
        // "bob" selects bytes [0, 4) of the first block.
        presence.set_remote_selection("bob".into(), block_presence_key(false, &block_id), 0, 4);

        let (carets, selections) = wp.remote_presence_overlays(&presence);
        assert!(carets.is_empty(), "a selection yields no bare caret");
        assert_eq!(selections.len(), 1, "one remote selection resolved");
        let s = &selections[0];
        assert_eq!(s.user, UserId("bob".into()));
        assert!(
            !s.rects.is_empty(),
            "the selection produced at least one highlight rect"
        );
        for r in &s.rects {
            assert!(
                r.width() > 0.0 && r.height() > 0.0,
                "highlight rect is non-degenerate: {r:?}"
            );
        }
    }

    // Phase: empty_remote_selection_produces_no_rects.
    // A degenerate (empty) remote selection range produces no highlight rects —
    // the render guards against a zero-width selection painting a stray band.
    {
        let wp = drawn_focused();
        let block_id = first_block_id(&wp);

        let mut presence = PresenceCtx::empty();
        presence.selections.insert(
            block_presence_key(false, &block_id),
            vec![(UserId("bob".into()), SelectionRange { start: 2, end: 2 })],
        );

        let (_carets, selections) = wp.remote_presence_overlays(&presence);
        assert!(
            selections.is_empty(),
            "an empty selection range paints nothing"
        );
    }
}

/// Distinct peers on distinct blocks resolve independently, and draw-with-presence renders a remote
/// caret through the production path.
#[test]
fn remote_peers_resolve_independently_and_draw_through_production() {
    // Phase: distinct_peers_on_distinct_blocks_resolve_independently.
    // Two peers on DIFFERENT blocks each get their own caret overlay, attributed
    // to the right user — the per-block key keeps them from colliding.
    {
        let wp = drawn_focused();
        assert!(
            wp.laid.len() >= 2,
            "the populated doc lays out at least two blocks"
        );
        let b0 = wp.laid[0].id.clone();
        let b1 = wp.laid[1].id.clone();

        let mut presence = PresenceCtx::empty();
        presence.set_remote_caret("alice".into(), block_presence_key(false, &b0), 1);
        presence.set_remote_caret("bob".into(), block_presence_key(false, &b1), 1);

        let (carets, _selections) = wp.remote_presence_overlays(&presence);
        assert_eq!(carets.len(), 2, "both peers' carets resolved");
        let users: Vec<&str> = carets.iter().map(|c| c.user.0.as_str()).collect();
        assert!(
            users.contains(&"alice") && users.contains(&"bob"),
            "got {users:?}"
        );
        // The two carets land in different blocks (different vertical bands).
        let ya = carets.iter().find(|c| c.user.0 == "alice").unwrap().rect.y0;
        let yb = carets.iter().find(|c| c.user.0 == "bob").unwrap().rect.y0;
        assert!(
            (ya - yb).abs() > 1.0,
            "carets in different blocks have distinct y positions"
        );
    }

    // Phase: draw_with_presence_renders_remote_caret_through_production_path.
    // The full `draw_with_presence` path consumes a populated `PresenceCtx`
    // without panicking AND the resolver agrees — i.e. the production draw and
    // the asserted helper are the same path, not a parallel re-implementation.
    {
        let mut framed = drawn_focused();
        let block_id = first_block_id(&framed);

        let mut presence = PresenceCtx::empty();
        presence.set_remote_caret("charlie".into(), block_presence_key(false, &block_id), 2);

        // Run the REAL five-pass draw with the populated presence (the same call
        // the App makes each frame). It must paint without panicking.
        Framed::harness_mut(&mut framed).set_presence_context(presence.clone());
        let (wp, _) = pipeline_draw(&mut framed, 1100, 760);

        // After the production draw, the resolver (which the draw itself called)
        // reports the same single remote caret — proving the draw consumed it.
        let (carets, _sel) = wp.remote_presence_overlays(&presence);
        assert_eq!(
            carets.len(),
            1,
            "the production draw consumed the remote caret"
        );
        assert_eq!(carets[0].user, UserId("charlie".into()));
    }
}

// ---- collaboration: outbound presence (presence_payload) ---------------

/// The presence payload reports the focused caret in bytes, the selection when present, and none when
/// unfocused.
#[test]
fn the_presence_payload_reports_focus_caret_and_selection() {
    // Phase: presence_payload_reports_focused_caret_in_bytes.
    // While focused, the widget reports its local caret as outbound presence,
    // keyed to the caret's block and carrying a BYTE offset (so a peer can feed
    // it straight into the layout geometry). The App polls this each frame and
    // broadcasts the diff to peers.
    {
        let (state, mut wp) = populated();
        wp.set_focus(true);
        // Move the caret to char offset 3 of the caret block.
        let block = wp.model().caret().block.clone();
        wp.model_mut().set_caret(Caret {
            block: block.clone(),
            offset: 3,
        });
        // A draw populates the block cache (needed for char→byte conversion).
        let mut framed = mount(state, wp, 1100, 760);
        let (wp, _) = Framed::split(&mut framed);

        match ui::presence::capability::presence_payload(wp) {
            Some((key, ui::PresencePayload::Caret { pos })) => {
                assert_eq!(
                    key,
                    block_presence_key(false, &block),
                    "keyed to the caret's block"
                );
                // The caret block text is ASCII here, so byte offset == char offset.
                assert_eq!(pos, 3, "caret reported at byte 3");
            }
            other => panic!("expected a Caret payload, got {other:?}"),
        }
    }

    // Phase: presence_payload_reports_selection_when_present.
    // A selection takes priority over the bare caret in the outbound payload,
    // reported as a byte range over the caret block.
    {
        let (state, mut wp) = populated();
        wp.set_focus(true);
        let mut framed = mount(state, wp, 1100, 760);
        let (wp, _) = Framed::split(&mut framed);
        // Select chars [1, 4) of the caret block.
        select_range(wp, 1, 4);
        // Re-draw so the selection is reflected in the cached block views.
        let (wp, _) = pipeline_draw(&mut framed, 1100, 760);

        let block = wp.model().caret().block.clone();
        match ui::presence::capability::presence_payload(wp) {
            Some((key, ui::PresencePayload::Selection { start, end })) => {
                assert_eq!(key, block_presence_key(false, &block));
                assert_eq!((start, end), (1, 4), "selection reported as a byte range");
            }
            other => panic!("expected a Selection payload, got {other:?}"),
        }
    }

    // Phase: presence_payload_is_none_when_unfocused.
    // An unfocused widget broadcasts no presence (so a background editor doesn't
    // flood peers with a stale caret).
    {
        let (state, wp) = populated();
        let mut framed = mount(state, wp, 1100, 760);
        let (wp, _) = Framed::split(&mut framed);
        wp.set_focus(false);
        assert!(
            ui::presence::capability::presence_payload(wp).is_none(),
            "no presence while unfocused"
        );
    }
}

// ---- image insert via the host file-chooser portal --------------------

/// Write [`TEST_PNG`] to a unique temp file and return its path.
fn temp_png(tag: &str) -> std::path::PathBuf {
    let p = std::env::temp_dir().join(format!("wp-img-{tag}-{}.png", std::process::id()));
    std::fs::write(&p, TEST_PNG).expect("write temp png");
    p
}

/// A recording host whose next single-file chooser answers `outcome`, current
/// on this thread while the returned guard lives.
fn host_answering(outcome: portal::PortalOutcome) -> (ui::host::HostScope, ui::host::HostRecorder) {
    let (services, recorder) = ui::host::HostServices::recording();
    let scope = ui::host::enter(&services);
    recorder.respond_to_pick(outcome);
    (scope, recorder)
}

/// Titles of every chooser the recording host was asked to open, in order.
fn pick_titles(recorder: &ui::host::HostRecorder) -> Vec<String> {
    recorder
        .pick_requests()
        .iter()
        .map(|r| r.title().to_string())
        .collect()
}

fn image_block_count(wp: &WordProcessorWidget, state: &ui::State) -> usize {
    let c = wp.model().containers.clone().unwrap();
    tilo_doc::model::materialize(&state.doc, &c)
        .blocks
        .iter()
        .filter(|b| b.kind == "image")
        .count()
}

/// The image button opens the host portal; a resolved pick inserts an image and a cancelled one inserts
/// nothing.
#[test]
fn the_image_portal_inserts_only_resolved_picks() {
    // Phase: image_button_opens_the_host_portal.
    // Pressing the toolbar Image button asks the **host portal** for a file (it
    // does NOT fabricate a path); nothing is inserted until the pick resolves.
    {
        let (mut state, mut wp) = bound();
        assert!(
            wp.portal_rx.is_none(),
            "no portal request before the button is pressed"
        );
        wp.apply_tool(&mut state.doc, ToolAction::InsertImage);
        assert!(
            wp.portal_rx.is_some(),
            "the Image button opens the host portal chooser"
        );
        assert_eq!(
            image_block_count(&wp, &state),
            0,
            "no image inserted until the portal returns a file"
        );
    }

    // Phase: a_resolved_portal_pick_inserts_an_image.
    // A resolved portal pick (a chosen file) is folded in on the next frame's
    // `sync` drain and inserts a real image block referencing the file's bytes.
    {
        let (mut state, mut wp) = bound();
        wp.model_mut().insert_str(&mut state.doc, "before");
        let path = temp_png("pick");
        // The Image button asks the host chooser, which answers with a file.
        let (_host, recorder) = host_answering(portal::PortalOutcome::Opened(path.clone()));
        wp.apply_tool(&mut state.doc, ToolAction::InsertImage);
        assert_eq!(pick_titles(&recorder), ["Insert image"]);
        wp.sync(&mut state.doc, &state.root);
        let _ = std::fs::remove_file(&path);
        assert!(
            wp.portal_rx.is_none(),
            "a resolved portal request clears the channel"
        );
        assert_eq!(
            image_block_count(&wp, &state),
            1,
            "the picked file inserts one image block"
        );
    }

    // Phase: a_cancelled_portal_pick_inserts_nothing.
    // A cancelled portal pick inserts nothing and clears the in-flight channel.
    {
        let (mut state, mut wp) = bound();
        let (_host, _recorder) = host_answering(portal::PortalOutcome::Cancelled);
        wp.apply_tool(&mut state.doc, ToolAction::InsertImage);
        wp.sync(&mut state.doc, &state.root);
        assert!(
            wp.portal_rx.is_none(),
            "a cancel clears the in-flight channel"
        );
        assert_eq!(
            image_block_count(&wp, &state),
            0,
            "a cancel inserts nothing"
        );
    }
}

/// Save As writes a real round-tripping docx; failed Save As and open keep the destination and draft; new
/// and open do not replace a modified draft without a decision; cancelled or failed saves keep the pending
/// open; cancelling a resource open closes its prompt; save-then-open writes first; a failed save is
/// visible; undo after open never takes back the file.
#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one scenario test: each phase is a former standalone test kept verbatim as a scoped block"
)]
fn file_ux_never_replaces_a_draft_without_a_decision() {
    // Phase: save_as_writes_a_real_docx_that_round_trips.
    // A docx opened → edited → **Save As** to a new `.docx` writes a *real* Word
    // file (an OOXML ZIP, `PK` signature) that round-trips back through the
    // opener with the edit intact — proving the portal Save-As path drives the
    // qdoc docx codec at the user-chosen path.
    {
        let dir = std::env::temp_dir();
        let pid = std::process::id();
        let src = dir.join(format!("wp-src-{pid}.docx"));
        let dest = dir.join(format!("wp-dest-{pid}.docx"));

        // Author a real source .docx via the codec (a seeded doc → save_document).
        {
            let mut state = ui::State::new();
            with_doc(&mut state, |dm| {
                let b = dm.insert_block(None, "paragraph");
                dm.splice_text(&b, 0, 0, "Hello docx");
            });
            crate::save_document(&state, &src, WpFormat::Docx, &tilo_content::NoContent)
                .expect("write source docx");
        }
        assert!(
            std::fs::read(&src).unwrap().starts_with(b"PK"),
            "the source is a real docx (OOXML zip)"
        );

        // Open it into a running editor and confirm the text came through.
        let (mut state, mut wp, fmt) =
            crate::open_document(&src, tilo_content::ContentStore::memory())
                .expect("open source docx");
        assert_eq!(fmt, WpFormat::Docx);
        wp.sync(&mut state.doc, &state.root);
        assert!(
            body_text(&wp, &state)
                .iter()
                .any(|t| t.contains("Hello docx")),
            "opened body carries the source text"
        );

        // Edit, then Save As to a NEW .docx through the real portal drain path.
        wp.model_mut().focus_body(&mut state.doc); // seat the caret on a real block
        wp.model_mut().insert_str(&mut state.doc, "EDITED ");
        let (_host, _recorder) = host_answering(portal::PortalOutcome::Opened(dest.clone()));
        wp.request_save_as();
        wp.sync(&mut state.doc, &state.root);

        // Save As adopted the chosen path/format and cleared the in-flight request.
        assert!(wp.portal_rx.is_none(), "the Save-As request drained");
        assert_eq!(
            wp.path.as_deref(),
            Some(dest.as_path()),
            "path is now the Save-As target"
        );
        assert_eq!(
            wp.format,
            Some(WpFormat::Docx),
            "format follows the chosen extension"
        );
        assert!(
            matches!(wp.toast.as_ref(), Some((_, ToastKind::Success))),
            "a completed save is visibly confirmed"
        );

        // The written file is a real docx (ZIP signature) — not markdown ...
        let written = std::fs::read(&dest).expect("Save As wrote the destination");
        assert!(
            written.starts_with(b"PK"),
            "Save As wrote a real docx zip, not a markdown fallback"
        );
        // ... and it round-trips back through the opener with both texts intact.
        let (mut state2, mut wp2, fmt2) =
            crate::open_document(&dest, tilo_content::ContentStore::memory())
                .expect("reopen the saved docx");
        assert_eq!(fmt2, WpFormat::Docx);
        wp2.sync(&mut state2.doc, &state2.root); // bind containers before reading
        let reopened = body_text(&wp2, &state2).join("\n");
        assert!(
            reopened.contains("Hello docx"),
            "original text survives: {reopened:?}"
        );
        assert!(
            reopened.contains("EDITED"),
            "the edit survives: {reopened:?}"
        );

        let _ = std::fs::remove_file(&src);
        let _ = std::fs::remove_file(&dest);
    }

    // Phase: file_ux_failed_save_as_keeps_the_original_destination.
    {
        let (mut state, mut wp) = bound();
        wp.model_mut().insert_str(&mut state.doc, "My draft");
        let dir = std::env::temp_dir().join(format!("wp-save-as-failure-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let original = dir.join("original.md");
        std::fs::write(&original, "Original on disk").unwrap();
        wp.set_file(original.clone(), WpFormat::Markdown);
        wp.save_status = DocumentSaveStatus::Modified;
        let (_host, _recorder) = host_answering(portal::PortalOutcome::Opened(
            dir.join("missing-parent").join("new.docx"),
        ));
        wp.request_save_as();
        wp.sync(&mut state.doc, &state.root);
        assert_eq!(wp.path, Some(original.clone()));
        assert_eq!(wp.format, Some(WpFormat::Markdown));
        assert_eq!(wp.save_status, DocumentSaveStatus::Modified);
        assert_eq!(
            std::fs::read_to_string(&original).unwrap(),
            "Original on disk"
        );
        assert!(matches!(wp.toast, Some((_, ToastKind::Error))));
        let _ = std::fs::remove_dir_all(dir);
    }

    // Phase: file_ux_failed_open_reports_an_error_without_changing_the_draft.
    {
        let (mut state, mut wp) = bound();
        wp.model_mut().insert_str(&mut state.doc, "My draft");
        let missing = std::env::temp_dir().join(format!("wp-missing-{}.docx", std::process::id()));
        let (_host, _recorder) = host_answering(portal::PortalOutcome::Opened(missing));
        wp.request_open_document();
        wp.sync(&mut state.doc, &state.root);
        assert_eq!(
            first_block(&wp, &state).map(|b| b.text),
            Some("My draft".into())
        );
        assert!(
            matches!(wp.toast, Some((_, ToastKind::Error))),
            "failed Open needs visible feedback"
        );
    }

    // Phase: file_ux_new_does_not_replace_a_modified_draft_without_a_decision.
    {
        let (mut state, mut wp) = bound();
        wp.inner.insert_str(&mut state.doc, "Unsaved work");
        wp.save_status = DocumentSaveStatus::Modified;
        let mut framed = mount(state, wp, 1000, 700);
        let (wp, state) = Framed::split(&mut framed);
        wp.open_template_picker();
        press_named(wp, state, NamedKey::ArrowDown);
        press_named(wp, state, NamedKey::Enter);
        assert_eq!(
            first_block(wp, state).map(|b| b.text),
            Some("Unsaved work".into())
        );
        press_named(wp, state, NamedKey::Escape);
        assert_eq!(
            first_block(wp, state).map(|b| b.text),
            Some("Unsaved work".into())
        );
        assert_eq!(wp.save_status, DocumentSaveStatus::Modified);
    }

    // Phase: file_ux_open_does_not_replace_a_modified_draft_without_a_decision.
    {
        let (mut state, mut wp) = bound();
        wp.inner.insert_str(&mut state.doc, "Unsaved work");
        wp.save_status = DocumentSaveStatus::Modified;
        let picked = std::env::temp_dir().join(format!("wp-guard-open-{}.md", std::process::id()));
        std::fs::write(&picked, "Other file").unwrap();
        let (_host, _recorder) = host_answering(portal::PortalOutcome::Opened(picked.clone()));
        wp.request_open_document();
        wp.sync(&mut state.doc, &state.root);
        assert_eq!(
            first_block(&wp, &state).map(|b| b.text),
            Some("Unsaved work".into())
        );
        assert!(wp.path.is_none());
        let _ = std::fs::remove_file(picked);
    }

    // Phase: file_ux_cancelled_or_failed_save_keeps_the_pending_open_and_draft.
    {
        for failure in 0..3 {
            let (mut state, mut wp) = bound();
            wp.inner.insert_str(&mut state.doc, "Unsaved draft");
            wp.save_status = DocumentSaveStatus::Modified;
            let invocation = tilo_actions::InvocationId::new();
            wp.resource_file_invocation = Some(invocation);
            wp.stage_replacement(PendingReplacement::Open(PathBuf::from(
                "never-open-this.md",
            )));
            let (services, recorder) = ui::host::HostServices::recording();
            let _host = ui::host::enter(&services);
            match failure {
                0 => recorder.respond_to_pick(portal::PortalOutcome::Cancelled),
                1 => recorder.respond_to_pick(portal::PortalOutcome::Failed("test failure".into())),
                _ => {
                    // A chooser whose worker hung up without answering.
                    let (sender, receiver) = std::sync::mpsc::channel();
                    drop(sender);
                    wp.portal_purpose = Some(PortalPurpose::SaveAs);
                    wp.portal_rx = Some(receiver);
                }
            }
            wp.resolve_unsaved_choice(&mut state.doc, UnsavedChoice::Save);
            wp.sync(&mut state.doc, &state.root);
            assert_eq!(
                first_block(&wp, &state).map(|b| b.text),
                Some("Unsaved draft".into())
            );
            assert!((wp.save_status == DocumentSaveStatus::Modified) && wp.path.is_none());
            assert!(wp.unsaved_changes.is_open() && wp.pending_replacement.is_some());
            assert!(
                wp.poll_resource_operation(invocation, false, &state.doc, &state.root)
                    .is_none()
            );
            wp.resolve_unsaved_choice(&mut state.doc, UnsavedChoice::Cancel);
            assert!(matches!(
                wp.poll_resource_operation(invocation, false, &state.doc, &state.root),
                Some(tilo_actions::InvocationUpdate::Cancelled { .. })
            ));
        }
    }

    // Phase: file_ux_cancelling_a_resource_open_closes_its_unsaved_prompt.
    {
        let (mut state, mut wp) = bound();
        wp.inner.insert_str(&mut state.doc, "Keep this draft");
        wp.save_status = DocumentSaveStatus::Modified;
        let invocation = tilo_actions::InvocationId::new();
        wp.pending_resource_open = Some((invocation, PathBuf::from("never-open.md")));
        wp.sync(&mut state.doc, &state.root);
        assert!(wp.unsaved_changes.is_open());
        assert!(matches!(
            wp.poll_resource_operation(invocation, true, &state.doc, &state.root),
            Some(tilo_actions::InvocationUpdate::Cancelled { .. })
        ));
        assert!(!wp.unsaved_changes.is_open() && wp.pending_replacement.is_none());
        assert_eq!(
            first_block(&wp, &state).map(|b| b.text),
            Some("Keep this draft".into())
        );
        assert_eq!(wp.save_status, DocumentSaveStatus::Modified);
    }

    // Phase: file_ux_save_then_open_writes_the_draft_before_replacing_it.
    {
        for corrupt in [false, true] {
            let (mut state, mut wp) = bound();
            wp.inner.insert_str(&mut state.doc, "Unsaved draft");
            wp.save_status = DocumentSaveStatus::Modified;
            let dir = std::env::temp_dir().join(format!(
                "wp-save-before-open-{}-{corrupt}",
                std::process::id()
            ));
            std::fs::create_dir_all(&dir).unwrap();
            let target = dir.join(if corrupt { "target.docx" } else { "target.md" });
            std::fs::write(&target, "Other document").unwrap();
            let saved = dir.join("saved-draft.md");
            let invocation = tilo_actions::InvocationId::new();
            wp.resource_file_invocation = Some(invocation);
            wp.stage_replacement(PendingReplacement::Open(target.clone()));
            let (_host, recorder) = host_answering(portal::PortalOutcome::Opened(saved.clone()));
            wp.resolve_unsaved_choice(&mut state.doc, UnsavedChoice::Save);
            assert_eq!(pick_titles(&recorder), ["Save as"]);
            wp.sync(&mut state.doc, &state.root);
            assert!(
                std::fs::read_to_string(&saved)
                    .unwrap()
                    .contains("Unsaved draft")
            );
            assert_eq!(wp.path, Some(if corrupt { saved } else { target }));
            assert_eq!(
                first_block(&wp, &state).map(|b| b.text),
                Some(
                    if corrupt {
                        "Unsaved draft"
                    } else {
                        "Other document"
                    }
                    .into()
                )
            );
            let result = wp.poll_resource_operation(invocation, false, &state.doc, &state.root);
            if corrupt {
                assert!(matches!(
                    result,
                    Some(tilo_actions::InvocationUpdate::Failed { .. })
                ));
                assert!(matches!(wp.toast, Some((_, ToastKind::Error))));
            } else {
                assert!(matches!(
                    result,
                    Some(tilo_actions::InvocationUpdate::Succeeded { .. })
                ));
            }
            assert!(
                wp.poll_resource_operation(invocation, false, &state.doc, &state.root)
                    .is_none()
            );
            assert!(!wp.unsaved_changes.is_open() && wp.pending_replacement.is_none());
            let _ = std::fs::remove_dir_all(dir);
        }
    }

    // Phase: failed_save_is_visible_and_keeps_the_document_modified.
    {
        let (mut state, mut wp) = bound();
        wp.model_mut().insert_str(&mut state.doc, "unsaved work");
        wp.save_status = DocumentSaveStatus::Modified;
        // Writing bytes to a directory reliably fails without relying on file
        // permissions (the test suite may run as a privileged user).
        wp.set_file(std::env::temp_dir(), WpFormat::Markdown);
        wp.write_to_current_path(&state.doc);

        assert!(
            wp.save_status == DocumentSaveStatus::Modified,
            "a failed save never clears the dirty state"
        );
        assert!(
            matches!(wp.toast.as_ref(), Some((_, ToastKind::Error))),
            "the user sees an error toast instead of an stderr-only failure"
        );
    }

    // Phase: undo_after_opening_a_file_takes_back_the_edit_and_never_the_file.
    {
        let dir = temp_dir("open-then-undo");
        std::fs::create_dir_all(&dir).unwrap();
        let source = dir.join("source.rtf");
        std::fs::write(
            &source,
            br"{\rtf1\ansi Plain {\b bold} and {\i italic} words\par A {\ul second} paragraph\par}",
        )
        .unwrap();
        let (mut state, mut wp, _) =
            crate::open_document(&source, tilo_content::ContentStore::memory()).unwrap();
        // Opening leaves nothing pending: the file is the document's committed starting point.
        assert_eq!(state.doc.pending_ops(), 0);
        let opened = body_content(&state.doc);
        assert!(
            opened
                .iter()
                .any(|(_, marks)| marks.iter().any(|m| m.name == "strong")),
            "the file's formatting came through: {opened:?}"
        );

        wp.sync(&mut state.doc, &state.root);
        wp.model_mut().insert_str(&mut state.doc, "edited ");
        state.commit_action(None, None);
        let edited = body_content(&state.doc);
        assert_ne!(edited, opened);

        assert!(state.undo(None, None).is_some(), "the edit is undoable");
        assert_eq!(
            body_content(&state.doc),
            opened,
            "undo restores the file as opened"
        );
        assert!(
            state.undo(None, None).is_none(),
            "there is nothing before the file to undo"
        );
        assert_eq!(body_content(&state.doc), opened);
        assert!(state.redo(None, None).is_some());
        assert_eq!(
            body_content(&state.doc),
            edited,
            "redo brings the edit back"
        );
        std::fs::remove_dir_all(dir).unwrap();
    }
}

/// Each body block's text and marks, what a reader sees of the document.
fn body_content(doc: &automerge::AutoCommit) -> Vec<(String, Vec<tilo_doc::MarkSpan>)> {
    let c = tilo_doc::model::open(doc, &automerge::ROOT).expect("qdoc schema");
    tilo_doc::model::materialize(doc, &c)
        .blocks
        .into_iter()
        .map(|b| (b.text, b.marks))
        .collect()
}

/// Failed and export-only Save As preserve source identity and recovery; open-pick offers destination
/// recovery and replaces in place; failed open preserves bytes, caret and identity; import does not reuse
/// a deleted block id; a read-only destination preserves both files.
#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one scenario test: each phase is a former standalone test kept verbatim as a scoped block"
)]
fn save_and_open_preserve_identity_recovery_and_bytes() {
    // Phase: failed_save_as_preserves_source_identity_and_recovery_for_picker_and_action.
    {
        let dir = temp_dir("failed-save-as");
        std::fs::create_dir_all(&dir).unwrap();
        let source = dir.join("source.md");
        std::fs::write(&source, b"original source").unwrap();
        let (mut state, mut wp, _) =
            crate::open_document(&source, tilo_content::ContentStore::memory()).unwrap();
        wp.set_recovery_dir(dir.clone());
        wp.sync(&mut state.doc, &state.root);
        wp.model_mut().insert_str(&mut state.doc, "edited ");
        wp.save_status = DocumentSaveStatus::Modified;
        let recovery = wp.recovery_path();
        std::fs::write(&recovery, b"unsaved recovery").unwrap();

        for via_action in [false, true] {
            // A directory cannot be replaced by a document, on every platform.
            if via_action {
                let payload = serde_json::to_vec(&crate::actions::WordProcessorOperation::SaveAs {
                    path: Some(dir.to_string_lossy().into_owned()),
                })
                .unwrap();
                assert!(
                    wp.start_operation(
                        &ui::widget::ResourceOperation::local(
                            tilo_actions::InvocationId::new(),
                            tilo_actions::OperationKind::Effect,
                            crate::actions::WORD_PROCESSOR_OPERATION_TYPE,
                            &payload
                        ),
                        &mut state.doc,
                        &state.root
                    )
                    .is_err()
                );
            } else {
                let (_host, _recorder) = host_answering(portal::PortalOutcome::Opened(dir.clone()));
                wp.request_save_as();
                wp.sync(&mut state.doc, &state.root);
            }
            assert_eq!(wp.path.as_deref(), Some(source.as_path()));
            assert_eq!(wp.format, Some(WpFormat::Markdown));
            assert_eq!(wp.save_status, DocumentSaveStatus::Modified);
            assert_eq!(std::fs::read(&source).unwrap(), b"original source");
            assert_eq!(std::fs::read(&recovery).unwrap(), b"unsaved recovery");
        }
        // A subsequent ordinary Save still updates the original document.
        wp.try_write_to_current_path(&state.doc).unwrap();
        assert!(std::fs::read_to_string(&source).unwrap().contains("edited"));
        assert!(!recovery.exists());
        std::fs::remove_dir_all(dir).unwrap();
    }

    // Phase: export_only_save_as_preserves_editable_path_and_recovery.
    {
        let dir = temp_dir("export-only-save-as");
        std::fs::create_dir_all(&dir).unwrap();
        let source = dir.join("source.md");
        std::fs::write(&source, b"original source").unwrap();
        let (mut state, mut wp, _) =
            crate::open_document(&source, tilo_content::ContentStore::memory()).unwrap();
        wp.set_recovery_dir(dir.clone());
        wp.sync(&mut state.doc, &state.root);
        wp.model_mut().insert_str(&mut state.doc, "edited ");
        wp.save_status = DocumentSaveStatus::Modified;
        let recovery = wp.recovery_path();
        std::fs::write(&recovery, b"unsaved recovery").unwrap();
        for extension in ["pdf", "epub"] {
            let destination = dir.join(format!("export.{extension}"));
            let (_host, _recorder) =
                host_answering(portal::PortalOutcome::Opened(destination.clone()));
            wp.request_save_as();
            wp.sync(&mut state.doc, &state.root);
            let bytes = std::fs::read(destination).unwrap();
            assert!(bytes.starts_with(if extension == "pdf" { b"%PDF" } else { b"PK" }));
            assert_eq!(wp.path.as_deref(), Some(source.as_path()));
            assert_eq!(wp.format, Some(WpFormat::Markdown));
            assert_eq!(wp.save_status, DocumentSaveStatus::Modified);
            assert_eq!(std::fs::read(&source).unwrap(), b"original source");
            assert_eq!(std::fs::read(&recovery).unwrap(), b"unsaved recovery");
        }
        std::fs::remove_dir_all(dir).unwrap();
    }

    // Phase: open_pick_offers_destination_recovery_and_preserves_previous_recovery.
    {
        let dir = temp_dir("open-recovery");
        std::fs::create_dir_all(&dir).unwrap();
        let (mut state, mut wp) = bound();
        wp.set_recovery_dir(dir.clone());
        wp.set_file(dir.join("previous.md"), WpFormat::Markdown);
        let previous_recovery = wp.recovery_path();
        std::fs::write(&previous_recovery, b"previous unsaved work").unwrap();
        assert!(wp.check_for_recovery());
        let destination = dir.join("next.md");
        std::fs::write(&destination, b"saved destination").unwrap();
        let destination_recovery = wp.recovery.path_for(Some(&destination));
        std::fs::write(&destination_recovery, b"destination unsaved work").unwrap();
        let (_host, _recorder) = host_answering(portal::PortalOutcome::Opened(destination.clone()));
        wp.request_open_document();
        wp.sync(&mut state.doc, &state.root);
        assert_eq!(wp.path.as_deref(), Some(destination.as_path()));
        assert!(wp.recovery.is_active());
        assert_eq!(
            std::fs::read(&destination_recovery).unwrap(),
            b"destination unsaved work"
        );
        assert_eq!(
            std::fs::read(&previous_recovery).unwrap(),
            b"previous unsaved work"
        );
        assert!(
            body_text(&wp, &state)
                .join("\n")
                .contains("saved destination")
        );

        let clean = dir.join("clean.md");
        std::fs::write(&clean, b"clean document").unwrap();
        wp.try_open_document_in_place(&mut state.doc, &clean)
            .unwrap();
        assert!(
            !wp.recovery.is_active(),
            "the previous document's offer must not follow the next document"
        );
        assert!(destination_recovery.exists());
        std::fs::remove_dir_all(dir).unwrap();
    }

    // Phase: failed_open_preserves_document_bytes_caret_and_file_identity.
    {
        let dir = temp_dir("failed-open");
        std::fs::create_dir_all(&dir).unwrap();
        let source = dir.join("source.md");
        std::fs::write(&source, b"original source").unwrap();
        let (mut state, mut wp, _) =
            crate::open_document(&source, tilo_content::ContentStore::memory()).unwrap();
        wp.set_recovery_dir(dir.clone());
        wp.sync(&mut state.doc, &state.root);
        wp.model_mut().insert_str(&mut state.doc, "unsaved ");
        wp.save_status = DocumentSaveStatus::Modified;
        let pending = state.doc.pending_ops();
        assert!(
            pending > 0,
            "the failed import must preserve uncommitted edits"
        );
        let before = state.doc.clone().save();
        let caret = wp.model_mut().caret().clone();
        for extension in ["docx", "odt", "pdf", "epub"] {
            let corrupt = dir.join(format!("corrupt.{extension}"));
            std::fs::write(&corrupt, b"not a valid document archive").unwrap();
            assert!(
                wp.try_open_document_in_place(&mut state.doc, &corrupt)
                    .is_err()
            );
            assert_eq!(
                state.doc.clone().save(),
                before,
                "failed {extension} import mutated the document"
            );
            assert_eq!(state.doc.pending_ops(), pending);
            assert_eq!(wp.model_mut().caret(), &caret);
            assert_eq!(wp.path.as_deref(), Some(source.as_path()));
            assert_eq!(wp.save_status, DocumentSaveStatus::Modified);
            assert!(matches!(wp.toast.as_ref(), Some((_, ToastKind::Error))));
            assert_eq!(std::fs::read(&source).unwrap(), b"original source");
        }
        std::fs::remove_dir_all(dir).unwrap();
    }

    // Phase: successful_import_does_not_reuse_deleted_highest_block_id.
    {
        let (mut state, mut wp) = bound();
        wp.model_mut().insert_str(&mut state.doc, "keep");
        wp.model_mut().enter(&mut state.doc);
        let deleted_id = wp.model_mut().caret().block.clone();
        wp.model_mut().backspace(&mut state.doc);
        let containers = tilo_doc::model::open(&state.doc, &state.root).unwrap();
        assert!(!tilo_doc::model::blocks_in_order(&state.doc, &containers).contains(&deleted_id));

        assert!(wp.model_mut().replace_with_format(
            &mut state.doc,
            b"replacement document",
            WpFormat::Markdown,
        ));
        let replacement_ids = tilo_doc::model::blocks_in_order(&state.doc, &containers);
        assert!(!replacement_ids.is_empty());
        assert!(
            !replacement_ids.contains(&deleted_id),
            "a deleted block's durable identity must not be resurrected by import"
        );
        assert!(
            wp.model_mut()
                .block_texts(&state.doc)
                .join("\n")
                .contains("replacement document")
        );
    }

    // Phase: readonly_destination_save_as_preserves_source_and_destination.
    {
        let dir = temp_dir("readonly-save-as");
        std::fs::create_dir_all(&dir).unwrap();
        let source = dir.join("source.md");
        let destination = dir.join("protected.md");
        std::fs::write(&source, b"source bytes").unwrap();
        std::fs::write(&destination, b"protected bytes").unwrap();
        let (mut state, mut wp, _) =
            crate::open_document(&source, tilo_content::ContentStore::memory()).unwrap();
        wp.set_recovery_dir(dir.clone());
        wp.sync(&mut state.doc, &state.root);
        wp.model_mut().insert_str(&mut state.doc, "edited ");
        wp.save_status = DocumentSaveStatus::Modified;
        let original_permissions = std::fs::metadata(&destination).unwrap().permissions();
        let mut protected_permissions = original_permissions.clone();
        protected_permissions.set_readonly(true);
        std::fs::set_permissions(&destination, protected_permissions).unwrap();
        let result = wp.try_write_to_path(&state.doc, &destination, WpFormat::Markdown);
        // Restore the fixture attribute before asserting so failures leave no
        // read-only temporary files behind.
        std::fs::set_permissions(&destination, original_permissions).unwrap();
        assert!(result.is_err());
        assert_eq!(std::fs::read(&source).unwrap(), b"source bytes");
        assert_eq!(std::fs::read(&destination).unwrap(), b"protected bytes");
        assert_eq!(wp.path.as_deref(), Some(source.as_path()));
        assert_eq!(wp.save_status, DocumentSaveStatus::Modified);
        assert!(matches!(wp.toast.as_ref(), Some((_, ToastKind::Error))));
        std::fs::remove_dir_all(dir).unwrap();
    }

    // Phase: open_pick_replaces_the_document_in_place.
    // A resolved **Open** pick replaces the running document in place: a blank
    // editor, handed a chosen `.docx` on the drain path, adopts that file's text,
    // path, and format — the Ctrl+O path, exercised without a live portal.
    {
        let dir = std::env::temp_dir();
        let src = dir.join(format!("wp-open-{}.docx", std::process::id()));
        {
            let mut state = ui::State::new();
            with_doc(&mut state, |dm| {
                let b = dm.insert_block(None, "paragraph");
                dm.splice_text(&b, 0, 0, "Opened via portal");
            });
            crate::save_document(&state, &src, WpFormat::Docx, &tilo_content::NoContent)
                .expect("write source docx");
        }

        // Start from a blank, untitled editor.
        let (mut state, mut wp) = bound();
        wp.model_mut().insert_str(&mut state.doc, "stale content");
        assert!(wp.path.is_none(), "untitled before opening");

        // Ctrl+O's request, answered by the host chooser with the file.
        let (_host, _recorder) = host_answering(portal::PortalOutcome::Opened(src.clone()));
        wp.request_open_document();
        wp.sync(&mut state.doc, &state.root);

        let body = body_text(&wp, &state).join("\n");
        assert!(
            body.contains("Opened via portal"),
            "opened file's text is now shown: {body:?}"
        );
        assert!(
            !body.contains("stale content"),
            "the previous document was replaced"
        );
        assert_eq!(
            wp.path.as_deref(),
            Some(src.as_path()),
            "opened path adopted"
        );
        assert_eq!(wp.format, Some(WpFormat::Docx), "opened format adopted");

        let _ = std::fs::remove_file(&src);
    }
}

/// A dropped image inserts at the drop point, a selected image shows resize and flip controls, and its
/// position controls fit the narrow inspector.
#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one scenario test: each phase is a former standalone test kept verbatim as a scoped block"
)]
fn dropped_and_selected_images_expose_their_controls() {
    // Phase: dropped_image_inserts_at_the_drop_point.
    // Dropping an image file onto the page inserts it at the **drop point**: the
    // caret is first seated under the pointer, so the image lands next to the
    // paragraph the file was dropped on — not wherever the caret happened to be.
    {
        let mut framed = doc_with_headings(1000, 700);
        let (wp, state) = pipeline_draw(&mut framed, 1000, 700);
        // Pick a body paragraph well down the page to drop onto.
        let target = wp
            .laid
            .iter()
            .find(|l| l.kind == "paragraph" && l.y_top > wp.rect.y0 + 200.0)
            .expect("a body paragraph to drop onto")
            .clone();
        let drop_pt = Point::new(target.text_x + 20.0, target.y_top + target.height / 2.0);

        let taffy: TaffyTree<()> = TaffyTree::new();
        let node = NodeId::new(0);
        let mods = Modifiers::default();
        let mut handled = false;
        let mut focus: Option<NodeId> = None;
        // Move the pointer over the drop target (as a real drag-hover would), then
        // drop the file there.
        wp.handle_event(
            &WindowEvent::CursorMoved {
                device_id: unsafe { std::mem::zeroed() },
                position: (drop_pt.x, drop_pt.y).into(),
            },
            &mut ui::widget::EventCtx {
                scale_factor: 1.0,
                modifiers: &mods,
                state: &mut state.doc,
                path: &state.root,
                handled: &mut handled,
                focus_target: &mut focus,
                node_id: node,
                taffy: &taffy,
            },
        );
        let path = temp_png("drop");
        wp.handle_event(
            &WindowEvent::DroppedFile(path.clone()),
            &mut ui::widget::EventCtx {
                scale_factor: 1.0,
                modifiers: &mods,
                state: &mut state.doc,
                path: &state.root,
                handled: &mut handled,
                focus_target: &mut focus,
                node_id: node,
                taffy: &taffy,
            },
        );
        let _ = std::fs::remove_file(&path);
        assert_eq!(
            image_block_count(wp, state),
            1,
            "the dropped file inserts one image"
        );

        // The image landed adjacent to the drop-target paragraph — its document
        // index is right after the paragraph the pointer was over.
        let c = wp.model().containers.clone().unwrap();
        let order: Vec<String> = tilo_doc::model::blocks_in_order(&state.doc, &c);
        let img_idx = tilo_doc::model::materialize(&state.doc, &c)
            .blocks
            .iter()
            .position(|b| b.kind == "image")
            .and_then(|i| order.get(i).cloned());
        let target_idx = order
            .iter()
            .position(|id| id == &target.id)
            .expect("drop target in order");
        let img_pos = order
            .iter()
            .position(|id| Some(id) == img_idx.as_ref())
            .expect("image in order");
        assert!(
            img_pos == target_idx + 1 || img_pos == target_idx,
            "image inserted at the drop point (target idx {target_idx}, image idx {img_pos})"
        );
    }

    // Phase: selected_image_shows_resize_and_flip_controls.
    // A selected image exposes resize + flip controls in the panel.
    {
        let (wp, _id) = doc_with_selected_image();
        assert!(wp.selected_image_id().is_some(), "the image is selected");
        let has = |want: &PanelHit| wp.panel_hits.iter().any(|(_, h)| h == want);
        assert!(
            wp.panel_hits
                .iter()
                .any(|(_, h)| matches!(h, PanelHit::ImageScale(_))),
            "resize steppers present"
        );
        assert!(
            has(&PanelHit::ImageFlipH),
            "flip-horizontal control present"
        );
        assert!(has(&PanelHit::ImageFlipV), "flip-vertical control present");
    }

    // Phase: image_position_controls_fit_the_narrow_inspector_without_covering_the_heading.
    // Position keeps a dedicated heading above its three controls. The same
    // full-width geometry must hold in the 390px overlay, where a label/control
    // collision is otherwise easy to miss in a desktop-only test.
    {
        let (mut framed, _id) = doc_with_selected_image();
        let (wp, _) = pipeline_draw(&mut framed, 390, 844);

        let rect_for = |want: PanelHit| {
            wp.panel_hits
                .iter()
                .find(|(_, hit)| *hit == want)
                .map(|(rect, _)| *rect)
                .expect("position control is painted")
        };
        let free = rect_for(PanelHit::ImageFreePosition);
        let left = rect_for(PanelHit::ImageWrapLeft);
        let right = rect_for(PanelHit::ImageWrapRight);
        for rect in [free, left, right] {
            assert!(
                rect.x0 >= wp.panel_rect.x0 && rect.x1 <= wp.panel_rect.x1,
                "position control stays inside the narrow inspector: {rect:?}",
            );
        }
        assert!(
            free.x1 < left.x0 && left.x1 < right.x0,
            "controls do not overlap"
        );
        assert!(
            (free.y0 - left.y0).abs() < f64::EPSILON && (left.y0 - right.y0).abs() < f64::EPSILON,
            "placement choices share their own row below Position",
        );
    }
}

// ---- image resize / flip inspector ------------------------------------

/// Insert an image (via the model), select it (caret on the image block), draw
/// so the panel builds its controls, then return the widget + state ready for
/// the resize / flip tests.
fn doc_with_selected_image() -> (Framed<WordProcessorWidget>, String) {
    let mut state = ui::State::new();
    let mut wp = WordProcessorWidget::new();
    wp.sync(&mut state.doc, &state.root);
    wp.model_mut().insert_str(&mut state.doc, "caption");
    wp.model_mut().insert_image_bytes(&mut state.doc, TEST_PNG);
    wp.sync(&mut state.doc, &state.root);
    let c = wp.model().containers.clone().unwrap();
    let img_id = tilo_doc::model::materialize(&state.doc, &c)
        .blocks
        .iter()
        .find(|b| b.kind == "image")
        .expect("image inserted")
        .id
        .clone();
    // Select the image: seat the caret on its block.
    wp.model_mut().set_caret(crate::word_processor::Caret {
        block: img_id.clone(),
        offset: 0,
    });
    wp.set_styles_panel_open(true);
    (mount(state, wp, 1000, 700), img_id)
}

/// Image position controls are named, described and @-actionable, and a selected image resizes and
/// flips from the panel.
#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one scenario test: each phase is a former standalone test kept verbatim as a scoped block"
)]
fn image_controls_are_named_and_resize_and_flip() {
    // Phase: image_position_controls_are_named_described_and_at_actionable.
    // Compact visual labels are backed by explicit screen-reader names and
    // descriptions, and AT activation reaches the same canonical placement
    // mutation as a pointer click.
    {
        use ui::a11y::Role;

        crate::i18n::set_locale("en-US");
        let (mut framed, _id) = doc_with_selected_image();
        let (wp, state) = Framed::split(&mut framed);
        let tree = wp.accessibility_tree();
        let group = tree
            .children
            .iter()
            .find(|child| child.node.name.as_deref() == Some("Image position"))
            .expect("selected image placement group is exported");
        let expected = [
            "Free image position",
            "Image left with text wrap",
            "Image right with text wrap",
        ];
        assert_eq!(
            group
                .children
                .iter()
                .filter_map(|child| child.node.name.as_deref())
                .collect::<Vec<_>>(),
            expected,
        );
        assert!(group.children.iter().all(|child| {
            child.node.role == Role::Button
                && child.node.description.is_some()
                && child.node.bounds.is_some()
                && child.node.action_target.is_some()
                && child.node.state.focusable
        }));

        let target = "word-processor.image-placement:left".to_string();
        assert!(Widget::perform_action_at(
            wp,
            &ui::a11y::Action::Activate,
            &[ui::a11y::hash_key(&target)],
            &mut state.doc,
            &state.root,
        ));
        let containers = wp.model().containers.clone().expect("open document");
        let image = tilo_doc::model::materialize(&state.doc, &containers)
            .blocks
            .into_iter()
            .find(|block| block.kind == "image")
            .expect("selected image remains materialized");
        assert!(matches!(
            image.attrs.get("anchor"),
            Some(tilo_doc::AttrVal::Str(anchor)) if anchor == "paragraph"
        ));
        assert!(matches!(
            image.attrs.get("wrap"),
            Some(tilo_doc::AttrVal::Str(wrap)) if wrap == "left"
        ));
    }

    // Phase: selected_image_resizes_from_the_panel.
    // Clicking the panel's "Larger" control resizes the selected image (its
    // explicit width grows), driven through the production panel-hit path.
    {
        let (mut framed, id) = doc_with_selected_image();
        let (wp, state) = Framed::split(&mut framed);
        let (larger_rect, _) = wp
            .panel_hits
            .iter()
            .find(|(_, h)| matches!(h, PanelHit::ImageScale(f) if *f > 1.0))
            .expect("a 'Larger' resize control")
            .clone();
        let pt = Point::new(
            f64::midpoint(larger_rect.x0, larger_rect.x1),
            f64::midpoint(larger_rect.y0, larger_rect.y1),
        );
        left_click(wp, state, pt);
        let (w, _h) = wp
            .model()
            .image_size(&state.doc, &id)
            .expect("image now has an explicit size");
        assert!(
            w > 2.0,
            "the image grew beyond its 2px intrinsic width (w={w})"
        );
    }

    // Phase: selected_image_flips_from_the_panel.
    // Clicking the panel's "Flip Horizontal" control mirrors the selected image
    // (sets its `flip_h` attr) through the production path.
    {
        let (mut framed, id) = doc_with_selected_image();
        let (wp, state) = Framed::split(&mut framed);
        let (flip_rect, _) = wp
            .panel_hits
            .iter()
            .find(|(_, h)| *h == PanelHit::ImageFlipH)
            .expect("a flip-horizontal control")
            .clone();
        let pt = Point::new(
            f64::midpoint(flip_rect.x0, flip_rect.x1),
            f64::midpoint(flip_rect.y0, flip_rect.y1),
        );
        left_click(wp, state, pt);
        let c = wp.model().containers.clone().unwrap();
        let flipped = tilo_doc::model::materialize(&state.doc, &c)
            .blocks
            .iter()
            .find(|b| b.id == id)
            .is_some_and(|b| matches!(b.attrs.get("flip_h"), Some(tilo_doc::AttrVal::Bool(true))));
        assert!(
            flipped,
            "the image's flip_h attr is set after clicking Flip Horizontal"
        );
    }
}

// ---- paragraph-style delete / duplicate context menu ------------------

/// A doc with one custom paragraph style applied to its (only) paragraph.
fn doc_with_custom_style() -> (Framed<WordProcessorWidget>, String) {
    let mut state = ui::State::new();
    let mut wp = WordProcessorWidget::new();
    wp.sync(&mut state.doc, &state.root);
    wp.model_mut()
        .insert_str(&mut state.doc, "styled paragraph");
    // Duplicate Body into a fresh custom style and apply it to the caret block.
    let new_id = wp
        .model_mut()
        .duplicate_style(&mut state.doc, "body")
        .expect("duplicate body");
    wp.model_mut().set_block_style(&mut state.doc, &new_id);
    wp.sync(&mut state.doc, &state.root);
    (mount(state, wp, 1000, 700), new_id)
}

// ---- style size and direct character size stay distinct ----------------

// ---- table-of-contents feedback ---------------------------------------

// ---- shared ColorPicker adoption --------------------------------------

/// The colour picker popover applies a colour to the focused style and cancel reverts the preview.
#[test]
fn the_colour_picker_applies_and_cancels_previews() {
    // Phase: color_picker_popover_applies_a_colour_to_the_focused_style.
    // The style inspector's "Custom…" affordance opens the shared `ColorPicker`
    // popover, and dragging within it applies a colour to the focused style —
    // proving the toolkit widget is adopted (not a bespoke re-implementation).
    {
        let (mut state, mut wp) = bound();
        wp.model_mut().insert_str(&mut state.doc, "colour me");
        wp.view.styles_panel_visible = true;
        let mut framed = mount(state, wp, 1000, 700);
        let (wp, state) = Framed::split(&mut framed);

        // Click the "Custom…" affordance to open the picker.
        let (open_rect, _) = wp
            .panel_hits
            .iter()
            .find(|(_, h)| *h == PanelHit::OpenColorPicker)
            .expect("a 'Custom…' colour affordance in the inspector")
            .clone();
        left_click(
            wp,
            state,
            Point::new(
                f64::midpoint(open_rect.x0, open_rect.x1),
                f64::midpoint(open_rect.y0, open_rect.y1),
            ),
        );
        assert!(
            wp.color_picker.is_open(),
            "the shared ColorPicker popover opened"
        );

        // Lay the picker out and drive a press into the top-left of its SV square
        // (high value, some saturation) so it previews a fresh colour.
        wp.color_picker.layout(wp.rect, wp.panel_rect);
        let sv = wp
            .color_picker
            .part("color_picker.sv")
            .expect("the picker lays out its SV square");
        let sv_pt = Point::new(sv.x0 + 20.0, sv.y0 + 20.0);
        let style_before = wp.focused_style_key().map(|k| wp.sheet.get(&k).color);
        // CursorMoved (to set the picker's internal cursor) then press.
        let taffy: TaffyTree<()> = TaffyTree::new();
        let node = NodeId::new(0);
        let mods = Modifiers::default();
        let mut handled = false;
        let mut focus: Option<NodeId> = None;
        wp.handle_event(
            &WindowEvent::CursorMoved {
                device_id: unsafe { std::mem::zeroed() },
                position: (sv_pt.x, sv_pt.y).into(),
            },
            &mut ui::widget::EventCtx {
                scale_factor: 1.0,
                modifiers: &mods,
                state: &mut state.doc,
                path: &state.root,
                handled: &mut handled,
                focus_target: &mut focus,
                node_id: node,
                taffy: &taffy,
            },
        );
        wp.handle_event(
            &WindowEvent::MouseInput {
                device_id: unsafe { std::mem::zeroed() },
                state: ElementState::Pressed,
                button: MouseButton::Left,
            },
            &mut ui::widget::EventCtx {
                scale_factor: 1.0,
                modifiers: &mods,
                state: &mut state.doc,
                path: &state.root,
                handled: &mut handled,
                focus_target: &mut focus,
                node_id: node,
                taffy: &taffy,
            },
        );
        let style_after = wp.focused_style_key().map(|k| wp.sheet.get(&k).color);
        assert_ne!(
            style_before, style_after,
            "dragging in the ColorPicker changed the focused style's colour"
        );
    }

    // Phase: color_picker_cancel_reverts_the_previewed_colour.
    // Escape cancels the confirm-mode picker and restores the opening colour.
    {
        let (state, wp) = bound();
        let (mut framed, before) = preview_custom_colour(state, wp);
        let (wp, state) = Framed::split(&mut framed);
        press_named(wp, state, NamedKey::Escape);
        assert!(!wp.color_picker.is_open(), "Escape closes the picker");
        assert_eq!(
            wp.focused_style_key().map(|k| wp.sheet.get(&k).color),
            before,
            "Cancel restores the style's colour"
        );
    }
}

/// Open the picker on a styled paragraph and preview a colour from its SV
/// square; returns the mounted widget and the style colour before the preview.
fn preview_custom_colour(
    mut state: ui::State,
    mut wp: WordProcessorWidget,
) -> (Framed<WordProcessorWidget>, Option<[u8; 3]>) {
    wp.model_mut().insert_str(&mut state.doc, "preview me");
    wp.view.styles_panel_visible = true;
    let mut framed = mount(state, wp, 1000, 700);
    let (wp, state) = Framed::split(&mut framed);
    wp.open_color_picker();
    assert!(wp.color_picker.is_open(), "Custom… opens the picker");
    wp.color_picker.layout(wp.rect, wp.panel_rect);
    let before = wp.focused_style_key().map(|k| wp.sheet.get(&k).color);
    let sv = wp
        .color_picker
        .part("color_picker.sv")
        .expect("the picker lays out its SV square");
    left_click(wp, state, Point::new(sv.x0 + 20.0, sv.y0 + 20.0));
    // End the SV drag the press started, as the pointer would.
    let mut handled = false;
    let mut focus: Option<NodeId> = None;
    wp.handle_event(
        &WindowEvent::MouseInput {
            device_id: unsafe { std::mem::zeroed() },
            state: ElementState::Released,
            button: MouseButton::Left,
        },
        &mut ui::widget::EventCtx {
            scale_factor: 1.0,
            modifiers: &Modifiers::default(),
            state: &mut state.doc,
            path: &state.root,
            handled: &mut handled,
            focus_target: &mut focus,
            node_id: NodeId::new(0),
            taffy: &TaffyTree::new(),
        },
    );
    assert_ne!(
        wp.focused_style_key().map(|k| wp.sheet.get(&k).color),
        before,
        "the pending colour previews on the style"
    );
    (framed, before)
}

/// Done keeps the previewed colour and an outside click dismisses the popover.
#[test]
fn the_colour_picker_keeps_or_dismisses_its_preview() {
    // Phase: color_picker_done_keeps_the_previewed_colour.
    // Done keeps the previewed colour.
    {
        let (state, wp) = bound();
        let (mut framed, before) = preview_custom_colour(state, wp);
        let (wp, state) = Framed::split(&mut framed);
        let previewed = wp.focused_style_key().map(|k| wp.sheet.get(&k).color);
        let done = wp
            .color_picker
            .part("color_picker.done")
            .expect("a confirm-mode picker has a Done button");
        wp.color_picker.layout(wp.rect, wp.panel_rect);
        left_click(wp, state, done.center());
        assert!(!wp.color_picker.is_open(), "Done closes the picker");
        let after = wp.focused_style_key().map(|k| wp.sheet.get(&k).color);
        assert_eq!(after, previewed, "Done keeps the preview");
        assert_ne!(after, before);
    }

    // Phase: color_picker_popover_dismisses_on_outside_click.
    // A click outside the open picker dismisses it.
    {
        let (mut state, mut wp) = bound();
        wp.model_mut().insert_str(&mut state.doc, "x");
        wp.view.styles_panel_visible = true;
        let mut framed = mount(state, wp, 1000, 700);
        let (wp, _) = Framed::split(&mut framed);
        wp.open_color_picker();
        assert!(wp.color_picker.is_open());
        let (wp, state) = pipeline_draw(&mut framed, 1000, 700);
        // Click far away from the popover (top-left corner of the canvas).
        left_click(wp, state, Point::new(80.0, 400.0));
        assert!(
            !wp.color_picker.is_open(),
            "an outside click closes the colour picker"
        );
    }
}

#[test]
#[ignore = "GPU pixel equivalence; run explicitly when changing text clipping"]
#[expect(
    clippy::too_many_lines,
    reason = "end-to-end raster scenario that reads top to bottom: seed review state, render, compare, then re-check state"
)]
fn clipped_document_text_matches_reference_raster_and_preserves_review_state() {
    let body = format!(
        "Annotated text {}",
        "wrapped words for pagination ".repeat(220)
    );
    let (wp, mut state, c, mut ids, first) = doc_with_paragraph(&body);
    {
        let mut dm = tilo_doc::DocMut {
            doc: &mut state.doc,
            c: &c,
            ids: &mut ids,
        };
        let note = dm.insert_footnote(&first, 10);
        let note_block = dm.insert_note_block(&note, None, "paragraph").unwrap();
        dm.splice_note_text(
            &note,
            &note_block,
            0,
            0,
            "Footnote remains associated with its page.",
        );
        dm.add_comment(&first, 0, 9, "Reviewer", "Keep this comment.");
        let change = dm.insert_change(tilo_doc::ChangeKind::Ins, "Reviewer", "");
        dm.add_change_mark(tilo_doc::ChangeKind::Ins, &first, 12, 20, &change);
        let mut after = first;
        for i in 0..24 {
            let id = dm.insert_block(Some(&after), "paragraph");
            dm.splice_text(
                &id,
                0,
                0,
                &format!("Paragraph {i}. {}", "Café text with wrapping. ".repeat(30)),
            );
            after = id;
        }
    }
    let (w, h) = (900, 1100);
    let mut framed = mount(state, wp, w, h);
    let (wp, _) = Framed::split(&mut framed);
    wp.view.auto_fit_page = false;
    let mut tr = TextRenderer::new();
    let mut renderer = ui::offscreen::OffscreenRenderer::new(w, h);
    let theme = Theme::dark();
    let presence = PresenceCtx::empty();
    let permission = PermissionContext::permissive();
    for (scroll, zoom) in [(0.0, 1.0), (932.25, 0.87), (5000.5, 1.25)] {
        wp.zoom.set(zoom);
        wp.scroll_y = scroll;
        let mut scenes = Vec::new();
        let mut geometry = Vec::new();
        for reference in [true, false] {
            wp.reference_text_rendering = reference;
            let mut scene = Scene::new();
            wp.draw_with_presence(
                &mut scene,
                Affine::IDENTITY,
                &mut tr,
                &theme,
                &presence,
                &permission,
            );
            wp.draw_overlay(&mut scene, Affine::IDENTITY, &mut tr, &theme, &presence);
            geometry.push(
                wp.laid
                    .iter()
                    .map(|l| (l.id.clone(), l.y_top, l.height, l.page))
                    .collect::<Vec<_>>(),
            );
            scenes.push(scene);
        }
        assert_eq!(
            geometry[0], geometry[1],
            "clipping must not change pagination or hit geometry"
        );
        assert_eq!(wp.notes.len(), 1);
        assert_eq!(wp.review.comments().len(), 1);
        assert_eq!(wp.review.changes().len(), 1);
        let bg = Color::from_rgb8(0x20, 0x22, 0x28);
        // Vello can vary a few quantized edge bytes even for an identical
        // scene. Measure that reference-only envelope; all other bytes must
        // match exactly, with no blanket pixel/AA tolerance.
        let mut low = renderer.render(&scenes[0], bg);
        let mut high = low.clone();
        for _ in 0..63 {
            for ((low, high), value) in low
                .iter_mut()
                .zip(&mut high)
                .zip(renderer.render(&scenes[0], bg))
            {
                *low = (*low).min(value);
                *high = (*high).max(value);
            }
        }
        let variable = low.iter().zip(&high).filter(|(a, b)| a != b).count();
        assert!(
            variable <= 256 && low.iter().zip(&high).all(|(a, b)| b - a <= 1),
            "reference raster is not stable enough for this oracle: {variable} variable bytes"
        );
        for _ in 0..4 {
            let clipped = renderer.render(&scenes[1], bg);
            let mismatches = clipped
                .iter()
                .zip(low.iter().zip(&high))
                .filter(|(value, (low, high))| value < low || value > high)
                .count();
            if mismatches > 0 {
                eprintln!(
                    "outside envelope {:?}",
                    clipped
                        .iter()
                        .zip(low.iter().zip(&high))
                        .enumerate()
                        .filter(|(_, (v, (a, b)))| v < a || v > b)
                        .take(20)
                        .map(|(i, (v, (a, b)))| (
                            i / 4 % w as usize,
                            i / 4 / w as usize,
                            i % 4,
                            v,
                            a,
                            b
                        ))
                        .collect::<Vec<_>>()
                );
            }
            assert_eq!(
                mismatches, 0,
                "document raster exceeds reference-only envelope at scroll={scroll}, zoom={zoom}"
            );
        }
        let full_glyphs = scenes[0].encoding().resources.glyphs.len();
        let clipped_glyphs = scenes[1].encoding().resources.glyphs.len();
        assert!(
            clipped_glyphs * 3 < full_glyphs,
            "offscreen glyphs must be omitted: {clipped_glyphs}/{full_glyphs}"
        );
    }
}

// ---- visual review shots ----------------------------------------------
// Renderers that write PNGs of the fixed surfaces for eyeballing. They assert
// nothing, so they are not part of an ordinary test run; render them with
//
//   TILO_WP_REVIEW_SHOTS=<dir> cargo test -p word-processor --lib review_shots -- --ignored

/// The directory review shots are written to. Asking for shots without saying
/// where to put them is a mistake worth failing on, not a silent pass.
fn review_shot_dir() -> std::path::PathBuf {
    std::env::var_os("TILO_WP_REVIEW_SHOTS")
        .map(std::path::PathBuf::from)
        .expect("set TILO_WP_REVIEW_SHOTS=<dir> to choose where review shots are written")
}

/// Run one frame and write it as a PNG to `dir/name.png`, handing back the
/// widget and its document for what follows.
fn render_review_png<'a>(
    framed: &'a mut Framed<WordProcessorWidget>,
    w: u32,
    h: u32,
    dir: &std::path::Path,
    name: &str,
) -> (&'a mut WordProcessorWidget, &'a mut ui::State) {
    render_review_png_themed(framed, w, h, dir, name, &Theme::dark());
    Framed::split(framed)
}

fn render_review_png_themed(
    framed: &mut Framed<WordProcessorWidget>,
    w: u32,
    h: u32,
    dir: &std::path::Path,
    name: &str,
    theme: &Theme,
) {
    use ui::offscreen::OffscreenRenderer;
    pipeline_draw_themed(framed, w, h, theme);
    std::fs::create_dir_all(dir).ok();
    OffscreenRenderer::new(w, h).render_png(
        Framed::harness(framed).scene(),
        Color::from_rgb8(0x20, 0x22, 0x28),
        dir.join(format!("{name}.png")),
    );
}

#[test]
#[ignore = "writes review PNGs and asserts nothing; run with TILO_WP_REVIEW_SHOTS=<dir> and --ignored"]
fn menu_ux_review_shots() {
    let dir = review_shot_dir();
    let dir = dir.as_path();
    for (name, theme, width) in [
        ("dark", Theme::dark(), 1000),
        ("light", Theme::light(), 1000),
        ("large", Theme::dark().with_text_scale(2.0), 480),
    ] {
        let (mut state, mut wp) = bound();
        wp.inner.insert_str(&mut state.doc, "Keep my unsaved draft");
        wp.save_status = DocumentSaveStatus::Modified;
        wp.request_template(&mut state.doc, crate::Template::Report);
        let mut framed = mount_themed(state, wp, width, 600, &theme);
        render_review_png_themed(
            &mut framed,
            width,
            600,
            dir,
            &format!("unsaved-{name}"),
            &theme,
        );
        let (wp, state) = Framed::split(&mut framed);
        wp.resolve_unsaved_choice(&mut state.doc, UnsavedChoice::Cancel);
        let point = first_block_start_point(wp);
        wp.open_context_menu(&state.doc, point, [num::f32_of_u32(width) - 2.0, 598.0]);
        wp.context_menu.focus_first_action();
        render_review_png_themed(
            &mut framed,
            width,
            600,
            dir,
            &format!("context-edge-{name}"),
            &theme,
        );
    }
}

#[test]
#[ignore = "writes review PNGs and asserts nothing; run with TILO_WP_REVIEW_SHOTS=<dir> and --ignored"]
fn render_review_shots() {
    let dir = review_shot_dir();
    let (w, h) = (1200u32, 800u32);

    // 1. Fixed toolbar: vector Image icon, no duplicated font-size stepper.
    {
        let (mut state, mut wp) = bound();
        wp.model_mut()
            .insert_str(&mut state.doc, "The quick brown fox");
        wp.model_mut().enter(&mut state.doc);
        wp.model_mut().insert_str(
            &mut state.doc,
            "Body paragraph with some words to fill the line nicely.",
        );
        let mut framed = mount(state, wp, w, h);
        render_review_png(&mut framed, w, h, &dir, "toolbar");
    }

    // 2a. Style context menu on a CUSTOM style: Delete + Duplicate.
    {
        let (mut framed, custom_id) = doc_with_custom_style();
        let (wp, _) = Framed::split(&mut framed);
        // Anchor the menu over the custom chip in the panel.
        let anchor = wp
            .panel_hits
            .iter()
            .find(|(_, hh)| matches!(hh, PanelHit::Pick(id) if id == &custom_id))
            .map_or(Point::new(950.0, 240.0), |(r, _)| {
                Point::new(r.x0 + 10.0, r.y1 + 2.0)
            });
        wp.style_menu.open(&custom_id, anchor);
        render_review_png(&mut framed, w, h, &dir, "style-menu-custom");
    }

    // 2b. Style context menu on a BUILT-IN style: Duplicate only.
    {
        let (mut framed, _id) = doc_with_custom_style();
        let (wp, _) = Framed::split(&mut framed);
        let anchor = wp
            .panel_hits
            .iter()
            .find(|(_, hh)| matches!(hh, PanelHit::Pick(id) if id == "body"))
            .map_or(Point::new(950.0, 240.0), |(r, _)| {
                Point::new(r.x0 + 10.0, r.y1 + 2.0)
            });
        wp.style_menu.open("body", anchor);
        render_review_png(&mut framed, w, h, &dir, "style-menu-builtin");
    }

    // 3. Selected image with resize + flip controls in the panel. Give it a
    //    visible display size (the fixture PNG is 2×3) so the selection box +
    //    handles read in the shot.
    {
        let (mut framed, id) = doc_with_selected_image();
        let (wp, state) = Framed::split(&mut framed);
        wp.model_mut()
            .set_image_size(&mut state.doc, &id, 300.0, 200.0);
        wp.model_mut().flip_image(&mut state.doc, &id, true);
        render_review_png(&mut framed, w, h, &dir, "image-selected");
    }

    // 4. Generated table of contents (+ its confirmation toast).
    {
        let mut framed = doc_with_headings(w, h);
        let (wp, state) = Framed::split(&mut framed);
        wp.apply_tool(&mut state.doc, ToolAction::InsertToc);
        render_review_png(&mut framed, w, h, &dir, "toc");
    }

    // 5. The shared ColorPicker popover, opened from the style inspector.
    {
        let (mut state, mut wp) = bound();
        wp.model_mut()
            .insert_str(&mut state.doc, "colour this paragraph");
        wp.view.styles_panel_visible = true;
        let mut framed = mount(state, wp, w, h);
        let (wp, _) = Framed::split(&mut framed);
        wp.open_color_picker();
        render_review_png(&mut framed, w, h, &dir, "color-picker");
    }

    // 6. Comment and tracked-change margin cards from the extracted review
    // component, including its Resolve and Accept/Reject action faces.
    {
        let (wp, mut state, c, mut ids, bid) =
            doc_with_paragraph("Keep this passage and review the ending.");
        {
            let mut doc = tilo_doc::DocMut {
                doc: &mut state.doc,
                c: &c,
                ids: &mut ids,
            };
            doc.add_comment(&bid, 5, 17, "Ada", "This needs a source.");
            let change = doc.insert_change(tilo_doc::ChangeKind::Ins, "Lin", "");
            doc.add_change_mark(tilo_doc::ChangeKind::Ins, &bid, 29, 39, &change);
        }
        state.doc.commit();
        let mut framed = mount(state, wp, w, h);
        render_review_png(&mut framed, w, h, &dir, "review-overlay");
    }
}

/// Responsive visual-review surfaces. Kept behind the same opt-in output
/// directory as the broader review gallery so CI remains deterministic and
/// headless; reviewers can inspect the normal desktop, full-toolbar, narrow
/// compact, and compact-overflow states from one rendering pass.
#[test]
#[ignore = "writes review PNGs and asserts nothing; run with TILO_WP_REVIEW_SHOTS=<dir> and --ignored"]
fn render_responsive_review_shots() {
    let dir = review_shot_dir();
    let mut state = ui::State::new();
    let mut wp = WordProcessorWidget::new();
    wp.sync(&mut state.doc, &state.root);
    wp.model_mut().insert_str(
        &mut state.doc,
        "A responsive Writer document with direct formatting, comments, and accessible controls.",
    );
    wp.model_mut().enter(&mut state.doc);
    wp.model_mut().insert_str(
        &mut state.doc,
        "The compact toolbar keeps writing primary and moves secondary commands into More.",
    );
    wp.set_focus(true);

    let mut framed = mount(state, wp, 1280, 800);
    render_review_png(&mut framed, 1280, 800, &dir, "responsive-1280-focus");
    render_review_png(&mut framed, 1800, 900, &dir, "responsive-1800-full");
    render_review_png(&mut framed, 760, 800, &dir, "responsive-760-narrow");

    // The menu is opened through the same toolbar action as a pointer or
    // screen-reader activation, proving compact colour/highlight commands
    // remain discoverable rather than merely existing in code.
    let (wp, state) =
        render_review_png(&mut framed, 1280, 800, &dir, "responsive-1280-before-more");
    wp.apply_tool(&mut state.doc, ToolAction::More);
    let (wp, _) = render_review_png(&mut framed, 1280, 800, &dir, "responsive-1280-more");
    wp.context_menu.close();
    wp.pointer = Point::new(410.0, TOOLBAR_H);
    wp.open_compact_swatch_palette(PaletteKind::Text);
    let (wp, _) = render_review_png(&mut framed, 1280, 800, &dir, "responsive-1280-text-palette");
    wp.compact_swatch_palette.close();
    let (wp, _) = render_review_png(&mut framed, 760, 800, &dir, "responsive-760-before-palette");
    wp.pointer = Point::new(400.0, TOOLBAR_H);
    wp.open_compact_swatch_palette(PaletteKind::Highlight);
    render_review_png(
        &mut framed,
        760,
        800,
        &dir,
        "responsive-760-highlight-palette",
    );
}

/// A canonical save preserves the symlink target and private permissions, an unsupported link Save As
/// keeps the source, and the close requirement survives cancelled saves and detects host undo.
#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one scenario test: each phase is a former standalone test kept verbatim as a scoped block"
)]
fn canonical_saves_preserve_links_permissions_and_close_requirements() {
    // Phase: canonical_save_preserves_symlink_target_and_private_permissions.
    #[cfg(unix)]
    {
        use std::os::unix::fs::{PermissionsExt, symlink};
        let dir = temp_dir("save-link-permissions");
        std::fs::create_dir_all(&dir).unwrap();
        let target = dir.join("private.md");
        let link = dir.join("document.md");
        std::fs::write(&target, b"original source").unwrap();
        std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o600)).unwrap();
        symlink("private.md", &link).unwrap();
        let (mut state, mut wp, _) =
            crate::open_document(&link, tilo_content::ContentStore::memory()).unwrap();
        wp.set_recovery_dir(dir.clone());
        wp.sync(&mut state.doc, &state.root);
        wp.model_mut().insert_str(&mut state.doc, "edited ");
        wp.save_status = DocumentSaveStatus::Modified;
        let payload =
            serde_json::to_vec(&crate::actions::WordProcessorOperation::Save { path: None })
                .unwrap();
        wp.start_operation(
            &ui::widget::ResourceOperation::local(
                tilo_actions::InvocationId::new(),
                tilo_actions::OperationKind::Effect,
                crate::actions::WORD_PROCESSOR_OPERATION_TYPE,
                &payload,
            ),
            &mut state.doc,
            &state.root,
        )
        .unwrap();
        assert!(
            std::fs::symlink_metadata(&link)
                .unwrap()
                .file_type()
                .is_symlink()
        );
        assert_eq!(
            std::fs::read_link(&link).unwrap(),
            std::path::PathBuf::from("private.md")
        );
        assert!(
            String::from_utf8(std::fs::read(&target).unwrap())
                .unwrap()
                .contains("edited")
        );
        assert_eq!(
            std::fs::metadata(&target).unwrap().permissions().mode() & 0o7777,
            0o600
        );
        assert_eq!(wp.path.as_deref(), Some(link.as_path()));
        assert_eq!(wp.save_status, DocumentSaveStatus::Clean);
        // The public export/save API has the same target and permission contract.
        crate::save_document(&state, &link, WpFormat::Markdown, &tilo_content::NoContent).unwrap();
        assert!(
            std::fs::symlink_metadata(&link)
                .unwrap()
                .file_type()
                .is_symlink()
        );
        assert_eq!(
            std::fs::metadata(&target).unwrap().permissions().mode() & 0o7777,
            0o600
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    // Phase: unsupported_link_save_as_keeps_source_identity_and_bytes.
    #[cfg(unix)]
    {
        use std::os::unix::fs::symlink;
        let dir = temp_dir("save-unsupported-link");
        std::fs::create_dir_all(&dir).unwrap();
        let source = dir.join("source.md");
        let target = dir.join("target.md");
        let linked = dir.join("hardlink.md");
        let dangling = dir.join("dangling.md");
        std::fs::write(&source, b"source bytes").unwrap();
        std::fs::write(&target, b"target bytes").unwrap();
        std::fs::hard_link(&target, &linked).unwrap();
        symlink("missing.md", &dangling).unwrap();
        let (mut state, mut wp, _) =
            crate::open_document(&source, tilo_content::ContentStore::memory()).unwrap();
        wp.set_recovery_dir(dir.clone());
        wp.sync(&mut state.doc, &state.root);
        wp.model_mut().insert_str(&mut state.doc, "edited ");
        wp.save_status = DocumentSaveStatus::Modified;
        for destination in [&linked, &dangling] {
            let payload = serde_json::to_vec(&crate::actions::WordProcessorOperation::SaveAs {
                path: Some(destination.to_string_lossy().into_owned()),
            })
            .unwrap();
            assert!(
                wp.start_operation(
                    &ui::widget::ResourceOperation::local(
                        tilo_actions::InvocationId::new(),
                        tilo_actions::OperationKind::Effect,
                        crate::actions::WORD_PROCESSOR_OPERATION_TYPE,
                        &payload
                    ),
                    &mut state.doc,
                    &state.root
                )
                .is_err()
            );
            assert_eq!(wp.path.as_deref(), Some(source.as_path()));
            assert_eq!(wp.save_status, DocumentSaveStatus::Modified);
            assert_eq!(std::fs::read(&source).unwrap(), b"source bytes");
            assert_eq!(std::fs::read(&target).unwrap(), b"target bytes");
            assert_eq!(std::fs::read(&linked).unwrap(), b"target bytes");
            assert!(
                std::fs::symlink_metadata(&dangling)
                    .unwrap()
                    .file_type()
                    .is_symlink()
            );
            assert!(!dir.join("missing.md").exists());
        }
        std::fs::remove_dir_all(dir).unwrap();
    }

    // Phase: close_requirement_survives_cancelled_save_as_and_failed_canonical_save.
    {
        let dir = temp_dir("close-admission");
        std::fs::create_dir_all(&dir).unwrap();
        let source = dir.join("source.md");
        std::fs::write(&source, "original source").unwrap();
        let (mut state, mut wp, _) =
            crate::open_document(&source, tilo_content::ContentStore::memory()).unwrap();
        wp.set_recovery_dir(dir.clone());
        wp.sync(&mut state.doc, &state.root);
        assert!(ui::component_lifecycle::unsaved_documents(&wp, &state.doc).is_empty());
        wp.model_mut().insert_str(&mut state.doc, "unsaved ");
        wp.save_status = DocumentSaveStatus::Modified;
        let before = ui::component_lifecycle::unsaved_documents(&wp, &state.doc);
        assert_eq!(before.len(), 1);
        let pending_ops = state.doc.pending_ops();
        let invocation = tilo_actions::InvocationId::new();
        wp.resource_file_invocation = Some(invocation);
        let (_host, _recorder) = host_answering(portal::PortalOutcome::Cancelled);
        wp.request_save_as();
        wp.sync(&mut state.doc, &state.root);
        assert!(matches!(
            wp.poll_resource_operation(invocation, false, &state.doc, &state.root),
            Some(tilo_actions::InvocationUpdate::Cancelled { .. })
        ));
        assert_eq!(
            ui::component_lifecycle::unsaved_documents(&wp, &state.doc),
            before
        );
        assert_eq!(state.doc.pending_ops(), pending_ops);
        let payload = serde_json::to_vec(&crate::actions::WordProcessorOperation::SaveAs {
            path: Some(dir.to_string_lossy().into_owned()),
        })
        .unwrap();
        assert!(
            wp.start_operation(
                &ui::widget::ResourceOperation::local(
                    tilo_actions::InvocationId::new(),
                    tilo_actions::OperationKind::Effect,
                    crate::actions::WORD_PROCESSOR_OPERATION_TYPE,
                    &payload
                ),
                &mut state.doc,
                &state.root
            )
            .is_err()
        );
        assert_eq!(
            ui::component_lifecycle::unsaved_documents(&wp, &state.doc),
            before
        );
        assert_eq!(std::fs::read_to_string(&source).unwrap(), "original source");
        let payload =
            serde_json::to_vec(&crate::actions::WordProcessorOperation::Save { path: None })
                .unwrap();
        assert!(matches!(
            wp.start_operation(
                &ui::widget::ResourceOperation::local(
                    tilo_actions::InvocationId::new(),
                    tilo_actions::OperationKind::Effect,
                    crate::actions::WORD_PROCESSOR_OPERATION_TYPE,
                    &payload
                ),
                &mut state.doc,
                &state.root
            )
            .unwrap(),
            Some(ui::widget::ResourceOperationStart::Completed(_))
        ));
        assert!(ui::component_lifecycle::unsaved_documents(&wp, &state.doc).is_empty());
        assert!(
            std::fs::read_to_string(&source)
                .unwrap()
                .contains("unsaved")
        );
        // The render cache also follows uncommitted edits and their rollback,
        // without creating an undo boundary just to observe either state.
        state.commit_action(None, None);
        let saved_projection = wp.blocks.clone();
        wp.model_mut().insert_str(&mut state.doc, "temporary ");
        let pending = state.doc.pending_ops();
        assert!(pending > 0);
        wp.sync(&mut state.doc, &state.root);
        assert_eq!(state.doc.pending_ops(), pending);
        assert!(
            wp.blocks
                .iter()
                .any(|block| block.text.contains("temporary"))
        );
        assert!(state.doc.rollback() > 0);
        wp.sync(&mut state.doc, &state.root);
        assert_eq!(state.doc.pending_ops(), 0);
        assert_eq!(wp.blocks, saved_projection);
        assert!(ui::component_lifecycle::unsaved_documents(&wp, &state.doc).is_empty());
        std::fs::remove_dir_all(dir).unwrap();
    }

    // Phase: close_requirement_detects_host_undo_after_successful_save_without_widget_dirty_flag.
    {
        let dir = temp_dir("close-host-undo");
        std::fs::create_dir_all(&dir).unwrap();
        let source = dir.join("source.md");
        std::fs::write(&source, "before editing").unwrap();
        let (mut state, mut wp, _) =
            crate::open_document(&source, tilo_content::ContentStore::memory()).unwrap();
        wp.set_recovery_dir(dir.clone());
        wp.sync(&mut state.doc, &state.root);
        state.commit_action(None, None);
        wp.model_mut().insert_str(&mut state.doc, "saved edit ");
        state.commit_action(None, None);
        wp.try_write_to_current_path(&state.doc).unwrap();
        let saved_bytes = std::fs::read(&source).unwrap();
        assert!(ui::component_lifecycle::unsaved_documents(&wp, &state.doc).is_empty());
        assert!(
            state.undo(None, None).is_some(),
            "use the host State undo service"
        );
        assert!(
            wp.save_status == DocumentSaveStatus::Clean,
            "host undo bypasses widget event bookkeeping"
        );
        let before = state.doc.pending_ops();
        assert_eq!(
            ui::component_lifecycle::unsaved_documents(&wp, &state.doc).len(),
            1
        );
        assert_eq!(
            state.doc.pending_ops(),
            before,
            "close inspection must not commit the undo transaction"
        );
        assert_eq!(std::fs::read(&source).unwrap(), saved_bytes);
        std::fs::remove_dir_all(dir).unwrap();
    }
}

/// Window groups offer an unclaimed event to each visible app in presentation
/// order. A zoom gesture must therefore leave an unfocused Writer untouched,
/// so the Writer selected by the host can receive it instead. When a
/// non-Writer owns focus neither retained Writer may consume or mutate it.
#[test]
#[expect(
    clippy::too_many_lines,
    reason = "one scenario covering pinch and Ctrl+wheel across focused and unfocused Writers"
)]
fn pinch_and_ctrl_wheel_only_zoom_the_host_focused_writer() {
    use winit::{
        dpi::PhysicalPosition,
        event::{DeviceId, MouseScrollDelta, TouchPhase},
        keyboard::ModifiersState,
    };

    let drawn_at_rest = || {
        let (state, mut writer) = populated();
        writer.set_zoom(1.0);
        let mut framed = mount(state, writer, 1000, 700);
        framed.pointer = Point::new(500.0, 300.0);
        framed
    };
    let mut first_framed = drawn_at_rest();
    let mut second_framed = drawn_at_rest();
    let (first, first_state) = Framed::split(&mut first_framed);
    let (second, second_state) = Framed::split(&mut second_framed);

    // The first visible Writer is stale/unfocused; the selected second Writer
    // must receive the same native pinch after the first declines it.
    first.set_focus(false);
    second.set_focus(true);
    assert!(
        !pinch(first, first_state, 0.025, TouchPhase::Moved),
        "an unfocused Writer must leave pinch for the selected sibling"
    );
    assert!(pinch(second, second_state, 0.025, TouchPhase::Moved));
    assert_eq!(first.zoom.factor(), 1.0);
    assert_eq!(second.zoom.factor(), 1.025);

    // Browser trackpads encode the same gesture as Ctrl + pixel wheel. The
    // same unfocused-first/focused-second delivery order must select the
    // second Writer, rather than letting the first visible Writer steal it.
    let ctrl: Modifiers = ModifiersState::CONTROL.into();
    let ctrl_wheel = WindowEvent::MouseWheel {
        device_id: DeviceId::dummy(),
        delta: MouseScrollDelta::PixelDelta(PhysicalPosition::new(0.0, 24.0)),
        phase: TouchPhase::Moved,
    };
    let mut first_handled = false;
    first.handle_event(
        &ctrl_wheel,
        &mut ui::widget::EventCtx {
            scale_factor: 1.0,
            modifiers: &ctrl,
            state: &mut first_state.doc,
            path: &first_state.root,
            handled: &mut first_handled,
            focus_target: &mut None,
            node_id: NodeId::new(0),
            taffy: &TaffyTree::new(),
        },
    );
    assert!(
        !first_handled,
        "an unfocused Writer must leave browser Ctrl+wheel for the selected sibling"
    );
    assert_eq!(first.zoom.factor(), 1.0);
    let mut second_handled = false;
    second.handle_event(
        &ctrl_wheel,
        &mut ui::widget::EventCtx {
            scale_factor: 1.0,
            modifiers: &ctrl,
            state: &mut second_state.doc,
            path: &second_state.root,
            handled: &mut second_handled,
            focus_target: &mut None,
            node_id: NodeId::new(0),
            taffy: &TaffyTree::new(),
        },
    );
    assert!(second_handled, "the focused Writer owns Ctrl+wheel zoom");
    assert_eq!(
        second.zoom.factor(),
        1.1,
        "Ctrl+wheel uses Writer's tenth-step zoom quantization"
    );

    // Focus may switch back to the first Writer; it then owns ordinary
    // native pinch while the second document remains exactly unchanged.
    first.set_focus(true);
    second.set_focus(false);
    assert!(pinch(first, first_state, 0.025, TouchPhase::Moved));
    assert_eq!(first.zoom.factor(), 1.025);
    assert_eq!(second.zoom.factor(), 1.1);

    // Selecting a non-Writer leaves every Writer unfocused. Neither may
    // claim or mutate a subsequent pinch; this is the regression reported by
    // the browser host where a hidden Writer used to zoom in the background.
    first.set_focus(false);
    let first_before = first.zoom.factor();
    let second_before = second.zoom.factor();
    // Focus ownership blocks zoom only. A plain wheel remains pointer-routed,
    // so an unfocused Writer still pans if its document has overflow.
    let plain_wheel = WindowEvent::MouseWheel {
        device_id: DeviceId::dummy(),
        delta: MouseScrollDelta::PixelDelta(PhysicalPosition::new(0.0, -24.0)),
        phase: TouchPhase::Moved,
    };
    let first_scroll_before = first.scroll_y;
    let mut plain_handled = false;
    first.handle_event(
        &plain_wheel,
        &mut ui::widget::EventCtx {
            scale_factor: 1.0,
            modifiers: &Modifiers::default(),
            state: &mut first_state.doc,
            path: &first_state.root,
            handled: &mut plain_handled,
            focus_target: &mut None,
            node_id: NodeId::new(0),
            taffy: &TaffyTree::new(),
        },
    );
    assert!(plain_handled, "plain wheel remains pointer-routed");
    assert!(
        first.scroll_y > first_scroll_before,
        "plain wheel pans overflow"
    );
    assert!(!pinch(first, first_state, 0.025, TouchPhase::Moved));
    assert!(!pinch(second, second_state, 0.025, TouchPhase::Moved));
    assert_eq!(first.zoom.factor(), first_before);
    assert_eq!(second.zoom.factor(), second_before);
}

/// A `LIVE_V1` caret whose block id is too long for the 64-byte anchor arrives
/// under the digest stand-in key; the widget resolves it to its own block, so
/// agent-written (130-byte id) paragraphs still show carets and selections.
#[test]
fn a_digest_addressed_remote_caret_resolves_to_its_block() {
    let wp = drawn_focused();
    let block_id = first_block_id(&wp);
    let digest = block_presence_key(false, &block_id).digest_form().unwrap();
    assert!(is_scope_digest_key(&digest));
    let peer = "agent-publisher";
    let mut presence = PresenceCtx::empty();
    presence.set_remote_caret(peer.into(), digest.clone(), 3);
    presence.set_selection(
        &digest,
        UserId(peer.into()),
        SelectionRange { start: 0, end: 2 },
    );
    let (carets, selections) = wp.remote_presence_overlays(&presence);
    assert_eq!(carets.len(), 1, "digest caret drawn once, in its block");
    assert_eq!(selections.len(), 1);
    // A digest naming no block of this document draws nothing.
    let mut stray = PresenceCtx::empty();
    stray.set_remote_caret(
        peer.into(),
        ui::presence::PresenceKey::from_scope_digest([7; 16]),
        1,
    );
    assert!(wp.remote_presence_overlays(&stray).0.is_empty());
}

#[test]
fn owner_release_refuses_active_edit_then_detaches_file_work_without_losing_draft() {
    let mut wp = WordProcessorWidget::new();
    let invocation = tilo_actions::InvocationId::new();
    let selected = PathBuf::from("selected.md");
    wp.path = Some(selected.clone());
    wp.save_status = DocumentSaveStatus::Modified;
    wp.editing_field = Some(NumField::Size);
    wp.pending_resource_open = Some((invocation, PathBuf::from("pending.md")));
    wp.resource_file_invocation = Some(invocation);
    assert!(ui::record::capability::release_record_scope(&mut wp).is_err());
    assert!(wp.pending_resource_open.is_some());
    assert_eq!(wp.resource_file_invocation, Some(invocation));
    assert_eq!(wp.path, Some(selected.clone()));
    assert_eq!(wp.save_status, DocumentSaveStatus::Modified);

    wp.editing_field = None;
    wp.pending_replacement = Some(PendingReplacement::Open(PathBuf::from("replacement.md")));
    wp.replace_after_save = true;
    wp.unsaved_changes.open();
    wp.resource_file_result = Some(tilo_actions::InvocationUpdate::Succeeded {
        invocation,
        output: tilo_actions::ActionOutput::default(),
    });
    ui::record::capability::release_record_scope(&mut wp).unwrap();
    assert!(wp.pending_resource_open.is_none());
    assert!(wp.pending_replacement.is_none());
    assert!(!wp.replace_after_save);
    assert!(!wp.unsaved_changes.is_open());
    assert!(wp.resource_file_invocation.is_none());
    assert!(wp.resource_file_result.is_none());
    assert_eq!(wp.path, Some(selected));
    assert_eq!(wp.save_status, DocumentSaveStatus::Modified);
}

#[test]
fn tile_host_preserves_completed_direct_open_result_across_record_intent_release() {
    let directory = std::env::temp_dir().join(format!(
        "tilo-owner-release-{}-{}",
        std::process::id(),
        ui::time::wall()
            .duration_since(ui::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&directory).unwrap();
    let target = directory.join("opened.md");
    std::fs::write(&target, "Opened through the real direct path").unwrap();
    let mut owners = ui::record::RecordOwners::new(directory.join("records"));
    let store = owners.open("word-processor", "owner-release").unwrap();
    let first =
        ui::RecordCheckout::new(ui::RecordId::for_tab(9101), ui::RecordKind::new("document"));
    let selected = ui::RecordCheckout::new(
        ui::RecordId::for_file(&target),
        ui::RecordKind::new("document"),
    );
    store
        .reserve_record(&first)
        .unwrap()
        .initialize(|_| Ok(()))
        .unwrap();
    let mut word_processor = WordProcessorWidget::new();
    word_processor
        .recovery
        .set_base_dir(directory.join("recovery"));
    let mut tile =
        ui::widget::TileHost::new(ui::widget::TileContent::App(Box::new(word_processor)));
    tile.bind_record(store.clone(), first);
    let mut outer = AutoCommit::new();
    tile.sync(&mut outer, &automerge::ROOT);
    store
        .reserve_record(&selected)
        .unwrap()
        .initialize(|doc| {
            let mut candidate = WordProcessorWidget::new();
            candidate
                .recovery
                .set_base_dir(directory.join("candidate-recovery"));
            candidate.sync(doc, &automerge::ROOT);
            Ok(())
        })
        .unwrap();
    let invocation = tilo_actions::InvocationId::new();
    let payload = serde_json::to_vec(&crate::actions::WordProcessorOperation::Open {
        path: Some(target.to_string_lossy().into_owned()),
    })
    .unwrap();
    assert!(matches!(
        tile.start_operation(
            &ui::widget::ResourceOperation::local(
                invocation,
                tilo_actions::OperationKind::Effect,
                crate::actions::WORD_PROCESSOR_OPERATION_TYPE,
                &payload
            ),
            &mut outer,
            &automerge::ROOT
        )
        .unwrap(),
        Some(ui::widget::ResourceOperationStart::Accepted)
    ));
    tile.sync(&mut outer, &automerge::ROOT);
    tile.sync(&mut outer, &automerge::ROOT);
    assert!(tile.record_transition_error().is_none());
    assert!(matches!(
        tile.poll_resource_operation(invocation, false, &outer, &automerge::ROOT),
        Some(tilo_actions::InvocationUpdate::Succeeded { invocation: completed, .. })
            if completed == invocation
    ));
    assert!(
        tile.poll_resource_operation(invocation, false, &outer, &automerge::ROOT)
            .is_none()
    );
    // The records' writer commits in the background: every write lands and
    // the writer stops before its directory goes.
    drop(tile);
    store.flush();
    drop(store);
    drop(owners);
    std::fs::remove_dir_all(directory).unwrap();
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "the controlled Open, refusal, managed Escape event and one-shot result form one lifecycle proof"
)]
fn tile_host_routes_escape_to_old_writer_draft_before_retrying_open_handoff() {
    use winit::keyboard::{NativeKeyCode, PhysicalKey};

    let directory = std::env::temp_dir().join(format!(
        "tilo-owner-recovery-{}-{}",
        std::process::id(),
        ui::time::wall()
            .duration_since(ui::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&directory).unwrap();
    let target = directory.join("opened.md");
    std::fs::write(&target, "Opened after the draft is cancelled").unwrap();
    let mut owners = ui::record::RecordOwners::new(directory.join("records"));
    let store = owners.open("word-processor", "owner-recovery").unwrap();
    let first =
        ui::RecordCheckout::new(ui::RecordId::for_tab(9102), ui::RecordKind::new("document"));
    let selected = ui::RecordCheckout::new(
        ui::RecordId::for_file(&target),
        ui::RecordKind::new("document"),
    );
    store
        .reserve_record(&first)
        .unwrap()
        .initialize(|_| Ok(()))
        .unwrap();
    store
        .reserve_record(&selected)
        .unwrap()
        .initialize(|doc| {
            let mut candidate = WordProcessorWidget::new();
            candidate
                .recovery
                .set_base_dir(directory.join("candidate-recovery"));
            candidate.sync(doc, &automerge::ROOT);
            Ok(())
        })
        .unwrap();

    let mut word_processor = Box::new(WordProcessorWidget::new());
    word_processor
        .recovery
        .set_base_dir(directory.join("recovery"));
    word_processor.lifecycle.focused = true;
    let word_processor_ptr = std::ptr::from_mut(word_processor.as_mut());
    let mut tile = ui::widget::TileHost::new(ui::widget::TileContent::App(word_processor));
    tile.bind_record(store, first);
    let mut state = ui::State::new();
    tile.sync(&mut state.doc, &state.root);

    let invocation = tilo_actions::InvocationId::new();
    let payload = serde_json::to_vec(&crate::actions::WordProcessorOperation::Open {
        path: Some(target.to_string_lossy().into_owned()),
    })
    .unwrap();
    assert!(matches!(
        tile.start_operation(
            &ui::widget::ResourceOperation::local(
                invocation,
                tilo_actions::OperationKind::Effect,
                crate::actions::WORD_PROCESSOR_OPERATION_TYPE,
                &payload
            ),
            &mut state.doc,
            &state.root
        )
        .unwrap(),
        Some(ui::widget::ResourceOperationStart::Accepted)
    ));
    tile.sync(&mut state.doc, &state.root);
    // SAFETY: the boxed Writer allocation remains owned by `tile` for the
    // rest of this test; no reference survives a widget callback.
    unsafe {
        (*word_processor_ptr).editing_field = Some(NumField::Size);
        (*word_processor_ptr).edit_buf = "24".into();
    }
    tile.sync(&mut state.doc, &state.root);
    assert!(tile.record_transition_error().is_some());
    let status = tile
        .accessibility()
        .expect("transition refusal is accessible");
    assert_eq!(status.name.as_deref(), Some("Document switch blocked"));

    let mut tile = Framed::new(state, tile, (1000, 700), ui::Theme::default());

    let mut key: KeyEvent = unsafe { std::mem::zeroed() };
    key.physical_key = PhysicalKey::Unidentified(NativeKeyCode::Unidentified);
    key.logical_key = Key::Named(NamedKey::Escape);
    key.text = None;
    key.location = winit::keyboard::KeyLocation::Standard;
    key.state = ElementState::Pressed;
    key.repeat = false;
    let handled = Framed::harness_mut(&mut tile).dispatch_to_root_with(
        &WindowEvent::KeyboardInput {
            device_id: unsafe { std::mem::zeroed() },
            event: key,
            is_synthetic: false,
        },
        &Modifiers::default(),
    );
    assert!(handled);
    // SAFETY: same stable boxed allocation as above.
    assert!(unsafe { (*word_processor_ptr).editing_field.is_none() });

    let (tile, state) = Framed::split(&mut tile);
    tile.sync(&mut state.doc, &state.root);
    assert!(tile.record_transition_error().is_none());
    assert!(matches!(
        tile.poll_resource_operation(invocation, false, &state.doc, &state.root),
        Some(tilo_actions::InvocationUpdate::Succeeded { invocation: completed, .. })
            if completed == invocation
    ));
    assert!(
        tile.poll_resource_operation(invocation, false, &state.doc, &state.root)
            .is_none()
    );
    std::fs::remove_dir_all(directory).unwrap();
}

#[test]
fn the_version_strip_is_the_only_history_control() {
    let mut state = ui::State::new();
    let mut wp = WordProcessorWidget::new();
    wp.sync(&mut state.doc, &state.root);
    wp.model_mut().insert_str(&mut state.doc, "draft");
    state.commit_action(None, None);
    let mut harness = ui::headless::FrameHarness::new(state, Box::new(wp), (1200, 800));
    harness.enable_document_history_chrome();
    harness.relayout();
    let targets = harness.targets();
    assert!(
        !targets.contains_key("standalone-history-open"),
        "the shared History control must not sit beside the editor's own strip"
    );
    assert!(harness.local_history_resources().unwrap().is_empty());
}
