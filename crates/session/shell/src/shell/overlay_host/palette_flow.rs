//! What the search palette does with its results beyond listing them: the
//! steps that collect an action's arguments and the other ways a row can be
//! opened.
//!
//! Pure functions over search results, so the host's event code stays a
//! thin dispatcher and every decision here is tested without a desktop.

use serde_json::{Map, Value};
use ui::widget::command_palette::{ActivationModifier, ArgumentKind, ArgumentSpec};

use crate::shell::i18n::t;
use crate::shell::search::action_api::{ActionParam, ParamType};
use crate::shell::search::{ResourceKind, SearchResult};

/// Row id of the row that shows the rest of a group the budget cut short.
pub(super) const MORE_PREFIX: &str = "more:";

/// Row id of the launcher's "All apps" row: every app, as the Apps chip
/// lists them.
pub(super) const ALL_APPS_ID: &str = "all-apps";

/// Row id of one value offered in an argument step.
pub(super) const CHOICE_PREFIX: &str = "choice:";

/// How one argument is asked for.
#[derive(Clone, Debug, PartialEq)]
pub(super) enum Step {
    /// Rows to pick from.
    Choices(Vec<Choice>),
    Field(ArgumentSpec),
}

/// One value offered in an argument step: what it binds, how it reads, a
/// second line that tells it apart, and for a person (an identity), the key
/// of their avatar.
#[derive(Clone, Debug, PartialEq)]
pub(super) struct Choice {
    pub value: Value,
    pub label: String,
    pub detail: Option<String>,
    pub person: Option<String>,
}

impl Choice {
    fn plain(value: Value, label: String) -> Self {
        Self {
            value,
            label,
            detail: None,
            person: None,
        }
    }
}

/// The step that asks for `param` of the action titled `title`. What the
/// value picks from the workspace (who, where, which grant) is listed as
/// `picks` finds it there, and a set of declared values as each set; declared
/// values are rows to pick from; a switch is On or Off; a number steps with
/// the arrows inside its schema's bounds; a secret is masked. Nothing is
/// typed in its encoding.
pub(super) fn step_for(
    param: &ActionParam,
    title: &str,
    picks: &dyn Fn(tilo_actions::Pick) -> Vec<Choice>,
) -> Step {
    if let Some(pick) = param.pick() {
        return Step::Choices(picks(pick));
    }
    if let Some(sets) = sets_of(param) {
        return Step::Choices(sets);
    }
    if !param.choices.is_empty() || param.has_labelled_choices() {
        // A choice reads as its label: an identity by its name, never by its
        // fingerprint.
        return Step::Choices(
            param
                .choices
                .iter()
                .filter_map(|choice| {
                    Some(Choice {
                        value: value_of(param.ty, choice).ok()?,
                        label: param.choice_label(choice).unwrap_or(choice).to_string(),
                        detail: None,
                        person: param.choice_is_person(choice).then(|| choice.to_string()),
                    })
                })
                .collect(),
        );
    }
    if param.ty == ParamType::Boolean {
        return Step::Choices(vec![
            Choice::plain(Value::Bool(true), t("palette-value-on")),
            Choice::plain(Value::Bool(false), t("palette-value-off")),
        ]);
    }
    let bound = |key: &str| {
        param
            .schema
            .as_deref()
            .and_then(|schema| schema.get(key))
            .and_then(Value::as_f64)
    };
    let kind = match param.ty {
        ParamType::Integer => ArgumentKind::Number {
            min: bound("minimum"),
            max: bound("maximum"),
            step: bound("multipleOf").unwrap_or(1.0),
        },
        ParamType::Number => {
            let (min, max) = (bound("minimum"), bound("maximum"));
            // A unit interval (a volume, an opacity) steps in twentieths.
            let unit = max.is_some_and(|max| max <= 1.0);
            ArgumentKind::Number {
                min,
                max,
                step: bound("multipleOf").unwrap_or(if unit { 0.05 } else { 1.0 }),
            }
        }
        ParamType::Secret => ArgumentKind::Secret,
        _ => ArgumentKind::Text,
    };
    let prefill = param
        .schema
        .as_deref()
        .and_then(|schema| schema.get("default"))
        .and_then(|value| match value {
            Value::String(text) => Some(text.clone()),
            Value::Number(number) => Some(number.to_string()),
            _ => None,
        })
        .filter(|_| param.ty != ParamType::Secret);
    Step::Field(ArgumentSpec {
        prompt: title.to_string(),
        placeholder: param.description.to_string(),
        prefill,
        kind,
    })
}

