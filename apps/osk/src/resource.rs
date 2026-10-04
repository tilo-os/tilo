//! The on-screen keyboard as a resource: this person's keyboard preferences
//! (letter layout, suggestions, personalization, recent emoji), changed only
//! by [`SetPreference`], and the app that shows them as the keyboard.
//!
//! What is typed is never part of it: the word being composed, the key under
//! a finger and the field's purpose live in the keyboard surface and are
//! published nowhere, so the palette, agents and scripts see the keyboard's
//! configuration and never its keystrokes.

use automerge::{ROOT, ReadDoc, ScalarValue, Value};
use ui::application::{ComponentBehaviour, WindowRole};
use ui::framework::{ResourceApp, ViewCx};
use ui::tilo_actions::{ActionParam, ActionRegistry, ParamType, Resource as PublishedResource};
use ui::tilo_resource::{ActionDecl, Doc, Invoke, ModelError, Resource};
use ui::widget::Element;

use crate::actions::{
    LAYOUT, LETTER_LAYOUTS, PERSONALIZATION, SUGGESTIONS, SetPreference, TYPE_ID,
    layout_declaration, preference_children, read_recents, recents_declaration,
};
use crate::surface::OskSurface;

/// The keyboard preferences a document holds.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OskPreferences {
    /// The letter layout text fields open in.
    pub layout: String,
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
            suggestions: true,
            personalization: true,
            emoji_recents: Vec::new(),
        }
    }
}

/// The on-screen keyboard resource.
pub struct Osk;

impl Resource for Osk {
    const KIND: &'static str = "osk.keyboard";
    const LABEL: &'static str = "On-screen keyboard";
    const ACTION_TYPE: &'static str = TYPE_ID;

    type State = OskPreferences;
    type Action = SetPreference;

    fn register(registry: &mut ActionRegistry) {
        registry.register::<SetPreference>(TYPE_ID);
    }

    fn read(doc: &Doc) -> Result<OskPreferences, ModelError> {
        let defaults = OskPreferences::default();
        Ok(OskPreferences {
            layout: read_str(doc, LAYOUT)
                .filter(|layout| LETTER_LAYOUTS.iter().any(|(name, _)| name == layout))
                .unwrap_or(defaults.layout),
            suggestions: read_bool(doc, SUGGESTIONS).unwrap_or(defaults.suggestions),
            personalization: read_bool(doc, PERSONALIZATION).unwrap_or(defaults.personalization),
            emoji_recents: read_recents(doc),
        })
    }

    fn declarations() -> Vec<ActionDecl<Self>> {
        let enabled = |id, label, description, value: fn(bool) -> SetPreference| {
            ActionDecl::new(
                id,
                label,
                vec![ActionParam::required(
                    "value",
                    ParamType::Boolean,
                    "Enabled",
                )],
                move |args, _| {
                    args.boolean("value")
                        .map(value)
                        .ok_or_else(|| "missing value".to_owned())
                },
            )
            .describe(description)
        };
        vec![
            layout_declaration("set-layout"),
            enabled(
                "set-suggestions",
                "Set suggestions",
                "Enable or disable the suggestion bar.",
                SetPreference::Suggestions,
            ),
            enabled(
                "set-personalization",
                "Set personalization",
                "Enable or disable adapting to what this person types.",
                SetPreference::Personalization,
            ),
            recents_declaration("set-emoji-recents"),
        ]
    }

    fn children(state: &OskPreferences) -> Vec<PublishedResource> {
        preference_children(state)
    }
}

fn read_str(doc: &Doc, key: &str) -> Option<String> {
    match doc.get(ROOT, key).ok().flatten() {
        Some((Value::Scalar(scalar), _)) => match scalar.as_ref() {
            ScalarValue::Str(text) => Some(text.to_string()),
            _ => None,
        },
        _ => None,
    }
}

