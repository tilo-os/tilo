//! Behavioural tests for the math editor. The interactive field can't be
//! clicked in CI, so these prove the editing semantics on the model: caret
//! motion for each key combo, word motion, selection ranges, delete/backspace
//! (incl. structure unwrap), clipboard round-trip, structured-input building,
//! and model → linear → engine round-trips.

use super::edit::Editor;
use super::model::{self, Node, Slot};

fn ed(s: &str) -> Editor {
    Editor::from_linear(s)
}

/// Caret as `(steps, offset)` for assertions.
fn caret(e: &Editor) -> (Vec<(usize, Slot)>, usize) {
    (e.caret.steps.clone(), e.caret.offset)
}

// -- caret movement ---------------------------------------------------------

/// Plain, Home/End and word-wise caret motion, including stepping into and out
/// of fraction boxes and exponents. Each phase starts from a fresh editor.
#[test]
fn caret_motion_by_atom_word_row_edge_and_structure() {
    // phase left_right_move_by_atom
    {
        let mut e = ed("x+1");
        // starts at end (offset 3)
        assert_eq!(e.caret.offset, 3, "left_right: starts at end");
        e.move_left(false);
        assert_eq!(e.caret.offset, 2, "left_right: first left");
        e.move_left(false);
        assert_eq!(e.caret.offset, 1, "left_right: second left");
        e.move_right(false);
        assert_eq!(e.caret.offset, 2, "left_right: right");
    }
    // phase home_end_jump_to_row_edges
    {
        let mut e = ed("sin(x)+2");
        e.home(false);
        assert_eq!(e.caret.offset, 0, "home_end: home");
        e.end(false);
        // root row: Func, +, 2  => len 3
        assert_eq!(e.caret.offset, e.expr.root.len(), "home_end: end");
    }
    // phase left_into_and_out_of_fraction
    {
        // (a)/(b): a single Frac node at root[0]
        let mut e = ed("(a)/(b)");
        assert_eq!(e.expr.root.len(), 1, "into_frac: single frac node");
        e.home(false); // offset 0, before the frac
        e.move_right(false); // step INTO numerator
        assert_eq!(caret(&e).0, vec![(0, Slot::Num)], "into_frac: numerator");
        assert_eq!(e.caret.offset, 0, "into_frac: numerator front");
        e.move_right(false); // past 'a'
        assert_eq!(e.caret.offset, 1, "into_frac: past a");
        e.move_right(false); // out to right of frac
        assert_eq!(
            caret(&e).0,
            Vec::<(usize, Slot)>::new(),
            "into_frac: back at root"
        );
        assert_eq!(e.caret.offset, 1, "into_frac: right of frac");
    }
    // phase up_down_navigate_fraction_boxes
    {
        let mut e = ed("(a)/(b)");
        // descend into denominator
        e.home(false);
        e.move_right(false); // into num
        e.move_down(); // num -> den
        assert_eq!(caret(&e).0, vec![(0, Slot::Den)], "up_down: down to den");
        e.move_up(); // den -> num
        assert_eq!(caret(&e).0, vec![(0, Slot::Num)], "up_down: up to num");
    }
    // phase up_into_exponent
    {
        let mut e = ed("x^2");
        // root: Char(x), Sup{2}
        e.home(false);
        e.move_right(false); // past x
        e.move_right(false); // into exponent (Sup enters from left)
        assert_eq!(caret(&e).0, vec![(1, Slot::Exp)], "into_exponent");
    }
    // phase ctrl_left_right_jump_tokens
    {
        let mut e = ed("12+sin");
        // root: 1 2 + s i n  (sin stays as chars since no paren)
        e.end(false);
        assert_eq!(e.caret.offset, 6, "word: end");
        e.move_word_left(false); // over "sin"
        assert_eq!(e.caret.offset, 3, "word: over sin");
        e.move_word_left(false); // over "+"
        assert_eq!(e.caret.offset, 2, "word: over +");
        e.move_word_left(false); // over "12"
        assert_eq!(e.caret.offset, 0, "word: over 12");
        e.move_word_right(false); // to end of "12"
        assert_eq!(e.caret.offset, 2, "word: right to end of 12");
    }
}

// -- selection --------------------------------------------------------------

