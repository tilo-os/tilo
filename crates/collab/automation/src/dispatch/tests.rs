use serde_json::{Map, json};
use tilo_actions::ActionKey;
use tilo_actions::entry::{Did, EditOp, FieldEdit, GroupOf, PathStep, VerbCall};
use tilo_actions::interaction::{ActorKind, InteractionActor};
use tilo_actions::params::{ActionParam, ParamType};

use super::*;
use crate::shortcut::{ActorFilter, Shortcut, TriggerSpec};

fn program(triggers: Vec<TriggerSpec>) -> Shortcut {
    let mut shortcut: Shortcut = serde_json::from_value(json!({
        "id": "bridge",
        "name": "Bridge",
        "steps": [],
        "saved_by": "ada"
    }))
    .unwrap();
    shortcut.triggers = triggers;
    shortcut.inputs = vec![ActionParam::required(
        "event",
        ParamType::Object,
        "what fired",
    )];
    shortcut
}

fn verb(action: &str) -> Did {
    Did::Verb(VerbCall::new(
        ActionKey::new("so.tilo.chat", "chat.workspace", action),
        [],
    ))
}

fn post(actor: InteractionActor, change: &str, caused_by: Option<Cause>) -> RecordEvent {
    RecordEvent {
        room: "room-1".into(),
        record: "rec-1".into(),
        kind: "chat.workspace".into(),
        change: change.into(),
        actor,
        did: verb("post"),
        arguments: Map::from_iter([("text".to_string(), json!("hi"))]),
        item: None,
        before: None,
        after: Some(json!({"text": "hi"})),
        caused_by,
        incoming_message: true,
        shortcut: None,
        run: None,
    }
}

fn person(id: &str) -> InteractionActor {
    InteractionActor::new(id, ActorKind::Person)
}

fn bot(id: &str) -> InteractionActor {
    InteractionActor {
        id: format!("bot:{id}"),
        kind: ActorKind::Bot,
        for_person: Some("ada".into()),
    }
}

fn watch_chat(by: ActorFilter) -> TriggerSpec {
    TriggerSpec::RecordChanged {
        kind: "chat.workspace".into(),
        actions: vec![],
        fields: vec![],
        by,
    }
}

fn as_bot<'a>(installation: &'a str, shortcut: &'a Shortcut) -> [Armed<'a>; 1] {
    [Armed {
        firer: Firer::Bot { installation },
        shortcut,
    }]
}

fn as_shortcut(shortcut: &Shortcut) -> [Armed<'_>; 1] {
    [Armed {
        firer: Firer::Shortcut {
            shortcut: "bridge",
            saver: "ada",
        },
        shortcut,
    }]
}

#[test]
fn a_post_by_a_person_fires_the_bot_with_the_event_bound() {
    let shortcut = program(vec![watch_chat(ActorFilter::Anyone)]);
    let event = post(person("bob"), &"a".repeat(64), None);
    let out = dispatch(&as_bot("b1", &shortcut), Event::Record(&event));
    assert_eq!(out.firings.len(), 1);
    let firing = &out.firings[0];
    assert_eq!(firing.firer, "bot:b1");
    assert_eq!(firing.inputs["event"]["after"]["text"], "hi");
    assert_eq!(firing.inputs["event"]["actor"]["id"], "bob");
    assert_eq!(
        firing.cause,
        Some(Cause {
            installation: "b1".into(),
            event: "a".repeat(64),
            depth: 1
        })
    );
}

#[test]
fn a_bot_never_fires_on_its_own_change() {
    let shortcut = program(vec![watch_chat(ActorFilter::Anyone)]);
    let event = post(bot("b1"), &"b".repeat(64), None);
    let out = dispatch(&as_bot("b1", &shortcut), Event::Record(&event));
    assert!(out.firings.is_empty());
    assert_eq!(
        out.skipped,
        vec![("bot:b1".to_string(), 0, Skipped::SelfTriggered)]
    );
}

