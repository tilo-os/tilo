//! The caret in the text a field shaped: its geometry through bidi and
//! graphemes, the IME preedit under it, the state it keeps across a round trip,
//! and the frames an unfocused, disabled or bound field paints.
use super::super::*;
use super::body_size;
use super::draw_to_populate;
use super::draw_with_theme;
use super::drive;
use super::middle_press;
use super::mods;
use crate::synthetic_input::{char_key, named_key};
use crate::testing::{Dispatch, dispatch};

#[test]
fn hovering_and_leaving_a_field_do_not_consume_a_siblings_pointer_movement() {
    // Motion reads at its end: this test checks the resting frame.
    let _settled = crate::motion::testing::settled();
    let mut input = TextInput::local();
    input.rect = Rect::new(0.0, 0.0, 200.0, 40.0);
    let mut doc = AutoCommit::new();
    for (x, hovered) in [(100.0, true), (250.0, false)] {
        let moved = crate::synthetic_input::cursor_moved(x, 20.0);
        assert!(!drive(&mut input, &mut doc, &moved, Modifiers::default()));
        assert_eq!(input.is_hovered, hovered);
    }
}

#[test]
fn caret_paints_with_real_line_geometry_and_can_be_suppressed() {
    // Motion reads at its end: this test checks the resting frame.
    let _settled = crate::motion::testing::settled();
    // Case `drag_preview_suppresses_caret_paint_without_blurring_or_clearing_ime`.
    {
        let mut input = TextInput::local().with_initial_value("editable");
        input.set_focus(true);
        input.set_selection(1, 4);
        input.core.set_preedit("composing", None);
        let before = (
            input.cursor_pos(),
            input.selection_range(),
            input.core.preedit().to_owned(),
        );
        let theme = Theme::dark();
        let mut visible = Scene::new();
        input.draw_single_line_caret(&mut visible, Affine::IDENTITY, &theme);
        assert!(!visible.encoding().draw_tags.is_empty());
        input.suppress_caret_paint(true);
        let mut hidden = Scene::new();
        input.draw_single_line_caret(&mut hidden, Affine::IDENTITY, &theme);
        assert!(
            hidden.encoding().draw_tags.is_empty(),
            "drag scenes must contain no local caret"
        );
        assert!(input.is_focused());
        assert_eq!(
            (
                input.cursor_pos(),
                input.selection_range(),
                input.core.preedit().to_owned()
            ),
            before
        );
        input.suppress_caret_paint(false);
        let mut restored = Scene::new();
        input.draw_single_line_caret(&mut restored, Affine::IDENTITY, &theme);
        assert_eq!(
            restored.encoding().draw_tags.len(),
            visible.encoding().draw_tags.len()
        );
    }

    // Case `caret_rect_is_real_caret_geometry`.
    // The peer caret and the local caret are the SAME geometry — full line
    // height, advancing via per-char metrics, NOT a fixed 2px×16px bar —
    // the contract that makes a remote caret pixel-identical, only recoloured.
    {
        let mut ti = TextInput::local().with_initial_value("hello world");
        ti.apply_layout_external(Rect::new(0.0, 0.0, 300.0, 24.0));
        let theme = Theme::dark();
        let presence = crate::presence::PresenceCtx::empty();
        let mut tr = TextRenderer::new();
        let mut scene = Scene::new();
        Widget::draw(
            &mut ti,
            &mut scene,
            Affine::IDENTITY,
            &mut tr,
            &theme,
            &presence,
        );

        let r0 = ti.caret_rect(&theme, 0);
        let r5 = ti.caret_rect(&theme, 5);
        let (_text_y, line_height) = ti.text_band();

        assert!(
            (r0.width() - theme.caret_width).abs() < 0.01,
            "caret uses the configured width"
        );
        assert!(
            (r0.height() - line_height).abs() < 0.01,
            "caret spans the full line band, not a fixed 16px"
        );
        assert!(
            line_height > 16.0,
            "body line height is >16 (so the old fixed 16px was wrong)"
        );
        assert!(
            r5.x0 > r0.x0,
            "caret advances with byte position via real glyph metrics"
        );
    }

    // Case `text_baseline_centred_with_caret`.
    // Pins the text top against `line_height`, *not* `body_size()` itself —
    // the naive computation shifts glyphs ~2-3 px below the caret in a
    // 40 px input.
    {
        let rect_h = 40.0_f32;
        let line_height = body_size() * LINE_HEIGHT_RATIO;
        let text_y_correct = (rect_h - line_height) / 2.0;
        let text_y_naive = (rect_h - body_size()) / 2.0;
        assert!(text_y_naive > text_y_correct);
        assert!((text_y_naive - text_y_correct - (line_height - body_size()) / 2.0).abs() < 0.01);
    }

    // Case `single_line_band_is_the_control_height_and_grows_with_the_scaled_line`.
    // The single-line band is a Regular control (32) at 1×, the height of a
    // button beside it, and grows to line + `spacing.s1` a side once the
    // scaled line no longer fits.
    {
        fn band_height(theme: &Theme) -> Dimension {
            let mut ti = TextInput::new(vec![PathSegment::Field("name".into())]);
            let mut taffy = TaffyTree::<()>::new();
            let mut node_map: std::collections::HashMap<NodeId, *mut dyn Widget> =
                std::collections::HashMap::new();
            let node = ti.build_layout(&mut taffy, theme, &mut node_map);
            taffy.style(node).unwrap().size.height
        }
        let theme = Theme::dark();
        assert_eq!(
            band_height(&theme),
            Dimension::length(32.0),
            "a field is as tall as a Regular button"
        );
        let scaled_line = f64::from(body_size() * LINE_HEIGHT_RATIO) * 2.0;
        let pads = 2.0 * f64::from(theme.tokens.spacing.s1);
        assert!(
            scaled_line + pads > 32.0,
            "fixture: the 2× body line and its pads outgrow the control band"
        );
        #[allow(clippy::cast_possible_truncation, reason = "small test constant")]
        let expected = (scaled_line + pads).ceil() as f32;
        assert_eq!(
            band_height(&Theme::dark().with_text_scale(2.0)),
            Dimension::length(expected),
            "2× band = scaled line + 2×spacing.s1"
        );
    }
}