/// Every selecting gesture yields the exact `[lo, hi)` range; a plain arrow
/// collapses a selection to its edge.
#[test]
fn selection_ranges_for_each_selecting_gesture() {
    // Each case builds a fresh editor, runs a gesture, expects (lo, hi).
    #[allow(
        clippy::type_complexity,
        reason = "one row per gesture: name, text, gesture, expected range"
    )]
    let cases: [(&str, &str, fn(&mut Editor), (usize, usize)); 5] = [
        (
            "shift_arrows_extend_selection",
            "abc",
            |e| {
                e.end(false);
                e.move_left(true);
                e.move_left(true);
            },
            (1, 3),
        ),
        (
            "shift_home_selects_to_start",
            "abc",
            |e| {
                e.end(false);
                e.home(true);
            },
            (0, 3),
        ),
        (
            "ctrl_shift_word_selection",
            "12+34",
            |e| {
                e.end(false);
                e.move_word_left(true); // select "34"
            },
            (3, 5),
        ),
        ("select_all_covers_row", "x+1", |e| e.select_all(), (0, 3)),
        (
            "double_click_selects_token",
            "12+abc",
            |e| {
                // token at index 4 ('b') -> word "abc" = [3,6)
                e.select_token_at(vec![], 4);
            },
            (3, 6),
        ),
    ];
    for (name, src, gesture, want) in cases {
        let mut e = ed(src);
        gesture(&mut e);
        let sel = e
            .selection()
            .unwrap_or_else(|| panic!("case {name}: selection"));
        assert_eq!((sel.lo(), sel.hi()), want, "case {name}");
    }

    // case plain_arrow_collapses_selection_to_edge
    let mut e = ed("abcd");
    e.end(false);
    e.move_left(true);
    e.move_left(true); // select "cd"
    e.move_left(false); // collapse to left edge
    assert!(e.selection().is_none(), "collapse: selection cleared");
    assert_eq!(e.caret.offset, 2, "collapse: caret at left edge");
}

// -- delete / backspace incl. structure unwrap ------------------------------

/// Backspace/Delete remove the adjacent atom or the selection, and at the front
/// of a structure box unwrap the structure.
#[test]
fn delete_and_backspace_including_structure_unwrap() {
    // phase backspace_deletes_atom_before
    {
        let mut e = ed("x+1");
        e.end(false);
        e.backspace();
        assert_eq!(e.to_linear(), "x+", "backspace_atom: text");
        assert_eq!(e.caret.offset, 2, "backspace_atom: caret");
    }
    // phase backspace_deletes_selection
    {
        let mut e = ed("abcd");
        e.end(false);
        e.move_left(true);
        e.move_left(true); // select "cd"
        e.backspace();
        assert_eq!(e.to_linear(), "ab", "backspace_selection");
    }
    // phase delete_removes_forward
    {
        let mut e = ed("x+1");
        e.home(false);
        e.delete();
        assert_eq!(e.to_linear(), "+1", "delete_forward");
    }
    // phase backspace_at_front_of_denominator_unwraps_fraction
    {
        let mut e = ed("(a)/(b)");
        // place caret at the front of the denominator
        e.home(false);
        e.move_right(false); // into numerator
        e.move_down(); // into denominator
        e.home(false); // front of denominator box
        assert_eq!(caret(&e).0, vec![(0, Slot::Den)], "unwrap_frac: in den");
        e.backspace(); // unwrap the fraction
        assert_eq!(
            caret(&e).0,
            Vec::<(usize, Slot)>::new(),
            "unwrap_frac: caret at root"
        );
        // num "a" + den "b" concatenated
        assert_eq!(e.to_linear(), "ab", "unwrap_frac: text");
    }
    // phase backspace_at_front_of_exponent_unwraps_power
    {
        let mut e = ed("x^2");
        e.home(false);
        e.move_right(false); // past x
        e.move_right(false); // into exponent
        e.home(false); // front of exponent
        e.backspace(); // unwrap
        assert_eq!(e.to_linear(), "x2", "unwrap_power");
    }
}

// -- structured input building ---------------------------------------------

