use std::cell::Cell;
use std::collections::HashMap;
use std::rc::Rc;
use taffy::AvailableSpace;
use taffy::Size;
use taffy::TaffyTree;
use tilo_ui_app::app::driver::Driver;
use tilo_ui_app::headless::InspectionNode;
use tilo_ui_app::widget::{Button, Column, Field, Form, FormSection, Row, Text, TextInput};
use tilo_ui_base::Theme;
use tilo_ui_base::widget::Element;
use tilo_ui_base::widget::LayoutInfo;
use tilo_ui_base::widget::Widget;
use tilo_ui_base::widget::WidgetLayoutError;
use tilo_ui_base::widget::bound_nodes::BoundNodeMap;
use tilo_ui_base::widget::manual::*;
use vello::Scene;
use vello::kurbo::Affine;
use vello::kurbo::Point;
use vello::kurbo::Rect;
fn find<'a>(node: &'a InspectionNode, id: &str) -> &'a InspectionNode {
    fn search<'a>(node: &'a InspectionNode, id: &str) -> Option<&'a InspectionNode> {
        if node.test_id.as_deref() == Some(id) {
            return Some(node);
        }
        node.children.iter().find_map(|child| search(child, id))
    }
    search(node, id).unwrap()
}
fn fixed(child: Element) -> Manual {
    Manual::new(|ctx| {
        ctx.place(0, Rect::new(10.25, 20.75, 130.75, 61.0))?;
        Ok(Size {
            width: ctx.available().width,
            height: 100.0,
        })
    })
    .child(child)
}
#[test]
fn manual_allocation_measures_real_subtrees_and_drives_semantics() {
    fn fractional_button_allocation_drives_inspection_semantics_and_click() {
        let mut harness = Driver::new(
            tilo_ui_app::State::new(),
            Box::new(fixed(Box::new(
                Button::new("action", "A very long action title")
                    .on_click(tilo_ui_app::testing::record_click("clicked")),
            ))),
            (400, 300),
        );
        let snapshot = harness.inspection();
        let button = find(&snapshot.root, "action");
        assert_eq!(button.rect, [10.0, 21.0, 121.0, 40.0]);
        assert!(button.focusable);
        let node_id = harness.a11y_node_named("A very long action title").unwrap();
        let accessible = harness
            .a11y_nodes()
            .into_iter()
            .find(|(id, _)| *id == node_id)
            .unwrap()
            .1;
        assert_eq!(
            accessible.bounds().unwrap(),
            accesskit::Rect::new(10.0, 21.0, 131.0, 61.0)
        );
        harness.click_at(70.0, 40.0);
        assert_eq!(tilo_ui_app::testing::clicks("clicked"), 1);
    }

    fn measures_real_wrapping_container_and_form_subtrees() {
        let seen = Rc::new(std::cell::RefCell::new(Vec::new()));
        let result = seen.clone();
        let region = Manual::new(move |ctx| {
        let narrow = ctx.measure(0, Size { width: MeasureConstraint::Exact(140.0), height: MeasureConstraint::Intrinsic })?;
        let wide = ctx.measure(0, Size { width: MeasureConstraint::Exact(600.0), height: MeasureConstraint::Intrinsic })?;
        result.borrow_mut().push((narrow.size, wide.size));
        ctx.place(0, Rect::new(0.25, 0.75, 140.25, f64::from(narrow.size.height) + 0.75))?;
        let form = ctx.measure(1, Size { width: MeasureConstraint::Exact(300.0), height: MeasureConstraint::Intrinsic })?;
        assert!(form.size.height > 80.0);
        ctx.place(1, Rect::new(150.0, 0.0, 450.0, f64::from(form.size.height)))?;
        Ok(Size { width: ctx.available().width, height: narrow.size.height.max(form.size.height) })
    })
    .child(Box::new(Column::new().with_no_padding().push(Box::new(Text::new("A long paragraph that must wrap onto several lines when constrained to a narrow column.")))))
    .child(Box::new(Form::new().section(FormSection::new("Profile").field(Field::new("Long account recovery email address", TextInput::local().with_id("email"))).field(Field::new("Name", TextInput::local().with_id("name"))))));
        let harness = Driver::new(tilo_ui_app::State::new(), Box::new(region), (600, 500));
        assert!(
            seen.borrow()
                .iter()
                .all(|(narrow, wide)| narrow.height > wide.height
                    && narrow.width == 140.0
                    && wide.width == 600.0)
        );
        let snapshot = harness.inspection();
        assert_eq!(snapshot.root.children[0].rect[0], 0.0);
        assert!(find(&snapshot.root, "name").rect[1] > find(&snapshot.root, "email").rect[1]);
    }

    fn row_and_column_keep_authored_allocation_separate_from_overflow() {
        for child in [
            Box::new(
                Column::new().with_no_padding().push(Box::new(
                    tilo_ui_app::widget::Container::new()
                        .with_no_padding()
                        .with_size(250.0, 90.0),
                )),
            ) as Element,
            Box::new(
                Row::new().no_wrap().push(Box::new(
                    tilo_ui_app::widget::Container::new()
                        .with_no_padding()
                        .with_size(250.0, 90.0),
                )),
            ) as Element,
        ] {
            let harness = Driver::new(
                tilo_ui_app::State::new(),
                Box::new(fixed(child)),
                (400, 300),
            );
            let snapshot = harness.inspection();
            assert_eq!(snapshot.root.children[0].rect, [10.0, 21.0, 121.0, 40.0]);
            let info = serde_json::to_value(snapshot).unwrap();
            assert!(
                info["root"]["layout"]["Manual"]["overflow"]
                    .as_bool()
                    .unwrap()
            );
        }
    }

    fractional_button_allocation_drives_inspection_semantics_and_click();
    measures_real_wrapping_container_and_form_subtrees();
    row_and_column_keep_authored_allocation_separate_from_overflow();
}
#[test]
fn manual_layout_errors_reject_invalid_geometry_admission_and_duplicates() {
    fn invalid_geometry_and_missing_children_are_errors() {
        let mut widget = Manual::new(|ctx| {
            ctx.place(0, Rect::new(0.0, 0.0, f64::NAN, 30.0))?;
            Ok(Size::ZERO)
        })
        .child(Box::new(Text::new("Text")));
        let mut tree = TaffyTree::new();
        let mut nodes = HashMap::new();
        let root = widget.build_layout(&mut tree, &Theme::dark(), &mut nodes);
        let mut bindings = tilo_ui_base::widget::bound_nodes::BoundNodeMap::new();
        unsafe {
            bindings.rebuild(&widget, &tree, root, &nodes);
        }
        let result = unsafe {
            tilo_ui_base::widget::compute_widget_layout(
                &mut tree,
                root,
                Size {
                    width: taffy::AvailableSpace::Definite(400.0),
                    height: taffy::AvailableSpace::Definite(300.0),
                },
                &nodes,
                &bindings,
                &mut tilo_ui_base::font_catalog::fixture_renderer(),
            )
        };
        assert!(matches!(
            result,
            Err(WidgetLayoutError::InvalidManualGeometry)
        ));
    }

    fn admission_is_required_for_owned_policy_and_mismatched_pointers_are_rejected() {
        let calls = Rc::new(Cell::new(0));
        let observed = calls.clone();
        let mut widget = Manual::new(move |ctx| {
            observed.set(observed.get() + 1);
            ctx.place(0, Rect::new(0.0, 0.0, 20.0, 20.0))?;
            Ok(Size {
                width: 100.0,
                height: 100.0,
            })
        })
        .child(Box::new(Text::new("Child")));
        let mut tree = TaffyTree::new();
        let mut nodes = HashMap::new();
        let root = widget.build_layout(&mut tree, &Theme::dark(), &mut nodes);
        let mut bindings = BoundNodeMap::new();
        let mut text = tilo_ui_base::font_catalog::fixture_renderer();
        let space = Size {
            width: AvailableSpace::Definite(300.0),
            height: AvailableSpace::Definite(200.0),
        };
        unsafe {
            tilo_ui_base::widget::compute_widget_layout(
                &mut tree, root, space, &nodes, &bindings, &mut text,
            )
            .unwrap();
        }
        assert_eq!(calls.get(), 0);
        unsafe {
            bindings.rebuild(&widget, &tree, root, &nodes);
        }
        let original = nodes[&root];
        let mut replacement = Text::new("Replacement");
        nodes.insert(root, &mut replacement as *mut dyn Widget);
        unsafe {
            tilo_ui_base::widget::compute_widget_layout(
                &mut tree, root, space, &nodes, &bindings, &mut text,
            )
            .unwrap();
        }
        assert_eq!(calls.get(), 0);
        nodes.insert(root, original);
        unsafe {
            tilo_ui_base::widget::compute_widget_layout(
                &mut tree, root, space, &nodes, &bindings, &mut text,
            )
            .unwrap();
        }
        assert!(calls.get() > 0);
    }

    fn duplicate_placement_is_rejected_instead_of_silently_replacing_geometry() {
        let mut widget = Manual::new(|ctx| {
            ctx.place(0, Rect::new(0.0, 0.0, 100.0, 30.0))?;
            ctx.place(0, Rect::new(20.0, 0.0, 120.0, 30.0))?;
            Ok(Size {
                width: 200.0,
                height: 100.0,
            })
        })
        .child(Box::new(Text::new("Child")));
        let mut tree = TaffyTree::new();
        let mut nodes = HashMap::new();
        let root = widget.build_layout(&mut tree, &Theme::dark(), &mut nodes);
        let mut bindings = BoundNodeMap::new();
        unsafe {
            bindings.rebuild(&widget, &tree, root, &nodes);
        }
        let result = unsafe {
            tilo_ui_base::widget::compute_widget_layout(
                &mut tree,
                root,
                Size {
                    width: AvailableSpace::Definite(300.0),
                    height: AvailableSpace::Definite(200.0),
                },
                &nodes,
                &bindings,
                &mut tilo_ui_base::font_catalog::fixture_renderer(),
            )
        };
        assert!(result.is_err(), "duplicate placement must be diagnosed");
    }

    fn impossible_padding_is_reported_with_requested_and_effective_rectangles() {
        let region = Manual::new(|ctx| {
            ctx.place(0, Rect::new(0.25, 0.75, 1.25, 1.75))?;
            Ok(Size {
                width: ctx.available().width,
                height: 100.0,
            })
        })
        .child(Box::new(Column::new().push(Box::new(Text::new("Content")))));
        let harness = Driver::new(tilo_ui_app::State::new(), Box::new(region), (400, 200));
        let snapshot = harness.inspection();
        let Some(LayoutInfo::Manual(info)) = snapshot.root.layout else {
            panic!("manual info")
        };
        let conflict = info
            .first_conflict
            .expect("padding cannot fit in one pixel");
        assert_eq!(conflict.child, 0);
        assert_eq!(conflict.requested, [0.25, 0.75, 1.0, 1.0]);
        assert!(conflict.effective[2] > 1.0 && conflict.effective[3] > 1.0);
        assert_eq!(
            snapshot.root.children[0].rect[2],
            f64::from(conflict.effective[2])
        );
    }

    invalid_geometry_and_missing_children_are_errors();
    admission_is_required_for_owned_policy_and_mismatched_pointers_are_rejected();
    duplicate_placement_is_rejected_instead_of_silently_replacing_geometry();
    impossible_padding_is_reported_with_requested_and_effective_rectangles();
}

