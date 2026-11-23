//! What a call that carries other actions would do, for the person who is asked about it.
//!
//! A resource publishes actions that carry its other actions: a typed edit, a group of edits
//! ([`tilo_actions::CARRIER_TAG`]). Such a call is what it carries. Where it waits for its
//! person, they are asked about the carried actions by their own names, in the words those
//! go by, and are told that Undo takes it back only where it takes back every one of them.
//!
//! An always is for the action that asked, by its own name and resource type, never for what
//! carried it. So the question about a call that carries others offers no always
//! ([`About::facts`]).
//!
//! What is carried is read from the call's arguments in the one form an invocation is
//! written as a value: `{action: id, args: {..}}`, alone, under `op`, or in a list. Each is
//! looked up among the actions the same resource offers the caller. Arguments that hold
//! anything else carry something that has no name of its own: such a call is asked about as
//! it stands, every time, and is not said to be undoable.

use serde_json::{Map, Value};
use tilo_actions::remote::Caller;
use tilo_actions::remote::asked::{AskedBy, AskedFacts, Named};
use tilo_actions::{ActionDescriptor, ConfirmationPolicy, Draft, DraftField, DraftValue};

use super::bounded::About;

/// One action a call carries: as its resource declares it under its own name, with its
/// own arguments.
pub(super) struct Carried {
    /// The name its resource publishes it under.
    name: String,
    descriptor: ActionDescriptor,
    arguments: Map<String, Value>,
    /// The keys that lead to those arguments in the arguments of the call that carries it.
    path: Vec<String>,
}

impl Carried {
    /// Whether it asks its person when it is called by its own name.
    fn asks(&self) -> bool {
        let contract = &self.descriptor.contract;
        contract.confirmation != ConfirmationPolicy::None || contract.requires_confirmation()
    }
}

/// What a call that waits for its person carries.
pub(super) enum Carries {
    /// Nothing: the call is an action of its own.
    Nothing,
    /// Something that has no name of its own: its resource declares no action for it, the
    /// caller is not offered that action, or whether it asks depends on its arguments.
    Unnamed,
    /// These actions of its resource, in the order it carries them. One of them at least
    /// asks by its own name.
    Actions(Vec<Carried>),
}

/// One invocation as a call's arguments hold it.
struct Invocation {
    action: String,
    arguments: Map<String, Value>,
    path: Vec<String>,
}

/// The invocation `value` is, written `{action: id, args: {..}}`, alone or under `op`.
/// `None` for anything else, also for a value that holds more than a name and its
/// arguments: what a carrier makes of more is its own to know.
fn invocation(value: &Value) -> Option<Invocation> {
    let (value, mut path) = match value.get("op") {
        Some(op @ Value::Object(_)) => (op, vec!["op".to_owned()]),
        _ => (value, Vec::new()),
    };
    let fields = value.as_object()?;
    if fields.keys().any(|key| key != "action" && key != "args") {
        return None;
    }
    let action = fields.get("action")?.as_str()?.to_owned();
    let arguments = match fields.get("args") {
        None | Some(Value::Null) => Map::new(),
        Some(Value::Object(arguments)) => arguments.clone(),
        Some(_) => return None,
    };
    path.push("args".to_owned());
    Some(Invocation {
        action,
        arguments,
        path,
    })
}

/// Every invocation `arguments` hold, in their order: each argument is one invocation or a
/// list of them. `None` where a part of them is anything else, and where they hold none.
fn invocations(arguments: &Map<String, Value>) -> Option<Vec<Invocation>> {
    let mut found = Vec::new();
    for (name, value) in arguments {
        if let Value::Array(members) = value {
            for (index, member) in members.iter().enumerate() {
                let mut held = invocation(member)?;
                held.path.splice(0..0, [name.clone(), index.to_string()]);
                found.push(held);
            }
        } else {
            let mut held = invocation(value)?;
            held.path.insert(0, name.clone());
            found.push(held);
        }
    }
    (!found.is_empty()).then_some(found)
}

