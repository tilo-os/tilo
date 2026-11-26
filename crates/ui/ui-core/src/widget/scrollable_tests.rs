//! The scroll view's own tests: its offsets, its bars, its keys and the
//! frames it asks for.

use super::*;
use crate::presence::PresenceCtx;
use crate::testing::TestLeaf;
use crate::testing::{Dispatch, dispatch};
use crate::theme::Theme;
use crate::widget::TaffyTreeExt;
use crate::widget::{Element, Widget};
use std::cell::Cell;
use std::rc::Rc;
use taffy::{Dimension, NodeId, Size, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use winit::event::{ElementState, MouseButton, MouseScrollDelta, TouchPhase, WindowEvent};
use winit::keyboard::NamedKey;
#[allow(
    clippy::cast_possible_truncation,
    reason = "the chrome fade is a few hundred milliseconds"
)]
fn vertical(content_h: f32) -> Scrollable {
    let mut s = Scrollable::new_vertical();
    s.rect = Rect::new(0.0, 0.0, 200.0, 300.0);
    s.set_viewport_size(200.0, 300.0);
    s.set_content_size(200.0, content_h);
    s
}

use crate::synthetic_input::cursor_moved;
fn mouse(state: ElementState) -> WindowEvent {
    crate::synthetic_input::mouse_input(state, MouseButton::Left)
}

tilo_test_env::scenario! {
    /// The give past an end of the content.
    /// Each phase was a test of its own and builds its own fixture.
    #[allow(clippy::float_cmp, reason = "a give at rest is exactly zero")]
    fn fingers_past_an_end_give_and_spring_back;

    phase fingers_past_the_top_pull_the_content_through_the_band_and_it_springs_back;
    {
        use crate::time::testing::with_monotonic_time;
        let start = crate::time::now();
        let mut s = vertical(1000.0);
        with_monotonic_time(start, || {
            assert!(s.pan(0.0, -60.0), "the give is a move");
            let (_, y) = s.drawn_offset();
            assert!(
                y < 0.0 && y > -60.0,
                "the content gives, less than the hand: {y}"
            );
            assert_eq!(s.offset_y, 0.0, "the scroll itself stays at the end");
            assert!(s.pan(0.0, 30.0));
            let (_, back) = s.drawn_offset();
            assert!(back < 0.0 && back > y, "{y} then {back}");
            assert_eq!(s.offset_y, 0.0);
            let lift = MouseScrollDelta::PixelDelta(winit::dpi::PhysicalPosition::new(0.0, 0.0));
            assert!(s.on_wheel(&lift, TouchPhase::Ended, false, 1.0));
            assert!(s.needs_animation_frame() || s.band.is_moving(start));
        });
        with_monotonic_time(start + Duration::from_secs(1), || {
            assert_eq!(s.drawn_offset(), (0.0, 0.0), "sprung back to the end");
        });
    }

    phase under_reduced_motion_fingers_stop_at_the_end;
    {
        use crate::time::testing::with_monotonic_time;
        let mut s = vertical(1000.0);
        s.motion = crate::tokens::EffectiveMotion::new(crate::tokens::Motion::default(), true);
        with_monotonic_time(crate::time::now(), || {
            assert!(!s.pan(0.0, -60.0), "nothing moved: the wheel may bubble");
            assert_eq!(s.drawn_offset(), (0.0, 0.0));
        });
    }

    phase a_browsers_swipe_past_the_top_springs_back_once_it_pauses;
    {
        use crate::time::testing::with_monotonic_time;
        let mut s = vertical(1000.0);
        let mut wheel = crate::widget::PhaselessWheel::default();
        let start = crate::time::now();
        let feed = |s: &mut Scrollable, event: &WindowEvent| {
            if let WindowEvent::MouseWheel { delta, phase, .. } = event {
                s.on_wheel(delta, *phase, false, 1.0);
            }
        };
        with_monotonic_time(start, || {
            let mut up = WindowEvent::MouseWheel {
                device_id: winit::event::DeviceId::dummy(),
                delta: MouseScrollDelta::PixelDelta(winit::dpi::PhysicalPosition::new(0.0, 60.0)),
                phase: TouchPhase::Moved,
            };
            wheel.observe(&mut up, start);
            feed(&mut s, &up);
            assert!(s.drawn_offset().1 < 0.0, "pulled past the top");
        });
        let due = wheel.deadline().expect("the swipe runs");
        with_monotonic_time(due, || {
            let end = wheel.end_if_due(due).expect("its pause ends it");
            feed(&mut s, &end);
        });
        with_monotonic_time(due + Duration::from_secs(1), || {
            assert_eq!(s.drawn_offset(), (0.0, 0.0), "sprung back to the top");
        });
    }
}

tilo_test_env::scenario! {
    /// The offset restored, persisted and following a transcript, and the damage a scroll asks for.
    /// Each phase was a test of its own and builds its own fixture.
    fn offsets_persist_follow_and_damage_only_near_rows;

    phase a_restored_offset_is_clamped_before_the_content_is_placed;
    {
        let cell = Rc::new(Cell::new((0.0_f32, 50_000.0_f32)));
        let mut scroll = Scrollable::new_vertical()
            .with_child(tall_child())
            .with_height(300.0)
            .with_persisted_offset(Rc::clone(&cell));
        host_solve(&mut scroll, 400.0, 300.0);
        let content = scroll.children()[0].get_rect();
        assert!(
            content.y1 >= scroll.rect.y1 - 0.5,
            "the end of the content fills the viewport, no blank frame: {content:?}"
        );
        assert!(cell.get().1 <= scroll.max_offset_y() + 0.01);
    }

    phase a_transcript_opens_at_its_end_and_follows_it_until_scrolled_away;
    {
        let mut scroll = Scrollable::new_vertical()
            .following_end()
            .with_child(tall_child())
            .with_height(300.0);
        host_solve(&mut scroll, 400.0, 300.0);
        assert!(scroll.max_offset_y() > 0.0);
        assert!(
            (scroll.offset_y() - scroll.max_offset_y()).abs() < 0.5,
            "it opens on the newest line"
        );
        let mut longer = Column::new().with_intrinsic_height();
        for i in 0..41 {
            longer = longer.push(Box::new(Text::new(&format!("row {i}"))));
        }
        if let Some(child) = scroll.child_mut() {
            *child = Box::new(longer);
        }
        host_solve(&mut scroll, 400.0, 300.0);
        assert!((scroll.offset_y() - scroll.max_offset_y()).abs() < 0.5);
        scroll.set_offset_y(0.0);
        host_solve(&mut scroll, 400.0, 300.0);
        assert!(scroll.offset_y() < 0.5, "reading older lines stays put");
    }

}