#[test]
fn caret_respects_graphemes_and_visual_bidi_order() {
    // Motion reads at its end: this test checks the resting frame.
    let _settled = crate::motion::testing::settled();
    // Case `programmatic_caret_never_splits_a_grapheme`.
    {
        let mut combining = TextInput::local().with_initial_value("a\u{301}b");
        combining.set_cursor_pos(1);
        assert_eq!(combining.cursor_pos(), 0);

        let mut emoji = TextInput::local().with_initial_value("👩‍👩‍👧‍👦!");
        emoji.set_cursor_pos(4);
        assert_eq!(emoji.cursor_pos(), 0);
        emoji.set_cursor_pos("👩‍👩‍👧‍👦".len());
        assert_eq!(emoji.cursor_pos(), "👩‍👩‍👧‍👦".len());
    }

    // Case `single_line_bidi_uses_visual_caret_hit_and_selection_geometry`.
    {
        let text = "abc אבג xyz";
        let mut ti = TextInput::local().with_initial_value(text);
        ti.apply_layout_external(Rect::new(0.0, 0.0, 400.0, 40.0));
        draw_to_populate(&mut ti);
        let theme = Theme::dark();

        // Use two unambiguous interior stops. At the LTR/RTL boundaries a
        // byte position legitimately has two visual affinities.
        let after_alef = "abc א".len();
        let after_bet = "abc אב".len();
        let before = ti.caret_rect(&theme, after_alef);
        let after = ti.caret_rect(&theme, after_bet);
        assert!(
            before.x0 > after.x0,
            "the Hebrew run advances right-to-left: before={before:?}, after={after:?}"
        );

        let hit = ti.byte_at_point(Point::new(before.x0 + 1.0, before.center().y));
        assert_eq!(hit, after_alef);

        let selected_end = "abc א".len();
        let bands = ti.selection_rects(&theme, 0, selected_end);
        assert_eq!(bands.len(), 2, "mixed-direction selection stays disjoint");
    }

    // Case `single_line_bidi_caret_retains_affinity_through_arrow_motion`.
    {
        let mut ti = TextInput::local().with_initial_value("abc אבג xyz");
        ti.apply_layout_external(Rect::new(0.0, 0.0, 400.0, 40.0));
        draw_to_populate(&mut ti);
        ti.set_focus(true);
        let theme = Theme::dark();
        let y = ti.rect.center().y;
        let mut positions = Vec::new();
        for x in 0..=400 {
            let position = ti.position_at_point(Point::new(f64::from(x), y));
            if positions.last() != Some(&position) {
                positions.push(position);
            }
        }
        let (a, b) = positions
            .iter()
            .find_map(|a| {
                let ax = ti.caret_rect_at(&theme, *a).x0;
                positions
                    .iter()
                    .find(|b| {
                        a.byte == b.byte
                            && a.affinity != b.affinity
                            && (ax - ti.caret_rect_at(&theme, **b).x0).abs() > 1.0
                    })
                    .map(|b| (*a, *b))
            })
            .expect("the mixed-direction boundary has two visual positions");
        let (left, right) = if ti.caret_rect_at(&theme, a).x0 < ti.caret_rect_at(&theme, b).x0 {
            (a, b)
        } else {
            (b, a)
        };

        ti.set_caret_position(left);
        let mut doc = AutoCommit::new();
        for _ in 0..16 {
            if ti.caret_position() == right {
                break;
            }
            drive(
                &mut ti,
                &mut doc,
                &named_key(NamedKey::ArrowRight),
                mods(false, false),
            );
        }
        assert_eq!(ti.caret_position(), right);
        assert_eq!(left.byte, right.byte);
        assert_ne!(left.affinity, right.affinity);
    }

    // Case `rtl_arrow_keys_follow_visual_not_storage_direction`.
    {
        let text = "שלום";
        let mut ti = TextInput::local().with_initial_value(text);
        ti.apply_layout_external(Rect::new(0.0, 0.0, 300.0, 40.0));
        draw_to_populate(&mut ti);
        ti.set_focus(true);
        ti.set_cursor_pos(0); // visual right edge
        let mut doc = AutoCommit::new();

        drive(
            &mut ti,
            &mut doc,
            &named_key(NamedKey::ArrowLeft),
            mods(false, false),
        );
        assert_eq!(
            ti.cursor_pos(),
            text.chars().next().unwrap().len_utf8(),
            "visual Left advances logically through an RTL run"
        );
        drive(
            &mut ti,
            &mut doc,
            &named_key(NamedKey::ArrowRight),
            mods(false, false),
        );
        assert_eq!(ti.cursor_pos(), 0, "visual Right returns to the RTL start");

        ti.set_cursor_pos(text.len()); // visual left edge
        drive(
            &mut ti,
            &mut doc,
            &named_key(NamedKey::ArrowRight),
            mods(false, false),
        );
        assert!(ti.cursor_pos() < text.len());
    }
}