/// Typing builds structured nodes: `/` fractions, `^` superscripts, `sqrt(` and
/// `√` radicals, function calls, `pi` → π, `*` → `·`, auto-paired parens.
/// Every phase starts from a fresh editor.
#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one scenario, one labelled phase per merged case"
)]
fn typed_characters_build_structured_nodes() {
    // phase slash_builds_fraction_and_enters_denominator
    {
        let mut e = Editor::from_linear("");
        e.type_char('x');
        e.type_char('/');
        // now inside the denominator
        assert_eq!(caret(&e).0, vec![(0, Slot::Den)], "slash: caret in den");
        e.type_char('2');
        assert_eq!(e.to_linear(), "(x)/(2)", "slash: text");
    }
    // phase caret_builds_superscript
    {
        let mut e = Editor::from_linear("");
        e.type_char('x');
        e.type_char('^');
        assert_eq!(caret(&e).0, vec![(1, Slot::Exp)], "caret: in exponent");
        e.type_char('2');
        assert_eq!(e.to_linear(), "x^(2)", "caret: text");
    }
    // phase sqrt_word_then_paren_builds_radical
    {
        let mut e = Editor::from_linear("");
        for ch in "sqrt".chars() {
            e.type_char(ch);
        }
        e.type_char('(');
        assert_eq!(
            caret(&e).0,
            vec![(0, Slot::Radicand)],
            "sqrt_word: in radicand"
        );
        e.type_char('x');
        assert_eq!(e.to_linear(), "sqrt(x)", "sqrt_word: text");
    }
    // phase radical_glyph_builds_structured_sqrt
    {
        // Entering the `√` glyph must build a real radical (caret dropping into the
        // radicand), identical to typing `sqrt(` — not a dead literal `√` char the
        // engine can't parse. This is the `type_char('√') -> insert_sqrt()` wiring.
        let mut e = Editor::from_linear("");
        e.type_char('√');
        assert_eq!(
            caret(&e).0,
            vec![(0, Slot::Radicand)],
            "caret must drop into the radicand"
        );
        for ch in "x+9".chars() {
            e.type_char(ch);
        }
        assert!(
            matches!(e.expr.root.first(), Some(Node::Sqrt { index: None, .. })),
            "root must hold a structured radical, not a literal glyph"
        );
        // Serializes to `sqrt(...)` the engine accepts.
        assert_eq!(e.to_linear(), "sqrt(x+9)", "radical_glyph: text");
        let linear = engine_linear(&e.to_linear());
        assert!(
            crate::engine::compile(&linear, false, false).is_ok(),
            "{linear} must compile"
        );
    }
    // phase radical_glyph_wraps_the_selection
    {
        // `√` over a selection replaces it with an empty radical (insert_sqrt goes
        // through insert_node, which deletes the selection first).
        let mut e = ed("x+1");
        e.select_all();
        e.type_char('√');
        assert_eq!(
            caret(&e).0,
            vec![(0, Slot::Radicand)],
            "radical_over_selection: in radicand"
        );
        assert_eq!(
            e.to_linear(),
            "sqrt()",
            "the selection is replaced by the radical"
        );
    }
    // phase function_name_then_paren_builds_call
    {
        let mut e = Editor::from_linear("");
        for ch in "sin".chars() {
            e.type_char(ch);
        }
        e.type_char('(');
        assert!(
            matches!(
                e.expr.root.first(),
                Some(Node::Func { name, .. }) if name == "sin"
            ),
            "function_call: Func node"
        );
        e.type_char('x');
        assert_eq!(e.to_linear(), "sin(x)", "function_call: text");
    }
    // phase pi_promotes_to_symbol
    {
        let mut e = Editor::from_linear("");
        e.type_char('p');
        e.type_char('i');
        assert_eq!(e.expr.root, vec![Node::Char('π')], "pi: node");
        assert_eq!(e.to_linear(), "π", "pi: text");
    }
    // phase star_becomes_middle_dot
    {
        let mut e = Editor::from_linear("");
        e.type_char('2');
        e.type_char('*');
        e.type_char('x');
        assert_eq!(e.to_linear(), "2·x", "star: middle dot");
        // and the engine accepts middle-dot multiplication? It does not — but the
        // graph editors translate · back to * on serialize-for-engine (see
        // engine_linear). Here we only check the structural form.
    }
    // phase paren_pair_auto_inserts_and_close_exits
    {
        let mut e = Editor::from_linear("");
        e.type_char('(');
        assert_eq!(caret(&e).0, vec![(0, Slot::Body)], "paren: in body");
        e.type_char('x');
        e.type_char(')'); // exit the group
        assert_eq!(
            caret(&e).0,
            Vec::<(usize, Slot)>::new(),
            "paren: close exits"
        );
        e.type_char('+');
        e.type_char('1');
        assert_eq!(e.to_linear(), "(x)+1", "paren: text");
    }
}

// -- clipboard round-trip ---------------------------------------------------

/// Copy/cut serialize the selection to linear form and paste parses linear
/// text back into the tree — the editor core behind Ctrl+C / Ctrl+X / Ctrl+V,
/// deterministic and independent of the (headless-unavailable) system clipboard.
#[test]
fn clipboard_copy_cut_paste_round_trip_through_linear_form() {
    // phase copy_selection_serializes_to_linear
    {
        let mut e = ed("x+1");
        e.home(false);
        e.move_right(true);
        e.move_right(true); // select "x+"
        assert_eq!(e.selected_linear().as_deref(), Some("x+"), "copy");
    }
    // phase cut_then_paste_round_trips
    {
        let mut e = ed("x+12");
        e.end(false);
        e.move_left(true);
        e.move_left(true); // select "12"
        let cut = e.selected_linear().unwrap();
        e.delete_selection();
        assert_eq!(e.to_linear(), "x+", "cut: removed");
        e.paste_linear(&cut);
        assert_eq!(e.to_linear(), "x+12", "cut: pasted back");
    }
    // phase paste_structured_string_parses_into_tree
    {
        let mut e = Editor::from_linear("");
        e.paste_linear("(x+1)/(x-3)");
        assert!(
            matches!(e.expr.root.first(), Some(Node::Frac { .. })),
            "paste_structured: Frac node"
        );
        assert_eq!(e.to_linear(), "(x+1)/(x-3)", "paste_structured: text");
    }
    // phase copy_then_paste_round_trips_through_editor_core
    {
        let mut e = ed("x+123");
        e.end(false);
        e.move_left(true);
        e.move_left(true);
        e.move_left(true); // select "123"
        let copied = e.selected_linear().expect("copy serializes the selection");
        assert_eq!(copied, "123", "copy_paste: copied");
        // Paste at the end appends exactly the copied span (Ctrl+V's parse path).
        e.end(false);
        e.paste_linear(&copied);
        assert_eq!(e.to_linear(), "x+123123", "copy_paste: pasted");
    }
}

