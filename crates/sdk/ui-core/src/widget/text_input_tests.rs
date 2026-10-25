use super::*;
use crate::testing::{Dispatch, dispatch};
use crate::text_edit::SUGGESTION_SETTLE;
use crate::text_selection::{next_word_boundary, prev_word_boundary};
use automerge::ObjType;
use automerge::transaction::Transactable as _;

/// The 1× size a role-less field sets its text at.
fn body_size() -> f32 {
    crate::tokens::typography::stock().size(TextRole::Body)
}

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

use crate::synthetic_input::char_key;

/// Sends `CursorMoved` first so `last_mouse_pos`/`is_hovered` are set, matching real input order.
#[cfg(test)]
fn right_click(ti: &mut TextInput, doc: &mut AutoCommit, x: f64, y: f64) -> bool {
    let mut sender = Dispatch::new().doc(doc);
    let moved = crate::synthetic_input::cursor_moved(x, y);
    let press = crate::synthetic_input::mouse_input(ElementState::Pressed, MouseButton::Right);
    sender.send(ti, &moved).handled | sender.send(ti, &press).handled
}

#[test]
fn read_only_context_menu_copies_without_offering_or_applying_edits() {
    // Motion reads at its end: this test checks the resting frame.
    let _settled = crate::motion::testing::settled();
    let services = crate::host::HostServices::null();
    let _host = crate::host::enter(&services);
    crate::text_selection::clipboard_copy("clipboard");
    let mut input = TextInput::local().with_initial_value("source");
    input.apply_layout_external(Rect::new(0.0, 0.0, 300.0, 40.0));
    input.select_all();
    input.open_text_menu_at([20.0, 20.0]);
    let cut = input
        .context_actions
        .iter()
        .position(|action| matches!(action, Some(CtxAction::Cut)))
        .unwrap();
    input.set_read_only(true);
    assert!(
        !input.dispatch_context_action(cut),
        "a menu opened before read-only cannot edit"
    );
    assert_eq!(input.value(), "source");
    assert_eq!(
        crate::text_selection::clipboard_paste().as_deref(),
        Some("clipboard")
    );
    input.context_menu.close();

    let mut doc = AutoCommit::new();
    drive(
        &mut input,
        &mut doc,
        &cursor_moved(20.0, 20.0),
        mods(false, false),
    );
    drive(
        &mut input,
        &mut doc,
        &crate::synthetic_input::mouse_input(ElementState::Pressed, MouseButton::Right),
        mods(false, false),
    );
    assert!(input.context_menu.is_open());
    let labels = input.context_menu_labels();
    assert!(labels.iter().any(|label| label == "Copy"));
    assert!(
        !labels
            .iter()
            .any(|label| label == "Cut" || label == "Paste")
    );
    let copy = input
        .context_actions
        .iter()
        .position(|action| matches!(action, Some(CtxAction::Copy)))
        .unwrap();
    input.dispatch_context_action(copy);
    assert_eq!(
        crate::text_selection::clipboard_paste().as_deref(),
        Some("source")
    );
    assert_eq!(input.value(), "source");
}

#[test]
fn context_menu_opens_on_right_click_dispatches_and_dismisses() {
    // Motion reads at its end: this test checks the resting frame.
    let _settled = crate::motion::testing::settled();
    // Case `right_click_selects_word_and_opens_menu_with_expected_labels`.
    {
        let mut ti = TextInput::local().with_initial_value("hello world");
        ti.apply_layout_external(Rect::new(0.0, 0.0, 300.0, 40.0));
        ti.set_focus(true);
        let mut doc = AutoCommit::new();

        assert!(ti.selection_range().is_none(), "precondition: no selection");
        assert!(
            right_click(&mut ti, &mut doc, 20.0, 20.0),
            "right-click is consumed"
        );

        // No char_x_positions yet → cursor_from_x resolves to byte 0, selecting "hello".
        assert_eq!(ti.selected_str(), Some("hello"), "clicked word is selected");
        assert!(ti.context_menu.is_open(), "menu opened");

        let labels = ti.context_menu_labels();
        assert!(labels.contains(&"Cut".to_string()), "got {labels:?}");
        assert!(labels.contains(&"Copy".to_string()), "got {labels:?}");
        assert!(labels.contains(&"Select All".to_string()), "got {labels:?}");
    }

    // Case `context_menu_dispatch_maps_index_to_copy`.
    {
        let mut ti = TextInput::local().with_initial_value("hello world");
        ti.apply_layout_external(Rect::new(0.0, 0.0, 300.0, 40.0));
        // Pre-select "world" so Cut/Copy are present and the selection survives a copy.
        ti.core.set_selection(6, 11);

        ti.open_context_menu([50.0, 18.0]);
        let labels = ti.context_menu_labels();
        let copy_idx = labels
            .iter()
            .position(|l| l == "Copy")
            .expect("Copy row present");
        // Labels skip separators, but Cut/Copy precede any separator so indices align here.
        let item_idx = ti
            .context_menu
            .items()
            .iter()
            .position(|i| i.label() == Some("Copy"))
            .unwrap();
        assert_eq!(
            ti.context_actions[item_idx],
            Some(CtxAction::Copy),
            "Copy row maps to the Copy action (label idx {copy_idx})"
        );

        assert!(ti.dispatch_context_action(item_idx), "dispatch ran");
        assert!(!ti.context_menu.is_open(), "menu closed after dispatch");
        // Copy preserves the selection (it's a read-only action).
        assert_eq!(ti.selected_str(), Some("world"));
    }

    // Case `context_menu_dispatch_cut_removes_selection`.
    {
        let mut ti = TextInput::local().with_initial_value("hello world");
        ti.apply_layout_external(Rect::new(0.0, 0.0, 300.0, 40.0));
        ti.core.set_selection(0, 5); // "hello"
        ti.open_context_menu([10.0, 18.0]);
        let cut_idx = ti
            .context_menu
            .items()
            .iter()
            .position(|i| i.label() == Some("Cut"))
            .expect("Cut row present");
        assert_eq!(ti.context_actions[cut_idx], Some(CtxAction::Cut));
        ti.dispatch_context_action(cut_idx);
        assert_eq!(ti.value(), " world", "cut removed the selected word");
    }

    // Case `left_click_outside_open_menu_dismisses_it`.
    {
        let mut ti = TextInput::local().with_initial_value("hello world");
        ti.apply_layout_external(Rect::new(0.0, 0.0, 300.0, 40.0));
        ti.set_focus(true);
        ti.open_context_menu([10.0, 18.0]);
        assert!(ti.context_menu.is_open());

        let moved = crate::synthetic_input::cursor_moved(280.0, 38.0);
        let press = crate::synthetic_input::mouse_press();
        let handled = dispatch(&mut ti, &moved) | dispatch(&mut ti, &press);
        assert!(handled, "click consumed while menu open");
        assert!(
            !ti.context_menu.is_open(),
            "menu dismissed by outside click"
        );
    }

    // Case `misspelling_context_action_replaces_the_selected_word`.
    {
        let mut ti = TextInput::local()
            .with_spell_language("en_US")
            .with_initial_value("teh");
        ti.core.set_selection(0, 3);
        ti.open_context_menu([10.0, 10.0]);

        let item_idx = ti
            .context_actions
            .iter()
            .position(|action| {
                matches!(
                    action,
                    Some(CtxAction::ApplySpelling { replacement, .. })
                        if replacement == "the"
                )
            })
            .expect("the correction is offered");
        assert!(ti.dispatch_context_action(item_idx));
        assert_eq!(ti.value(), "the");
    }

    // Case `selected_text_action_is_reported_without_blocking_the_widget`.
    {
        let mut ti = TextInput::local()
            .with_spellcheck(false)
            .with_text_action("translate", "Translate")
            .with_initial_value("hello world");
        ti.core.set_selection(0, 5);
        ti.open_context_menu([10.0, 10.0]);
        let item_idx = ti
            .context_actions
            .iter()
            .position(|action| matches!(action, Some(CtxAction::TextAction(0))))
            .expect("registered action is in the context menu");

        assert!(ti.dispatch_context_action(item_idx));
        assert_eq!(
            ti.take_text_action_request(),
            Some(TextActionRequest {
                id: "translate".to_string(),
                text: "hello".to_string(),
                range: (0, 5),
            })
        );
        assert_eq!(ti.value(), "hello world", "dispatch itself is read-only");
    }
}

#[test]
fn spellcheck_eligibility_follows_field_kind_vocabulary_and_language() {
    // Motion reads at its end: this test checks the resting frame.
    let _settled = crate::motion::testing::settled();
    // Case `spelling_defaults_on_and_can_be_disabled_per_field`.
    {
        let mut prose = TextInput::local()
            .with_spell_language("en_US")
            .with_initial_value("teh");
        prose.set_focus(true);
        prose.refresh_spelling();
        assert!(prose.spellcheck_enabled());
        assert_eq!(prose.spell_spans, vec![(0, 3)]);

        let mut search_or_identifier = TextInput::local()
            .with_kind(TextInputKind::Search)
            .with_spell_language("en_US")
            .with_initial_value("teh");
        search_or_identifier.set_focus(true);
        search_or_identifier.refresh_spelling();
        assert!(search_or_identifier.spell_spans.is_empty());

        for kind in [
            TextInputKind::Name,
            TextInputKind::Search,
            TextInputKind::Username,
            TextInputKind::Email,
            TextInputKind::Url,
            TextInputKind::Path,
            TextInputKind::Code,
            TextInputKind::Number,
            TextInputKind::Password,
            TextInputKind::Secret,
        ] {
            let mut structured = TextInput::local()
                .with_kind(kind)
                .with_spell_language("en_US")
                .with_initial_value("teh");
            structured.set_focus(true);
            structured.refresh_spelling();
            assert!(structured.spell_spans.is_empty(), "kind {kind:?}");
        }

        let mut opted_in = TextInput::local()
            .with_kind(TextInputKind::Search)
            .with_spellcheck(true)
            .with_spell_language("en_US")
            .with_initial_value("teh");
        opted_in.set_focus(true);
        opted_in.refresh_spelling();
        assert_eq!(opted_in.spell_spans, vec![(0, 3)]);

        let mut secret = TextInput::local()
            .with_secret(true)
            .with_spell_language("en_US")
            .with_initial_value("teh");
        secret.refresh_spelling();
        assert!(
            secret.spell_spans.is_empty(),
            "secret contents must never reach a checker"
        );
    }

    // Case `custom_vocabulary_suppresses_a_squiggle`.
    {
        let mut ti = TextInput::local()
            .with_spell_language("en_US")
            .with_initial_value("qwordz");
        ti.set_focus(true);
        ti.refresh_personal_spelling();
        ti.spell_dictionary = Some(spellcheck::Dictionary::from_words(["qwordz"]));
        ti.refresh_spelling();
        assert!(ti.spell_spans.is_empty());
    }

    // Case `field_language_override_is_independent_of_session_default`.
    {
        let english = TextInput::local().with_spell_language("en-US");
        let german = TextInput::local().with_spell_language("de-DE");
        assert_eq!(english.spell_language(), "en-US");
        assert_eq!(german.spell_language(), "de-DE");
    }

    // Case `untouched_inactive_field_does_no_spellcheck_work`.
    {
        let mut ti = TextInput::local()
            .with_spell_language("en_US")
            .with_initial_value("teh");
        ti.refresh_spelling();
        assert!(ti.spell_spans.is_empty());
        assert!(
            ti.spell_cache_key.is_none(),
            "inactive initial values must not populate the checker cache"
        );
    }
}

#[test]
fn spellcheck_squiggles_appear_at_word_boundaries_and_after_settling() {
    // Motion reads at its end: this test checks the resting frame.
    let _settled = crate::motion::testing::settled();
    // Case `spelling_checks_immediately_but_only_hides_the_word_being_typed`.
    {
        let mut ti = TextInput::local()
            .with_spell_language("en_US")
            .with_initial_value("teh wrold");
        ti.set_focus(true);
        ti.core.set_caret(ti.text.len());
        ti.typing.touch();

        ti.refresh_spelling();
        // Dictionary warm-up can dominate this unit test in an unoptimised
        // build. Model a fresh edit after checking to test presentation timing.
        ti.typing.touch();

        assert_eq!(ti.spell_spans, vec![(0, 3), (4, 9)]);
        assert!(ti.spelling_span_is_visible(0, 3));
        assert!(!ti.spelling_span_is_visible(4, 9));
        assert!(!Widget::needs_animation_frame(&ti));
        assert!(Widget::next_wake_deadline(&ti).is_some());
    }

    // Case `a_word_becomes_visible_immediately_when_a_boundary_is_typed`.
    {
        let mut ti = TextInput::local()
            .with_spell_language("en_US")
            .with_initial_value("wrold ");
        ti.set_focus(true);
        ti.core.set_caret(ti.text.len());
        ti.typing.touch();

        ti.refresh_spelling();

        assert_eq!(ti.spell_spans, vec![(0, 5)]);
        assert!(ti.actively_edited_word().is_none());
        assert!(ti.spelling_span_is_visible(0, 5));
    }

    // Case `active_word_is_revealed_after_the_short_settle_period`.
    {
        let mut ti = TextInput::local()
            .with_spell_language("en_US")
            .with_initial_value("wrold");
        ti.set_focus(true);
        ti.core.set_caret(ti.text.len());
        if let Some(at) = crate::time::now()
            .checked_sub(SPELL_EDIT_SETTLE)
            .and_then(|t| t.checked_sub(Duration::from_millis(1)))
        {
            ti.typing.touch_at(at);
        }

        ti.refresh_spelling();

        assert_eq!(ti.spell_spans, vec![(0, 5)]);
        assert!(ti.spelling_span_is_visible(0, 5));
        assert!(!Widget::needs_animation_frame(&ti));
    }
}

/// A focused en-US field holding `text` with the caret at its end.
fn suggesting_field(text: &str) -> TextInput {
    let mut ti = TextInput::local()
        .with_spellcheck(true)
        .with_autocorrect(true)
        .with_spell_language("en_US")
        .with_initial_value(text);
    ti.set_focus(true);
    ti.core.set_caret(ti.text.len());
    ti
}

#[test]
fn a_short_pause_at_the_end_of_a_misspelled_word_shows_its_suggestion() {
    // Motion reads at its end: this test checks the resting frame.
    let _settled = crate::motion::testing::settled();
    let mut ti = suggesting_field("so beautifull");
    let start = crate::time::now();

    ti.refresh_suggestion_at(start);
    assert!(
        ti.autocorrection_preview.is_none(),
        "nothing is offered while the word may still be growing"
    );
    assert_eq!(ti.caret_word.deadline(), Some(start + SUGGESTION_SETTLE));

    ti.refresh_suggestion_at(start + SUGGESTION_SETTLE - Duration::from_millis(1));
    assert!(ti.autocorrection_preview.is_none());

    ti.refresh_suggestion_at(start + SUGGESTION_SETTLE);
    let preview = ti
        .autocorrection_preview
        .clone()
        .expect("a settled misspelled word gets a suggestion");
    assert_eq!(
        (preview.start, preview.end, preview.original.as_str()),
        (3, 13, "beautifull")
    );
    assert_eq!(preview.replacement, "beautiful");
    assert!(
        !preview.automatic,
        "an ordinary suggestion, not autocorrect"
    );
    assert!(!ti.spelling_span_is_visible(3, 13));

    // A boundary does not apply an ordinary suggestion …
    let mut boundary = suggesting_field("so beautifull");
    boundary.refresh_suggestion_at(start);
    boundary.refresh_suggestion_at(start + SUGGESTION_SETTLE);
    assert!(boundary.autocorrection_preview.is_some());
    boundary.insert_word_boundary(" ");
    assert_eq!(boundary.value(), "so beautifull ");

    // … a click on its chip does.
    assert!(ti.accept_suggestion());
    assert_eq!(ti.value(), "so beautiful");
    assert_eq!(ti.cursor_pos(), "so beautiful".len());
    assert!(ti.autocorrection_preview.is_none());
}

#[test]
fn moving_the_caret_back_into_an_earlier_word_shows_its_suggestion() {
    // Motion reads at its end: this test checks the resting frame.
    let _settled = crate::motion::testing::settled();
    let mut ti = suggesting_field("beautifull day");
    let start = crate::time::now();
    ti.refresh_suggestion_at(start);
    ti.refresh_suggestion_at(start + SUGGESTION_SETTLE);
    assert!(
        ti.autocorrection_preview.is_none(),
        "`day` is spelled right"
    );

    // Back into the first word: it settles after the same short wait.
    let back = start + SUGGESTION_SETTLE * 2;
    ti.set_cursor_pos(4);
    ti.refresh_suggestion_at(back);
    assert!(ti.autocorrection_preview.is_none());
    ti.refresh_suggestion_at(back + SUGGESTION_SETTLE);
    let preview = ti.autocorrection_preview.clone().expect("suggestion");
    assert_eq!((preview.start, preview.end), (0, 10));
    assert_eq!(preview.replacement, "beautiful");

    // Moving within the word keeps the suggestion; leaving it clears it.
    ti.set_cursor_pos(7);
    ti.refresh_suggestion_at(back + SUGGESTION_SETTLE);
    assert!(ti.autocorrection_preview.is_some());
    ti.set_cursor_pos(10);
    ti.refresh_suggestion_at(back + SUGGESTION_SETTLE);
    assert!(
        ti.autocorrection_preview.is_some(),
        "the word's end is in it"
    );
    ti.set_cursor_pos(ti.text.len());
    ti.refresh_suggestion_at(back + SUGGESTION_SETTLE);
    assert!(ti.autocorrection_preview.is_none());

    // A selection or an unfocused field offers nothing.
    ti.set_selection(0, 4);
    ti.refresh_suggestion_at(back + SUGGESTION_SETTLE * 3);
    assert!(ti.autocorrection_preview.is_none());
    ti.set_cursor_pos(4);
    ti.set_focus(false);
    ti.refresh_suggestion_at(back + SUGGESTION_SETTLE * 4);
    assert!(ti.autocorrection_preview.is_none());
}

#[test]
fn the_settling_word_schedules_its_own_wake() {
    // Motion reads at its end: this test checks the resting frame.
    let _settled = crate::motion::testing::settled();
    let mut ti = suggesting_field("so beautifull");
    // Far enough ahead that a slow test run never sees the deadline pass.
    let now = crate::time::now() + Duration::from_secs(3600);
    ti.refresh_suggestion_at(now);
    assert_eq!(
        Widget::next_wake_deadline(&ti),
        Some(now + SUGGESTION_SETTLE),
        "the suggestion appears without further input"
    );
    let mut off = TextInput::local()
        .with_spellcheck(false)
        .with_autocorrect(false)
        .with_initial_value("so beautifull");
    off.set_focus(true);
    off.refresh_suggestion_at(now);
    assert_eq!(Widget::next_wake_deadline(&off), None);
}

