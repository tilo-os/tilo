//! A strip across several displays: one section per display, each laid out and panned on its
//! own.

use super::tests::{dispatch_at, make_group, press_at, relayout_row_owned};
use super::*;

/// A 1000-wide display with the wheel gutter beside an 800-wide one whose bottom edge is
/// 200 rows higher.
pub(super) fn two_sections() -> Vec<StripSection> {
    vec![
        StripSection {
            output: 0,
            area: Rect::new(0.0, 0.0, 1000.0, 600.0),
            usable: 1000.0,
            leading: true,
        },
        StripSection {
            output: 1,
            area: Rect::new(1000.0, 0.0, 1800.0, 400.0),
            usable: 800.0,
            leading: false,
        },
    ]
}

/// Groups 100..=300 on the first display, 400..=600 on the second.
pub(super) fn partitioned_row() -> GroupRow {
    let groups: Vec<WindowGroup> = (1..=6_u64)
        .map(|i| make_group(i.saturating_mul(100), &[i]))
        .collect();
    let mut row = GroupRow::new(groups);
    let assignment: HashMap<u64, u32> = (1..=6_u64)
        .map(|i| (i.saturating_mul(100), u32::from(i > 3)))
        .collect();
    row.set_sections(&two_sections(), &assignment);
    let (row, _, _) = relayout_row_owned(row, 1800.0);
    let (row, _, _) = relayout_row_owned(row, 1800.0);
    row
}

fn rect_of(row: &GroupRow, id: u64) -> Rect {
    row.groups
        .iter()
        .find(|g| g.id == id)
        .map(WindowGroup::get_rect)
        .expect("group laid out")
}