// -- model -> linear -> engine round-trip ----------------------------------

/// Model → linear → engine: parsed and typed formulas serialize to linear text
/// the engine compiles, and normalized forms round-trip exactly.
#[test]
fn linear_serialization_round_trips_and_compiles() {
    // phase round_trip_to_engine_for_many_formulas
    let cases = [
        "sin(x)",
        "0.25*x^2-2",
        "1/x",
        "(x^2+1)/(x-3)",
        "sqrt(x^2+1)",
        "cos(x)+sin(x)",
        "abs(x-1)",
        "x^3-3*x",
    ];
    for src in cases {
        let e = Editor::from_linear(src);
        let linear = engine_linear(&e.to_linear());
        assert!(
            crate::engine::compile(&linear, false, false).is_ok(),
            "{src} -> {linear} did not compile"
        );
    }

    // phase typed_fraction_compiles_in_engine
    {
        let mut e = Editor::from_linear("");
        for ch in "x+1".chars() {
            e.type_char(ch);
        }
        // wrap as a fraction over x: select all, then... simpler: build directly
        let mut e2 = Editor::from_linear("");
        e2.type_char('1');
        e2.type_char('/');
        e2.type_char('x');
        let linear = engine_linear(&e2.to_linear());
        assert!(
            crate::engine::compile(&linear, false, false).is_ok(),
            "typed_fraction: {linear}"
        );
        let _ = e;
    }

    // phase parse_to_linear_is_idempotent_for_normalized_forms (external sync)
    for s in ["x^(2)", "sin(x)", "(a)/(b)", "sqrt(x)", "root(3,x)"] {
        assert_eq!(model::to_linear(&model::parse(s)), s, "round trip {s}");
    }
}

/// Mirror the graph editors' serialize-for-engine step: middle-dot → `*`.
fn engine_linear(s: &str) -> String {
    s.replace('·', "*")
}

// -- real field key / pointer paths -----------------------------------------

/// The real `MathField` input paths edit content: a `√` Character event builds
/// a radical, press → drag → Delete removes exactly the dragged span, and
/// Ctrl+X cuts exactly the keyboard-selected span. Each phase uses a fresh field.
#[test]
fn field_key_and_pointer_paths_edit_content() {
    use super::MathField;
    use ui::text_render::TextRenderer;
    use vello::kurbo::{Point, Rect};
    use winit::event::Modifiers;
    use winit::keyboard::{Key as WKey, ModifiersState, NamedKey};

    let mut tr = TextRenderer::new();

    // phase field_key_path_accepts_radical_glyph
    {
        // Drive the real field key path: a `√` Character event routes through
        // handle_key -> type_char -> insert_sqrt and builds the radical.
        let mut f = MathField::new("").with_size(20.0);
        f.set_rect(Rect::new(0.0, 0.0, 200.0, 60.0), &mut tr);
        let mods = Modifiers::default();
        for ch in ["√", "x"] {
            assert!(
                f.handle_key(&WKey::Character(ch.into()), &mods),
                "{ch} must be accepted as a content change"
            );
        }
        assert_eq!(f.linear(), "sqrt(x)", "radical_key: text");
    }

    // phase drag_select_then_delete_removes_exactly_the_span
    {
        // Dragging the mouse selects exactly the dragged span, and Delete removes
        // exactly it — the real press → drag → key path, not a model shortcut.
        let mut f = MathField::new("x+123").with_size(20.0);
        f.set_rect(Rect::new(10.0, 5.0, 320.0, 45.0), &mut tr);
        let origin = f.content_origin();
        let py = origin.y + 10.0;
        let gap_x = |f: &MathField, k: usize| -> f64 {
            let g = f
                .layout
                .as_ref()
                .unwrap()
                .gaps
                .iter()
                .find(|g| g.steps.is_empty() && g.index == k)
                .unwrap();
            origin.x + g.x
        };
        let x2 = gap_x(&f, 2); // before '1'
        let x5 = gap_x(&f, 5); // end
        f.on_press_cached(Point::new(x2, py), false);
        f.on_drag_cached(Point::new(x5, py));
        let sel = f.editor.selection().expect("a drag selection");
        assert_eq!(
            (sel.lo(), sel.hi()),
            (2, 5),
            "drag must select exactly the dragged span"
        );
        f.on_release();
        let changed = f.handle_key(&WKey::Named(NamedKey::Delete), &Modifiers::default());
        assert!(changed, "Delete over a selection changes the content");
        assert_eq!(
            f.linear(),
            "x+",
            "Delete must remove exactly the selected span"
        );
    }

    // phase ctrl_x_cuts_the_selected_span_through_handle_key
    {
        // Ctrl+X over a selection cuts exactly that span through the field's real key
        // path (`handle_key`). Deletion is asserted deterministically; the copy half
        // routes to the shared `ui` clipboard core (a headless-safe no-op here).
        let mut f = MathField::new("x+123").with_size(20.0);
        f.set_rect(Rect::new(0.0, 0.0, 320.0, 45.0), &mut tr);
        // Select "123" via End then shift+Left ×3 — all through handle_key.
        f.handle_key(&WKey::Named(NamedKey::End), &Modifiers::default());
        let shift = Modifiers::from(ModifiersState::SHIFT);
        for _ in 0..3 {
            f.handle_key(&WKey::Named(NamedKey::ArrowLeft), &shift);
        }
        let ctrl = Modifiers::from(ModifiersState::CONTROL);
        let changed = f.handle_key(&WKey::Character("x".into()), &ctrl);
        assert!(changed, "Ctrl+X over a selection changes the content");
        assert_eq!(f.linear(), "x+", "Ctrl+X removes exactly the selected span");
    }
}