#[test]
fn autocorrect_previews_applies_restores_and_respects_opt_outs() {
    // Motion reads at its end: this test checks the resting frame.
    let _settled = crate::motion::testing::settled();
    // Case `obvious_typo_autocorrects_at_boundary_and_backspace_restores_it`.
    {
        let mut ti = TextInput::local()
            .with_spellcheck(true)
            .with_autocorrect(true)
            .with_spell_language("en_US")
            .with_initial_value("teh");
        ti.core.set_caret(ti.text.len());
        ti.insert_word_boundary(" ");
        assert_eq!(ti.value(), "the ");
        assert!(ti.last_autocorrection.is_some());

        let mut handled = false;
        ti.handle_backspace_key(false, &mut handled);
        assert!(handled);
        assert_eq!(ti.value(), "teh");
        assert!(ti.last_autocorrection.is_none());
    }

    // Case `autocorrection_is_previewed_before_the_boundary_applies_it`.
    {
        let mut ti = TextInput::local()
            .with_autocorrect(true)
            .with_spell_language("en_US")
            .with_initial_value("adress");
        ti.set_focus(true);
        ti.core.set_caret(ti.text.len());

        ti.refresh_autocorrection_preview();
        ti.refresh_spelling();

        let preview = ti
            .autocorrection_preview
            .as_ref()
            .expect("a high-confidence correction is previewed");
        assert_eq!(preview.original, "adress");
        assert_eq!(preview.replacement, "address");
        assert!(!ti.spelling_span_is_visible(0, 6));

        ti.insert_word_boundary(" ");
        assert_eq!(ti.value(), "address ");
    }

    // Case `autocorrect_respects_field_opt_out_and_personal_vocabulary`.
    {
        let mut off = TextInput::local()
            .with_autocorrect(false)
            .with_spell_language("en_US")
            .with_initial_value("teh");
        off.core.set_caret(off.text.len());
        off.insert_word_boundary(" ");
        assert_eq!(off.value(), "teh ");

        let mut known = TextInput::local()
            .with_autocorrect(true)
            .with_spell_language("en_US")
            .with_initial_value("teh");
        known.core.set_caret(known.text.len());
        known.spell_active = true;
        known.refresh_personal_spelling();
        known.spell_dictionary = Some(spellcheck::Dictionary::from_words(["teh"]));
        known.insert_word_boundary(" ");
        assert_eq!(known.value(), "teh ");
    }
}

#[test]
fn word_boundaries_walk_starts_ends_and_snap() {
    // Motion reads at its end: this test checks the resting frame.
    let _settled = crate::motion::testing::settled();
    // Case `prev_word_boundary_walks_word_starts`.
    {
        let s = "hello world quick ui";
        //       ^0    ^6    ^12   ^18
        assert_eq!(prev_word_boundary(s, s.len()), 18);
        assert_eq!(prev_word_boundary(s, 18), 12);
        assert_eq!(prev_word_boundary(s, 12), 6);
        assert_eq!(prev_word_boundary(s, 6), 0);
        assert_eq!(prev_word_boundary(s, 0), 0);
    }

    // Case `next_word_boundary_walks_word_ends`.
    // `next_word_boundary` lands *past* the trailing space, like "Ctrl + Right".
    {
        let s = "hello world quick ui";
        assert_eq!(next_word_boundary(s, 0), 6);
        assert_eq!(next_word_boundary(s, 6), 12);
        assert_eq!(next_word_boundary(s, 12), 18);
        assert_eq!(next_word_boundary(s, 18), s.len());
        assert_eq!(next_word_boundary(s, s.len()), s.len());
    }

    // Case `word_boundaries_snaps_to_word_bounds`.
    // At a space immediately after a word, snaps to the word on the left
    // (matches every native editor's double-click-on-trailing-space).
    {
        let s = "hello world";
        assert_eq!(word_boundaries(s, 2), (0, 5)); // mid-word
        assert_eq!(word_boundaries(s, 0), (0, 5)); // word start
        assert_eq!(word_boundaries(s, 5), (0, 5)); // space → snap left
        assert_eq!(word_boundaries(s, 6), (6, 11)); // start of next word
        assert_eq!(word_boundaries(s, 11), (6, 11)); // end → snap left
    }
}

#[test]
fn editing_primitives_splice_replace_filter_and_repeat() {
    // Motion reads at its end: this test checks the resting frame.
    let _settled = crate::motion::testing::settled();
    // Case `delete_selection_removes_range_and_collapses`.
    {
        let mut ti = TextInput::local();
        ti.text = "hello world".to_string();
        ti.core.set_selection(6, 11);
        let mut doc = AutoCommit::new();
        let obj_id = doc
            .put_object(&automerge::ROOT, "text_1", ObjType::Text)
            .unwrap();
        let _ = doc.splice_text(&obj_id, 0, 0, "hello world");
        ti.delete_selection();
        assert_eq!(ti.text, "hello ");
        assert_eq!(ti.core.caret(), 6);
        assert!(!ti.has_selection());
    }

    // Case `insert_at_cursor_splices_and_marks_dirty`.
    {
        let mut ti = TextInput::local().with_initial_value("hi world");
        ti.set_cursor_pos(2); // just after "hi"
        ti.insert_at_cursor(" there");
        assert_eq!(ti.value(), "hi there world");
        assert_eq!(
            ti.cursor_pos(),
            "hi there".len(),
            "caret advances past the insert"
        );
        assert_eq!(
            ti.take_dirty().as_deref(),
            Some("hi there world"),
            "the edit is observable"
        );
    }

    // Case `character_filter_applies_before_replacing_the_selection`.
    {
        let mut ti = TextInput::local()
            .with_initial_value("A1")
            .with_char_filter(|ch| !ch.is_whitespace());
        ti.select_all();
        ti.insert_at_cursor(" B 2 : D 4 ");
        assert_eq!(ti.value(), "B2:D4");
        assert_eq!(ti.cursor_pos(), "B2:D4".len());
        assert_eq!(ti.take_dirty().as_deref(), Some("B2:D4"));
    }

    // Case `insert_str_replaces_selection`.
    // The path Ctrl+V uses when pasting over selected text.
    {
        let mut ti = TextInput::local();
        ti.text = "hello world".to_string();
        ti.core.set_selection(6, 11);
        let mut doc = AutoCommit::new();
        let obj_id = doc
            .put_object(&automerge::ROOT, "text_1", ObjType::Text)
            .unwrap();
        let _ = doc.splice_text(&obj_id, 0, 0, "hello world");
        ti.insert_str("there");
        assert_eq!(ti.text, "hello there");
        assert_eq!(ti.core.caret(), "hello there".len());
        assert!(!ti.has_selection());
    }

    // Case `typing_replaces_selection_via_handle_event`.
    {
        let mut ti = TextInput::local().with_initial_value("hello world");
        ti.set_focus(true);
        let mut doc = AutoCommit::new();
        drive(&mut ti, &mut doc, &char_key("a"), mods(true, false)); // select all
        drive(&mut ti, &mut doc, &char_key("Z"), mods(false, false)); // type over it
        assert_eq!(ti.value(), "Z", "typing over a full selection replaces it");
    }

    // Case `key_text_preserves_shift_and_german_layout_characters`.
    {
        let mut ti = TextInput::local();
        ti.set_focus(true);
        let mut doc = AutoCommit::new();

        // `KeyEvent::text` is the layout/IME-resolved text. The widget must
        // insert it verbatim rather than reconstructing ASCII from a physical
        // key, which would lose Shift and German/AltGr punctuation.
        for text in ["A", "Ä", "@", "?"] {
            drive(&mut ti, &mut doc, &char_key(text), mods(false, false));
        }
        assert_eq!(ti.value(), "AÄ@?");
    }

    // Case `repeated_backspace_and_delete_press_events_keep_editing`.
    {
        let mut doc = AutoCommit::new();
        let mut backspace = TextInput::local().with_initial_value("abcd");
        backspace.set_focus(true);
        let first = named_key(NamedKey::Backspace);
        let mut repeat = named_key(NamedKey::Backspace);
        if let WindowEvent::KeyboardInput { event, .. } = &mut repeat {
            event.repeat = true;
        }
        drive(&mut backspace, &mut doc, &first, mods(false, false));
        drive(&mut backspace, &mut doc, &repeat, mods(false, false));
        assert_eq!(
            backspace.value(),
            "ab",
            "held Backspace deletes on repeat events"
        );

        let mut delete = TextInput::local().with_initial_value("abcd");
        delete.set_focus(true);
        delete.set_cursor_pos(0);
        let first = named_key(NamedKey::Delete);
        let mut repeat = named_key(NamedKey::Delete);
        if let WindowEvent::KeyboardInput { event, .. } = &mut repeat {
            event.repeat = true;
        }
        drive(&mut delete, &mut doc, &first, mods(false, false));
        drive(&mut delete, &mut doc, &repeat, mods(false, false));
        assert_eq!(delete.value(), "cd", "held Delete deletes on repeat events");
    }
}

#[test]
fn secret_input_masks_rendering_and_accessibility_but_keeps_the_value() {
    // Motion reads at its end: this test checks the resting frame.
    let _settled = crate::motion::testing::settled();
    // Case `secret_input_masks_glyphs_but_keeps_value`.
    // Proven end-to-end through a real Automerge `Text` object.
    {
        let mut ti = TextInput::local().with_secret(true);

        let mut doc = AutoCommit::new();
        let obj_id = doc
            .put_object(&automerge::ROOT, "password", ObjType::Text)
            .unwrap();
        let _ = doc.splice_text(&obj_id, 0, 0, "hunter2");
        ti.bind_text(obj_id);

        ti.sync(&doc, &automerge::ROOT);
        assert_eq!(ti.text, "hunter2", "stored value must be the real secret");

        assert_eq!(ti.display_text(), "•••••••");
        // The masked prefix tracks character count, not bytes.
        assert_eq!(ti.display_prefix(3), "•••");

        let mut plain = TextInput::new(Vec::new());
        plain.text = "hunter2".to_string();
        assert_eq!(plain.display_text(), "hunter2");
        assert_eq!(plain.display_prefix(3), "hun");
    }

    // Case `local_secret_mirrors_without_entering_the_document`.
    {
        let buffer = Arc::new(Mutex::new(String::new()));
        let mut input = TextInput::local()
            .with_external_buffer(Arc::clone(&buffer))
            .with_secret(true);
        input.rect = Rect::new(0.0, 0.0, 200.0, 40.0);
        input.text = "correct horse battery staple".into();
        input.dirty = true;
        let document = AutoCommit::new();
        input.sync(&document, &automerge::ROOT);

        assert_eq!(&*buffer.lock().unwrap(), "correct horse battery staple");
        assert!(
            input.layout_needs_measure(),
            "publication must not consume layout invalidation"
        );
        buffer.lock().unwrap().clear();
        input.sync(&document, &automerge::ROOT);
        assert!(
            buffer.lock().unwrap().is_empty(),
            "an acknowledged secret must not be republished"
        );
        input.core.set_caret(input.text.len());
        input.insert_at_cursor("new draft");
        input.sync(&document, &automerge::ROOT);
        assert_eq!(
            &*buffer.lock().unwrap(),
            input.value(),
            "subsequent edits still publish"
        );
        assert!(buffer.lock().unwrap().ends_with("new draft"));
        assert!(document.keys(&automerge::ROOT).next().is_none());
    }

    // Case `accessibility_secret_input_masks_value_and_sets_protected`.
    {
        let mut ti = TextInput::new(Vec::new()).with_secret(true);
        ti.text = "hunter2".to_string();
        let node = Widget::accessibility(&ti).unwrap();
        assert_eq!(
            node.value, None,
            "a secret value must not leak into the a11y tree"
        );
        assert!(node.state.protected, "secret input is marked protected");
        assert_eq!(
            node.text_selection, None,
            "selection offsets must not reveal a secret's length"
        );
    }

    // Case `password_and_key_kinds_never_leak_to_a11y_even_unmasked`.
    for kind in [TextInputKind::Password, TextInputKind::Secret] {
        let mut ti = TextInput::new(Vec::new()).with_kind(kind);
        ti.text = "JBSWY3DPEHPK3PXP".to_string();
        let node = Widget::accessibility(&ti).unwrap();
        assert_eq!(node.value, None, "{kind:?}: a key is not exported");
        assert!(node.state.protected, "{kind:?}: a key field is protected");
        assert_eq!(node.text_selection, None, "{kind:?}: no selection offsets");
        assert_eq!(
            Widget::ime_content(&ti),
            Some(crate::ime::InputContent::PASSWORD),
            "{kind:?}: no input-method suggestions learn the key"
        );
    }

    // Case `secret_reveal_toggle_shows_glyphs_but_never_leaks_to_a11y`.
    {
        let mut ti = TextInput::new(Vec::new()).with_secret(true);
        ti.text = "hunter2".to_string();
        assert_eq!(ti.display_text(), "•••••••", "masked by default");

        ti.toggle_reveal();
        assert!(ti.is_revealed());
        assert_eq!(
            ti.display_text(),
            "hunter2",
            "revealed shows the real glyphs"
        );
        let node = Widget::accessibility(&ti).unwrap();
        assert_eq!(
            node.value, None,
            "revealed value still must not reach the a11y tree"
        );
        assert!(
            node.state.protected,
            "still marked protected while revealed"
        );

        ti.toggle_reveal();
        assert_eq!(ti.display_text(), "•••••••", "toggles back to masked");
    }

    // Case `secret_caret_moves_over_the_real_value`.
    {
        let mut ti = TextInput::local()
            .with_secret(true)
            .with_initial_value("s3cr3t");
        ti.set_focus(true);
        let mut doc = AutoCommit::new();

        ti.set_cursor_pos(0);
        drive(
            &mut ti,
            &mut doc,
            &named_key(NamedKey::ArrowRight),
            mods(false, false),
        );
        assert_eq!(ti.cursor_pos(), 1, "Right moved one real char boundary");
    }
}

#[test]
fn a_secret_never_reaches_the_clipboard() {
    // Motion reads at its end: this test checks the resting frame.
    let _settled = crate::motion::testing::settled();
    let services = crate::host::HostServices::null();
    let _host = crate::host::enter(&services);
    let clipboard = || crate::text_selection::clipboard_paste();

    for revealed in [false, true] {
        {
            crate::text_selection::clipboard_copy("before");
            let mut ti = TextInput::local()
                .with_secret(true)
                .with_initial_value("s3cr3t");
            if revealed {
                ti.toggle_reveal();
            }
            ti.set_focus(true);
            let mut doc = AutoCommit::new();

            drive(&mut ti, &mut doc, &char_key("a"), mods(true, false));
            assert_eq!(ti.selected_str(), Some("s3cr3t"), "Select All still works");
            drive(&mut ti, &mut doc, &char_key("c"), mods(true, false));
            assert_eq!(
                clipboard().as_deref(),
                Some("before"),
                "revealed: {revealed}"
            );
            drive(&mut ti, &mut doc, &char_key("x"), mods(true, false));
            assert_eq!(
                clipboard().as_deref(),
                Some("before"),
                "revealed: {revealed}"
            );
            assert_eq!(ti.value(), "s3cr3t", "a refused cut keeps the value");
            assert_eq!(ti.selected_str(), Some("s3cr3t"), "and the selection");
        }

        {
            crate::text_selection::clipboard_copy("pasted");
            let mut ti = TextInput::local()
                .with_secret(true)
                .with_initial_value("s3cr3t")
                .with_text_action("translate", "Translate");
            if revealed {
                ti.toggle_reveal();
            }
            ti.apply_layout_external(Rect::new(0.0, 0.0, 300.0, 40.0));
            ti.select_all();
            ti.open_context_menu([20.0, 18.0]);
            let labels = ti.context_menu_labels();
            assert!(!labels.contains(&"Cut".to_string()), "got {labels:?}");
            assert!(!labels.contains(&"Copy".to_string()), "got {labels:?}");
            assert!(!labels.contains(&"Translate".to_string()), "got {labels:?}");
            assert!(labels.contains(&"Paste".to_string()), "got {labels:?}");
            assert!(labels.contains(&"Select All".to_string()), "got {labels:?}");
        }

        {
            crate::text_selection::clipboard_copy("hunter2");
            let mut ti = TextInput::local().with_secret(true);
            if revealed {
                ti.toggle_reveal();
            }
            ti.set_focus(true);
            let mut doc = AutoCommit::new();
            drive(&mut ti, &mut doc, &char_key("v"), mods(true, false));
            assert_eq!(ti.value(), "hunter2");
        }
    }
}

#[test]
fn secret_field_carries_a_show_hide_control_off_the_tab_order() {
    use crate::a11y::{Action, Role};

    // Motion reads at its end: this test checks the resting frame.
    let _settled = crate::motion::testing::settled();
    // Case `secret_is_hidden_by_default_with_a_named_show_control`.
    {
        let mut ti = TextInput::local()
            .with_secret(true)
            .with_initial_value("hunter2");
        ti.apply_layout_external(Rect::new(0.0, 0.0, 240.0, 40.0));
        assert!(!ti.is_revealed(), "a secret starts hidden");
        assert_eq!(ti.display_text(), "•••••••");
        assert!(ti.has_reveal_control());
        let control = ti.reveal_control_rect().unwrap();
        assert_eq!(
            control,
            Rect::new(200.0, 0.0, 240.0, 40.0),
            "trailing square"
        );

        let children = Widget::children(&ti);
        assert_eq!(children.len(), 1, "the control is a real child widget");
        let child = children.first().unwrap();
        assert!(
            !child.is_focusable(),
            "an inline accessory is not a Tab stop"
        );
        let node = child.accessibility().unwrap();
        assert_eq!(node.role, Role::Button);
        assert_eq!(node.name.as_deref(), Some("Show password"));
        assert_eq!(
            Widget::tooltip_target_at(&ti, control.center()).map(|(_, tip)| tip),
            Some("Show password".to_string())
        );
        assert_eq!(
            Widget::cursor_shape(&ti, control.center()),
            Some(winit::window::CursorIcon::Pointer)
        );
    }

    // Case `activating_the_control_toggles_glyphs_and_its_name_only`.
    {
        let mut ti = TextInput::local()
            .with_secret(true)
            .with_initial_value("hunter2");
        ti.apply_layout_external(Rect::new(0.0, 0.0, 240.0, 40.0));
        let mut doc = AutoCommit::new();
        let activated = Widget::children_mut(&mut ti)
            .into_iter()
            .next()
            .unwrap()
            .perform_action(
                &Action::Activate,
                &[],
                &mut crate::widget::Edits::lend(&mut doc),
                &automerge::ROOT,
            );
        assert!(activated, "Enter/Space and AT activation toggle");
        assert!(ti.is_revealed());
        assert_eq!(ti.display_text(), "hunter2");
        let child = Widget::children(&ti).into_iter().next().unwrap();
        assert_eq!(
            child.accessibility().unwrap().name.as_deref(),
            Some("Hide password")
        );
        let field = Widget::accessibility(&ti).unwrap();
        assert_eq!(field.value, None, "revealing never exposes the value");
        assert!(field.state.protected);
        assert!(Widget::text_review(&ti).is_none());
        assert!(
            doc.keys(&automerge::ROOT).next().is_none(),
            "nothing persisted"
        );

        let _ = Widget::children_mut(&mut ti)
            .into_iter()
            .next()
            .unwrap()
            .perform_action(
                &Action::Activate,
                &[],
                &mut crate::widget::Edits::lend(&mut doc),
                &automerge::ROOT,
            );
        assert!(!ti.is_revealed(), "a second activation hides it again");
    }

    // Case `pointer_toggle_keeps_focus_and_caret_in_the_field`.
    {
        let mut ti = TextInput::local()
            .with_secret(true)
            .with_initial_value("hunter2");
        ti.apply_layout_external(Rect::new(0.0, 0.0, 240.0, 40.0));
        ti.set_focus(true);
        draw_to_populate(&mut ti);
        let caret = ti.cursor_pos();
        let mut doc = AutoCommit::new();
        let target = ti.reveal_control_rect().unwrap().center();
        drive(
            &mut ti,
            &mut doc,
            &cursor_moved(target.x, target.y),
            mods(false, false),
        );

        let mut sender = Dispatch::new().doc(&mut doc);
        let press = sender.send(&mut ti, &mouse_press());
        let release = sender.send(&mut ti, &mouse_release());
        let handled = press.handled | release.handled;
        let focus_target = release.focus_target.or(press.focus_target);
        assert!(handled);
        assert!(ti.is_revealed(), "a click reveals");
        assert_eq!(focus_target, None, "the click does not move focus");
        assert!(ti.is_focused());
        assert_eq!(ti.cursor_pos(), caret, "the caret stays where it was");

        let tap = Point::new(target.x, target.y);
        drive(
            &mut ti,
            &mut doc,
            &touch(TouchPhase::Started, 7, tap),
            mods(false, false),
        );
        drive(
            &mut ti,
            &mut doc,
            &touch(TouchPhase::Ended, 7, tap),
            mods(false, false),
        );
        assert!(!ti.is_revealed(), "a tap hides it again");
    }

    // Case `tab_moves_from_field_to_field_past_the_control`.
    {
        let mut ti = TextInput::local().with_secret(true);
        let mut taffy = TaffyTree::new();
        let mut node_map = std::collections::HashMap::new();
        let root = ti.build_layout(&mut taffy, &Theme::dark(), &mut node_map);
        let (order, trapped) = crate::focus_navigation::order(root, &ti, &taffy);
        assert!(!trapped);
        assert_eq!(order, [root], "the field alone, not its show/hide control");
    }

    // Case `only_single_line_secrets_carry_the_control`.
    {
        let plain = TextInput::local();
        assert!(!plain.has_reveal_control());
        assert!(Widget::children(&plain).is_empty());
        let multiline = TextInput::local().with_secret(true).multiline(true);
        assert!(!multiline.has_reveal_control());
    }

    // Case `a_disabled_secret_offers_no_reachable_control`.
    {
        let mut ti = TextInput::local().with_secret(true).disabled(true);
        ti.apply_layout_external(Rect::new(0.0, 0.0, 240.0, 40.0));
        let child = Widget::children(&ti).into_iter().next().unwrap();
        assert!(!child.is_focusable());
        assert!(child.accessibility().unwrap().state.disabled);
    }
}

