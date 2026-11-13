//! The math editor's editing semantics, proven on the model and on the field's
//! key and pointer paths: caret motion, selection, deletion, clipboard, and the
//! model → linear → engine round-trip.

use super::edit::Editor;
use super::model::{self, Node, Slot};

fn ed(s: &str) -> Editor {
    Editor::from_linear(s)
}

fn caret(e: &Editor) -> (Vec<(usize, Slot)>, usize) {
    (e.caret.steps.clone(), e.caret.offset)
}

/// Caret motion, including stepping into and out of fraction boxes and
/// exponents.
#[test]
fn caret_motion_by_atom_word_row_edge_and_structure() {
    // phase left_right_move_by_atom
    {
        let mut e = ed("x+1");
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
        assert_eq!(e.caret.offset, e.expr.root.len(), "home_end: end");
    }
    // phase left_into_and_out_of_fraction
    {
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
        e.home(false);
        e.move_right(false); // past x
        e.move_right(false); // into exponent (Sup enters from left)
        assert_eq!(caret(&e).0, vec![(1, Slot::Exp)], "into_exponent");
    }
    // phase ctrl_left_right_jump_tokens
    {
        let mut e = ed("12+sin");
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

/// Every selecting gesture yields its `[lo, hi)`; a plain arrow collapses the
/// selection to its edge.
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

/// At the front of a box, Backspace unwraps the structure.
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

/// Typing builds structured nodes: fractions, superscripts, radicals, calls,
/// `pi` → π, `*` → `·`, auto-paired parens.
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
        // The `√` glyph must build a real radical, as `sqrt(` does, not a literal
        // `√` the engine cannot parse.
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
        assert_eq!(e.to_linear(), "sqrt(x+9)", "radical_glyph: text");
        let linear = engine_linear(&e.to_linear());
        assert!(
            crate::engine::compile(&linear, false, false).is_ok(),
            "{linear} must compile"
        );
    }
    // phase radical_glyph_wraps_the_selection
    {
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
        // The engine takes `*`: the editors translate `·` back on serialize.
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

/// The editor core behind Ctrl+C, Ctrl+X and Ctrl+V: deterministic, without the
/// system clipboard, which headless has not got.
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
        e.end(false);
        e.paste_linear(&copied);
        assert_eq!(e.to_linear(), "x+123123", "copy_paste: pasted");
    }
}

/// Parsed and typed formulas serialize to text the engine compiles, and
/// normalized forms round-trip exactly.
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

/// The graph editors' serialize step: middle-dot → `*`.
fn engine_linear(s: &str) -> String {
    s.replace('·', "*")
}

/// The field's own paths: a `√` builds a radical, a drag then Delete removes
/// exactly the dragged span, and Ctrl+X cuts the selected one.
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
        // The real press → drag → key path, not a model shortcut.
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
        // Deletion is asserted; the copy half routes to the `ui` clipboard core,
        // a no-op headless.
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

/// The integral glyph must pass the field's character filter.
#[test]
fn field_handle_key_accepts_the_integral_glyph() {
    use super::MathField;
    use winit::event::Modifiers;
    use winit::keyboard::Key;

    let mut field = MathField::new("");
    let modifiers = Modifiers::default();
    for ch in ["∫", "("] {
        assert!(field.handle_key(&Key::Character(ch.into()), &modifiers));
    }
    assert_eq!(field.linear(), "int(,,,)");
    assert_eq!(caret(&field.editor), (vec![(0, Slot::Arg)], 0));
    for ch in "x,x,0,1)".chars() {
        assert!(field.handle_key(&Key::Character(ch.to_string().into()), &modifiers));
    }
    assert_eq!(field.linear(), "int(x,x,0,1)");
    assert_eq!(caret(&field.editor), (vec![], 1));
}

/// The painted rectangles against the layout's own boundaries.
fn assert_field_selection_bands(field: &super::MathField, spans: &[(usize, usize)]) {
    use super::render::{caret_gap, row_gaps, selection_bands};
    use vello::kurbo::Rect;

    let selection = field.editor.selection().expect("selected range");
    let layout = field.layout.as_ref().expect("laid-out field");
    let gaps = row_gaps(layout, &selection.steps);
    let actual = selection_bands(&selection, &gaps, &field.editor.expr);
    let expected: Vec<_> = spans
        .iter()
        .map(|&(start, end)| {
            let a = caret_gap(layout, &selection.steps, start).expect("span start");
            let b = caret_gap(layout, &selection.steps, end).expect("span end");
            Rect::new(a.x, a.top, b.x, b.bottom)
        })
        .collect();
    assert_eq!(actual, expected, "selection {selection:?}");
    assert!(
        actual
            .iter()
            .all(|band| band.width() > 0.0 && band.height() > 0.0)
    );
}

