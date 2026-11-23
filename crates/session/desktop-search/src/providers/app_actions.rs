//! The actions open applications publish, as the palette finds them.

use tilo_actions::ResourceSpec;
use ui::search::system::ResourceKind;

use super::offered;
use crate::model::{ActionTarget, Hit, Source, Target};
use crate::rank;

/// The actions open applications publish (`tilo_actions`), the same
/// catalogue their own controls, agents and remote peers invoke.
pub struct Actions(pub Vec<ResourceSpec>);

impl Source for Actions {
    fn id(&self) -> &'static str {
        "app-actions"
    }

    fn name(&self) -> String {
        crate::i18n::t("search-source-app-actions")
    }

    fn query(&self, needle: &str) -> Vec<Hit> {
        // Every open window's actions would bury everything else unasked.
        if needle.trim().is_empty() {
            return Vec::new();
        }
        let records: std::collections::HashSet<&str> =
            self.0.iter().map(|record| record.id.as_str()).collect();
        let flats: Vec<_> = tilo_actions::flatten(&self.0)
            .into_iter()
            .filter(|flat| flat.action.contract.a_person_can_take_now())
            .collect();
        // An open document's actions answer to what they do. An item's (one
        // message of a chat, one card of a board) answer only to the item
        // named: "edit message" must not list the same edit once for every
        // message there is. Each kind is ranked as one list.
        // An action whose one asked value is a choice among named ones ("Link a provider:
        // OpenRouter") is a row per choice, found by the choice's name as a button that does it is.
        let rows: Vec<ChoiceRow<'_, '_>> = flats
            .iter()
            .flat_map(|flat| {
                let choices = named_choices(flat.action);
                let whole = choices.is_empty().then_some(None);
                whole
                    .into_iter()
                    .chain(choices.into_iter().map(Some))
                    .map(move |choice| ChoiceRow::new(flat, choice))
            })
            .collect();
        // An open document's actions answer to what they do. An item's (one
        // message of a chat, one card of a board) answer only to the item
        // named: "edit message" must not list the same edit once for every
        // message there is. Each kind is ranked as one list.
        let own_of = |row: &ChoiceRow<'_, '_>| records.contains(row.flat.resource_id.as_str());
        let (own, items): (Vec<&ChoiceRow<'_, '_>>, Vec<&ChoiceRow<'_, '_>>) =
            rows.iter().partition(|row| own_of(row));
        let mut own_scores = rank::action_match_scores(
            needle,
            own.iter().map(|row| {
                (
                    row.title.as_str(),
                    row.keywords.as_slice(),
                    row.flat.resource_label.as_str(),
                )
            }),
        )
        .into_iter();
        let mut item_scores = rank::item_action_match_scores(
            needle,
            items.iter().map(|row| row.flat.resource_label.as_str()),
        )
        .into_iter();
        rows.into_iter()
            .filter_map(|row| {
                let score = if own_of(&row) {
                    own_scores.next().flatten()?
                } else {
                    item_scores.next().flatten()?
                };
                let ChoiceRow {
                    flat,
                    choice,
                    title,
                    ..
                } = row;
                let (id, preset) = match choice {
                    Some(choice) => (
                        format!("{}\u{1f}{}", flat.key, choice.value),
                        Some((choice.param, choice.value)),
                    ),
                    None => (flat.key.clone(), None),
                };
                Some(Hit {
                    row: None,
                    preview: None,
                    faces: Vec::new(),
                    outcome: None,
                    kind: ResourceKind::Action,
                    provider: self.id().into(),
                    id,
                    title,
                    subtitle: Some(flat.resource_label.clone()),
                    facts: Vec::new(),
                    score,
                    icon: crate::window::action_icon(flat.action),
                    target: Target::Action(Box::new(ActionTarget {
                        resource: flat.resource_id.clone(),
                        resource_kind: flat.resource_kind.clone(),
                        resource_label: flat.resource_label.clone(),
                        spec: flat.action.clone(),
                        preset,
                    })),
                })
            })
            .collect()
    }
}

/// One row of an open app's action: the action, or one of the named values it chooses among.
struct ChoiceRow<'f, 'a> {
    flat: &'f tilo_actions::FlatAction<'a>,
    choice: Option<Choice>,
    title: String,
    keywords: Vec<String>,
}

impl<'f, 'a> ChoiceRow<'f, 'a> {
    fn new(flat: &'f tilo_actions::FlatAction<'a>, choice: Option<Choice>) -> Self {
        let mut keywords = flat.action.keywords.clone();
        let title = match &choice {
            Some(choice) => {
                keywords.push(choice.label.clone());
                format!("{}: {}", flat.action.label, choice.label)
            }
            None => flat.action.label.clone(),
        };
        Self {
            flat,
            choice,
            title,
            keywords,
        }
    }
}

/// One named value an action's one asked parameter takes.
struct Choice {
    param: String,
    value: String,
    label: String,
}

/// The named values `action` takes, when the one value it asks for is a choice among them that
/// each say how a person reads them ([`tilo_actions::ActionParam::with_labelled_choices`]); none
/// for any other action.
fn named_choices(action: &tilo_actions::ActionSpec) -> Vec<Choice> {
    let mut required = action.params().iter().filter(|param| param.required);
    let (Some(param), None) = (required.next(), required.next()) else {
        return Vec::new();
    };
    param
        .choices
        .iter()
        .filter_map(|value| {
            Some(Choice {
                param: param.name.to_string(),
                value: value.to_string(),
                label: param.choice_label(value)?.to_owned(),
            })
        })
        .collect()
}
