use tilo_ui_base::widget::layout::*;
use tilo_ui_base::widget::TaffyTreeExt;
use vello::kurbo::{Point, Rect};
use taffy::AvailableSpace;
use tilo_ui_base::widget::bound_nodes::BoundNodeMap;
use std::collections::HashMap;
use taffy::NodeId;
use taffy::Size;
use taffy::TaffyTree;
use tilo_ui_base::tokens::TextRole;
use tilo_ui_base::Theme;
use tilo_ui_base::widget::layout::Widget;

struct AccessCount {
    built: std::rc::Rc<std::cell::Cell<usize>>,
    focused: bool,
    id: Option<&'static str>,
}
impl Widget for AccessCount {
    fn name(&self) -> &'static str {
        "AccessCount"
    }
    fn build_layout(
        &mut self,
        tree: &mut TaffyTree<()>,
        _: &tilo_ui_base::Theme,
        nodes: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        tree.widget_leaf(taffy::Style::default(), nodes, self)
    }
    fn is_focusable(&self) -> bool {
        true
    }
    fn is_focused(&self) -> bool {
        self.focused
    }
    fn test_id(&self) -> Option<&str> {
        self.id
    }
    fn accessibility(&self) -> Option<tilo_ui_base::a11y::AccessNode> {
        self.built.set(self.built.get().saturating_add(1));
        Some(
            tilo_ui_base::a11y::AccessNode::new(tilo_ui_base::a11y::Role::Button).with_semantic_key("shared"),
        )
    }
}