/// The values a list of declared values (`items` with an `enum`) can take,
/// each set in the declared order and read by its values' labels: every
/// non-empty set of a few values, else each value alone and all of them.
fn sets_of(param: &ActionParam) -> Option<Vec<Choice>> {
    const MAX_SET_VALUES: usize = 4;
    if param.ty != ParamType::Array {
        return None;
    }
    let items = param.schema.as_deref()?.get("items")?;
    let values: Vec<&str> = items
        .get("enum")?
        .as_array()?
        .iter()
        .filter_map(Value::as_str)
        .collect();
    let label = |value: &str| {
        items
            .get(tilo_actions::CHOICE_LABELS_ANNOTATION)
            .and_then(|labels| labels.get(value))
            .and_then(Value::as_str)
            .unwrap_or(value)
            .to_owned()
    };
    let sets: Vec<Vec<&str>> = if values.len() <= MAX_SET_VALUES {
        (1..1_usize << values.len())
            .map(|mask| {
                values
                    .iter()
                    .enumerate()
                    .filter(|(index, _)| mask & (1 << index) != 0)
                    .map(|(_, value)| *value)
                    .collect()
            })
            .collect()
    } else {
        values
            .iter()
            .map(|value| vec![*value])
            .chain(std::iter::once(values.clone()))
            .collect()
    };
    Some(
        sets.into_iter()
            .map(|set| {
                Choice::plain(
                    Value::Array(set.iter().map(|value| Value::from(*value)).collect()),
                    set.iter()
                        .map(|value| label(value))
                        .collect::<Vec<_>>()
                        .join(", "),
                )
            })
            .collect(),
    )
}

/// `text` as the JSON value a `ty` parameter binds from. Numbers may carry
/// a trailing percent sign, as people write volumes and scales.
pub(super) fn value_of(ty: ParamType, text: &str) -> Result<Value, String> {
    let trimmed = text.trim();
    let number = trimmed.trim_end_matches('%').trim();
    match ty {
        ParamType::Integer => number
            .parse::<i64>()
            .map(Value::from)
            .map_err(|_| t("palette-value-not-whole-number")),
        ParamType::Number => number
            .replace(',', ".")
            .parse::<f64>()
            .ok()
            .and_then(serde_json::Number::from_f64)
            .map(Value::Number)
            .ok_or_else(|| t("palette-value-not-number")),
        ParamType::Boolean => match fold(trimmed).as_str() {
            "true" | "on" | "yes" => Ok(Value::Bool(true)),
            "false" | "off" | "no" => Ok(Value::Bool(false)),
            other if other == fold(&t("palette-value-on")) => Ok(Value::Bool(true)),
            other if other == fold(&t("palette-value-off")) => Ok(Value::Bool(false)),
            _ => Err(t("palette-value-not-on-off")),
        },
        ParamType::Object | ParamType::Array => {
            serde_json::from_str(trimmed).map_err(|_| t("palette-value-not-understood"))
        }
        ParamType::Text | ParamType::Resource | ParamType::DateTime | ParamType::Secret => {
            Ok(Value::String(text.to_string()))
        }
    }
}

fn fold(text: &str) -> String {
    tilo_search::fold(text)
}

/// An argument read off the query itself ("volume 40", "layout de").
#[derive(Clone, Debug, PartialEq)]
pub(super) struct InlineArgument {
    pub name: String,
    pub value: Value,
    /// The value is a declared choice or a number, so the action may run
    /// at once. Free text is only offered in its field, since the words
    /// after a title are as often more of a search as they are a value.
    pub certain: bool,
}