// -- click → caret position round-trip (real layout + hit-test) -------------

/// Caret index ⇄ pixel x on the real layout: clicks land at measured glyph
/// boundaries, drawn gaps sit at summed advances, remote cursors snap to a gap,
/// and the layout cache rebuilds only when content changes. Fresh field per phase.
#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one scenario, one labelled phase per merged case"
)]
fn field_layout_maps_carets_to_measured_glyph_positions() {
    use super::MathField;
    use ui::text_render::TextRenderer;
    use vello::kurbo::{Point, Rect};
    use vello::peniko::Color;

    let mut tr = TextRenderer::new();

    // phase click_at_measured_glyph_boundary_places_caret_there
    {
        // The caret must be placed by measuring real glyph advances with the same
        // font/size/weight the glyphs are drawn with. Independently measure the
        // cumulative advance to each glyph boundary, click there against the live
        // layout, and assert the caret lands at exactly that index. A regression to
        // "char-count × fixed advance" or a font/size/weight mismatch would drift the
        // landing offset and fail this.
        let size = 22.0f32;
        let mut f = MathField::new("x+12").with_size(size);
        let rect = Rect::new(10.0, 5.0, 320.0, 65.0);
        f.set_rect(rect, &mut tr);

        let chars = ['x', '+', '1', '2'];
        let origin = f.content_origin();
        let py = origin.y + 10.0; // anywhere inside the single row's band
        let mut cum = 0.0f64;
        for (k, &ch) in chars.iter().enumerate() {
            let px = origin.x + cum + 0.5; // just right of boundary k
            f.on_press_cached(Point::new(px, py), false);
            assert!(
                f.editor.caret.steps.is_empty(),
                "root row expected at boundary {k}"
            );
            assert_eq!(
                f.editor.caret.offset, k,
                "click at measured boundary {k} (x={px:.1}) must land at offset {k}",
            );
            cum += ui::math::measure_atom(&mut tr, ch, size);
        }
        // Clicking past the last glyph lands at the row end.
        f.on_press_cached(Point::new(origin.x + cum + 0.5, py), false);
        assert_eq!(
            f.editor.caret.offset,
            chars.len(),
            "click past end lands at row end"
        );
    }

    // phase caret_gap_x_matches_summed_glyph_advances
    {
        // The drawn caret bar sits exactly at the active gap's x, which equals the
        // summed advance up to the caret index — so caret-index ⇄ pixel-x is a true
        // inverse of the hit-test above.
        let size = 22.0f32;
        let mut f = MathField::new("abc").with_size(size);
        f.set_rect(Rect::new(0.0, 0.0, 200.0, 60.0), &mut tr);

        let mut cum = 0.0f64;
        for (k, ch) in ['a', 'b', 'c'].iter().enumerate() {
            let gap = f
                .layout
                .as_ref()
                .unwrap()
                .gaps
                .iter()
                .find(|g| g.steps.is_empty() && g.index == k)
                .expect("gap exists");
            assert!(
                (gap.x - cum).abs() < 0.01,
                "gap {k} x={} should equal summed advance {cum}",
                gap.x,
            );
            cum += ui::math::measure_atom(&mut tr, *ch, size);
        }
    }

    // phase layout_is_cached_until_content_changes
    {
        // The layout cache must not rebuild while the content is unchanged, but must
        // rebuild after an edit — this is what keeps typing from re-shaping the whole
        // formula every frame.
        let mut f = MathField::new("x+1").with_size(22.0);
        f.set_rect(Rect::new(0.0, 0.0, 200.0, 60.0), &mut tr);
        let sig1 = f.layout_sig.clone();
        // Re-laying out with the same content keeps the same signature (no rebuild).
        f.set_rect(Rect::new(0.0, 0.0, 240.0, 60.0), &mut tr);
        assert_eq!(
            f.layout_sig, sig1,
            "unchanged content must not rebuild layout"
        );
        // Editing the content changes the signature (forces a rebuild).
        f.set_source("x+12");
        f.set_rect(Rect::new(0.0, 0.0, 240.0, 60.0), &mut tr);
        assert_ne!(f.layout_sig, sig1, "edit must invalidate the cached layout");
    }

    // phase remote_cursor_maps_to_a_drawn_caret_bar
    {
        // A remote collaborator's cursor (a `RemoteCursorMoved` scene-space point)
        // must map onto the field's 2D glyph layout and produce a drawn caret bar,
        // snapped to the nearest insertion gap in the peer's colour — this is what
        // renders multi-cursor presence *inside* the structured formula.
        let mut f = MathField::new("x+12").with_size(20.0);
        let rect = Rect::new(10.0, 5.0, 260.0, 45.0);
        f.set_rect(rect, &mut tr);

        let peer = Color::from_rgba8(0xec, 0x48, 0x99, 0xff); // pink "alice"
        // A cursor near the middle of the formula.
        let mid = Point::new(rect.center().x, rect.center().y);
        let bars = f.remote_caret_bars(&[(mid, peer)]);
        assert_eq!(
            bars.len(),
            1,
            "a remote cursor over the field must map to one caret bar"
        );
        let (bar, col) = bars[0];
        assert_eq!(col, peer, "the caret must render in the peer's colour");
        assert!(
            bar.height() > 4.0,
            "the caret bar must have a visible height, got {}",
            bar.height()
        );
        assert!(
            bar.center().x >= rect.x0
                && bar.center().x <= rect.x1
                && bar.center().y >= rect.y0 - 0.5
                && bar.center().y <= rect.y1 + 0.5,
            "the mapped caret must sit inside the field rect: {bar:?} vs {rect:?}"
        );

        // A cursor OUTSIDE the field maps to nothing (a peer editing elsewhere must
        // not paint a phantom caret here).
        let outside = Point::new(rect.x1 + 200.0, rect.center().y);
        assert!(
            f.remote_caret_bars(&[(outside, peer)]).is_empty(),
            "remote cursor outside the field paints no caret"
        );
    }
}