tilo_test_env::scenario! {
    fn measures_and_authored_extents_resolve;

    phase authored_extents_and_rounding_resolve_by_sizing;
    {
        fn authored_extents_resolve_by_their_sizing() {
            let text = AuthoredExtent::text(760.0);
            let icon = AuthoredExtent::icon(86.0);
            assert_eq!(
                (text.resolve(&Theme::dark()), icon.resolve(&Theme::dark())),
                (760.0, 86.0)
            );
            let two = Theme::dark().with_text_scale(2.0);
            assert_eq!((text.resolve(&two), icon.resolve(&two)), (1520.0, 86.0));
            let small = Theme::dark().with_text_scale(0.75);
            assert_eq!(text.resolve(&small), 760.0, "text never shrinks an extent");
            let dense = Theme::dark().with_ui_scale(1.5);
            assert!((text.resolve(&dense) - 1140.0).abs() < 1e-9);
            assert!((icon.resolve(&dense) - 129.0).abs() < 1e-9);
            assert_eq!(text.with_px(300.0), AuthoredExtent::text(300.0));
            assert_eq!(icon.with_px(56.0).sizing(), ExtentSizing::Icon);
            assert_eq!(text.authored(), 760.0);
        }

        fn text_control_height_keeps_the_authored_height_until_the_line_outgrows_it() {
            let stock = Theme::dark();
            assert_eq!(text_control_height(34.0, &stock), 34.0);
            assert_eq!(text_control_height(36.0, &stock), 36.0);

            let line_1x = f64::from(stock.text(TextRole::Body).line_height_px());
            let two = Theme::dark().with_text_scale(2.0);
            let line_2x = f64::from(two.text(TextRole::Body).line_height_px());
            assert!(
                (line_2x - 2.0 * line_1x).abs() < 1e-3,
                "the theme's body line doubles"
            );
            let grown = text_control_height(34.0, &two);
            assert!(
                (grown - (line_2x + (34.0 - line_1x))).abs() < 1e-6,
                "34 px at 1× becomes the 2× line plus the same padding, got {grown}"
            );
            assert!(
                grown > 34.0 && grown < 68.0,
                "grows by the line, not by the whole scale"
            );

            let smaller = Theme::dark().with_text_scale(0.75);
            assert_eq!(
                text_control_height(34.0, &smaller),
                34.0,
                "smaller text never shrinks a control below its authored height"
            );

            let dense = Theme::dark().with_ui_scale(1.5);
            assert!(
                (text_control_height(34.0, &dense) - 51.0).abs() < 1e-6,
                "ui_scale scales the whole control"
            );
        }

        fn manual_allocation_at(scale: f64) -> Rect {
            use crate::widget::Button;
            use tilo_ui_base::widget::Manual;
            let mut widget = Manual::new(|ctx| {
                ctx.place(0, Rect::new(10.25, 20.75, 130.75, 61.0))?;
                Ok(Size {
                    width: ctx.available().width,
                    height: 100.0,
                })
            })
            .child(Box::new(Button::new("action", "Action")));
            let _grid = enter_device_scale(scale);
            let mut tree = TaffyTree::new();
            let mut nodes = HashMap::new();
            let root = widget.build_layout(&mut tree, &tilo_ui_base::Theme::dark(), &mut nodes);
            let mut bindings = BoundNodeMap::new();
            unsafe {
                bindings.rebuild(&widget, &tree, root, &nodes);
            }
            let mut text = tilo_ui_base::font_catalog::fixture_renderer();
            unsafe {
                compute_widget_layout(
                    &mut tree,
                    root,
                    Size {
                        width: AvailableSpace::Definite(400.0),
                        height: AvailableSpace::Definite(300.0),
                    },
                    &nodes,
                    &bindings,
                    &mut text,
                )
                .unwrap();
            }
            widget.apply_layout(&tree, root, Point::ZERO);
            assert!(
                widget
                    .effective_layout()
                    .is_some_and(|info| info.first_conflict.is_none()),
                "the allocation is the policy's, unrounded: no conflict"
            );
            widget.children()[0].get_rect()
        }

        fn manual_fractional_allocation_lands_on_device_pixels() {
            assert_eq!(
                manual_allocation_at(4.0),
                Rect::new(10.25, 20.75, 130.75, 61.0),
                "quarter pixels are device pixels at 4x"
            );
            assert_eq!(
                manual_allocation_at(1.0),
                Rect::new(10.0, 21.0, 131.0, 61.0)
            );
            let third = manual_allocation_at(1.5);
            for edge in [third.x0, third.y0, third.x1, third.y1] {
                assert!(
                    ((edge * 1.5) - (edge * 1.5).round()).abs() < 1e-3,
                    "{edge} is a device pixel at 1.5x: {third:?}"
                );
            }
        }

        fn pixel_rounding_at(scale: f64) -> Rect {
            let mut widget = crate::widget::Container::new()
                .with_no_padding()
                .with_size(100.25, 40.25);
            let _grid = enter_device_scale(scale);
            let mut tree = TaffyTree::new();
            let mut nodes = HashMap::new();
            let root = widget.build_layout(&mut tree, &tilo_ui_base::Theme::dark(), &mut nodes);
            let mut bindings = BoundNodeMap::new();
            unsafe {
                bindings.rebuild(&widget, &tree, root, &nodes);
            }
            unsafe {
                compute_widget_layout(
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
                .unwrap();
            }
            widget.apply_layout(&tree, root, Point::ZERO);
            widget.get_rect()
        }

        fn ordinary_hosts_round_to_the_display_pixels() {
            assert_eq!(pixel_rounding_at(1.0), Rect::new(0.0, 0.0, 100.0, 40.0));
            assert_eq!(pixel_rounding_at(2.0), Rect::new(0.0, 0.0, 100.5, 40.5));
        }

        authored_extents_resolve_by_their_sizing();
        text_control_height_keeps_the_authored_height_until_the_line_outgrows_it();
        manual_fractional_allocation_lands_on_device_pixels();
        ordinary_hosts_round_to_the_display_pixels();
    }

}

tilo_test_env::scenario! {
    fn focus_follows_identity_across_rebuilds;

    phase focus_captured_across_a_rebuild_builds_no_accessibility_node_for_a_named_target;
    {
        use crate::widget::Column;
        let built = std::rc::Rc::new(std::cell::Cell::new(0));
        let list = |focused: Option<Option<&'static str>>| {
            let mut column = Column::new();
            for _ in 0..50 {
                column = column.push(Box::new(AccessCount {
                    built: built.clone(),
                    focused: false,
                    id: None,
                }));
            }
            if let Some(id) = focused {
                column = column.push(Box::new(AccessCount {
                    built: built.clone(),
                    focused: true,
                    id,
                }));
            }
            column
        };
        assert!(CompositionFocus::capture_live(&list(None)).is_none());
        assert_eq!(built.get(), 0, "with nothing focused, no node is built");

        let focus = CompositionFocus::capture_live(&list(Some(Some("target")))).unwrap();
        assert_eq!(focus.key.as_deref(), Some("named:target"));
        assert!(!focus.ambiguous, "no other control has its name");
        assert_eq!(built.get(), 0, "a name alone identifies its target");

        built.set(0);
        let focus = CompositionFocus::capture_live(&list(Some(None))).unwrap();
        assert_eq!(focus.key.as_deref(), Some("semantic:shared"));
        assert!(focus.ambiguous, "the others answer to the same key");
    }

    phase focus_transfer_refuses_unnamed_ambiguous_or_unadmitted_targets;
    {
        fn unnamed_focus_has_no_transferable_snapshot_and_keeps_its_own_state() {
            let mut old = AnonymousFocusProbe::new("old selection");
            let mut tree = TaffyTree::new();
            let mut nodes = HashMap::new();
            let root = old.build_layout(&mut tree, &tilo_ui_base::Theme::dark(), &mut nodes);
            let mut bindings = BoundNodeMap::new();
            unsafe {
                bindings.rebuild(&old, &tree, root, &nodes);
            }
            let focus = unsafe { CompositionFocus::capture(Some(root), &nodes, &bindings) };
            assert!(
                focus.is_none(),
                "anonymous state must never be captured for transfer"
            );
            nodes.clear();
            bindings.clear();
            drop(old);
            let mut replacement = AnonymousFocusProbe::new("replacement selection");
            let root = replacement.build_layout(&mut tree, &tilo_ui_base::Theme::dark(), &mut nodes);
            unsafe {
                bindings.rebuild(&replacement, &tree, root, &nodes);
            }
            if let Some(focus) = focus {
                unsafe {
                    focus.restore(&nodes, &bindings, None);
                }
            }
            assert_eq!(replacement.state, "replacement selection");
            assert!(replacement.focused);
        }

        fn current_focus_refuses_unadmitted_disabled_excluded_and_unrelated_siblings() {
            use crate::widget::Column;
            let mut one = AnonymousFocusProbe::new("one");
            let mut tree = TaffyTree::new();
            let mut nodes = HashMap::new();
            let root = one.build_layout(&mut tree, &tilo_ui_base::Theme::dark(), &mut nodes);
            let mut bindings = BoundNodeMap::new();
            assert!(unsafe { CompositionFocus::current(&nodes, &bindings, None) }.is_none());
            unsafe {
                bindings.rebuild(&one, &tree, root, &nodes);
            }
            assert_eq!(
                unsafe { CompositionFocus::current(&nodes, &bindings, None) },
                Some(root)
            );
            assert!(unsafe { CompositionFocus::current(&nodes, &bindings, Some(&[])) }.is_none());
            one.enabled = false;
            assert!(unsafe { CompositionFocus::current(&nodes, &bindings, None) }.is_none());
            nodes.clear();
            bindings.clear();
            let mut siblings = Column::new()
                .push(Box::new(AnonymousFocusProbe::new("a")))
                .push(Box::new(AnonymousFocusProbe::new("b")));
            let root = siblings.build_layout(&mut tree, &tilo_ui_base::Theme::dark(), &mut nodes);
            unsafe {
                bindings.rebuild(&siblings, &tree, root, &nodes);
            }
            assert!(unsafe { CompositionFocus::current(&nodes, &bindings, None) }.is_none());
        }

        fn focus_never_uses_an_ordinal_or_an_ambiguous_semantic_key() {
            use crate::widget::{Button, Column};
            for target_keys in [
                ["other", "target"],
                ["target", "target"],
                ["other", "removed"],
            ] {
                let mut old = Box::new(Button::new("target", "Target"));
                let mut tree = TaffyTree::new();
                let mut nodes = HashMap::new();
                let root = old.build_layout(&mut tree, &tilo_ui_base::Theme::dark(), &mut nodes);
                let mut bindings = BoundNodeMap::new();
                // SAFETY: exact exclusively owned tree and maps.
                unsafe {
                    bindings.rebuild(old.as_ref(), &tree, root, &nodes);
                }
                let focus =
                    unsafe { CompositionFocus::capture(Some(root), &nodes, &bindings) }.unwrap();
                nodes.clear();
                bindings.clear();
                let mut rebuilt = Column::new()
                    .push(Box::new(Button::new(target_keys[0], "First")))
                    .push(Box::new(Button::new(target_keys[1], "Second")));
                let root = rebuilt.build_layout(&mut tree, &tilo_ui_base::Theme::dark(), &mut nodes);
                // SAFETY: exact exclusively owned rebuilt tree and maps.
                unsafe {
                    bindings.rebuild(&rebuilt, &tree, root, &nodes);
                }
                let restored = unsafe { focus.restore(&nodes, &bindings, None) };
                if target_keys == ["other", "target"] {
                    assert!(restored.is_some());
                    assert!(!rebuilt.children()[0].is_focused());
                    assert!(rebuilt.children()[1].is_focused());
                } else {
                    assert!(restored.is_none());
                    assert!(rebuilt.children().iter().all(|child| !child.is_focused()));
                }
            }
        }

        fn duplicate_source_key_cannot_transfer_focus_to_the_surviving_other_control() {
            use crate::widget::{Button, Column};
            let mut old = Column::new()
                .push(Box::new(Button::new("duplicate", "Removed")))
                .push(Box::new(Button::new("duplicate", "Survivor")));
            let mut tree = TaffyTree::new();
            let mut nodes = HashMap::new();
            let root = old.build_layout(&mut tree, &tilo_ui_base::Theme::dark(), &mut nodes);
            let selected = tree.children(root).unwrap()[0];
            let mut bindings = BoundNodeMap::new();
            // SAFETY: exact exclusively owned tree and maps.
            unsafe {
                bindings.rebuild(&old, &tree, root, &nodes);
            }
            let focus =
                unsafe { CompositionFocus::capture(Some(selected), &nodes, &bindings) }.unwrap();
            nodes.clear();
            bindings.clear();
            let mut survivor = Button::new("duplicate", "Survivor");
            let root = survivor.build_layout(&mut tree, &tilo_ui_base::Theme::dark(), &mut nodes);
            // SAFETY: exact exclusively owned replacement tree and maps.
            unsafe {
                bindings.rebuild(&survivor, &tree, root, &nodes);
            }
            assert!(unsafe { focus.restore(&nodes, &bindings, None) }.is_none());
            assert!(!survivor.is_focused());
        }

        fn review_disabled_duplicate_target_does_not_transfer_focus_to_enabled_other() {
            use crate::widget::{Button, Column};
            let mut old = Button::new("target", "Original");
            let mut tree = TaffyTree::new();
            let mut nodes = HashMap::new();
            let root = old.build_layout(&mut tree, &tilo_ui_base::Theme::dark(), &mut nodes);
            let mut bindings = BoundNodeMap::new();
            unsafe {
                bindings.rebuild(&old, &tree, root, &nodes);
            }
            let focus =
                unsafe { CompositionFocus::capture(Some(root), &nodes, &bindings) }.unwrap();
            nodes.clear();
            bindings.clear();
            let mut disabled = Button::new("target", "Original");
            disabled.set_enabled(false);
            let mut rebuilt = Column::new()
                .push(Box::new(disabled))
                .push(Box::new(Button::new("target", "Other")));
            let root = rebuilt.build_layout(&mut tree, &tilo_ui_base::Theme::dark(), &mut nodes);
            unsafe {
                bindings.rebuild(&rebuilt, &tree, root, &nodes);
            }
            assert!(unsafe { focus.restore(&nodes, &bindings, None) }.is_none());
            assert!(rebuilt.children().iter().all(|child| !child.is_focused()));
        }

        unnamed_focus_has_no_transferable_snapshot_and_keeps_its_own_state();
        current_focus_refuses_unadmitted_disabled_excluded_and_unrelated_siblings();
        focus_never_uses_an_ordinal_or_an_ambiguous_semantic_key();
        duplicate_source_key_cannot_transfer_focus_to_the_surviving_other_control();
        review_disabled_duplicate_target_does_not_transfer_focus_to_enabled_other();
    }

    phase compound_editor_focus_follows_named_identity_across_rebuilds;
    {
        /// A compound text widget as a document editor is one: a named, focusable owner
        /// whose first line takes its focus, laid out as one unnamed measured leaf under its
        /// own node.
        struct Lines {
            lines: Vec<crate::widget::TextInput>,
            leaf: tilo_ui_base::widget::Spacer,
        }
        impl Lines {
            fn new(count: usize) -> Self {
                Self {
                    lines: (0..count)
                        .map(|_| crate::widget::TextInput::new(Vec::new()))
                        .collect(),
                    leaf: tilo_ui_base::widget::Spacer::new(),
                }
            }
            fn first_line_focused(&self) -> bool {
                self.lines.first().is_some_and(|line| line.is_focused())
            }
        }
        impl Widget for Lines {
            fn build_layout(
                &mut self,
                tree: &mut TaffyTree<()>,
                theme: &Theme,
                nodes: &mut HashMap<NodeId, *mut dyn Widget>,
            ) -> NodeId {
                let leaf = self.leaf.build_layout(tree, theme, nodes);
                tree.widget_node(taffy::Style::default(), &[leaf], nodes, self)
            }
            fn name(&self) -> &str {
                "lines"
            }
            fn test_id(&self) -> Option<&str> {
                Some("lines")
            }
            fn is_focusable(&self) -> bool {
                true
            }
            fn set_focus(&mut self, focused: bool) {
                for (index, line) in self.lines.iter_mut().enumerate() {
                    line.set_focus(focused && index == 0);
                }
            }
            fn is_focused(&self) -> bool {
                self.lines.iter().any(Widget::is_focused)
            }
        }

        fn compound_editor_focus_survives_rebuilt_or_split_focused_line() {
            for capture_leaf in [false, true] {
                let mut editor = Lines::new(1);
                editor.set_focus(true);
                let mut tree = TaffyTree::new();
                let mut nodes = HashMap::new();
                let root = editor.build_layout(&mut tree, &tilo_ui_base::Theme::dark(), &mut nodes);
                let selected = if capture_leaf {
                    tree.children(root).unwrap()[0]
                } else {
                    root
                };
                let mut bindings = BoundNodeMap::new();
                unsafe {
                    bindings.rebuild(&editor, &tree, root, &nodes);
                }
                let focus = unsafe { CompositionFocus::capture(Some(selected), &nodes, &bindings) }
                    .unwrap();
                nodes.clear();
                bindings.clear();
                editor = Lines::new(2);
                editor.set_focus(true);
                let root = editor.build_layout(&mut tree, &tilo_ui_base::Theme::dark(), &mut nodes);
                unsafe {
                    bindings.rebuild(&editor, &tree, root, &nodes);
                }
                assert_eq!(
                    unsafe { focus.restore(&nodes, &bindings, None) },
                    Some(root)
                );
                assert!(editor.is_focused());
                assert!(editor.first_line_focused());
                assert_eq!(
                    unsafe { CompositionFocus::current(&nodes, &bindings, None) },
                    Some(root)
                );
            }
        }

        fn title_focus_uses_named_identity_across_editing_delegate_replacement() {
            use crate::widget::EditableLabel;
            let mut old = EditableLabel::new("Title").with_id("note_title");
            let mut tree = TaffyTree::new();
            let mut nodes = HashMap::new();
            let root = old.build_layout(&mut tree, &tilo_ui_base::Theme::dark(), &mut nodes);
            let mut bindings = BoundNodeMap::new();
            unsafe {
                bindings.rebuild(&old, &tree, root, &nodes);
            }
            let focus =
                unsafe { CompositionFocus::capture(Some(root), &nodes, &bindings) }.unwrap();
            nodes.clear();
            bindings.clear();
            let mut rebuilt = EditableLabel::new("Title").with_id("note_title");
            rebuilt.begin_edit();
            let root = rebuilt.build_layout(&mut tree, &tilo_ui_base::Theme::dark(), &mut nodes);
            unsafe {
                bindings.rebuild(&rebuilt, &tree, root, &nodes);
            }
            assert_eq!(
                unsafe { focus.restore(&nodes, &bindings, None) },
                Some(root)
            );
            assert!(rebuilt.is_editing());
            assert_eq!(rebuilt.take_outcome(), crate::widget::EditOutcome::None);
        }

        fn duplicate_named_editor_or_removed_editor_cannot_receive_focus() {
            use crate::widget::Column;
            for duplicate in [false, true] {
                let mut old = Lines::new(1);
                old.set_focus(true);
                let mut tree = TaffyTree::new();
                let mut nodes = HashMap::new();
                let root = old.build_layout(&mut tree, &tilo_ui_base::Theme::dark(), &mut nodes);
                let mut bindings = BoundNodeMap::new();
                unsafe {
                    bindings.rebuild(&old, &tree, root, &nodes);
                }
                let focus =
                    unsafe { CompositionFocus::capture(Some(root), &nodes, &bindings) }.unwrap();
                nodes.clear();
                bindings.clear();
                let mut rebuilt = Column::new();
                if duplicate {
                    rebuilt = rebuilt
                        .push(Box::new(Lines::new(1)))
                        .push(Box::new(Lines::new(1)));
                }
                let root = rebuilt.build_layout(&mut tree, &tilo_ui_base::Theme::dark(), &mut nodes);
                unsafe {
                    bindings.rebuild(&rebuilt, &tree, root, &nodes);
                }
                assert!(unsafe { focus.restore(&nodes, &bindings, None) }.is_none());
                assert!(rebuilt.children().iter().all(|child| !child.is_focused()));
            }
        }

        fn compound_editor_restores_owner_and_inner_text_focus_after_rebuild() {
            let mut editor = Box::new(Lines::new(1));
            editor.set_focus(true);
            let mut tree = TaffyTree::new();
            let mut nodes = HashMap::new();
            let root = editor.build_layout(&mut tree, &tilo_ui_base::Theme::dark(), &mut nodes);
            let mut bindings = BoundNodeMap::new();
            unsafe {
                bindings.rebuild(editor.as_ref(), &tree, root, &nodes);
            }
            assert!(editor.first_line_focused());
            assert_eq!(
                nodes
                    .values()
                    .filter(|&&pointer| unsafe { (*pointer).is_focused() })
                    .count(),
                1
            );
            let focus =
                unsafe { CompositionFocus::capture(Some(root), &nodes, &bindings) }.unwrap();
            for _ in 0..8 {
                nodes.clear();
                bindings.clear();
                tree = TaffyTree::new();
                let rebuilt = editor.build_layout(&mut tree, &tilo_ui_base::Theme::dark(), &mut nodes);
                unsafe {
                    bindings.rebuild(editor.as_ref(), &tree, rebuilt, &nodes);
                }
                assert_eq!(
                    unsafe { focus.restore(&nodes, &bindings, None) },
                    Some(rebuilt)
                );
                assert!(editor.is_focused());
                assert!(editor.first_line_focused());
            }
            // The stable named owner still cannot bypass a current focus trap.
            assert!(unsafe { focus.restore(&nodes, &bindings, Some(&[])) }.is_none());
            assert!(!editor.is_focused());
            assert!(!editor.first_line_focused());
        }

        compound_editor_focus_survives_rebuilt_or_split_focused_line();
        title_focus_uses_named_identity_across_editing_delegate_replacement();
        duplicate_named_editor_or_removed_editor_cannot_receive_focus();
        compound_editor_restores_owner_and_inner_text_focus_after_rebuild();
    }

    phase canonical_tab_focus_survives_reorder_but_never_transfers_to_a_remaining_peer;
    {
        use crate::widget::{
            Column,
            tabs::{TabBar, TabDescriptor},
        };
        let bar = |id| {
            Box::new(TabBar::new(
                &format!("g{id}"),
                vec![TabDescriptor::new(id, "Tab")],
                id,
            )) as Box<dyn Widget>
        };
        let mut old = Column::new().push(bar(3)).push(bar(4));
        let mut tree = TaffyTree::new();
        let mut nodes = HashMap::new();
        let mut bindings = BoundNodeMap::new();
        let root = old.build_layout(&mut tree, &tilo_ui_base::Theme::dark(), &mut nodes);
        unsafe {
            bindings.rebuild(&old, &tree, root, &nodes);
        }
        let source = *nodes
            .keys()
            .find(|&&node| unsafe {
                tilo_ui_base::widget::bound_nodes::inspect_bound_node(&bindings, &nodes, node, |widget| {
                    widget
                        .accessibility()
                        .and_then(|a| a.semantic_key)
                        .as_deref()
                        == Some("tabbar:g3")
                })
                .unwrap_or(false)
            })
            .unwrap();
        let focus = unsafe { CompositionFocus::capture(Some(source), &nodes, &bindings) }.unwrap();
        for ids in [vec![4, 3], vec![4]] {
            nodes.clear();
            bindings.clear();
            let mut rebuilt = Column::new();
            for id in &ids {
                rebuilt = rebuilt.push(bar(*id));
            }
            let root = rebuilt.build_layout(&mut tree, &tilo_ui_base::Theme::dark(), &mut nodes);
            unsafe {
                bindings.rebuild(&rebuilt, &tree, root, &nodes);
            }
            let restored = unsafe { focus.restore(&nodes, &bindings, None) };
            assert_eq!(restored.is_some(), ids.contains(&3));
            if let Some(node) = restored {
                assert!(unsafe {
                    tilo_ui_base::widget::bound_nodes::inspect_bound_node(&bindings, &nodes, node, |widget| {
                        widget
                            .accessibility()
                            .and_then(|a| a.semantic_key)
                            .as_deref()
                            == Some("tabbar:g3")
                    })
                    .unwrap()
                });
            }
        }
    }
}