/// The words of `query` that are not words of `title`, read as the value
/// of the first required parameter, when they make one. Never a secret:
/// the query is plain text on the screen.
pub(super) fn inline_argument(
    query: &str,
    title: &str,
    params: &[ActionParam],
) -> Option<InlineArgument> {
    let title_words: Vec<String> = fold(title)
        .split(|c: char| !c.is_alphanumeric())
        .filter(|word| !word.is_empty())
        .map(str::to_string)
        .collect();
    let rest: Vec<&str> = query
        .split_whitespace()
        .filter(|word| {
            let word = fold(word);
            !title_words.iter().any(|title| title.starts_with(&word))
        })
        .collect();
    if rest.is_empty() {
        return None;
    }
    let text = rest.join(" ");
    let param = params
        .iter()
        .find(|param| param.required && param.ty != ParamType::Secret)?;
    let name = param.name.to_string();
    if !param.choices.is_empty() {
        let wanted = fold(&text);
        // A labelled choice is named by its label ("Alice"), as it reads.
        let reads = |c: &str| fold(param.choice_label(c).unwrap_or(c));
        let exact = param.choices.iter().find(|c| reads(c) == wanted);
        let mut prefixed = param
            .choices
            .iter()
            .filter(|c| reads(c).starts_with(&wanted));
        let choice = exact.or_else(|| match (prefixed.next(), prefixed.next()) {
            (Some(only), None) => Some(only),
            _ => None,
        })?;
        return Some(InlineArgument {
            name,
            value: value_of(param.ty, choice).ok()?,
            certain: true,
        });
    }
    match param.ty {
        ParamType::Integer | ParamType::Number | ParamType::Boolean => Some(InlineArgument {
            name,
            value: value_of(param.ty, &text).ok()?,
            certain: true,
        }),
        ParamType::Text => Some(InlineArgument {
            name,
            value: Value::String(text),
            certain: false,
        }),
        _ => None,
    }
}

/// The parameters still to ask for, in declared order: the required ones
/// `values` lacks. Optional parameters keep their defaults; an agent or a
/// script names them.
pub(super) fn missing_params(params: &[ActionParam], values: &Map<String, Value>) -> Vec<usize> {
    params
        .iter()
        .enumerate()
        .filter(|(_, param)| param.required && !values.contains_key(param.name.as_ref()))
        .map(|(i, _)| i)
        .collect()
}

/// The action being given its arguments, one step at a time.
pub(super) struct ParamFlow {
    pub result: SearchResult,
    pub values: Map<String, Value>,
    /// Parameters still to ask, by index into `result.params`; the first
    /// is the one on screen.
    pub pending: Vec<usize>,
    /// Parameters answered in this flow, in order, for stepping back.
    pub asked: Vec<usize>,
    /// The values the step on screen offers, by row.
    pub choices: Vec<Value>,
    /// What was typed before the first step, given back on backing out.
    pub query: String,
    /// Free text read off the query for a parameter (its name and the
    /// text), offered in that parameter's field.
    pub prefill: Option<(String, String)>,
}

impl ParamFlow {
    pub fn param(&self) -> Option<&ActionParam> {
        self.pending
            .first()
            .and_then(|&index| self.result.params.get(index))
    }
}

/// Whether rows of `kind` open a window, so where it lands is a choice.
pub(super) fn opens_a_window(kind: &ResourceKind) -> bool {
    matches!(
        kind,
        ResourceKind::Application | ResourceKind::File | ResourceKind::Note | ResourceKind::Record
    )
}

/// What a held modifier does to a row that opens a window.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Activation {
    TabHere,
    /// In its own group right beside the focused window.
    Beside,
    /// The row's other actions, as rows to pick from.
    More,
}

impl Activation {
    pub fn of(modifier: ActivationModifier) -> Self {
        match modifier {
            ActivationModifier::Shift => Self::TabHere,
            ActivationModifier::Ctrl => Self::Beside,
            ActivationModifier::Alt => Self::More,
        }
    }

    pub fn label(self) -> String {
        match self {
            Self::TabHere => t("palette-open-tab-here"),
            Self::Beside => t("palette-open-beside"),
            Self::More => t("palette-more-actions"),
        }
    }
}

/// The other things to do with a file: open it either way, open the folder
/// it is in, copy its path.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum SecondaryAction {
    TabHere,
    Beside,
    ContainingFolder,
    CopyPath,
}

impl SecondaryAction {
    pub const FOR_FILES: [Self; 4] = [
        Self::TabHere,
        Self::Beside,
        Self::ContainingFolder,
        Self::CopyPath,
    ];

    pub fn label(self) -> String {
        match self {
            Self::TabHere => t("palette-open-tab-here"),
            Self::Beside => t("palette-open-beside"),
            Self::ContainingFolder => t("palette-open-containing-folder"),
            Self::CopyPath => t("palette-copy-path"),
        }
    }

