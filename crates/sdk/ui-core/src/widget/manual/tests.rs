use super::*;
use crate::headless::{FrameHarness, InspectionNode};
use crate::widget::{Button, Column, Field, Form, FormSection, Row, Text, TextInput};

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
        let mut harness = FrameHarness::new(
            crate::State::new(),
            Box::new(fixed(Box::new(Button::new(
                "action",
                "A very long action title",
                Some("clicked"),
            )))),
            (400, 300),
        );
        let snapshot = harness.inspection();
        let button = find(&snapshot.root, "action");
        assert_eq!(button.rect, [10.25, 20.75, 120.5, 40.25]);
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
            accesskit::Rect::new(10.25, 20.75, 130.75, 61.0)
        );
        harness.click_at(70.0, 40.0);
        use automerge::ReadDoc;
        assert!(
            harness
                .state_mut()
                .doc
                .get(automerge::ROOT, "clicked")
                .unwrap()
                .is_some()
        );
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
        let harness = FrameHarness::new(crate::State::new(), Box::new(region), (600, 500));
        assert!(
            seen.borrow()
                .iter()
                .all(|(narrow, wide)| narrow.height > wide.height
                    && narrow.width == 140.0
                    && wide.width == 600.0)
        );
        let snapshot = harness.inspection();
        assert_eq!(snapshot.root.children[0].rect[0], 0.25);
        assert!(find(&snapshot.root, "name").rect[1] > find(&snapshot.root, "email").rect[1]);
    }

    fn row_and_column_keep_authored_allocation_separate_from_overflow() {
        for child in [
            Box::new(
                Column::new().with_no_padding().push(Box::new(
                    crate::widget::Container::new()
                        .with_no_padding()
                        .with_size(250.0, 90.0),
                )),
            ) as Element,
            Box::new(
                Row::new().no_wrap().push(Box::new(
                    crate::widget::Container::new()
                        .with_no_padding()
                        .with_size(250.0, 90.0),
                )),
            ) as Element,
        ] {
            let harness =
                FrameHarness::new(crate::State::new(), Box::new(fixed(child)), (400, 300));
            let snapshot = harness.inspection();
            assert_eq!(snapshot.root.children[0].rect, [10.25, 20.75, 120.5, 40.25]);
            let info = serde_json::to_value(snapshot).unwrap();
            assert!(
                info["root"]["layout"]["Manual"]["overflow"]
                    .as_bool()
                    .unwrap()
            );
        }
    }

    fn exact_proposals_fill_intrinsic_proposals_unfill_and_at_most_caps() {
        let region = Manual::new(|ctx| {
            let natural = ctx.measure(
                0,
                Size {
                    width: MeasureConstraint::Intrinsic,
                    height: MeasureConstraint::Intrinsic,
                },
            )?;
            let bounded = ctx.measure(
                0,
                Size {
                    width: MeasureConstraint::AtMost(110.0),
                    height: MeasureConstraint::Intrinsic,
                },
            )?;
            let exact = ctx.measure(
                0,
                Size {
                    width: MeasureConstraint::Exact(300.5),
                    height: MeasureConstraint::Exact(80.25),
                },
            )?;
            assert!(natural.size.width > 110.0 && natural.size.height > 0.0);
            assert!(bounded.size.width <= 110.0 && bounded.size.height >= natural.size.height);
            assert_eq!(
                exact.size,
                Size {
                    width: 300.5,
                    height: 80.25
                }
            );
            ctx.place(0, Rect::new(0.0, 0.0, 300.5, 80.25))?;
            Ok(Size {
                width: ctx.available().width,
                height: 100.0,
            })
        })
        .child(Box::new(Column::new().with_no_padding().push(Box::new(
            Text::new("A paragraph that naturally exceeds the narrow requested maximum width."),
        ))));
        let _ = FrameHarness::new(crate::State::new(), Box::new(region), (400, 300));
    }

    fn manual_desired_width_is_honored_and_float_rects_use_logical_f32_precision() {
        let region = Manual::new(|ctx| {
            ctx.place(0, Rect::new(0.1, 0.2, 100.3, 40.4))?;
            Ok(Size {
                width: 200.5,
                height: 100.25,
            })
        })
        .child(Box::new(Button::new("float", "Float", None)));
        let harness = FrameHarness::new(crate::State::new(), Box::new(region), (500, 300));
        let snapshot = harness.inspection();
        assert_eq!(snapshot.root.rect[2], 200.5);
        let Some(LayoutInfo::Manual(info)) = snapshot.root.layout else {
            panic!("manual info")
        };
        assert!(
            info.first_conflict.is_none(),
            "representable f32 rounding is not a constraint failure"
        );
    }

    fractional_button_allocation_drives_inspection_semantics_and_click();
    measures_real_wrapping_container_and_form_subtrees();
    row_and_column_keep_authored_allocation_separate_from_overflow();
    exact_proposals_fill_intrinsic_proposals_unfill_and_at_most_caps();
    manual_desired_width_is_honored_and_float_rects_use_logical_f32_precision();
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
        let mut bindings = crate::bound_dispatch::BoundNodeMap::new();
        unsafe {
            bindings.rebuild(&widget, &tree, root, &nodes);
        }
        let result = unsafe {
            super::super::compute_widget_layout(
                &mut tree,
                root,
                Size {
                    width: taffy::AvailableSpace::Definite(400.0),
                    height: taffy::AvailableSpace::Definite(300.0),
                },
                &nodes,
                &bindings,
                &mut crate::font_catalog::fixture_renderer(),
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
        let mut text = crate::font_catalog::fixture_renderer();
        let space = Size {
            width: AvailableSpace::Definite(300.0),
            height: AvailableSpace::Definite(200.0),
        };
        unsafe {
            super::super::compute_widget_layout(
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
            super::super::compute_widget_layout(
                &mut tree, root, space, &nodes, &bindings, &mut text,
            )
            .unwrap();
        }
        assert_eq!(calls.get(), 0);
        nodes.insert(root, original);
        unsafe {
            super::super::compute_widget_layout(
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
            super::super::compute_widget_layout(
                &mut tree,
                root,
                Size {
                    width: AvailableSpace::Definite(300.0),
                    height: AvailableSpace::Definite(200.0),
                },
                &nodes,
                &bindings,
                &mut crate::font_catalog::fixture_renderer(),
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
        let harness = FrameHarness::new(crate::State::new(), Box::new(region), (400, 200));
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
fn nested_manual_measurement_settles_bounded_and_side_effect_free() {
    fn nested_manual_regions_settle_and_preserve_auto_sibling_allocation() {
        let manual = fixed(Box::new(fixed(Box::new(Button::new(
            "nested", "Nested", None,
        )))));
        let form = Form::new().section(
            FormSection::new("Mixed")
                .content(Box::new(manual))
                .field(Field::new("Name", TextInput::local().with_id("auto-input"))),
        );
        let harness = FrameHarness::new(crate::State::new(), Box::new(form), (800, 500));
        let snapshot = harness.inspection();
        let nested = find(&snapshot.root, "nested");
        assert_eq!(nested.rect[0], 20.5);
        assert_eq!(nested.rect[2], 120.5);
        assert!(find(&snapshot.root, "auto-input").rect[2] > 300.0);
        let json = serde_json::to_value(snapshot).unwrap();
        assert_eq!(
            json["root"]["layout"]["Form"]["density"]["origin"],
            "Pattern"
        );
    }

    fn probes_do_not_leave_hypothetical_form_decisions_in_final_inspection() {
        let region = Manual::new(|ctx| {
            let narrow = ctx.measure(
                0,
                Size {
                    width: MeasureConstraint::Exact(180.0),
                    height: MeasureConstraint::Intrinsic,
                },
            )?;
            let _wide = ctx.measure(
                0,
                Size {
                    width: MeasureConstraint::Exact(800.0),
                    height: MeasureConstraint::Intrinsic,
                },
            )?;
            ctx.place(0, Rect::new(0.0, 0.0, 180.0, f64::from(narrow.size.height)))?;
            Ok(Size {
                width: ctx.available().width,
                height: narrow.size.height,
            })
        })
        .child(Box::new(Form::new().section(
            FormSection::new("Profile").field(Field::new(
                "Recovery email",
                TextInput::local().with_id("email"),
            )),
        )));
        let harness = FrameHarness::new(crate::State::new(), Box::new(region), (900, 500));
        let snapshot = harness.inspection();
        fn field(node: &InspectionNode) -> Option<&InspectionNode> {
            if node.widget == "Field" {
                Some(node)
            } else {
                node.children.iter().find_map(field)
            }
        }
        let Some(LayoutInfo::Field(info)) = field(&snapshot.root).unwrap().layout else {
            panic!("missing info")
        };
        assert_eq!(info.resolved_placement, super::super::LabelPlacement::Above);
        assert_eq!(info.available_width, 180.0);
    }

    fn intrinsic_measurement_preserves_authored_fixed_child_size() {
        let region = Manual::new(|ctx| {
            let size = ctx
                .measure(
                    0,
                    Size {
                        width: MeasureConstraint::Intrinsic,
                        height: MeasureConstraint::Intrinsic,
                    },
                )?
                .size;
            assert_eq!(
                size,
                Size {
                    width: 96.5,
                    height: 44.25
                }
            );
            ctx.place(0, Rect::new(0.0, 0.0, 96.5, 44.25))?;
            Ok(Size {
                width: ctx.available().width,
                height: 60.0,
            })
        })
        .child(Box::new(
            crate::widget::Container::new()
                .with_no_padding()
                .with_size(96.5, 44.25),
        ));
        let _ = FrameHarness::new(crate::State::new(), Box::new(region), (300, 200));
    }

    fn manual_oscillation_and_recursive_measurement_are_bounded() {
        fn solve(mut widget: Element) -> Result<(), WidgetLayoutError> {
            let mut tree = TaffyTree::new();
            let mut nodes = HashMap::new();
            let root = widget.build_layout(&mut tree, &Theme::dark(), &mut nodes);
            let mut bindings = BoundNodeMap::new();
            unsafe {
                bindings.rebuild(widget.as_ref(), &tree, root, &nodes);
            }
            unsafe {
                super::super::compute_widget_layout(
                    &mut tree,
                    root,
                    Size {
                        width: AvailableSpace::Definite(400.0),
                        height: AvailableSpace::Definite(300.0),
                    },
                    &nodes,
                    &bindings,
                    &mut crate::font_catalog::fixture_renderer(),
                )
            }
        }
        let oscillating = Manual::new(|ctx| {
            Ok(Size {
                width: ctx.available().width,
                height: if ctx.available().height > 75.0 {
                    50.0
                } else {
                    100.0
                },
            })
        });
        assert!(matches!(
            solve(Box::new(oscillating)),
            Err(WidgetLayoutError::DidNotSettle)
        ));
        let mut nested: Element = Box::new(Text::new("Bottom"));
        for _ in 0..34 {
            nested = Box::new(
                Manual::new(|ctx| {
                    let size = ctx
                        .measure(
                            0,
                            Size {
                                width: MeasureConstraint::Exact(100.0),
                                height: MeasureConstraint::Intrinsic,
                            },
                        )?
                        .size;
                    ctx.place(
                        0,
                        Rect::from_origin_size(
                            (0.0, 0.0),
                            (f64::from(size.width), f64::from(size.height)),
                        ),
                    )?;
                    Ok(size)
                })
                .child(nested),
            );
        }
        assert!(matches!(
            solve(nested),
            Err(WidgetLayoutError::ManualNestingLimit)
        ));
    }

    fn policy_invalidation_waits_for_actual_apply_and_survives_failure_and_reentrant_change() {
        let fail = Rc::new(Cell::new(false));
        let invalidator = Rc::new(std::cell::RefCell::new(None::<ManualInvalidation>));
        let invalidate_during_policy = Rc::new(Cell::new(false));
        let (failed, handle, bump) = (
            fail.clone(),
            invalidator.clone(),
            invalidate_during_policy.clone(),
        );
        let mut manual = Manual::new(move |ctx| {
            if failed.get() {
                return Err(WidgetLayoutError::InvalidManualGeometry);
            }
            if bump.replace(false) {
                handle.borrow().as_ref().unwrap().invalidate();
            }
            Ok(Size {
                width: ctx.available().width,
                height: 100.0,
            })
        });
        let invalidation = manual.invalidation();
        *invalidator.borrow_mut() = Some(invalidation.clone());
        let mut tree = TaffyTree::new();
        let mut nodes = HashMap::new();
        let root = manual.build_layout(&mut tree, &Theme::dark(), &mut nodes);
        let mut bindings = BoundNodeMap::new();
        unsafe {
            bindings.rebuild(&manual, &tree, root, &nodes);
        }
        let mut text = crate::font_catalog::fixture_renderer();
        let solve = |tree: &mut TaffyTree<()>, text: &mut TextRenderer| unsafe {
            super::super::compute_widget_layout(
                tree,
                root,
                Size {
                    width: AvailableSpace::Definite(400.0),
                    height: AvailableSpace::Definite(300.0),
                },
                &nodes,
                &bindings,
                text,
            )
        };
        solve(&mut tree, &mut text).unwrap();
        manual.apply_layout(&tree, root, Point::ZERO);
        assert!(!manual.layout_needs_measure());
        invalidation.invalidate();
        // A hypothetical subtree probe may solve the policy but cannot acknowledge
        // the real region's input revision or change its applied placement.
        solve(&mut tree.clone(), &mut text).unwrap();
        assert!(manual.layout_needs_measure());
        fail.set(true);
        assert!(solve(&mut tree, &mut text).is_err());
        assert!(manual.layout_needs_measure());
        fail.set(false);
        invalidate_during_policy.set(true);
        solve(&mut tree, &mut text).unwrap();
        manual.apply_layout(&tree, root, Point::ZERO);
        assert!(
            manual.layout_needs_measure(),
            "invalidation during policy evaluation must remain pending"
        );
        solve(&mut tree, &mut text).unwrap();
        manual.apply_layout(&tree, root, Point::ZERO);
        assert!(!manual.layout_needs_measure());
    }

    nested_manual_regions_settle_and_preserve_auto_sibling_allocation();
    probes_do_not_leave_hypothetical_form_decisions_in_final_inspection();
    intrinsic_measurement_preserves_authored_fixed_child_size();
    manual_oscillation_and_recursive_measurement_are_bounded();
    policy_invalidation_waits_for_actual_apply_and_survives_failure_and_reentrant_change();
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
            .child(Box::new(Button::new("overflow", "Visible overflow", None)));
            let mut tree = TaffyTree::new();
            let mut nodes = HashMap::new();
            let root = widget.build_layout(&mut tree, &Theme::dark(), &mut nodes);
            let mut bindings = BoundNodeMap::new();
            unsafe {
                bindings.rebuild(&widget, &tree, root, &nodes);
            }
            let mut text = crate::font_catalog::fixture_renderer();
            unsafe {
                super::super::compute_widget_layout(
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
            let presence = PresenceCtx::empty();
            presence.viewport.set(Some(viewport));
            let mut scene = Scene::new();
            widget.draw_with_presence(
                &mut scene,
                Affine::IDENTITY,
                &mut text,
                &Theme::dark(),
                &presence,
                &crate::permissions::PermissionContext::permissive(),
            );
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
        .child(Box::new(Button::new("clipped", "Clipped grandchild", None)));
        let region = Manual::new(|ctx| {
            ctx.place(0, Rect::new(0.0, 0.0, 100.0, 20.0))?;
            Ok(Size {
                width: 100.0,
                height: 20.0,
            })
        })
        .child(Box::new(
            crate::widget::Scrollable::new_vertical().with_child(Box::new(inner)),
        ));
        let harness = FrameHarness::new(crate::State::new(), Box::new(region), (300, 200));
        let snapshot = harness.inspection();
        let Some(LayoutInfo::Manual(info)) = snapshot.root.layout else {
            panic!("manual info")
        };
        assert_eq!(info.content_bounds, [0.0, 0.0, 100.0, 20.0]);
        assert!(!info.overflow);
    }

    fn descendant_bounds_accumulate_anonymous_offsets_and_clip_each_axis_independently() {
        fn positioned(x: f32, y: f32, width: f32, height: f32) -> Style {
            Style {
                position: taffy::Position::Absolute,
                inset: taffy::Rect {
                    left: taffy::LengthPercentageAuto::length(x),
                    top: taffy::LengthPercentageAuto::length(y),
                    ..taffy::Rect::auto()
                },
                size: Size {
                    width: Dimension::length(width),
                    height: Dimension::length(height),
                },
                ..Style::default()
            }
        }
        let mut tree = TaffyTree::new();
        tree.disable_rounding();
        let leaf = tree
            .new_leaf(positioned(-60.0, -80.0, 100.0, 10.0))
            .unwrap();
        let mut clipped = positioned(12.5, 17.25, 100.0, 50.0);
        clipped.overflow.x = taffy::Overflow::Hidden;
        let anonymous = tree.new_with_children(clipped, &[leaf]).unwrap();
        let branch = tree
            .new_with_children(positioned(20.25, 30.75, 100.0, 50.0), &[anonymous])
            .unwrap();
        let root = tree.new_with_children(Style::default(), &[branch]).unwrap();
        tree.compute_layout(
            root,
            Size {
                width: AvailableSpace::Definite(300.0),
                height: AvailableSpace::Definite(200.0),
            },
        )
        .unwrap();
        assert_eq!(
            descendant_bounds(&tree, branch).unwrap(),
            Rect::new(0.0, -32.0, 132.75, 98.0)
        );
    }

    intentional_overflow_remains_drawn_when_region_allocation_is_outside_viewport();
    nested_negative_x_overflow_is_painted_and_inspected();
    nested_negative_y_overflow_is_painted_and_inspected();
    nested_overflow_stops_at_a_scrollable_clip();
    descendant_bounds_accumulate_anonymous_offsets_and_clip_each_axis_independently();
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
    .child(Box::new(Button::new("grandchild", "Visible", None)));
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
    let mut text = crate::font_catalog::fixture_renderer();
    unsafe {
        super::super::compute_widget_layout(
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
    let presence = PresenceCtx::empty();
    presence.viewport.set(Some(viewport));
    let mut scene = Scene::new();
    outer.draw_with_presence(
        &mut scene,
        Affine::IDENTITY,
        &mut text,
        &Theme::dark(),
        &presence,
        &crate::permissions::PermissionContext::permissive(),
    );
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
