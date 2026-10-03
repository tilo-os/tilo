//! Preference mutations and the nested preference children published by
//! [`crate::resource::Osk`]. The palette door is [`tilo_resource::publish`];
//! this module does not hand-build a second keyboard root.
use crate::framework_history::{EMOJI_RECENTS, LAYOUT, PERSONALIZATION, SUGGESTIONS};
use crate::resource::OskPreferences;
use automerge::{ROOT, ReadDoc, ScalarValue, transaction::Transactable};
use serde::{Deserialize, Serialize};
use ui::tilo_actions::{
    Action, ActionArgs, ActionBinding, ActionError, ActionParam, ActionRegistry, ActionSpec,
    ControlInvocation, Doc, EncodedAction, ExecutionLocus, OperationKind, ParamType, Resource,
    RetryPolicy,
};
pub const TYPE_ID: &str = "osk.preference";
pub const KEYBOARD_RESOURCE_ID: &str = "osk.keyboard";
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) enum Preference {
    Layout(String),
    Suggestions(bool),
    Personalization(bool),
    /// This user's recently picked emoji, as the JSON `emoji::Recents`.
    EmojiRecents(String),
}
impl Preference {
    pub fn index(&self) -> usize {
        match self {
            Self::Layout(_) => 0,
            Self::Suggestions(_) => 1,
            Self::Personalization(_) => 2,
            Self::EmojiRecents(_) => 3,
        }
    }
    fn key(&self) -> &'static str {
        match self {
            Self::Layout(_) => LAYOUT,
            Self::Suggestions(_) => SUGGESTIONS,
            Self::Personalization(_) => PERSONALIZATION,
            Self::EmojiRecents(_) => EMOJI_RECENTS,
        }
    }
    fn scalar(&self) -> ScalarValue {
        match self {
            Self::Layout(v) | Self::EmojiRecents(v) => v.as_str().into(),
            Self::Suggestions(v) | Self::Personalization(v) => (*v).into(),
        }
    }
    pub fn matches(&self, doc: &Doc) -> bool {
        doc.get(ROOT, self.key())
            .ok()
            .flatten()
            .is_some_and(|(v, _)| {
                v == automerge::Value::Scalar(std::borrow::Cow::Owned(self.scalar()))
            })
    }
    fn address(&self) -> (&'static str, &'static str) {
        match self {
            Self::Layout(_) => ("osk.keyboard.layout", "set"),
            Self::Suggestions(_) => ("osk.keyboard.suggestions", "set"),
            Self::Personalization(_) => ("osk.keyboard.suggestions", "set-personal"),
            Self::EmojiRecents(_) => ("osk.keyboard.emoji", "set-recents"),
        }
    }
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SetPreference {
    pub(crate) value: Preference,
    pub(crate) draft: Option<String>,
}
impl Action for SetPreference {
    fn apply(&self, doc: &mut Doc) -> Result<(), ActionError> {
        if !self.value.matches(doc) {
            doc.put(ROOT, self.value.key(), self.value.scalar())?;
        }
        Ok(())
    }
    fn locus(&self) -> ExecutionLocus {
        ExecutionLocus::ClientLocal
    }
}
pub fn register(registry: &mut ActionRegistry) {
    registry.register::<SetPreference>(TYPE_ID);
}
#[allow(
    clippy::expect_used,
    reason = "preference action contains only serializable scalar values"
)]
fn encode(value: Preference, draft: Option<String>) -> EncodedAction {
    EncodedAction::encode(TYPE_ID, &SetPreference { value, draft })
        .expect("scalar preferences serialize")
}
pub(crate) fn decode(encoded: &EncodedAction) -> Option<SetPreference> {
    if encoded.type_id != TYPE_ID {
        return None;
    }
    serde_json::from_slice(&encoded.payload).ok()
}
/// Nested preference resources for [`crate::resource::Osk::children`].
/// Addresses (`osk.keyboard.layout` / `suggestions` / `emoji`) stay stable for
/// drafts, history tests, and the palette.
#[allow(
    clippy::too_many_lines,
    reason = "one catalogue for the preference fields and their existing named affordances"
)]
pub(crate) fn preference_children(state: &OskPreferences) -> Vec<Resource> {
    let preferred = state.layout.as_deref().unwrap_or("qwerty");
    let layouts = if state.layout_choices.is_empty() {
        vec!["qwerty".into(), "azerty".into()]
    } else {
        state.layout_choices.clone()
    };
    let mut layout = Resource::new(
        "osk.keyboard.layout",
        "osk.layout",
        format!("Keyboard layout ({})", preferred.to_uppercase()),
    );
    layout.actions.push(ActionSpec::typed(
        "set",
        "Set keyboard layout",
        "Registered layout",
        ExecutionLocus::ClientLocal,
        vec![
            ActionParam::required("value", ParamType::Text, "Registered layout")
                .with_choices(layouts),
            ActionParam::optional("draft", ParamType::Text, "Local draft correlation"),
        ],
        |args: &ActionArgs| {
            encode(
                Preference::Layout(args.text("value").unwrap_or_default().into()),
                args.text("draft").map(str::to_owned),
            )
        },
    ));
    for (id, label) in [
        ("qwerty", "English (QWERTY)"),
        ("azerty", "French (AZERTY)"),
    ] {
        layout.actions.push(ActionSpec::immediate_encoded(
            id,
            label,
            ExecutionLocus::ClientLocal,
            encode(Preference::Layout(id.into()), None),
        ));
    }
    let mut suggestions = Resource::new(
        "osk.keyboard.suggestions",
        "osk.suggestions",
        "Suggestions and privacy",
    );
    for (id, label, personal) in [
        ("set", "Set suggestions", false),
        ("set-personal", "Set personalization", true),
    ] {
        suggestions.actions.push(ActionSpec::typed(
            id,
            label,
            "Enabled",
            ExecutionLocus::ClientLocal,
            vec![
                ActionParam::required("value", ParamType::Boolean, "Enabled"),
                ActionParam::optional("draft", ParamType::Text, "Local draft correlation"),
            ],
            move |args: &ActionArgs| {
                encode(
                    if personal {
                        Preference::Personalization(args.boolean("value").unwrap_or(true))
                    } else {
                        Preference::Suggestions(args.boolean("value").unwrap_or(true))
                    },
                    args.text("draft").map(str::to_owned),
                )
            },
        ));
    }
    for (id, label, value) in [
        (
            "enable",
            "Turn on suggestions",
            Preference::Suggestions(true),
        ),
        (
            "disable",
            "Turn off suggestions",
            Preference::Suggestions(false),
        ),
        (
            "enable-personal",
            "Turn on personalized suggestions",
            Preference::Personalization(true),
        ),
        (
            "disable-personal",
            "Turn off personalized suggestions",
            Preference::Personalization(false),
        ),
    ] {
        suggestions.actions.push(ActionSpec::immediate_encoded(
            id,
            label,
            ExecutionLocus::ClientLocal,
            encode(value, None),
        ));
    }
    if let Ok(spec) = ActionSpec::operation(
        "clear-personal",
        "Clear learned suggestions",
        OperationKind::Effect,
        "osk.suggestions",
        &"clear-personal",
    ) {
        suggestions
            .actions
            .push(spec.retry_policy(RetryPolicy::AtMostOnce));
    }
    let mut emoji = Resource::new("osk.keyboard.emoji", "osk.emoji", "Recent emoji");
    emoji.actions.push(ActionSpec::typed(
        "set-recents",
        "Set recent emoji",
        "Recent emoji",
        ExecutionLocus::ClientLocal,
        vec![
            ActionParam::required("value", ParamType::Text, "Recent emoji, as JSON"),
            ActionParam::optional("draft", ParamType::Text, "Local draft correlation"),
        ],
        |args: &ActionArgs| {
            encode(
                Preference::EmojiRecents(args.text("value").unwrap_or_default().into()),
                args.text("draft").map(str::to_owned),
            )
        },
    ));
    vec![layout, suggestions, emoji]
}

