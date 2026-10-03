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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::actions::{
        ADD_ELEMENT, ADD_SLIDE, DELETE_ELEMENT, MOVE_ELEMENT, MOVE_SLIDE, RESTYLE_ELEMENT,
    };
    use crate::model::{Deck, PaletteKind, Slide, seed_deck};
    use ui::framework::ResourceSurface;
    use ui::resources::PublishesResources;
    use ui::widget::Widget;

    #[test]
    fn the_deck_publish_includes_add_slide_and_theme_ops() {
        let ctx = SlidesResourceCtx::from_deck(&seed_deck());
        let published = ui::tilo_resource::publish::<SlidesDeck>("slides.deck", "Deck", &ctx);
        let ids: Vec<&str> = published.actions.iter().map(|a| a.id.as_str()).collect();
        assert!(
            ids.contains(&ADD_SLIDE),
            "add-slide must ride bound_actions through publish, not a bolt-on: {ids:?}"
        );
        for kind in PaletteKind::ALL {
            let theme = format!("theme-{}", kind.as_str());
            assert!(
                ids.contains(&theme.as_str()),
                "theme `{theme}` must ride bound_actions through publish: {ids:?}"
            );
        }
    }

    #[test]
    fn hosted_resources_keep_selection_extras_without_deck_root() {
        let ctx = SlidesResourceCtx::from_deck(&seed_deck());
        let published = ui::tilo_resource::publish::<SlidesDeck>("slides.deck", "Deck", &ctx);
        assert_eq!(published.kind, "slides.deck");
        let ids: Vec<&str> = published.actions.iter().map(|a| a.id.as_str()).collect();
        for expected in [
            ADD_ELEMENT,
            MOVE_ELEMENT,
            RESTYLE_ELEMENT,
            DELETE_ELEMENT,
            MOVE_SLIDE,
            ADD_SLIDE,
            "theme-emerald",
            ui::tilo_resource::APPLY,
            ui::tilo_resource::GROUP,
            "present",
            "editor",
            "overview",
        ] {
            assert!(
                ids.contains(&expected),
                "palette `{expected}` must ride publish: {ids:?}"
            );
        }
        assert!(
            published
                .children
                .iter()
                .all(|child| child.kind == "slides.slide"),
            "publish children are durable slides, not the live selection: {:?}",
            published
                .children
                .iter()
                .map(|child| child.kind.as_str())
                .collect::<Vec<_>>()
        );
        assert!(
            published
                .children
                .iter()
                .flat_map(|child| child.children.iter())
                .all(|child| child.kind != "slides.selection"),
            "selection is not document state"
        );

        let mut app = SlidesApp::demo(None);
        let mut state = ui::State::new();
        Widget::sync(&mut app, &mut state.doc, &state.root);
        assert!(
            !app.is_framework_hosted(),
            "standalone stays on the full action tree"
        );

        let slide = app
            .slides_resource_ctx()
            .slides
            .get(app.current_slide())
            .cloned()
            .expect("demo deck has a current slide");
        let element = slide
            .elements
            .first()
            .cloned()
            .expect("current slide has an element for selection extras");
        app.select_element_for_test(element.id.clone());

        let standalone = app.resources(&state.doc);
        assert_eq!(standalone.len(), 1);
        assert_eq!(standalone[0].kind, "slides.deck");
        let standalone_selection = standalone[0]
            .children
            .iter()
            .find(|child| child.id.0 == format!("slides.slide:{}", slide.data.id))
            .expect("current slide is published")
            .children
            .iter()
            .find(|child| child.kind == "slides.selection")
            .expect("standalone nests selection under the current slide");
        assert!(
            standalone_selection
                .actions
                .iter()
                .any(|action| action.id == "delete"),
            "standalone selection offers delete on the focused element"
        );

        app.set_framework_hosted(true);
        assert!(app.is_framework_hosted());
        let extras = app.resources(&state.doc);
        assert!(
            extras.iter().all(|resource| resource.kind != "slides.deck"),
            "hosted must not republish the deck root beside ResourceRoot: {:?}",
            extras
                .iter()
                .map(|resource| resource.id.as_str())
                .collect::<Vec<_>>()
        );
        assert_eq!(extras.len(), 1, "hosted returns the selection slice alone");
        assert_eq!(extras[0].kind, "slides.selection");
        assert_eq!(
            extras[0].id, standalone_selection.id,
            "hosted selection tree matches the standalone nested slice"
        );
        assert!(
            extras[0]
                .actions
                .iter()
                .any(|action| action.id == "delete" || action.id == "bring-front"),
            "hosted keeps selection-bound element conveniences"
        );
    }

    #[test]
    fn present_empty_does_not_wipe_unpublished_local_deck() {
        let mut app = SlidesApp::new();
        let mut local = Deck {
            title: "local-only".into(),
            palette: crate::model::PaletteKind::Indigo,
            slides: Vec::new(),
        };
        let mut slide = Slide::new(crate::model::Layout::TitleContent);
        slide.add(crate::model::Element::text(
            "Not published yet",
            96.0,
            96.0,
            400.0,
            60.0,
            32.0,
        ));
        let kept_id = slide.id.clone();
        local.slides.push(slide);
        app.seed_unpublished_deck_for_test(local);
        let empty = Rc::new(SlidesResourceCtx {
            deck_title: String::new(),
            slides: Vec::new(),
            selection: None,
        });
        ResourceSurface::<SlidesDeck>::present(&mut app, &empty);
        assert_eq!(
            app.local_slide_ids_for_test(),
            vec![kept_id],
            "empty present must not wipe an unpublished local deck"
        );
    }
}
