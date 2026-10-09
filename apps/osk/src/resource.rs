//! The on-screen keyboard as a resource: the door its preference effects
//! and rows are published on, and the app that shows the keyboard.
//!
//! The keyboard edits no document of its own. This person's keyboard
//! preferences (letter layout, suggestions, personalization, recent emoji)
//! are their settings ([`OskPreferences`]), which follow them to their other
//! devices; what typing teaches it is their person record
//! ([`crate::learned::Learned`]). What is typed is never part of either: the
//! word being composed, the key under a finger and the field's purpose live
//! in the keyboard surface and are published nowhere, so the palette, agents
//! and scripts see the keyboard's configuration and never its keystrokes.

use serde::{Deserialize, Serialize};
use ui::application::{ComponentBehaviour, WindowRole};
use ui::framework::{ResourceApp, ViewCx};
use ui::i18n::EmbeddedCatalog;
use ui::tilo_resource::{self, Model, resource};
use ui::widget::Element;

use crate::actions::LETTER_LAYOUTS;
use crate::surface::OskSurface;

/// This person's keyboard preferences.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct OskPreferences {
    /// The letter layout text fields open in.
    pub layout: String,
    /// Configured letter layouts, in their user-selected switching order.
    /// An absent list preserves the historic bundled letter layouts.
    pub layouts: Vec<String>,
    /// Whether the suggestion bar offers words.
    pub suggestions: bool,
    /// Whether the keyboard adapts to what this person types.
    pub personalization: bool,
    /// Recently picked emoji, most recent first.
    pub emoji_recents: Vec<String>,
}

impl Default for OskPreferences {
    fn default() -> Self {
        Self {
            layout: LETTER_LAYOUTS[0].0.to_owned(),
            layouts: Vec::new(),
            suggestions: true,
            personalization: true,
            emoji_recents: Vec::new(),
        }
    }
}

impl ui::storage::SettingsSchema for OskPreferences {
    const SCOPE: ui::storage::Scope = ui::storage::Scope::Personal;
}

/// The on-screen keyboard resource: the door its effects and rows are
/// published on. It holds nothing; preferences are settings and what was
/// learned is the person's own record.
#[derive(Model, Default, Debug, Clone, PartialEq)]
pub struct Osk {}

#[resource(kind = "osk.keyboard", label = "On-screen keyboard")]
impl Osk {}

/// The value of `--purpose`: the field kind the keyboard starts for.
pub const PURPOSE_OPTION: &str = "--purpose";
/// Require the compositor's input method: fail rather than fall back to an
/// always-visible keyboard.
pub const INPUT_METHOD_FLAG: &str = "--input-method";
/// Never use the input method: an always-visible keyboard that types through
/// the virtual keyboard.
pub const NO_INPUT_METHOD_FLAG: &str = "--no-input-method";
/// Paint the effective tap targets over the keys, to see how the typing
/// context grows and shrinks them.
pub const HIT_OVERLAY_FLAG: &str = "--debug-hit-areas";

/// The on-screen keyboard app: the keyboard surface over [`Osk`].
pub struct OskShell;

