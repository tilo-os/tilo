//! History mode for every framework app: past versions rendered by the app's
//! own view, read-only.
//!
//! A view is a function of the resource state, so a historical version needs
//! no separate renderer: the framework reads the resource from the frozen
//! document, builds the view again with editing disabled, and lays it out in
//! History mode's preview pane.

use std::any::TypeId;

use tilo_resource::Resource;
use ui_core::State;
use ui_core::Theme;
use ui_core::automerge::AutoCommit;
use ui_core::history_mode::{
    HistoryDefinition, HistoryPresentation, HistoryPreview, HistoryScopeDefinition,
    PreviewAccessibility, PreviewInput, PreviewNavigation, PreviewView,
};
use ui_core::launch::ShotFrame;
use ui_core::presence::PresenceCtx;
use ui_core::text_render::TextRenderer;
use ui_core::vello::Scene;
use ui_core::vello::kurbo::{Affine, Rect, Vec2};
use ui_core::widget::Widget;

use super::app::{ResourceApp, build_root, leaked};
use super::root::ResourceRoot;

struct HistoryKey<A>(std::marker::PhantomData<A>);

/// The History mode registration of `A`: one scope, previewed by its view.
pub(crate) fn history_definition<A: ResourceApp>() -> &'static HistoryDefinition {
    leaked(TypeId::of::<HistoryKey<A>>(), || {
        let scopes: &'static [HistoryScopeDefinition] =
            Box::leak(Box::new([HistoryScopeDefinition {
                key: "resource",
                title: <A::Resource as Resource>::LABEL,
                presentation: HistoryPresentation::Domain {
                    create: preview::<A>,
                },
                content_schema: None,
            }]));
        HistoryDefinition {
            app_id: A::ID,
            scopes,
        }
    })
}

fn preview<A: ResourceApp>(input: PreviewInput) -> Result<Box<dyn HistoryPreview>, String> {
    let doc = AutoCommit::load(input.snapshot.bytes()).map_err(|error| error.to_string())?;
    let state = <A::Resource as Resource>::read(&doc).map_err(|error| error.to_string())?;
    Ok(Box::new(ViewPreview {
        root: build_root::<A>(state, true),
        doc: Some(doc),
        bounds: Rect::ZERO,
        view: input.initial_view,
    }))
}

struct ViewPreview<R: Resource> {
    root: ResourceRoot<R>,
    doc: Option<AutoCommit>,
    bounds: Rect,
    view: PreviewView,
}

impl<R: Resource> HistoryPreview for ViewPreview<R> {
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "a preview pane is a few thousand logical pixels"
    )]
    fn layout(
        &mut self,
        bounds: Rect,
        _text: &mut TextRenderer,
        theme: &Theme,
    ) -> Result<(), String> {
        self.bounds = bounds;
        let size = (
            bounds.width().max(1.0) as u32,
            bounds.height().max(1.0) as u32,
        );
        let mut frame = ShotFrame::new(size, theme.clone());
        if let Some(doc) = self.doc.take() {
            frame = frame.with_state(State::from_doc(doc));
        }
        frame.sync(&mut self.root);
        frame
            .layout(&mut self.root)
            .map_err(|error| error.to_string())?;
        self.doc = Some(std::mem::take(&mut frame.state.doc));
        Ok(())
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text: &mut TextRenderer,
        theme: &Theme,
    ) {
        let offset = Vec2::new(self.bounds.x0, self.bounds.y0 - self.view.scroll.1);
        let transform = transform.pre_translate(offset);
        self.root
            .draw(scene, transform, text, theme, &PresenceCtx::empty());
    }

    fn navigate(&mut self, command: PreviewNavigation) -> bool {
        match command {
            PreviewNavigation::Scroll { dy, .. } => {
                let content = self.root.get_rect().height();
                let limit = (content - self.bounds.height()).max(0.0);
                let next = (self.view.scroll.1 + dy).clamp(0.0, limit);
                let changed = (next - self.view.scroll.1).abs() > f64::EPSILON;
                self.view.scroll.1 = next;
                changed
            }
            _ => false,
        }
    }

    fn view(&self) -> PreviewView {
        self.view.clone()
    }

    fn accessibility(&self) -> PreviewAccessibility {
        PreviewAccessibility {
            description: format!("{} as of this version", R::LABEL),
            pages: Vec::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::prelude::*;

    #[derive(Model, Default)]
    pub struct Page {
        pub heading: String,
    }

    #[resource(kind = "test.page", label = "Page")]
    impl Page {
        /// Set the heading.
        #[action]
        pub fn set_heading(&mut self, heading: String) {
            self.heading = heading;
        }
    }

    struct PageApp;

    impl ResourceApp for PageApp {
        type Resource = Page;
        const ID: &'static str = "so.tilo.test-page";
        const TITLE: &'static str = "Page";

        fn view(cx: &mut ViewCx<Page>) -> Element {
            Box::new(Column::new().push(Box::new(
                cx.text(|page| page.heading.clone()).with_id("heading"),
            )))
        }
    }

    fn shown(root: &ResourceRoot<Page>) -> Vec<String> {
        fn visit(widget: &dyn Widget, out: &mut Vec<String>) {
            if let Some(node) = widget.accessibility()
                && let Some(label) = node.name
            {
                out.push(label);
            }
            widget.for_each_child(&mut |child| visit(child, out));
        }
        let mut out = Vec::new();
        visit(root, &mut out);
        out
    }

    #[test]
    fn a_past_version_renders_through_the_apps_own_view_read_only() {
        let mut page = Replica::<Page>::new();
        page.apply(Page::SET_HEADING.with("First draft".into()))
            .unwrap();
        let past = page.heads();
        page.apply(Page::SET_HEADING.with("Final".into())).unwrap();
        let mut doc = page.doc().clone();
        let bytes = doc.fork_at(&past).unwrap().save();

        let frozen = AutoCommit::load(&bytes).unwrap();
        let state = <Page as Resource>::read(&frozen).unwrap();
        let mut preview = ViewPreview {
            root: build_root::<PageApp>(state, true),
            doc: Some(frozen),
            bounds: Rect::ZERO,
            view: PreviewView::default(),
        };
        let theme = Theme::dark();
        let mut text = TextRenderer::new();
        preview
            .layout(Rect::new(10.0, 20.0, 410.0, 320.0), &mut text, &theme)
            .unwrap();
        let mut scene = Scene::new();
        preview.draw(&mut scene, Affine::IDENTITY, &mut text, &theme);
        assert_eq!(shown(&preview.root), ["First draft"]);
        assert!(preview.root.get_rect().width() > 0.0);
    }
}