#[test]
fn a_shortcut_never_fires_on_a_change_its_own_run_made() {
    let shortcut = program(vec![watch_chat(ActorFilter::Anyone)]);
    // A Shortcut's run acts as its person; the run's `shortcut` facet is
    // what names it.
    let mut event = post(
        InteractionActor {
            id: "ada".into(),
            kind: ActorKind::Shortcut,
            for_person: Some("ada".into()),
        },
        &"c".repeat(64),
        None,
    );
    event.shortcut = Some("bridge".into());
    let out = dispatch(&as_shortcut(&shortcut), Event::Record(&event));
    assert!(out.firings.is_empty());
    assert_eq!(out.skipped[0].2, Skipped::SelfTriggered);
    // Another Shortcut's run is somebody else's change.
    event.shortcut = Some("tidy".into());
    let out = dispatch(&as_shortcut(&shortcut), Event::Record(&event));
    assert_eq!(out.firings.len(), 1);
    assert_eq!(out.firings[0].cause, None, "a Shortcut records no cause");
}

#[test]
fn a_chain_stops_after_four_hops() {
    let shortcut = program(vec![watch_chat(ActorFilter::Anyone)]);
    for (depth, fires) in [(0_u8, true), (3, true), (4, false), (9, false)] {
        let caused = (depth > 0).then(|| Cause {
            installation: "b1".into(),
            event: "x".into(),
            depth,
        });
        let event = post(bot("b1"), &"d".repeat(64), caused);
        let out = dispatch(&as_bot("b2", &shortcut), Event::Record(&event));
        assert_eq!(!out.firings.is_empty(), fires, "depth {depth}");
        if fires {
            assert_eq!(out.firings[0].cause.as_ref().unwrap().depth, depth + 1);
        } else {
            assert_eq!(out.skipped[0].2, Skipped::TooDeep { depth: depth + 1 });
        }
    }
}

#[test]
fn the_depth_of_a_bot_change_comes_from_the_run_not_the_claim() {
    // A person's change claims a cause; a bot firing on it starts a new
    // chain at depth 1 regardless, whether the claim is short or long.
    let shortcut = program(vec![watch_chat(ActorFilter::Anyone)]);
    for claimed in [0, 9] {
        let forged = Some(Cause {
            installation: "b9".into(),
            event: "y".into(),
            depth: claimed,
        });
        let event = post(person("mallory"), &"e".repeat(64), forged);
        let out = dispatch(&as_bot("b2", &shortcut), Event::Record(&event));
        assert_eq!(out.firings[0].cause.as_ref().unwrap().depth, 1);
    }
}

#[test]
fn the_same_change_gives_the_same_run_id() {
    let shortcut = program(vec![
        watch_chat(ActorFilter::Anyone),
        watch_chat(ActorFilter::People),
    ]);
    let event = post(person("bob"), &"f".repeat(64), None);
    let first = dispatch(&as_bot("b1", &shortcut), Event::Record(&event));
    let again = dispatch(&as_bot("b1", &shortcut), Event::Record(&event));
    assert_eq!(first.firings[0].run, again.firings[0].run);
    // Each trigger index is its own run.
    assert_ne!(first.firings[0].run, first.firings[1].run);
    // Another installation of the same program is another run.
    assert_ne!(
        dispatch(&as_bot("b2", &shortcut), Event::Record(&event)).firings[0].run,
        first.firings[0].run
    );
}

#[test]
fn actor_filters_select_whose_changes_count() {
    let people = program(vec![watch_chat(ActorFilter::People)]);
    let not_bots = program(vec![watch_chat(ActorFilter::NotBots)]);
    let agent = InteractionActor {
        id: "agent:a".into(),
        kind: ActorKind::Agent,
        for_person: Some("ada".into()),
    };
    let cases = [
        (person("bob"), true, true),
        (agent, false, true),
        (bot("b9"), false, false),
    ];
    for (actor, by_people, by_not_bots) in cases {
        let event = post(actor.clone(), &"0".repeat(64), None);
        let fires = |shortcut: &Shortcut| {
            !dispatch(&as_bot("b1", shortcut), Event::Record(&event))
                .firings
                .is_empty()
        };
        assert_eq!(fires(&people), by_people, "{actor:?}");
        assert_eq!(fires(&not_bots), by_not_bots, "{actor:?}");
    }
}