// -- bounded field + scrolling ----------------------------------------------

/// The field is bounded: content wider or taller than it scrolls (clamped,
/// caret kept visible) instead of resizing it; short content does not scroll;
/// an expandable card grows to fit up to its max, then scrolls. Fresh field per
/// phase.
#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one scenario, one labelled phase per merged case"
)]
fn bounded_field_scrolls_instead_of_resizing() {
    use super::{CARD_MAX_H, CARD_MIN_H, MathField, card_view_height};
    use ui::text_render::TextRenderer;
    use vello::kurbo::Rect;
    use winit::event::Modifiers;
    use winit::keyboard::{Key as WKey, NamedKey};

    let mut tr = TextRenderer::new();

    // phase long_formula_scrolls_horizontally_to_keep_caret_visible
    {
        // A formula longer than the field must scroll horizontally so the caret stays
        // visible while typing — the field itself never grows.
        let mut f = MathField::new("").with_size(20.0);
        let rect = Rect::new(10.0, 5.0, 210.0, 45.0); // fixed 200 x 40 field
        f.set_rect(rect, &mut tr);
        let mods = Modifiers::default();

        // Type a long run, re-laying out after each keystroke like the live app.
        for _ in 0..60 {
            f.handle_key(&WKey::Character("9".into()), &mods);
            f.set_rect(rect, &mut tr);
        }

        assert!(
            f.overflows(),
            "a formula wider than the field must scroll, not resize it"
        );
        assert!(
            f.scroll.x > 0.0,
            "typing off the right edge must scroll right"
        );

        // The caret (at the end) is on-screen inside the field rect.
        let steps = f.editor.caret.steps.clone();
        let off = f.editor.caret.offset;
        let gap = super::render::caret_gap(f.layout.as_ref().unwrap(), &steps, off).unwrap();
        let caret_x = f.content_origin().x + gap.x;
        assert!(
            caret_x >= rect.x0 - 0.5 && caret_x <= rect.x1 + 0.5,
            "caret x={caret_x:.1} is outside the field [{}, {}]",
            rect.x0,
            rect.x1
        );

        // Jumping home scrolls back so the start is visible again.
        f.handle_key(&WKey::Named(NamedKey::Home), &mods);
        f.set_rect(rect, &mut tr);
        assert_eq!(f.scroll.x, 0.0, "Home must reveal the start of the formula");
        let gap0 = super::render::caret_gap(f.layout.as_ref().unwrap(), &[], 0).unwrap();
        let caret_x0 = f.content_origin().x + gap0.x;
        assert!(
            caret_x0 >= rect.x0 - 0.5 && caret_x0 <= rect.x1 + 0.5,
            "start caret x={caret_x0:.1} outside the field",
        );
    }

    // phase tall_formula_is_bounded_not_resized
    {
        // A formula taller than the field (a fraction) stays bounded to the field
        // rect and scrolls vertically instead of resizing the row — the exact bug the
        // user reported ("formulas resizing vertically with the height of the
        // formula").
        let mut f = MathField::new("(x^2+1)/(x-3)").with_size(20.0);
        let rect = Rect::new(0.0, 0.0, 240.0, 30.0); // deliberately shorter than a fraction
        f.set_rect(rect, &mut tr);

        let content_h = f.layout.as_ref().unwrap().height;
        assert!(
            content_h > rect.height(),
            "test fixture must be taller than the field ({content_h:.1} vs {:.1})",
            rect.height()
        );
        assert!(
            f.overflows(),
            "a taller-than-field formula must scroll, not grow the field"
        );
        // The field rect the caller gave is never mutated by content.
        assert_eq!(f.rect.height(), 30.0, "the field height must stay fixed");

        // The caret band is kept within the field vertically.
        let steps = f.editor.caret.steps.clone();
        let off = f.editor.caret.offset;
        if let Some(g) = super::render::caret_gap(f.layout.as_ref().unwrap(), &steps, off) {
            let oy = f.content_origin().y;
            assert!(
                oy + g.top >= rect.y0 - 0.5 && oy + g.bottom <= rect.y1 + 0.5,
                "caret band {:.1}..{:.1} escaped the field [{}, {}]",
                oy + g.top,
                oy + g.bottom,
                rect.y0,
                rect.y1
            );
        }
    }

    // phase short_formula_does_not_scroll
    {
        // A short formula that fits is unchanged: no scroll, content left-aligned and
        // vertically centered exactly as before the scroll support was added.
        let mut f = MathField::new("x+1").with_size(20.0);
        let rect = Rect::new(10.0, 5.0, 310.0, 45.0);
        f.set_rect(rect, &mut tr);
        assert!(!f.overflows(), "a short formula must not overflow");
        assert_eq!(f.scroll.x, 0.0, "short: no horizontal scroll");
        assert_eq!(f.scroll.y, 0.0, "short: no vertical scroll");
        // Left-padded origin, unchanged from the original centered behaviour.
        assert!(
            (f.content_origin().x - (rect.x0 + 4.0)).abs() < 1e-9,
            "short: left-padded origin"
        );
    }

    // phase wheel_scrolls_within_bounds
    {
        // The wheel scrolls a long field, and scrolling is clamped to the content.
        let mut f = MathField::new("1234567890123456789012345678901234567890").with_size(20.0);
        let rect = Rect::new(0.0, 0.0, 120.0, 40.0);
        f.set_rect(rect, &mut tr);
        assert!(f.overflows(), "wheel: fixture overflows");
        let (max_x, _) = f.scroll_bounds();
        // Over-scroll right: clamps to the max, never past the content.
        f.scroll_by(10_000.0, 0.0);
        assert!(
            (f.scroll.x - max_x).abs() < 1e-9,
            "over-scroll must clamp to max"
        );
        // Over-scroll left: clamps to 0.
        f.scroll_by(-10_000.0, 0.0);
        assert_eq!(f.scroll.x, 0.0, "wheel: over-scroll left clamps to 0");
    }

    // phase card_grows_to_fit_then_scrolls_beyond_max
    // (expandable card: grow to fit, then scroll — HIG §2)
    {
        // A card grows to fit its formula UP TO the max, then scrolls — never clips.
        // Proven on the real path across three regimes: a fraction (grows to fit, shown
        // in full), the 3D-default stacked surface (grows to fit, shown in full), and a
        // fraction-over-fraction that exceeds the max (clamps to the max, then scrolls).

        // A fraction is taller than a plain line but under the generous max: the
        // card grows to its exact height and shows it IN FULL (no scroll, no clip).
        for src in ["(x^2+1)/(x-3)", "sin(sqrt(x^2+y^2))"] {
            let mut f = MathField::new(src).with_size(20.0);
            let h = f.content_height(&mut tr);
            assert!(
                h > CARD_MIN_H && h <= CARD_MAX_H,
                "{src}: fixture must grow (taller than min, at/under max), got {h}"
            );
            let view = card_view_height(h);
            assert!(
                (view - h).abs() < 1e-9,
                "{src}: card must grow to fit the formula exactly ({view} vs {h})"
            );
            f.set_rect(Rect::new(0.0, 0.0, 260.0, view), &mut tr);
            assert!(
                !f.overflows(),
                "{src}: a formula that fits under the max must show in full"
            );
        }

        // A fraction-over-fraction exceeds even the generous max: the card clamps to
        // the max and the field reports overflow (scroll), never clip.
        let mut huge = MathField::new("(1/x)/(1/y)").with_size(20.0);
        let huge_h = huge.content_height(&mut tr);
        assert!(
            huge_h > CARD_MAX_H,
            "fixture must exceed the card max ({huge_h})"
        );
        let huge_view = card_view_height(huge_h);
        assert!(
            (huge_view - CARD_MAX_H).abs() < 1e-9,
            "a taller-than-max formula clamps the card to the max"
        );
        huge.set_rect(Rect::new(0.0, 0.0, 260.0, huge_view), &mut tr);
        assert!(
            huge.overflows(),
            "content beyond the card max must scroll within the card, not clip"
        );
    }
}

