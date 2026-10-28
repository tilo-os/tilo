use super::edit::Editor;
use super::layout::{BoxKind, layout, nearest_gap};
use super::model::{Node, Path, Slot, integral_ranges, parse, to_linear};
use crate::text_render::TextRenderer;

#[test]
fn integral_input_round_trips_and_preserves_nested_argument_commas() {
    for source in [
        "int(x,x,0,1)",
        "∫(x,x,0,1)",
        "int(max(x,y),x,-1,2)",
        "int(int(x,x,0,1),y,0,2)",
        "int(x^(2),x,0,sqrt(y^(2)))",
        "int(,,,)",
    ] {
        let expr = parse(source);
        let Node::Func { name, arg } = &expr.root[0] else {
            panic!("integral must remain an editable function: {source}");
        };
        assert_eq!(name, "int");
        assert!(integral_ranges(arg).is_some(), "{source}");
        let linear = to_linear(&expr);
        assert_eq!(parse(&linear), expr, "{source}: {linear}");
        assert!(!linear.contains('∫'), "engine form uses the int name");
    }
    assert_eq!(to_linear(&parse("∫")), "∫", "unfinished input is retained");
}

#[test]
fn integral_function_operands_do_not_consume_following_operators() {
    for (source, expected) in [
        ("1/int(x,x,0,1)+2", "(1)/(int(x,x,0,1))+2"),
        ("1/∫(x,x,0,1)+2", "(1)/(int(x,x,0,1))+2"),
        ("2^int(x,x,0,1)+3", "2^(int(x,x,0,1))+3"),
        ("2^∫(x,x,0,1)+3", "2^(int(x,x,0,1))+3"),
        ("1/sin(x)+2", "(1)/(sin(x))+2"),
        ("2^sqrt(x)+3", "2^(sqrt(x))+3"),
    ] {
        let expr = parse(source);
        assert_eq!(to_linear(&expr), expected);
        assert_eq!(parse(expected), expr);
    }
}

#[test]
fn integral_typing_and_paste_use_the_same_editable_function() {
    for source in [
        "int(x^2,x,0,1)",
        "∫(x^2,x,0,1)",
        "int(max(x,y),x,0,1)",
        "int(1/x,x,0,1)",
    ] {
        let mut typed = Editor::from_linear("");
        for character in source.chars() {
            typed.type_char(character);
        }
        let mut pasted = Editor::from_linear("");
        pasted.paste_linear(source);
        assert_eq!(typed.expr, pasted.expr, "{source}");
        assert_eq!(typed.caret, Path::root(1), "closing the call exits it");
        assert_eq!(parse(&typed.to_linear()), typed.expr);
    }

    let mut editor = Editor::from_linear("");
    for character in "int(".chars() {
        editor.type_char(character);
    }
    assert_eq!(editor.to_linear(), "int(,,,)");
    assert_eq!(editor.caret.steps, vec![(0, Slot::Arg)]);
    assert_eq!(editor.caret.offset, 0);
    editor.type_char('x');
    editor.type_char(',');
    editor.type_char('x');
    editor.type_char(',');
    editor.type_char('0');
    editor.type_char(',');
    editor.type_char('1');
    editor.type_char(')');
    assert_eq!(editor.to_linear(), "int(x,x,0,1)");
}

#[test]
fn integral_layout_places_limits_and_differential_with_editable_hit_targets() {
    let mut renderer = TextRenderer::new();
    let source = "int(x+1,x,0,2)";
    let expr = parse(source);
    let Node::Func { arg, .. } = &expr.root[0] else {
        panic!("expected integral");
    };
    let ranges = integral_ranges(arg).unwrap();
    let arranged = layout(&expr.root, &mut renderer, 22.0);
    let sign = arranged
        .boxes
        .iter()
        .find(|b| matches!(b.kind, BoxKind::Glyph { ch: '∫', size } if size > 22.0))
        .expect("a display-sized integral glyph");
    let differential = arranged
        .boxes
        .iter()
        .find(|b| matches!(&b.kind, BoxKind::Name { text, .. } if text == "d"))
        .expect("upright differential d");
    assert!(
        !arranged
            .boxes
            .iter()
            .any(|b| { matches!(&b.kind, BoxKind::Name { text, .. } if text == "int") })
    );
    let argument_steps = vec![(0, Slot::Arg)];
    let starts: Vec<_> = ranges
        .iter()
        .map(|range| {
            arranged
                .gaps
                .iter()
                .find(|gap| gap.steps == argument_steps && gap.index == range.start)
                .unwrap()
        })
        .collect();
    assert!(
        starts[3].bottom < starts[0].top,
        "upper bound above integrand"
    );
    assert!(
        starts[2].top > starts[0].bottom,
        "lower bound below integrand"
    );
    assert!(starts[0].x > sign.x + sign.w);
    assert!(differential.x > starts[0].x);
    assert!(starts[1].x >= differential.x + differential.w);
    assert!((starts[0].top - starts[1].top).abs() < 0.01);

    let expected = [
        "int(9x+1,x,0,2)",
        "int(x+1,9x,0,2)",
        "int(x+1,x,90,2)",
        "int(x+1,x,0,92)",
    ];
    for (index, gap) in starts.iter().enumerate() {
        let hit = nearest_gap(&arranged.gaps, gap.x, (gap.top + gap.bottom) * 0.5).unwrap();
        assert_eq!(hit.steps, argument_steps);
        assert_eq!(hit.index, ranges[index].start);
        let mut editor = Editor::from_linear(source);
        editor.caret = Path {
            steps: hit.steps.clone(),
            offset: hit.index,
        };
        editor.type_char('9');
        assert_eq!(editor.to_linear(), expected[index]);
        assert_eq!(parse(&editor.to_linear()), editor.expr);
    }
}