/// Integral arguments share a model row, but a selection must not connect the
/// limits to the body or cover the differential.
#[test]
fn integral_selection_bands_follow_visual_arguments_across_boundaries() {
    use super::{MathField, render};
    use ui::text_render::TextRenderer;
    use vello::kurbo::{Point, Rect};
    use winit::event::Modifiers;
    use winit::keyboard::{Key, ModifiersState, NamedKey};

    type SelectionCase<'a> = (usize, usize, &'a [(usize, usize)]);
    let mut tr = TextRenderer::new();
    let shift = Modifiers::from(ModifiersState::SHIFT);
    let cases: &[SelectionCase<'_>] = &[
        (1, 11, &[(1, 2), (3, 5), (6, 8), (9, 11)]),
        (10, 0, &[(0, 2), (3, 5), (6, 8), (9, 10)]),
        (1, 4, &[(1, 2), (3, 4)]),
        (4, 7, &[(4, 5), (6, 7)]),
        (2, 3, &[]), // only a hidden comma
    ];
    for &(start, end, spans) in cases {
        let mut field = MathField::new("int(ab,xy,01,23)").with_size(22.0);
        field.set_rect(Rect::new(10.0, 5.0, 450.0, 200.0), &mut tr);
        let steps = vec![(0, Slot::Arg)];
        let gap = render::caret_gap(field.layout.as_ref().unwrap(), &steps, start).unwrap();
        let origin = field.content_origin();
        let point = Point::new(origin.x + gap.x, origin.y + (gap.top + gap.bottom) * 0.5);
        field.on_press_cached(point, false);
        field.on_release();
        assert_eq!(caret(&field.editor), (steps.clone(), start));
        let key = Key::Named(if end > start {
            NamedKey::ArrowRight
        } else {
            NamedKey::ArrowLeft
        });
        for _ in 0..start.abs_diff(end) {
            assert!(!field.handle_key(&key, &shift));
        }
        assert_eq!(caret(&field.editor), (steps, end));
        assert_field_selection_bands(&field, spans);
    }
}

/// Ordinary commas and nested fraction rows stay contiguous: a fraction inside
/// an integral is one atom.
#[test]
fn selection_bands_preserve_ordinary_rows_and_nested_fractions() {
    use super::{MathField, model::Path};
    use ui::text_render::TextRenderer;
    use vello::kurbo::Rect;
    use winit::event::Modifiers;
    use winit::keyboard::{Key, ModifiersState, NamedKey};

    let mut tr = TextRenderer::new();
    let shift = Modifiers::from(ModifiersState::SHIFT);
    let cases = [
        ("abcd", vec![], 1, 3),
        ("sin(a,b)", vec![(0, Slot::Arg)], 0, 3),
        ("int(a,b)", vec![(0, Slot::Arg)], 0, 3),
        ("int(a,b,c,d,e)", vec![(0, Slot::Arg)], 0, 9),
        ("(ab)/(cd)", vec![(0, Slot::Num)], 0, 2),
        ("(ab)/(cd)", vec![(0, Slot::Den)], 0, 2),
        ("((ab)/(cd))/(ef)", vec![], 0, 1),
        (
            "((ab)/(cd))/(ef)",
            vec![(0, Slot::Num), (0, Slot::Num)],
            0,
            2,
        ),
        ("int((ab)/(cd),x,0,1)", vec![(0, Slot::Arg)], 0, 1),
        (
            "int((ab)/(cd),x,0,1)",
            vec![(0, Slot::Arg), (0, Slot::Num)],
            1,
            2,
        ),
    ];
    for (source, steps, start, end) in cases {
        let mut field = MathField::new(source).with_size(22.0);
        field.set_rect(Rect::new(0.0, 0.0, 450.0, 200.0), &mut tr);
        field.editor.caret = Path {
            steps,
            offset: start,
        };
        for _ in start..end {
            field.handle_key(&Key::Named(NamedKey::ArrowRight), &shift);
        }
        assert_field_selection_bands(&field, &[(start, end)]);
    }

    let mut nested = MathField::new("sin(int(ab,xy,01,23))");
    nested.set_rect(Rect::new(0.0, 0.0, 450.0, 200.0), &mut tr);
    nested.editor.caret = Path {
        steps: vec![(0, Slot::Arg), (0, Slot::Arg)],
        offset: 0,
    };
    nested.handle_key(&Key::Named(NamedKey::End), &shift);
    assert_field_selection_bands(&nested, &[(0, 2), (3, 5), (6, 8), (9, 11)]);
}