#[test]
fn accessibility_exposes_role_name_value_and_grapheme_actions() {
    // Motion reads at its end: this test checks the resting frame.
    let _settled = crate::motion::testing::settled();
    // Case `accessibility_reports_text_input_role_and_value`.
    {
        use crate::a11y::{AccessibleTextSelection, Role};
        let mut ti = TextInput::new(Vec::new());
        ti.text = "hello".to_string();
        ti.core.set_selection(1, 4);
        let node = Widget::accessibility(&ti).unwrap();
        assert_eq!(node.role, Role::TextInput);
        assert_eq!(node.value.as_deref(), Some("hello"));
        assert!(node.state.focusable, "enabled input is focusable");
        assert!(!node.state.disabled);
        assert!(!node.state.protected);
        assert_eq!(
            node.text_selection,
            Some(AccessibleTextSelection {
                anchor: 1,
                focus: 4,
            })
        );
    }

    // Case `accessibility_uses_explicit_name_not_placeholder`.
    {
        let input = TextInput::local()
            .with_placeholder("name@example.com")
            .with_accessible_name("Email address");
        let node = Widget::accessibility(&input).unwrap();
        assert_eq!(node.name.as_deref(), Some("Email address"));
        assert_ne!(node.name.as_deref(), Some(input.placeholder()));
    }

    // Case `accessibility_actions_use_extended_grapheme_indices_and_leave_the_document_to_the_owner`.
    {
        use crate::a11y::Action;

        let family = "👩‍👩‍👧‍👦";
        let original = format!("a{family}b");
        let mut ti = TextInput::local();
        ti.text = original.clone();
        let mut doc = AutoCommit::new();
        let obj = doc
            .put_object(&automerge::ROOT, "body", ObjType::Text)
            .unwrap();
        doc.splice_text(&obj, 0, 0, &original).unwrap();
        ti.bind_text(obj.clone());

        assert!(Widget::perform_action(
            &mut ti,
            &Action::SetSelection {
                anchor: 1,
                focus: 2,
            },
            &[],
            &mut crate::widget::Edits::lend(&mut doc),
            &automerge::ROOT,
        ));
        assert_eq!(
            ti.selection_range_public(),
            Some((1, 1 + family.len())),
            "one AccessKit character selects the whole ZWJ family"
        );

        assert!(Widget::perform_action(
            &mut ti,
            &Action::SetValue("new value".into()),
            &[],
            &mut crate::widget::Edits::lend(&mut doc),
            &automerge::ROOT,
        ));
        assert_eq!(ti.value(), "new value");
        assert_eq!(doc.text(&obj).unwrap(), original, "the owner writes it");
        assert_eq!(ti.take_dirty().as_deref(), Some("new value"));
    }

    // Case `multiline_sets_accessibility_flag`.
    {
        let ml = TextInput::local().multiline(true);
        assert!(
            Widget::accessibility(&ml).unwrap().state.multiline,
            "multiline flagged in a11y"
        );
        let single = TextInput::local();
        assert!(!Widget::accessibility(&single).unwrap().state.multiline);
    }
}

#[test]
fn a_bound_text_is_only_read_and_edits_wait_for_the_owner() {
    use crate::a11y::Action;

    // Motion reads at its end: this test checks the resting frame.
    let _settled = crate::motion::testing::settled();
    let mut document = AutoCommit::new();
    let title = document
        .put_object(&automerge::ROOT, "title", ObjType::Text)
        .unwrap();
    document.splice_text(&title, 0, 0, "Draft").unwrap();
    let mut input = TextInput::local();
    input.bind_text(title.clone());
    input.sync(&document, &automerge::ROOT);
    assert_eq!(input.value(), "Draft");
    assert!(
        input.take_dirty().is_none(),
        "showing the text is not an edit"
    );

    assert!(Widget::perform_action(
        &mut input,
        &Action::SetSelection {
            anchor: 0,
            focus: 0,
        },
        &[],
        &mut crate::widget::Edits::lend(&mut document),
        &automerge::ROOT,
    ));
    assert_eq!(input.value(), "Draft", "moving the caret changes no text");

    assert!(Widget::perform_action(
        &mut input,
        &Action::SetValue("Project Atlas".into()),
        &[],
        &mut crate::widget::Edits::lend(&mut document),
        &automerge::ROOT,
    ));
    assert_eq!(
        document.text(&title).unwrap(),
        "Draft",
        "the input never writes the document"
    );
    assert_eq!(input.take_dirty().as_deref(), Some("Project Atlas"));
}

// Driven through `handle_event`, never by poking private fields, so a
// regression in event routing is caught too.
fn mods(ctrl: bool, shift: bool) -> Modifiers {
    use winit::keyboard::ModifiersState;
    let mut s = ModifiersState::empty();
    if ctrl {
        s |= ModifiersState::CONTROL;
    }
    if shift {
        s |= ModifiersState::SHIFT;
    }
    Modifiers::from(s)
}

fn command_mods() -> Modifiers {
    use winit::keyboard::ModifiersState;
    Modifiers::from(ModifiersState::SUPER)
}

use crate::synthetic_input::named_key;

use crate::synthetic_input::cursor_moved;
use crate::synthetic_input::mouse_press;
use crate::synthetic_input::mouse_release;
fn middle_press() -> WindowEvent {
    crate::synthetic_input::mouse_input(ElementState::Pressed, MouseButton::Middle)
}

fn test_node() -> (TaffyTree<()>, NodeId) {
    let mut taffy = TaffyTree::new();
    let node = taffy.leaf_node(Style::default());
    (taffy, node)
}

fn touch(phase: TouchPhase, id: u64, point: Point) -> WindowEvent {
    WindowEvent::Touch(winit::event::Touch {
        device_id: winit::event::DeviceId::dummy(),
        phase,
        location: winit::dpi::PhysicalPosition::new(point.x, point.y),
        force: None,
        id,
    })
}

fn drive(ti: &mut TextInput, doc: &mut AutoCommit, ev: &WindowEvent, m: Modifiers) -> bool {
    Dispatch::new().modifiers(m).doc(doc).send(ti, ev).handled
}

#[test]
fn touch_owns_text_selection_until_release() {
    // Motion reads at its end: this test checks the resting frame.
    let _settled = crate::motion::testing::settled();
    // Case `touch_places_caret_drags_selection_and_releases_ownership`.
    {
        let mut input = TextInput::local().with_initial_value("abcdef");
        input.apply_layout_external(Rect::new(0.0, 0.0, 240.0, 44.0));
        draw_to_populate(&mut input);
        let mut doc = AutoCommit::new();
        let start = Point::new(x_for_byte(&input, 1), 22.0);
        let end = Point::new(x_for_byte(&input, 5), 22.0);

        assert!(drive(
            &mut input,
            &mut doc,
            &touch(TouchPhase::Started, 7, start),
            Modifiers::default()
        ));
        assert_eq!(input.active_touch, Some(7));
        assert!(drive(
            &mut input,
            &mut doc,
            &touch(TouchPhase::Moved, 7, end),
            Modifiers::default()
        ));
        assert_eq!(input.selection_range(), Some((1, 5)));
        assert!(drive(
            &mut input,
            &mut doc,
            &touch(TouchPhase::Ended, 7, end),
            Modifiers::default()
        ));
        assert_eq!(input.active_touch, None);
        assert!(!input.is_dragging);
    }

    // Case `secondary_touch_cannot_steal_text_selection`.
    {
        let mut input = TextInput::local().with_initial_value("abcdef");
        input.apply_layout_external(Rect::new(0.0, 0.0, 240.0, 44.0));
        draw_to_populate(&mut input);
        let mut doc = AutoCommit::new();
        let point = Point::new(x_for_byte(&input, 2), 22.0);
        assert!(drive(
            &mut input,
            &mut doc,
            &touch(TouchPhase::Started, 7, point),
            Modifiers::default()
        ));
        assert!(drive(
            &mut input,
            &mut doc,
            &touch(TouchPhase::Started, 8, point),
            Modifiers::default()
        ));
        assert_eq!(input.active_touch, Some(7));
        assert!(drive(
            &mut input,
            &mut doc,
            &touch(TouchPhase::Ended, 8, point),
            Modifiers::default()
        ));
        assert_eq!(input.active_touch, Some(7));
        assert!(drive(
            &mut input,
            &mut doc,
            &touch(TouchPhase::Cancelled, 7, point),
            Modifiers::default()
        ));
        assert_eq!(input.active_touch, None);
    }
}

#[test]
fn action_bindings_own_edits_and_accessibility_set_value() {
    // Motion reads at its end: this test checks the resting frame.
    let _settled = crate::motion::testing::settled();
    // Case `bound_text_edits_optimistically_without_splicing_document`.
    {
        let mut doc = AutoCommit::new();
        let text = doc
            .put_object(&automerge::ROOT, "note", ObjType::Text)
            .expect("note text");
        doc.splice_text(&text, 0, 0, "before")
            .expect("seed note text");
        let mut input = TextInput::local();
        input.bind_text(text.clone());
        input.sync(&doc, &automerge::ROOT);
        input.set_focus(true);
        input.set_cursor_pos("before".len());

        assert!(drive(
            &mut input,
            &mut doc,
            &char_key("!"),
            mods(false, false)
        ));
        assert_eq!(input.value(), "before!", "optimistic text remains visible");
        assert_eq!(doc.text(&text).expect("document text"), "before");
    }

    // Case `action_binding_owns_accessibility_set_value`.
    {
        let mut doc = AutoCommit::new();
        let object = doc
            .put_object(&automerge::ROOT, "legacy", ObjType::Text)
            .unwrap();
        let changes = std::rc::Rc::new(std::cell::RefCell::new(Vec::<String>::new()));
        let captured = std::rc::Rc::clone(&changes);
        let mut input = TextInput::local()
            .on_change(move |value| captured.borrow_mut().push(value.to_owned()))
            .on_action(crate::widget::test_string_action_binding());
        input.bind_text(object.clone());
        crate::widget::controls::refresh_control_action_availability(
            &mut input,
            &std::collections::HashSet::new(),
        );
        assert!(input.is_disabled());
        crate::widget::controls::refresh_control_action_availability(
            &mut input,
            &std::collections::HashSet::from([tilo_actions::ActionAddress::new(
                "test:control",
                "set",
            )]),
        );
        assert!(!input.is_disabled());
        assert!(Widget::perform_action(
            &mut input,
            &crate::a11y::Action::SetValue("Project Atlas".into()),
            &[],
            &mut crate::widget::Edits::lend(&mut doc),
            &automerge::ROOT
        ));
        assert_eq!(doc.text(&object).unwrap(), "");
        assert_eq!(
            crate::widget::controls::take_control_invocation(&mut input)
                .unwrap()
                .arguments["value"],
            "Project Atlas"
        );
        assert_eq!(
            *changes.borrow(),
            ["Project Atlas"],
            "the local callback runs beside the bound action"
        );
        doc.splice_text(&object, 0, 0, "Remote title").unwrap();
        Widget::sync(&mut input, &doc, &automerge::ROOT);
        assert_eq!(input.value(), "Remote title");
        assert_eq!(changes.borrow().len(), 1, "remote text is not a user edit");
    }

    // Case `change_callback_and_action_both_fire_in_either_order`.
    {
        let mut doc = AutoCommit::new();
        let changes = std::rc::Rc::new(std::cell::RefCell::new(Vec::<String>::new()));
        let captured = std::rc::Rc::clone(&changes);
        let mut input = TextInput::local()
            .on_action(crate::widget::test_string_action_binding())
            .on_change(move |value| captured.borrow_mut().push(value.to_owned()));
        crate::widget::controls::refresh_control_action_availability(
            &mut input,
            &std::collections::HashSet::from([tilo_actions::ActionAddress::new(
                "test:control",
                "set",
            )]),
        );
        assert!(Widget::perform_action(
            &mut input,
            &crate::a11y::Action::SetValue("Atlas".into()),
            &[],
            &mut crate::widget::Edits::lend(&mut doc),
            &automerge::ROOT
        ));
        assert_eq!(
            crate::widget::controls::take_control_invocation(&mut input)
                .unwrap()
                .arguments["value"],
            "Atlas"
        );
        assert_eq!(*changes.borrow(), ["Atlas"]);
    }
}

/// Unfocused, so `scroll_x` stays 0 and click math is simple.
fn draw_to_populate(ti: &mut TextInput) {
    draw_with_theme(ti, &Theme::dark());
}

fn draw_with_theme(ti: &mut TextInput, theme: &Theme) {
    let presence = crate::presence::PresenceCtx::empty();
    let mut tr = TextRenderer::new();
    tr.set_text_scale(theme.text_scale);
    let mut scene = Scene::new();
    Widget::draw(ti, &mut scene, Affine::IDENTITY, &mut tr, theme, &presence);
}