#[test]
fn verbs_and_field_patterns_narrow_a_record_trigger() {
    assert!(field_matches("channels.*.name", "channels.c1.name"));
    assert!(!field_matches("channels.*.name", "channels.c1.topic"));
    assert!(field_matches("channels", "channels.c1.name"));
    assert!(!field_matches("channels.*.name", "channels.c1"));
    let only_edits = program(vec![TriggerSpec::RecordChanged {
        kind: "chat.workspace".into(),
        actions: vec!["edit-message".into()],
        fields: vec![],
        by: ActorFilter::Anyone,
    }]);
    let event = post(person("bob"), &"1".repeat(64), None);
    assert!(
        dispatch(&as_bot("b1", &only_edits), Event::Record(&event))
            .firings
            .is_empty()
    );
    let other_kind = program(vec![TriggerSpec::RecordChanged {
        kind: "discord.guild".into(),
        actions: vec![],
        fields: vec![],
        by: ActorFilter::Anyone,
    }]);
    assert!(
        dispatch(&as_bot("b1", &other_kind), Event::Record(&event))
            .firings
            .is_empty()
    );

    let names = program(vec![TriggerSpec::RecordChanged {
        kind: "chat.workspace".into(),
        actions: vec![],
        fields: vec!["channels.*.name".into()],
        by: ActorFilter::Anyone,
    }]);
    let mut renamed = post(person("bob"), &"6".repeat(64), None);
    renamed.did = Did::Edit(FieldEdit {
        ty: "channel".into(),
        field: "name".into(),
        path: vec![
            PathStep::Field("channels".into()),
            PathStep::Item("c1".into()),
            PathStep::Field("name".into()),
        ],
        op: EditOp::Set,
        old: None,
        new: None,
        args: std::collections::BTreeMap::new(),
    });
    assert_eq!(edited_paths(&renamed.did), vec!["channels.c1.name"]);
    assert_eq!(
        dispatch(&as_bot("b1", &names), Event::Record(&renamed))
            .firings
            .len(),
        1
    );
}

#[test]
fn incoming_message_matches_only_flagged_verbs_by_someone_else() {
    let shortcut = program(vec![TriggerSpec::IncomingMessage {
        app_id: Some("so.tilo.chat".into()),
        from: None,
    }]);
    let by_bob = post(person("bob"), &"2".repeat(64), None);
    assert_eq!(
        dispatch(&as_shortcut(&shortcut), Event::Record(&by_bob))
            .firings
            .len(),
        1
    );
    let by_saver = post(person("ada"), &"3".repeat(64), None);
    assert!(
        dispatch(&as_shortcut(&shortcut), Event::Record(&by_saver))
            .firings
            .is_empty()
    );
    let mut unflagged = post(person("bob"), &"4".repeat(64), None);
    unflagged.incoming_message = false;
    assert!(
        dispatch(&as_shortcut(&shortcut), Event::Record(&unflagged))
            .firings
            .is_empty()
    );
    let from_carol = program(vec![TriggerSpec::IncomingMessage {
        app_id: None,
        from: Some("carol".into()),
    }]);
    assert!(
        dispatch(&as_shortcut(&from_carol), Event::Record(&by_bob))
            .firings
            .is_empty()
    );
    let elsewhere = program(vec![TriggerSpec::IncomingMessage {
        app_id: Some("so.tilo.mail".into()),
        from: None,
    }]);
    assert!(
        dispatch(&as_shortcut(&elsewhere), Event::Record(&by_bob))
            .firings
            .is_empty()
    );
}