#[test]
fn manual_overflow_is_painted_inspected_and_clipped_per_axis() {
    fn intentional_overflow_remains_drawn_when_region_allocation_is_outside_viewport() {
        for (child_y, origin, viewport) in [
            (100.75, Point::ZERO, Rect::new(0.0, 90.0, 300.0, 200.0)),
            (
                -100.75,
                Point::new(0.0, 100.75),
                Rect::new(0.0, 0.0, 300.0, 90.0),
            ),
        ] {
            let mut widget = Manual::new(move |ctx| {
                ctx.place(0, Rect::new(10.25, child_y, 130.75, child_y + 40.25))?;
                Ok(Size {
                    width: ctx.available().width,
                    height: 20.0,
                })
            })
            .child(Box::new(Button::new("overflow", "Visible overflow")));
            let mut tree = TaffyTree::new();
            let mut nodes = HashMap::new();
            let root = widget.build_layout(&mut tree, &Theme::dark(), &mut nodes);
            let mut bindings = BoundNodeMap::new();
            unsafe {
                bindings.rebuild(&widget, &tree, root, &nodes);
            }
            let mut text = tilo_ui_base::font_catalog::fixture_renderer();
            unsafe {
                tilo_ui_base::widget::compute_widget_layout(
                    &mut tree,
                    root,
                    Size {
                        width: AvailableSpace::Definite(300.0),
                        height: AvailableSpace::Definite(200.0),
                    },
                    &nodes,
                    &bindings,
                    &mut text,
                )
                .unwrap();
            }
            widget.apply_layout(&tree, root, origin);
            let presence = tilo_ui_base::presence::PresenceCtx::empty();
            presence.viewport.set(Some(viewport));
            let mut scene = Scene::new();
            widget.draw(&mut tilo_ui_base::draw::DrawCtx::new(
                &mut scene,
                Affine::IDENTITY,
                &mut text,
                &Theme::dark(),
                presence.draw_env(),
            ));
            assert!(
                !scene.encoding().draw_tags.is_empty(),
                "visible overflowing control was culled with its small region allocation"
            );
        }
    }

    fn nested_negative_x_overflow_is_painted_and_inspected() {
        nested_negative_overflow(true);
    }

    fn nested_negative_y_overflow_is_painted_and_inspected() {
        nested_negative_overflow(false);
    }

    fn nested_overflow_stops_at_a_scrollable_clip() {
        let inner = Manual::new(|ctx| {
            ctx.place(0, Rect::new(-100.0, -100.0, 0.0, -60.0))?;
            Ok(Size {
                width: 100.0,
                height: 20.0,
            })
        })
        .child(Box::new(Button::new("clipped", "Clipped grandchild")));
        let region = Manual::new(|ctx| {
            ctx.place(0, Rect::new(0.0, 0.0, 100.0, 20.0))?;
            Ok(Size {
                width: 100.0,
                height: 20.0,
            })
        })
        .child(Box::new(
            tilo_ui_app::widget::Scrollable::new_vertical().with_child(Box::new(inner)),
        ));
        let harness = Driver::new(tilo_ui_app::State::new(), Box::new(region), (300, 200));
        let snapshot = harness.inspection();
        let Some(LayoutInfo::Manual(info)) = snapshot.root.layout else {
            panic!("manual info")
        };
        assert_eq!(info.content_bounds, [0.0, 0.0, 100.0, 20.0]);
        assert!(!info.overflow);
    }

    intentional_overflow_remains_drawn_when_region_allocation_is_outside_viewport();
    nested_negative_x_overflow_is_painted_and_inspected();
    nested_negative_y_overflow_is_painted_and_inspected();
    nested_overflow_stops_at_a_scrollable_clip();
}