#[test]
fn multiline_measurement_matches_draw_scale_and_invalidation() {
    // Motion reads at its end: this test checks the resting frame.
    let _settled = crate::motion::testing::settled();
    // Case `multiline_shaped_editor_scales_font_and_line_metrics_with_theme`.
    {
        let mut normal = TextInput::local()
            .multiline(true)
            .with_text_role(TextRole::Caption)
            .with_initial_value("A durable comment body that wraps onto another row.");
        normal.apply_layout_external(Rect::new(0.0, 0.0, 260.0, 160.0));
        draw_with_theme(&mut normal, &Theme::dark());
        let normal_layout = normal.single_layout.as_ref().expect("normal shaped layout");
        let normal_height = normal_layout.height();
        let (_, _, normal_line_h) = normal_layout.caret_geometry_at(TextPosition::new(0));

        let mut large = TextInput::local()
            .multiline(true)
            .with_text_role(TextRole::Caption)
            .with_initial_value("A durable comment body that wraps onto another row.");
        large.apply_layout_external(Rect::new(0.0, 0.0, 260.0, 160.0));
        let large_theme = Theme::dark().with_text_scale(2.0);
        draw_with_theme(&mut large, &large_theme);
        let large_layout = large.single_layout.as_ref().expect("large shaped layout");
        let large_height = large_layout.height();
        let (_, _, large_line_h) = large_layout.caret_geometry_at(TextPosition::new(0));

        assert!(
            large_line_h >= normal_line_h * 1.9,
            "editor line metric scales with the theme ({large_line_h} vs {normal_line_h})"
        );
        assert!(
            large_height >= normal_height * 1.9,
            "editor paragraph geometry scales with the theme ({large_height} vs {normal_height})"
        );
    }

    // Case `multiline_measure_uses_the_same_scaled_shaped_rows_as_draw`.
    {
        let text = "A long wrapping line whose measured height must match the caret rows produced by the shaped draw path at large text.";
        let mut input = TextInput::local()
            .multiline(true)
            .with_text_role(TextRole::Body)
            .with_initial_value(text);
        let mut renderer = TextRenderer::new();
        renderer.set_text_scale(2.0);
        let measured = Widget::measure(
            &mut input,
            taffy::Size {
                width: Some(180.0),
                height: None,
            },
            taffy::Size {
                width: taffy::AvailableSpace::Definite(180.0),
                height: taffy::AvailableSpace::MaxContent,
            },
            &mut renderer,
        );

        input.apply_layout_external(Rect::new(0.0, 0.0, 180.0, f64::from(measured.height)));
        let theme = Theme::dark().with_text_scale(2.0);
        draw_with_theme(&mut input, &theme);
        let layout = input
            .single_layout
            .as_ref()
            .expect("shaped multiline layout");
        let expected_height = layout.height() + (ML_PAD_Y * 2.0) as f32;
        assert!(
            (measured.height - expected_height).abs() < 0.1,
            "measure {measured:?} must use drawn shaped height {expected_height}"
        );
        let last_row = layout.visual_line_index(input.text_to_layout_byte(text.len()));
        let caret = input.caret_rect(&theme, text.len());
        assert_eq!(
            (caret.y0 / caret.height()).floor() as usize,
            last_row,
            "caret row and shaped visual row index agree"
        );
    }

    // Case `multiline_trailing_lf_keeps_measure_draw_hit_and_caret_scroll_in_sync`.
    {
        let text = "let value = 1;\n";
        for scale in [1.0, 2.0] {
            let theme = Theme::dark().with_text_scale(scale);
            let mut input = TextInput::local()
                .multiline(true)
                .with_mono()
                .with_gutter(true)
                .with_current_line_highlight(true)
                .with_initial_value(text);
            input.set_highlighter(crate::widget::code_editor::Language::Rust.highlighter());
            input.set_focus(true);
            input.set_cursor_pos(text.len());

            let mut renderer = TextRenderer::new();
            renderer.set_text_scale(theme.text_scale);
            let measured = Widget::measure(
                &mut input,
                taffy::Size {
                    width: Some(240.0),
                    height: None,
                },
                taffy::Size {
                    width: taffy::AvailableSpace::Definite(240.0),
                    height: taffy::AvailableSpace::MaxContent,
                },
                &mut renderer,
            );
            let viewport_height =
                f64::from(input.multiline_style(scale).line_height_px()) + ML_PAD_Y * 2.0;
            input.apply_layout_external(Rect::new(0.0, 0.0, 240.0, viewport_height));
            draw_with_theme(&mut input, &theme);

            let drawn_height = input.content_height() as f32;
            assert!(
                measured.height >= drawn_height && measured.height - drawn_height < 1.0,
                "pixel-ceiled measure {measured:?} agrees with attributed draw height {drawn_height} at {scale}x"
            );
            let layout = input
                .single_layout
                .as_ref()
                .expect("draw retained attributed multiline layout");
            let (_, eof_top, eof_height) =
                layout.caret_geometry(input.text_to_layout_byte(text.len()));
            assert_eq!(
                layout.visual_line_index(text.len()).saturating_add(1),
                layout.buffer.layout_runs().count(),
                "EOF stays on the final visual row even when large text wraps"
            );
            assert_eq!(layout.logical_line_origins().len(), 2);
            assert!(
                (eof_top + eof_height - layout.height()).abs() < 0.01,
                "EOF caret reaches the bottom of the final row"
            );

            let caret = input.caret_rect(&theme, text.len());
            assert_eq!(
                input.byte_at_point(Point::new(caret.x0 + 1.0, caret.center().y)),
                text.len(),
                "clicking the final empty row returns EOF"
            );
            let view_height = input.rect.height() - ML_PAD_Y * 2.0;
            let expected_scroll = (f64::from(layout.height()) - view_height).max(0.0);
            assert!(expected_scroll > 0.0);
            assert!(
                (input.scroll_y() - expected_scroll).abs() < 0.01,
                "caret-follow scroll remains at the EOF row"
            );
            assert!(input.gutter_width() > 0.0);
            assert!(
                input
                    .code_row_colors(&theme)
                    .iter()
                    .any(|(_, color)| *color != theme.text_primary),
                "syntax attribution survives terminal-row normalization"
            );
        }
    }

    // Case `multiline_measure_uses_build_layout_theme_scale_when_renderer_is_stale`.
    {
        let text = "The layout theme owns accessibility scale even when a retained renderer still carries its previous frame value.";
        let mut input = TextInput::local().multiline(true).with_initial_value(text);
        let large = Theme::dark().with_text_scale(2.0);
        let mut taffy = TaffyTree::<()>::new();
        let mut node_map = std::collections::HashMap::new();
        let _ = Widget::build_layout(&mut input, &mut taffy, &large, &mut node_map);

        let mut renderer = TextRenderer::new();
        assert_eq!(renderer.text_scale(), 1.0, "stale renderer precondition");
        let measured_large = Widget::measure(
            &mut input,
            taffy::Size {
                width: Some(180.0),
                height: None,
            },
            taffy::Size {
                width: taffy::AvailableSpace::Definite(180.0),
                height: taffy::AvailableSpace::MaxContent,
            },
            &mut renderer,
        );
        input.apply_layout_external(Rect::new(0.0, 0.0, 180.0, f64::from(measured_large.height)));
        draw_with_theme(&mut input, &large);
        let drawn_large = input.content_height().ceil() as f32;
        assert!((measured_large.height - drawn_large).abs() < 0.1);

        let normal = Theme::dark();
        let mut taffy = TaffyTree::<()>::new();
        let mut node_map = std::collections::HashMap::new();
        let _ = Widget::build_layout(&mut input, &mut taffy, &normal, &mut node_map);
        let measured_normal = Widget::measure(
            &mut input,
            taffy::Size {
                width: Some(180.0),
                height: None,
            },
            taffy::Size {
                width: taffy::AvailableSpace::Definite(180.0),
                height: taffy::AvailableSpace::MaxContent,
            },
            &mut renderer,
        );
        input.apply_layout_external(Rect::new(
            0.0,
            0.0,
            180.0,
            f64::from(measured_normal.height),
        ));
        draw_with_theme(&mut input, &normal);
        assert!((measured_normal.height - input.content_height().ceil() as f32).abs() < 0.1);
        assert!(
            measured_large.height > measured_normal.height * 1.8,
            "same instance remeasures from large text back to normal"
        );
    }

    // Case `measured_multiline_pointer_geometry_tracks_applied_origin_before_draw`.
    {
        let text = "first wrapped row continues for a while before the second hard line\nsecond";
        let mut input = TextInput::local().multiline(true).with_initial_value(text);
        let mut renderer = TextRenderer::new();
        renderer.set_text_scale(2.0);
        let measured = Widget::measure(
            &mut input,
            taffy::Size {
                width: Some(180.0),
                height: None,
            },
            taffy::Size {
                width: taffy::AvailableSpace::Definite(180.0),
                height: taffy::AvailableSpace::MaxContent,
            },
            &mut renderer,
        );
        input.apply_layout_external(Rect::new(
            100.0,
            50.0,
            280.0,
            50.0 + f64::from(measured.height),
        ));
        let second = text.find('\n').expect("hard break") + 1;
        let (x, top, height) = input
            .single_layout
            .as_ref()
            .expect("measurement retained shaped rows")
            .caret_geometry(input.text_to_layout_byte(second));
        let point = Point::new(
            input.rect.x0 + input.ml_left() + f64::from(x) + 1.0,
            input.rect.y0 + ML_PAD_Y + f64::from(top + height / 2.0),
        );
        assert_eq!(
            input.byte_at_point(point),
            second,
            "pointer uses the applied widget origin before the first draw"
        );
    }

    // Case `crdt_sync_undo_redo_invalidates_multiline_measurement`.
    {
        let short = "short";
        let long = "undo and redo replay real CRDT text through the same widget, including a long line that wraps at large text";
        let theme = Theme::dark().with_text_scale(2.0);
        let mut state = crate::State::new();
        let text_obj = state
            .doc
            .put_object(&state.root, "body", ObjType::Text)
            .expect("create document text");
        state
            .doc
            .splice_text(&text_obj, 0, 0, short)
            .expect("seed document text");
        state.commit_action(None, None);

        let mut input = TextInput::local().multiline(true);
        input.bind_text(text_obj.clone());
        Widget::sync(&mut input, &state.doc, &state.root);
        let mut renderer = TextRenderer::new();
        let mut taffy = TaffyTree::<()>::new();
        let mut node_map = std::collections::HashMap::new();
        let _ = Widget::build_layout(&mut input, &mut taffy, &theme, &mut node_map);
        let measure = |input: &mut TextInput, renderer: &mut TextRenderer| {
            Widget::measure(
                input,
                taffy::Size {
                    width: Some(180.0),
                    height: None,
                },
                taffy::Size {
                    width: taffy::AvailableSpace::Definite(180.0),
                    height: taffy::AvailableSpace::MaxContent,
                },
                renderer,
            )
            .height
        };
        let short_height = measure(&mut input, &mut renderer);
        input.apply_layout_external(Rect::new(0.0, 0.0, 180.0, f64::from(short_height)));
        assert!(!Widget::layout_needs_measure(&input));

        let old_len = state.doc.length(&text_obj);
        state
            .doc
            .splice_text(
                &text_obj,
                0,
                isize::try_from(old_len).expect("test text length fits isize"),
                long,
            )
            .expect("replace with wrapping text");
        state.commit_action(None, None);
        Widget::sync(&mut input, &state.doc, &state.root);
        assert_eq!(input.take_dirty(), None, "model sync is not a user edit");
        assert!(
            Widget::layout_needs_measure(&input),
            "CRDT text growth invalidates retained measurement"
        );
        let long_height = measure(&mut input, &mut renderer);
        assert!(long_height > short_height * 1.8, "long text wraps at 2x");

        state.undo(None, None).expect("undo long text");
        Widget::sync(&mut input, &state.doc, &state.root);
        assert!(Widget::layout_needs_measure(&input));
        let undo_height = measure(&mut input, &mut renderer);
        assert!((undo_height - short_height).abs() < 0.1);

        state.redo(None, None).expect("redo long text");
        Widget::sync(&mut input, &state.doc, &state.root);
        assert!(Widget::layout_needs_measure(&input));
        let redo_height = measure(&mut input, &mut renderer);
        assert!((redo_height - long_height).abs() < 0.1);
    }
}

/// Mirrors the handler's own `padding_x = 10.0`; assumes `scroll_x == 0`.
fn x_for_byte(ti: &TextInput, b: usize) -> f64 {
    ti.rect.x0 + 10.0 + ti.char_x_positions()[b]
}

#[test]
fn arrow_keys_move_and_extend_by_char_and_word() {
    // Motion reads at its end: this test checks the resting frame.
    let _settled = crate::motion::testing::settled();
    // Case `shift_arrow_extends_selection_by_char`.
    {
        let mut ti = TextInput::local().with_initial_value("abcdef");
        ti.set_focus(true);
        ti.set_cursor_pos(3);
        let mut doc = AutoCommit::new();

        assert!(drive(
            &mut ti,
            &mut doc,
            &named_key(NamedKey::ArrowRight),
            mods(false, true)
        ));
        assert_eq!(
            ti.selection_range(),
            Some((3, 4)),
            "shift+Right extends by one char"
        );
        drive(
            &mut ti,
            &mut doc,
            &named_key(NamedKey::ArrowRight),
            mods(false, true),
        );
        assert_eq!(ti.selection_range(), Some((3, 5)));
        drive(
            &mut ti,
            &mut doc,
            &named_key(NamedKey::ArrowLeft),
            mods(false, true),
        );
        assert_eq!(
            ti.selection_range(),
            Some((3, 4)),
            "shift+Left shrinks it back"
        );
    }

    // Case `ctrl_arrow_moves_caret_by_word`.
    {
        let mut ti = TextInput::local().with_initial_value("hello world foo");
        ti.set_focus(true);
        ti.set_cursor_pos(0);
        let mut doc = AutoCommit::new();

        drive(
            &mut ti,
            &mut doc,
            &named_key(NamedKey::ArrowRight),
            mods(true, false),
        );
        assert_eq!(
            ti.cursor_pos(),
            6,
            "ctrl+Right jumps to start of the next word"
        );
        assert_eq!(ti.selection_range(), None, "no shift → no selection");
        drive(
            &mut ti,
            &mut doc,
            &named_key(NamedKey::ArrowRight),
            mods(true, false),
        );
        assert_eq!(ti.cursor_pos(), 12, "ctrl+Right again → start of 'foo'");
        drive(
            &mut ti,
            &mut doc,
            &named_key(NamedKey::ArrowLeft),
            mods(true, false),
        );
        assert_eq!(ti.cursor_pos(), 6, "ctrl+Left walks back a word");
    }

    // Case `ctrl_arrow_collapses_a_selection_before_moving_by_word`.
    {
        let mut ti = TextInput::local().with_initial_value("hello world foo");
        ti.set_focus(true);
        let mut doc = AutoCommit::new();

        ti.set_selection(3, 12);
        drive(
            &mut ti,
            &mut doc,
            &named_key(NamedKey::ArrowLeft),
            mods(true, false),
        );
        assert_eq!(
            ti.cursor_pos(),
            3,
            "Ctrl+Left collapses to the selection start"
        );
        assert_eq!(ti.selection_range(), None);

        ti.set_selection(3, 12);
        drive(
            &mut ti,
            &mut doc,
            &named_key(NamedKey::ArrowRight),
            mods(true, false),
        );
        assert_eq!(
            ti.cursor_pos(),
            12,
            "Ctrl+Right collapses to the selection end"
        );
        assert_eq!(ti.selection_range(), None);
    }

    // Case `shift_ctrl_arrow_extends_selection_by_word`.
    {
        let mut ti = TextInput::local().with_initial_value("hello world foo");
        ti.set_focus(true);
        ti.set_cursor_pos(0);
        let mut doc = AutoCommit::new();

        drive(
            &mut ti,
            &mut doc,
            &named_key(NamedKey::ArrowRight),
            mods(true, true),
        );
        assert_eq!(
            ti.selection_range(),
            Some((0, 6)),
            "shift+ctrl+Right selects the first word span"
        );
        drive(
            &mut ti,
            &mut doc,
            &named_key(NamedKey::ArrowRight),
            mods(true, true),
        );
        assert_eq!(
            ti.selection_range(),
            Some((0, 12)),
            "extends by another word"
        );
    }
}

#[test]
fn home_end_and_select_all_shortcuts_move_extend_and_cut() {
    // Motion reads at its end: this test checks the resting frame.
    let _settled = crate::motion::testing::settled();
    // Case `home_end_move_and_extend`.
    {
        let mut ti = TextInput::local().with_initial_value("hello world");
        ti.set_focus(true);
        ti.set_cursor_pos(5);
        let mut doc = AutoCommit::new();

        drive(
            &mut ti,
            &mut doc,
            &named_key(NamedKey::End),
            mods(false, false),
        );
        assert_eq!(ti.cursor_pos(), 11, "End → end of field");
        assert_eq!(ti.selection_range(), None);
        drive(
            &mut ti,
            &mut doc,
            &named_key(NamedKey::Home),
            mods(false, false),
        );
        assert_eq!(ti.cursor_pos(), 0, "Home → start of field");

        drive(
            &mut ti,
            &mut doc,
            &named_key(NamedKey::End),
            mods(false, true),
        );
        assert_eq!(
            ti.selection_range(),
            Some((0, 11)),
            "shift+End selects to line end"
        );
        ti.set_cursor_pos(11);
        drive(
            &mut ti,
            &mut doc,
            &named_key(NamedKey::Home),
            mods(false, true),
        );
        assert_eq!(
            ti.selection_range(),
            Some((0, 11)),
            "shift+Home selects to line start"
        );
    }

    // Case `ctrl_home_end_move_and_extend_across_multiline_document`.
    {
        let mut ti = ml_input("first\nsecond\nthird", 400.0, 200.0);
        let mut doc = AutoCommit::new();
        ti.set_cursor_pos(8); // middle of the second visual line

        drive(
            &mut ti,
            &mut doc,
            &named_key(NamedKey::Home),
            mods(true, false),
        );
        assert_eq!(ti.cursor_pos(), 0, "Ctrl+Home reaches document start");

        drive(
            &mut ti,
            &mut doc,
            &named_key(NamedKey::End),
            mods(true, true),
        );
        assert_eq!(
            ti.selection_range(),
            Some((0, ti.value().len())),
            "Ctrl+Shift+End selects through the whole document"
        );

        ti.set_cursor_pos(8);
        drive(
            &mut ti,
            &mut doc,
            &named_key(NamedKey::Home),
            mods(true, true),
        );
        assert_eq!(
            ti.selection_range(),
            Some((0, 8)),
            "Ctrl+Shift+Home selects back to document start"
        );
    }

    // Case `ctrl_a_selects_all_and_ctrl_x_cuts`.
    {
        let mut ti = TextInput::local().with_initial_value("hello world");
        ti.set_focus(true);
        let mut doc = AutoCommit::new();

        drive(&mut ti, &mut doc, &char_key("a"), mods(true, false));
        assert_eq!(
            ti.selection_range(),
            Some((0, 11)),
            "ctrl+A selects everything"
        );

        drive(&mut ti, &mut doc, &char_key("x"), mods(true, false));
        assert_eq!(ti.value(), "", "ctrl+X cuts the selection");
        assert_eq!(ti.selection_range(), None, "selection collapsed after cut");
    }

    // Case `command_a_selects_all_on_platforms_using_super`.
    {
        let mut ti = TextInput::local().with_initial_value("hello 👋");
        ti.set_focus(true);
        let mut doc = AutoCommit::new();

        drive(&mut ti, &mut doc, &char_key("a"), command_mods());
        assert_eq!(ti.selection_range(), Some((0, ti.value().len())));
    }
}

#[test]
fn pointer_clicks_and_drags_select_word_range_and_all() {
    // Motion reads at its end: this test checks the resting frame.
    let _settled = crate::motion::testing::settled();
    // Case `double_click_selects_word_via_handle_event`.
    {
        let mut ti = TextInput::local().with_initial_value("hello world");
        ti.apply_layout_external(Rect::new(0.0, 0.0, 300.0, 40.0));
        draw_to_populate(&mut ti);
        let mut doc = AutoCommit::new();

        let bx = x_for_byte(&ti, 8); // inside "world" (bytes 6..11)
        drive(
            &mut ti,
            &mut doc,
            &cursor_moved(bx, 20.0),
            mods(false, false),
        );
        drive(&mut ti, &mut doc, &mouse_press(), mods(false, false));
        drive(&mut ti, &mut doc, &mouse_release(), mods(false, false));
        // Within the multi-click interval → double click.
        drive(&mut ti, &mut doc, &mouse_press(), mods(false, false));

        assert_eq!(
            ti.selection_range(),
            Some((6, 11)),
            "double-click selected the word"
        );
        assert_eq!(ti.selected_str(), Some("world"));
    }

    // Case `drag_selects_range_via_handle_event`.
    {
        let mut ti = TextInput::local().with_initial_value("hello world");
        ti.apply_layout_external(Rect::new(0.0, 0.0, 300.0, 40.0));
        draw_to_populate(&mut ti);
        let mut doc = AutoCommit::new();

        let x_start = x_for_byte(&ti, 2);
        let x_end = x_for_byte(&ti, 9);
        drive(
            &mut ti,
            &mut doc,
            &cursor_moved(x_start, 20.0),
            mods(false, false),
        );
        drive(&mut ti, &mut doc, &mouse_press(), mods(false, false));
        assert!(ti.is_dragging(), "press starts a drag");
        drive(
            &mut ti,
            &mut doc,
            &cursor_moved(x_end, 20.0),
            mods(false, false),
        );
        assert_eq!(
            ti.selection_range(),
            Some((2, 9)),
            "drag selected the swept range"
        );
        drive(&mut ti, &mut doc, &mouse_release(), mods(false, false));
        assert!(!ti.is_dragging(), "release ends the drag");
    }

    // Case `triple_click_selects_all_via_handle_event`.
    {
        let mut ti = TextInput::local().with_initial_value("hello world");
        ti.apply_layout_external(Rect::new(0.0, 0.0, 300.0, 40.0));
        draw_to_populate(&mut ti);
        let mut doc = AutoCommit::new();

        let bx = x_for_byte(&ti, 8);
        drive(
            &mut ti,
            &mut doc,
            &cursor_moved(bx, 20.0),
            mods(false, false),
        );
        drive(&mut ti, &mut doc, &mouse_press(), mods(false, false));
        drive(&mut ti, &mut doc, &mouse_press(), mods(false, false));
        drive(&mut ti, &mut doc, &mouse_press(), mods(false, false));
        assert_eq!(
            ti.selection_range(),
            Some((0, 11)),
            "triple-click selects the whole field"
        );
    }
}