    pub fn slug(self) -> &'static str {
        match self {
            Self::TabHere => "tab-here",
            Self::Beside => "beside",
            Self::ContainingFolder => "folder",
            Self::CopyPath => "copy-path",
        }
    }

    pub fn from_slug(slug: &str) -> Option<Self> {
        Self::FOR_FILES
            .into_iter()
            .find(|action| action.slug() == slug)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn no_picks(_: tilo_actions::Pick) -> Vec<Choice> {
        Vec::new()
    }

    fn rows_of(params: &[ActionParam]) -> Vec<String> {
        let id = params.iter().find(|param| param.name == "id").unwrap();
        match step_for(id, "Use identity in this workspace", &no_picks) {
            Step::Choices(rows) => rows.into_iter().map(|row| row.label).collect(),
            Step::Field(_) => Vec::new(),
        }
    }

    #[test]
    fn choosing_an_identity_offers_names_and_avatars_never_a_fingerprint() {
        use crate::shell::search::providers::actions::identity_select_params;
        let (alice, bob) = ("a1".repeat(32), "b2".repeat(32));
        let params =
            identity_select_params(&[(alice.clone(), "Alice".into()), (bob.clone(), "Bob".into())]);
        let id = params.iter().find(|param| param.name == "id").unwrap();
        let Step::Choices(rows) = step_for(id, "Use identity in this workspace", &no_picks) else {
            panic!("the identity is picked from a list, never typed");
        };
        assert_eq!(
            rows.iter()
                .map(|row| row.label.as_str())
                .collect::<Vec<_>>(),
            ["Alice", "Bob"],
            "each reads as its name"
        );
        assert_eq!(
            rows.iter()
                .map(|row| row.person.clone())
                .collect::<Vec<_>>(),
            [Some(alice.clone()), Some(bob)],
            "each wears its avatar"
        );
        assert_eq!(rows[0].value, Value::String(alice), "and binds its id");
        assert!(
            !id.description.contains("fingerprint"),
            "nobody is asked for a fingerprint"
        );

        // The empty picker's next step is an action
        // "Set up your identity" that starts the first-identity setup, offered
        // only when the device has no identity.
        let none = identity_select_params(&[]);
        let id = none.iter().find(|param| param.name == "id").unwrap();
        let Step::Choices(rows) = step_for(id, "Use identity in this workspace", &no_picks) else {
            panic!("with none connected it is still a list, never a field for a fingerprint");
        };
        assert_eq!(
            rows.iter()
                .map(|row| row.label.as_str())
                .collect::<Vec<_>>(),
            ["Set up your identity"],
            "the next step, not a spelling hint"
        );
        assert_eq!(rows[0].person, None);
        assert!(
            none.iter().all(|param| param.name != "confirm"),
            "setting one up delegates nothing, so nothing is confirmed"
        );
        assert!(
            rows_of(&params)
                .iter()
                .all(|label| label != "Set up your identity"),
            "a device with an identity is not offered another"
        );
    }

    #[test]
    fn who_where_and_which_grant_are_picked_from_the_workspace_never_typed() {
        let subject = ActionParam::required("subject", ParamType::Object, "Who")
            .picking(tilo_actions::Pick::Subject);
        let ada = Choice {
            value: serde_json::json!({ "person": "ada" }),
            detail: None,
            label: "Ada".into(),
            person: Some("ada".into()),
        };
        let picks = |pick| {
            assert_eq!(pick, tilo_actions::Pick::Subject);
            vec![ada.clone()]
        };
        let Step::Choices(rows) = step_for(&subject, "Grant", &picks) else {
            panic!("a subject is picked from the workspace's people");
        };
        assert_eq!(rows, [ada]);

        let kinds = ActionParam::required("kinds", ParamType::Array, "Which kinds").with_schema(
            serde_json::json!({
                "type": "array",
                "items": {
                    "enum": ["read", "undoable", "outside"],
                    "choice_labels": { "read": "reading" },
                },
            }),
        );
        let Step::Choices(rows) = step_for(&kinds, "Grant", &no_picks) else {
            panic!("a set of declared values is picked as a set");
        };
        assert_eq!(rows.len(), 7, "every non-empty set");
        assert_eq!(rows[0].value, serde_json::json!(["read"]));
        assert_eq!(rows[0].label, "reading");
        assert_eq!(
            rows[6].value,
            serde_json::json!(["read", "undoable", "outside"])
        );
        assert_eq!(rows[6].label, "reading, undoable, outside");
    }

    #[test]
    fn each_parameter_type_gets_its_own_step() {
        let choices = ActionParam::required("minutes", ParamType::Integer, "Minutes")
            .with_choices(["0", "5", "10"]);
        let Step::Choices(rows) = step_for(&choices, "Lock after", &no_picks) else {
            panic!("declared values are rows");
        };
        assert_eq!(rows[1], Choice::plain(Value::from(5), "5".to_string()));

        let switch = ActionParam::required("enabled", ParamType::Boolean, "On or off");
        assert!(
            matches!(step_for(&switch, "Wi-Fi", &no_picks), Step::Choices(rows) if rows.len() == 2)
        );

        let percent = ActionParam::required("percent", ParamType::Integer, "Percent")
            .with_schema(serde_json::json!({"minimum": 0, "maximum": 100, "default": 40}));
        let Step::Field(spec) = step_for(&percent, "Volume", &no_picks) else {
            panic!("a number is a field");
        };
        assert_eq!(spec.prompt, "Volume");
        assert_eq!(spec.prefill.as_deref(), Some("40"));
        assert_eq!(
            spec.kind,
            ArgumentKind::Number {
                min: Some(0.0),
                max: Some(100.0),
                step: 1.0
            }
        );

        let secret = ActionParam::required("secret", ParamType::Secret, "Password");
        let Step::Field(spec) = step_for(&secret, "Unlock", &no_picks) else {
            panic!("a secret is a field");
        };
        assert_eq!(spec.kind, ArgumentKind::Secret);
    }

    #[test]
    fn values_parse_by_type() {
        assert_eq!(value_of(ParamType::Integer, "125%"), Ok(Value::from(125)));
        assert_eq!(value_of(ParamType::Number, "0,5"), Ok(Value::from(0.5)));
        assert_eq!(value_of(ParamType::Boolean, "On"), Ok(Value::Bool(true)));
        assert!(value_of(ParamType::Integer, "loud").is_err());
        assert_eq!(
            value_of(ParamType::Text, "de"),
            Ok(Value::String("de".into()))
        );
    }

    #[test]
    fn the_words_after_a_title_fill_its_first_parameter() {
        let scale = [
            ActionParam::required("percent", ParamType::Integer, "Scale")
                .with_choices(["100", "125", "150"]),
        ];
        assert_eq!(
            inline_argument("scale 125", "Display scale", &scale),
            Some(InlineArgument {
                name: "percent".into(),
                value: Value::from(125),
                certain: true,
            })
        );
        assert_eq!(
            inline_argument("display scale", "Display scale", &scale),
            None,
            "a query that is all title words names no value"
        );
        assert_eq!(
            inline_argument("scale 130", "Display scale", &scale),
            None,
            "a value that isn't offered is not guessed"
        );
        let layout = [ActionParam::required("layout", ParamType::Text, "Layout")];
        assert_eq!(
            inline_argument("layout de", "Keyboard layout", &layout),
            Some(InlineArgument {
                name: "layout".into(),
                value: Value::String("de".into()),
                certain: false,
            }),
            "free text is offered, not run"
        );
        let secret = [ActionParam::required(
            "secret",
            ParamType::Secret,
            "Password",
        )];
        assert_eq!(
            inline_argument("unlock hunter2", "Unlock", &secret),
            None,
            "a secret is never read off the visible query"
        );
    }

    #[test]
    fn only_required_parameters_are_asked_for() {
        let params = [
            ActionParam::required("percent", ParamType::Integer, "Scale"),
            ActionParam::optional("display", ParamType::Text, "Which display"),
        ];
        assert_eq!(missing_params(&params, &Map::new()), vec![0]);
        let mut values = Map::new();
        values.insert("percent".into(), Value::from(125));
        assert!(missing_params(&params, &values).is_empty());
    }
}

#[cfg(test)]
mod icon_tests {
    use crate::shell::search::providers::actions::action_icon;
    use ui::icons::Icon;

    #[test]
    fn actions_wear_the_picture_of_what_they_act_on() {
        assert_eq!(action_icon("appearance.theme.dark"), Some(Icon::Moon));
        assert_eq!(action_icon("appearance.other"), Some(Icon::Palette));
        assert_eq!(action_icon("tab.next"), Some(Icon::AppWindow));
        assert_eq!(action_icon("quick-settings.wifi.toggle"), Some(Icon::Wifi));
        assert_eq!(
            action_icon("tabular.thing"),
            None,
            "families are whole segments"
        );
    }
}