#[test]
fn integral_nested_paths_and_empty_slots_reach_real_model_rows() {
    let mut renderer = TextRenderer::new();
    for source in ["int(,,,)", "int(x,x,0,1+sqrt(y^(2)))", "sin(int(x,x,0,1))"] {
        let mut editor = Editor::from_linear(source);
        let arranged = layout(&editor.expr.root, &mut renderer, 22.0);
        for gap in &arranged.gaps {
            let row = editor
                .expr
                .row_at(&gap.steps)
                .expect("layout path addresses a model row");
            assert!(gap.index <= row.len(), "{source}: {gap:?}");
            assert!(gap.x >= 0.0 && gap.x <= arranged.width);
            assert!(gap.top >= -0.01 && gap.bottom <= arranged.height + 0.01);
        }
        if source.contains("sqrt") {
            let gap = arranged
                .gaps
                .iter()
                .find(|gap| {
                    gap.steps.last().is_some_and(|&(_, slot)| slot == Slot::Exp) && gap.index == 1
                })
                .expect("nested upper-bound exponent");
            let hit = nearest_gap(&arranged.gaps, gap.x, (gap.top + gap.bottom) * 0.5).unwrap();
            assert_eq!(
                hit.steps, gap.steps,
                "nested hit must retain full source path"
            );
            editor.caret = Path {
                steps: hit.steps.clone(),
                offset: hit.index,
            };
            editor.type_char('3');
            assert_eq!(editor.to_linear(), "int(x,x,0,1+sqrt(y^(23)))");
        }
    }
}

#[test]
fn integral_bounds_support_keyboard_navigation_and_deletion() {
    let mut editor = Editor::from_linear("int(x,x,0,1)");
    editor.caret = Path {
        steps: vec![(0, Slot::Arg)],
        offset: 0,
    };
    editor.move_up();
    assert_eq!(editor.caret.offset, 6, "up reaches the upper bound");
    editor.type_char('2');
    editor.backspace();
    assert_eq!(editor.to_linear(), "int(x,x,0,1)");
    editor.move_down();
    assert_eq!(editor.caret.offset, 4, "down reaches the lower bound");
    editor.type_char('-');
    assert_eq!(editor.to_linear(), "int(x,x,-0,1)");
}

#[test]
fn malformed_integral_calls_keep_their_arguments_visible() {
    let mut renderer = TextRenderer::new();
    for source in ["int(x,x,0)", "int(x,x,0,1,2)"] {
        let expr = parse(source);
        assert_eq!(to_linear(&expr), source);
        let arranged = layout(&expr.root, &mut renderer, 22.0);
        assert!(
            arranged
                .boxes
                .iter()
                .any(|b| { matches!(&b.kind, BoxKind::Name { text, .. } if text == "int") })
        );
        assert!(
            !arranged
                .boxes
                .iter()
                .any(|b| { matches!(b.kind, BoxKind::Glyph { ch: '∫', .. }) })
        );
    }
}

#[test]
fn integrals_serialize_as_typeset_integrals_in_tex_and_typst() {
    let expr = parse("int(x+1,x,0,2)");
    assert_eq!(super::tex::serialize_tex(&expr), r"\int_{0}^{2} x+1 \, dx");
    assert_eq!(
        super::typst::serialize_typst(&expr),
        "integral_(0)^(2) x+1 dif x"
    );
}