// -- field scrolling -------------------------------------------------------

/// Only the focused field follows its caret. An inactive row (a neighbouring
/// surface in the Graph 3D list) shows its formula from the start, instead of
/// the left-clipped tail the caret-at-end seed would otherwise scroll it to.
#[test]
fn unfocused_field_shows_its_start_while_the_focused_field_follows_the_caret() {
    use super::MathField;
    use ui::text_render::TextRenderer;
    use ui::theme::Theme;
    use vello::Scene;
    use vello::kurbo::{Affine, Rect};

    let mut tr = TextRenderer::new();
    let theme = Theme::dark();
    let mut field = MathField::new("0.05*(x^2+y^2)-3+0.25*(x^2+y^2)").with_size(20.0);
    let rect = Rect::new(0.0, 0.0, 120.0, 40.0);
    field.set_rect(rect, &mut tr);
    field.park_caret_at_end();
    assert!(
        field.overflows(),
        "the fixture formula must be wider than the field"
    );

    let mut scene = Scene::new();
    field.draw(
        &mut scene,
        Affine::IDENTITY,
        &mut tr,
        &theme,
        theme.text_primary,
        true,
    );
    assert!(
        field.scroll.x > 0.0,
        "a focused field scrolls to keep its caret in view"
    );
    field.draw(
        &mut scene,
        Affine::IDENTITY,
        &mut tr,
        &theme,
        theme.text_primary,
        false,
    );
    assert!(
        field.scroll.x.abs() < f64::EPSILON,
        "a field that just lost focus shows the formula from its start"
    );
    assert!((field.content_origin().x - (rect.x0 + MathField::PAD_X)).abs() < f64::EPSILON);

    // Wheel-scrolling an inactive row sticks: the next unfocused frame keeps
    // the offset (clamped) instead of chasing the parked caret or snapping
    // back to the start.
    field.scroll_by(30.0, 0.0);
    field.draw(
        &mut scene,
        Affine::IDENTITY,
        &mut tr,
        &theme,
        theme.text_primary,
        false,
    );
    assert!(
        (field.scroll.x - 30.0).abs() < f64::EPSILON,
        "an unfocused field keeps its wheel scroll, got {}",
        field.scroll.x
    );
    // Regaining focus resumes following the caret at the formula's end.
    field.draw(
        &mut scene,
        Affine::IDENTITY,
        &mut tr,
        &theme,
        theme.text_primary,
        true,
    );
    let (max_x, _) = field.scroll_bounds();
    assert!(
        (field.scroll.x - max_x).abs() < f64::EPSILON,
        "the focused field scrolls its end caret into view: {} vs {max_x}",
        field.scroll.x
    );
}