#[test]
fn widget_metadata_reports_id_ime_content_cursor_and_scrim() {
    // Motion reads at its end: this test checks the resting frame.
    let _settled = crate::motion::testing::settled();
    // Case `permission_denied_overlay_alpha_matches_other_widgets`.
    {
        let expected = crate::tokens::colors::PERMISSION_DENIED_SCRIM;
        assert_eq!(TextInput::PERMISSION_DENIED_OVERLAY, expected);
    }

    // Case `text_input_test_id_default_is_text_input`.
    {
        let ti = TextInput::new(Vec::new());
        assert_eq!(Widget::test_id(&ti), Some("text_input"));
    }

    // Case `text_input_test_id_can_be_overridden`.
    {
        let ti = TextInput::new(Vec::new()).with_id("search_box");
        assert_eq!(Widget::test_id(&ti), Some("search_box"));
    }

    // Case `ime_content_does_not_leak_secret_field_contents`.
    {
        let plain = TextInput::local();
        let secret = TextInput::local().with_secret(true);
        assert_eq!(
            Widget::ime_content(&plain),
            Some(crate::ime::InputContent::TEXT)
        );
        assert_eq!(
            Widget::ime_content(&secret),
            Some(crate::ime::InputContent::PASSWORD)
        );
    }

    // Case `cursor_shape_is_text_over_rect`.
    {
        let mut ti = TextInput::local();
        ti.apply_layout_external(Rect::new(0.0, 0.0, 200.0, 40.0));
        assert_eq!(
            Widget::cursor_shape(&ti, Point::new(20.0, 20.0)),
            Some(winit::window::CursorIcon::Text),
            "pointer over the editable area requests an I-beam",
        );
        assert_eq!(
            Widget::cursor_shape(&ti, Point::new(500.0, 500.0)),
            None,
            "pointer outside the input defers (no cursor claim)",
        );

        let mut off = TextInput::local().disabled(true);
        off.apply_layout_external(Rect::new(0.0, 0.0, 200.0, 40.0));
        assert_eq!(
            Widget::cursor_shape(&off, Point::new(20.0, 20.0)),
            None,
            "disabled input claims no cursor",
        );
    }
}

#[test]
fn caret_and_selection_survive_widget_state_round_trip() {
    // Motion reads at its end: this test checks the resting frame.
    let _settled = crate::motion::testing::settled();
    // Case `selection_survives_widget_state_round_trip_and_legacy_state_still_loads`.
    {
        let mut original = TextInput::local().with_initial_value("hello world");
        original.core.set_selection(1, 5);
        let saved = crate::undo::get_state(&original).unwrap();
        let mut rebuilt = TextInput::local().with_initial_value("hello world");
        crate::undo::set_state(&mut rebuilt, &saved);
        assert_eq!(rebuilt.selection_range_public(), Some((1, 5)));
        crate::undo::set_state(&mut rebuilt, "3");
        assert_eq!(rebuilt.cursor_pos(), 3);
        assert_eq!(rebuilt.selection_range_public(), None);
        crate::undo::set_state(&mut rebuilt, "4:a");
        assert_eq!(
            rebuilt.caret_position(),
            TextPosition::with_affinity(4, TextAffinity::After)
        );
    }

    // Case `caret_affinity_survives_widget_state_round_trip`.
    {
        let mut original = TextInput::local().with_initial_value("abc אבג");
        let position = TextPosition::with_affinity(4, TextAffinity::After);
        original.set_caret_position(position);
        let saved = crate::undo::get_state(&original).unwrap();

        let mut rebuilt = TextInput::local().with_initial_value("abc אבג");
        crate::undo::set_state(&mut rebuilt, &saved);
        assert_eq!(rebuilt.caret_position(), position);
        assert_eq!(saved, "4:a");
    }
}

