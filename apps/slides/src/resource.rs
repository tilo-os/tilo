//! Slides as a [`tilo_resource::Resource`]: the deck is [`SlidesDeck`], the
//! editor is the surface, and `publish` is the only palette/agent door.

use std::rc::Rc;

use ui::framework::{ResourceApp, ResourceSurface, SurfaceCx, ViewCx};
use ui::widget::Element;

use crate::actions::{SlidesDeck, SlidesResourceCtx};
use crate::app::SlidesApp;

/// Framework app: the slides editor is the surface over [`SlidesDeck`].
pub struct SlidesShell;

impl ResourceApp for SlidesShell {
    type Resource = SlidesDeck;
    const ID: &'static str = "so.tilo.slides";
    const TITLE: &'static str = "Slides";
    const ICON: Option<&'static str> = Some("slides");
    const SHOT_VIEWPORT: (u32, u32) = (1280, 800);

    fn view(cx: &mut ViewCx<SlidesDeck>) -> Element {
        cx.surface(SlidesApp::new())
    }

    fn label(state: &SlidesResourceCtx) -> String {
        let title = state.deck_title.trim();
        if title.is_empty() {
            "Slides".into()
        } else {
            title.to_owned()
        }
    }
}

impl ResourceSurface<SlidesDeck> for SlidesApp {
    fn present(&mut self, state: &Rc<SlidesResourceCtx>) {
        self.adopt_resource_state(state);
    }

    fn attach(&mut self, cx: SurfaceCx<SlidesDeck>) {
        self.attach_framework(cx.dispatcher());
    }
}
