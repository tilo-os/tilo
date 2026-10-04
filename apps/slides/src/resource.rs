//! Slides as a [`tilo_resource::Resource`]: the deck is [`SlidesDeck`], the
//! editor is the surface, and `publish` is the only palette/agent door.

use std::rc::Rc;

use ui::framework::{Fixture, LaunchInput, ResourceApp, ResourceSurface, ViewCx};
use ui::record::RecordId;
use ui::tilo_resource::{ActionDecl, Invoke};
use ui::widget::Element;

use crate::actions::{SlidesAction, SlidesDeck, SlidesResourceCtx, replace_deck_action};
use crate::app::SlidesApp;
use crate::model::{Deck, seed_deck};

/// The media type of an ODF presentation (`.odp`).
pub const ODP_MEDIA_TYPE: &str = "application/vnd.oasis.opendocument.presentation";
/// The media type of an Office Open XML presentation (`.pptx`).
pub const PPTX_MEDIA_TYPE: &str =
    "application/vnd.openxmlformats-officedocument.presentationml.presentation";

/// The window size of the scenarios that show the narrow layout, where the
/// rail and the inspector are flaps over the canvas.
const NARROW_VIEWPORT: (u32, u32) = (820, 800);

/// Framework app: the slides editor is the surface over [`SlidesDeck`].
pub struct SlidesShell;

impl ResourceApp for SlidesShell {
    type Resource = SlidesDeck;
    const ID: &'static str = "so.tilo.slides";
    const TITLE: &'static str = "Slides";
    const ICON: Option<&'static str> = Some("slides");
    const OPENS: &'static [&'static str] = &[ODP_MEDIA_TYPE, PPTX_MEDIA_TYPE];
    const ARGUMENTS: usize = 1;

    fn view(cx: &mut ViewCx<SlidesDeck>) -> Element {
        // In a container, the editor is a widget of the view's tree that
        // tests and staging reach by its type.
        let editor = SlidesApp::new(cx);
        Box::new(ui::widget::Stack::new().push(cx.surface(editor)))
    }

    fn catalog() -> Option<&'static ui::i18n::EmbeddedCatalog> {
        Some(&crate::strings::CATALOG)
    }

    fn label(state: &SlidesResourceCtx) -> String {
        let title = state.deck_title.trim();
        if title.is_empty() {
            "Slides".into()
        } else {
            title.to_owned()
        }
    }

    /// A presentation file opens as a deck of its own, derived from the
    /// file, so opening it again returns to that deck.
    fn launch_record(input: &LaunchInput<'_>) -> Option<RecordId> {
        input
            .argument()
            .map(|path| RecordId::derived("slides", &RecordId::identify_file(path)))
    }

    /// The deck a file opens as is the file's when the record is new; a deck
    /// opened before keeps the edits made to it since.
    fn launch(input: &LaunchInput<'_>, created: bool) -> Vec<Invoke<SlidesDeck>> {
        let Some(file) = input.file().filter(|_| created) else {
            return Vec::new();
        };
        let content = crate::assets::app_content();
        let deck = if file.media_type() == PPTX_MEDIA_TYPE {
            crate::pptx::read_pptx(file.bytes(), &content)
        } else {
            crate::odp::read_odp(file.bytes(), &content)
        };
        match deck {
            Ok(deck) => replace_deck(&deck).into_iter().collect(),
            Err(error) => {
                tracing::warn!(%error, file = file.name(), "slides: the presentation cannot be read");
                Vec::new()
            }
        }
    }

    fn scenarios() -> &'static [&'static str] {
        &[
            "overview",
            "present",
            "present_notes",
            "presenter",
            "present_white",
            "empty",
            "help",
            "rename",
            "notes_edit",
            "slide_props",
            "toolbar_menu",
            "many",
            "many_rail",
            "overview_end",
            "picker",
            "image",
            "image_loading",
            "open_error",
            "narrow",
            "narrow_closed",
            "inspector",
            "element_menu",
            "slide_menu",
            "reorder",
            "edit_text",
            "delete_zone",
            "inspector_widgets",
        ]
    }

    fn shot_viewport(scenario: Option<&str>) -> (u32, u32) {
        match scenario {
            Some(
                "narrow" | "narrow_closed" | "inspector" | "many_rail" | "toolbar_menu"
                | "inspector_widgets",
            ) => NARROW_VIEWPORT,
            _ => ui::launch::DEFAULT_VIEWPORT,
        }
    }

    /// The `image` scenario's picture is in the capture's content store.
    fn fixture(fixture: &mut Fixture<SlidesDeck>) {
        if fixture.scenario() == Some("image") {
            let _ = crate::assets::store_asset(fixture.content(), &crate::sample::photo_png());
        }
    }

    /// Every scenario but `empty` opens the sample deck; the long and
    /// picture scenarios a variant of it.
    fn scenario(name: Option<&str>) -> Vec<Invoke<SlidesDeck>> {
        let deck = match name {
            Some("empty") => return Vec::new(),
            Some("many" | "many_rail") => crate::sample::long_deck(),
            Some("image") => match crate::sample::photo_src() {
                Some(src) => crate::sample::picture_deck("Site photo", &src),
                None => seed_deck(),
            },
            Some("image_loading") => {
                crate::sample::picture_deck("Importing photo", crate::sample::MISSING_SRC)
            }
            _ => seed_deck(),
        };
        replace_deck(&deck).into_iter().collect()
    }
}