#[test]
fn highlighted_code_shares_bidi_geometry_and_scaled_gutter() {
    // Motion reads at its end: this test checks the resting frame.
    let _settled = crate::motion::testing::settled();
    // Case `highlighted_code_uses_shared_bidi_geometry_through_the_gutter`.
    {
        let text = "let אבג = 1;\nprintln!(\"ok\");";
        let mut ti = TextInput::local()
            .multiline(true)
            .with_mono()
            .with_gutter(true)
            .with_current_line_highlight(true)
            .with_initial_value(text);
        ti.set_highlighter(crate::widget::code_editor::Language::Rust.highlighter());
        ti.set_focus(true);
        ti.apply_layout_external(Rect::new(0.0, 0.0, 420.0, 140.0));
        draw_to_populate(&mut ti);

        assert!(ti.multiline_shaped);
        assert!(
            ti.lines.is_empty(),
            "code no longer builds the approximate VisualLine geometry"
        );
        let after_alef = "let א".len();
        let after_bet = "let אב".len();
        let theme = Theme::dark();
        let alef = ti.caret_rect(&theme, after_alef);
        let bet = ti.caret_rect(&theme, after_bet);
        assert!(
            alef.x0 > bet.x0,
            "the highlighted Hebrew identifier advances visually right-to-left"
        );
        assert!(alef.x0 > ti.rect.x0 + ti.gutter_width());
        assert_eq!(
            ti.byte_at_point(Point::new(alef.x0 + 1.0, alef.center().y)),
            after_alef
        );
    }

    // Case `code_gutter_reserves_space_for_scaled_line_numbers`.
    {
        let source = (1..=12)
            .map(|line| format!("line {line}"))
            .collect::<Vec<_>>()
            .join("\n");
        let mut normal = TextInput::local()
            .multiline(true)
            .with_mono()
            .with_gutter(true)
            .with_initial_value(&source);
        normal.apply_layout_external(Rect::new(0.0, 0.0, 420.0, 300.0));
        draw_with_theme(&mut normal, &Theme::dark());
        let normal_gutter = normal.gutter_width();

        let mut large = TextInput::local()
            .multiline(true)
            .with_mono()
            .with_gutter(true)
            .with_initial_value(&source);
        large.apply_layout_external(Rect::new(0.0, 0.0, 420.0, 300.0));
        let large_theme = Theme::dark().with_text_scale(2.0);
        draw_with_theme(&mut large, &large_theme);

        assert!(
            large.gutter_width() >= normal_gutter * 1.9,
            "gutter follows line-number text scale ({} vs {normal_gutter})",
            large.gutter_width()
        );
        assert!(
            large.caret_rect(&large_theme, 0).x0 > large.rect.x0 + large.gutter_width(),
            "the first code glyph stays to the right of the scaled gutter"
        );
    }

    // Case `code_gutter_numbers_do_not_apply_accessibility_scale_twice`.
    {
        let paragraph_style = TextStyle::new(26.0).with_letter_spacing(0.4);
        let number_style =
            TextInput::gutter_number_style(&paragraph_style, &Theme::dark().with_text_scale(2.0));
        assert_eq!(number_style.size, 13.0);
        assert_eq!(number_style.letter_spacing, 0.2);
    }
}

#[test]
fn a_collapsed_anchor_does_not_select_the_text_inserted_after_it() {
    // Motion reads at its end: this test checks the resting frame.
    let _settled = crate::motion::testing::settled();
    // A tap leaves the anchor where the caret was; the soft keyboard then
    // commits a word and, separately, the space after it.
    let mut ti = TextInput::local();
    ti.set_focus(true);
    ti.apply_layout_external(Rect::new(0.0, 0.0, 300.0, 40.0));
    ti.core.set_selection(0, 0);
    let mut doc = AutoCommit::new();
    for text in ["Hello", " ", "world"] {
        assert!(drive(
            &mut ti,
            &mut doc,
            &WindowEvent::Ime(winit::event::Ime::Commit(text.into())),
            mods(false, false),
        ));
    }
    assert_eq!(ti.value(), "Hello world");
}