fn nested_negative_overflow(horizontal: bool) {
    let inner = Manual::new(move |ctx| {
        let (x, y) = if horizontal {
            (-100.0, 0.0)
        } else {
            (0.0, -100.0)
        };
        ctx.place(0, Rect::new(x, y, x + 100.0, y + 40.0))?;
        Ok(Size {
            width: 100.0,
            height: 20.0,
        })
    })
    .child(Box::new(Button::new("grandchild", "Visible")));
    let mut outer = Manual::new(|ctx| {
        ctx.place(0, Rect::new(0.0, 0.0, 100.0, 20.0))?;
        Ok(Size {
            width: 100.0,
            height: 20.0,
        })
    })
    .child(Box::new(inner));
    let mut tree = TaffyTree::new();
    let mut nodes = HashMap::new();
    let root = outer.build_layout(&mut tree, &Theme::dark(), &mut nodes);
    let mut bindings = BoundNodeMap::new();
    unsafe {
        bindings.rebuild(&outer, &tree, root, &nodes);
    }
    let mut text = tilo_ui_base::font_catalog::fixture_renderer();
    unsafe {
        tilo_ui_base::widget::compute_widget_layout(
            &mut tree,
            root,
            Size {
                width: AvailableSpace::Definite(300.0),
                height: AvailableSpace::Definite(200.0),
            },
            &nodes,
            &bindings,
            &mut text,
        )
        .unwrap();
    }
    let (origin, viewport, expected) = if horizontal {
        (
            Point::new(100.0, 0.0),
            Rect::new(0.0, 0.0, 90.0, 100.0),
            [-100.0, 0.0, 200.0, 40.0],
        )
    } else {
        (
            Point::new(0.0, 100.0),
            Rect::new(0.0, 0.0, 200.0, 90.0),
            [0.0, -100.0, 100.0, 120.0],
        )
    };
    outer.apply_layout(&tree, root, origin);
    let presence = tilo_ui_base::presence::PresenceCtx::empty();
    presence.viewport.set(Some(viewport));
    let mut scene = Scene::new();
    outer.draw(&mut tilo_ui_base::draw::DrawCtx::new(
        &mut scene,
        Affine::IDENTITY,
        &mut text,
        &Theme::dark(),
        presence.draw_env(),
    ));
    assert!(
        !scene.encoding().draw_tags.is_empty(),
        "visible nested grandchild was culled"
    );
    let info = outer.effective_layout().unwrap();
    assert_eq!(info.content_bounds, expected);
    assert!(info.overflow);
    assert_eq!(info.requested_bounds, [0.0, 0.0, 100.0, 20.0]);
    assert_eq!(info.allocation, [100.0, 20.0]);
    assert!(info.first_conflict.is_none());
    assert_eq!(
        outer.children()[0].get_rect().size(),
        vello::kurbo::Size::new(100.0, 20.0)
    );
}