#[test]
fn primary_selection_publishes_pointer_selections_and_middle_click_pastes() {
    // Motion reads at its end: this test checks the resting frame.
    let _settled = crate::motion::testing::settled();
    // Case `mouse_selection_is_eligible_for_primary_publication`.
    {
        let mut ti = TextInput::local().with_initial_value("hello world");
        ti.apply_layout_external(Rect::new(0.0, 0.0, 300.0, 40.0));
        draw_to_populate(&mut ti);
        let mut doc = AutoCommit::new();
        let start = x_for_byte(&ti, 0);
        let end = x_for_byte(&ti, 5);
        drive(
            &mut ti,
            &mut doc,
            &cursor_moved(start, 20.0),
            mods(false, false),
        );
        drive(&mut ti, &mut doc, &mouse_press(), mods(false, false));
        drive(
            &mut ti,
            &mut doc,
            &cursor_moved(end, 20.0),
            mods(false, false),
        );
        drive(&mut ti, &mut doc, &mouse_release(), mods(false, false));

        assert_eq!(ti.shareable_selection(), Some("hello"));
    }

    // Case `double_and_triple_click_selections_are_eligible_for_primary_publication`.
    {
        let mut ti = TextInput::local().with_initial_value("hello world");
        ti.apply_layout_external(Rect::new(0.0, 0.0, 300.0, 40.0));
        draw_to_populate(&mut ti);
        let mut doc = AutoCommit::new();
        let at = x_for_byte(&ti, 8);
        drive(
            &mut ti,
            &mut doc,
            &cursor_moved(at, 20.0),
            mods(false, false),
        );
        drive(&mut ti, &mut doc, &mouse_press(), mods(false, false));
        drive(&mut ti, &mut doc, &mouse_release(), mods(false, false));
        drive(&mut ti, &mut doc, &mouse_press(), mods(false, false));
        assert_eq!(ti.shareable_selection(), Some("world"));
        drive(&mut ti, &mut doc, &mouse_release(), mods(false, false));
        drive(&mut ti, &mut doc, &mouse_press(), mods(false, false));
        assert_eq!(ti.shareable_selection(), Some("hello world"));
    }

    // Case `middle_click_pastes_primary_at_clicked_caret_and_applies_filter`.
    {
        let mut ti = TextInput::local()
            .with_initial_value("leftright")
            .with_char_filter(|ch| !ch.is_whitespace());
        ti.apply_layout_external(Rect::new(0.0, 0.0, 300.0, 40.0));
        draw_to_populate(&mut ti);
        ti.set_selection(0, ti.value().len());
        let mut doc = AutoCommit::new();
        let at = x_for_byte(&ti, 4);
        drive(
            &mut ti,
            &mut doc,
            &cursor_moved(at, 20.0),
            mods(false, false),
        );
        let mut focus_target = None;
        let mut handled = false;
        ti.handle_middle_click_with_primary(
            Some(" P A "),
            &mut focus_target,
            test_node().1,
            &mut handled,
        );
        assert!(handled);
        assert_eq!(ti.value(), "leftPAright");
        assert_eq!(ti.cursor_pos(), 6);
        assert_eq!(ti.selection_range(), None);
    }

    // Case `secret_selection_never_publishes_but_middle_paste_is_allowed`.
    {
        let mut secret = TextInput::local()
            .with_secret(true)
            .with_initial_value("secret");
        secret.apply_layout_external(Rect::new(0.0, 0.0, 300.0, 40.0));
        draw_to_populate(&mut secret);
        let mut doc = AutoCommit::new();
        let start = x_for_byte(&secret, 0);
        let end = x_for_byte(&secret, 6);
        drive(
            &mut secret,
            &mut doc,
            &cursor_moved(start, 20.0),
            mods(false, false),
        );
        drive(&mut secret, &mut doc, &mouse_press(), mods(false, false));
        drive(
            &mut secret,
            &mut doc,
            &cursor_moved(end, 20.0),
            mods(false, false),
        );
        drive(&mut secret, &mut doc, &mouse_release(), mods(false, false));
        assert_eq!(secret.shareable_selection(), None);

        drive(
            &mut secret,
            &mut doc,
            &cursor_moved(start, 20.0),
            mods(false, false),
        );
        let mut focus_target = None;
        let mut handled = false;
        secret.handle_middle_click_with_primary(
            Some("public"),
            &mut focus_target,
            test_node().1,
            &mut handled,
        );
        assert!(handled);
        assert_eq!(secret.value(), "publicsecret");
    }
}

fn ml_input(text: &str, w: f64, h: f64) -> TextInput {
    let mut ti = TextInput::local().multiline(true).with_initial_value(text);
    ti.set_focus(true);
    ti.apply_layout_external(Rect::new(0.0, 0.0, w, h));
    draw_to_populate(&mut ti);
    ti
}

#[test]
fn multiline_ime_intent_and_paragraph_commit_preserve_the_submit_boundary() {
    // Motion reads at its end: this test checks the resting frame.
    let _settled = crate::motion::testing::settled();
    let mut input = ml_input("before old after", 400.0, 200.0).with_enter_submits(true);
    input.set_selection(7, 10);
    let intent = crate::ime::ImeWindowState::from_widget(&input).unwrap();
    assert!(intent.content.hints.multiline);
    assert_eq!(intent.content.purpose, crate::ime::InputPurpose::Text);
    let mut doc = AutoCommit::new();
    assert!(drive(
        &mut input,
        &mut doc,
        &WindowEvent::Ime(winit::event::Ime::Commit("First café.\n\nSecond 👋".into())),
        Modifiers::default(),
    ));
    assert_eq!(input.value(), "before First café.\n\nSecond 👋 after");
    assert!(!input.take_submit());
    input.set_selection(7, 7 + "First café.\n\nSecond 👋".len());
    assert!(drive(
        &mut input,
        &mut doc,
        &WindowEvent::Ime(winit::event::Ime::Commit("\n\n".into())),
        Modifiers::default(),
    ));
    assert_eq!(input.value(), "before \n\n after");
    assert!(!input.take_submit());
    input.set_multiline(false);
    assert!(
        !crate::ime::ImeWindowState::from_widget(&input)
            .unwrap()
            .content
            .hints
            .multiline
    );
    input.set_focus(false);
    assert!(crate::ime::ImeWindowState::from_widget(&input).is_none());
    let mut secret = TextInput::local().multiline(true).with_secret(true);
    secret.set_focus(true);
    let secret = crate::ime::ImeWindowState::from_widget(&secret).unwrap();
    assert_eq!(secret.content, crate::ime::InputContent::PASSWORD);
}

fn shaped_row(ti: &TextInput, byte: usize) -> usize {
    ti.single_layout
        .as_ref()
        .expect("multiline input retained its shaped paragraph")
        .visual_line_index(ti.text_to_layout_byte(byte))
}

#[test]
fn multiline_enter_backspace_and_copy_handle_newlines() {
    // Motion reads at its end: this test checks the resting frame.
    let _settled = crate::motion::testing::settled();
    // Case `multiline_enter_inserts_newline`.
    {
        let mut ti = ml_input("ab", 300.0, 120.0);
        ti.set_cursor_pos(1); // between 'a' and 'b'
        let mut doc = AutoCommit::new();
        assert!(drive(
            &mut ti,
            &mut doc,
            &named_key(NamedKey::Enter),
            mods(false, false)
        ));
        assert_eq!(ti.value(), "a\nb", "Enter inserted a newline at the caret");
        assert_eq!(
            ti.cursor_pos(),
            2,
            "caret sits just after the inserted newline"
        );
    }

    // Case `enter_submits_signals_without_inserting_and_shift_enter_still_splits`.
    {
        let mut ti = TextInput::local()
            .with_enter_submits(true)
            .with_initial_value("hi");
        ti.set_focus(true);
        let mut doc = AutoCommit::new();
        assert!(drive(
            &mut ti,
            &mut doc,
            &named_key(NamedKey::Enter),
            mods(false, false)
        ));
        assert_eq!(
            ti.value(),
            "hi",
            "Enter must not insert a newline in submit mode"
        );
        assert!(ti.take_submit(), "Enter fired the submit signal");
        assert!(!ti.take_submit(), "submit is one-shot — cleared on read");

        let mut ml = TextInput::local()
            .multiline(true)
            .with_enter_submits(true)
            .with_initial_value("hi");
        ml.set_focus(true);
        ml.set_cursor_pos(2);
        drive(
            &mut ml,
            &mut doc,
            &named_key(NamedKey::Enter),
            mods(false, true),
        ); // Shift+Enter
        assert_eq!(
            ml.value(),
            "hi\n",
            "Shift+Enter inserts a newline in multiline submit mode"
        );
        assert!(!ml.take_submit(), "Shift+Enter must not submit");
    }

    // Case `multiline_backspace_at_line_start_merges_lines`.
    // The newline is just a byte, so the shared delete path handles it.
    {
        let mut ti = ml_input("hello\nworld", 400.0, 200.0);
        let mut doc = AutoCommit::new();
        ti.set_cursor_pos(6); // start of "world", just after the newline
        drive(
            &mut ti,
            &mut doc,
            &named_key(NamedKey::Backspace),
            mods(false, false),
        );
        assert_eq!(
            ti.value(),
            "helloworld",
            "Backspace removed the newline, merging the lines"
        );
        assert_eq!(ti.cursor_pos(), 5);
    }

    // Case `multiline_selection_spans_lines_and_copy_includes_newline`.
    {
        let mut ti = ml_input("hello\nworld", 400.0, 200.0);
        let mut doc = AutoCommit::new();
        ti.set_cursor_pos(3); // inside "hello"
        drive(
            &mut ti,
            &mut doc,
            &named_key(NamedKey::ArrowDown),
            mods(false, true),
        ); // Shift+Down
        let (s, e) = ti
            .selection_range_public()
            .expect("a cross-line selection is active");
        assert!(
            s <= 5 && e > 6,
            "selection crosses the newline at byte 5, got ({s},{e})"
        );
        let selected = ti.selected_str().expect("selected slice");
        assert!(
            selected.contains('\n'),
            "copy would emit the newline: {selected:?}"
        );
    }
}

#[test]
fn multiline_vertical_and_line_edge_navigation_follows_visual_lines() {
    // Motion reads at its end: this test checks the resting frame.
    let _settled = crate::motion::testing::settled();
    // Case `multiline_up_down_moves_between_visual_lines`.
    {
        let mut ti = ml_input("hello\nworld\nfoo", 400.0, 200.0);
        let mut doc = AutoCommit::new();
        ti.set_cursor_pos(8); // inside "world" (row 1)
        assert_eq!(shaped_row(&ti, ti.cursor_pos()), 1, "precondition: row 1");

        drive(
            &mut ti,
            &mut doc,
            &named_key(NamedKey::ArrowUp),
            mods(false, false),
        );
        assert_eq!(shaped_row(&ti, ti.cursor_pos()), 0, "Up moved to row 0");

        drive(
            &mut ti,
            &mut doc,
            &named_key(NamedKey::ArrowDown),
            mods(false, false),
        );
        assert_eq!(
            shaped_row(&ti, ti.cursor_pos()),
            1,
            "Down returned to row 1"
        );
        drive(
            &mut ti,
            &mut doc,
            &named_key(NamedKey::ArrowDown),
            mods(false, false),
        );
        assert_eq!(shaped_row(&ti, ti.cursor_pos()), 2, "Down reached row 2");
    }

    // Case `multiline_vertical_motion_preserves_goal_column`.
    {
        let mut ti = ml_input("abcdefgh\nx\nabcdefgh", 500.0, 200.0);
        let mut doc = AutoCommit::new();
        ti.set_cursor_pos(8); // end of the first "abcdefgh" (row 0, high column)

        drive(
            &mut ti,
            &mut doc,
            &named_key(NamedKey::ArrowDown),
            mods(false, false),
        );
        assert_eq!(
            shaped_row(&ti, ti.cursor_pos()),
            1,
            "Down reached short row"
        );

        drive(
            &mut ti,
            &mut doc,
            &named_key(NamedKey::ArrowDown),
            mods(false, false),
        );
        let row = shaped_row(&ti, ti.cursor_pos());
        assert_eq!(row, 2, "Down reached the third row");
        assert!(
            ti.cursor_pos() >= 17,
            "goal column preserved across the short line — caret is near row 3's end, got {}",
            ti.cursor_pos()
        );
    }

    // Case `multiline_home_end_are_per_visual_line`.
    {
        let mut ti = ml_input("hello\nworld", 400.0, 200.0);
        let mut doc = AutoCommit::new();
        ti.set_cursor_pos(8); // "wo|rld" on row 1
        drive(
            &mut ti,
            &mut doc,
            &named_key(NamedKey::Home),
            mods(false, false),
        );
        assert_eq!(
            ti.cursor_pos(),
            6,
            "Home went to the start of the second line, not the field"
        );
        drive(
            &mut ti,
            &mut doc,
            &named_key(NamedKey::End),
            mods(false, false),
        );
        assert_eq!(
            ti.cursor_pos(),
            11,
            "End went to the end of the second line"
        );
    }
}

#[test]
fn multiline_wraps_at_unicode_line_breaks() {
    // Motion reads at its end: this test checks the resting frame.
    let _settled = crate::motion::testing::settled();
    // Case `multiline_soft_wraps_at_word_boundaries`.
    {
        let text = "the quick brown fox jumps over the lazy dog again and again";
        let mut ti = ml_input(text, 170.0, 220.0); // narrow → must wrap
        let layout = ti.single_layout.as_ref().expect("paragraph was shaped");
        let runs: Vec<_> = layout.buffer.layout_runs().collect();
        assert!(runs.len() > 1, "long paragraph wrapped into visual rows");
        let first_end = runs[0]
            .glyphs
            .iter()
            .map(|glyph| glyph.end)
            .max()
            .unwrap_or(0);
        assert!(
            ti.value()
                .get(first_end..)
                .is_some_and(|s| s.starts_with(' ')),
            "wrap happened at a word boundary (the break precedes whitespace): {:?}",
            ti.value().get(..first_end)
        );

        let mut doc = AutoCommit::new();
        ti.set_cursor_pos(0);
        drive(
            &mut ti,
            &mut doc,
            &named_key(NamedKey::ArrowDown),
            mods(false, false),
        );
        assert_eq!(shaped_row(&ti, ti.cursor_pos()), 1, "Down entered row 1");
        assert!(
            ti.cursor_pos() > 0,
            "caret advanced into the second visual row"
        );
    }

    // Case `multiline_row_geometry_uses_unicode_cjk_breaks`.
    {
        let mut ti = TextInput::local()
            .multiline(true)
            .with_initial_value("東京京都大阪名古屋");
        ti.apply_layout_external(Rect::new(0.0, 0.0, 70.0, 220.0));
        let mut renderer = TextRenderer::new();
        ti.build_lines(&mut renderer, 50.0);
        assert!(ti.lines.len() > 1, "narrow CJK input creates visual rows");
        let opportunities: Vec<_> = line_break_opportunities(ti.value())
            .map(|(byte, _)| byte)
            .collect();
        for row in ti.lines.iter().filter(|row| !row.hard_break) {
            assert!(
                opportunities.contains(&row.end),
                "soft row end {} is a UAX #14 opportunity: {opportunities:?}",
                row.end
            );
        }
    }
}

#[test]
fn clearing_direct_binding_preserves_interaction_and_cannot_write_stale_object() {
    // Motion reads at its end: this test checks the resting frame.
    let _settled = crate::motion::testing::settled();
    let mut doc = AutoCommit::new();
    let stale = doc
        .put_object(&automerge::ROOT, "stale", ObjType::Text)
        .expect("stale text");
    doc.splice_text(&stale, 0, 0, "draft")
        .expect("seed stale text");
    let mut input = TextInput::local();
    input.bind_text(stale.clone());
    input.sync(&doc, &automerge::ROOT);
    input.set_focus(true);
    input.core.set_selection(1, 4);
    input.core.set_preedit("編", Some((0, "編".len())));
    input.scroll_x = 17.0;
    input.scroll_y = 23.0;

    let heads = doc.get_heads();
    input.clear_text_binding();
    assert_eq!(
        doc.get_heads(),
        heads,
        "detach does not mutate the document"
    );
    assert_eq!(input.bound_obj(), None);
    assert_eq!(input.value(), "draft");
    assert!(input.is_focused);
    assert_eq!(input.core.caret(), 4);
    assert_eq!(input.core.anchor(), 1);
    assert_eq!(input.core.preedit(), "編");
    assert_eq!(input.core.preedit_cursor(), Some((0, "編".len())));
    assert_eq!(input.scroll_x, 17.0);
    assert_eq!(input.scroll_y, 23.0);

    doc.splice_text(&stale, 0, 5, "historical")
        .expect("replace stale text");
    input.sync(&doc, &automerge::ROOT);
    assert_eq!(input.value(), "draft", "sync ignores the stale object");

    input.core.clear_preedit();
    input.core.set_caret(input.value().len());
    assert!(drive(
        &mut input,
        &mut doc,
        &char_key("!"),
        mods(false, false)
    ));
    assert_eq!(input.value(), "draft!");
    assert_eq!(doc.text(&stale).expect("stale text"), "historical");
}

#[test]
fn clearing_without_a_binding_is_a_noop_for_local_users() {
    // Motion reads at its end: this test checks the resting frame.
    let _settled = crate::motion::testing::settled();
    let mut doc = AutoCommit::new();
    let state = crate::LocalState::new(String::from("local"));
    let mut local_input = TextInput::from_state(&state);
    local_input.clear_text_binding();
    assert!(Widget::perform_action(
        &mut local_input,
        &crate::a11y::Action::SetValue("updated".into()),
        &[],
        &mut crate::widget::Edits::lend(&mut doc),
        &automerge::ROOT,
    ));
    assert_eq!(state.get(), "updated");
}

#[test]
fn controlled_local_input_opt_outs_survive_draw_without_dictionary_work() {
    // Motion reads at its end: this test checks the resting frame.
    let _settled = crate::motion::testing::settled();
    let mut input = TextInput::local_with_language("en_US")
        .with_spellcheck(false)
        .with_autocorrect(false)
        .with_initial_value("teh mispeled words");
    input.set_focus(true);
    input.rect = Rect::new(0.0, 0.0, 360.0, 42.0);
    let mut scene = Scene::new();
    let mut renderer = crate::font_catalog::fixture_renderer();
    input.draw(
        &mut scene,
        Affine::IDENTITY,
        &mut renderer,
        &Theme::dark(),
        &crate::PresenceCtx::empty(),
    );
    assert!(input.is_focused);
    assert!(!input.text.is_empty());
    assert!(input.rect.width() > 0.0);
    assert!(input.spell_dictionary.is_none());
    assert!(input.spell_cache_key.is_none());
    assert!(input.spell_spans.is_empty());
    assert!(input.autocorrection_preview_key.is_none());
    assert!(input.autocorrection_preview.is_none());
    assert_eq!(input.text, "teh mispeled words");
}

#[test]
fn attributed_single_line_is_shaped_during_measure_before_draw() {
    use crate::widget::marks::{Mark, MarkSpan};
    use crate::widget::rich_text_line::RichTextLine;
    // Motion reads at its end: this test checks the resting frame.
    let _settled = crate::motion::testing::settled();
    for scale in [1.0, 2.0] {
        let theme = Theme::dark().with_text_scale(scale);
        let mut line = RichTextLine::new().with_text("aBz");
        line.set_marks(vec![MarkSpan::new(1..2, Mark::Bold)]);
        let mut taffy = TaffyTree::new();
        line.build_layout(&mut taffy, &theme, &mut Default::default());
        let mut renderer = TextRenderer::new(); // intentionally stale 1x
        line.measure(
            Size::NONE,
            Size {
                width: taffy::AvailableSpace::MaxContent,
                height: taffy::AvailableSpace::MaxContent,
            },
            &mut renderer,
        );
        let layout = line
            .inner()
            .single_layout
            .as_ref()
            .expect("measurement must shape styled text");
        let weights: Vec<_> = layout
            .buffer
            .layout_runs()
            .flat_map(|r| r.glyphs.iter().map(|g| g.font_weight.0))
            .collect();
        assert_eq!(weights, [400, 700, 400]);
        assert_eq!(layout.buffer.metrics().font_size, body_size() * scale);
    }
}

