//! Suspend owned preparation across a media window, while the framework
//! retires and renews its actual action bindings on every window transition.
use super::*;
use std::rc::Weak;
use ui::widget::Decorator;

struct RetainedPreview {
    preview: Option<InstallerPreview>,
    suspended: Weak<RefCell<Option<InstallerPreview>>>,
}
impl Decorator for RetainedPreview {
    type Inner = InstallerPreview;
    fn inner(&self) -> &InstallerPreview {
        self.preview.as_ref().unwrap()
    }
    fn inner_mut(&mut self) -> &mut InstallerPreview {
        self.preview.as_mut().unwrap()
    }
    fn children(&self) -> Vec<&dyn Widget> {
        vec![self.inner()]
    }
    fn for_each_child(&self, visitor: &mut dyn FnMut(&dyn Widget)) {
        visitor(self.inner());
    }
    fn children_mut(&mut self) -> Vec<&mut (dyn Widget + '_)> {
        vec![self.inner_mut()]
    }
}
impl Drop for RetainedPreview {
    fn drop(&mut self) {
        if self
            .preview
            .as_ref()
            .and_then(|preview| preview.media.as_ref())
            .is_some_and(ui::LocalState::get)
            && let Some(suspended) = self.suspended.upgrade()
        {
            *suspended.borrow_mut() = self.preview.take();
        }
    }
}
/// `preview` as a view of the imager resource, retained across a media
/// window: one coming back from it keeps what it prepared and only takes up
/// the new view's handles.
pub(super) fn view(
    cx: &ViewCx<Imager>,
    mut preview: InstallerPreview,
    suspended: &Rc<RefCell<Option<InstallerPreview>>>,
    returned: bool,
) -> ui::widget::Element {
    if returned {
        preview.reattach_after_media(cx);
    } else {
        preview.attach_view(cx);
    }
    let suspended = Rc::downgrade(suspended);
    crate::window_view::wrapped_window_view(
        cx,
        preview,
        move |preview| {
            Box::new(RetainedPreview {
                preview: Some(preview),
                suspended,
            })
        },
        InstallerPreview::effect,
    )
}
