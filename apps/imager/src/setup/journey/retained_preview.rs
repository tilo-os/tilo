//! Suspend owned preparation across a media surface, while the framework
//! retires and renews its actual action bindings on every surface transition.
use super::*;
use std::rc::Weak;
use ui::widget::Decorator;

pub(super) struct RetainedPreview {
    preview: Option<InstallerPreview>,
    suspended: Weak<RefCell<Option<InstallerPreview>>>,
    returned: bool,
}
impl RetainedPreview {
    pub fn new(
        preview: InstallerPreview,
        suspended: &Rc<RefCell<Option<InstallerPreview>>>,
        returned: bool,
    ) -> Self {
        Self {
            preview: Some(preview),
            suspended: Rc::downgrade(suspended),
            returned,
        }
    }
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
impl ResourceSurface<Imager, InstallerPreviewEffect> for RetainedPreview {
    fn present(&mut self, state: &Rc<Imager>) {
        self.inner_mut().present(state);
    }
    fn attach(&mut self, cx: SurfaceCx<Imager>) {
        if self.returned {
            self.inner_mut().reattach_after_media(cx);
        } else {
            self.inner_mut().attach(cx);
        }
    }
    fn stage(&mut self, scenario: Option<&str>) {
        self.inner_mut().stage(scenario);
    }
    fn effect(
        &mut self,
        effect: InstallerPreviewEffect,
        _cx: &mut EffectCx<Imager, Self>,
    ) -> EffectAnswer {
        self.inner_mut().answer_preview(effect)
    }
    fn effects(&self) -> Vec<EffectStatus> {
        self.inner().effects()
    }
}