tilo_test_env::scenario! {
    fn each_display_lays_out_pans_and_takes_input_on_its_own;

    phase a_partitioned_strip_lays_each_display_out_on_its_own;
    {
        let row = partitioned_row();
        let sections = two_sections();
        for id in [100, 200, 300] {
            let r = rect_of(&row, id);
            assert!(
                r.x0 >= sections[0].area.x0,
                "{id} starts left of its display: {r:?}"
            );
            assert!(
                r.y1 <= sections[0].area.y1 + 0.5,
                "{id} is taller than its display"
            );
            assert_eq!(row.output_of_group(id), Some(0));
        }
        let first_on_second = rect_of(&row, 400);
        assert!(
            (first_on_second.x0 - sections[1].area.x0).abs() < 0.5,
            "the second display's first window starts at its own left edge: {first_on_second:?}"
        );
        for id in [400, 500, 600] {
            let r = rect_of(&row, id);
            assert!(
                r.x0 >= sections[1].area.x0,
                "{id} leaks onto the first display: {r:?}"
            );
            assert!(
                r.y1 <= sections[1].area.y1 + 0.5,
                "{id} runs past the shorter display's bottom edge: {r:?}"
            );
            assert_eq!(row.output_of_group(id), Some(1));
        }
        let border = row.handle_at(2).expect("handle after the third group");
        assert!(border.get_rect().area() <= 0.0, "{:?}", border.get_rect());
        assert_eq!(row.group_ids_on(1), vec![400, 500, 600]);
    }

    phase a_strip_shown_on_one_display_takes_no_input_on_the_other;
    {
        let row = partitioned_row();
        let (mut row, taffy, root) = relayout_row_owned(row, 1800.0);
        let _ = row.take_structure_dirty();
        row.set_shown_outputs(&[0]);
        row.apply_layout(&taffy, root, Point::ORIGIN);
        assert!(
            !row.take_structure_dirty(),
            "moving a section needs no new layout"
        );
        assert!(
            row.covers(Point::new(500.0, 300.0)),
            "the first display still shows the strip"
        );
        assert!(
            !row.covers(Point::new(1400.0, 200.0)),
            "the second display shows another strip, not this one"
        );
    }

    phase panning_one_display_leaves_the_other_where_it_was;
    {
        let mut row = partitioned_row();
        let before: Vec<Rect> = [100, 200, 300]
            .iter()
            .map(|&id| rect_of(&row, id))
            .collect();
        assert!(
            row.scroll_group_into_view(600),
            "the last window is off the second display"
        );
        let (row, _, _) = relayout_row_owned(row, 1800.0);
        let after: Vec<Rect> = [100, 200, 300]
            .iter()
            .map(|&id| rect_of(&row, id))
            .collect();
        assert_eq!(before, after, "the first display must not move");
        assert!(row.output_pan(1).expect("section").scroll_x() > 0.0);
        assert_eq!(row.output_pan(0).expect("section").scroll_x(), 0.0);
        let last = rect_of(&row, 600);
        assert!(last.x1 <= 1800.5 && last.x0 >= 1000.0, "{last:?}");
    }

    phase a_window_reaching_past_its_display_takes_no_pointer_on_the_next;
    {
        use crate::widget::app_widget::{AppWidget, IconDescriptor};
        use crate::widget::{AppKind, EventCtx};
        use std::cell::RefCell;
        use std::rc::Rc;
        use winit::event::MouseScrollDelta;

        /// What the app inside a window heard from the pointer.
        #[derive(Default)]
        struct Heard {
            moves: u32,
            presses: u32,
            wheels: u32,
        }

        struct Hears {
            heard: Rc<RefCell<Heard>>,
            rect: Rect,
        }

        impl Widget for Hears {
            fn build_layout(
                &mut self,
                taffy: &mut TaffyTree<()>,
                _theme: &Theme,
                node_map: &mut HashMap<NodeId, *mut dyn Widget>,
            ) -> NodeId {
                taffy.fill_leaf(node_map, self)
            }
            crate::rect_geometry!();
            fn name(&self) -> &'static str {
                "Hears"
            }
            fn handle_event(&mut self, event: &WindowEvent, _ctx: &mut EventCtx<'_>) {
                let mut heard = self.heard.borrow_mut();
                match event {
                    WindowEvent::CursorMoved { .. } => heard.moves += 1,
                    WindowEvent::MouseInput {
                        state: ElementState::Pressed,
                        button: MouseButton::Left,
                        ..
                    } => heard.presses += 1,
                    WindowEvent::MouseWheel { .. } => heard.wheels += 1,
                    _ => {}
                }
            }
        }

        impl AppWidget for Hears {
            fn app_kind(&self) -> AppKind {
                AppKind::Files
            }
            fn app_title(&self) -> &'static str {
                "Hears"
            }
            fn app_icon(&self) -> IconDescriptor {
                IconDescriptor::Glyph("H")
            }
        }

        /// One window on the first display, laid out wider than that display so its rect reaches
        /// past the edge, and nothing on the second.
        fn reaching_row() -> (
            GroupRow,
            TaffyTree<()>,
            NodeId,
            Rc<RefCell<Heard>>,
        ) {
            let heard = Rc::new(RefCell::new(Heard::default()));
            let group = WindowGroup::new(
                100,
                vec![
                    (
                        TabDescriptor::new(1, "Hears"),
                        TileContent::App(Box::new(Hears {
                            heard: Rc::clone(&heard),
                            rect: Rect::ZERO,
                        })),
                    ),
                    (
                        TabDescriptor::new(2, "Notes"),
                        TileContent::Stub {
                            kind: AppKind::Settings,
                            label: "Notes".into(),
                        },
                    ),
                ],
                1,
                WindowWidth::Fraction(1600),
            );
            let mut row = GroupRow::new(vec![group]);
            row.set_sections(&two_sections(), &HashMap::from([(100, 0)]));
            let (row, _, _) = relayout_row_owned(row, 1800.0);
            let (row, taffy, root) = relayout_row_owned(row, 1800.0);
            (row, taffy, root, heard)
        }

        fn a_click_hover_or_wheel_on_the_next_displays_canvas_reaches_no_app() {
            let (mut row, taffy, root, heard) = reaching_row();
            let past = Point::new(1400.0, 200.0);
            let rect = rect_of(&row, 100);
            assert!(
                rect.x1 > 1200.0,
                "fixture: the window reaches onto the second display: {rect:?}"
            );
            assert!(
                rect.contains(past),
                "fixture: its laid-out rect holds the point: {rect:?}"
            );
            assert_eq!(row.section_at(past), Some(1), "the point stands on display 2");

            press_at(&mut row, &taffy, root, past);
            dispatch_at(
                &mut row,
                &taffy,
                root,
                &WindowEvent::MouseWheel {
                    device_id: winit::event::DeviceId::dummy(),
                    delta: MouseScrollDelta::LineDelta(0.0, -1.0),
                    phase: winit::event::TouchPhase::Moved,
                },
            );
            let heard = heard.borrow();
            assert_eq!(
                heard.moves, 0,
                "hovering the second display's canvas reaches no app"
            );
            assert_eq!(
                heard.presses, 0,
                "a press on the second display's canvas reaches no app"
            );
            assert_eq!(
                heard.wheels, 0,
                "a wheel on the second display's canvas reaches no app"
            );
        }

        fn a_pointer_on_the_windows_own_display_still_reaches_it() {
            let (mut row, taffy, root, heard) = reaching_row();
            let own = Point::new(500.0, 200.0);
            assert_eq!(
                row.section_at(own),
                Some(0),
                "the point stands on the window's own display"
            );
            press_at(&mut row, &taffy, root, own);
            assert_eq!(heard.borrow().moves, 1, "the move reaches the app");
            assert_eq!(heard.borrow().presses, 1, "so does the press");
        }

        fn a_tab_dragged_onto_the_next_display_keeps_its_pointer() {
            let (mut row, taffy, root, _heard) = reaching_row();
            let pill = row.groups[0].tab_rects()[1].center();
            press_at(&mut row, &taffy, root, pill);
            assert!(
                !row.group_drag_active(),
                "fixture: a bar of several tabs keeps a press on its tab, not the window"
            );
            assert!(
                row.groups[0].wants_pointer_capture(),
                "the tab the person pressed holds the pointer"
            );
            dispatch_at(
                &mut row,
                &taffy,
                root,
                &WindowEvent::CursorMoved {
                    device_id: winit::event::DeviceId::dummy(),
                    position: winit::dpi::PhysicalPosition::new(1400.0, 200.0),
                },
            );
            assert!(
                row.groups[0].drag_in_flight(),
                "the drag follows the pointer onto the next display"
            );
        }

        a_click_hover_or_wheel_on_the_next_displays_canvas_reaches_no_app();
        a_pointer_on_the_windows_own_display_still_reaches_it();
        a_tab_dragged_onto_the_next_display_keeps_its_pointer();
    }
}
