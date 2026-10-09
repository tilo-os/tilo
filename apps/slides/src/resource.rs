//! The slides app on the framework: the [`Deck`] record, its editor, and
//! what opening a presentation file does.

use std::rc::Rc;

use ui::framework::{
    EffectAnswer, EffectCx, Fixture, LaunchInput, ResourceApp, ResourceSurface, ViewCx,
};

use crate::effects::SlidesEffect;
use ui::record::RecordId;
use ui::tilo_resource::{ActionDecl, Invoke};
use ui::widget::Element;

use crate::app::SlidesApp;
use crate::deck::Deck;
use crate::model::seed_deck;

/// The media type of an ODF presentation (`.odp`).
pub const ODP_MEDIA_TYPE: &str = "application/vnd.oasis.opendocument.presentation";
/// The media type of an Office Open XML presentation (`.pptx`).
pub const PPTX_MEDIA_TYPE: &str =
    "application/vnd.openxmlformats-officedocument.presentationml.presentation";

/// The window size of the scenarios that show the narrow layout, where the
/// rail and the inspector are flaps over the canvas.
const NARROW_VIEWPORT: (u32, u32) = (820, 800);

/// Framework app: the slides editor is the surface over the [`Deck`].
pub struct SlidesShell;

impl ResourceApp for SlidesShell {
    type Resource = Deck;
    const ID: &'static str = "so.tilo.slides";
    const TITLE: &'static str = "Slides";
    const ICON: Option<&'static str> = Some("slides");
    const OPENS: &'static [&'static str] = &[ODP_MEDIA_TYPE, PPTX_MEDIA_TYPE];
    const ARGUMENTS: usize = 1;

    fn view(cx: &mut ViewCx<Deck>) -> Element {
        // In a container, the editor is a widget of the view's tree that
        // tests and staging reach by its type.
        let editor = SlidesApp::new(cx);
        cx.on_open_item(editor.opener());
        Box::new(ui::widget::Stack::new().push(cx.surface(editor)))
    }

    fn catalog() -> Option<&'static ui::i18n::EmbeddedCatalog> {
        Some(&crate::strings::CATALOG)
    }

    fn label(state: &Deck) -> String {
        let title = state.title.trim();
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
    fn launch(input: &LaunchInput<'_>, created: bool) -> Vec<Invoke<Deck>> {
        let Some(file) = input.file().filter(|_| created) else {
            return Vec::new();
        };
        // The host's store carries the pictures to the deck's other readers.
        let content = input
            .content()
            .cloned()
            .unwrap_or_else(crate::assets::app_content);
        let deck = if file.media_type() == PPTX_MEDIA_TYPE {
            crate::pptx::read_pptx(file.bytes(), &content)
        } else {
            crate::odp::read_odp(file.bytes(), &content)
        };
        match deck {
            Ok(deck) => vec![replace_deck(&deck)],
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
    fn fixture(fixture: &mut Fixture<Deck>) {
        if fixture.scenario() == Some("image") {
            let _ = crate::assets::store_asset(fixture.content(), &crate::sample::photo_png());
        }
    }

    /// Every scenario but `empty` opens the sample deck; the long and
    /// picture scenarios a variant of it.
    fn scenario(name: Option<&str>) -> Vec<Invoke<Deck>> {
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
        vec![replace_deck(&deck)]
    }
}

fn replace_deck(deck: &crate::model::Deck) -> Invoke<Deck> {
    Deck::REPLACE_DECK.with(crate::deck::deck::ReplaceDeck {
        title: deck.title.clone(),
        palette: deck.palette,
        slides: deck.slides.iter().map(crate::deck::SlideData::of).collect(),
    })
}

impl ResourceSurface<Deck, SlidesEffect> for SlidesApp {
    fn present(&mut self, state: &Rc<Deck>) {
        self.adopt_resource_state(state);
    }

    fn stage(&mut self, scenario: Option<&str>) {
        self.stage_scenario(scenario);
    }

    fn effect(&mut self, effect: SlidesEffect, cx: &mut EffectCx<Deck, Self>) -> EffectAnswer {
        self.answer_effect(effect, cx)
    }

    fn bound_declarations(&self) -> Vec<ActionDecl<Deck>> {
        self.selection_declarations()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::seed_deck;
    use ui::tilo_resource::{Invoke, Replica};

    use ui::framework::testing::TestApp;

    fn titles(app: &TestApp<SlidesShell>) -> (String, usize) {
        let state = app.state();
        (state.title.clone(), state.slides.len())
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

    fn odp_of(deck: &crate::model::Deck) -> Vec<u8> {
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
        let mut replica = Replica::<Deck>::new();
        let refused =
            replica.apply(Invoke::apply(&crate::actions::replace_deck_action(&deck)).unwrap());
        assert!(refused.is_err());
        let empty = crate::model::Deck {
            slides: Vec::new(),
            ..seed_deck()
        };
        assert!(
            replica
                .apply(Invoke::apply(&crate::actions::replace_deck_action(&empty)).unwrap())
                .is_err()
        );
    }

    /// The editor's effects — showing, presenting, files, reading — are
    /// published on the deck beside its actions, and each slide offers to
    /// open itself.
    #[test]
    fn the_editors_effects_are_published_on_the_deck() {
        let mut app = TestApp::<SlidesShell>::new().unwrap();
        let surface = app.agent_surface();
        let rows = surface.as_array().unwrap();
        let on = |resource: &str, action: &str| {
            rows.iter()
                .any(|row| row["resource"] == resource && row["action"] == action)
        };
        for effect in [
            "editor",
            "overview",
            "present",
            "present-largest-external",
            "present-primary-external",
            "end-presentation",
            "open-presentation",
            "save-as",
            "read-deck",
        ] {
            assert!(
                on("slides.deck", effect),
                "`{effect}` is published: {surface}"
            );
        }
        let slide = app.state().slides.ids().next().unwrap().clone();
        let item = crate::actions::slide_resource_id(slide.as_str());
        for verb in [
            "duplicate-slide",
            "delete-slide",
            "move-slide",
            "set-text",
            "add-element",
        ] {
            assert!(
                on(&item, verb),
                "`{verb}` is offered on the slide itself: {surface}"
            );
        }
    }

    /// The deck publishes its verbs and edits, and its slides as items.
    #[test]
    fn the_deck_publishes_its_verbs_and_slides() {
        let mut app = TestApp::<SlidesShell>::new().unwrap();
        let surface = app.agent_surface();
        let rows = surface.as_array().unwrap();
        let on_deck = |action: &str| {
            rows.iter()
                .any(|row| row["resource"] == "slides.deck" && row["action"] == action)
        };
        for verb in [
            "add-slide",
            "insert-slide",
            "replace-deck",
            ui::tilo_resource::APPLY,
            ui::tilo_resource::GROUP,
        ] {
            assert!(on_deck(verb), "`{verb}` is published: {surface}");
        }
        let slides = app.state().slides.len();
        let items = rows
            .iter()
            .filter_map(|row| row["resource"].as_str())
            .filter(|resource| resource.starts_with("slides.deck/slides/"))
            .collect::<std::collections::BTreeSet<_>>();
        assert_eq!(items.len(), slides, "each slide is an item: {surface}");
    }
}