/// A retained resize re-solves the region against what its parent allocates
/// now, so a region filling the window grows with it as well as shrinking.
/// Its node holds the size the policy answered last time; that must not
/// become the next solve's allocation.
#[test]
fn retained_resize_reallocates_a_filling_region_both_ways() {
    let seen = Rc::new(Cell::new(0.0_f32));
    let observed = seen.clone();
    let region = Manual::new(move |ctx| {
        let available = ctx.available();
        observed.set(available.width);
        ctx.place(0, Rect::new(0.0, 0.0, f64::from(available.width), 40.0))?;
        Ok(Size {
            width: available.width,
            height: 40.0,
        })
    })
    .child(Box::new(
        tilo_ui_app::widget::Container::new().with_no_padding(),
    ));
    let mut harness = Driver::new(tilo_ui_app::State::new(), Box::new(region), (400, 200));
    for width in [600_u32, 300, 800] {
        harness.resize((width, 200)).unwrap();
        assert!(
            !harness.last_relayout().rebuilt,
            "a resize re-solves the retained tree"
        );
        let snapshot = harness.inspection();
        #[allow(clippy::cast_precision_loss, reason = "small window widths")]
        let expected = width as f32;
        assert_eq!(seen.get(), expected, "the policy sees the new allocation");
        assert_eq!(snapshot.root.rect[2], f64::from(expected));
        assert_eq!(snapshot.root.children[0].rect[2], f64::from(expected));
    }
}