/// What the call of `descriptor` with `arguments` carries. `declared` answers a name with
/// the action its resource offers the caller under it.
pub(super) fn read(
    descriptor: &ActionDescriptor,
    arguments: &Map<String, Value>,
    declared: impl Fn(&str) -> Option<ActionDescriptor>,
) -> Carries {
    if !descriptor.contract.carries_actions() {
        return Carries::Nothing;
    }
    let Some(held) = invocations(arguments) else {
        return Carries::Unnamed;
    };
    let mut actions = Vec::with_capacity(held.len());
    for Invocation {
        action,
        arguments,
        path,
    } in held
    {
        // Only an action whose own declaration says whether it asks is read by its name: one
        // that carries others itself, or whose asking depends on its arguments, is not.
        let own = declared(&action).filter(|own| {
            let contract = &own.contract;
            !contract.carries_actions()
                && !contract
                    .tags
                    .iter()
                    .any(|tag| tag == tilo_actions::ARGUMENT_RISK_TAG)
        });
        let Some(descriptor) = own else {
            return Carries::Unnamed;
        };
        actions.push(Carried {
            name: action,
            descriptor,
            arguments,
            path,
        });
    }
    if actions.iter().any(Carried::asks) {
        Carries::Actions(actions)
    } else {
        Carries::Unnamed
    }
}

impl Carries {
    /// The carried actions that ask their person by their own name.
    fn asking(&self) -> impl Iterator<Item = &Carried> {
        let actions: &[Carried] = match self {
            Self::Actions(actions) => actions,
            Self::Nothing | Self::Unnamed => &[],
        };
        actions.iter().filter(|carried| carried.asks())
    }

    /// What the question is about, in the system's words: the labels of the carried actions
    /// that ask, each once, and `own`, the call's own label, where none is named.
    fn label(&self, own: &str) -> String {
        let mut labels: Vec<&str> = Vec::new();
        for carried in self.asking() {
            let label = carried.descriptor.action_label.as_str();
            if !labels.contains(&label) {
                labels.push(label);
            }
        }
        if labels.is_empty() {
            own.to_owned()
        } else {
            labels.join(", ")
        }
    }

    /// The call of `descriptor` with `arguments` as its person reviews it: every action it
    /// carries with its own arguments, by its own label where there are several, and Undo
    /// takes it back only where it takes back each of them.
    fn draft(&self, descriptor: &ActionDescriptor, arguments: &Map<String, Value>) -> Draft {
        let own = Draft::render(descriptor, arguments);
        match self {
            Self::Nothing => own,
            // Whether Undo takes back what has no name nothing here knows: it is not said.
            Self::Unnamed => Draft {
                undoable: false,
                ..own
            },
            Self::Actions(actions) => {
                let mut draft = Draft {
                    action_label: self.label(&own.action_label),
                    resource_label: own.resource_label,
                    undoable: true,
                    ..Draft::default()
                };
                let several = actions.len() > 1;
                for carried in actions {
                    let its = Draft::render(&carried.descriptor, &carried.arguments);
                    draft.undoable &= its.undoable;
                    if its.fields.is_empty() {
                        draft.fields.push(DraftField {
                            path: carried.path.clone(),
                            label: its.action_label.clone(),
                            value: DraftValue::Flag { on: true },
                            editable: false,
                            multiline: false,
                            shows: None,
                            role: None,
                        });
                    }
                    for mut field in its.fields {
                        field.path.splice(0..0, carried.path.iter().cloned());
                        if several {
                            field.label = format!("{}: {}", its.action_label, field.label);
                        }
                        field.role = None;
                        draft.fields.push(field);
                    }
                }
                draft
            }
        }
    }
}

impl About {
    /// The call as its person reviews it ([`Carries::draft`]).
    pub(super) fn draft(&self) -> Draft {
        self.carries.draft(&self.descriptor, &self.arguments)
    }

    /// What the person `caller` acts for is shown about this call while it waits for their
    /// answer: who asks, the call as they review it, the record it acts on, and the resource
    /// type an always would cover, which a call that carries others offers none of.
    pub(super) fn facts(&self, caller: &Caller) -> AskedFacts {
        let draft = self.draft();
        let acting = caller.acting();
        let by = match acting.agent_id() {
            Some(id) => AskedBy::Agent(Named {
                id: id.to_owned(),
                name: caller.agent_called().unwrap_or(id).to_owned(),
            }),
            None => AskedBy::Device(Named::default()),
        };
        let own = matches!(self.carries, Carries::Nothing);
        AskedFacts {
            by,
            words: String::new(),
            on: Some(self.record.clone()).filter(|record| !record.is_empty()),
            facts: draft
                .summary()
                .into_iter()
                .map(|(label, value)| tilo_actions::remote::asked::Fact { label, value })
                .collect(),
            destructive: self.descriptor.contract.risk == tilo_actions::RiskLevel::Destructive,
            choices: Vec::new(),
            verb: verb_of(&self.carries.label(&self.descriptor.action_label)),
            plural: own
                .then(|| tilo_actions::plural(&self.descriptor.resource_label).to_lowercase())
                .filter(|plural| !plural.is_empty()),
            conversation: caller.turn().map(|turn| turn.conversation.clone()),
        }
    }
}

