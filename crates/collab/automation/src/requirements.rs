//! What a program needs from a room and its host, derived from its steps and
//! triggers alone. A directory entry lists exactly this, and the registry
//! refuses a listing that says anything else.

use std::collections::BTreeSet;

use crate::{AGENT_APP_ID, Shortcut, Step, StepKind, TriggerSpec};

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ProgramRequirements {
    /// Apps whose actions the steps invoke or whose messages a trigger
    /// watches, other than connectors.
    pub apps: BTreeSet<String>,
    /// Apps the triggers take connector events from. Invoking a connector's
    /// verbs counts here too, never under `apps`.
    pub connectors: BTreeSet<String>,
    /// Whether any step posts under another name and avatar.
    pub appearance: bool,
    /// `TriggerSpec::kind` of every trigger.
    pub triggers: BTreeSet<String>,
}

/// Everything `shortcut` needs, walking nested steps.
#[must_use]
pub fn requirements(shortcut: &Shortcut) -> ProgramRequirements {
    let mut out = ProgramRequirements::default();
    for trigger in &shortcut.triggers {
        out.triggers.insert(trigger.kind().to_owned());
        match trigger {
            TriggerSpec::IncomingMessage {
                app_id: Some(app_id),
                ..
            } => {
                out.apps.insert(app_id.clone());
            }
            TriggerSpec::Connector { app_id, .. } => {
                out.connectors.insert(app_id.clone());
            }
            // A record kind does not name the app that owns it; the steps
            // that act on it do.
            _ => {}
        }
    }
    walk(&shortcut.steps, &mut out);
    let connectors = out.connectors.clone();
    out.apps.retain(|app| !connectors.contains(app));
    out
}

fn walk(steps: &[Step], out: &mut ProgramRequirements) {
    for step in steps {
        match &step.kind {
            StepKind::Invoke {
                action, appearance, ..
            } => {
                out.apps.insert(action.app.clone());
                out.appearance |= appearance.is_some();
            }
            StepKind::Ai { .. } => {
                out.apps.insert(AGENT_APP_ID.to_owned());
            }
            StepKind::If {
                then, otherwise, ..
            } => {
                walk(then, out);
                walk(otherwise, out);
            }
            StepKind::ForEach { body, .. } => walk(body, out),
            StepKind::SetVar { .. } | StepKind::Ask { .. } => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{AiTransform, Binding, Condition, Selector};
    use std::collections::BTreeMap;
    use tilo_actions::ActionKey;

    fn invoke(app: &str, kind: &str, action: &str) -> StepKind {
        StepKind::Invoke {
            action: ActionKey::new(app, kind, action),
            resource: Selector::FirstOfKind(kind.into()),
            args: BTreeMap::new(),
            appearance: None,
        }
    }

    fn program(steps: Vec<Step>, triggers: Vec<TriggerSpec>) -> Shortcut {
        Shortcut {
            id: "p".into(),
            name: "P".into(),
            inputs: Vec::new(),
            steps,
            step_timeout_secs: None,
            output: None,
            grant: Vec::new(),
            triggers,
            saved_by: None,
            saved_proof: None,
        }
    }

    #[test]
    fn requirements_walk_nested_steps() {
        let program = program(
            vec![Step::new(
                "branch",
                StepKind::If {
                    cond: Condition::Truthy(Binding::Literal(true.into())),
                    then: vec![Step::new(
                        "post",
                        invoke("so.tilo.chat", "chat.workspace", "post"),
                    )],
                    otherwise: vec![Step::new(
                        "each",
                        StepKind::ForEach {
                            items: Binding::Literal(serde_json::json!([])),
                            item: "item".into(),
                            body: vec![
                                Step::new("send", invoke("so.tilo.mail", "mail.account", "send")),
                                Step::new(
                                    "sum",
                                    StepKind::Ai {
                                        transform: AiTransform::Summarise,
                                        input: Binding::Var("item".into()),
                                    },
                                ),
                            ],
                        },
                    )],
                },
            )],
            vec![TriggerSpec::IncomingMessage {
                app_id: Some("so.tilo.messages".into()),
                from: None,
            }],
        );
        let r = requirements(&program);
        assert_eq!(
            r.apps,
            BTreeSet::from([
                AGENT_APP_ID.to_owned(),
                "so.tilo.chat".to_owned(),
                "so.tilo.mail".to_owned(),
                "so.tilo.messages".to_owned(),
            ])
        );
        assert!(r.connectors.is_empty());
        assert!(!r.appearance);
        assert_eq!(r.triggers, BTreeSet::from(["incoming_message".to_owned()]));
    }

    #[test]
    fn a_nested_post_under_another_name_needs_the_appearance() {
        let mut post = invoke("so.tilo.chat", "chat.workspace", "post");
        if let StepKind::Invoke { appearance, .. } = &mut post {
            *appearance = Some(Binding::Var("item/appearance".into()));
        }
        let program = program(
            vec![Step::new(
                "each",
                StepKind::ForEach {
                    items: Binding::Literal(serde_json::json!([])),
                    item: "item".into(),
                    body: vec![Step::new("post", post)],
                },
            )],
            Vec::new(),
        );
        assert!(requirements(&program).appearance);
    }

    #[test]
    fn a_connector_is_never_counted_as_an_app() {
        let program = program(
            vec![
                Step::new("post", invoke("so.tilo.chat", "chat.workspace", "post")),
                Step::new(
                    "relay",
                    invoke("so.tilo.discord", "discord.guild", "send-message"),
                ),
            ],
            vec![
                TriggerSpec::Connector {
                    app_id: "so.tilo.discord".into(),
                    source: "message-created".into(),
                    filter: serde_json::Map::new(),
                },
                TriggerSpec::RecordChanged {
                    kind: "chat.workspace".into(),
                    actions: vec!["post".into()],
                    fields: Vec::new(),
                    by: crate::ActorFilter::default(),
                },
            ],
        );
        let r = requirements(&program);
        assert_eq!(r.apps, BTreeSet::from(["so.tilo.chat".to_owned()]));
        assert_eq!(r.connectors, BTreeSet::from(["so.tilo.discord".to_owned()]));
        assert!(!r.appearance);
        assert_eq!(
            r.triggers,
            BTreeSet::from(["connector".to_owned(), "record_changed".to_owned()])
        );
    }
}