#[test]
fn ime_preedit_is_shaped_underlined_and_committed_without_touching_the_document() {
    // Motion reads at its end: this test checks the resting frame.
    let _settled = crate::motion::testing::settled();
    // Case `ime_preedit_is_shaped_underlined_and_committed_atomically`.
    {
        let mut ti = TextInput::local().with_initial_value("ab");
        ti.set_cursor_pos(1);
        ti.set_focus(true);
        ti.apply_layout_external(Rect::new(0.0, 0.0, 300.0, 40.0));
        let mut doc = AutoCommit::new();

        assert!(drive(
            &mut ti,
            &mut doc,
            &WindowEvent::Ime(winit::event::Ime::Preedit(
                "漢".into(),
                Some(("漢".len(), "漢".len())),
            )),
            mods(false, false),
        ));
        assert_eq!(ti.value(), "ab", "preedit never mutates the document");

        draw_to_populate(&mut ti);
        let layout = ti.single_layout.as_ref().expect("preedit was shaped");
        assert_eq!(layout.underlines, vec![(1, 1 + "漢".len())]);
        let ime_rect = Widget::ime_cursor_rect(&ti).expect("focused input enables IME");
        let committed_caret = ti.caret_rect(&Theme::dark(), 1);
        assert!(
            ime_rect.x0 > committed_caret.x0,
            "candidate anchor follows the in-preedit caret"
        );

        assert!(drive(
            &mut ti,
            &mut doc,
            &WindowEvent::Ime(winit::event::Ime::Commit("漢".into())),
            mods(false, false),
        ));
        assert_eq!(ti.value(), "a漢b");
        assert!(!ti.core.has_preedit());

        ti.set_focus(false);
        assert!(Widget::ime_cursor_rect(&ti).is_none());
    }

    // Case `highlighted_code_shapes_visible_preedit_and_recolors_with_theme`.
    {
        let mut ti = TextInput::local()
            .multiline(true)
            .with_mono()
            .with_gutter(true)
            .with_initial_value("let value = 1;");
        ti.set_highlighter(crate::widget::code_editor::Language::Rust.highlighter());
        ti.set_cursor_pos(4);
        ti.set_focus(true);
        ti.apply_layout_external(Rect::new(0.0, 0.0, 420.0, 120.0));
        let mut doc = AutoCommit::new();
        assert!(drive(
            &mut ti,
            &mut doc,
            &WindowEvent::Ime(winit::event::Ime::Preedit(
                "漢".into(),
                Some(("漢".len(), "漢".len())),
            )),
            mods(false, false),
        ));

        draw_to_populate(&mut ti);
        assert_eq!(ti.value(), "let value = 1;");
        assert_eq!(
            ti.single_layout.as_ref().unwrap().underlines,
            vec![(4, 4 + "漢".len())],
            "preedit is part of the attributed display layout"
        );
        assert!(Widget::ime_cursor_rect(&ti).is_some());

        ti.core.clear_preedit();
        draw_with_theme(&mut ti, &Theme::dark());
        let dark_signature = ti.shaped_layout_signature;
        draw_with_theme(&mut ti, &Theme::light());
        assert_eq!(
            ti.shaped_layout_signature, dark_signature,
            "syntax colours paint with the rows, so another theme keeps the shaped layout"
        );
    }
}

#[test]
fn shaped_layout_is_cached_until_display_inputs_or_fonts_change() {
    // Motion reads at its end: this test checks the resting frame.
    let _settled = crate::motion::testing::settled();
    // Case `shaped_paragraph_is_reused_until_its_display_inputs_change`.
    {
        let mut ti = TextInput::local().with_initial_value("office שלום");
        ti.apply_layout_external(Rect::new(0.0, 0.0, 400.0, 40.0));
        draw_to_populate(&mut ti);
        let initial_signature = ti.shaped_layout_signature;
        let initial_layout = std::ptr::from_ref(ti.single_layout.as_ref().unwrap());

        ti.set_cursor_pos(1);
        draw_to_populate(&mut ti);
        assert_eq!(ti.shaped_layout_signature, initial_signature);
        assert_eq!(
            std::ptr::from_ref(ti.single_layout.as_ref().unwrap()),
            initial_layout,
            "caret-only movement reuses the shaped paragraph"
        );

        ti.core.set_preedit("漢", Some(("漢".len(), "漢".len())));
        draw_to_populate(&mut ti);
        assert_ne!(
            ti.shaped_layout_signature, initial_signature,
            "preedit changes the displayed paragraph and invalidates shaping"
        );
    }

    // Case `multiline_reuse_refreshes_unchanged_paragraphs_after_font_generation`.
    {
        let mut input = TextInput::local()
            .multiline(true)
            .with_initial_value("first\nWWW iii unchanged");
        let mut tr = TextRenderer::new();
        tr.font_system = crate::font_catalog::fixture_font_system();
        let theme = Theme::dark();
        let style = input.multiline_style(theme.text_scale);
        input.ensure_shaped_layout(&mut tr, &theme, &style, 300.0);
        let before = input
            .single_layout
            .as_ref()
            .unwrap()
            .caret_geometry(input.text.len());
        tr.font_system
            .db_mut()
            .set_sans_serif_family(crate::text_render::UI_MONOSPACE_FAMILY);
        tr.bump_generation();
        // No edit must also refresh cached geometry when fonts change.
        input.ensure_shaped_layout(&mut tr, &theme, &style, 300.0);
        let fresh = RichTextLayout::shape_styled(
            &mut tr.font_system,
            &input.highlighted_display_spans(&theme),
            &style,
            Some(300.0),
            TextAlign::Left,
            vec![],
            vec![],
        );
        let after = input
            .single_layout
            .as_ref()
            .unwrap()
            .caret_geometry(input.text.len());
        assert_ne!(
            before,
            fresh.caret_geometry(input.text.len()),
            "fixture fonts must differ"
        );
        assert_eq!(after, fresh.caret_geometry(input.text.len()));
        input.text.insert(0, 'x');
        input.ensure_shaped_layout(&mut tr, &theme, &style, 300.0);
        let fresh = RichTextLayout::shape_styled(
            &mut tr.font_system,
            &input.highlighted_display_spans(&theme),
            &style,
            Some(300.0),
            TextAlign::Left,
            vec![],
            vec![],
        );
        assert_eq!(
            input
                .single_layout
                .as_ref()
                .unwrap()
                .caret_geometry(input.text.len()),
            fresh.caret_geometry(input.text.len())
        );
    }

    // Case `font_family_override_shapes_in_named_family`.
    // The authoritative input layout resolves its ASCII glyphs from the
    // requested named family rather than silently reverting to sans.
    {
        let mut tr = crate::font_catalog::fixture_renderer();
        let fam = crate::tokens::ReadingFace::Literata.family().to_owned();
        assert!(tr.system_families().contains(&fam));

        let mut ti = TextInput::local()
            .with_initial_value("Reading")
            .with_font_family(FontChoice::Named(fam.clone()));
        ti.apply_layout_external(Rect::new(0.0, 0.0, 300.0, 40.0));
        let theme = Theme::dark();
        let presence = crate::presence::PresenceCtx::empty();
        let mut scene = Scene::new();
        Widget::draw(
            &mut ti,
            &mut scene,
            Affine::IDENTITY,
            &mut tr,
            &theme,
            &presence,
        );

        let layout = ti
            .single_layout
            .as_ref()
            .expect("input retained its layout");
        let face_ids: Vec<_> = layout
            .buffer
            .layout_runs()
            .flat_map(|run| run.glyphs.iter().map(|glyph| glyph.font_id))
            .collect();
        assert!(!face_ids.is_empty());
        assert!(face_ids.iter().all(|id| {
            tr.font_system.db().face(*id).is_some_and(|face| {
                face.families
                    .iter()
                    .any(|(family, _language)| family == &fam)
            })
        }));
    }
}