impl ResourceApp for OskShell {
    type Resource = Osk;
    const ID: &'static str = crate::KEYBOARD_APP_ID;
    const TITLE: &'static str = "On-screen keyboard";
    const ICON: Option<&'static str> = Some("osk");
    const BEHAVIOUR: ComponentBehaviour = ComponentBehaviour {
        window: WindowRole::InputSurface,
        ..ComponentBehaviour::DEFAULT
    };
    const OPTIONS: &'static [&'static str] = &[PURPOSE_OPTION];
    const FLAGS: &'static [&'static str] =
        &[INPUT_METHOD_FLAG, NO_INPUT_METHOD_FLAG, HIT_OVERLAY_FLAG];

    fn view(cx: &mut ViewCx<Osk>) -> Element {
        cx.surface(OskSurface::new())
    }

    fn catalog() -> Option<&'static EmbeddedCatalog> {
        Some(&crate::i18n::CATALOG)
    }

    fn scenarios() -> &'static [&'static str] {
        crate::surface::SCENARIOS
    }

    /// Phone-portrait width with room for the toolbar, the suggestion bar
    /// and four key rows (48 + 40 + 4 × 56 = 312, rounded up).
    fn shot_viewport(scenario: Option<&str>) -> (u32, u32) {
        match scenario {
            Some("emoji") => (720, 420),
            _ => (720, 360),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::actions::OskEffect;
    use ui::framework::testing::TestApp;
    use ui::tilo_resource::Effect;

    /// The person's keyboard preferences in the test profile's settings.
    fn prefs(app: &TestApp<OskShell>) -> OskPreferences {
        app.settings::<OskPreferences>().get()
    }

    fn keyboard() -> TestApp<OskShell> {
        let mut app = TestApp::<OskShell>::new().unwrap();
        app.harness().relayout();
        app
    }

    fn tap(app: &mut TestApp<OskShell>, name: &str) {
        let harness = app.harness();
        let node = harness
            .a11y_node_named(name)
            .unwrap_or_else(|| panic!("no key named {name:?}"));
        harness
            .perform_a11y_action(node, &ui::a11y::Action::Activate)
            .unwrap();
        harness.relayout();
    }

    #[test]
    fn service_keyboard_uses_the_assigned_surface_in_both_orientations() {
        use ui::widget::Widget;
        for theme in [
            ui::Theme::light(),
            ui::Theme::dark(),
            ui::Theme::high_contrast(),
        ] {
            let mut app = keyboard();
            app.harness().set_theme(theme);
            for viewport in [(800, 600), (600, 800)] {
                app.harness().resize(viewport).unwrap();
                let rect = app
                    .with_surface::<OskSurface, _>(|surface| {
                        let keyboard = surface.keyboard();
                        assert!(keyboard.key_geometry().len() > 30);
                        for (_, key) in keyboard.key_geometry() {
                            assert!(key.width() > 0.0 && key.height() > 0.0);
                            assert!(keyboard.get_rect().contains(key.center()));
                        }
                        keyboard.get_rect()
                    })
                    .expect("the real owner keyboard surface is mounted");
                assert!(
                    rect.height() >= f64::from(viewport.1) * 0.75,
                    "keyboard must fill its assigned content area instead of scaling a short keyboard inside a tall buffer: {viewport:?} {rect:?}"
                );
                let pixels = app.harness().render_rgba();
                assert_eq!(
                    pixels.len(),
                    usize::try_from(viewport.0 * viewport.1 * 4).unwrap()
                );
                let colours: std::collections::HashSet<_> = pixels
                    .chunks_exact(4)
                    .map(|pixel| [pixel[0], pixel[1], pixel[2], pixel[3]])
                    .collect();
                assert!(
                    colours.len() > 3,
                    "the actual owner keyboard frame must be rendered"
                );
            }
        }
    }

    #[test]
    fn the_published_keyboard_offers_its_configuration_and_never_what_is_typed() {
        let mut app = keyboard();
        for letter in ["s", "e", "c", "r", "e", "t", "Space", "m", "a"] {
            tap(&mut app, letter);
        }
        app.settle();
        assert!(
            !learned(&mut app).is_empty(),
            "the committed word was learned, privately"
        );
        let published = app.agent_surface().to_string();
        for address in [
            "osk.keyboard.layout",
            "osk.keyboard.suggestions",
            "osk.keyboard.emoji",
            "clear-personal",
            "set-layout",
            "English (QWERTY)",
            "French (AZERTY)",
            "Clear learned suggestions",
        ] {
            assert!(published.contains(address), "{address} is published");
        }
        assert!(
            !published.contains("secret") && !published.contains("Composing"),
            "neither the word being typed nor what was learned reaches a palette, agent or script: {published}"
        );
        // The person's record of what was learned offers taking a change
        // back, never what it holds.
        let surface = app.agent_surface();
        let learned_actions: Vec<&str> = surface
            .as_array()
            .into_iter()
            .flatten()
            .filter(|action| action["resource"] == "person:osk.learned")
            .filter_map(|action| action["action"].as_str())
            .collect();
        assert!(
            learned_actions
                .iter()
                .all(|action| matches!(*action, "undo" | "redo")),
            "{learned_actions:?}"
        );
    }

    #[test]
    fn preferences_change_through_the_door_and_follow_the_person_to_the_next_launch() {
        let mut app = keyboard();
        app.call_on("osk.keyboard.layout", "azerty", serde_json::json!({}))
            .unwrap();
        app.call("set-suggestions", serde_json::json!({ "value": false }))
            .unwrap();
        assert_eq!(prefs(&app).layout, "azerty");
        assert!(!prefs(&app).suggestions);
        assert!(
            app.call_on(
                "osk.keyboard.layout",
                "set",
                serde_json::json!({ "value": "dvorak" })
            )
            .is_err(),
            "only a layout the keyboard carries"
        );

        let app = app.reopen().unwrap();
        assert_eq!(prefs(&app).layout, "azerty");
        assert!(!prefs(&app).suggestions);
        assert!(prefs(&app).personalization);
    }

    #[test]
    fn a_picked_emoji_is_saved_and_turning_personalization_off_forgets_it() {
        let mut app = keyboard();
        tap(&mut app, &crate::i18n::t("osk-emoji-key"));
        let first = crate::OskWidget::new()
            .emoji_panel()
            .sections(&emoji::Recents::default())[0]
            .emoji[0]
            .clone();
        app.harness()
            .perform_semantic_named(
                "on_screen_keyboard",
                Some(&format!("osk:emoji:{first}")),
                &ui::a11y::Action::Activate,
            )
            .unwrap();
        app.settle();
        assert_eq!(prefs(&app).emoji_recents, [first]);

        app.call("set-personalization", serde_json::json!({ "value": false }))
            .unwrap();
        assert!(prefs(&app).emoji_recents.is_empty());
        assert!(!prefs(&app).personalization);
    }

    #[test]
    fn clearing_learned_suggestions_forgets_the_recent_emoji_and_keeps_the_preference() {
        let mut app = keyboard();
        app.call(
            "set-emoji-recents",
            serde_json::json!({ "value": ["👍", "🎉"] }),
        )
        .unwrap();
        assert_eq!(prefs(&app).emoji_recents, ["👍", "🎉"]);
        app.effect_on_confirmed("osk.keyboard.suggestions", &OskEffect::ClearPersonal)
            .unwrap();
        app.settle();
        assert!(prefs(&app).emoji_recents.is_empty());
        assert!(prefs(&app).personalization);
        assert_eq!(OskEffect::ClearPersonal.id(), "clear-personal");
    }

    fn learned(app: &mut TestApp<OskShell>) -> crate::learned::Learned {
        let record = app
            .person::<crate::learned::Learned>()
            .expect("the person's record of what the keyboard learned");
        (*record.get()).clone()
    }

    #[test]
    fn what_typing_teaches_is_the_persons_own_and_survives_a_relaunch() {
        let mut app = keyboard();
        for key in ["h", "i", "Space", "y", "o", "u", "Space"] {
            tap(&mut app, key);
        }
        app.settle();
        let taught = learned(&mut app);
        assert_eq!(taught.count("en", "", "hi"), 1);
        assert_eq!(taught.count("en", "hi", "you"), 1);

        let mut app = app.reopen().unwrap();
        app.harness().relayout();
        assert_eq!(learned(&mut app).count("en", "hi", "you"), 1);

        app.call("set-personalization", serde_json::json!({ "value": false }))
            .unwrap();
        app.settle();
        assert!(
            learned(&mut app).is_empty(),
            "turning personalization off forgets what was learned"
        );
    }

    #[test]
    fn clearing_learned_suggestions_forgets_the_saved_lessons() {
        let mut app = keyboard();
        for key in ["h", "i", "Space"] {
            tap(&mut app, key);
        }
        app.settle();
        assert!(!learned(&mut app).is_empty());
        app.effect_on_confirmed("osk.keyboard.suggestions", &OskEffect::ClearPersonal)
            .unwrap();
        app.settle();
        assert!(learned(&mut app).is_empty());
        assert!(prefs(&app).personalization);
    }

    #[test]
    fn the_learned_records_history_reads_as_sentences() {
        let mut app = keyboard();
        for key in ["h", "i", "Space"] {
            tap(&mut app, key);
        }
        app.settle();
        app.effect_on_confirmed("osk.keyboard.suggestions", &OskEffect::ClearPersonal)
            .unwrap();
        app.settle();
        let record = app
            .person::<crate::learned::Learned>()
            .expect("the person's record of what the keyboard learned");
        let document = record.record().document().expect("the record's document");
        let mut replica = ui::tilo_resource::Replica::<crate::learned::Learned>::load(&document)
            .expect("the record reads as what the keyboard learned");
        let english = include_str!("../locales/en-US.ftl");
        let catalogue = tilo_history::Catalogue::new("en-US", &[english]);
        let reader = tilo_history::Reader::new("en-US").with_app(&catalogue);
        let sentences: Vec<String> = replica
            .versions()
            .into_iter()
            .filter_map(|version| version.context)
            .map(|entry| reader.sentence(&entry))
            .collect();
        assert_eq!(
            sentences,
            ["Learned from typing", "Forgot what the keyboard learned"]
        );
    }

    #[test]
    fn the_keyboard_service_runs_without_a_workspace_runtime() {
        assert!(
            !ui::framework::launch_spec::<OskShell>()
                .attaches_runtime(&ui::launch::LaunchArgs::component()),
            "the compositor starts the keyboard without a room; attaching would refuse it"
        );
    }

    #[test]
    fn launch_options_pick_the_first_layout() {
        let mut app = TestApp::<OskShell>::launched(&["--purpose", "phone"]).unwrap();
        app.harness().relayout();
        assert!(
            app.harness().a11y_node_named("q").is_none(),
            "a phone field opens the dialpad"
        );
        assert!(app.harness().a11y_node_named("5").is_some());
    }

    #[test]
    fn every_scenario_captures() {
        for &scenario in OskShell::scenarios() {
            let mut app = TestApp::<OskShell>::capture(Some(scenario)).unwrap();
            app.harness().relayout();
        }
        let french = TestApp::<OskShell>::capture(Some("french")).unwrap();
        assert_eq!(prefs(&french).layout, "azerty");
    }
}