/// Caret index and pixel x on the real layout: clicks land at measured glyph
/// boundaries, drawn gaps sit at summed advances, a remote cursor snaps to a
/// gap, and the layout cache rebuilds only when the content changes.
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
        // The advance to each boundary is measured independently, so a regression to
        // "char count × fixed advance" or a font or size mismatch fails here.
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
        f.on_press_cached(Point::new(origin.x + cum + 0.5, py), false);
        assert_eq!(
            f.editor.caret.offset,
            chars.len(),
            "click past end lands at row end"
        );
    }

    // phase caret_gap_x_matches_summed_glyph_advances
    {
        // The caret bar sits at the summed advance up to the caret index, the
        // inverse of the hit test above.
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
        // This is what keeps typing from re-shaping the formula every frame.
        let mut f = MathField::new("x+1").with_size(22.0);
        f.set_rect(Rect::new(0.0, 0.0, 200.0, 60.0), &mut tr);
        let sig1 = f.layout_sig.clone();
        f.set_rect(Rect::new(0.0, 0.0, 240.0, 60.0), &mut tr);
        assert_eq!(
            f.layout_sig, sig1,
            "unchanged content must not rebuild layout"
        );
        f.set_source("x+12");
        f.set_rect(Rect::new(0.0, 0.0, 240.0, 60.0), &mut tr);
        assert_ne!(f.layout_sig, sig1, "edit must invalidate the cached layout");
    }

    // phase remote_cursor_maps_to_a_drawn_caret_bar
    {
        // A peer's cursor maps onto the glyph layout as a caret bar snapped to the
        // nearest gap, in the peer's colour.
        let mut f = MathField::new("x+12").with_size(20.0);
        let rect = Rect::new(10.0, 5.0, 260.0, 45.0);
        f.set_rect(rect, &mut tr);

        let peer = Color::from_rgba8(0xec, 0x48, 0x99, 0xff); // pink "alice"
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

        let outside = Point::new(rect.x1 + 200.0, rect.center().y);
        assert!(
            f.remote_caret_bars(&[(outside, peer)]).is_empty(),
            "remote cursor outside the field paints no caret"
        );
    }
}

/// Content wider or taller than the field scrolls, with the caret kept in view,
/// instead of resizing it; an expandable card grows to its maximum, then
/// scrolls.
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
        let mut f = MathField::new("x+1").with_size(20.0);
        let rect = Rect::new(10.0, 5.0, 310.0, 45.0);
        f.set_rect(rect, &mut tr);
        assert!(!f.overflows(), "a short formula must not overflow");
        assert_eq!(f.scroll.x, 0.0, "short: no horizontal scroll");
        assert_eq!(f.scroll.y, 0.0, "short: no vertical scroll");
        assert!(
            (f.content_origin().x - (rect.x0 + 4.0)).abs() < 1e-9,
            "short: left-padded origin"
        );
    }

    // phase wheel_scrolls_within_bounds
    {
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

    {
        // A fraction and the 3D-default stacked root grow to fit; a
        // fraction-over-fraction clamps to the maximum and scrolls.

        // Under the maximum, the card grows to the formula's exact height.
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

        // Past the maximum the field reports overflow and scrolls, never clips.
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

/// An inactive row shows its formula from the start, not the clipped tail a
/// caret-at-end seed would scroll it to.
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

    // Wheel-scrolling an inactive row sticks: the next unfocused frame keeps the
    // offset instead of chasing a parked caret.
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

/// A subtraction reads as the true minus sign, while the engine's copy keeps
/// its ASCII hyphen.
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