fn draw_rich_line_for_test(line: &mut crate::widget::RichTextLine, theme: &Theme) -> Scene {
    let mut renderer = TextRenderer::new();
    renderer.set_text_scale(theme.text_scale);
    let mut scene = Scene::new();
    line.draw(
        &mut scene,
        Affine::IDENTITY,
        &mut renderer,
        theme,
        &crate::presence::PresenceCtx::empty(),
    );
    scene
}

#[test]
fn attributed_single_line_preedit_preserves_utf8_marks_and_commit_route() {
    use crate::widget::marks::{Mark, MarkSpan};
    // Motion reads at its end: this test checks the resting frame.
    let _settled = crate::motion::testing::settled();
    for scale in [1.0, 2.0] {
        let theme = Theme::dark().with_text_scale(scale);
        let mut line = crate::widget::RichTextLine::new().with_text("éW");
        line.set_marks(vec![
            MarkSpan::new(0..3, Mark::Bold),
            MarkSpan::new(0..3, Mark::Code),
            MarkSpan::new(0..3, Mark::Underline),
            MarkSpan::new(0..3, Mark::Strikethrough),
        ]);
        line.apply_layout_external(Rect::new(0.0, 0.0, 400.0, 60.0));
        line.set_focus(true);
        line.set_cursor_pos(2);
        let send = |line: &mut crate::widget::RichTextLine, event: WindowEvent| {
            dispatch(line, &event);
        };
        send(
            &mut line,
            WindowEvent::Ime(winit::event::Ime::Preedit("漢".into(), Some((3, 3)))),
        );
        draw_rich_line_for_test(&mut line, &theme);
        assert_eq!(line.value(), "éW");
        let input = line.inner();
        let layout = input.single_layout.as_ref().unwrap();
        assert_eq!(layout.buffer.lines[0].text(), "é漢W");
        assert_eq!(layout.underlines, [(0, 2), (5, 6), (2, 5)]);
        assert_eq!(layout.strikes, [(0, 2), (5, 6)]);
        let weights: Vec<_> = layout
            .buffer
            .layout_runs()
            .flat_map(|r| r.glyphs.iter().map(|g| g.font_weight.0))
            .collect();
        assert_eq!(weights, [700, 400, 700]);
        let anchor = input.ime_cursor_rect().unwrap();
        assert!(anchor.x0 > line.caret_rect(&theme, 2).x0);
        assert_eq!(
            input.display_ranges(&input.single_attributes.as_ref().unwrap().chips),
            [(0, 2), (5, 6)]
        );
        send(
            &mut line,
            WindowEvent::Ime(winit::event::Ime::Commit("漢".into())),
        );
        let scene = draw_rich_line_for_test(&mut line, &theme);
        assert_eq!(line.value(), "é漢W");
        assert_eq!(line.cursor_pos(), 5);
        assert_eq!(scene.encoding().resources.glyphs.len(), 3);
        assert!(
            line.marks()
                .iter()
                .all(|m| line.value().is_char_boundary(m.range.start)
                    && line.value().is_char_boundary(m.range.end))
        );
    }
}

#[test]
fn attributed_single_line_chip_and_link_rules_follow_shaped_ranges() {
    use crate::widget::marks::{Mark, MarkSpan};
    // Motion reads at its end: this test checks the resting frame.
    let _settled = crate::motion::testing::settled();
    for scale in [1.0, 2.0] {
        let theme = Theme::dark().with_text_scale(scale);
        let mut line = crate::widget::RichTextLine::new().with_text("aWWWz");
        line.set_marks(vec![
            MarkSpan::new(1..4, Mark::Bold),
            MarkSpan::new(1..4, Mark::Code),
            MarkSpan::new(1..4, Mark::Link(uuid::Uuid::from_u128(1))),
            MarkSpan::new(1..4, Mark::Strikethrough),
        ]);
        line.apply_layout_external(Rect::new(10.0, 20.0, 400.0, 90.0));
        let scene = draw_rich_line_for_test(&mut line, &theme);
        assert_eq!(scene.encoding().resources.glyphs.len(), 5);
        let input = line.inner();
        let left = input.caret_rect(&theme, 1).x0;
        let right = input.caret_rect(&theme, 4).x0;
        let layout = input.single_layout.as_ref().unwrap();
        assert_eq!(layout.strikes, [(1, 4)]);
        assert!(
            layout
                .buffer
                .layout_runs()
                .flat_map(|r| r.glyphs.iter())
                .all(|g| g.font_size == body_size() * scale)
        );
        for (background, pad) in [(true, 2.0), (false, 0.0)] {
            let mut decorations = Scene::new();
            input.draw_single_line_decorations(
                &mut decorations,
                Affine::IDENTITY,
                &theme,
                background,
            );
            let xs: Vec<_> = decorations
                .encoding()
                .path_data
                .chunks_exact(2)
                .map(|xy| f64::from(f32::from_bits(xy[0])))
                .collect();
            let min = xs.iter().copied().fold(f64::INFINITY, f64::min);
            let max = xs.iter().copied().fold(f64::NEG_INFINITY, f64::max);
            assert!((min - (left - pad)).abs() < 0.01);
            assert!((max - (right + pad)).abs() < 0.01);
            assert_eq!(
                decorations.encoding().resources.glyphs.len(),
                0,
                "decorations never repaint text"
            );
        }
    }
}

#[test]
fn attributed_single_line_style_inputs_invalidate_cached_shape() {
    use crate::richtext::{BaselineShift, FontFamily};
    // Motion reads at its end: this test checks the resting frame.
    let _settled = crate::motion::testing::settled();
    let mut input = TextInput::local().with_initial_value("WWW");
    input.apply_layout_external(Rect::new(0.0, 0.0, 400.0, 60.0));
    let mut attributes = SingleLineAttributes {
        spans: vec![RichSpan::new("WWW")],
        ..Default::default()
    };
    input.set_single_line_attributes(attributes.clone());
    draw_to_populate(&mut input);
    let original_width = input.single_layout.as_ref().unwrap().width();
    attributes.spans[0].size = Some(28.0);
    input.set_single_line_attributes(attributes.clone());
    draw_to_populate(&mut input);
    let sized_width = input.single_layout.as_ref().unwrap().width();
    assert!(sized_width > original_width * 1.5);
    attributes.spans[0].family = Some(FontFamily::Mono);
    input.set_single_line_attributes(attributes.clone());
    draw_to_populate(&mut input);
    assert!((input.single_layout.as_ref().unwrap().width() - sized_width).abs() > 1.0);
    attributes.spans[0].background = Some([40, 50, 60]);
    attributes.spans[0].baseline = BaselineShift::Super;
    attributes.spans[0].color = Some([100, 110, 120]);
    attributes.underlines = vec![(0, 1)];
    attributes.strikes = vec![(1, 3)];
    input.set_single_line_attributes(attributes);
    draw_to_populate(&mut input);
    let layout = input.single_layout.as_ref().unwrap();
    assert_eq!(layout.backgrounds, [(0, 3, [40, 50, 60])]);
    assert_eq!(layout.baselines, [(0, 3, BaselineShift::Super)]);
    assert_eq!(layout.underlines, [(0, 1)]);
    assert_eq!(layout.strikes, [(1, 3)]);
    assert!(
        layout
            .buffer
            .layout_runs()
            .flat_map(|r| r.glyphs.iter())
            .all(|g| g.color_opt == Some(cosmic_text::Color::rgb(100, 110, 120)))
    );
}

#[test]
fn attributed_single_line_sync_undo_redo_rebuilds_runs() {
    use crate::widget::marks::{Mark, MarkSpan};
    // Motion reads at its end: this test checks the resting frame.
    let _settled = crate::motion::testing::settled();
    let mut state = crate::State::new();
    let text = state
        .doc
        .put_object(&state.root, "body", ObjType::Text)
        .unwrap();
    state.doc.splice_text(&text, 0, 0, "WWW").unwrap();
    state.commit_action(None, None);
    let mut line = crate::widget::RichTextLine::new();
    line.bind_text(text.clone());
    line.sync(&state.doc, &state.root);
    line.set_marks(vec![MarkSpan::new(0..3, Mark::Bold)]);
    line.apply_layout_external(Rect::new(0.0, 0.0, 400.0, 60.0));
    let theme = Theme::dark().with_text_scale(2.0);
    draw_rich_line_for_test(&mut line, &theme);
    let wide = line.pixel_x_for_byte(&theme, 3);
    state.doc.splice_text(&text, 0, 3, "iii").unwrap();
    state.commit_action(None, None);
    for (step, expected) in [(0, "iii"), (1, "WWW"), (2, "iii")] {
        if step == 1 {
            state.undo(None, None).unwrap();
        }
        if step == 2 {
            state.redo(None, None).unwrap();
        }
        line.sync(&state.doc, &state.root);
        let _ = line.take_dirty(); // preserve direct-binding reconciliation semantics
        assert!(line.layout_needs_measure());
        draw_rich_line_for_test(&mut line, &theme);
        assert_eq!(line.value(), expected);
        let input = line.inner();
        assert!(
            input
                .single_layout
                .as_ref()
                .unwrap()
                .buffer
                .layout_runs()
                .flat_map(|r| r.glyphs.iter())
                .all(|g| g.font_weight.0 == 700)
        );
        let x = line.pixel_x_for_byte(&theme, 3);
        if expected == "WWW" {
            assert!((x - wide).abs() < 0.01);
        } else {
            assert!(x < wide * 0.6);
        }
        assert_eq!(line.byte_for_pixel_x(&theme, x), 3);
    }
}

#[test]
fn accessible_selection_preserves_anchor_and_focus_direction() {
    // Motion reads at its end: this test checks the resting frame.
    let _settled = crate::motion::testing::settled();
    let mut input = TextInput::local().with_initial_value("Ae\u{301}👋B");
    input.core.set_selection(8, 1);

    assert_eq!(input.selection_range_public(), Some((1, 8)));
    assert_eq!(input.selection_anchor_focus_public(), Some((8, 1)));
}

#[test]
fn insertion_collapses_an_empty_drag_selection() {
    // Motion reads at its end: this test checks the resting frame.
    let _settled = crate::motion::testing::settled();
    for (text, caret, expected) in [("", 0, "Nx"), ("ab", 1, "aNxb")] {
        let mut ti = TextInput::new(Vec::new());
        ti.set_text(text);
        ti.core.set_selection(caret, caret);
        ti.insert_str("N");
        assert_eq!(ti.selection_range_public(), None);
        assert_eq!(ti.cursor_pos(), caret + 1);
        ti.insert_str("x");
        assert_eq!(ti.value(), expected);
        assert_eq!(ti.selection_range_public(), None);
    }
}

#[test]
fn field_kinds_set_their_text_kind_while_typing() {
    // Motion reads at its end: this test checks the resting frame.
    let _settled = crate::motion::testing::settled();
    for (kind, text) in [
        (TextInputKind::Prose, TextKind::Prose),
        (TextInputKind::Search, TextKind::Prose),
        (TextInputKind::Username, TextKind::Identifier),
        (TextInputKind::Email, TextKind::Identifier),
        (TextInputKind::Url, TextKind::Url),
        (TextInputKind::Number, TextKind::Numeric),
        (TextInputKind::Code, TextKind::Code),
        (TextInputKind::Password, TextKind::Secret),
        (TextInputKind::Secret, TextKind::Secret),
    ] {
        assert_eq!(kind.text_kind(), text, "{kind:?}");
        let input = TextInput::local().with_kind(kind);
        assert_eq!(input.multiline_style(1.0).kind, text, "{kind:?}");
    }
}

#[test]
fn fields_share_rows_with_buttons_and_state_what_they_are() {
    fn layout_height(ti: &mut TextInput, theme: &Theme, width: f32) -> f32 {
        let mut taffy = TaffyTree::<()>::new();
        let mut node_map: std::collections::HashMap<NodeId, *mut dyn Widget> =
            std::collections::HashMap::new();
        let node = ti.build_layout(&mut taffy, theme, &mut node_map);
        let mut tr = TextRenderer::new();
        tr.set_text_scale(theme.text_scale);
        let size = Widget::measure(
            ti,
            taffy::Size {
                width: Some(width),
                height: None,
            },
            taffy::Size {
                width: taffy::AvailableSpace::Definite(width),
                height: taffy::AvailableSpace::MaxContent,
            },
            &mut tr,
        );
        match taffy.style(node).unwrap().size.height.into_option() {
            Some(fixed) => fixed,
            None => size.height,
        }
    }

    // Motion reads at its end: this test checks the resting frame.
    let _settled = crate::motion::testing::settled();
    // Case `a_small_field_is_as_tall_as_a_small_button`.
    {
        let theme = Theme::dark();
        let mut small = TextInput::local().control_size(ControlSize::Small);
        assert_eq!(layout_height(&mut small, &theme, 200.0), 28.0);
        assert!(
            (FieldFace::height(ControlSize::Regular, &theme) - 32.0).abs() < 1e-9,
            "the painted face agrees with the retained field"
        );
    }

    // Case `min_lines_holds_a_wrapping_field_open_with_the_field_inset`.
    {
        let theme = Theme::dark();
        let mut notes = TextInput::local().with_min_lines(4);
        let height = layout_height(&mut notes, &theme, 300.0);
        let line = body_size() * LINE_HEIGHT_RATIO;
        assert!(
            height >= 4.0 * line,
            "four lines tall while empty: {height} vs line {line}"
        );
        let mut one = TextInput::local().multiline(true);
        assert!(
            layout_height(&mut one, &theme, 300.0) >= 32.0,
            "a wrapping field is never shorter than a single-line one"
        );
        let single = TextInput::local();
        assert!(
            (notes.text_inset_x(&theme) - single.text_inset_x(&theme)).abs() < 1e-9,
            "wrapping and single-line fields start their text at one inset"
        );
    }

    // Case `a_plain_field_leaves_the_gutter_to_its_parent`.
    {
        let theme = Theme::dark();
        let plain = TextInput::local().multiline(true).with_plain_chrome();
        assert_eq!(plain.text_inset_x(&theme), 0.0);
        let mut document = TextInput::local().multiline(true).with_plain_chrome();
        document.keep_document_inset();
        assert_eq!(document.text_inset_x(&theme), ML_PAD_X);
    }

    // Case `the_role_sets_the_family_and_the_placeholder_follows_it`.
    {
        let reading = TextInput::local().with_text_role(TextRole::Reading);
        assert_eq!(reading.placeholder_style(16.0).family, FontChoice::Serif);
        let code = TextInput::local().with_text_role(TextRole::Code);
        assert_eq!(code.placeholder_style(13.0).family, FontChoice::Monospace);
        let body = TextInput::local();
        assert_eq!(body.placeholder_style(14.0).family, FontChoice::SansSerif);
    }

    // Case `set_text_parks_the_caret_at_the_end`.
    {
        let mut ti = TextInput::local().with_initial_value("abc");
        ti.set_cursor_pos(1);
        ti.set_text("hello");
        assert_eq!(ti.cursor_pos(), 5);
    }

    // Case `content_sized_text_does_not_grow_with_the_text_scale`.
    {
        let big = Theme::dark().with_text_scale(2.0);
        let mut document = TextInput::local().with_content_size(12.0);
        let mut chrome = TextInput::local();
        assert!(
            layout_height(&mut document, &big, 200.0) < layout_height(&mut chrome, &big, 200.0),
            "a document-space field keeps its size under a large text scale"
        );
        assert!((document.scale_for(2.0) - 1.0).abs() < f32::EPSILON);
    }
}