#[test]
fn value_bindings_publish_user_edits_but_not_model_updates() {
    // Motion reads at its end: this test checks the resting frame.
    let _settled = crate::motion::testing::settled();
    // Case `on_change_reports_committed_user_edits_once`.
    {
        let changes = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
        let captured = std::rc::Rc::clone(&changes);
        let mut ti = TextInput::local()
            .with_initial_value("a")
            .on_change(move |value| captured.borrow_mut().push(value.to_string()));
        ti.set_focus(true);
        let mut doc = AutoCommit::new();

        assert!(drive(&mut ti, &mut doc, &char_key("b"), mods(false, false),));
        assert_eq!(&*changes.borrow(), &["ab"]);

        // A non-edit event must not repeat the prior notification even though
        // the legacy dirty flag remains set until its owner drains it.
        drive(
            &mut ti,
            &mut doc,
            &named_key(NamedKey::ArrowLeft),
            mods(false, false),
        );
        assert_eq!(&*changes.borrow(), &["ab"]);

        assert!(drive(
            &mut ti,
            &mut doc,
            &WindowEvent::Ime(winit::event::Ime::Preedit(
                "漢".into(),
                Some(("漢".len(), "漢".len())),
            )),
            mods(false, false),
        ));
        assert_eq!(&*changes.borrow(), &["ab"]);

        assert!(drive(
            &mut ti,
            &mut doc,
            &WindowEvent::Ime(winit::event::Ime::Commit("漢".into())),
            mods(false, false),
        ));
        assert_eq!(&*changes.borrow(), &["ab", "a漢b"]);

        ti.set_text("programmatic");
        assert_eq!(&*changes.borrow(), &["ab", "a漢b"]);
        ti.insert_at_cursor(" helper");
        drive(
            &mut ti,
            &mut doc,
            &named_key(NamedKey::ArrowLeft),
            mods(false, false),
        );
        assert_eq!(&*changes.borrow(), &["ab", "a漢b"]);
    }

    // Case `local_state_binding_is_two_way_across_multiple_inputs`.
    {
        let state = crate::LocalState::new(String::from("Ada"));
        let mut first = TextInput::from_state(&state);
        let mut second = TextInput::from_state(&state);
        let mut document = AutoCommit::new();

        state.set(String::from("Grace"));
        Widget::sync(&mut first, &document, &automerge::ROOT);
        Widget::sync(&mut second, &document, &automerge::ROOT);
        assert_eq!(first.value(), "Grace");
        assert_eq!(second.value(), "Grace");
        assert_eq!(first.take_dirty(), None, "model updates are not user edits");

        assert!(Widget::perform_action(
            &mut first,
            &crate::a11y::Action::SetValue("Lin".into()),
            &[],
            &mut crate::widget::Edits::lend(&mut document),
            &automerge::ROOT,
        ));
        assert_eq!(state.get(), "Lin", "user edits publish typed state");

        Widget::sync(&mut second, &document, &automerge::ROOT);
        assert_eq!(second.value(), "Lin", "sibling input follows the model");
        assert_eq!(second.take_dirty(), None);

        first.set_text("programmatic");
        assert_eq!(state.get(), "programmatic");
        Widget::sync(&mut second, &document, &automerge::ROOT);
        assert_eq!(second.value(), "programmatic");
        assert_eq!(first.take_dirty(), None);
    }

    // Case `local_state_binding_coexists_with_an_app_callback`.
    {
        let state = crate::LocalState::new(String::from("before"));
        let changes = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
        let captured = std::rc::Rc::clone(&changes);
        let mut input = TextInput::from_state(&state)
            .on_change(move |value| captured.borrow_mut().push(value.to_owned()));
        let mut document = AutoCommit::new();

        assert!(Widget::perform_action(
            &mut input,
            &crate::a11y::Action::SetValue("after".into()),
            &[],
            &mut crate::widget::Edits::lend(&mut document),
            &automerge::ROOT,
        ));
        assert_eq!(state.get(), "after");
        assert_eq!(&*changes.borrow(), &["after"]);
    }

    // Case `local_state_model_update_invalidates_multiline_measurement_only`.
    {
        let state = crate::LocalState::new(String::from("short"));
        let mut input = TextInput::from_state(&state).multiline(true);
        input.apply_layout_external(Rect::new(0.0, 0.0, 240.0, 80.0));
        assert!(!Widget::layout_needs_measure(&input));

        state.set(String::from("a much longer line\nwith another row"));
        let document = AutoCommit::new();
        Widget::sync(&mut input, &document, &automerge::ROOT);

        assert!(Widget::layout_needs_measure(&input));
        assert_eq!(input.take_dirty(), None, "model sync is not a user edit");
    }
}

