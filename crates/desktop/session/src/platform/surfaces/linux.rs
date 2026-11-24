//! The Linux desktop's layers: the compositor shows each widget tree the session places on a
//! surface of its own, and the session's API reaches what it publishes.

use std::cell::RefCell;
use std::rc::Rc;

use tilo_compositor::Compositor;

use super::{ShellDesktop, WidgetSurface};
use tilo_session_core::surfaces::Published;
use ui::widget::Widget;

use super::{Heard, Layer, Placement, Shell, Surfaces};

/// What a Linux session places layers on: its compositor, and the published side its surfaces go
/// through.
pub struct Hosted<'a> {
    compositor: &'a mut Compositor,
    published: &'a mut tilo_session_core::surfaces::Surfaces,
    theme: ui::Theme,
}

impl<'a> Hosted<'a> {
    /// The layers of the session that owns `compositor` and `published`, drawn in `theme`.
    pub fn new(
        compositor: &'a mut Compositor,
        published: &'a mut tilo_session_core::surfaces::Surfaces,
        theme: ui::Theme,
    ) -> Self {
        Self {
            compositor,
            published,
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
        let mut host = WidgetSurface::new(tree, placement, false).named(name);
        host.set_theme(self.theme.clone());
        let host = Rc::new(RefCell::new(
            host.covering_outputs()
                .with_keyboard(Rc::clone(&heard.wants_keyboard))
                .with_dismissal(heard.pressed_elsewhere),
        ));
        self.compositor.add_surface(host.clone());
        host
    }

    fn publish(&mut self, published: Rc<RefCell<dyn Published>>) {
        self.published.add(published);
    }
}

impl Layer for RefCell<WidgetSurface> {
    fn relayout(&self) {
        self.borrow_mut().relayout();
    }

    fn set_theme(&self, theme: ui::Theme) {
        self.borrow_mut().set_theme(theme);
    }

    fn revision(&self) -> u64 {
        WidgetSurface::revision(&self.borrow())
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

    fn controls(&self) -> Vec<ui::window_targets::WindowControl> {
        Published::controls(&mut *self.borrow_mut())
    }

    fn press(&self, name: &str) -> Result<(), String> {
        Published::press(&mut *self.borrow_mut(), name)
    }
}

impl Shell for ShellDesktop {
    fn shown(&self) -> Vec<(u64, ui::kurbo::Rect)> {
        self.with_policy(|policy| policy.placement().shown)
    }

    fn origin(&self) -> (i32, i32) {
        self.with_widgets(|host| host.placement().origin)
    }

    fn relayout(&self) {
        self.with_widgets(WidgetSurface::relayout);
    }
}
