//! The keyboard's preferences as one replayable action, and the preference
//! rows the [`crate::resource::Osk`] resource publishes beside its own
//! declarations.
use automerge::{ObjType, ROOT, ReadDoc, ScalarValue, transaction::Transactable};
use serde::{Deserialize, Serialize};
use ui::tilo_actions::{
    Action, ActionError, ActionParam, ChangeKind, Doc, ExecutionLocus, ParamType, Resource,
};
use ui::tilo_resource::{self, ActionDecl, Effect, declaration_spec};

use crate::resource::{Osk, OskPreferences};

pub const TYPE_ID: &str = "osk.preference";
pub const KEYBOARD_RESOURCE_ID: &str = "osk.keyboard";

pub(crate) const LAYOUT: &str = "osk.preferred_layout";
pub(crate) const SUGGESTIONS: &str = "osk.suggestions_enabled";
pub(crate) const PERSONALIZATION: &str = "osk.personalization_enabled";
/// Recently picked emoji, most recent first.
pub(crate) const EMOJI_RECENTS: &str = "osk.emoji_recents";

/// The letter layouts every keyboard carries; a preference names one of them.
pub const LETTER_LAYOUTS: [(&str, &str); 2] = [
    ("qwerty", "English (QWERTY)"),
    ("azerty", "French (AZERTY)"),
];

/// One keyboard preference set to a value.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SetPreference {
    /// The letter layout text fields open in.
    Layout(String),
    /// Whether the suggestion bar offers words.
    Suggestions(bool),
    /// Whether the keyboard adapts to what this person types. Turning it off
    /// also forgets the recent emoji.
    Personalization(bool),
    /// This person's recently picked emoji, most recent first.
    EmojiRecents(Vec<String>),
}

impl Action for SetPreference {
    fn apply(&self, doc: &mut Doc) -> Result<(), ActionError> {
        match self {
            Self::Layout(layout) => {
                if !LETTER_LAYOUTS.iter().any(|(name, _)| name == layout) {
                    return Err(ActionError::Refused(format!(
                        "no letter layout named {layout:?}"
                    )));
                }
                put_scalar(doc, LAYOUT, layout.as_str().into())
            }
            Self::Suggestions(enabled) => put_scalar(doc, SUGGESTIONS, (*enabled).into()),
            Self::Personalization(enabled) => {
                put_scalar(doc, PERSONALIZATION, (*enabled).into())?;
                if !enabled {
                    put_recents(doc, &[])?;
                }
                Ok(())
            }
            Self::EmojiRecents(recents) => put_recents(doc, recents),
        }
    }

    fn locus(&self) -> ExecutionLocus {
        ExecutionLocus::ClientLocal
    }

    /// Recent emoji are learned from use, not set: they follow the person
    /// but no undo takes a pick back.
    fn change_kind(&self) -> ChangeKind {
        match self {
            Self::EmojiRecents(_) => ChangeKind::External,
            _ => ChangeKind::Content,
        }
    }
}

fn put_scalar(doc: &mut Doc, key: &str, value: ScalarValue) -> Result<(), ActionError> {
    let current = doc.get(ROOT, key)?;
    if !matches!(current, Some((automerge::Value::Scalar(ref held), _)) if **held == value) {
        doc.put(ROOT, key, value)?;
    }
    Ok(())
}

fn put_recents(doc: &mut Doc, recents: &[String]) -> Result<(), ActionError> {
    if read_recents(doc) == recents {
        return Ok(());
    }
    let list = doc.put_object(ROOT, EMOJI_RECENTS, ObjType::List)?;
    for (index, emoji) in recents.iter().enumerate() {
        doc.insert(&list, index, emoji.as_str())?;
    }
    Ok(())
}

/// The recent emoji the document holds, most recent first.
pub(crate) fn read_recents(doc: &Doc) -> Vec<String> {
    let Ok(Some((automerge::Value::Object(ObjType::List), list))) = doc.get(ROOT, EMOJI_RECENTS)
    else {
        return Vec::new();
    };
    (0..doc.length(&list))
        .filter_map(|index| match doc.get(&list, index).ok().flatten() {
            Some((automerge::Value::Scalar(scalar), _)) => match scalar.as_ref() {
                ScalarValue::Str(text) => Some(text.to_string()),
                _ => None,
            },
            _ => None,
        })
        .collect()
}

