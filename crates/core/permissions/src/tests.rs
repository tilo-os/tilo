#![allow(clippy::unwrap_used, reason = "test: a failure is the assertion")]

use tilo_actions::shown::{ActionDecision, ActionVerdict, CHECKING, ShownAction};
use tilo_actions::{ActionKind, ActionSpec, OperationKind, ResourceSpec};

use super::*;

const CHAT: &str = "so.tilo.chat";

fn edit(id: &str) -> ActionSpec {
    let mut spec =
        ActionSpec::operation(id, id, OperationKind::Effect, format!("t.{id}"), &()).unwrap();
    spec.type_mut().contract.kind = ActionKind::Undoable;
    spec
}

/// A chat whose typed edit carries a post and a pin, the pin a moderator's.
fn chat() -> ResourceSpec {
    let typed = ActionSpec::typed(
        "apply",
        "Apply",
        "Action as JSON",
        tilo_actions::ExecutionLocus::ClientLocal,
        vec![
            tilo_actions::ActionParam::required(
                "action",
                tilo_actions::ParamType::Object,
                "An edit",
            )
            .with_schema(serde_json::json!({
                "type": "object",
                "properties": {
                    "action": {"type": "string", "enum": ["post", "pin"]},
                    "args": {"type": "object"},
                },
            })),
        ],
        |_| tilo_actions::EncodedAction {
            type_id: "t.apply".to_owned(),
            payload: Vec::new(),
        },
    )
    .carrying()
    .of_kind(ActionKind::Undoable);
    ResourceSpec::new("chat", "chat:1", "Chat")
        .with_action(edit("post"))
        .with_action(edit("pin").below_field(policy::roles::MODERATION_FIELD))
        .with_action(typed)
}

/// The window's verdicts once the device said `verdict` of each of `chat`'s actions.
fn decided(chat: &ResourceSpec, verdict: impl Fn(&str) -> ActionVerdict) -> Verdicts {
    let mut verdicts = Verdicts::default();
    verdicts.ask(
        chat.actions
            .iter()
            .map(|action| ShownAction::published(CHAT, chat, action)),
    );
    let decisions: Vec<ActionDecision> = chat
        .actions
        .iter()
        .map(|action| ActionDecision {
            action: ShownAction::published(CHAT, chat, action),
            verdict: verdict(&action.id),
        })
        .collect();
    verdicts.decide(&decisions);
    verdicts
}

/// What the typed edit names of `shown`.
fn named(shown: &ResourceSpec) -> Option<serde_json::Value> {
    shown
        .actions
        .iter()
        .find(|action| action.id == "apply")
        .and_then(|apply| apply.params().first()?.schema.clone())
        .map(|schema| schema["properties"]["action"]["enum"].clone())
}

/// A window shows what its device allows and leaves out what it refuses, and a typed edit
/// beside them names only what the person may take by name: to pin is a moderator's, so an
/// editor's typed edit does not name it.
#[test]
fn a_window_offers_what_its_device_allows_and_names_nothing_it_refuses() {
    let chat = chat();
    let moderator = decided(&chat, |_| ActionVerdict::Allowed)
        .permitted_resource(CHAT, chat.clone())
        .unwrap();
    assert_eq!(named(&moderator), Some(serde_json::json!(["post", "pin"])));
    let editor = decided(&chat, |action| match action {
        "pin" => ActionVerdict::Refused("a moderator's".into()),
        _ => ActionVerdict::Allowed,
    });
    let shown = editor.permitted_resource(CHAT, chat.clone()).unwrap();
    assert!(shown.actions.iter().all(|action| action.id != "pin"));
    assert_eq!(named(&shown), Some(serde_json::json!(["post"])));
    let pin = &chat.actions[1];
    assert_eq!(
        editor.admit(CHAT, &chat, pin, (None, None)),
        Err(tilo_actions::ApiError::Unavailable {
            code: "permission_denied".into(),
            reason: "a moderator's".into(),
        }),
        "a call of it says why it is refused"
    );
    let viewer = decided(&chat, |_| ActionVerdict::Refused(REFUSED.into()));
    assert!(viewer.permitted_resource(CHAT, chat).is_none());
}

/// Before its device answers, a window shows its controls as not yet decided and acts on none
/// of them; what it reads of a question it never asked, it asks next.
#[test]
fn a_window_acts_on_nothing_its_device_has_not_answered() {
    let chat = chat();
    let mut verdicts = Verdicts::default();
    verdicts.ask(
        chat.actions
            .iter()
            .map(|action| ShownAction::published(CHAT, &chat, action)),
    );
    let post = &chat.actions[0];
    assert_eq!(
        verdicts.admit(CHAT, &chat, post, (None, None)),
        Err(tilo_actions::ApiError::Unavailable {
            code: "permission_denied".into(),
            reason: CHECKING.into(),
        })
    );
    let shown = verdicts.permitted_resource(CHAT, chat.clone()).unwrap();
    assert_eq!(shown.actions.len(), 3, "each still shows, not yet decided");
    let settings = ShownAction::workspace_action(policy::roles::governance::SETTINGS);
    assert!(!verdicts.permits(&settings));
    assert!(!verdicts.asked().contains(&settings));
    assert!(verdicts.take_missed());
    assert!(verdicts.asked().contains(&settings), "asked next");
    assert!(!verdicts.take_missed(), "and only once");
    verdicts.decide(&[ActionDecision {
        action: settings.clone(),
        verdict: ActionVerdict::AsksFirst,
    }]);
    assert!(verdicts.permits(&settings));
    let reading = verdicts.reading();
    assert!(
        !reading.permits(&settings),
        "a past version changes nothing"
    );
    assert_eq!(reading.refusal(&settings).as_deref(), Some(REFUSED));
}

/// A grant given once is never spent in a window: the device says such an action asks first,
/// so the window sends it through the device's dispatch, which spends the grant as it admits it.
#[test]
fn an_action_asking_first_goes_through_the_device() {
    let chat = chat();
    let verdicts = decided(&chat, |_| ActionVerdict::AsksFirst);
    assert_eq!(
        verdicts.admit(CHAT, &chat, &chat.actions[0], (None, None)),
        Err(tilo_actions::ApiError::ConfirmationRequired)
    );
}

/// A member-own action stays on a resource whatever the window's verdicts say of it, even one
/// opened read-only: the workspace's host runs it for a member, as them, and admits each call.
#[test]
fn a_member_own_action_is_published_whatever_the_verdicts_say() {
    let mut contract = tilo_actions::ActionContract {
        kind: ActionKind::Undoable,
        ..tilo_actions::ActionContract::default()
    };
    contract.tags.push(tilo_actions::MEMBER_OWN_TAG.to_owned());
    let connect = ActionSpec::operation(
        "connect",
        "connect",
        OperationKind::Effect,
        "discord.effects",
        &(),
    )
    .unwrap()
    .with_contract(contract);
    let mut guild = ResourceSpec::new("discord.guild", "discord.guild", "Discord server");
    guild.actions.push(connect);
    let reader = Verdicts::default().reading();
    let kept = reader
        .permitted_resource("discord", guild.clone())
        .expect("the reader keeps the member-own action");
    assert!(kept.actions.iter().any(|action| action.id == "connect"));
    assert_eq!(
        reader.admit("discord", &guild, &guild.actions[0], (None, None)),
        Ok(()),
        "the host decides the call"
    );
}