#[test]
fn submit_accessory_and_span_highlighting() {
    // Motion reads at its end: this test checks the resting frame.
    let _settled = crate::motion::testing::settled();
    // Case `on_submit_reports_the_text_on_enter`.
    {
        let seen = std::rc::Rc::new(std::cell::RefCell::new(Vec::<String>::new()));
        let sink = seen.clone();
        let mut ti = TextInput::local()
            .with_initial_value("https://tilo.so")
            .on_submit(move |text| sink.borrow_mut().push(text.to_string()));
        ti.set_focus(true);
        let mut doc = AutoCommit::new();
        assert!(drive(
            &mut ti,
            &mut doc,
            &named_key(NamedKey::Enter),
            mods(false, false)
        ));
        assert_eq!(seen.borrow().as_slice(), ["https://tilo.so"]);
        assert!(ti.take_submit(), "the polled signal still fires");
    }

    // Case `the_clear_accessory_empties_the_field_as_an_edit`.
    {
        let mut ti = TextInput::local()
            .with_initial_value("query")
            .with_clear_button();
        ti.apply_layout_external(Rect::new(0.0, 0.0, 240.0, 32.0));
        draw_to_populate(&mut ti);
        let node = Widget::accessibility(&ti).expect("field semantics");
        assert_eq!(node.children.len(), 1);
        assert_eq!(node.children[0].name.as_deref(), Some("Clear"));
        let rect = Widget::synthetic_accessibility_rect(&ti, &[0]).expect("accessory rect");
        assert!(rect.x1 <= 240.0 && rect.x0 > 120.0, "{rect:?}");
        let mut doc = AutoCommit::new();
        drive(
            &mut ti,
            &mut doc,
            &cursor_moved(rect.center().x, rect.center().y),
            mods(false, false),
        );
        assert!(drive(&mut ti, &mut doc, &mouse_press(), mods(false, false)));
        assert_eq!(ti.value(), "");
        assert_eq!(
            ti.take_dirty().as_deref(),
            Some(""),
            "clearing is a user edit"
        );
        let node = Widget::accessibility(&ti).expect("field semantics");
        assert!(
            node.children.is_empty(),
            "an empty field hides its clear button"
        );
    }

    // Case `a_toggle_accessory_is_a_pressed_toggle_for_people_and_agents`.
    {
        let seen = std::rc::Rc::new(std::cell::RefCell::new(Vec::<bool>::new()));
        let sink = seen.clone();
        let mut ti = TextInput::local().with_trailing_accessory(
            crate::icons::Icon::X,
            "Clear for every photo",
            AccessoryAction::Toggle {
                pressed: false,
                on_toggle: Box::new(move |on| sink.borrow_mut().push(on)),
            },
        );
        ti.apply_layout_external(Rect::new(0.0, 0.0, 240.0, 32.0));
        draw_to_populate(&mut ti);
        let node = Widget::accessibility(&ti).expect("field semantics");
        assert_eq!(node.children.len(), 1, "shown while the field is empty");
        let toggle = &node.children[0];
        assert_eq!(toggle.role, crate::a11y::Role::ToggleButton);
        assert_eq!(toggle.state.checked, Some(false));
        let target = toggle.action_target.clone().expect("agents can reach it");

        let rect = Widget::synthetic_accessibility_rect(&ti, &[0]).expect("accessory rect");
        let mut doc = AutoCommit::new();
        drive(
            &mut ti,
            &mut doc,
            &cursor_moved(rect.center().x, rect.center().y),
            mods(false, false),
        );
        assert!(drive(&mut ti, &mut doc, &mouse_press(), mods(false, false)));
        assert_eq!(ti.accessory_pressed(), Some(true));
        let node = Widget::accessibility(&ti).expect("field semantics");
        assert_eq!(node.children[0].state.checked, Some(true));

        let root = automerge::ROOT;
        assert!(Widget::perform_semantic_action(
            &mut ti,
            &target,
            &crate::a11y::Action::Activate,
            &mut crate::widget::Edits::lend(&mut doc),
            &root,
        ));
        assert_eq!(seen.borrow().as_slice(), [true, false]);
        assert_eq!(ti.accessory_pressed(), Some(false));

        ti.set_accessory_pressed(true);
        assert_eq!(ti.accessory_pressed(), Some(true), "the host syncs it");
        assert!(ti.take_dirty().is_none(), "toggling is not a text edit");
    }

    // Case `a_go_accessory_shows_while_editing_and_commits_the_field`.
    {
        let seen = std::rc::Rc::new(std::cell::RefCell::new(Vec::<String>::new()));
        let sink = seen.clone();
        let mut ti = TextInput::local()
            .with_initial_value("example.org")
            .with_trailing_accessory(
                crate::icons::Icon::ArrowRight,
                "Go",
                AccessoryAction::Submit,
            )
            .on_submit(move |text| sink.borrow_mut().push(text.to_string()));
        ti.apply_layout_external(Rect::new(0.0, 0.0, 240.0, 32.0));
        draw_to_populate(&mut ti);
        let node = Widget::accessibility(&ti).expect("field semantics");
        assert!(node.children.is_empty(), "no go button at rest");

        ti.set_focus(true);
        draw_to_populate(&mut ti);
        let node = Widget::accessibility(&ti).expect("field semantics");
        assert_eq!(node.children.len(), 1, "the go button shows while editing");
        assert_eq!(node.children[0].name.as_deref(), Some("Go"));
        assert_eq!(node.children[0].role, crate::a11y::Role::Button);
        let rect = Widget::synthetic_accessibility_rect(&ti, &[0]).expect("accessory rect");
        let mut doc = AutoCommit::new();
        drive(
            &mut ti,
            &mut doc,
            &cursor_moved(rect.center().x, rect.center().y),
            mods(false, false),
        );
        assert!(drive(&mut ti, &mut doc, &mouse_press(), mods(false, false)));
        assert_eq!(seen.borrow().as_slice(), ["example.org"]);
        assert!(ti.take_submit(), "the polled signal fires too");
        assert_eq!(ti.value(), "example.org", "going is not an edit");
    }

    // Case `a_span_highlighter_colours_the_single_line`.
    {
        let mut ti = TextInput::local()
            .with_initial_value("=A1+B2")
            .with_span_highlighter(|text, theme| {
                text.match_indices("A1")
                    .map(|(at, m)| (at..at + m.len(), theme.tokens.colors.semantic.danger))
                    .collect()
            });
        ti.apply_layout_external(Rect::new(0.0, 0.0, 240.0, 32.0));
        draw_to_populate(&mut ti);
        let spans = &ti.single_attributes.as_ref().expect("highlighted").spans;
        assert_eq!(
            spans.iter().map(|s| s.text.as_str()).collect::<Vec<_>>(),
            ["=", "A1", "+B2"]
        );
        assert!(spans[1].color.is_some() && spans[0].color.is_none());
    }

    // Case `the_painted_field_matches_the_retained_one`.
    {
        let theme = Theme::dark();
        let mut face = Scene::new();
        FieldFace::new(Rect::new(0.0, 0.0, 200.0, 32.0), "")
            .focused(true)
            .paint_chrome(&mut face, Affine::IDENTITY, &theme);
        let mut tr = TextRenderer::new();
        FieldFace::new(Rect::new(0.0, 0.0, 200.0, 32.0), "")
            .placeholder("Search")
            .paint(&mut face, Affine::IDENTITY, &mut tr, &theme);
        let text_rect = FieldFace::new(Rect::new(0.0, 0.0, 200.0, 32.0), "").text_rect(&theme);
        assert!(
            (text_rect.x0 - TextInput::local().text_inset_x(&theme)).abs() < 1e-9,
            "a painted field insets its text like a retained one"
        );
    }
}

#[cfg(feature = "broker-transport")]
#[test]
fn shared_personal_words_protect_autocorrection_and_clear_stale_spelling() {
    // Motion reads at its end: this test checks the resting frame.
    let _settled = crate::motion::testing::settled();
    // The authenticated host it installs is process-wide: every field
    // checking spelling on another thread would read its personal words.
    if !crate::testing::in_own_process(&crate::testing::test_path(
        module_path!(),
        "shared_personal_words_protect_autocorrection_and_clear_stale_spelling",
    )) {
        return;
    }
    struct Restore;
    impl Drop for Restore {
        fn drop(&mut self) {
            crate::local_identity::install_source(None);
        }
    }
    let _guard = tilo_test_env::lock();
    let _restore = Restore;
    crate::local_identity::install_source(Some(Arc::new(crate::local_identity::FixedIdentity(
        crate::local_identity::LocalIdentity {
            group_public_key: [1; 32],
            display_name: String::new(),
            avatar_color: [0; 3],
        },
    ))));
    let directory = tempfile::tempdir().unwrap();
    let service = Arc::new(
        settings_store::personal_dictionary::Service::new(directory.path().to_owned()).unwrap(),
    );
    let source = Arc::new(settings_store::personal_dictionary::host::Host::new(
        Arc::new(settings_store::HostDocument::new(
            serde_json::Map::new(),
            |_| Ok(()),
        )),
        service,
        Arc::new(|| Some([1; 32])),
    ));
    let mut host =
        crate::personal_dictionary::TestHost::new(source, "so.tilo.notes", [1; 32]).unwrap();
    let field = |text: &str| {
        TextInput::local()
            .with_autocorrect(true)
            .with_spell_language("en_US")
            .with_initial_value(text)
    };
    let mut input = field("teh");
    input.core.set_caret(input.text.len());
    assert!(input.autocorrection_candidate().is_some());
    crate::personal_dictionary::edit(
        0,
        crate::personal_dictionary::Edit::Add(
            crate::personal_dictionary::Word::new("teh", None).unwrap(),
        ),
    )
    .unwrap();
    host.flush().unwrap();
    input.insert_word_boundary(" ");
    assert_eq!(input.value(), "teh ");
    assert!(
        crate::personal_dictionary::edit(1, crate::personal_dictionary::Edit::Reset).is_err(),
        "ordinary apps may add explicit words but cannot reset the dictionary"
    );
    let mut ordinary = field("adress");
    ordinary.core.set_caret(ordinary.text.len());
    ordinary.insert_word_boundary(" ");
    assert_eq!(
        ordinary.value(),
        "address ",
        "personal words never force ordinary replacements"
    );
    crate::personal_dictionary::edit(
        1,
        crate::personal_dictionary::Edit::Add(
            crate::personal_dictionary::Word::new("Élodie", Some("fr")).unwrap(),
        ),
    )
    .unwrap();
    host.flush().unwrap();
    let mut french = field("E\u{301}lodie").with_spell_language("fr_FR");
    french.set_focus(true);
    french.refresh_personal_spelling();
    assert!(french.personal_word("E\u{301}lodie"));
    let mut english = field("Élodie");
    english.set_focus(true);
    english.refresh_personal_spelling();
    assert!(!english.personal_word("Élodie"));
    crate::local_identity::install_source(Some(Arc::new(crate::local_identity::NoLocalIdentity)));
    french.refresh_personal_spelling();
    assert!(!french.personal_word("Élodie"));
    assert!(french.spell_dictionary.as_ref().unwrap().is_empty());
}

#[test]
fn delayed_paste_publishes_once_and_cancels_when_the_target_changes() {
    use crate::clipboard::{ClipboardBackend, ClipboardTarget};
    use std::sync::{Arc, Mutex, mpsc};
    #[derive(Default)]
    struct Delayed(Mutex<Option<mpsc::Sender<Option<String>>>>);
    impl ClipboardBackend for Delayed {
        fn set_text(&self, _: ClipboardTarget, _: &str) -> bool {
            false
        }
        fn text(&self, _: ClipboardTarget) -> Option<String> {
            None
        }
        fn request_text(&self, _: ClipboardTarget) -> mpsc::Receiver<Option<String>> {
            let (send, receive) = mpsc::channel();
            *self.0.lock().unwrap() = Some(send);
            receive
        }
        fn name(&self) -> &'static str {
            "delayed-paste"
        }
    }
    // Motion reads at its end: this test checks the resting frame.
    let _settled = crate::motion::testing::settled();
    let clipboard = Arc::new(Delayed::default());
    let services = crate::host::HostServices::null().with_clipboard(clipboard.clone());
    let _host = crate::host::enter(&services);
    let changes = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
    let captured = changes.clone();
    let mut input = TextInput::local()
        .with_initial_value("a")
        .on_change(move |value| captured.borrow_mut().push(value.to_owned()));
    input.set_focus(true);
    let mut document = AutoCommit::new();
    drive(&mut input, &mut document, &char_key("v"), mods(true, false));
    assert_eq!(input.value(), "a");
    clipboard
        .0
        .lock()
        .unwrap()
        .take()
        .unwrap()
        .send(Some("b\nc".into()))
        .unwrap();
    Widget::sync(&mut input, &document, &automerge::ROOT);
    assert_eq!(input.value(), "abc");
    assert_eq!(&*changes.borrow(), &["abc"]);
    Widget::sync(&mut input, &document, &automerge::ROOT);
    assert_eq!(&*changes.borrow(), &["abc"]);

    for change in 0..4 {
        drive(&mut input, &mut document, &char_key("v"), mods(true, false));
        let reply = clipboard.0.lock().unwrap().take().unwrap();
        match change {
            0 => {
                input.set_focus(false);
                input.set_focus(true);
            }
            1 => {
                input.set_cursor_pos(0);
                input.set_cursor_pos(3);
            }
            2 => {
                input.set_text("changed");
                input.set_text("abc");
            }
            _ => {
                input.set_read_only(true);
                let _ = reply.send(Some("stale".into()));
                Widget::sync(&mut input, &document, &automerge::ROOT);
                assert_eq!(
                    input.value(),
                    "abc",
                    "read-only fields reject pending paste"
                );
                input.set_read_only(false);
                Widget::sync(&mut input, &document, &automerge::ROOT);
                assert_eq!(
                    input.value(),
                    "abc",
                    "re-enabling must not revive stale paste"
                );
                continue;
            }
        }
        let _ = reply.send(Some("stale".into()));
        Widget::sync(&mut input, &document, &automerge::ROOT);
        assert_eq!(
            input.value(),
            "abc",
            "changed target must cancel delayed paste"
        );
    }
}

/// One host frame of a multiline field: Taffy's sizing queries at the
/// widths a Scrollable's grid asks for (zero, min/max-content, final), then
/// placement and paint against a window-sized viewport.
fn typing_frame(input: &mut TextInput, tr: &mut TextRenderer, theme: &Theme) {
    let mut height = 0.0f32;
    for (known, available) in [
        (Some(0.0), taffy::AvailableSpace::Definite(0.0)),
        (None, taffy::AvailableSpace::MinContent),
        (None, taffy::AvailableSpace::MaxContent),
        (Some(600.0), taffy::AvailableSpace::Definite(600.0)),
    ] {
        height = Widget::measure(
            input,
            taffy::Size {
                width: known,
                height: None,
            },
            taffy::Size {
                width: available,
                height: taffy::AvailableSpace::MaxContent,
            },
            tr,
        )
        .height;
    }
    input.apply_layout_external(Rect::new(0.0, 0.0, 600.0, f64::from(height)));
    let presence = crate::presence::PresenceCtx::empty();
    presence.push_viewport(Rect::new(0.0, 0.0, 600.0, 400.0));
    let mut scene = Scene::new();
    Widget::draw(input, &mut scene, Affine::IDENTITY, tr, theme, &presence);
}

fn typing_field(text: &str, tr: &mut TextRenderer, theme: &Theme) -> TextInput {
    let mut input = TextInput::local()
        .multiline(true)
        .with_spell_language("en_US")
        .with_initial_value(text);
    input.set_focus(true);
    let mut taffy = TaffyTree::new();
    let mut nodes = std::collections::HashMap::new();
    Widget::build_layout(&mut input, &mut taffy, theme, &mut nodes);
    typing_frame(&mut input, tr, theme);
    input
}

#[test]
fn multiline_keystroke_prepares_only_the_edited_paragraph_at_every_width() {
    // Motion reads at its end: this test checks the resting frame.
    let _settled = crate::motion::testing::settled();
    let mut tr = crate::font_catalog::fixture_renderer();
    let theme = Theme::dark();
    tr.set_text_scale(theme.text_scale);
    let text = (0..200)
        .map(|index| format!("Paragraph {index} of a long note, with enough words to wrap."))
        .collect::<Vec<_>>()
        .join("\n");
    let mut input = typing_field(&text, &mut tr, &theme);
    let at = text.find("Paragraph 100 ").unwrap() + "Paragraph 100".len();
    input.set_cursor_pos(at);
    typing_frame(&mut input, &mut tr, &theme);
    crate::richtext::take_prepared_paragraphs();
    let mut doc = AutoCommit::new();
    for (step, key) in ["x", "y", " ", "z"].into_iter().enumerate() {
        assert!(drive(
            &mut input,
            &mut doc,
            &char_key(key),
            Modifiers::default()
        ));
        typing_frame(&mut input, &mut tr, &theme);
        assert_eq!(
            crate::richtext::take_prepared_paragraphs(),
            1,
            "keystroke {step}: one paragraph prepared and shaped for every width"
        );
    }
    // The paragraph measured and painted is the one a fresh field builds.
    let fresh = typing_field(input.value(), &mut tr, &theme);
    let (typed, fresh) = (
        input.single_layout.as_ref().unwrap(),
        fresh.single_layout.as_ref().unwrap(),
    );
    assert_eq!(typed.height(), fresh.height());
    assert_eq!(typed.logical_line_origins(), fresh.logical_line_origins());
    for byte in (0..=input.value().len()).filter(|byte| input.value().is_char_boundary(*byte)) {
        assert_eq!(
            typed.caret_geometry(byte),
            fresh.caret_geometry(byte),
            "byte {byte}"
        );
    }
    for (a, b) in typed.buffer.lines.iter().zip(&fresh.buffer.lines) {
        assert_eq!(
            format!("{:?}", a.layout_opt()),
            format!("{:?}", b.layout_opt())
        );
    }
}

#[test]
fn spelling_marks_follow_edits_like_a_full_check() {
    // Motion reads at its end: this test checks the resting frame.
    let _settled = crate::motion::testing::settled();
    let text = "teh first paragraph\n\nsecond wrold here\nthird is fine\n\nlast lnie";
    let mut input = TextInput::local()
        .with_spell_language("en_US")
        .with_initial_value(text);
    input.set_focus(true);
    input.refresh_spelling();
    let edits: [(&str, usize); 6] = [
        ("x", 4),
        (" mispeled", 19),
        ("\nnew paragrph\n", 21),
        ("", 0),
        ("Teh", 0),
        ("ok", 10),
    ];
    for (insert, at) in edits {
        let mut edited = input.value().to_owned();
        let at = at.min(edited.len());
        if insert.is_empty() {
            // Join the first two lines.
            if let Some(newline) = edited.find('\n') {
                edited.remove(newline);
            }
        } else {
            edited.insert_str(at, insert);
        }
        input.text.clone_from(&edited);
        input.refresh_spelling();
        let mut fresh = TextInput::local()
            .with_spell_language("en_US")
            .with_initial_value(&edited);
        fresh.set_focus(true);
        fresh.refresh_spelling();
        assert_eq!(input.spell_spans, fresh.spell_spans, "{edited:?}");
        assert!(!input.spell_spans.is_empty(), "{edited:?}");
    }
}

#[test]
fn single_line_batch_metrics_preserve_graphemes_preedit_and_theme_geometry() {
    // Motion reads at its end: this test checks the resting frame.
    let _settled = crate::motion::testing::settled();
    for text in ["", "ordinary ffi text", "abc אבג xyz", "é 👩‍💻 end"] {
        for scale in [1.0, 2.0] {
            let theme = Theme::dark().with_text_scale(scale);
            let mut input = TextInput::local().with_initial_value(text);
            input.apply_layout_external(Rect::new(20.0, 30.0, 420.0, 70.0));
            for preedit_at in [None, Some(0), Some(text.len() / 2), Some(text.len())] {
                input.core.clear_preedit();
                if let Some(at) = preedit_at {
                    input.set_cursor_pos(at);
                    input.core.set_preedit("編", Some((0, "編".len())));
                }
                draw_with_theme(&mut input, &theme);
                let layout = input
                    .single_layout
                    .as_ref()
                    .expect("production draw shapes the field");
                let mut reference = vec![0.0; text.len().saturating_add(1)];
                for byte in grapheme_indices(text)
                    .map(|(byte, _)| byte)
                    .chain([text.len()])
                {
                    reference[byte] =
                        f64::from(layout.caret_geometry(input.text_to_layout_byte(byte)).0);
                }
                assert_eq!(
                    input.char_x_positions, reference,
                    "{text:?}, scale={scale}, preedit_at={preedit_at:?}"
                );
            }
        }
    }
}

#[test]
fn themed_input_typography_reflows_and_centers_actual_shaped_text() {
    // Motion reads at its end: this test checks the resting frame.
    let _settled = crate::motion::testing::settled();
    let mut input = TextInput::local()
        .with_initial_value("Themed input baseline")
        .with_spellcheck(false)
        .with_autocorrect(false);
    input.core.set_selection(6, 12);
    let mut authored = Theme::dark();
    authored.tokens.typography = authored.tokens.typography.with_base_size(18.0);
    let themes = [
        authored.clone().with_ui_scale(1.25).with_text_scale(1.5),
        authored.with_ui_scale(2.0).with_text_scale(1.5),
    ];
    let mut previous_height = 0.0;
    for theme in themes {
        let mut tree = TaffyTree::new();
        let node = input.build_layout(&mut tree, &theme, &mut Default::default());
        tree.compute_layout(
            node,
            Size {
                width: taffy::AvailableSpace::Definite(640.0),
                height: taffy::AvailableSpace::MaxContent,
            },
        )
        .unwrap();
        input.apply_layout(&tree, node, Point::ZERO);
        draw_with_theme(&mut input, &theme);
        let expected = theme.text(TextRole::Body).size;
        assert!(expected > body_size() * theme.text_scale);
        let layout = input
            .single_layout
            .as_ref()
            .expect("actual draw shapes the input");
        assert!((layout.buffer.metrics().font_size - expected).abs() < 0.001);
        let line_height = f64::from(layout.buffer.metrics().line_height);
        assert!(input.rect.height() >= line_height + 2.0 * f64::from(theme.tokens.spacing.s1));
        assert!(
            input.rect.height() > previous_height,
            "a changed theme reflows the retained field"
        );
        previous_height = input.rect.height();
        assert!((input.single_origin.y + line_height / 2.0 - input.rect.center().y).abs() < 0.001);
        assert_eq!(input.core.caret(), 12);
        assert_eq!(input.core.anchor(), 6);
        assert!(
            input.take_dirty().is_none(),
            "theme changes are not user edits"
        );
    }
    input.set_text_role(TextRole::Heading);
    let theme = Theme::dark().with_ui_scale(2.0).with_text_scale(1.5);
    input.prepare_theme(&theme);
    draw_with_theme(&mut input, &theme);
    assert!(
        (input
            .single_layout
            .as_ref()
            .unwrap()
            .buffer
            .metrics()
            .font_size
            - theme.text(TextRole::Heading).size)
            .abs()
            < 0.001,
        "a role change reads the current theme instead of a stale cached Body size"
    );
}