/// What the keyboard does that changes no preference.
#[derive(Clone, Debug, PartialEq, Eq, Effect)]
#[effect(operation = "osk.suggestions")]
pub enum OskEffect {
    /// Forget what the keyboard learned from typing.
    ///
    /// Learned word rankings, tap positions, kept words and recent emoji;
    /// the personalization preference stays as it is.
    #[effect(row, destructive, label = "Clear learned suggestions")]
    ClearPersonal,
}

fn set(
    id: &'static str,
    label: &'static str,
    value: SetPreference,
) -> ui::tilo_actions::ActionSpec {
    declaration_spec(&ActionDecl::<Osk>::new(
        id,
        label,
        Vec::new(),
        move |_, _| Ok(value.clone()),
    ))
}

fn set_enabled(
    id: &'static str,
    label: &'static str,
    value: fn(bool) -> SetPreference,
) -> ui::tilo_actions::ActionSpec {
    declaration_spec(&ActionDecl::<Osk>::new(
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
    ))
}

/// The preference rows of [`crate::resource::Osk`]: the layout, the
/// suggestions and the recent emoji, each with the edits it offers.
pub(crate) fn preference_children(state: &OskPreferences) -> Vec<Resource> {
    let mut layout = Resource::new(
        "osk.keyboard.layout",
        "osk.layout",
        format!("Keyboard layout ({})", state.layout.to_uppercase()),
    )
    .with_action(declaration_spec(&layout_declaration("set")));
    for (id, label) in LETTER_LAYOUTS {
        layout = layout.with_action(set(id, label, SetPreference::Layout(id.into())));
    }
    let suggestions = Resource::new(
        "osk.keyboard.suggestions",
        "osk.suggestions",
        "Suggestions and privacy",
    )
    .with_action(set_enabled(
        "set",
        "Set suggestions",
        SetPreference::Suggestions,
    ))
    .with_action(set_enabled(
        "set-personal",
        "Set personalization",
        SetPreference::Personalization,
    ))
    .with_action(set(
        "enable",
        "Turn on suggestions",
        SetPreference::Suggestions(true),
    ))
    .with_action(set(
        "disable",
        "Turn off suggestions",
        SetPreference::Suggestions(false),
    ))
    .with_action(set(
        "enable-personal",
        "Turn on personalized suggestions",
        SetPreference::Personalization(true),
    ))
    .with_action(set(
        "disable-personal",
        "Turn off personalized suggestions",
        SetPreference::Personalization(false),
    ))
    .with_action(OskEffect::ClearPersonal.bound());
    let emoji = Resource::new("osk.keyboard.emoji", "osk.emoji", "Recent emoji")
        .with_action(declaration_spec(&recents_declaration("set-recents")));
    vec![layout, suggestions, emoji]
}

/// Set the letter layout, offered as `id`.
pub(crate) fn layout_declaration(id: &'static str) -> ActionDecl<Osk> {
    ActionDecl::new(
        id,
        "Set keyboard layout",
        vec![
            ActionParam::required("value", ParamType::Text, "Letter layout")
                .with_choices(LETTER_LAYOUTS.map(|(name, _)| name)),
        ],
        |args, _| {
            args.text("value")
                .map(|layout| SetPreference::Layout(layout.to_owned()))
                .ok_or_else(|| "missing layout".to_owned())
        },
    )
    .describe("Set the letter layout text fields open in.")
}

/// Replace the recent emoji, offered as `id`.
pub(crate) fn recents_declaration(id: &'static str) -> ActionDecl<Osk> {
    ActionDecl::new(
        id,
        "Set recent emoji",
        vec![ActionParam::required(
            "value",
            ParamType::Array,
            "Recent emoji, most recent first",
        )],
        |args, _| {
            let recents = args
                .array("value")
                .ok_or_else(|| "missing recent emoji".to_owned())?
                .iter()
                .map(|emoji| {
                    emoji
                        .as_str()
                        .map(str::to_owned)
                        .ok_or_else(|| "every recent emoji is text".to_owned())
                })
                .collect::<Result<Vec<_>, _>>()?;
            Ok(SetPreference::EmojiRecents(recents))
        },
    )
    .describe("Replace this person's recent emoji list.")
}