tilo_test_env::scenario! {
    /// How a scrollable sizes and places its content.
    /// Each phase was a test of its own and builds its own fixture.
    fn content_is_sized_and_placed_under_the_viewport;

    phase content_insets_keep_the_child_clear_of_footer_gutters_and_the_bar;
    {
        {
            let (spy, seen) = TestLeaf::sized(200.0, 500.0).focusable().boxed();
            let mut scroll = Scrollable::new_vertical()
                .with_child(spy)
                .with_height(100.0)
                .with_bottom_content_inset(20.0);
            let theme = Theme::dark();
            let mut taffy = TaffyTree::<()>::new();
            let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
            let mut tr = TextRenderer::new();
            let node = scroll.build_layout(&mut taffy, &theme, &mut node_map);
            let parent = taffy.parent_node(
                Style {
                    size: Size::from_lengths(200.0, 100.0),
                    ..Default::default()
                },
                &[node],
            );
            taffy
                .compute_layout_with_measure(
                    parent,
                    Size {
                        width: taffy::AvailableSpace::Definite(200.0),
                        height: taffy::AvailableSpace::Definite(100.0),
                    },
                    |known, available, node_id, _, _| {
                        if let Some(&ptr) = node_map.get(&node_id) {
                            unsafe { (*ptr).measure(known, available, &mut tr) }
                        } else {
                            Size::ZERO
                        }
                    },
                )
                .unwrap();
            scroll.apply_layout(&taffy, node, Point::ZERO);

            let mut sender = Dispatch::new().layout(&taffy, node);
            let mut focus_target = None;
            for event in [cursor_moved(40.0, 90.0), mouse(ElementState::Pressed)] {
                focus_target = sender
                    .send(&mut scroll, &event)
                    .focus_target
                    .or(focus_target);
            }
            assert_eq!(
                seen.pointer(),
                0,
                "the clipped child sees no pointer event"
            );
            assert_eq!(
                seen.presses(),
                0,
                "an invisible child action cannot activate"
            );
            assert_eq!(
                focus_target, None,
                "the footer gutter cannot focus the scroll view"
            );

            for event in [cursor_moved(40.0, 70.0), mouse(ElementState::Pressed)] {
                focus_target = sender
                    .send(&mut scroll, &event)
                    .focus_target
                    .or(focus_target);
            }
            assert_eq!(
                seen.pointer(),
                2,
                "visible content still receives pointer input"
            );
            assert_eq!(seen.presses(), 1, "visible child action still activates");
            assert!(focus_target.is_some(), "visible content remains focusable");
        }

        {
            const INSET: f32 = 12.0;
            let mut scroll = Scrollable::new_vertical().with_bottom_content_inset(INSET);
            scroll.set_viewport_rect(Rect::new(20.0, 30.0, 420.0, 330.0));
            scroll.set_content_size(400.0, 500.0);

            let clip = scroll.content_viewport_rect();
            assert_eq!(clip, Rect::new(20.0, 30.0, 420.0, 318.0));
            assert_eq!(scroll.effective_viewport_h(), 288.0);
            assert_eq!(
                scroll.vertical_track().expect("content overflows").y1,
                clip.y1 - BAR_MARGIN,
                "the scrollbar and content clip both stop above the footer gutter",
            );

            scroll.set_offset_y(f32::MAX);
            assert_eq!(scroll.offset_y(), 212.0);
            assert_eq!(
                scroll.offset_y() + scroll.effective_viewport_h(),
                scroll.content_h,
                "reserving the paint-free gutter must not make the final content unreachable",
            );
        }

        {
            let mut scroll = Scrollable::new_vertical().with_bottom_content_inset(12.0);
            scroll.set_viewport_rect(Rect::new(20.0, 30.0, 420.0, 330.0));
            scroll.set_content_size(400.0, 300.0);

            assert_eq!(scroll.content_viewport_rect(), scroll.rect);
            assert_eq!(scroll.effective_viewport_h(), 300.0);
            assert!(!scroll.vertical_overflows());
            assert!(scroll.vertical_track().is_none());
            scroll.set_offset_y(f32::MAX);
            assert_eq!(scroll.offset_y(), 0.0, "fitting content has no max scroll");
        }

        {
            use std::collections::HashMap;
            let theme = Theme::dark();
            let mut taffy = TaffyTree::<()>::new();
            let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
            let mut tr = TextRenderer::new();
            let mut scroll = Scrollable::new_vertical()
                .with_child(tall_child())
                .with_height(300.0);
            let node = scroll.build_layout(&mut taffy, &theme, &mut node_map);
            let parent = taffy.parent_node(
                Style {
                    size: Size::from_lengths(400.0, 300.0),
                    ..Default::default()
                },
                &[node],
            );
            // The solve reserves the gutter once the content overflows, and lays the content
            // out beside it, in the same solve.
            crate::widget::layout::solve_built(
                &mut scroll,
                &mut taffy,
                (parent, node),
                &node_map,
                Size {
                    width: taffy::AvailableSpace::Definite(400.0),
                    height: taffy::AvailableSpace::Definite(300.0),
                },
                &mut tr,
            )
            .unwrap();
            scroll.apply_layout(&taffy, node, Point::ZERO);

            let child = &scroll.children()[0];
            let child_right = child.get_rect().x1;
            let track = scroll.vertical_track().expect("tall content shows the bar");
            assert!(
                child_right <= track.x0 + 0.5,
                "content right edge {child_right} must not reach the bar track {} (gutter reserved)",
                track.x0,
            );
            assert!(
                (scroll.rect.x1 - child_right - f64::from(theme.tokens.metrics.scrollbar_gutter)).abs()
                    < 1.0,
                "child is inset by exactly the gutter: viewport right {}, child right {child_right}",
                scroll.rect.x1,
            );
        }
    }

    phase the_bar_gutter_is_reserved_only_while_the_content_overflows;
    {
        let short = || -> Element {
            Box::new(
                Column::new()
                    .with_intrinsic_height()
                    .push(Box::new(Text::new("one line"))),
            )
        };
        let mut fits = Scrollable::new_vertical()
            .with_child(short())
            .with_height(300.0);
        host_solve(&mut fits, 400.0, 300.0);
        let child = fits.children()[0].get_rect();
        assert!(
            (child.x1 - fits.rect.x1).abs() < 0.5,
            "fitting content keeps the full width: {child:?}"
        );

        let mut overflows = Scrollable::new_vertical()
            .with_child(tall_child())
            .with_height(300.0);
        host_solve(&mut overflows, 400.0, 300.0);
        let child = overflows.children()[0].get_rect();
        let gutter = f64::from(Theme::dark().tokens.metrics.scrollbar_gutter);
        assert!(
            (overflows.rect.x1 - child.x1 - gutter).abs() < 0.5,
            "{child:?}"
        );
        let track = overflows.vertical_track().expect("bar");
        let thumb = f64::from(Theme::dark().tokens.metrics.scroll_thumb);
        assert!((track.width() - thumb).abs() < 1e-6);
        assert!(
            ((overflows.rect.x1 - track.x1) - (track.x0 - child.x1)).abs() < 0.5,
            "the thumb is centred in its gutter"
        );
    }

    phase horizontal_content_is_laid_out_at_its_max_content_width;
    {
        let wide = Row::new()
            .no_wrap()
            .with_gap(0.0)
            .push(Box::new(
                crate::widget::Container::new().with_size(500.0, 20.0),
            ))
            .push(Box::new(
                crate::widget::Container::new().with_size(500.0, 20.0),
            ));
        let mut scroll = Scrollable::new_horizontal()
            .with_child(Box::new(wide))
            .with_height(80.0);
        host_solve(&mut scroll, 300.0, 80.0);
        assert!(scroll.children()[0].get_rect().width() >= 999.0);
        assert!(scroll.hbar_active(), "wide content scrolls sideways");
        let gutter = f64::from(Theme::dark().tokens.metrics.scrollbar_gutter);
        assert!(
            (scroll.rect.height() - f64::from(scroll.viewport_h)).abs() < 0.5
                && scroll.children()[0].get_rect().height() <= 80.0 - gutter + 0.5,
            "the bar's gutter at the bottom is reserved"
        );
    }

    phase fill_parent_height_fills_a_row_or_a_root_container;
    {
        let mut root = crate::widget::Container::new()
            .with_no_padding()
            .with_max_width(500.0)
            .filling()
            .with_child(Box::new(
                Scrollable::new_vertical()
                    .fill_parent_height()
                    .with_child(tall_child()),
            ));
        host_solve(&mut root, 800.0, 600.0);
        let scroll = scroll_of(&mut root);
        assert!(
            (scroll.rect.height() - 600.0).abs() < 0.5,
            "the scroller takes the root's height: {:?}",
            scroll.rect
        );
        assert!(scroll.vertical_overflows());
    }

    phase height_constraints_decide_scrollability_under_indefinite_parents;
    {
        {
            let h = viewport_height_under_indefinite_parent(
                Scrollable::new_vertical()
                    .with_child(tall_child())
                    .with_height(300.0),
            );
            assert!(
                (h - 300.0).abs() < 0.5,
                "with_height must pin the viewport to 300 even under an \
         indefinite-height parent; got {h}"
            );
        }

        {
            let (vh, scrolled) = can_scroll_under_indefinite_parent(
                Scrollable::new_vertical()
                    .with_child(tall_child())
                    .with_height(300.0),
            );
            assert!((vh - 300.0).abs() < 0.5, "viewport pinned to 300, got {vh}");
            assert!(
                scrolled,
                "a pinned-viewport pane with tall content must scroll"
            );
        }

        {
            let short = viewport_height_under_indefinite_parent(
                Scrollable::new_vertical()
                    .with_child(Box::new(Text::new("one compact line")))
                    .with_max_height(300.0),
            );
            assert!(
                short < 100.0,
                "a short document must not reserve the full cap: {short}"
            );

            let (tall, scrolled) = can_scroll_under_indefinite_parent(
                Scrollable::new_vertical()
                    .with_child(tall_child())
                    .with_max_height(300.0),
            );
            assert!(
                (tall - 300.0).abs() < 0.5,
                "a long document must stop at the cap: {tall}"
            );
            assert!(scrolled, "a capped long document must remain scrollable");
        }

        {
            let (_vh, scrolled) =
                can_scroll_under_indefinite_parent(Scrollable::new_vertical().with_child(tall_child()));
            assert!(
                !scrolled,
                "legacy default is expected to be unscrollable under an \
         indefinite parent; the fix is opt-in via with_height / \
         fill_parent_height"
            );
        }
    }
}

