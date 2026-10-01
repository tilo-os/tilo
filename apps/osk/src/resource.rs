//! On-screen keyboard as a [`tilo_resource::Resource`]: preference keys in the
//! shared document, with [`crate::actions::SetPreference`] as the replayable
//! action.
//!
//! Layout / suggestions / personalization / emoji-recents ride
//! [`Resource::children`] (historical palette addresses). Clear-learned is an
//! Effect on the suggestions child. Hosted [`PublishesResources`] keeps only
//! live extras (compose / key highlight) — never a second keyboard root beside
//! [`tilo_resource::publish`].

use automerge::{ROOT, ReadDoc, ScalarValue, Value};
use ui::automerge::AutoCommit;
use ui::framework::{Dispatcher, ResourceApp, ResourceSurface, SurfaceCx, ViewCx};
use ui::tilo_actions::{
    ActionParam, ActionRegistry, EncodedAction, ParamType, Resource as PublishedResource,
};
use ui::tilo_resource::{ActionDecl, Doc, ModelError, Resource};
use ui::widget::Element;

use crate::actions::{
    KEYBOARD_RESOURCE_ID, Preference, SetPreference, TYPE_ID, preference_children, register,
};
use crate::framework_history::{LAYOUT, PERSONALIZATION, SUGGESTIONS};
use crate::widget::OskWidget;

