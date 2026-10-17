//! What the search palette does with its results beyond listing them: how
//! many rows each group shows, the steps that collect an action's arguments,
//! and the other ways a row can be opened.
//!
//! Pure functions over search results, so the host's event code stays a
//! thin dispatcher and every decision here is tested without a desktop.

use serde_json::{Map, Value};
use ui::widget::command_palette::{ActivationModifier, ArgumentKind, ArgumentSpec};

use crate::shell::i18n::t;
use crate::shell::search::action_api::{ActionParam, ParamType};
use crate::shell::search::{KindGroup, ResourceKind, SearchResult};

/// Row id of the row that shows the rest of a group the budget cut short.
pub(super) const MORE_PREFIX: &str = "more:";

/// Row id of one value offered in an argument step.
pub(super) const CHOICE_PREFIX: &str = "choice:";

/// Keep each group to a fair share of `budget` rows, front to back, except
/// the kinds in `expanded`, which show everything. A group that lost rows
/// keeps its `total`, so its header and its "show all" row can say how many
/// there are. A lone group, or a zero budget, keeps everything.
pub(super) fn trim_groups(
    mut groups: Vec<KindGroup>,
    budget: usize,
    expanded: &[ResourceKind],
) -> Vec<KindGroup> {
    let rows: usize = groups.iter().map(|g| g.results.len()).sum();
    if budget == 0 || groups.len() < 2 || rows <= budget {
        return groups;
    }
    let n = groups.len();
    let mut remaining = budget;
    for (i, group) in groups.iter_mut().enumerate() {
        if expanded.contains(&group.kind) {
            remaining = remaining.saturating_sub(group.results.len());
            continue;
        }
        let groups_left = n.saturating_sub(i).max(1);
        let share = remaining.checked_div(groups_left).unwrap_or(1).max(1);
        let keep = share.min(group.results.len());
        group.results.truncate(keep);
        remaining = remaining.saturating_sub(keep);
    }
    groups
}

/// Whether a group shows fewer rows than it holds.
pub(super) fn is_capped(group: &KindGroup) -> bool {
    group.results.len() < group.total
}

/// How one argument is asked for.
#[derive(Clone, Debug, PartialEq)]
pub(super) enum Step {
    /// Rows to pick from: each value and how it reads.
    Choices(Vec<(Value, String)>),
    /// A field of a kind.
    Field(ArgumentSpec),
}

/// The step that asks for `param` of the action titled `title`. Declared
/// values are rows to pick from; a switch is On or Off; a number steps with
/// the arrows inside its schema's bounds; a secret is masked.
pub(super) fn step_for(param: &ActionParam, title: &str) -> Step {
    if !param.choices.is_empty() {
        return Step::Choices(
            param
                .choices
                .iter()
                .filter_map(|choice| Some((value_of(param.ty, choice).ok()?, choice.to_string())))
                .collect(),
        );
    }
    if param.ty == ParamType::Boolean {
        return Step::Choices(vec![
            (Value::Bool(true), t("palette-value-on")),
            (Value::Bool(false), t("palette-value-off")),
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
        let exact = param.choices.iter().find(|c| fold(c) == wanted);
        let mut prefixed = param
            .choices
            .iter()
            .filter(|c| fold(c).starts_with(&wanted));
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
        ResourceKind::Application
            | ResourceKind::HostApplication
            | ResourceKind::File
            | ResourceKind::Note
            | ResourceKind::Record
    )
}

/// What a held modifier does to a row that opens a window.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Activation {
    /// As a tab in the focused window's group.
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

    fn rows(kind: &ResourceKind, n: usize) -> KindGroup {
        KindGroup {
            kind: kind.clone(),
            total: n,
            results: (0..n)
                .map(|i| {
                    SearchResult::nullary(
                        kind.clone(),
                        "test",
                        format!("{i}"),
                        format!("Row {i}"),
                        None,
                        0,
                        |_, _, _| {},
                    )
                })
                .collect(),
        }
    }

    #[test]
    fn groups_share_the_budget_and_an_expanded_group_keeps_every_row() {
        let groups = || {
            vec![
                rows(&ResourceKind::Action, 66),
                rows(&ResourceKind::Application, 3),
                rows(&ResourceKind::File, 19),
            ]
        };
        let trimmed = trim_groups(groups(), 12, &[]);
        let shown: Vec<usize> = trimmed.iter().map(|g| g.results.len()).collect();
        assert_eq!(shown, vec![4, 3, 5], "a fair share, what's left rolls on");
        assert!(trimmed.iter().all(|g| g.total >= g.results.len()));
        assert!(is_capped(&trimmed[0]) && !is_capped(&trimmed[1]));

        let expanded = trim_groups(groups(), 12, &[ResourceKind::File]);
        let file = expanded
            .iter()
            .find(|g| g.kind == ResourceKind::File)
            .expect("files");
        assert_eq!(file.results.len(), 19, "show all means all");
    }

    #[test]
    fn each_parameter_type_gets_its_own_step() {
        let choices = ActionParam::required("minutes", ParamType::Integer, "Minutes")
            .with_choices(["0", "5", "10"]);
        let Step::Choices(rows) = step_for(&choices, "Lock after") else {
            panic!("declared values are rows");
        };
        assert_eq!(rows[1], (Value::from(5), "5".to_string()));

        let switch = ActionParam::required("enabled", ParamType::Boolean, "On or off");
        assert!(matches!(step_for(&switch, "Wi-Fi"), Step::Choices(rows) if rows.len() == 2));

        let percent = ActionParam::required("percent", ParamType::Integer, "Percent")
            .with_schema(serde_json::json!({"minimum": 0, "maximum": 100, "default": 40}));
        let Step::Field(spec) = step_for(&percent, "Volume") else {
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
        let Step::Field(spec) = step_for(&secret, "Unlock") else {
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