#[test]
fn a_connector_event_fires_by_source_and_filter() {
    let shortcut = program(vec![TriggerSpec::Connector {
        app_id: "so.tilo.discord".into(),
        source: "message-created".into(),
        filter: Map::from_iter([("channel".to_string(), json!("42"))]),
    }]);
    let event = ConnectorEvent {
        app_id: "so.tilo.discord".into(),
        source: "message-created".into(),
        id: "1188".into(),
        at_ms: 5,
        payload: Map::from_iter([
            ("channel".to_string(), json!("42")),
            ("content".to_string(), json!("yo")),
        ]),
    };
    let out = dispatch(&as_bot("b1", &shortcut), Event::Connector(&event));
    assert_eq!(out.firings.len(), 1);
    assert_eq!(out.firings[0].inputs["event"]["payload"]["content"], "yo");
    assert_eq!(out.firings[0].event, "so.tilo.discord/message-created/1188");
    assert_eq!(out.firings[0].cause.as_ref().unwrap().depth, 1);
    let mut elsewhere = event.clone();
    elsewhere.payload.insert("channel".into(), json!("7"));
    assert!(
        dispatch(&as_bot("b1", &shortcut), Event::Connector(&elsewhere))
            .firings
            .is_empty()
    );
    // A person's Shortcut never fires on a connector event.
    assert!(
        dispatch(&as_shortcut(&shortcut), Event::Connector(&event))
            .firings
            .is_empty()
    );
}

#[test]
fn a_program_without_an_event_input_still_fires_with_no_inputs() {
    let mut shortcut = program(vec![watch_chat(ActorFilter::Anyone)]);
    shortcut.inputs.clear();
    let event = post(person("bob"), &"5".repeat(64), None);
    let out = dispatch(&as_bot("b1", &shortcut), Event::Record(&event));
    assert!(out.firings[0].inputs.is_empty());
}

#[test]
fn group_changes_count_every_member_verb_and_edit() {
    let group = Did::Group(GroupOf {
        members: vec![verb("rename-channel"), verb("post")],
        count: 3,
        distinct: vec![verb("post"), verb("pin")],
    });
    assert_eq!(verbs_of(&group), vec!["rename-channel", "post", "pin"]);
}

#[test]
fn an_entry_reads_as_the_event_every_host_sees() {
    use tilo_actions::{ActionContext, ActionFacet, ActionInvoker, Described, EncodedAction};
    let mut context = ActionContext::for_app("so.tilo.chat", "Chat")
        .with_invoker(ActionInvoker::actor("bob"))
        .with_described(Described::new(verb("post")))
        .with_facet(ActionFacet::new(
            tilo_actions::INCOMING_MESSAGE_FACET,
            "true",
        ))
        .with_facet(ActionFacet::new("shortcut", "tidy"))
        .with_group("6f1c2a8e-3b4d-4c5e-9f60-718293a4b5c6", "Tidy", "shortcut");
    context.action = Some(EncodedAction {
        type_id: "chat.workspace".into(),
        payload: br#"{"action":"post","args":{"channel":"c1","text":"hi"}}"#.to_vec(),
    });
    let event = event_of("room-1", "rec-1", "chat.workspace", "ab", &context);
    assert_eq!(event.actor.id, "bob");
    assert!(event.incoming_message);
    assert_eq!(event.shortcut.as_deref(), Some("tidy"));
    assert_eq!(event.arguments["text"], "hi");
    assert_eq!(verbs_of(&event.did), vec!["post"]);
    // As a door records it: the operation in an envelope with its time.
    context.action = Some(EncodedAction {
        type_id: "chat.workspace".into(),
        payload: br#"{"op":{"action":"react","args":{"message":"m1","emoji":"x"}},"at":0}"#
            .to_vec(),
    });
    let event = event_of("room-1", "rec-1", "chat.workspace", "ab", &context);
    assert_eq!(event.arguments["emoji"], "x");
    // A verb whose own argument is named `op` keeps it.
    context.action = Some(EncodedAction {
        type_id: "discord.guild".into(),
        payload: br#"{"action":"send","args":{"op":{"channel":"1"}}}"#.to_vec(),
    });
    let event = event_of("room-1", "rec-1", "discord.guild", "ab", &context);
    assert_eq!(event.arguments["op"]["channel"], "1");
    assert_eq!(
        event.started_run().map(|run| run.0.to_string()).as_deref(),
        Some("6f1c2a8e-3b4d-4c5e-9f60-718293a4b5c6")
    );
}