fn replace_deck(deck: &Deck) -> Option<Invoke<SlidesDeck>> {
    let action: SlidesAction = replace_deck_action(deck);
    Invoke::apply(&action).ok()
}

impl ResourceSurface<SlidesDeck> for SlidesApp {
    fn present(&mut self, state: &Rc<SlidesResourceCtx>) {
        self.adopt_resource_state(state);
    }

    fn stage(&mut self, scenario: Option<&str>) {
        self.stage_scenario(scenario);
    }

    fn bound_declarations(&self) -> Vec<ActionDecl<SlidesDeck>> {
        self.selection_declarations()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::actions::{
        ADD_ELEMENT, ADD_SLIDE, DELETE_ELEMENT, MOVE_ELEMENT, MOVE_SLIDE, RESTYLE_ELEMENT,
    };
    use crate::model::{Deck, PaletteKind, seed_deck};
    use ui::tilo_resource::Resource;

    use ui::framework::testing::TestApp;

    fn titles(app: &TestApp<SlidesShell>) -> (String, usize) {
        let state = app.state();
        (state.deck_title.clone(), state.slides.len())
    }

    #[test]
    fn every_scenario_stages_its_deck_and_view() {
        for &scenario in SlidesShell::scenarios() {
            let mut app = TestApp::<SlidesShell>::capture(Some(scenario))
                .unwrap_or_else(|error| panic!("`{scenario}` stages: {error}"));
            let (title, slides) = titles(&app);
            let expected = match scenario {
                "empty" => 1,
                "many" | "many_rail" => crate::sample::LONG_DECK_SLIDES,
                "image" | "image_loading" => crate::model::seed_deck().slides.len() + 1,
                _ => crate::model::seed_deck().slides.len(),
            };
            assert_eq!(slides, expected, "`{scenario}` opens its deck ({title})");
            let shown = app
                .harness()
                .with_widget_mut::<SlidesApp, _>(|slides| slides.current_slide())
                .expect("the editor is mounted");
            match scenario {
                "many" | "many_rail" | "image" | "image_loading" => {
                    assert_eq!(shown, slides - 1, "`{scenario}` rests on its last slide");
                }
                "empty" | "reorder" | "delete_zone" => assert_eq!(shown, 0, "`{scenario}`"),
                _ => assert_eq!(shown, 1, "`{scenario}` opens on the second slide"),
            }
        }
    }

    #[test]
    fn the_narrow_scenarios_are_captured_at_a_narrow_window() {
        assert_eq!(SlidesShell::shot_viewport(Some("narrow")), NARROW_VIEWPORT);
        assert_eq!(
            SlidesShell::shot_viewport(Some("overview")),
            ui::launch::DEFAULT_VIEWPORT
        );
    }

    #[test]
    fn the_image_scenario_shows_its_picture_decoded() {
        let mut app = TestApp::<SlidesShell>::capture(Some("image")).unwrap();
        let src = crate::sample::photo_src().unwrap();
        let ready = app
            .harness()
            .with_widget_mut::<SlidesApp, _>(|slides| slides.picture_ready(&src))
            .unwrap();
        assert!(ready, "the capture's content store holds the picture");
    }

    fn odp_of(deck: &Deck) -> Vec<u8> {
        crate::odp::write_odp(deck, &tilo_content::ContentStore::memory()).unwrap()
    }

    #[test]
    fn a_presentation_file_opens_as_its_own_deck() {
        let mut deck = seed_deck();
        deck.title = "Quarterly review".into();
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("review.odp");
        std::fs::write(&path, odp_of(&deck)).unwrap();
        let path = path.to_string_lossy().into_owned();

        let app = TestApp::<SlidesShell>::launched(&[&path]).unwrap();
        assert_eq!(titles(&app), ("Quarterly review".into(), deck.slides.len()));
        assert_eq!(
            app.record(),
            &RecordId::derived("slides", &RecordId::identify_file(&path))
        );
    }

    #[test]
    fn a_handed_file_replaces_the_new_deck_in_one_undo_step() {
        let mut deck = seed_deck();
        deck.title = "Handed over".into();
        let (file, _receipt) =
            ui::application::OpenFile::new("handed.odp", ODP_MEDIA_TYPE, odp_of(&deck)).unwrap();
        let mut app = TestApp::<SlidesShell>::handed(file).unwrap();
        assert_eq!(titles(&app), ("Handed over".into(), deck.slides.len()));
        assert!(app.undo(), "opening the file is an undo step");
        assert_eq!(titles(&app).1, 1, "undo returns to the starter deck");
    }

    #[test]
    fn a_replacement_deck_is_refused_when_it_names_a_slide_twice() {
        let mut deck = seed_deck();
        let twin = deck.slides[0].clone();
        deck.slides.push(twin);
        let mut doc = ui::tilo_resource::Doc::new();
        SlidesDeck::initialize(&mut doc).unwrap();
        let refused =
            tilo_actions::Action::apply(&crate::actions::replace_deck_action(&deck), &mut doc);
        assert!(refused.is_err());
        let empty = crate::model::Deck {
            slides: Vec::new(),
            ..seed_deck()
        };
        assert!(
            tilo_actions::Action::apply(&crate::actions::replace_deck_action(&empty), &mut doc)
                .is_err()
        );
    }

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
    fn the_deck_publishes_its_operations_and_durable_slides() {
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
    }
}