pub(crate) struct Draft {
    pub value: Preference,
    pub token: String,
}
pub(crate) struct PreferenceDrafts {
    lifetime: String,
    pub fields: [Option<Draft>; 4],
    offers: [Option<ControlInvocation>; 4],
    consumed: [bool; 4],
}
impl Default for PreferenceDrafts {
    fn default() -> Self {
        Self {
            lifetime: ui::tilo_actions::ActionId::new().0.to_string(),
            fields: std::array::from_fn(|_| None),
            offers: std::array::from_fn(|_| None),
            consumed: [false; 4],
        }
    }
}
impl PreferenceDrafts {
    pub fn stage(&mut self, value: Preference) {
        let index = value.index();
        if let (Some(field), Some(offer)) = (self.fields.get_mut(index), self.offers.get_mut(index))
        {
            *field = Some(Draft {
                value,
                token: format!("{}:{}", self.lifetime, ui::tilo_actions::ActionId::new().0),
            });
            *offer = None;
        }
    }
    pub fn pending(&self) -> bool {
        self.fields.iter().any(Option::is_some)
    }
    pub fn has(&self, index: usize) -> bool {
        self.fields.get(index).is_some_and(Option::is_some)
    }
    /// Only an ordinary model pass offers retries. Availability/resource queries
    /// and drain/acknowledgement paths never regenerate an in-flight offer.
    pub fn prepare_after_sync(&mut self, resources: &[Resource]) {
        self.consumed.fill(false);
        for (draft, offer) in self.fields.iter().zip(self.offers.iter_mut()) {
            let Some(draft) = draft else {
                continue;
            };
            if offer.is_some() {
                continue;
            }
            let (resource_id, action_id) = draft.value.address();
            let Some(resource) = resources
                .iter()
                .flat_map(|root| &root.children)
                .find(|r| r.id.as_str() == resource_id)
            else {
                continue;
            };
            let Some(spec) = resource.actions.iter().find(|a| a.id == action_id) else {
                continue;
            };
            let arguments = serde_json::Map::from_iter([
                (
                    "draft".into(),
                    serde_json::Value::String(draft.token.clone()),
                ),
                (
                    "value".into(),
                    match &draft.value {
                        Preference::Layout(v) | Preference::EmojiRecents(v) => v.clone().into(),
                        Preference::Suggestions(v) | Preference::Personalization(v) => (*v).into(),
                    },
                ),
            ]);
            if let Ok(binding) = ActionBinding::new(resource, spec, |arguments| arguments) {
                *offer = binding.invoke(arguments).ok();
            }
        }
    }
    pub fn take(&mut self) -> Option<ControlInvocation> {
        for (offer, consumed) in self.offers.iter_mut().zip(&mut self.consumed) {
            if !*consumed && let Some(offer) = offer.take() {
                *consumed = true;
                return Some(offer);
            }
        }
        None
    }
    /// Explicit local resource success supersedes an older same-field draft.
    /// Correlated successes settle only their exact root lifetime/generation.
    pub fn acknowledge(&mut self, action: &SetPreference, doc: &Doc) -> bool {
        if !action.value.matches(doc) {
            return false;
        }
        let index = action.value.index();
        let (Some(field), Some(offer)) = (self.fields.get_mut(index), self.offers.get_mut(index))
        else {
            return false;
        };
        if let Some(token) = &action.draft
            && !field
                .as_ref()
                .is_some_and(|draft| &draft.token == token && draft.value == action.value)
        {
            return false;
        }
        *field = None;
        *offer = None;
        true
    }
}