tilo_test_env::scenario! {
    /// Keys, the wheel, focus reveal, and the pointer motion the content receives.
    /// Each phase was a test of its own and builds its own fixture.
    fn input_scrolls_reveals_focus_and_reaches_the_content;

    phase focus_reveal_scrolls_to_new_focus_with_context_but_respects_manual_scroll;
    {
        {
            let mut spy = TestLeaf::sized(200.0, 500.0);
            spy.rect = Rect::new(12.0, 520.0, 188.0, 568.0);
            let mut scroll = vertical(900.0).with_child(Box::new(spy));
            scroll.rect = Rect::new(0.0, 0.0, 200.0, 300.0);
            let target = {
                let child = scroll.child.as_deref_mut().expect("child");
                std::ptr::from_ref::<dyn Widget>(child)
            };

            assert!(scroll.reveal_descendant(target, Rect::new(12.0, 520.0, 188.0, 568.0)));
            assert!(
                scroll.offset_y() <= 484.0,
                "focus reveal should retain at least 36px of label/context above the field; offset={} ",
                scroll.offset_y()
            );
            assert!(
                scroll.offset_y() + 300.0 >= 604.0,
                "the focused field plus context below must remain visible"
            );
        }

        {
            use crate::{LocalState, widget::TextInput};
            let mut input = TextInput::from_state(&LocalState::new(String::new())).with_id("last");
            input.set_focus(true);
            let mut column = Column::new().with_intrinsic_height();
            for _ in 0..15 {
                column = column.push(Box::new(Text::new("Spacer row")));
            }
            column = column.push(Box::new(input));
            let mut scroll = Scrollable::new_vertical()
                .with_height(150.0)
                .with_focus_reveal()
                .with_child(Box::new(column));
            let theme = Theme::dark();
            let mut tree = TaffyTree::<()>::new();
            let mut nodes = HashMap::new();
            let mut text = TextRenderer::new();
            let node = scroll.build_layout(&mut tree, &theme, &mut nodes);
            tree.compute_layout_with_measure(
                node,
                Size {
                    width: taffy::AvailableSpace::Definite(400.0),
                    height: taffy::AvailableSpace::Definite(150.0),
                },
                |known, available, node, _, _| {
                    nodes
                        .get(&node)
                        .map(|p| unsafe { (**p).measure(known, available, &mut text) })
                        .unwrap_or(Size::ZERO)
                },
            )
            .unwrap();
            scroll.apply_layout(&tree, node, Point::ZERO);
            assert!(
                scroll.offset_y() > 0.0,
                "keyboard focus below viewport must be revealed"
            );
            let rect = scroll
                .child
                .as_ref()
                .unwrap()
                .children()
                .last()
                .unwrap()
                .get_rect();
            assert!(
                rect.y0 >= scroll.rect.y0 && rect.y1 <= scroll.rect.y1 + 0.5,
                "focused input must be visible: {rect:?}"
            );
            scroll.set_offset_y(0.0);
            scroll.apply_layout(&tree, node, Point::ZERO);
            assert_eq!(
                scroll.offset_y(),
                0.0,
                "manual scrolling must not snap back to unchanged focus"
            );
        }
    }

    phase a_section_receives_same_position_motion_after_pointer_reentry;
    {
        use crate::widget::{Section, SectionHeader, Text};

        let opened = Rc::new(Cell::new(None));
        let seen = Rc::clone(&opened);
        let section = Section::new(vec![Box::new(Text::new("Details"))])
            .with_header(SectionHeader::group("Advanced options"))
            .collapsed()
            .on_expand(move |open| seen.set(Some(open)));
        let mut scroll = Scrollable::new_vertical()
            .with_height(150.0)
            .with_child(Box::new(section));
        let mut tree = TaffyTree::new();
        let mut nodes = HashMap::new();
        let mut text = TextRenderer::new();
        let node = scroll.build_layout(&mut tree, &Theme::dark(), &mut nodes);
        tree.compute_layout_with_measure(
            node,
            Size {
                width: taffy::AvailableSpace::Definite(400.0),
                height: taffy::AvailableSpace::Definite(150.0),
            },
            |known, available, node, _, _| {
                nodes
                    .get(&node)
                    .map(|p| {
                        // SAFETY: scroll exclusively owns the unchanged widget
                        // tree; its layout map remains valid throughout measurement.
                        unsafe { (**p).measure(known, available, &mut text) }
                    })
                    .unwrap_or(Size::ZERO)
            },
        )
        .unwrap();
        scroll.apply_layout(&tree, node, Point::ZERO);
        let at = scroll.child.as_ref().unwrap().children()[0]
            .get_rect()
            .center();
        assert!(scroll.content_viewport_rect().contains(at));
        let mut sender = Dispatch::new().layout(&tree, node);
        for event in [
            cursor_moved(at.x, at.y),
            WindowEvent::CursorLeft {
                device_id: winit::event::DeviceId::dummy(),
            },
            cursor_moved(at.x, at.y),
            mouse(ElementState::Pressed),
            mouse(ElementState::Released),
        ] {
            sender.send(&mut scroll, &event);
        }
        assert_eq!(opened.get(), Some(true), "the real disclosure must open");
        assert!(scroll.child.as_ref().unwrap().children().len() > 1);
    }

    phase a_scrolled_disclosure_gets_motion_after_leaving_a_text_field;
    {
        use crate::LocalState;
        use crate::widget::{Column, Section, SectionHeader, Spacer, Text, TextInput};

        for scale in [1.0, 2.0] {
            let opened = Rc::new(Cell::new(None));
            let seen = Rc::clone(&opened);
            let mut scroll = Scrollable::new_vertical()
                .with_height(150.0)
                .with_child(Box::new(
                    Column::new()
                        .with_intrinsic_height()
                        .push(Box::new(TextInput::from_state(&LocalState::new(
                            String::new(),
                        ))))
                        .push(Box::new(Spacer::fixed(300.0)))
                        .push(Box::new(
                            Section::new(vec![Box::new(Text::new("Details"))])
                                .with_header(SectionHeader::group("Advanced options"))
                                .collapsed()
                                .on_expand(move |open| seen.set(Some(open))),
                        )),
                ));
            let theme = Theme::dark().with_text_scale(scale);
            let mut tree = TaffyTree::new();
            let mut nodes = HashMap::new();
            let mut text = TextRenderer::new();
            let node = scroll.build_layout(&mut tree, &theme, &mut nodes);
            tree.compute_layout_with_measure(
                node,
                Size {
                    width: taffy::AvailableSpace::Definite(400.0),
                    height: taffy::AvailableSpace::Definite(150.0),
                },
                |known, available, node, _, _| {
                    nodes
                        .get(&node)
                        .map(|p| {
                            // SAFETY: scroll exclusively owns the unchanged widget tree;
                            // the node map remains valid for this measurement pass.
                            unsafe { (**p).measure(known, available, &mut text) }
                        })
                        .unwrap_or(Size::ZERO)
                },
            )
            .unwrap();
            scroll.apply_layout(&tree, node, Point::ZERO);
            let field = scroll.child.as_ref().unwrap().children()[0]
                .get_rect()
                .center();
            let mut measured = Dispatch::new().layout(&tree, node);
            measured.send(&mut scroll, &cursor_moved(field.x, field.y));
            scroll.set_offset_y(scroll.max_offset_y());
            scroll.apply_layout(&tree, node, Point::ZERO);
            assert!(
                scroll.offset_y() > 0.0,
                "the disclosure was scrolled into view"
            );
            let at = scroll.child.as_ref().unwrap().children()[2].children()[0]
                .get_rect()
                .center();
            assert!(scroll.content_viewport_rect().contains(at));
            assert!(
                !measured
                    .send(&mut scroll, &cursor_moved(at.x, at.y))
                    .handled,
                "hover transitions do not consume motion intended for a sibling"
            );
            assert!(
                measured
                    .send(&mut scroll, &mouse(ElementState::Pressed))
                    .handled
            );
            assert!(
                measured
                    .send(&mut scroll, &mouse(ElementState::Released))
                    .handled
            );
            assert_eq!(
                opened.get(),
                Some(true),
                "the real scrolled disclosure opens at scale {scale}"
            );
        }
    }

    phase keyboard_scrolling_and_focusability_require_scrollable_focus;
    {
        {
            use crate::a11y::Role;
            let s = vertical(1200.0);
            let node = Widget::accessibility(&s).expect("scroll container is a region");
            assert_eq!(node.role, Role::Group);
            assert_eq!(
                node.name, None,
                "a scroll container carries no name of its own"
            );
            assert_eq!(node.value, None);
        }

        {
            assert!(
                Widget::is_focusable(&vertical(1200.0)),
                "overflowing is focusable"
            );
            assert!(
                !Widget::is_focusable(&vertical(150.0)),
                "fitting is not focusable"
            );
            let mut hidden = vertical(1200.0);
            hidden.show_scrollbar = false;
            assert!(!Widget::is_focusable(&hidden));
        }

        {
            let mut s = vertical(1200.0); // viewport 300, max_scroll 900
            s.set_focus(true);
            assert!(s.is_focused());

            assert!(
                dispatch(&mut s, &named_key(NamedKey::PageDown)),
                "PageDown handled"
            );
            let page = 300.0 * PAGE_FRACTION;
            assert!(
                (s.offset_y() - page).abs() < 0.5,
                "PageDown ≈ 0.9 viewport, got {}",
                s.offset_y()
            );

            assert!(dispatch(&mut s, &named_key(NamedKey::End)), "End handled");
            assert!(
                (s.offset_y() - 900.0).abs() < 1e-3,
                "End → bottom, got {}",
                s.offset_y()
            );

            assert!(
                dispatch(&mut s, &named_key(NamedKey::PageUp)),
                "PageUp handled"
            );
            assert!(
                (s.offset_y() - (900.0 - page)).abs() < 0.5,
                "PageUp from bottom, got {}",
                s.offset_y()
            );

            assert!(dispatch(&mut s, &named_key(NamedKey::Home)), "Home handled");
            assert_eq!(s.offset_y(), 0.0, "Home → top");

            assert!(
                dispatch(&mut s, &named_key(NamedKey::ArrowDown)),
                "ArrowDown handled"
            );
            assert!(
                (s.offset_y() - LINE_STEP).abs() < 1e-3,
                "ArrowDown = one line, got {}",
                s.offset_y()
            );
        }

        {
            let mut s = vertical(1200.0);
            assert!(!s.is_focused());
            assert!(
                !dispatch(&mut s, &named_key(NamedKey::PageDown)),
                "unfocused key not handled"
            );
            assert_eq!(s.offset_y(), 0.0, "unfocused container does not scroll");
        }
    }

    phase wheel_scrolls_vertically_and_horizontally;
    {
        {
            let mut s = vertical(1200.0);
            let before = s.offset_y();
            let ev = crate::synthetic_input::mouse_wheel(0.0, -1.0);
            assert!(dispatch(&mut s, &ev), "wheel handled");
            assert!(s.offset_y() > before, "wheel down increases offset");
        }

        {
            use winit::keyboard::ModifiersState;
            let mut s = both(1200.0, 1200.0);
            let shift = winit::event::Modifiers::from(ModifiersState::SHIFT);
            let wheel = WindowEvent::MouseWheel {
                device_id: winit::event::DeviceId::dummy(),
                delta: MouseScrollDelta::LineDelta(0.0, -1.0), // a plain vertical wheel
                phase: winit::event::TouchPhase::Moved,
            };
            assert!(
                Dispatch::new()
                    .modifiers(shift)
                    .send(&mut s, &wheel)
                    .handled,
                "shift+wheel handled"
            );
            assert!(
                s.offset_x() > 0.0,
                "shift+wheel scrolls X, got x={}",
                s.offset_x()
            );
            assert_eq!(s.offset_y(), 0.0, "shift+wheel leaves Y untouched");
        }

        {
            let mut s = both(1200.0, 1200.0);
            let tilt = WindowEvent::MouseWheel {
                device_id: winit::event::DeviceId::dummy(),
                delta: MouseScrollDelta::LineDelta(-1.0, 0.0), // horizontal tilt
                phase: winit::event::TouchPhase::Moved,
            };
            assert!(dispatch(&mut s, &tilt), "tilt wheel handled");
            assert!(
                s.offset_x() > 0.0,
                "tilt wheel scrolls X, got x={}",
                s.offset_x()
            );
        }
    }

    phase a_page_key_the_focused_field_leaves_alone_pages_the_view_holding_it;
    {
        let mut scroll =
            Scrollable::new_vertical().with_child(Box::new(FocusedField { rect: Rect::ZERO }));
        let mut taffy = TaffyTree::new();
        let mut nodes = HashMap::new();
        let node = scroll.build_layout(&mut taffy, &Theme::dark(), &mut nodes);
        taffy
            .compute_layout(
                node,
                Size {
                    width: taffy::AvailableSpace::Definite(200.0),
                    height: taffy::AvailableSpace::Definite(300.0),
                },
            )
            .unwrap();
        scroll.apply_layout(&taffy, node, Point::ZERO);
        assert!(
            !scroll.is_focused(),
            "the field has the focus, not the view"
        );
        assert!(
            dispatch(&mut scroll, &named_key(NamedKey::PageDown)),
            "PageDown taken"
        );
        let page = scroll.offset_y();
        assert!(page > 200.0, "a page down: {page}");
        assert!(
            dispatch(&mut scroll, &named_key(NamedKey::PageUp)),
            "PageUp taken"
        );
        assert_eq!(scroll.offset_y(), 0.0, "back at the top");
        assert!(
            !dispatch(&mut scroll, &named_key(NamedKey::PageUp)),
            "at the top, PageUp is left to the view around it"
        );
    }
}