#[test]
fn unfocused_input_shows_start_and_placeholder() {
    // Motion reads at its end: this test checks the resting frame.
    let _settled = crate::motion::testing::settled();
    // Case `unfocused_input_renders_from_start_not_caret`.
    // Otherwise caret-following scroll hides the first characters.
    {
        let long = "a very long line that easily exceeds a narrow input width so caret-following scroll would otherwise hide the beginning";
        let mut ti = TextInput::local().with_initial_value(long);
        ti.apply_layout_external(Rect::new(0.0, 0.0, 120.0, 24.0)); // narrow
        ti.set_cursor_pos(long.len()); // caret at the very end
        let theme = Theme::dark();
        let presence = crate::presence::PresenceCtx::empty();
        let mut tr = TextRenderer::new();
        let mut scene = Scene::new();
        Widget::draw(
            &mut ti,
            &mut scene,
            Affine::IDENTITY,
            &mut tr,
            &theme,
            &presence,
        );
        assert_eq!(
            ti.scroll_x(),
            0.0,
            "unfocused input must not scroll the start off-screen"
        );

        ti.set_focus(true);
        Widget::draw(
            &mut ti,
            &mut scene,
            Affine::IDENTITY,
            &mut tr,
            &theme,
            &presence,
        );
        assert!(
            ti.scroll_x() > 0.0,
            "focused input follows the caret on a long line"
        );
    }

    // Case `placeholder_visible_while_empty_focused_or_not`.
    {
        let mut ti = TextInput::local().with_placeholder("Search…");
        assert_eq!(ti.placeholder(), "Search…");
        assert!(
            ti.shows_placeholder(),
            "empty unfocused input shows placeholder"
        );

        ti.set_focus(true);
        assert!(
            ti.shows_placeholder(),
            "focusing an empty input keeps the placeholder"
        );

        ti.set_text("h");
        assert!(
            !ti.shows_placeholder(),
            "the first character replaces the placeholder"
        );

        ti.set_focus(false);
        ti.set_text("hello");
        assert!(!ti.shows_placeholder(), "non-empty input hides placeholder");

        ti.set_text("");
        assert!(ti.shows_placeholder());

        let plain = TextInput::local();
        assert!(
            !plain.shows_placeholder(),
            "no placeholder set → never shows"
        );
    }

    // Case `placeholder_emits_glyphs_when_shown`.
    // Checks that the dim hint paints.
    {
        let theme = Theme::dark();
        let mut tr = TextRenderer::new();
        let presence = crate::presence::PresenceCtx::empty();

        let mut shown = TextInput::local().with_placeholder("Type here");
        shown.apply_layout_external(Rect::new(0.0, 0.0, 200.0, 40.0));
        let mut scene = Scene::new();
        Widget::draw(
            &mut shown,
            &mut scene,
            Affine::IDENTITY,
            &mut tr,
            &theme,
            &presence,
        );
        assert!(
            !scene.encoding().resources.glyph_runs.is_empty(),
            "placeholder must paint a glyph run when shown",
        );

        let mut focused = TextInput::local().with_placeholder("Type here");
        focused.set_focus(true);
        focused.apply_layout_external(Rect::new(0.0, 0.0, 200.0, 40.0));
        let mut scene_focused = Scene::new();
        Widget::draw(
            &mut focused,
            &mut scene_focused,
            Affine::IDENTITY,
            &mut tr,
            &theme,
            &presence,
        );
        assert!(
            !scene_focused.encoding().resources.glyph_runs.is_empty(),
            "a focused empty input still paints the placeholder",
        );
    }

    // Case `an_unfocused_field_the_wheel_scrolled_keeps_its_scroll`.
    {
        for shaped in [false, true] {
            let text = "a long source row\n".repeat(80);
            let mut input = TextInput::local().multiline(true).with_initial_value(&text);
            input.rect = Rect::new(0., 0., 300., 100.);
            input.is_focused = false;
            input.is_hovered = true;
            input.lines = (0..80)
                .map(|i| VisualLine {
                    start: i * 18,
                    end: i * 18 + 17,
                    hard_break: true,
                    y: i as f64 * 20.,
                    char_x: vec![],
                })
                .collect();
            let mut handled = false;
            input.handle_mouse_wheel(
                &winit::event::MouseScrollDelta::LineDelta(0.0, -2.0),
                winit::event::TouchPhase::Moved,
                1.0,
                &mut handled,
            );
            let scrolled = input.scroll_y;
            assert!(
                handled && scrolled > 0.0,
                "the wheel scrolls a hovered field"
            );
            if shaped {
                input.update_ml_scroll(0., 20., 1600.);
            } else {
                input.update_ml_body_scroll(20.);
            }
            assert_eq!(input.scroll_y, scrolled, "the next frame keeps the scroll");
            input.set_focus(true);
            input.set_focus(false);
            if shaped {
                input.update_ml_scroll(0., 20., 1600.);
            } else {
                input.update_ml_body_scroll(20.);
            }
            assert_eq!(input.scroll_y, 0.0, "a field left again shows its start");
        }
    }

    // Case `snapshot_scroll_opt_in_preserves_unfocused_shaped_and_body_views`.
    {
        for shaped in [false, true] {
            for preserve in [false, true] {
                let text = "a long source row\n".repeat(80);
                let mut input = TextInput::local().multiline(true).with_initial_value(&text);
                input.set_preserve_unfocused_scroll(preserve);
                input.rect = Rect::new(0., 0., 300., 100.);
                input.is_focused = false;
                if !shaped {
                    input.lines = (0..80)
                        .map(|i| VisualLine {
                            start: i * 18,
                            end: i * 18 + 17,
                            hard_break: true,
                            y: i as f64 * 20.,
                            char_x: vec![],
                        })
                        .collect();
                }
                input.scroll_y = 60.;
                if shaped {
                    input.update_ml_scroll(0., 20., 1600.);
                } else {
                    input.update_ml_body_scroll(20.);
                }
                assert_eq!(input.scroll_y, if preserve { 60. } else { 0. });
                input.rect = Rect::new(0., 0., 400., 140.);
                if shaped {
                    input.update_ml_scroll(0., 20., 1600.);
                } else {
                    input.update_ml_body_scroll(20.);
                }
                assert_eq!(input.scroll_y, if preserve { 60. } else { 0. });
                assert!(!input.is_focused);
            }
        }
    }
}

