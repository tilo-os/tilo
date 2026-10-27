use super::*;
use crate::widget::{Button, Field, Form, FormSection, Manual, MeasureConstraint, TextInput};

#[test]
fn manual_composition_preserves_focus_selection_clicks_and_children_across_frames() {
    // Case `manual_and_mixed_hosts_preserve_focus_selection_and_authored_clicks_on_resize`.
    {
        for mode in 0..3 {
            let shift = std::rc::Rc::new(std::cell::Cell::new(0.25_f64));
            let offset = shift.clone();
            let mut input = TextInput::local()
                .with_id("name")
                .with_accessible_name("Name");
            input.set_text("Alex Morgan");
            let child: Box<dyn Widget> = if mode == 1 {
                Box::new(
                    Form::new()
                        .section(FormSection::new("Profile").field(Field::new("Name", input))),
                )
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
            .child(Box::new(
                Button::new("save", "Save").on_click(crate::testing::record_click("saved")),
            ));
            let root: Box<dyn Widget> = if mode == 2 {
                Box::new(
                    Form::new()
                        .section(FormSection::new("Automatic parent").content(Box::new(root))),
                )
            } else {
                Box::new(root)
            };
            let mut app = App::new_with_controlled_inputs(State::new(), root);
            app.init_layout();
            let run = |app: &mut App, width| {
                app.layout.layout_dirty = true;
                run_layout_pass(
                    app,
                    app.layout.root_node.unwrap(),
                    width,
                    800,
                    1.0,
                    false,
                    None,
                )
                .unwrap();
            };
            run(&mut app, 900);
            app.set_focused_node(Some(app.focusable_order[0]));
            app.mutate_node(app.focused_node.unwrap(), |widget| {
                crate::undo::set_state(widget, "8:b:2")
            })
            .unwrap();
            for (width, x) in [(340, 0.25), (900, 5.75), (900, 0.25)] {
                shift.set(x);
                run(&mut app, width);
                let focused = app.focused_node.unwrap();
                assert_eq!(
                    app.inspect_node(focused, |widget| (
                        widget.test_id().map(str::to_owned),
                        crate::undo::get_state(widget),
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
                app.set_focused_node(Some(app.focusable_order[0]));
            }
            assert!(crate::testing::clicks("saved") > 0);
        }
    }

    // Case `removing_last_manual_region_restores_host_default_rounding`.
    {
        let manual = Manual::new(|ctx| {
            Ok(taffy::Size {
                width: ctx.available().width,
                height: 100.0,
            })
        });
        let mut app = App::new_with_controlled_inputs(State::new(), Box::new(manual));
        app.init_layout();
        let node = app.layout.root_node.unwrap();
        run_layout_pass(&mut app, node, 500, 300, 1.0, false, None).unwrap();
        app.root_widget = Box::new(
            crate::widget::Container::new()
                .with_no_padding()
                .with_size(100.25, 40.25),
        );
        app.layout.structure_dirty = true;
        app.layout.layout_dirty = true;
        let node = app.layout.root_node.unwrap();
        run_layout_pass(&mut app, node, 500, 300, 1.0, false, None).unwrap();
        assert_eq!(app.root_widget.get_rect().width(), 100.0);
        assert_eq!(app.root_widget.get_rect().height(), 40.0);
    }

    // Case `manual_policy_input_moves_clean_same_viewport_frame_without_replacing_children`.
    {
        use std::{cell::Cell, rc::Rc};
        use vello::kurbo::Rect;
        for mode in 0..3 {
            let shift = Rc::new(Cell::new(0.25));
            let offset = shift.clone();
            let calls = Rc::new(Cell::new(0));
            let evaluated = calls.clone();
            let mut input = TextInput::local()
                .with_id("name")
                .with_accessible_name("Name");
            input.set_text("Alex Morgan");
            let child: Box<dyn Widget> = if mode == 1 {
                Box::new(
                    Form::new()
                        .section(FormSection::new("Profile").field(Field::new("Name", input))),
                )
            } else {
                Box::new(input)
            };
            let manual = Manual::new(move |ctx| {
                evaluated.set(evaluated.get() + 1);
                let x = offset.get();
                ctx.place(0, Rect::new(x, 10.0, x + 260.0, 160.0))?;
                ctx.place(1, Rect::new(x, 180.0, x + 80.0, 220.0))?;
                Ok(taffy::Size {
                    width: ctx.available().width,
                    height: 240.0,
                })
            })
            .child(child)
            .child(Box::new(
                Button::new("save", "Save").on_click(crate::testing::record_click("saved")),
            ));
            let invalidation = manual.invalidation();
            let root: Box<dyn Widget> = if mode == 2 {
                Box::new(
                    Form::new()
                        .section(FormSection::new("Automatic parent").content(Box::new(manual))),
                )
            } else {
                Box::new(manual)
            };
            let mut app = App::new_with_controlled_inputs(State::new(), root);
            app.init_layout();
            let run = |app: &mut App| {
                app.layout.layout_dirty = true;
                run_layout_pass(
                    app,
                    app.layout.root_node.unwrap(),
                    900,
                    800,
                    1.0,
                    false,
                    None,
                )
                .unwrap();
            };
            let paint = |app: &mut App| {
                draw_frame_content(app, &mut vello::Scene::new(), 900, 800, 1.0, None);
                app.root_widget.mark_drawn();
                assert!(
                    !app.root_widget.layout_needs_measure(),
                    "painted tree must be clean"
                );
            };
            run(&mut app);
            let input_node = app.focusable_order[0];
            let save_node = app.focusable_order[1];
            app.set_focused_node(Some(input_node));
            app.mutate_node(input_node, |widget| crate::undo::set_state(widget, "8:b:2"))
                .unwrap();
            paint(&mut app);
            let input_before = app
                .inspect_node(input_node, |widget| widget.get_rect())
                .unwrap();
            let save_before = app
                .inspect_node(save_node, |widget| widget.get_rect())
                .unwrap();
            let before_calls = calls.get();
            run(&mut app);
            assert_eq!(
                calls.get(),
                before_calls,
                "unchanged layout/scroll frame must not solve policies"
            );
            paint(&mut app);
            shift.set(105.75);
            invalidation.invalidate();
            run(&mut app);
            let input_after = app
                .inspect_node(input_node, |widget| widget.get_rect())
                .unwrap();
            let save_after = app
                .inspect_node(save_node, |widget| widget.get_rect())
                .unwrap();
            assert_eq!(input_after.x0 - input_before.x0, 105.5);
            assert_eq!(save_after.x0 - save_before.x0, 105.5);
            assert_eq!(app.focused_node, Some(input_node));
            assert_eq!(
                app.inspect_node(input_node, |widget| (
                    crate::undo::get_state(widget),
                    widget.text_review().unwrap().1
                ))
                .unwrap(),
                (Some("8:b:2".into()), "Alex Morgan".into())
            );
            let a11y = app.build_a11y_tree_update(1.0);
            let save_access = a11y
                .nodes
                .iter()
                .find(|(_, node)| node.role() == accesskit::Role::Button)
                .unwrap();
            assert_eq!(
                save_access.1.bounds(),
                Some(accesskit::Rect::new(
                    save_after.x0,
                    save_after.y0,
                    save_after.x1,
                    save_after.y1
                ))
            );
            assert!(!app.dispatch_semantic_click(save_before.center().x, save_before.center().y));
            assert!(app.dispatch_semantic_click(save_after.center().x, save_after.center().y));
            assert!(crate::testing::clicks("saved") > 0);
            paint(&mut app);
            let settled_calls = calls.get();
            run(&mut app);
            assert_eq!(calls.get(), settled_calls);
        }
    }
}
