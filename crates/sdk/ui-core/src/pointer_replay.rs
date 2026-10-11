//! A still pointer over a rebuilt tree.
//!
//! Controls learn that the pointer is over them from its moves. A rebuild
//! (a worker result, a model refresh) or a relayout can put a fresh or moved
//! control under a pointer that has not moved since, and that control never
//! saw it arrive: a press there would be lost, and a release after a
//! rebuild that happened while the button was held would reach a control
//! that never saw the press.
//!
//! Every host keeps one [`PointerReplay`]. It remembers where the pointer
//! last moved and is marked unsettled whenever the widgets were rebuilt or
//! moved. Before the next button press or release it hands back the last
//! position as a `CursorMoved`, which the host dispatches first, so hover
//! follows the pointer and a control that moved out from under it is no
//! longer hovered.
//!
//! The release half needs the press too. A [`crate::widget::Button`] records
//! what it was when it took a press ([`record_press`]); a fresh button with
//! the same id at the same place accepts the release that ends that press
//! ([`press_continues`]). The host forgets the press before every press
//! and after every release ([`PointerReplay::end_press`]), whichever widget
//! it reached.

use std::cell::RefCell;

use vello::kurbo::Rect;
use winit::dpi::PhysicalPosition;
use winit::event::{ElementState, WindowEvent};

/// The pointer's last position over one surface, and whether the widgets
/// changed under it since.
#[derive(Debug, Default)]
pub struct PointerReplay {
    last: Option<PhysicalPosition<f64>>,
    unsettled: bool,
}

impl PointerReplay {
    /// The widgets were rebuilt or moved: a control under the still pointer
    /// may not know it is there.
    pub fn mark_unsettled(&mut self) {
        self.unsettled = true;
    }

    /// Observe `event` before the host dispatches it. Returns the move to
    /// dispatch first when `event` is a pointer button and the widgets
    /// changed since the pointer last moved. A press starts afresh: a press
    /// whose release never came (the window lost the pointer) is forgotten.
    pub fn before(&mut self, event: &WindowEvent) -> Option<WindowEvent> {
        if matches!(
            event,
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                ..
            }
        ) {
            end_press();
        }
        match event {
            WindowEvent::CursorMoved { position, .. } => {
                self.last = Some(*position);
                self.unsettled = false;
                None
            }
            WindowEvent::CursorLeft { .. } => {
                self.last = None;
                None
            }
            WindowEvent::MouseInput { device_id, .. } if self.unsettled => {
                self.unsettled = false;
                self.last.map(|position| WindowEvent::CursorMoved {
                    device_id: *device_id,
                    position,
                })
            }
            _ => None,
        }
    }

    /// Whether `event` ends the press, so the host calls
    /// [`Self::end_press`] once it has dispatched it.
    pub fn ends_press(event: &WindowEvent) -> bool {
        matches!(
            event,
            WindowEvent::MouseInput {
                state: ElementState::Released,
                ..
            }
        )
    }

    /// A release was dispatched: the press is over, whether or not a
    /// control took it.
    pub fn end_press(&mut self) {
        end_press();
    }
}

/// The control holding the current press: its id and where it was.
struct Press {
    id: String,
    rect: Rect,
}

thread_local! {
    /// Input is dispatched on the thread that owns the widgets, one physical
    /// pointer at a time, so one slot per thread holds the press.
    static PRESS: RefCell<Option<Press>> = const { RefCell::new(None) };
}

/// The control `id`, placed at `rect`, took a press.
pub(crate) fn record_press(id: &str, rect: Rect) {
    PRESS.with(|press| {
        *press.borrow_mut() = Some(Press {
            id: id.to_owned(),
            rect,
        });
    });
}

/// Whether a control `id` placed at `rect` is the one that took the current
/// press, rebuilt since: the same id at the same place.
pub(crate) fn press_continues(id: &str, rect: Rect) -> bool {
    PRESS.with(|press| {
        press
            .borrow()
            .as_ref()
            .is_some_and(|press| press.id == id && press.rect == rect)
    })
}