/// The verb an action's label leads with, as "Always let {asker} {verb} {plural}" reads it.
fn verb_of(label: &str) -> String {
    label
        .split_whitespace()
        .next()
        .unwrap_or_default()
        .to_lowercase()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use tilo_actions::{
        ARGUMENT_RISK_TAG, ActionAddress, ActionContract, ActionParam, ActionState, CARRIER_TAG,
        ExecutionLocus, IRREVERSIBLE_TAG, InputSpec, OperationKind, ParamType, RiskLevel,
    };

    fn arguments(value: &Value) -> Map<String, Value> {
        value.as_object().cloned().unwrap_or_default()
    }

    fn tagged(tags: &[&str]) -> ActionContract {
        ActionContract {
            tags: tags.iter().map(|tag| (*tag).to_owned()).collect(),
            ..ActionContract::default()
        }
    }

    fn descriptor(action: &str, label: &str, contract: ActionContract) -> ActionDescriptor {
        ActionDescriptor {
            did: None,
            state: ActionState::default(),
            semantic_identity: None,
            address: ActionAddress::new("shelf", action),
            resource_kind: "test.shelf".into(),
            resource_label: "Shelf".into(),
            action_label: label.into(),
            keywords: Vec::new(),
            icon: None,
            description: None,
            locus: ExecutionLocus::ClientLocal,
            kind: OperationKind::Document,
            contract,
            input: InputSpec::None,
            params: vec![ActionParam::required("book", ParamType::Text, "The book")],
            view: None,
        }
    }

    /// A shelf's actions by name: a removal Undo cannot take back, which asks; a harmless
    /// shelving; a rename whose asking depends on its arguments; and the two carriers.
    fn declared(name: &str) -> Option<ActionDescriptor> {
        let removal = ActionContract {
            risk: RiskLevel::Destructive,
            confirmation: ConfirmationPolicy::Confirm,
            ..tagged(&[IRREVERSIBLE_TAG])
        };
        Some(match name {
            "remove" => descriptor(name, "Remove from library", removal),
            "shelve" => descriptor(name, "Shelve", ActionContract::default()),
            "rename" => descriptor(name, "Rename", tagged(&[ARGUMENT_RISK_TAG])),
            "apply" | "group" => descriptor(name, "Apply", carrier()),
            _ => return None,
        })
    }

    fn carrier() -> ActionContract {
        tagged(&[CARRIER_TAG, ARGUMENT_RISK_TAG])
    }

    fn carried_by(value: &Value) -> Carries {
        read(
            &descriptor("apply", "Apply Shelf edit", carrier()),
            &arguments(value),
            declared,
        )
    }

    /// An invocation is read alone, under `op` and in a list, with where its arguments sit.
    /// Anything else is no invocation: an edit written flat, a field edit, an argument that
    /// is neither, arguments that are no object, and nothing at all.
    #[test]
    fn an_invocation_is_read_in_the_one_form_it_is_written_as_a_value() {
        let one = invocations(&arguments(&json!({
            "action": {"op": {"action": "remove", "args": {"book": "b1"}}, "at": 5}
        })))
        .expect("an invocation under op");
        assert_eq!(one.len(), 1);
        assert_eq!(one[0].action, "remove");
        assert_eq!(one[0].arguments, arguments(&json!({"book": "b1"})));
        assert_eq!(one[0].path, ["action", "op", "args"]);

        let two = invocations(&arguments(&json!({
            "actions": [{"action": "shelve", "args": {"book": "b2"}}, {"action": "remove"}]
        })))
        .expect("a list of invocations");
        assert_eq!(two.len(), 2);
        assert_eq!(two[0].path, ["actions", "0", "args"]);
        assert_eq!(two[1].action, "remove");
        assert!(two[1].arguments.is_empty());
        assert_eq!(two[1].path, ["actions", "1", "args"]);

        for other in [
            json!({"action": {"action": "remove", "book": "b1"}}),
            json!({"edit": {"path": [{"field": "entries"}], "op": "remove"}}),
            json!({"actions": [{"action": "remove"}], "dry": true}),
            json!({"action": {"op": {"action": "remove", "args": ["b1"]}}}),
            json!({"actions": []}),
            json!({}),
        ] {
            assert!(invocations(&arguments(&other)).is_none(), "{other}");
        }
    }

    /// A call that is no carrier carries nothing. A carrier's actions are read by the names
    /// its resource offers, where one of them at least asks by its own name. What it holds
    /// has no name where it is no invocation, names nothing offered, names a carrier or an
    /// action whose asking depends on its arguments, or where none of it asks by its name.
    #[test]
    fn what_a_carrier_holds_is_read_by_the_names_its_resource_offers() {
        let alone = read(
            &descriptor("remove", "Remove from library", ActionContract::default()),
            &arguments(&json!({"book": "b1"})),
            declared,
        );
        assert!(matches!(alone, Carries::Nothing));

        let removal = json!({"action": "remove", "args": {"book": "b1"}});
        let shelving = json!({"action": "shelve", "args": {"book": "b2"}});
        let mixed = carried_by(&json!({"actions": [shelving, removal]}));
        let Carries::Actions(actions) = &mixed else {
            panic!("a shelving and a removal are read by their names");
        };
        assert_eq!(actions.len(), 2);
        let asking: Vec<&str> = mixed
            .asking()
            .map(|carried| carried.name.as_str())
            .collect();
        assert_eq!(asking, ["remove"], "only the removal asks by its own name");

        for unnamed in [
            json!({"actions": [shelving]}),
            json!({"actions": [removal, {"action": "burn", "args": {}}]}),
            json!({"actions": [removal, {"action": "rename", "args": {"book": "b1"}}]}),
            json!({"actions": [removal, {"action": "apply", "args": {}}]}),
            json!({"action": {"action": "remove", "book": "b1"}}),
        ] {
            assert!(
                matches!(carried_by(&unnamed), Carries::Unnamed),
                "{unnamed}"
            );
        }
    }

    /// The question about a carrier says what it carries: the label of what asks, each once;
    /// one carried action as its own question would show it; several each under its own
    /// label; and Undo is not said to take back what holds a removal it cannot bring back,
    /// nor what has no name.
    #[test]
    fn the_question_about_a_carrier_says_what_it_carries() {
        let own = descriptor("apply", "Apply Shelf edit", carrier());
        let removal = json!({"action": "remove", "args": {"book": "b1"}});
        let held = json!({"action": {"op": removal}});
        let one = carried_by(&held);
        assert_eq!(one.label("Apply Shelf edit"), "Remove from library");
        let draft = one.draft(&own, &arguments(&held));
        assert_eq!(draft.action_label, "Remove from library");
        assert_eq!(draft.resource_label, "Shelf");
        assert_eq!(draft.fields.len(), 1, "{draft:?}");
        assert_eq!(draft.fields[0].path, ["action", "op", "args", "book"]);
        assert_eq!(draft.fields[0].value.line(), "b1");
        assert!(!draft.undoable, "Undo does not bring the book back");

        let held = json!({"actions": [
            {"action": "shelve", "args": {"book": "b2"}},
            removal,
            {"action": "remove", "args": {"book": "b3"}},
        ]});
        let several = carried_by(&held);
        assert_eq!(several.label("Apply"), "Remove from library", "said once");
        let draft = several.draft(&own, &arguments(&held));
        let shown: Vec<(String, String)> = draft
            .fields
            .iter()
            .map(|field| (field.label.clone(), field.value.line()))
            .collect();
        assert_eq!(shown.len(), 3, "{shown:?}");
        assert!(
            shown[0].0.starts_with("Shelve: ") && shown[0].1 == "b2",
            "{shown:?}"
        );
        assert!(
            shown[1].0.starts_with("Remove from library: ") && shown[1].1 == "b1",
            "{shown:?}"
        );
        assert_eq!(shown[2].1, "b3", "{shown:?}");
        assert!(!draft.undoable);

        let held = json!({"edit": {"path": [{"field": "entries"}], "op": "remove"}});
        let unnamed = carried_by(&held);
        assert_eq!(unnamed.label("Apply Shelf edit"), "Apply Shelf edit");
        assert!(!unnamed.draft(&own, &arguments(&held)).undoable);
    }
}