fn read_bool(doc: &Doc, key: &str) -> Option<bool> {
    match doc.get(ROOT, key).ok().flatten() {
        Some((Value::Scalar(scalar), _)) => match scalar.as_ref() {
            ScalarValue::Boolean(flag) => Some(*flag),
            _ => None,
        },
        _ => None,
    }
}

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

    fn scenario(name: Option<&str>) -> Vec<Invoke<Osk>> {
        let staged = match name {
            Some("french") => SetPreference::Layout("azerty".into()),
            Some("emoji") => {
                SetPreference::EmojiRecents(["👍", "😂", "❤️", "🎉"].map(str::to_owned).to_vec())
            }
            _ => return Vec::new(),
        };
        Invoke::apply(&staged).into_iter().collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::actions::OskEffect;
    use ui::framework::testing::TestApp;
    use ui::tilo_resource::Effect;

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
    fn the_published_keyboard_offers_its_configuration_and_never_what_is_typed() {
        let mut app = keyboard();
        for letter in ["s", "e", "c", "r", "e", "t", "space", "m", "a"] {
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
        ] {
            assert!(published.contains(address), "{address} is published");
        }
        assert!(
            !published.contains("secret")
                && !published.contains("Composing")
                && !published.contains("osk.learned"),
            "neither the word being typed nor what was learned reaches a palette, agent or script: {published}"
        );
    }

    #[test]
    fn preferences_change_through_the_door_and_follow_the_person_to_the_next_launch() {
        let mut app = keyboard();
        app.call_on("osk.keyboard.layout", "azerty", serde_json::json!({}))
            .unwrap();
        app.call("set-suggestions", serde_json::json!({ "value": false }))
            .unwrap();
        assert_eq!(app.state().layout, "azerty");
        assert!(!app.state().suggestions);
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
        assert_eq!(app.state().layout, "azerty");
        assert!(!app.state().suggestions);
        assert!(app.state().personalization);
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
        assert_eq!(app.state().emoji_recents, [first]);

        app.call("set-personalization", serde_json::json!({ "value": false }))
            .unwrap();
        assert!(app.state().emoji_recents.is_empty());
        assert!(!app.state().personalization);
    }

    #[test]
    fn clearing_learned_suggestions_forgets_the_recent_emoji_and_keeps_the_preference() {
        let mut app = keyboard();
        app.call(
            "set-emoji-recents",
            serde_json::json!({ "value": ["👍", "🎉"] }),
        )
        .unwrap();
        assert_eq!(app.state().emoji_recents, ["👍", "🎉"]);
        app.effect_on_confirmed("osk.keyboard.suggestions", &OskEffect::ClearPersonal)
            .unwrap();
        app.settle();
        assert!(app.state().emoji_recents.is_empty());
        assert!(app.state().personalization);
        assert_eq!(OskEffect::ClearPersonal.id(), "clear-personal");
    }

    fn learned(app: &mut TestApp<OskShell>) -> crate::learned::LearnedTyping {
        let record = app
            .person::<crate::learned::Learned>()
            .expect("the person's record of what the keyboard learned");
        (*record.get()).clone()
    }

    #[test]
    fn what_typing_teaches_is_the_persons_own_and_survives_a_relaunch() {
        let mut app = keyboard();
        for key in ["h", "i", "space", "y", "o", "u", "space"] {
            tap(&mut app, key);
        }
        app.settle();
        let taught = learned(&mut app);
        assert_eq!(taught.transitions["en"][""]["hi"], 1);
        assert_eq!(taught.transitions["en"]["hi"]["you"], 1);

        let mut app = app.reopen().unwrap();
        app.harness().relayout();
        assert_eq!(learned(&mut app).transitions["en"]["hi"]["you"], 1);

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
        for key in ["h", "i", "space"] {
            tap(&mut app, key);
        }
        app.settle();
        assert!(!learned(&mut app).is_empty());
        app.effect_on_confirmed("osk.keyboard.suggestions", &OskEffect::ClearPersonal)
            .unwrap();
        app.settle();
        assert!(learned(&mut app).is_empty());
        assert!(app.state().personalization);
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
        assert_eq!(french.state().layout, "azerty");
    }

    // The keyboard publishes its preferences as client-local
    // `osk.preference` document actions plus the clear-learned effect;
    // keystrokes never become actions.
    ui::action_contract_tests! {
        seed: ui::automerge::AutoCommit::new(),
        registry: {
            let mut registry = ui::tilo_actions::ActionRegistry::new();
            Osk::register(&mut registry);
            registry
        },
        resources: |doc| vec![ui::tilo_resource::publish::<Osk>(
            crate::actions::KEYBOARD_RESOURCE_ID,
            Osk::LABEL,
            &Osk::read(doc).unwrap(),
        )],
        palette: ["English (QWERTY)", "French (AZERTY)", "Clear learned suggestions"],
    }
}
