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

#[test]
fn left_right_move_by_atom() {
    let mut e = ed("x+1");
    // starts at end (offset 3)
    assert_eq!(e.caret.offset, 3);
    e.move_left(false);
    assert_eq!(e.caret.offset, 2);
    e.move_left(false);
    assert_eq!(e.caret.offset, 1);
    e.move_right(false);
    assert_eq!(e.caret.offset, 2);
}

#[test]
fn home_end_jump_to_row_edges() {
    let mut e = ed("sin(x)+2");
    e.home(false);
    assert_eq!(e.caret.offset, 0);
    e.end(false);
    // root row: Func, +, 2  => len 3
    assert_eq!(e.caret.offset, e.expr.root.len());
}

#[test]
fn left_into_and_out_of_fraction() {
    // (a)/(b): a single Frac node at root[0]
    let mut e = ed("(a)/(b)");
    assert_eq!(e.expr.root.len(), 1);
    e.home(false); // offset 0, before the frac
    e.move_right(false); // step INTO numerator
    assert_eq!(caret(&e).0, vec![(0, Slot::Num)]);
    assert_eq!(e.caret.offset, 0);
    e.move_right(false); // past 'a'
    assert_eq!(e.caret.offset, 1);
    e.move_right(false); // out to right of frac
    assert_eq!(caret(&e).0, Vec::<(usize, Slot)>::new());
    assert_eq!(e.caret.offset, 1);
}

#[test]
fn up_down_navigate_fraction_boxes() {
    let mut e = ed("(a)/(b)");
    // descend into denominator
    e.home(false);
    e.move_right(false); // into num
    e.move_down(); // num -> den
    assert_eq!(caret(&e).0, vec![(0, Slot::Den)]);
    e.move_up(); // den -> num
    assert_eq!(caret(&e).0, vec![(0, Slot::Num)]);
}

#[test]
fn up_into_exponent() {
    let mut e = ed("x^2");
    // root: Char(x), Sup{2}
    e.home(false);
    e.move_right(false); // past x
    e.move_right(false); // into exponent (Sup enters from left)
    assert_eq!(caret(&e).0, vec![(1, Slot::Exp)]);
}

// -- word-wise motion -------------------------------------------------------

#[test]
fn ctrl_left_right_jump_tokens() {
    let mut e = ed("12+sin");
    // root: 1 2 + s i n  (sin stays as chars since no paren)
    e.end(false);
    assert_eq!(e.caret.offset, 6);
    e.move_word_left(false); // over "sin"
    assert_eq!(e.caret.offset, 3);
    e.move_word_left(false); // over "+"
    assert_eq!(e.caret.offset, 2);
    e.move_word_left(false); // over "12"
    assert_eq!(e.caret.offset, 0);
    e.move_word_right(false); // to end of "12"
    assert_eq!(e.caret.offset, 2);
}

// -- selection --------------------------------------------------------------

#[test]
fn shift_arrows_extend_selection() {
    let mut e = ed("abc");
    e.end(false);
    e.move_left(true);
    e.move_left(true);
    let sel = e.selection().expect("selection");
    assert_eq!((sel.lo(), sel.hi()), (1, 3));
}

#[test]
fn shift_home_selects_to_start() {
    let mut e = ed("abc");
    e.end(false);
    e.home(true);
    let sel = e.selection().expect("sel");
    assert_eq!((sel.lo(), sel.hi()), (0, 3));
}

#[test]
fn ctrl_shift_word_selection() {
    let mut e = ed("12+34");
    e.end(false);
    e.move_word_left(true); // select "34"
    let sel = e.selection().expect("sel");
    assert_eq!((sel.lo(), sel.hi()), (3, 5));
}

#[test]
fn select_all_covers_row() {
    let mut e = ed("x+1");
    e.select_all();
    let sel = e.selection().expect("sel");
    assert_eq!((sel.lo(), sel.hi()), (0, 3));
}

#[test]
fn plain_arrow_collapses_selection_to_edge() {
    let mut e = ed("abcd");
    e.end(false);
    e.move_left(true);
    e.move_left(true); // select "cd"
    e.move_left(false); // collapse to left edge
    assert!(e.selection().is_none());
    assert_eq!(e.caret.offset, 2);
}

#[test]
fn double_click_selects_token() {
    let mut e = ed("12+abc");
    // token at index 4 ('b') -> word "abc" = [3,6)
    e.select_token_at(vec![], 4);
    let sel = e.selection().expect("sel");
    assert_eq!((sel.lo(), sel.hi()), (3, 6));
}

// -- delete / backspace incl. structure unwrap ------------------------------

#[test]
fn backspace_deletes_atom_before() {
    let mut e = ed("x+1");
    e.end(false);
    e.backspace();
    assert_eq!(e.to_linear(), "x+");
    assert_eq!(e.caret.offset, 2);
}