#[test]
fn field_validation_focus_reveal_fits_context_or_prioritises_protected_input() {
    use crate::app::driver::Driver;
    use crate::widget::{Field, TextInput, TextInputKind};
    use crate::{LocalState, State};

    fn named<'a>(root: &'a dyn Widget, id: &str) -> Option<&'a dyn Widget> {
        if root.test_id() == Some(id) {
            return Some(root);
        }
        root.children()
            .into_iter()
            .find_map(|child| named(child, id))
    }
    fn correction<'a>(root: &'a dyn Widget, message: &str) -> Option<&'a dyn Widget> {
        if root.accessibility().is_some_and(|node| {
            node.name.as_deref() == Some(message)
                && node.live == Some(crate::a11y::Politeness::Assertive)
        }) {
            return Some(root);
        }
        root.children()
            .into_iter()
            .find_map(|child| correction(child, message))
    }
    for scale in [1.0, 2.0] {
        for oversized in [false, true] {
            let message = if oversized {
                "Use a different password. ".repeat(80)
            } else {
                "Use at least 15 characters.".to_string()
            };
            let value = LocalState::new("private sample password".to_string());
            let mut input = TextInput::from_state(&value)
                .with_id("validation-password")
                .with_kind(TextInputKind::Password)
                .with_secret(true);
            input.set_focus(true);
            let field = Field::new("Password", input)
                .with_error_live(&message, crate::a11y::Politeness::Assertive);
            let mut content = Column::new().with_intrinsic_height();
            for _ in 0..20 {
                content = content.push(Box::new(Text::new("Earlier form content")));
            }
            content = content.push(Box::new(field));
            let scroll = Scrollable::new_vertical()
                .fill_parent_height()
                .with_focus_reveal()
                .with_child(Box::new(content));
            let mut harness = Driver::new_with_theme(
                State::new(),
                Box::new(scroll),
                (400, 300),
                Theme::dark().with_ui_scale(scale).with_reduce_motion(true),
            );
            harness.with_widget_mut::<Scrollable, _>(|scroll| {
                let viewport = scroll.content_viewport_rect();
                let input = named(scroll, "validation-password").unwrap();
                let access = input.accessibility().unwrap();
                assert!(input.is_focused());
                assert!(access.state.invalid && access.state.protected);
                assert!(access.value.is_none(), "focus must not expose the password");
                let input = input.get_rect();
                let caption = correction(scroll, &message).unwrap().get_rect();
                let context_height = caption.y1 - input.y0;
                assert!(scroll.offset_y() > 0.0, "focus must reveal a field after earlier content");
                assert!(input.y0 >= viewport.y0 && input.y1 <= viewport.y1,
                    "actual protected input remains fully visible: {input:?}, {viewport:?}");
                if oversized {
                    assert!(context_height > viewport.height(),
                        "fixture must exercise the too-tall context fallback: {context_height}, {viewport:?}");
                    assert!(caption.y1 > viewport.y1,
                        "oversized caption cannot displace the correction input");
                } else {
                    assert!(context_height <= viewport.height(),
                        "fixture must exercise fitting shared Field context");
                    assert!(caption.y0 >= viewport.y0 && caption.y1 <= viewport.y1,
                        "entire actual correction caption is revealed with input: {caption:?}, {viewport:?}");
                }
                scroll.set_offset_y(0.0);
            }).unwrap();
            // The driver ran the production layout after that deliberate
            // scroll. Unchanged focus must not pull the person back down.
            harness
                .with_widget_mut::<Scrollable, _>(|scroll| {
                    assert_eq!(scroll.offset_y(), 0.0);
                    let input = named(scroll, "validation-password").unwrap();
                    assert!(input.is_focused());
                    assert!(
                        input.get_rect().y0 > scroll.content_viewport_rect().y1,
                        "manual scrolling really moved the focused field out of view"
                    );
                })
                .unwrap();
            harness
                .with_widget_mut::<Scrollable, _>(|scroll| {
                    assert_eq!(
                        scroll.offset_y(),
                        0.0,
                        "later layout must continue respecting manual scroll with unchanged focus"
                    );
                })
                .unwrap();
        }
    }
}