#[test]
fn disabled_input_is_inert_to_focus_keys_pointer_and_accessibility() {
    // Motion reads at its end: this test checks the resting frame.
    let _settled = crate::motion::testing::settled();
    // Case `disabled_input_is_not_focusable`.
    {
        let enabled = TextInput::new(Vec::new());
        assert!(Widget::is_focusable(&enabled), "default input is focusable");

        let off = TextInput::new(Vec::new()).disabled(true);
        assert!(
            !Widget::is_focusable(&off),
            "disabled input must not be focusable"
        );

        let mut later = TextInput::new(Vec::new());
        later.set_disabled(true);
        assert!(!Widget::is_focusable(&later));
        assert!(later.is_disabled());
    }

    // Case `disabled_input_ignores_keyboard_input`.
    {
        let mut ti = TextInput::local().disabled(true);
        ti.set_focus(true); // even if somehow focused, the input is inert
        ti.apply_layout_external(Rect::new(0.0, 0.0, 200.0, 40.0));

        let event = char_key("x");
        let handled = dispatch(&mut ti, &event);
        assert!(!handled, "disabled input must not consume the keypress");
        assert_eq!(ti.value(), "", "disabled input must not insert text");
        assert!(ti.take_dirty().is_none(), "no edit should be recorded");

        let mut on = TextInput::local();
        on.set_focus(true);
        on.apply_layout_external(Rect::new(0.0, 0.0, 200.0, 40.0));
        let handled = dispatch(&mut on, &char_key("x"));
        assert!(handled);
        assert_eq!(on.value(), "x");
    }

    // Case `disabled_input_ignores_pointer_input`.
    {
        let mut ti = TextInput::local().disabled(true);
        ti.apply_layout_external(Rect::new(0.0, 0.0, 200.0, 40.0));

        let moved = crate::synthetic_input::cursor_moved(20.0, 20.0);
        let moved = Dispatch::new().send(&mut ti, &moved);
        let press = crate::synthetic_input::mouse_press();
        let press = Dispatch::new().send(&mut ti, &press);
        let handled = moved.handled | press.handled;
        let focus_target = press.focus_target.or(moved.focus_target);

        assert!(!handled, "disabled input must not consume the click");
        assert!(
            focus_target.is_none(),
            "disabled input must not request focus"
        );
    }

    // Case `disabled_input_ignores_middle_click`.
    {
        let mut ti = TextInput::local()
            .with_initial_value("unchanged")
            .disabled(true);
        ti.apply_layout_external(Rect::new(0.0, 0.0, 300.0, 40.0));
        let mut doc = AutoCommit::new();
        assert!(!drive(
            &mut ti,
            &mut doc,
            &middle_press(),
            mods(false, false)
        ));
        assert_eq!(ti.value(), "unchanged");
    }

    // Case `accessibility_disabled_input_reports_disabled_and_not_focusable`.
    {
        let ti = TextInput::new(Vec::new()).disabled(true);
        let node = Widget::accessibility(&ti).unwrap();
        assert!(node.state.disabled);
        assert!(
            !node.state.focusable,
            "disabled input is dropped from focus order"
        );
    }
}