/// A subtraction reads as the true minus sign wherever it sits in the
/// notation, while the formula the engine receives keeps its ASCII hyphen.
#[test]
fn subtraction_is_drawn_with_the_true_minus_and_stored_with_the_hyphen() {
    use super::MathField;
    use super::layout::BoxKind;
    use ui::text_render::TextRenderer;

    let mut tr = TextRenderer::new();
    let mut field = MathField::new("(x^2+1)/(x-3)-2^-1").with_size(20.0);
    field.set_rect(vello::kurbo::Rect::new(0.0, 0.0, 400.0, 80.0), &mut tr);
    let layout = field.layout.as_ref().expect("laid out");
    let glyphs: Vec<char> = layout
        .boxes
        .iter()
        .filter_map(|b| match b.kind {
            BoxKind::Glyph { ch, .. } => Some(ch),
            _ => None,
        })
        .collect();
    assert!(
        !glyphs.contains(&'-'),
        "no ASCII hyphen is drawn: {glyphs:?}"
    );
    assert_eq!(
        glyphs.iter().filter(|&&ch| ch == '\u{2212}').count(),
        3,
        "the denominator, the subtraction and the exponent's sign"
    );
    let linear = field.linear();
    assert!(
        linear.contains("(x-3)-2") && !linear.contains('\u{2212}'),
        "the engine form keeps the hyphen: {linear}"
    );
}
