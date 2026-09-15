use super::*;
use crate::widget::{Button, Field, Form, Manual, MeasureConstraint, Section, TextInput};

#[test]
fn manual_and_mixed_hosts_preserve_focus_selection_and_authored_clicks_on_resize() {
    for mode in 0..3 {
        let shift = std::rc::Rc::new(std::cell::Cell::new(0.25_f64));
        let offset = shift.clone();
        let mut input = TextInput::local()
            .with_id("name")
            .with_accessible_name("Name");
        input.set_text("Alex Morgan");
        let child: Box<dyn Widget> = if mode == 1 {
            Box::new(Form::new().section(Section::new("Profile").field(Field::new("Name", input))))
        } else {
            Box::new(input)
        };
        let root = Manual::new(move |ctx| {
            let width = (ctx.available().width - 40.0).max(0.0);
            let measured = ctx.measure(
                0,
                taffy::Size {
                    width: MeasureConstraint::Exact(width),
                    height: MeasureConstraint::Intrinsic,
                },
            )?;
            ctx.place(
                0,
                vello::kurbo::Rect::new(
                    20.0 + offset.get(),
                    20.75,
                    20.0 + offset.get() + f64::from(width),
                    20.75 + f64::from(measured.size.height),
                ),
            )?;
            ctx.place(
                1,
                vello::kurbo::Rect::new(
                    20.25,
                    f64::from(measured.size.height) + 40.0,
                    140.75,
                    f64::from(measured.size.height) + 80.25,
                ),
            )?;
            Ok(taffy::Size {
                width: ctx.available().width,
                height: measured.size.height + 100.0,
            })
        })
        .child(child)
        .child(Box::new(Button::new("save", "Save", Some("saved"))));
        let root: Box<dyn Widget> = if mode == 2 {
            Box::new(Form::new().section(Section::new("Automatic parent").content(Box::new(root))))
        } else {
            Box::new(root)
        };
        let mut app = App::new_with_controlled_inputs(State::new(), root);
        app.init_layout();
        let run = |app: &mut App, width| {
            app.layout_dirty = true;
            run_layout_pass(app, app.root_node.unwrap(), width, 800, 1.0, false, None).unwrap();
        };
        run(&mut app, 900);
        app.set_focused_node(Some(app.focusable_order[0]));
        app.mutate_node(app.focused_node.unwrap(), |widget| {
            widget.set_state("8:b:2")
        })
        .unwrap();
        for (width, x) in [(340, 0.25), (900, 5.75), (900, 0.25)] {
            shift.set(x);
            run(&mut app, width);
            let focused = app.focused_node.unwrap();
            assert_eq!(
                app.inspect_node(focused, |widget| (
                    widget.test_id().map(str::to_owned),
                    widget.get_state(),
                    widget.text_review().unwrap().1
                ))
                .unwrap(),
                (
                    Some("name".into()),
                    Some("8:b:2".into()),
                    "Alex Morgan".into()
                )
            );
            let save = app.focusable_order[1];
            let bounds = app.inspect_node(save, |widget| widget.get_rect()).unwrap();
            assert_eq!(bounds.width(), 120.5);
            assert!(app.dispatch_semantic_click(bounds.center().x, bounds.center().y));
            // Return keyboard focus to the same entry for the next resize.
            app.set_focused_node(Some(app.focusable_order[0]));
        }
        use automerge::ReadDoc;
        assert!(
            app.state
                .doc
                .get(automerge::ROOT, "saved")
                .unwrap()
                .is_some()
        );
    }
}

#[test]
fn removing_last_manual_region_restores_host_default_rounding() {
    let manual = Manual::new(|ctx| {
        Ok(taffy::Size {
            width: ctx.available().width,
            height: 100.0,
        })
    });
    let mut app = App::new_with_controlled_inputs(State::new(), Box::new(manual));
    app.init_layout();
    let node = app.root_node.unwrap();
    run_layout_pass(&mut app, node, 500, 300, 1.0, false, None).unwrap();
    app.root_widget = Box::new(Button::new("ordinary", "Ordinary", None).with_size(100.25, 40.25));
    app.structure_dirty = true;
    app.layout_dirty = true;
    let node = app.root_node.unwrap();
    run_layout_pass(&mut app, node, 500, 300, 1.0, false, None).unwrap();
    assert_eq!(app.root_widget.get_rect().width(), 100.0);
    assert_eq!(app.root_widget.get_rect().height(), 40.0);
}