tilo_test_env::scenario! {
    /// The bars: thumb geometry and presses, corners, fades and the primary region.
    /// Each phase was a test of its own and builds its own fixture.
    fn bars_thumbs_and_edge_fades;

    phase thumb_drags_and_track_presses_scroll_by_offset_and_page;
    {
        {
            let mut s = vertical(1200.0); // max_scroll = 900
            let thumb = s.vertical_thumb().unwrap();
            let bar_x = s.rect.x1 - BAR_MARGIN - BAR_THICKNESS / 2.0;
            let mid_y = f64::midpoint(thumb.y0, thumb.y1);

            dispatch(&mut s, &cursor_moved(bar_x, mid_y));
            assert!(
                dispatch(&mut s, &mouse(ElementState::Pressed)),
                "press on thumb handled"
            );
            assert!(s.vbar.is_dragging(), "press on thumb begins a drag");
            assert!(s.wants_pointer_capture(), "drag requests pointer capture");
            assert_eq!(s.offset_y(), 0.0);

            dispatch(&mut s, &cursor_moved(bar_x, mid_y + 100.0));
            assert!(s.offset_y() > 0.0, "drag down increases offset");
            assert!(s.offset_y() <= 900.0, "offset stays clamped to max");
            let after_down = s.offset_y();

            dispatch(&mut s, &cursor_moved(bar_x, mid_y - 100.0));
            assert!(s.offset_y() < after_down, "drag up decreases offset");

            dispatch(&mut s, &mouse(ElementState::Released));
            assert!(!s.vbar.is_dragging(), "release ends the drag");
        }

        {
            let mut s = vertical(1200.0);
            let thumb = s.vertical_thumb().unwrap();
            let bar_x = s.rect.x1 - BAR_MARGIN - BAR_THICKNESS / 2.0;
            let below_y = thumb.y1 + 5.0;
            dispatch(&mut s, &cursor_moved(bar_x, below_y));
            assert!(
                dispatch(&mut s, &mouse(ElementState::Pressed)),
                "track click handled"
            );
            assert!(!s.vbar.is_dragging(), "track click does not start a drag");
            let paged = s.offset_y();
            assert!(paged > 0.0, "click below thumb pages down: {paged}");
            let page_frac = PAGE_FRACTION;
            assert!(
                (paged - 300.0 * page_frac).abs() < 1.0,
                "page step should be ≈viewport*{page_frac}, got {paged}",
            );
        }

        {
            let mut s = both(1200.0, 1200.0);
            let track = s.horizontal_track().expect("h-bar shown");
            let thumb = s.thumb_rect(ScrollAxis::Horizontal).expect("h-thumb");
            let mid_x = f64::midpoint(thumb.x0, thumb.x1);
            let bar_y = track.y0 + track.height() / 2.0;
            dispatch(&mut s, &cursor_moved(mid_x, bar_y));
            assert!(
                dispatch(&mut s, &mouse(ElementState::Pressed)),
                "press on h-thumb handled"
            );
            assert!(s.hbar.is_dragging(), "h-thumb press begins a drag");
            dispatch(&mut s, &cursor_moved(mid_x + 80.0, bar_y));
            assert!(
                s.offset_x() > 0.0,
                "drag right increases offset_x: {}",
                s.offset_x()
            );
            dispatch(&mut s, &mouse(ElementState::Released));
            assert!(!s.hbar.is_dragging(), "release ends the h-drag");
        }

        {
            let mut s = vertical(3000.0); // viewport 300, max_scroll 2700
            let bar_x = s.rect.x1 - BAR_MARGIN - BAR_THICKNESS / 2.0;
            let thumb = s.vertical_thumb().unwrap();
            let click_y = thumb.y1 + (s.rect.y1 - thumb.y1) * 0.8;
            dispatch(&mut s, &cursor_moved(bar_x, click_y));
            assert!(
                dispatch(&mut s, &mouse(ElementState::Pressed)),
                "track press handled"
            );
            assert!(
                s.hold.is_some(),
                "a track press below the thumb arms a hold"
            );
            assert!(!s.needs_animation_frame(), "a repeat is a wait, not motion");
            assert_eq!(
                s.next_wake_deadline(),
                s.hold.map(|h| h.next_at),
                "a held track pins the bar and wakes for its next page"
            );
            let after_first = s.offset_y();
            assert!(after_first > 0.0, "first page moved down: {after_first}");
            assert!(
                s.wants_pointer_capture(),
                "a live hold captures the pointer"
            );

            let mut prev = after_first;
            let mut steps = 0;
            for _ in 0..40 {
                let due = s.hold.map(|h| h.next_at).unwrap();
                s.advance_hold(due + Duration::from_millis(1));
                if s.offset_y() > prev {
                    steps += 1;
                    prev = s.offset_y();
                } else {
                    break; // thumb reached the click point → paging stopped
                }
            }
            assert!(
                steps >= 2,
                "the hold auto-repeats past the first page ({steps} extra steps)"
            );

            let thumb_now = s.vertical_thumb().unwrap();
            assert!(
                thumb_now.y1 >= click_y - BAR_THICKNESS && thumb_now.y0 <= click_y + 1.0,
                "paging stopped at the click, thumb=({},{}) click_y={click_y}",
                thumb_now.y0,
                thumb_now.y1,
            );

            assert!(
                dispatch(&mut s, &mouse(ElementState::Released)),
                "release handled"
            );
            assert!(s.hold.is_none(), "release ends the hold");
        }
    }

    phase edge_fades_show_only_where_more_content_can_scroll_in;
    {
        use crate::surface::ContentEdge;
        let mut scroll = vertical(900.0).with_edge_fade();
        assert_eq!(scroll.faded_edges(), vec![ContentEdge::Bottom]);
        scroll.set_offset_y(100.0);
        assert_eq!(
            scroll.faded_edges(),
            vec![ContentEdge::Top, ContentEdge::Bottom]
        );
        scroll.set_offset_y(f32::MAX);
        assert_eq!(scroll.faded_edges(), vec![ContentEdge::Top]);
        assert!(vertical(900.0).faded_edges().is_empty(), "off unless asked");
        assert!(
            vertical(100.0).with_edge_fade().faded_edges().is_empty(),
            "fits"
        );
    }

    phase bars_keep_out_of_the_windows_rounded_corners_only;
    {
        let draw = |s: &mut Scrollable, theme: &Theme| {
            s.draw(&mut crate::draw::DrawCtx::new(&mut Scene::new(), Affine::IDENTITY, &mut TextRenderer::new(), theme, PresenceCtx::empty().draw_env()));
        };
        let mut square = both(900.0, 900.0);
        let mut theme = Theme::dark();
        draw(&mut square, &theme);
        let (v_square, h_square) = (
            square.vertical_track().expect("tall content shows the bar"),
            square
                .horizontal_track()
                .expect("wide content shows the bar"),
        );

        let radius = f64::from(theme.tokens.radii.tile);
        theme.surface = Some(crate::theme::SurfaceShape::rounded(
            Rect::new(0.0, 0.0, 300.0, 300.0),
            radius,
        ));
        let mut rounded = both(900.0, 900.0);
        draw(&mut rounded, &theme);
        let v = rounded.vertical_track().expect("bar");
        let h = rounded.horizontal_track().expect("bar");
        assert!((v.y0 - radius).abs() < 1e-9, "the top corner: {v:?}");
        assert!(
            (h.x0 - radius).abs() < 1e-9,
            "the bottom-left corner: {h:?}"
        );
        assert!(v.y1 <= v_square.y1 && h.x1 <= h_square.x1);

        theme.surface = Some(crate::theme::SurfaceShape::rounded(
            Rect::new(0.0, 0.0, 300.0, 300.0),
            0.0,
        ));
        draw(&mut rounded, &theme);
        assert_eq!(rounded.vertical_track(), Some(v_square));
        assert_eq!(rounded.horizontal_track(), Some(h_square));

        theme.surface = Some(crate::theme::SurfaceShape::rounded(
            Rect::new(0.0, 0.0, 300.0, 300.0),
            radius,
        ));
        let mut alone = Scrollable::new_vertical();
        alone.rect = Rect::new(0.0, 0.0, 300.0, 300.0);
        alone.set_viewport_size(300.0, 300.0);
        alone.set_content_size(300.0, 1200.0);
        draw(&mut alone, &theme);
        let track = alone.vertical_track().expect("bar");
        assert!((track.y1 - (300.0 - radius)).abs() < 1e-9, "{track:?}");
    }

}

