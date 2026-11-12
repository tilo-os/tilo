//! The Linux desktop's layers: the compositor shows each widget tree the session places on a
//! surface of its own, and the session's door reaches what it publishes.

use std::cell::RefCell;
use std::rc::Rc;

use tilo_compositor::Compositor;

use tilo_session_core::door::{Doors, Published};
use tilo_surface_host::{DesktopHost, WidgetHost};
use ui::widget::Widget;

use super::{Desk, Heard, Layer, Placement, Surfaces};

/// What a Linux session places layers on: its compositor, and the door its surfaces publish
/// through.
pub struct Hosted<'a> {
    compositor: &'a mut Compositor,
    doors: &'a mut Doors,
    theme: ui::Theme,
}

impl<'a> Hosted<'a> {
    /// The layers of the session that owns `compositor` and `doors`, drawn in `theme`.
    pub fn new(compositor: &'a mut Compositor, doors: &'a mut Doors, theme: ui::Theme) -> Self {
        Self {
            compositor,
            doors,
            theme,
        }
    }
}

impl Surfaces for Hosted<'_> {
    fn place(
        &mut self,
        name: &str,
        tree: Box<dyn Widget>,
        placement: Placement,
        heard: Heard,
    ) -> Rc<dyn Layer> {
        let mut host = WidgetHost::new(tree, placement, false).named(name);
        host.set_theme(self.theme.clone());
        let host = Rc::new(RefCell::new(
            host.covering_outputs()
                .with_keyboard(Rc::clone(&heard.wants_keyboard))
                .with_dismissal(heard.pressed_elsewhere),
        ));
        self.compositor.add_host(host.clone());
        host
    }

    fn publish(&mut self, published: Rc<RefCell<dyn Published>>) {
        self.doors.add(published);
    }
}

impl Layer for RefCell<WidgetHost> {
    fn relayout(&self) {
        self.borrow_mut().relayout();
    }

    fn set_theme(&self, theme: ui::Theme) {
        self.borrow_mut().set_theme(theme);
    }

    fn revision(&self) -> u64 {
        WidgetHost::revision(&self.borrow())
    }

    fn a11y_tree(&self) -> Option<ui::accesskit::TreeUpdate> {
        Published::a11y_tree(&mut *self.borrow_mut())
    }

    fn a11y_action(&self, target: ui::accesskit::NodeId, action: &ui::a11y::Action) -> bool {
        Published::a11y_action(&mut *self.borrow_mut(), target, action)
    }

    fn accessible_labels(&self) -> Vec<String> {
        Published::accessible_labels(&mut *self.borrow_mut())
    }

    fn controls(&self) -> Vec<(String, (f64, f64, f64, f64))> {
        Published::controls(&mut *self.borrow_mut())
    }

    fn press(&self, name: &str) -> Result<(), String> {
        Published::press(&mut *self.borrow_mut(), name)
    }
}

impl Desk for DesktopHost {
    fn shown(&self) -> Vec<(u64, ui::kurbo::Rect)> {
        self.with_policy(|policy| policy.layout().shown)
    }

    fn origin(&self) -> (i32, i32) {
        self.with_widgets(|host| host.placement().origin)
    }

    fn relayout(&self) {
        self.with_widgets(WidgetHost::relayout);
    }
}
