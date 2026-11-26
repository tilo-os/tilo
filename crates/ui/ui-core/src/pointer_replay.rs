//! A still pointer over a rebuilt tree. Controls learn that the pointer is over them from its moves. A
//! rebuild (a worker result, a model refresh) or a relayout can put a fresh or moved control under a
//! pointer that has not moved since, and that control never saw it arrive: a press there would be lost,
//! and a release after a rebuild that happened while the button was held would reach a control that
//! never saw the press.

use std::cell::RefCell;

use vello::kurbo::Rect;
use winit::dpi::PhysicalPosition;
use winit::event::{ElementState, WindowEvent};

/// The pointer's last position over one surface, and whether the widgets changed under it since.
#[derive(Debug, Default)]
pub struct PointerReplay {
    last: Option<PhysicalPosition<f64>>,
    unsettled: bool,
}

impl PointerReplay {
    /// Whether a pointer rests over the surface with nothing changed under it since it moved: only then
    /// can a relayout that moves a control under it go unnoticed, so only then is the geometry worth
    /// comparing.
    pub fn is_resting(&self) -> bool {
        self.last.is_some() && !self.unsettled
    }

    /// The widgets were rebuilt or moved: a control under the still pointer may not know it is there.
    pub fn mark_unsettled(&mut self) {
        self.unsettled = true;
    }

    /// Observe `event` before the host dispatches it. Returns the move to dispatch first when `event`
    /// is a pointer button and the widgets changed since the pointer last moved. A press starts afresh:
    /// a press whose release never came (the window lost the pointer) is forgotten.
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

    /// Whether `event` ends the press, so the host calls [`Self::end_press`] once it has dispatched it.
    pub fn ends_press(event: &WindowEvent) -> bool {
        matches!(
            event,
            WindowEvent::MouseInput {
                state: ElementState::Released,
                ..
            }
        )
    }

    /// A release was dispatched: the press is over, whether or not a control took it.
    pub fn end_press(&mut self) {
        end_press();
    }
}

/// Every widget's layout box in tree order that a pointer can reach: in a long scrolled collection only
/// the children near its view ([`tilo_ui_base::widget::Widget::for_each_child_in_clip`]), so the snapshot
/// costs what is shown, not what is held. Two snapshots of one unchanged tree are equal exactly when no
/// widget the pointer can reach moved or resized.
pub(crate) fn widget_geometry(root: &dyn tilo_ui_base::widget::Widget) -> Vec<Rect> {
    fn walk(widget: &dyn tilo_ui_base::widget::Widget, out: &mut Vec<Rect>) {
        out.push(widget.get_rect());
        widget.for_each_child_in_clip(&mut |child| walk(child, out));
    }
    let mut out = Vec::new();
    walk(root, &mut out);
    out
}

/// One target a control can press: `target` tells the control's own targets apart (`()` for a control
/// that is one target), `key` names it across a rebuild and `rect` is where it is now. The key, with
/// the place, is what a rebuilt control must match to complete a press its predecessor took: an id the
/// app gave the control, or the content that tells it apart (a row's label). An empty key never carries
/// a press across a rebuild.
#[derive(Clone, Copy, Debug)]
pub(crate) struct PressAt<'a, T = ()> {
    pub(crate) target: T,
    pub(crate) key: &'a str,
    pub(crate) rect: Rect,
}

impl<'a> PressAt<'a> {
    /// A control that is one target.
    pub(crate) fn one(key: &'a str, rect: Rect) -> Self {
        Self {
            target: (),
            key,
            rect,
        }
    }
}

/// What a release did to a control's press.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Release<T = ()> {
    /// The control holds no press: the release is not its.
    NotHeld,
    /// The pointer left the pressed target first: the release ends the press and runs nothing.
    Cancelled,
    /// A completed click on this target.
    Click(T),
}

impl<T> Release<T> {
    /// Whether the release ended a press this control held, so it is consumed.
    pub(crate) fn ended(&self) -> bool {
        !matches!(self, Self::NotHeld)
    }
}

/// The press a control took and has not seen released, carried across a rebuild.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct HeldPress<T = ()> {
    held: Option<T>,
}

impl<T> Default for HeldPress<T> {
    fn default() -> Self {
        Self { held: None }
    }
}

impl<T: Copy + PartialEq> HeldPress<T> {
    pub(crate) fn press(&mut self, at: PressAt<'_, T>) {
        record_press(at.key, at.rect);
        self.held = Some(at.target);
    }

    pub(crate) fn is_held(&self) -> bool {
        self.held.is_some()
    }

    /// A left-button release with the pointer over `over` (`None` when it is over none of this
    /// control's targets).
    pub(crate) fn release(&mut self, over: Option<PressAt<'_, T>>) -> Release<T> {
        let carried = || {
            over.filter(|at| press_continues(at.key, at.rect))
                .map(|at| at.target)
        };
        let Some(held) = self.held.take().or_else(carried) else {
            return Release::NotHeld;
        };
        end_press();
        match over {
            Some(at) if at.target == held => Release::Click(held),
            _ => Release::Cancelled,
        }
    }

    /// The gesture ended without a release this control saw (the pointer left the window, the control
    /// was disabled): forget the press, here and for a control rebuilt in its place.
    pub(crate) fn cancel(&mut self) {
        if self.held.take().is_some() {
            end_press();
        }
    }
}

struct Press {
    id: String,
    rect: Rect,
}

thread_local! {
    /// Input is dispatched on the thread that owns the widgets, one physical pointer at a time, so one
    /// slot per thread holds the press.
    static PRESS: RefCell<Option<Press>> = const { RefCell::new(None) };
}

/// The control `id`, placed at `rect`, took a press. A control with no id cannot be recognised after a
/// rebuild, so its press is not recorded.
fn record_press(id: &str, rect: Rect) {
    PRESS.with(|press| {
        *press.borrow_mut() = (!id.is_empty()).then(|| Press {
            id: id.to_owned(),
            rect,
        });
    });
}

/// Whether a control `id` placed at `rect` is the one that took the current press, rebuilt since: the
/// same id at the same place.
fn press_continues(id: &str, rect: Rect) -> bool {
    PRESS.with(|press| {
        press
            .borrow()
            .as_ref()
            .is_some_and(|press| press.id == id && press.rect == rect)
    })
}

fn end_press() {
    PRESS.with(|press| press.borrow_mut().take());
}

#[cfg(test)]
mod tests {
    use super::*;
    use vello::kurbo::Rect;
    use winit::event::{DeviceId, MouseButton};

    fn moved(x: f64, y: f64) -> WindowEvent {
        tilo_ui_base::synthetic_input::cursor_moved(x, y)
    }

    fn button(state: ElementState) -> WindowEvent {
        tilo_ui_base::synthetic_input::mouse_input(state, MouseButton::Left)
    }

    tilo_test_env::scenario! {
        fn a_pointer_is_replayed_once_after_a_rebuild_and_a_press_ends_with_its_release;

        phase a_press_replays_the_last_position_only_after_the_widgets_changed;
        {
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

        phase a_pointer_that_left_is_not_replayed;
        {
            let mut replay = PointerReplay::default();
            replay.before(&moved(1.0, 1.0));
            replay.before(&WindowEvent::CursorLeft {
                device_id: DeviceId::dummy(),
            });
            replay.mark_unsettled();
            assert!(replay.before(&button(ElementState::Pressed)).is_none());
        }

        phase a_release_ends_the_press_wherever_it_went;
        {
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
    }
}