#[test]
fn backspace_deletes_selection() {
    let mut e = ed("abcd");
    e.end(false);
    e.move_left(true);
    e.move_left(true); // select "cd"
    e.backspace();
    assert_eq!(e.to_linear(), "ab");
}

#[test]
fn delete_removes_forward() {
    let mut e = ed("x+1");
    e.home(false);
    e.delete();
    assert_eq!(e.to_linear(), "+1");
}

#[test]
fn backspace_at_front_of_denominator_unwraps_fraction() {
    let mut e = ed("(a)/(b)");
    // place caret at the front of the denominator
    e.home(false);
    e.move_right(false); // into numerator
    e.move_down(); // into denominator
    e.home(false); // front of denominator box
    assert_eq!(caret(&e).0, vec![(0, Slot::Den)]);
    e.backspace(); // unwrap the fraction
    assert_eq!(caret(&e).0, Vec::<(usize, Slot)>::new());
    // num "a" + den "b" concatenated
    assert_eq!(e.to_linear(), "ab");
}

#[test]
fn backspace_at_front_of_exponent_unwraps_power() {
    let mut e = ed("x^2");
    e.home(false);
    e.move_right(false); // past x
    e.move_right(false); // into exponent
    e.home(false); // front of exponent
    e.backspace(); // unwrap
    assert_eq!(e.to_linear(), "x2");
}

// -- structured input building ---------------------------------------------

#[test]
fn slash_builds_fraction_and_enters_denominator() {
    let mut e = Editor::from_linear("");
    e.type_char('x');
    e.type_char('/');
    // now inside the denominator
    assert_eq!(caret(&e).0, vec![(0, Slot::Den)]);
    e.type_char('2');
    assert_eq!(e.to_linear(), "(x)/(2)");
}

#[test]
fn caret_builds_superscript() {
    let mut e = Editor::from_linear("");
    e.type_char('x');
    e.type_char('^');
    assert_eq!(caret(&e).0, vec![(1, Slot::Exp)]);
    e.type_char('2');
    assert_eq!(e.to_linear(), "x^(2)");
}

#[test]
fn sqrt_word_then_paren_builds_radical() {
    let mut e = Editor::from_linear("");
    for ch in "sqrt".chars() {
        e.type_char(ch);
    }
    e.type_char('(');
    assert_eq!(caret(&e).0, vec![(0, Slot::Radicand)]);
    e.type_char('x');
    assert_eq!(e.to_linear(), "sqrt(x)");
}

#[test]
fn function_name_then_paren_builds_call() {
    let mut e = Editor::from_linear("");
    for ch in "sin".chars() {
        e.type_char(ch);
    }
    e.type_char('(');
    assert!(matches!(
        e.expr.root.first(),
        Some(Node::Func { name, .. }) if name == "sin"
    ));
    e.type_char('x');
    assert_eq!(e.to_linear(), "sin(x)");
}

#[test]
fn pi_promotes_to_symbol() {
    let mut e = Editor::from_linear("");
    e.type_char('p');
    e.type_char('i');
    assert_eq!(e.expr.root, vec![Node::Char('π')]);
    assert_eq!(e.to_linear(), "π");
}

#[test]
fn star_becomes_middle_dot() {
    let mut e = Editor::from_linear("");
    e.type_char('2');
    e.type_char('*');
    e.type_char('x');
    assert_eq!(e.to_linear(), "2·x");
    // and the engine accepts middle-dot multiplication? It does not — but the
    // graph editors translate · back to * on serialize-for-engine (see
    // engine_linear). Here we only check the structural form.
}

#[test]
fn paren_pair_auto_inserts_and_close_exits() {
    let mut e = Editor::from_linear("");
    e.type_char('(');
    assert_eq!(caret(&e).0, vec![(0, Slot::Body)]);
    e.type_char('x');
    e.type_char(')'); // exit the group
    assert_eq!(caret(&e).0, Vec::<(usize, Slot)>::new());
    e.type_char('+');
    e.type_char('1');
    assert_eq!(e.to_linear(), "(x)+1");
}

// -- clipboard round-trip ---------------------------------------------------

#[test]
fn copy_selection_serializes_to_linear() {
    let mut e = ed("x+1");
    e.home(false);
    e.move_right(true);
    e.move_right(true); // select "x+"
    assert_eq!(e.selected_linear().as_deref(), Some("x+"));
}

#[test]
fn cut_then_paste_round_trips() {
    let mut e = ed("x+12");
    e.end(false);
    e.move_left(true);
    e.move_left(true); // select "12"
    let cut = e.selected_linear().unwrap();
    e.delete_selection();
    assert_eq!(e.to_linear(), "x+");
    e.paste_linear(&cut);
    assert_eq!(e.to_linear(), "x+12");
}