use crate::widget::{Column, Row, Text};
use std::collections::HashMap;

/// `scroll` laid out under a parent that takes its height from what it holds, which is what a
/// scroll on its own gets when nothing bounds it.
fn laid_out_under_indefinite_parent(mut scroll: Scrollable) -> Scrollable {
    let theme = Theme::dark();
    let mut taffy = TaffyTree::<()>::new();
    let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
    let mut tr = TextRenderer::new();
    let scroll_node = scroll.build_layout(&mut taffy, &theme, &mut node_map);
    let parent = taffy.parent_node(
        Style {
            display: taffy::Display::Block,
            size: Size {
                width: Dimension::length(400.0),
                height: Dimension::auto(),
            },
            ..Default::default()
        },
        &[scroll_node],
    );
    taffy
        .compute_layout_with_measure(
            parent,
            Size::max_content(),
            |known, available, node_id, _, _| {
                if let Some(&ptr) = node_map.get(&node_id) {
                    unsafe { (*ptr).measure(known, available, &mut tr) }
                } else {
                    Size::ZERO
                }
            },
        )
        .unwrap();
    scroll.apply_layout(&taffy, scroll_node, Point::ZERO);
    scroll
}

fn viewport_height_under_indefinite_parent(scroll: Scrollable) -> f64 {
    laid_out_under_indefinite_parent(scroll).get_rect().height()
}