/// The current press ended where a control took it.
pub(crate) fn end_press() {
    PRESS.with(|press| press.borrow_mut().take());
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::{Cell, RefCell};
    use std::collections::HashMap;
    use std::rc::Rc;

    use taffy::{NodeId, TaffyTree};
    use vello::Scene;
    use vello::kurbo::{Affine, Point, Vec2};
    use winit::event::{DeviceId, MouseButton};

    use crate::headless::FrameHarness;
    use crate::presence::PresenceCtx;
    use crate::text_render::TextRenderer;
    use crate::theme::Theme;
    use crate::widget::{Button, EventCtx, TaffyTreeExt as _, Widget};

    fn moved(x: f64, y: f64) -> WindowEvent {
        WindowEvent::CursorMoved {
            device_id: DeviceId::dummy(),
            position: PhysicalPosition::new(x, y),
        }
    }

    fn button(state: ElementState) -> WindowEvent {
        WindowEvent::MouseInput {
            device_id: DeviceId::dummy(),
            state,
            button: MouseButton::Left,
        }
    }

    #[test]
    fn a_press_replays_the_last_position_only_after_the_widgets_changed() {
        let mut replay = PointerReplay::default();
        assert!(replay.before(&moved(10.0, 20.0)).is_none());
        assert!(
            replay.before(&button(ElementState::Pressed)).is_none(),
            "nothing changed under the pointer"
        );
        replay.mark_unsettled();
        let Some(WindowEvent::CursorMoved { position, .. }) =
            replay.before(&button(ElementState::Released))
        else {
            panic!("a release after a rebuild must replay the pointer first");
        };
        assert_eq!(position, PhysicalPosition::new(10.0, 20.0));
        assert!(
            replay.before(&button(ElementState::Pressed)).is_none(),
            "one replay settles the pointer"
        );
    }

    #[test]
    fn a_pointer_that_left_is_not_replayed() {
        let mut replay = PointerReplay::default();
        replay.before(&moved(1.0, 1.0));
        replay.before(&WindowEvent::CursorLeft {
            device_id: DeviceId::dummy(),
        });
        replay.mark_unsettled();
        assert!(replay.before(&button(ElementState::Pressed)).is_none());
    }

    #[test]
    fn a_release_ends_the_press_wherever_it_went() {
        let rect = Rect::new(0.0, 0.0, 10.0, 10.0);
        record_press("copy", rect);
        assert!(press_continues("copy", rect));
        assert!(!press_continues(
            "copy",
            rect + vello::kurbo::Vec2::new(0.0, 40.0)
        ));
        assert!(!press_continues("delete", rect));
        assert!(PointerReplay::ends_press(&button(ElementState::Released)));
        PointerReplay::default().end_press();
        assert!(!press_continues("copy", rect));

        // A press whose release never arrived does not outlive the next one.
        record_press("copy", rect);
        PointerReplay::default().before(&button(ElementState::Pressed));
        assert!(!press_continues("copy", rect));
    }

    type Staged = Rc<RefCell<Option<(Box<dyn Widget>, f64)>>>;
    type Clicks = Rc<RefCell<Vec<&'static str>>>;

    /// Shows one child and swaps in the staged replacement on its next
    /// structural poll, as an app rebuilding its content from a worker
    /// result does. The replacement sits `offset` lower.
    struct Rebuilds {
        child: Box<dyn Widget>,
        staged: Staged,
        offset: f64,
        replaced: bool,
        rect: Rect,
        /// Where the child was last placed, for a test with no harness.
        placed: Rc<Cell<Rect>>,
    }

    impl Rebuilds {
        /// The first "Copy link" button, rebuilt from `staged`.
        fn new(clicks: &Clicks, staged: &Staged) -> (Self, Rc<Cell<Rect>>) {
            let placed = Rc::new(Cell::new(Rect::ZERO));
            let root = Self {
                child: copy_link(clicks, "first"),
                staged: Rc::clone(staged),
                offset: 0.0,
                replaced: false,
                rect: Rect::ZERO,
                placed: Rc::clone(&placed),
            };
            (root, placed)
        }
    }

    impl Widget for Rebuilds {
        fn take_structure_dirty(&mut self) -> bool {
            if let Some((child, offset)) = self.staged.borrow_mut().take() {
                self.child = child;
                self.offset = offset;
                self.replaced = true;
            }
            std::mem::take(&mut self.replaced) | self.child.take_structure_dirty()
        }
        fn build_layout(
            &mut self,
            taffy: &mut TaffyTree<()>,
            theme: &Theme,
            node_map: &mut HashMap<NodeId, *mut dyn Widget>,
        ) -> NodeId {
            let child = self.child.build_layout(taffy, theme, node_map);
            taffy.widget_node(taffy::Style::default(), &[child], node_map, self)
        }
        fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
            self.rect = Rect::from_origin_size(origin, (400.0, 200.0));
            if let Ok(children) = taffy.children(node)
                && let Some(child) = children.first()
            {
                let at = origin + Vec2::new(0.0, self.offset);
                self.child.apply_layout(taffy, *child, at);
                self.placed.set(self.child.get_rect());
            }
        }
        fn draw(
            &mut self,
            scene: &mut Scene,
            transform: Affine,
            text: &mut TextRenderer,
            theme: &Theme,
            presence: &PresenceCtx,
        ) {
            self.child.draw(scene, transform, text, theme, presence);
        }
        fn handle_event(&mut self, event: &WindowEvent, ctx: &mut EventCtx<'_>) {
            self.child.handle_event(event, ctx);
        }
        fn children(&self) -> Vec<&dyn Widget> {
            vec![self.child.as_ref()]
        }
        fn children_mut(&mut self) -> Vec<&mut (dyn Widget + '_)> {
            vec![self.child.as_mut()]
        }
        fn name(&self) -> &'static str {
            "Rebuilds"
        }
        fn get_rect(&self) -> Rect {
            self.rect
        }
    }

    /// A "Copy link" button that logs `generation` when it is clicked.
    fn copy_link(clicks: &Clicks, generation: &'static str) -> Box<dyn Widget> {
        let clicks = Rc::clone(clicks);
        Box::new(
            Button::new("copy_link", "Copy link")
                .on_click(move || clicks.borrow_mut().push(generation)),
        )
    }

    /// A harness showing the first "Copy link" button, the pointer resting
    /// on its centre.
    fn hovered() -> (FrameHarness, Staged, Clicks) {
        let clicks = Clicks::default();
        let staged = Staged::default();
        let mut harness = FrameHarness::new(
            crate::State::new(),
            Box::new(Rebuilds::new(&clicks, &staged).0),
            (400, 200),
        );
        let centre = harness.targets()["copy_link"].center();
        harness.send(&moved(centre.x, centre.y));
        (harness, staged, clicks)
    }

    /// Stage a fresh, equivalent button `offset` below the first and run
    /// the frame that swaps it in, as a worker result landing does.
    fn rebuild(harness: &mut FrameHarness, staged: &Staged, clicks: &Clicks, offset: f64) {
        *staged.borrow_mut() = Some((copy_link(clicks, "rebuilt"), offset));
        harness.relayout();
        assert!(staged.borrow().is_none(), "the frame swapped the button in");
    }

    #[test]
    fn a_click_on_a_control_rebuilt_under_a_still_pointer_runs_it() {
        let (mut harness, staged, clicks) = hovered();
        let before = harness.targets()["copy_link"];
        rebuild(&mut harness, &staged, &clicks, 0.0);
        assert_eq!(harness.targets()["copy_link"], before, "the same place");
        harness.send(&button(ElementState::Pressed));
        harness.send(&button(ElementState::Released));
        assert_eq!(*clicks.borrow(), vec!["rebuilt"]);
    }

    #[test]
    fn a_rebuild_between_press_and_release_still_completes_the_click() {
        let (mut harness, staged, clicks) = hovered();
        harness.send(&button(ElementState::Pressed));
        rebuild(&mut harness, &staged, &clicks, 0.0);
        harness.send(&button(ElementState::Released));
        assert_eq!(*clicks.borrow(), vec!["rebuilt"]);
        // The press is over: a later bare release clicks nothing.
        rebuild(&mut harness, &staged, &clicks, 0.0);
        harness.send(&button(ElementState::Released));
        assert_eq!(*clicks.borrow(), vec!["rebuilt"]);
    }

    #[test]
    fn a_control_rebuilt_away_from_the_pointer_is_not_clicked() {
        let (mut harness, staged, clicks) = hovered();
        rebuild(&mut harness, &staged, &clicks, 120.0);
        harness.send(&button(ElementState::Pressed));
        harness.send(&button(ElementState::Released));
        assert!(clicks.borrow().is_empty(), "nothing is under the pointer");

        let (mut harness, staged, clicks) = hovered();
        harness.send(&button(ElementState::Pressed));
        rebuild(&mut harness, &staged, &clicks, 120.0);
        harness.send(&button(ElementState::Released));
        assert!(
            clicks.borrow().is_empty(),
            "a press does not follow its control to another place"
        );
    }

    /// The live host and an embedding one: the App replays the pointer on
    /// its own surface input, not only the headless driver.
    #[cfg(feature = "native")]
    #[test]
    fn an_app_surface_clicks_a_control_rebuilt_under_a_still_pointer() {
        struct Region;
        impl crate::app::AppSurface for Region {
            fn scale_factor(&self) -> f64 {
                1.0
            }
            fn inner_size(&self) -> winit::dpi::PhysicalSize<u32> {
                winit::dpi::PhysicalSize::new(400, 200)
            }
            fn request_redraw(&self) {}
            fn set_cursor(&self, _cursor: winit::window::CursorIcon) {}
        }
        let clicks = Clicks::default();
        let staged = Staged::default();
        let (root, placed) = Rebuilds::new(&clicks, &staged);
        let mut app =
            crate::app::App::new_with_controlled_inputs(crate::State::new(), Box::new(root));
        let mut scene = Scene::new();
        let _ = app.surface_frame(&Region, &mut scene);
        let centre = placed.get().center();
        app.surface_input(&Region, moved(centre.x, centre.y), false);

        *staged.borrow_mut() = Some((copy_link(&clicks, "rebuilt"), 0.0));
        let _ = app.surface_frame(&Region, &mut scene);
        assert!(staged.borrow().is_none(), "the frame swapped the button in");
        app.surface_input(&Region, button(ElementState::Pressed), false);
        *staged.borrow_mut() = Some((copy_link(&clicks, "again"), 0.0));
        let _ = app.surface_frame(&Region, &mut scene);
        app.surface_input(&Region, button(ElementState::Released), false);
        assert_eq!(
            *clicks.borrow(),
            vec!["again"],
            "a rebuild before the press and another before the release"
        );
    }

    #[test]
    fn a_pointer_that_left_the_pressed_control_still_cancels_after_a_rebuild() {
        let (mut harness, staged, clicks) = hovered();
        harness.send(&button(ElementState::Pressed));
        harness.send(&moved(390.0, 190.0));
        rebuild(&mut harness, &staged, &clicks, 0.0);
        harness.send(&button(ElementState::Released));
        assert!(
            clicks.borrow().is_empty(),
            "the pointer left before release"
        );
    }
}