#[test]
fn paste_structured_string_parses_into_tree() {
    let mut e = Editor::from_linear("");
    e.paste_linear("(x+1)/(x-3)");
    assert!(matches!(e.expr.root.first(), Some(Node::Frac { .. })));
    assert_eq!(e.to_linear(), "(x+1)/(x-3)");
}

// -- model -> linear -> engine round-trip ----------------------------------

#[test]
fn round_trip_to_engine_for_many_formulas() {
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
}

#[test]
fn typed_fraction_compiles_in_engine() {
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
    assert!(crate::engine::compile(&linear, false, false).is_ok(), "{linear}");
    let _ = e;
}

/// Mirror the graph editors' serialize-for-engine step: middle-dot → `*`.
fn engine_linear(s: &str) -> String {
    s.replace('·', "*")
}

// -- click → caret position round-trip (real layout + hit-test) -------------

/// The caret must be placed by measuring real glyph advances with the same
/// font/size/weight the glyphs are drawn with. Independently measure the
/// cumulative advance to each glyph boundary, click there against the live
/// layout, and assert the caret lands at exactly that index. A regression to
/// "char-count × fixed advance" or a font/size/weight mismatch would drift the
/// landing offset and fail this.
#[test]
fn click_at_measured_glyph_boundary_places_caret_there() {
    use super::MathField;
    use ui::text_render::{TextRenderer, TextWeight};
    use vello::kurbo::{Point, Rect};

    let mut tr = TextRenderer::new();
    let size = 22.0f32;
    let mut f = MathField::new("x+12").with_size(size);
    let rect = Rect::new(10.0, 5.0, 320.0, 65.0);
    f.set_rect(rect, &mut tr);

    let chars = ['x', '+', '1', '2'];
    let origin = f.content_origin();
    let py = origin.y + 10.0; // anywhere inside the single row's band
    let mut cum = 0.0f64;
    for k in 0..chars.len() {
        let px = origin.x + cum + 0.5; // just right of boundary k
        f.on_press_cached(Point::new(px, py), false);
        assert!(f.editor.caret.steps.is_empty(), "root row expected at boundary {k}");
        assert_eq!(
            f.editor.caret.offset, k,
            "click at measured boundary {k} (x={px:.1}) must land at offset {k}",
        );
        let mut buf = [0u8; 4];
        let s = chars[k].encode_utf8(&mut buf);
        let (w, _) = tr.measure_weighted(s, size, None, TextWeight::Medium);
        cum += w as f64;
    }
    // Clicking past the last glyph lands at the row end.
    f.on_press_cached(Point::new(origin.x + cum + 0.5, py), false);
    assert_eq!(f.editor.caret.offset, chars.len(), "click past end lands at row end");
}

/// The drawn caret bar sits exactly at the active gap's x, which equals the
/// summed advance up to the caret index — so caret-index ⇄ pixel-x is a true
/// inverse of the hit-test above.
#[test]
fn caret_gap_x_matches_summed_glyph_advances() {
    use super::MathField;
    use ui::text_render::{TextRenderer, TextWeight};
    use vello::kurbo::Rect;

    let mut tr = TextRenderer::new();
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
        let mut buf = [0u8; 4];
        let s = ch.encode_utf8(&mut buf);
        let (w, _) = tr.measure_weighted(s, size, None, TextWeight::Medium);
        cum += w as f64;
    }
}

/// The layout cache must not rebuild while the content is unchanged, but must
/// rebuild after an edit — this is what keeps typing from re-shaping the whole
/// formula every frame.
#[test]
fn layout_is_cached_until_content_changes() {
    use super::MathField;
    use ui::text_render::TextRenderer;
    use vello::kurbo::Rect;

    let mut tr = TextRenderer::new();
    let mut f = MathField::new("x+1").with_size(22.0);
    f.set_rect(Rect::new(0.0, 0.0, 200.0, 60.0), &mut tr);
    let sig1 = f.layout_sig.clone();
    // Re-laying out with the same content keeps the same signature (no rebuild).
    f.set_rect(Rect::new(0.0, 0.0, 240.0, 60.0), &mut tr);
    assert_eq!(f.layout_sig, sig1, "unchanged content must not rebuild layout");
    // Editing the content changes the signature (forces a rebuild).
    f.set_source("x+12");
    f.set_rect(Rect::new(0.0, 0.0, 240.0, 60.0), &mut tr);
    assert_ne!(f.layout_sig, sig1, "edit must invalidate the cached layout");
}

// -- external sync ----------------------------------------------------------

#[test]
fn parse_to_linear_is_idempotent_for_normalized_forms() {
    for s in ["x^(2)", "sin(x)", "(a)/(b)", "sqrt(x)", "root(3,x)"] {
        assert_eq!(model::to_linear(&model::parse(s)), s, "round trip {s}");
    }
}