#[test]
fn read_only_text_selects_and_copies_but_never_changes() {
    // Motion reads at its end: this test checks the resting frame.
    let _settled = crate::motion::testing::settled();
    let mut ti = TextInput::local()
        .with_initial_value("read me")
        .with_read_only(true);
    ti.set_focus(true);
    ti.set_cursor_pos(7);
    let mut doc = AutoCommit::new();
    for key in [
        char_key("x"),
        named_key(NamedKey::Backspace),
        named_key(NamedKey::Enter),
    ] {
        assert!(
            !drive(&mut ti, &mut doc, &key, Modifiers::default()),
            "an edit key is left to the app"
        );
    }
    assert!(!drive(
        &mut ti,
        &mut doc,
        &crate::synthetic_input::key_press(Key::Character("v".into()), None),
        mods(true, false)
    ));
    assert_eq!(ti.value(), "read me");
    assert!(drive(
        &mut ti,
        &mut doc,
        &named_key(NamedKey::ArrowLeft),
        mods(false, true)
    ));
    assert_eq!(
        ti.selection_range(),
        Some((6, 7)),
        "shift+Left still selects"
    );
}

#[test]
fn embedded_input_measurement_adopts_theme_and_keeps_document_size_override() {
    // Motion reads at its end: this test checks the resting frame.
    let _settled = crate::motion::testing::settled();
    let text = "A wrapping field must measure the same themed typography that it paints.";
    let mut theme = Theme::dark();
    theme.tokens.typography = theme.tokens.typography.with_base_size(19.0);
    let theme = theme.with_ui_scale(2.0).with_text_scale(1.5);
    let mut input = TextInput::local().multiline(true).with_initial_value(text);
    let mut renderer = TextRenderer::new();
    assert_eq!(
        renderer.text_scale(),
        1.0,
        "renderer starts at its old frame scale"
    );
    let height = input.measure_wrapped_content(280.0, &mut renderer, &theme);
    let layout = input
        .single_layout
        .as_ref()
        .expect("actual wrapped measurement shapes text");
    assert!((layout.buffer.metrics().font_size - theme.text(TextRole::Body).size).abs() < 0.001);
    assert!(height >= layout.height());
    assert!(
        layout.buffer.layout_runs().count() > 1,
        "the themed paragraph really wraps"
    );

    let mut document = TextInput::local()
        .with_content_size(17.0)
        .with_initial_value("document");
    document.prepare_theme(&theme);
    document.apply_layout_external(Rect::new(0.0, 0.0, 300.0, 80.0));
    draw_with_theme(&mut document, &theme);
    assert_eq!(
        document
            .single_layout
            .as_ref()
            .unwrap()
            .buffer
            .metrics()
            .font_size,
        17.0
    );
}

#[test]
fn read_only_text_links_its_addresses_and_the_ranges_it_is_given() {
    // Motion reads at its end: this test checks the resting frame.
    let _settled = crate::motion::testing::settled();
    let ti = TextInput::local()
        .with_initial_value("see https://example.com/a, or www.example.org and click here")
        .with_read_only(true);
    let at = |needle: &str| ti.text.find(needle).unwrap();
    assert_eq!(
        ti.link_at(at("example.com")).as_deref(),
        Some("https://example.com/a")
    );
    assert_eq!(
        ti.link_at(at("example.org")).as_deref(),
        Some("https://www.example.org"),
        "a bare host opens over https"
    );
    assert_eq!(ti.link_at(at("see")), None);

    let here = at("click here");
    let ti = ti.with_links(vec![(here..here + 10, "https://example.net/x".into())]);
    assert_eq!(
        ti.link_at(here + 2).as_deref(),
        Some("https://example.net/x")
    );

    let editable = TextInput::local().with_initial_value("https://example.com");
    assert_eq!(editable.link_at(3), None, "an editable field links nothing");
}

#[test]
fn only_the_web_and_mail_open_from_a_link() {
    use crate::text_detect::Detected;
    // Motion reads at its end: this test checks the resting frame.
    let _settled = crate::motion::testing::settled();
    assert_eq!(
        link_target(Detected::Url, "javascript://x/%0aalert(1)"),
        None
    );
    assert_eq!(link_target(Detected::Url, "ftp://example.com"), None);
    assert_eq!(
        link_target(Detected::Email, "me@example.com").as_deref(),
        Some("mailto:me@example.com")
    );
    assert_eq!(link_target(Detected::Mention, "@ilya"), None);
}

#[test]
fn forced_color_idle_field_boundary_has_rendered_non_text_contrast() {
    // Motion reads at its end: this test checks the resting frame.
    let _settled = crate::motion::testing::settled();
    let mut renderer = crate::offscreen::OffscreenRenderer::new(160, 64);
    for theme in [
        Theme::high_contrast(),
        Theme::preset(crate::theme::Preset::HighContrastLight),
    ] {
        assert!(theme.colors_are_forced());
        let mut scene = Scene::new();
        paint_field_chrome(
            &mut scene,
            Affine::IDENTITY,
            &theme,
            &RoundedRect::from_rect(Rect::new(12.0, 12.0, 148.0, 52.0), 8.0),
            FieldState::default(),
        );
        let pixels = renderer.render(&scene, theme.tokens.colors.surfaces.content);
        let pixel = |x: usize, y: usize| {
            let offset = (y * 160 + x) * 4;
            vello::peniko::Color::from_rgba8(
                pixels[offset],
                pixels[offset + 1],
                pixels[offset + 2],
                pixels[offset + 3],
            )
        };
        let ground = pixel(80, 24);
        for edge in [pixel(80, 12), pixel(12, 32)] {
            assert!(
                crate::a11y::contrast_ratio(edge, ground)
                    >= crate::tokens::colors::NON_TEXT_MIN_CONTRAST,
                "actual idle field edge must remain visible when forced surfaces coincide"
            );
        }
        let enabled_contrast = crate::a11y::contrast_ratio(pixel(80, 12), ground);
        let mut disabled_scene = Scene::new();
        paint_field_chrome(
            &mut disabled_scene,
            Affine::IDENTITY,
            &theme,
            &RoundedRect::from_rect(Rect::new(12.0, 12.0, 148.0, 52.0), 8.0),
            FieldState {
                disabled: true,
                ..FieldState::default()
            },
        );
        let disabled_pixels =
            renderer.render(&disabled_scene, theme.tokens.colors.surfaces.content);
        for (x, y) in [(80, 12), (12, 32)] {
            let offset = (y * 160 + x) * 4;
            let edge = vello::peniko::Color::from_rgba8(
                disabled_pixels[offset],
                disabled_pixels[offset + 1],
                disabled_pixels[offset + 2],
                disabled_pixels[offset + 3],
            );
            let contrast = crate::a11y::contrast_ratio(edge, ground);
            assert!(
                contrast > 1.5 && contrast < enabled_contrast,
                "the actual disabled group must dim the forced outline while keeping it visible"
            );
        }
    }
}

#[test]
fn a_replacement_lands_only_on_the_text_it_was_made_for_and_is_left_selected() {
    // Motion reads at its end: this test checks the resting frame.
    let _settled = crate::motion::testing::settled();
    let mut ti = TextInput::local().with_initial_value("hello world");
    assert!(
        !ti.replace_range(0..5, "hullo", "HELLO"),
        "a range that no longer holds the expected text is refused"
    );
    assert!(!ti.replace_range(6..40, "world", "there"));
    assert_eq!(ti.value(), "hello world");
    assert!(ti.replace_range(6..11, "world", "there"));
    assert_eq!(ti.value(), "hello there");
    assert_eq!(ti.selection_range(), Some((6, 11)));

    let mut read_only = TextInput::local()
        .with_initial_value("hello")
        .with_read_only(true);
    assert!(!read_only.replace_range(0..5, "hello", "HELLO"));
    assert_eq!(read_only.value(), "hello");

    // A spelling fix chosen from the menu refuses a word that changed
    // while the menu was open.
    let mut ti = TextInput::local()
        .with_spell_language("en_US")
        .with_initial_value("teh");
    ti.core.set_selection(0, 3);
    ti.open_context_menu([10.0, 10.0]);
    let fix = ti
        .context_actions
        .iter()
        .position(|action| matches!(action, Some(CtxAction::ApplySpelling { .. })))
        .expect("a correction is offered");
    ti.set_text("ten");
    assert!(!ti.dispatch_context_action(fix));
    assert_eq!(ti.value(), "ten");
}

#[test]
fn a_secret_field_never_hands_its_text_to_a_text_action() {
    // Motion reads at its end: this test checks the resting frame.
    let _settled = crate::motion::testing::settled();
    for field in [
        TextInput::local().with_secret(true),
        TextInput::local().with_kind(TextInputKind::Password),
        TextInput::local().with_kind(TextInputKind::Secret),
    ] {
        let mut ti = field
            .with_text_action("translate", "Translate")
            .with_initial_value("hunter2");
        ti.core.set_selection(0, 7);
        ti.open_context_menu([10.0, 10.0]);
        assert!(
            !ti.context_actions
                .iter()
                .any(|action| matches!(action, Some(CtxAction::TextAction(_)))),
            "{:?}",
            ti.context_menu_labels()
        );
        assert_eq!(ti.take_text_action_request(), None);
    }
}

#[test]
fn every_kind_and_override_reaches_the_input_method_as_its_purpose_and_hints() {
    use crate::ime::InputPurpose;
    // Motion reads at its end: this test checks the resting frame.
    let _settled = crate::motion::testing::settled();
    // `zwp_text_input_v3` wire values.
    const NORMAL: u32 = 0;
    const DIGITS: u32 = 2;
    const NUMBER: u32 = 3;
    const PHONE: u32 = 4;
    const URL: u32 = 5;
    const EMAIL: u32 = 6;
    const NAME: u32 = 7;
    const PASSWORD: u32 = 8;
    const PIN: u32 = 9;
    const DATE: u32 = 10;
    const TIME: u32 = 11;
    const TERMINAL: u32 = 13;
    const COMPLETION: u32 = 0x1;
    const SPELLCHECK: u32 = 0x2;
    const AUTO_CAPITALIZATION: u32 = 0x4;
    const LOWERCASE: u32 = 0x8;
    const TITLECASE: u32 = 0x20;
    const SECRET: u32 = 0x40 | 0x80;
    const MULTILINE: u32 = 0x200;

    let kind = |kind| TextInput::local().with_kind(kind);
    let cases: Vec<(&str, TextInput, (u32, u32))> = vec![
        // Prose asks for nothing beyond the input method's defaults, as
        // before, whatever the toolkit's own checking does.
        ("prose", TextInput::local(), (0, NORMAL)),
        (
            "prose, multiline",
            TextInput::local().multiline(true),
            (MULTILINE, NORMAL),
        ),
        (
            "prose, checking off",
            TextInput::local()
                .with_spellcheck(false)
                .with_autocorrect(false),
            (0, NORMAL),
        ),
        (
            "prose, spellcheck on",
            TextInput::local().with_spellcheck(true),
            (SPELLCHECK, NORMAL),
        ),
        (
            "prose, autocorrect on",
            TextInput::local().with_autocorrect(true),
            (COMPLETION, NORMAL),
        ),
        ("name", kind(TextInputKind::Name), (TITLECASE, NAME)),
        ("search", kind(TextInputKind::Search), (0, NORMAL)),
        (
            "username",
            kind(TextInputKind::Username),
            (LOWERCASE, NORMAL),
        ),
        ("email", kind(TextInputKind::Email), (LOWERCASE, EMAIL)),
        ("url", kind(TextInputKind::Url), (LOWERCASE, URL)),
        ("path", kind(TextInputKind::Path), (0, NORMAL)),
        ("code", kind(TextInputKind::Code), (0, NORMAL)),
        (
            "code, multiline",
            kind(TextInputKind::Code).multiline(true),
            (MULTILINE, NORMAL),
        ),
        ("number", kind(TextInputKind::Number), (0, NUMBER)),
        ("phone", kind(TextInputKind::Phone), (0, PHONE)),
        ("pin", kind(TextInputKind::Pin), (0, PIN)),
        (
            "masked pin keeps its digit pad",
            TextInput::local()
                .with_secret(true)
                .with_kind(TextInputKind::Pin),
            (SECRET, PIN),
        ),
        (
            "password",
            kind(TextInputKind::Password),
            (SECRET, PASSWORD),
        ),
        ("secret", kind(TextInputKind::Secret), (SECRET, PASSWORD)),
        (
            "masked email",
            TextInput::local()
                .with_secret(true)
                .with_kind(TextInputKind::Email),
            (SECRET, PASSWORD),
        ),
        (
            "password, multiline and spellcheck on",
            kind(TextInputKind::Password)
                .multiline(true)
                .with_spellcheck(true)
                .with_autocorrect(true),
            (SECRET, PASSWORD),
        ),
    ];
    for (case, field, want) in cases {
        let mut field = field.with_initial_value("x");
        field.set_focus(true);
        field.apply_layout_external(Rect::new(0.0, 0.0, 300.0, 120.0));
        draw_to_populate(&mut field);
        let intent = crate::ime::ImeWindowState::from_widget(&field).expect(case);
        assert_eq!(intent.content.wayland(), want, "{case}");
        assert_eq!(
            intent.content.winit_purpose() == winit::window::ImePurpose::Password,
            want.0 & SECRET == SECRET,
            "{case}: platforms with three purposes still hide a secret"
        );
    }

    // Purposes no text field kind asks for, which custom editors declare.
    for (purpose, want) in [
        (InputPurpose::Decimal, NUMBER),
        (InputPurpose::OneTimeCode, DIGITS),
        (InputPurpose::Date, DATE),
        (InputPurpose::Time, TIME),
        (InputPurpose::Terminal, TERMINAL),
    ] {
        assert_eq!(crate::ime::InputContent::of(purpose).wayland(), (0, want));
    }
    let mut sentences = crate::ime::InputContent::TEXT;
    sentences.hints.capitalization = crate::ime::Capitalization::Sentences;
    assert_eq!(sentences.wayland(), (AUTO_CAPITALIZATION, NORMAL));
}

#[test]
fn an_open_text_menu_owns_the_keyboard() {
    // Motion reads at its end: this test checks the resting frame.
    let _settled = crate::motion::testing::settled();
    let mut ti = TextInput::local()
        .with_spellcheck(false)
        .with_text_action("translate", "Translate")
        .with_initial_value("hello world");
    ti.set_focus(true);
    ti.core.set_selection(0, 5);
    ti.open_context_menu([10.0, 10.0]);
    let named = |key| crate::synthetic_input::named_key(key);
    // Arrows move through the menu, not the caret or the selection.
    assert!(dispatch(&mut ti, &named(NamedKey::ArrowDown)));
    assert!(dispatch(&mut ti, &crate::synthetic_input::char_key("x")));
    assert_eq!(ti.value(), "hello world", "typing does not reach the text");
    assert_eq!(ti.selection_range(), Some((0, 5)));
    // Enter runs the highlighted row, the registered action.
    assert!(dispatch(&mut ti, &named(NamedKey::Enter)));
    assert!(!ti.context_menu.is_open());
    assert_eq!(ti.value(), "hello world", "Enter inserts no line");
    assert_eq!(
        ti.take_text_action_request().map(|request| request.id),
        Some("translate".to_owned())
    );

    // Escape closes the menu and leaves the field as it was.
    ti.open_context_menu([10.0, 10.0]);
    assert!(dispatch(&mut ti, &named(NamedKey::Escape)));
    assert!(!ti.context_menu.is_open());
    assert_eq!(ti.selection_range(), Some((0, 5)));
}

#[test]
fn a_lowercase_pronoun_is_corrected_at_the_next_word_boundary() {
    // Motion reads at its end: this test checks the resting frame.
    let _settled = crate::motion::testing::settled();
    for (typed, corrected) in [("so i", "so I "), ("im", "I'm ")] {
        let mut ti = suggesting_field(typed);
        let start = crate::time::now();
        ti.refresh_suggestion_at(start);
        ti.refresh_suggestion_at(start + SUGGESTION_SETTLE);
        let preview = ti
            .autocorrection_preview
            .clone()
            .expect("the pronoun is corrected although word lists hold \"i\"");
        assert!(preview.automatic, "{typed}: a sure correction");
        ti.insert_word_boundary(" ");
        assert_eq!(ti.value(), corrected);
    }
}

/// "cant" and "wont" become "can't" and "won't" at the next space, as a
/// phone keyboard does; Backspace right after takes the correction back,
/// and the word taken back stays as typed when the space comes again.
#[test]
fn cant_and_wont_are_corrected_and_backspace_takes_a_correction_back_for_good() {
    // Motion reads at its end: this test checks the resting frame.
    let _settled = crate::motion::testing::settled();
    for (typed, corrected) in [("I cant", "I can't "), ("it wont", "it won't ")] {
        let mut ti = TextInput::local()
            .with_spellcheck(true)
            .with_autocorrect(true)
            .with_spell_language("en_US")
            .with_initial_value(typed);
        ti.core.set_caret(ti.text.len());
        ti.insert_word_boundary(" ");
        assert_eq!(ti.value(), corrected);

        let mut handled = false;
        ti.handle_backspace_key(false, &mut handled);
        assert!(handled);
        assert_eq!(ti.value(), typed, "Backspace restores the word as typed");
        ti.insert_word_boundary(" ");
        assert_eq!(
            ti.value(),
            format!("{typed} "),
            "a correction taken back is not made again"
        );
    }
}

#[test]
fn arrow_keys_cross_the_rows_a_paragraph_wraps_onto() {
    let _settled = crate::motion::testing::settled();
    let text = "alpha beta gamma delta epsilon zeta eta theta";
    let mut ti = TextInput::local().multiline(true).with_initial_value(text);
    ti.set_focus(true);
    ti.apply_layout_external(Rect::new(0.0, 0.0, 120.0, 400.0));
    draw_to_populate(&mut ti);
    assert!(ti.multiline_shaped);
    let rows = ti
        .single_layout
        .as_ref()
        .map_or(0, |layout| layout.buffer.layout_runs().count());
    assert!(rows > 2, "the paragraph wraps onto {rows} rows");
    let mut doc = AutoCommit::new();
    ti.set_cursor_pos(0);
    for (key, end) in [(NamedKey::ArrowRight, text.len()), (NamedKey::ArrowLeft, 0)] {
        let mut presses = 0;
        while ti.cursor_pos() != end && presses <= 2 * text.len() {
            let before = ti.cursor_pos();
            drive(&mut ti, &mut doc, &named_key(key), mods(false, false));
            presses += 1;
            assert_ne!(
                ti.cursor_pos(),
                before,
                "{key:?} stopped at byte {before} after {presses} presses"
            );
        }
        assert_eq!(ti.cursor_pos(), end, "{key:?} reaches the end of the text");
    }
}