/// Solve `root` the way the host does: measure, adjust, repeat.
fn host_solve(root: &mut dyn Widget, width: f32, height: f32) {
    use std::collections::HashMap;
    let theme = Theme::dark();
    let mut tree = TaffyTree::<()>::new();
    let mut nodes: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
    let node = root.build_layout(&mut tree, &theme, &mut nodes);
    let mut bindings = crate::widget::bound_nodes::BoundNodeMap::new();
    let mut text = crate::font_catalog::fixture_renderer();
    // SAFETY: exact live, exclusively owned widget tree and matching maps.
    unsafe {
        bindings.rebuild(root, &tree, node, &nodes);
        crate::widget::compute_widget_layout(
            &mut tree,
            node,
            Size {
                width: taffy::AvailableSpace::Definite(width),
                height: taffy::AvailableSpace::Definite(height),
            },
            &nodes,
            &bindings,
            &mut text,
        )
        .unwrap();
    }
    root.apply_layout(&tree, node, Point::ZERO);
}

fn scroll_of(root: &mut dyn Widget) -> &mut Scrollable {
    fn find(widget: &mut dyn Widget) -> Option<&mut Scrollable> {
        if widget.name() == "Scrollable" {
            return widget.as_any_mut()?.downcast_mut::<Scrollable>();
        }
        widget.children_mut().into_iter().find_map(find)
    }
    find(root).expect("a scrollable")
}

fn tall_child() -> Element {
    let mut col = Column::new().with_intrinsic_height();
    for i in 0..40 {
        col = col.push(Box::new(Text::new(&format!("row {i}"))));
    }
    Box::new(col)
}

fn can_scroll_under_indefinite_parent(mut scroll: Scrollable) -> (f64, bool) {
    scroll = laid_out_under_indefinite_parent(scroll);
    let vh = scroll.get_rect().height();
    scroll.scroll_by(0.0, 200.0);
    (vh, scroll.offset_y() > 0.0)
}

fn both(content_w: f32, content_h: f32) -> Scrollable {
    let mut s = Scrollable::new(ScrollAxis::Both);
    s.rect = Rect::new(0.0, 0.0, 300.0, 300.0);
    s.set_viewport_size(300.0, 300.0);
    s.set_content_size(content_w, content_h);
    s
}

use crate::synthetic_input::named_key;

#[test]
fn persistent_overflow_bar_paints_before_input_and_after_idle_without_ticks() {
    use crate::app::driver::Driver;
    use crate::widget::{Column, Text};
    for theme in [Theme::dark(), Theme::light(), Theme::high_contrast()] {
        let mut content = Column::new().with_no_padding().with_intrinsic_height();
        for index in 0..71 {
            content = content.push(Box::new(Text::new(&format!("Application {index}"))));
        }
        let mut host = Driver::new_with_theme(
            crate::State::new(),
            Box::new(
                Scrollable::new_vertical()
                    .always_visible()
                    .with_child(Box::new(content)),
            ),
            (400, 300),
            theme.clone(),
        );
        host.with_widget_mut(|scroll: &mut Scrollable| {
            assert!(
                scroll.max_offset_y() > 0.0,
                "real laid-out content must overflow"
            );
            let track = scroll.vertical_track().unwrap();
            assert!(
                scroll.child.as_ref().unwrap().get_rect().x1 <= track.x0,
                "the persistent bar must not cover content"
            );
            scroll.last_activity = None;
            for activity in [None, Some(crate::time::now() - Duration::from_secs(30))] {
                scroll.last_activity = activity;
                let mut scene = Scene::new();
                scroll.draw_scrollbars(&mut scene, Affine::IDENTITY, &theme);
                assert!(
                    !scene.encoding().path_tags.is_empty(),
                    "overflow must paint without hover or input"
                );
                assert_eq!(scroll.bar_opacity(&theme), 1.0);
                assert!(
                    !scroll.needs_animation_frame(),
                    "persistent bars must not animate at rest"
                );
                assert!(scroll.next_wake_deadline().is_none(), "no idle fade wake");
            }
            scroll.set_content_size(100.0, 100.0);
            let mut scene = Scene::new();
            scroll.draw_scrollbars(&mut scene, Affine::IDENTITY, &theme);
            assert!(
                scene.encoding().path_tags.is_empty(),
                "fitting content has no bar"
            );
        })
        .unwrap();
    }
}

/// A scroll a view asks to show its bar (a card held to a height, its words going on below)
/// paints its thumb, in the theme's thumb ink, from its first frame and with no input: the
/// person sees there is more before they scroll. The default scroll keeps the overlay bar,
/// which shows only once it is scrolled.
#[test]
fn a_scroll_that_shows_its_bar_paints_it_while_it_overflows_without_input() {
    use crate::app::driver::Driver;
    use crate::component::Described;
    use crate::widget::{Column, Text};
    let theme = Theme::dark();
    for shows_bar in [true, false] {
        let mut content = Column::new().with_no_padding().with_intrinsic_height();
        for index in 0..40 {
            content = content.push(Box::new(Text::new(&format!("Due this week {index}"))));
        }
        let scroll = Scrollable::from_props(
            "turn-scroll",
            super::component::ScrollProps {
                shows_bar,
                content: Some(Box::new(content)),
                ..super::component::ScrollProps::default()
            },
        );
        let mut host = Driver::new_with_theme(
            crate::State::new(),
            Box::new(scroll),
            (400, 300),
            theme.clone(),
        );
        host.with_widget_mut(|scroll: &mut Scrollable| {
            assert!(scroll.max_offset_y() > 0.0, "the words overflow");
            let mut scene = Scene::new();
            scroll.draw_scrollbars(&mut scene, Affine::IDENTITY, &theme);
            assert_eq!(
                !scene.encoding().path_tags.is_empty(),
                shows_bar,
                "the bar shows at rest exactly when the view asked for it"
            );
        })
        .unwrap();
    }
}