/// Preference projection from the keyboard's Automerge document.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct OskPreferences {
    pub layout: Option<String>,
    pub suggestions: Option<bool>,
    pub personalization: Option<bool>,
    /// Local layout catalogue for `set` choices; not collaborative document
    /// state. Empty → built-in qwerty / azerty.
    pub(crate) layout_choices: Vec<String>,
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
        register(registry);
    }

    fn read(doc: &Doc) -> Result<OskPreferences, ModelError> {
        Ok(OskPreferences {
            layout: read_str(doc, LAYOUT),
            suggestions: read_bool(doc, SUGGESTIONS),
            personalization: read_bool(doc, PERSONALIZATION),
            layout_choices: Vec::new(),
        })
    }

    fn declarations() -> Vec<ActionDecl<Self>> {
        // Named document mutations also advertised on the root. Nested
        // children keep the historical addresses drafts and the palette use.
        vec![
            ActionDecl::new(
                "set-layout",
                "Set keyboard layout",
                vec![
                    ActionParam::required("value", ParamType::Text, "Registered layout")
                        .with_choices(["qwerty", "azerty"]),
                    ActionParam::optional("draft", ParamType::Text, "Local draft correlation"),
                ],
                |args, _| {
                    Ok(SetPreference {
                        value: Preference::Layout(args.text("value").unwrap_or_default().into()),
                        draft: args.text("draft").map(str::to_owned),
                    })
                },
            )
            .describe("Set the shared letter layout preference."),
            ActionDecl::new(
                "set-suggestions",
                "Set suggestions",
                vec![
                    ActionParam::required("value", ParamType::Boolean, "Enabled"),
                    ActionParam::optional("draft", ParamType::Text, "Local draft correlation"),
                ],
                |args, _| {
                    Ok(SetPreference {
                        value: Preference::Suggestions(args.boolean("value").unwrap_or(true)),
                        draft: args.text("draft").map(str::to_owned),
                    })
                },
            )
            .describe("Enable or disable the suggestion bar."),
            ActionDecl::new(
                "set-personalization",
                "Set personalization",
                vec![
                    ActionParam::required("value", ParamType::Boolean, "Enabled"),
                    ActionParam::optional("draft", ParamType::Text, "Local draft correlation"),
                ],
                |args, _| {
                    Ok(SetPreference {
                        value: Preference::Personalization(args.boolean("value").unwrap_or(true)),
                        draft: args.text("draft").map(str::to_owned),
                    })
                },
            )
            .describe("Enable or disable local adaptive ranking."),
            ActionDecl::new(
                "set-emoji-recents",
                "Set recent emoji",
                vec![
                    ActionParam::required("value", ParamType::Text, "Recent emoji, as JSON"),
                    ActionParam::optional("draft", ParamType::Text, "Local draft correlation"),
                ],
                |args, _| {
                    Ok(SetPreference {
                        value: Preference::EmojiRecents(
                            args.text("value").unwrap_or_default().into(),
                        ),
                        draft: args.text("draft").map(str::to_owned),
                    })
                },
            )
            .describe("Replace this user's recent emoji list."),
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

/// Hosted-only extras that are not document state: in-flight compose and the
/// current key highlight. The keyboard door stays on [`tilo_resource::publish`].
pub(crate) fn live_extras(widget: &OskWidget) -> Vec<PublishedResource> {
    let mut extras = Vec::new();
    let word = widget.current_word();
    if !word.is_empty() {
        extras.push(PublishedResource::new(
            "osk.compose",
            "osk.compose",
            format!("Composing “{word}”"),
        ));
    }
    if let Some(label) = widget.highlighted_key_label() {
        extras.push(PublishedResource::new(
            "osk.key-highlight",
            "osk.key-highlight",
            format!("Key {label}"),
        ));
    }
    extras
}

/// Framework app: the keyboard widget is the surface over [`Osk`].
pub struct OskShell;

impl ResourceApp for OskShell {
    type Resource = Osk;
    const ID: &'static str = "so.tilo.osk";
    const TITLE: &'static str = "On-screen keyboard";
    const ICON: Option<&'static str> = Some("osk");
    const SHOT_VIEWPORT: (u32, u32) = (720, 360);

    fn view(cx: &mut ViewCx<Osk>) -> Element {
        cx.surface(OskWidget::new())
    }
}

impl ResourceSurface<Osk> for OskWidget {
    fn present(&mut self, state: &std::rc::Rc<OskPreferences>) {
        // Adopt collaborative prefs only when set — never clear a local layout
        // that has not been published yet.
        self.adopt_resource_state(state);
    }

    fn attach(&mut self, cx: SurfaceCx<Osk>) {
        self.attach_framework(cx.dispatcher());
    }
}

/// Framework dispatcher when hosted; the local action host for standalone
/// tests and the screenshot launcher.
pub(crate) struct ActionSink {
    pub(crate) local: ui::ActionHost,
    pub(crate) framework: Option<Dispatcher<Osk>>,
    /// Set by [`Self::attach`]; tests can flip it without a real dispatcher.
    pub(crate) hosted: bool,
    pub(crate) last: Option<EncodedAction>,
}

impl Default for ActionSink {
    fn default() -> Self {
        Self {
            local: ui::ActionHost::new(),
            framework: None,
            hosted: false,
            last: None,
        }
    }
}

impl ActionSink {
    pub(crate) fn attach(&mut self, dispatcher: Dispatcher<Osk>) {
        self.framework = Some(dispatcher);
        self.hosted = true;
    }

    pub(crate) fn is_hosted(&self) -> bool {
        self.hosted
    }

    pub(crate) fn dispatch(&mut self, doc: &mut AutoCommit, action: &SetPreference) {
        let context = ui::tilo_actions::ActionContext::for_action(
            "so.tilo.osk",
            "On-screen keyboard",
            action,
        );
        match &self.framework {
            Some(dispatcher) => {
                self.last = EncodedAction::encode(Osk::ACTION_TYPE, action).ok();
                let _ = dispatcher.apply_now_with(doc, action, context);
            }
            None => self
                .local
                .dispatch_with_context(doc, Osk::ACTION_TYPE, action, context),
        }
    }

    pub(crate) fn last_dispatched(&self) -> Option<&EncodedAction> {
        if self.framework.is_some() {
            self.last.as_ref()
        } else {
            self.local.last_dispatched()
        }
    }

    /// Test-only: mark the sink hosted without constructing a framework dispatcher.
    #[cfg(test)]
    pub(crate) fn mark_hosted_for_test(&mut self) {
        self.hosted = true;
    }
}

/// Standalone / `--shots` / unit-test tree via [`tilo_resource::publish`].
pub fn provide_resources(state: &OskPreferences) -> Vec<PublishedResource> {
    vec![ui::tilo_resource::publish::<Osk>(
        KEYBOARD_RESOURCE_ID,
        "On-screen keyboard",
        state,
    )]
}

#[cfg(test)]
mod tests {
    use super::*;
    use ui::tilo_actions::Action;
    use ui::widget::Widget;

    #[test]
    fn resource_publish_owns_keyboard_door_and_hosted_keeps_only_live_extras() {
        let mut doc = AutoCommit::new();
        SetPreference {
            value: Preference::Layout("azerty".into()),
            draft: None,
        }
        .apply(&mut doc)
        .unwrap();
        SetPreference {
            value: Preference::Suggestions(false),
            draft: None,
        }
        .apply(&mut doc)
        .unwrap();
        SetPreference {
            value: Preference::Personalization(false),
            draft: None,
        }
        .apply(&mut doc)
        .unwrap();
        doc.commit();

        let state = Osk::read(&doc).expect("seeded keyboard prefs");
        let published =
            ui::tilo_resource::publish::<Osk>(KEYBOARD_RESOURCE_ID, "On-screen keyboard", &state);
        assert_eq!(published.kind, "osk.keyboard");
        for id in [
            "set-layout",
            "set-suggestions",
            "set-personalization",
            "set-emoji-recents",
        ] {
            assert!(
                published.actions.iter().any(|action| action.id == id),
                "{id} must be on the framework resource"
            );
        }
        assert_eq!(published.children.len(), 3);
        assert!(
            published
                .children
                .iter()
                .any(|child| child.id.as_str() == "osk.keyboard.layout"
                    && child.actions.iter().any(|action| action.id == "qwerty")),
            "layout child keeps palette shortcuts"
        );
        assert!(
            published.children.iter().any(|child| {
                child.id.as_str() == "osk.keyboard.suggestions"
                    && child
                        .actions
                        .iter()
                        .any(|action| action.id == "clear-personal")
            }),
            "clear-personal Effect stays on the suggestions child"
        );
        assert_eq!(state.layout.as_deref(), Some("azerty"));
        assert_eq!(state.suggestions, Some(false));
        assert_eq!(state.personalization, Some(false));

        let mut app = OskWidget::new();
        let mut seeded = doc.clone();
        app.sync(&mut seeded, &ui::automerge::ROOT);
        assert!(!app.is_hosted(), "standalone stays on the local host");
        let standalone = app.publish_resources(&seeded);
        assert_eq!(standalone.len(), 1);
        assert_eq!(standalone[0].kind, "osk.keyboard");
        assert_eq!(standalone[0].children.len(), 3);

        // Hosted extras while composing / highlighting a key.
        app.activate_label("h");
        app.activate_label("i");
        assert_eq!(app.current_word(), "hi");
        app.mark_hosted_for_test();
        assert!(app.is_hosted());
        assert!(ui::resources::resource(&app, &seeded, KEYBOARD_RESOURCE_ID).is_none());
        assert!(
            ui::resources::resource(&app, &seeded, "osk.keyboard.layout")
                .is_some_and(|layout| layout.actions.iter().any(|action| action.id == "qwerty")),
            "a hosted layout stays the published row"
        );
        let extras = ui::resources::PublishesResources::resources(&app, &seeded);
        assert!(
            extras
                .iter()
                .all(|resource| resource.kind != "osk.keyboard"),
            "hosted must not republish osk.keyboard beside ResourceRoot: {:?}",
            extras.iter().map(|r| r.id.as_str()).collect::<Vec<_>>()
        );
        assert!(
            extras.iter().any(|resource| resource.kind == "osk.compose"),
            "hosted still publishes in-flight compose: {:?}",
            extras.iter().map(|r| r.kind.as_str()).collect::<Vec<_>>()
        );

        let door = provide_resources(&state);
        assert_eq!(door[0].kind, "osk.keyboard");
        assert_eq!(door[0].children.len(), 3);

        let mut local = AutoCommit::new();
        let mut standalone = OskWidget::new();
        standalone.commit_preference(
            &mut local,
            &SetPreference {
                value: Preference::Layout("azerty".into()),
                draft: None,
            },
        );
        assert!(
            standalone.last_document_action().is_some(),
            "ActionSink must record document actions on the local host"
        );
        assert_eq!(read_str(&local, LAYOUT).as_deref(), Some("azerty"));

        // Empty present must not wipe a local layout that has not been published.
        let before = standalone.preferred_text_layout().to_owned();
        assert_eq!(before, "azerty");
        standalone.adopt_resource_state(&OskPreferences::default());
        assert_eq!(
            standalone.preferred_text_layout(),
            before,
            "adopting empty view prefs must not clear the local layout"
        );
    }
}