#[test]
fn stable_vertical_gutter_keeps_media_columns_across_overflow() {
    use crate::app::driver::Driver;
    use crate::widget::{MediaAspect, MediaCard, MediaGrid, MediaSource};
    for theme in [
        Theme::dark(),
        Theme::light(),
        Theme::dark().with_text_scale(2.0),
    ] {
        let mount = |count| {
            let cards = (0..count)
                .map(|i| {
                    MediaCard::new(format!("gutter-card-{i}"), MediaSource::Placeholder)
                        .aspect(MediaAspect::Wide)
                        .title("A media title")
                        .meta("The creator")
                })
                .collect();
            Driver::new_with_theme(
                crate::State::new(),
                Box::new(
                    Scrollable::new_vertical()
                        .with_stable_vertical_gutter()
                        .with_child(Box::new(MediaGrid::new(cards))),
                ),
                (952, 800),
                theme.clone(),
            )
        };
        let mut short = mount(3);
        let mut long = mount(12);
        let short_root = short.inspection().root;
        let long_root = long.inspection().root;
        let short_grid = &short_root.children[0];
        let long_grid = &long_root.children[0];
        assert_eq!(
            short_grid.rect[2], long_grid.rect[2],
            "the short page keeps the same content width as its overflowing sibling"
        );
        assert!(
            (short_grid.children[0].rect[2] - long_grid.children[0].rect[2]).abs() < 1.0,
            "the real measured media cards retain equal widths"
        );
        let short_bar = short
            .with_widget_mut(|scroll: &mut Scrollable| scroll.vbar_active())
            .unwrap();
        let long_bar = long
            .with_widget_mut(|scroll: &mut Scrollable| scroll.vbar_active())
            .unwrap();
        assert!(
            !short_bar,
            "a stable empty gutter does not fabricate a scrollbar"
        );
        assert!(long_bar, "overflow still exposes the real scrollbar");
        assert!(
            (short_grid.rect[2] - (952.0 - f64::from(theme.tokens.metrics.scrollbar_gutter))).abs()
                < 0.1,
            "the reserved gutter follows the actual theme metric"
        );
    }
}

/// A field that holds the keyboard focus and handles no key: a note's
/// paragraph has no page of its own to move through.
struct FocusedField {
    rect: Rect,
}

impl Widget for FocusedField {
    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        _theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let style = Style {
            size: Size::from_lengths(200.0, 2000.0),
            ..Default::default()
        };
        taffy.widget_leaf(style, node_map, self)
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        self.rect = crate::widget::layout_rect(taffy, node, origin);
    }

    fn is_focused(&self) -> bool {
        true
    }

    fn name(&self) -> &'static str {
        "FocusedField"
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }
}

/// A 100 px block that asks a scroll view around it to keep it in view, by `name`.
struct Kept {
    rect: Rect,
    name: &'static str,
}

impl Widget for Kept {
    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        _theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let style = Style {
            size: Size::from_lengths(100.0, 100.0),
            ..Default::default()
        };
        taffy.widget_leaf(style, node_map, self)
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        self.rect = crate::widget::layout_rect(taffy, node, origin);
    }

    fn name(&self) -> &'static str {
        "Kept"
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }

    fn kept_in_view(&self) -> Option<&str> {
        Some(self.name)
    }
}

/// A scroll view keeps the descendant that names itself in view when that name changes: the
/// row of strips shows the front one as the person moves between windows, without waiting to be
/// panned there, and a second layout with the same name leaves it where it is.
#[test]
fn a_scroll_keeps_the_named_descendant_in_view() {
    let mut row = crate::widget::Row::new().no_wrap();
    for index in 0..8 {
        row = row.push(Box::new(Kept {
            rect: Rect::ZERO,
            name: if index == 7 { "front" } else { "" },
        }));
    }
    let mut scroll = Scrollable::new_horizontal().with_child(Box::new(row));
    scroll.keep_in_view("front");
    let mut taffy = TaffyTree::new();
    let mut nodes = HashMap::new();
    let node = scroll.build_layout(&mut taffy, &Theme::dark(), &mut nodes);
    taffy
        .compute_layout(
            node,
            Size {
                width: taffy::AvailableSpace::Definite(400.0),
                height: taffy::AvailableSpace::Definite(400.0),
            },
        )
        .unwrap();
    scroll.apply_layout(&taffy, node, Point::ZERO);
    let moved = scroll.offset_x();
    assert!(
        moved > 0.0,
        "the scroll moves to show the one kept in view, not left at {moved}"
    );
    scroll.apply_layout(&taffy, node, Point::ZERO);
    assert_eq!(
        scroll.offset_x(),
        moved,
        "the same one kept in view leaves the content where it is"
    );
}

/// A wheel goes to the scroller under the pointer, innermost first: one that does not scroll that
/// way, or that the pointer stands outside of, leaves it to the one above, as a row of windows
/// takes the sideways wheel a window's own column is no use for.
#[test]
fn a_wheel_a_scroll_refuses_reaches_the_row_above_it() {
    let block = |width: f32, height: f32| {
        Box::new(crate::testing::TestLeaf::sized(width, height)) as Element
    };
    let inner = Scrollable::new_vertical()
        .with_cursor(Point::new(50.0, 50.0))
        .with_child(block(200.0, 1600.0));
    let column = crate::widget::Container::new()
        .with_size(200.0, 300.0)
        .with_child(Box::new(inner));
    let mut strip = crate::widget::Row::new().no_wrap();
    strip = strip.push(Box::new(column)).push(block(800.0, 300.0));
    let mut row = Scrollable::new_horizontal()
        .with_cursor(Point::new(50.0, 50.0))
        .with_child(Box::new(strip));
    let mut taffy = TaffyTree::new();
    let mut nodes = HashMap::new();
    let node = row.build_layout(&mut taffy, &Theme::dark(), &mut nodes);
    taffy
        .compute_layout(
            node,
            Size {
                width: taffy::AvailableSpace::Definite(400.0),
                height: taffy::AvailableSpace::Definite(400.0),
            },
        )
        .unwrap();
    row.apply_layout(&taffy, node, Point::ZERO);
    let inner_offset = |row: &Scrollable| {
        row.children()[0].children()[0].children()[0]
            .as_any()
            .and_then(|any| any.downcast_ref::<Scrollable>())
            .map_or(0.0, Scrollable::offset_y)
    };

    let sideways = WindowEvent::MouseWheel {
        device_id: winit::event::DeviceId::dummy(),
        delta: MouseScrollDelta::PixelDelta(winit::dpi::PhysicalPosition::new(-120.0, 0.0)),
        phase: TouchPhase::Moved,
    };
    let taken = Dispatch::new()
        .layout(&taffy, node)
        .send(&mut row, &sideways);
    assert!(taken.handled, "a wheel the column refuses is the row's");
    assert_eq!(row.offset_x(), 120.0, "the row went sideways");
    assert_eq!(inner_offset(&row), 0.0, "the column keeps its place");

    let outside = cursor_moved(50.0, 380.0);
    Dispatch::new()
        .layout(&taffy, node)
        .send(&mut row, &outside);
    let taken = Dispatch::new()
        .layout(&taffy, node)
        .send(&mut row, &sideways);
    assert!(
        taken.handled,
        "the row takes a wheel outside the column's viewport"
    );
    assert_eq!(row.offset_x(), 240.0, "the row went on sideways");
    assert_eq!(inner_offset(&row), 0.0, "the column keeps its place");
}
