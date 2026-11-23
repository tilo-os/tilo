use serde_json::{Map, Value, json};
use tilo_actions::Acting;
use tilo_actions::ActionKey;
use tilo_actions::entry::{Did, EditOp, FieldEdit, GroupOf, PathStep, VerbCall};
use tilo_actions::params::{ActionParam, ParamType};

use super::*;
use crate::shortcut::{ActorFilter, Shortcut, TriggerSpec};

/// An app id with its parts separated by no literal separator: the test
/// process must not depend on how one is written.
const APP: &str = "so.tilo.mail";
const OTHER_APP: &str = "so.tilo.calendar";

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

fn post(actor: Acting, change: &str, caused_by: Option<Cause>) -> DocumentEvent {
    DocumentEvent {
        workspace: "ws-1".into(),
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
        origin: None,
        incoming_message: true,
        housekeeping: false,
        shortcut: None,
        invocation: None,
    }
}

fn person(id: &str) -> Acting {
    Acting::person(id)
}

fn bot(id: &str) -> Acting {
    Acting::bot(id).acting_for(Acting::person("ada"))
}

fn watch_chat(by: ActorFilter) -> TriggerSpec {
    TriggerSpec::ResourceChanged {
        kind: "chat.workspace".into(),
        actions: vec![],
        fields: vec![],
        by,
        filter: crate::shortcut::ChangeFilter::default(),
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

tilo_test_env::scenario! {
    fn a_firing_carries_the_event_that_fired_it;

    phase a_post_by_a_person_fires_the_bot_with_the_event_bound;
    {
        let shortcut = program(vec![watch_chat(ActorFilter::Anyone)]);
        let event = post(person("bob"), &"a".repeat(64), None);
        let out = dispatch(&as_bot("b1", &shortcut), Event::Record(&event));
        assert_eq!(out.firings.len(), 1);
        let firing = &out.firings[0];
        assert_eq!(firing.firer, "bot:b1");
        assert_eq!(firing.inputs["event"]["after"]["text"], "hi");
        assert_eq!(
        firing.inputs["event"]["actor"]["by"]["member"]["person"],
        "bob"
    );
        assert_eq!(
            firing.cause,
            Some(Cause {
                installation: "b1".into(),
                event: "a".repeat(64),
                depth: 1
            })
        );
    }

    phase a_program_without_an_event_input_still_fires_with_no_inputs;
    {
        let mut shortcut = program(vec![watch_chat(ActorFilter::Anyone)]);
        shortcut.inputs.clear();
        let event = post(person("bob"), &"5".repeat(64), None);
        let out = dispatch(&as_bot("b1", &shortcut), Event::Record(&event));
        assert!(out.firings[0].inputs.is_empty());
    }

    phase an_entry_reads_as_the_event_every_host_sees;
    {
        use tilo_actions::{ActionContext, ActionFacet, ActionInvoker, Described, EncodedAction};
        let mut context = ActionContext::for_app("so.tilo.chat", "Chat")
            .with_invoker(ActionInvoker::person("bob"))
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
        let event = event_of("ws-1", "rec-1", "chat.workspace", "ab", &context);
        assert_eq!(event.actor, Acting::person("bob"));
        assert!(event.incoming_message);
        assert_eq!(event.shortcut.as_deref(), Some("tidy"));
        assert_eq!(event.arguments["text"], "hi");
        assert_eq!(verbs_of(&event.did), vec!["post"]);
        // As an action API records it: the operation in an envelope with its time.
        context.action = Some(EncodedAction {
            type_id: "chat.workspace".into(),
            payload: br#"{"op":{"action":"react","args":{"message":"m1","emoji":"x"}},"at":0}"#
                .to_vec(),
        });
        let event = event_of("ws-1", "rec-1", "chat.workspace", "ab", &context);
        assert_eq!(event.arguments["emoji"], "x");
        // A verb whose own argument is named `op` keeps it.
        context.action = Some(EncodedAction {
            type_id: "discord.guild".into(),
            payload: br#"{"action":"send","args":{"op":{"channel":"1"}}}"#.to_vec(),
        });
        let event = event_of("ws-1", "rec-1", "discord.guild", "ab", &context);
        assert_eq!(event.arguments["op"]["channel"], "1");
        assert_eq!(
            event.started_invocation().map(|run| run.0.to_string()).as_deref(),
            Some("6f1c2a8e-3b4d-4c5e-9f60-718293a4b5c6")
        );
    }
}

#[test]
fn housekeeping_fires_only_a_trigger_that_names_its_action() {
    let mut read = post(person("bob"), &"h".repeat(64), None);
    read.did = verb("mark-read");
    read.housekeeping = true;
    let any_change = program(vec![watch_chat(ActorFilter::Anyone)]);
    assert!(
        dispatch(&as_bot("b1", &any_change), Event::Record(&read))
            .firings
            .is_empty(),
        "marking a channel read changes nothing the record holds"
    );
    let reads = program(vec![TriggerSpec::ResourceChanged {
        kind: "chat.workspace".into(),
        actions: vec!["mark-read".into()],
        fields: vec![],
        by: ActorFilter::Anyone,
        filter: crate::shortcut::ChangeFilter::default(),
    }]);
    assert_eq!(
        dispatch(&as_bot("b1", &reads), Event::Record(&read))
            .firings
            .len(),
        1
    );
}

tilo_test_env::scenario! {
    fn an_invocation_never_fires_itself_again;

    phase a_bot_never_fires_on_its_own_change;
    {
        let shortcut = program(vec![watch_chat(ActorFilter::Anyone)]);
        let event = post(bot("b1"), &"b".repeat(64), None);
        let out = dispatch(&as_bot("b1", &shortcut), Event::Record(&event));
        assert!(out.firings.is_empty());
        assert_eq!(
            out.skipped,
            vec![("bot:b1".to_string(), 0, Skipped::SelfTriggered)]
        );
    }

    phase a_shortcut_never_fires_on_a_change_its_own_run_made;
    {
        let shortcut = program(vec![watch_chat(ActorFilter::Anyone)]);
        // A Shortcut's run acts as its person; the invocation's `shortcut` facet is
        // what names it.
        let mut event = post(
            Acting::automation("ada").acting_for(Acting::person("ada")),
            &"c".repeat(64),
            None,
        );
        event.shortcut = Some("bridge".into());
        let out = dispatch(&as_shortcut(&shortcut), Event::Record(&event));
        assert!(out.firings.is_empty());
        assert_eq!(out.skipped[0].2, Skipped::SelfTriggered);
        // Another Shortcut's run is somebody else's change, but it is still a
        // invocation's: firing on it would run two shortcuts into each other.
        event.shortcut = Some("tidy".into());
        let out = dispatch(&as_shortcut(&shortcut), Event::Record(&event));
        assert!(out.firings.is_empty());
        assert_eq!(
            out.skipped[0].2,
            Skipped::Loop {
                through: "tidy".into()
            }
        );
        // A person's own change still fires it, and records no cause.
        let typed = post(person("bob"), &"e".repeat(64), None);
        let out = dispatch(&as_shortcut(&shortcut), Event::Record(&typed));
        assert_eq!(out.firings.len(), 1);
        assert_eq!(out.firings[0].cause, None, "a Shortcut records no cause");
    }

    phase a_chain_stops_after_four_hops;
    {
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

    phase the_depth_of_a_bot_change_comes_from_the_run_not_the_claim;
    {
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
}

#[test]
fn the_same_change_gives_the_same_invocation_id() {
    let shortcut = program(vec![
        watch_chat(ActorFilter::Anyone),
        watch_chat(ActorFilter::People),
    ]);
    let event = post(person("bob"), &"f".repeat(64), None);
    let first = dispatch(&as_bot("b1", &shortcut), Event::Record(&event));
    let again = dispatch(&as_bot("b1", &shortcut), Event::Record(&event));
    assert_eq!(first.firings[0].invocation, again.firings[0].invocation);
    // Each trigger index is its own invocation.
    assert_ne!(first.firings[0].invocation, first.firings[1].invocation);
    // Another installation of the same program is another invocation.
    assert_ne!(
        dispatch(&as_bot("b2", &shortcut), Event::Record(&event)).firings[0].invocation,
        first.firings[0].invocation
    );
}

tilo_test_env::scenario! {
    fn a_trigger_matches_only_the_changes_it_watches;

    phase actor_filters_select_whose_changes_count;
    {
        let people = program(vec![watch_chat(ActorFilter::People)]);
        let not_bots = program(vec![watch_chat(ActorFilter::NotBots)]);
        let agent = Acting::agent("a").acting_for(Acting::person("ada"));
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

    phase verbs_and_field_patterns_narrow_a_record_trigger;
    {
        assert!(field_matches("channels.*.name", "channels.c1.name"));
        assert!(!field_matches("channels.*.name", "channels.c1.topic"));
        assert!(field_matches("channels", "channels.c1.name"));
        assert!(!field_matches("channels.*.name", "channels.c1"));
        let only_edits = program(vec![TriggerSpec::ResourceChanged {
            kind: "chat.workspace".into(),
            actions: vec!["edit-message".into()],
            fields: vec![],
            by: ActorFilter::Anyone,
            filter: crate::shortcut::ChangeFilter::default(),
        }]);
        let event = post(person("bob"), &"1".repeat(64), None);
        assert!(
            dispatch(&as_bot("b1", &only_edits), Event::Record(&event))
                .firings
                .is_empty()
        );
        let other_kind = program(vec![TriggerSpec::ResourceChanged {
            kind: "discord.guild".into(),
            actions: vec![],
            fields: vec![],
            by: ActorFilter::Anyone,
            filter: crate::shortcut::ChangeFilter::default(),
        }]);
        assert!(
            dispatch(&as_bot("b1", &other_kind), Event::Record(&event))
                .firings
                .is_empty()
        );

        let names = program(vec![TriggerSpec::ResourceChanged {
            kind: "chat.workspace".into(),
            actions: vec![],
            fields: vec!["channels.*.name".into()],
            by: ActorFilter::Anyone,
            filter: crate::shortcut::ChangeFilter::default(),
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

    phase incoming_message_matches_only_flagged_verbs_by_someone_else;
    {
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

    phase a_connector_event_fires_by_source_and_filter;
    {
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

    phase an_app_declaring_an_event_fires_the_trigger_watching_its_kind;
    {
        let shortcut = program(vec![TriggerSpec::AppEvent {
            app_id: APP.into(),
            kind: "mail.message".into(),
        }]);
        // The event's kind carries the app that declared it, and the change is
        // that app's own verb, so a second app cannot announce it.
        let announced = |kind: &str| {
            let mut change = post(person("bob"), &"7".repeat(64), None);
            change.kind = format!("{APP}.{kind}.event");
            change.did = Did::Verb(VerbCall::new(ActionKey::new(APP, kind, "received"), []));
            change.arguments = Map::from_iter([
                ("subject".to_string(), json!("Invoice")),
                ("from".to_string(), json!("bob")),
            ]);
            change.incoming_message = false;
            AppEvent::of_change(&change).expect("a declaration")
        };
        let event = announced("mail.message");
        assert_eq!(event.app_id, APP);
        assert_eq!(event.kind, "mail.message");
        assert_eq!(event.workspace, "ws-1");
        let out = dispatch(&as_shortcut(&shortcut), Event::AppEvent(&event));
        assert_eq!(out.firings.len(), 1);
        assert_eq!(
            out.firings[0].inputs["event"],
            json!({
                "app_id": APP,
                "kind": "mail.message",
                "payload": {"subject": "Invoice", "from": "bob"},
            })
        );
        // The same declaration twice is one event, so one invocation.
        assert_eq!(
            out.firings[0].invocation,
            dispatch(
                &as_shortcut(&shortcut),
                Event::AppEvent(&announced("mail.message"))
            )
            .firings[0]
                .invocation
        );
        // Another app's change of the same kind is no declaration at all.
        let mut foreign = post(person("bob"), &"9".repeat(64), None);
        foreign.kind = format!("{APP}.mail.message.event");
        foreign.did = Did::Verb(VerbCall::new(
            ActionKey::new(OTHER_APP, "mail.message", "received"),
            [],
        ));
        assert_eq!(AppEvent::of_change(&foreign), None);
        let mut plain = post(person("bob"), &"8".repeat(64), None);
        plain.kind = "mail.mail.message".into();
        assert_eq!(AppEvent::of_change(&plain), None, "no event announced");
        plain.kind = format!("{APP}.mail.message.event");
        assert_eq!(
            AppEvent::of_change(&plain),
            None,
            "the change is not the declaring app's verb"
        );
        plain.did = Did::Verb(VerbCall::new(
            ActionKey::new(APP, "mail.message", "received"),
            [],
        ));
        plain.kind = ".mail.message.event".into();
        assert_eq!(AppEvent::of_change(&plain), None, "no app named the event");
        plain.kind = "mail.message.event".into();
        assert_eq!(
            AppEvent::of_change(&plain),
            None,
            "no separator, so no app named the event"
        );
        plain.kind = format!("{APP}.mail.message.event.more");
        assert_eq!(
            AppEvent::of_change(&plain),
            None,
            "a kind that does not end as an event announces none"
        );
    }

    phase group_changes_count_every_member_verb_and_edit;
    {
        let group = Did::Group(GroupOf {
            members: vec![verb("rename-channel"), verb("post")],
            count: 3,
            distinct: vec![verb("post"), verb("pin")],
        });
        assert_eq!(verbs_of(&group), vec!["rename-channel", "post", "pin"]);
    }
}

tilo_test_env::scenario! {
    fn a_fire_time_fires_the_trigger_it_came_round_for;

    phase a_moment_fires_its_own_trigger_as_the_same_run_however_often_it_comes_round;
    {
        let shortcut = program(vec![
            TriggerSpec::Schedule {
                schedule: crate::shortcut::Schedule::Interval { seconds: 60 },
                catch_up: crate::shortcut::CatchUp::Skip,
            },
            TriggerSpec::Once { at_ms: 90_000 },
        ]);
        let moment = ScheduleEvent {
            workspace: "ws-1".into(),
            trigger: 1,
            due_ms: 90_000,
        };
        let out = dispatch(&as_bot("b1", &shortcut), Event::Schedule(&moment));
        assert_eq!(out.firings.len(), 1, "only the trigger whose time came: {out:?}");
        let firing = &out.firings[0];
        assert_eq!(firing.trigger, TriggerSpec::Once { at_ms: 90_000 });
        assert_eq!(firing.inputs["event"]["due_ms"], 90_000);
        // A host that restarted before it kept the answer hands the moment
        // over again: the run is the same one, so its steps act once.
        let again = dispatch(&as_bot("b1", &shortcut), Event::Schedule(&moment));
        assert_eq!(again.firings[0].invocation, firing.invocation);
    }

    phase a_schedule_fires_on_its_own_fire_time_and_other_triggers_on_none;
    {
        let shortcut = program(vec![
            TriggerSpec::Schedule {
                schedule: crate::shortcut::Schedule::Interval { seconds: 60 },
                catch_up: crate::shortcut::CatchUp::Skip,
            },
            TriggerSpec::Once { at_ms: 90_000 },
            watch_chat(ActorFilter::Anyone),
        ]);
        let tick = ScheduleEvent {
            workspace: "ws-1".into(),
            trigger: 0,
            due_ms: 120_000,
        };
        let out = dispatch(&as_bot("b1", &shortcut), Event::Schedule(&tick));
        assert_eq!(out.firings.len(), 1, "{out:?}");
        assert_eq!(out.firings[0].trigger_index, 0);
        // A fire time named for a trigger that waits for something else.
        let stray = ScheduleEvent {
            workspace: "ws-1".into(),
            trigger: 2,
            due_ms: 120_000,
        };
        assert!(
            dispatch(&as_bot("b1", &shortcut), Event::Schedule(&stray))
                .firings
                .is_empty()
        );
    }

    phase something_new_in_a_source_fires_the_trigger_it_was_found_for_once;
    {
        let shortcut = program(vec![
            watch_chat(ActorFilter::Anyone),
            TriggerSpec::SourceChanged {
                kind: crate::source_change::PERSON_MAIL.into(),
                at: String::new(),
                every_secs: None,
                filter: Map::new(),
            },
        ]);
        let found = SourceEvent {
            workspace: "ws-1".into(),
            trigger: 1,
            key: "ada@example.com:7:4".into(),
            payload: Map::from_iter([("item".to_string(), json!("ada@example.com:7:4"))]),
        };
        let out = dispatch(&as_bot("b1", &shortcut), Event::Source(&found));
        assert_eq!(out.firings.len(), 1, "only the trigger it was found for: {out:?}");
        let firing = &out.firings[0];
        assert_eq!(firing.trigger_index, 1);
        assert_eq!(firing.inputs["event"]["item"], "ada@example.com:7:4");
        // Found again, it is the same invocation, so its steps act once.
        let again = dispatch(&as_bot("b1", &shortcut), Event::Source(&found));
        assert_eq!(again.firings[0].invocation, firing.invocation);
        // Another thing found there is another invocation.
        let next = SourceEvent {
            key: "ada@example.com:7:5".into(),
            ..found.clone()
        };
        let other = dispatch(&as_bot("b1", &shortcut), Event::Source(&next));
        assert_eq!(other.firings.len(), 1, "{other:?}");
        assert_ne!(other.firings[0].invocation, firing.invocation);
        // Found for a trigger that waits for something else: nothing.
        let stray = SourceEvent { trigger: 0, ..found };
        assert!(
            dispatch(&as_bot("b1", &shortcut), Event::Source(&stray))
                .firings
                .is_empty()
        );
    }
}

/// What an agent's own triggers watch, as its person saves them: a message that mentions it, a
/// message in its direct conversation, a task assigned to it and a message that arrived in the
/// workspace's mailbox, each a change of the record's kind narrowed by the change's own fields.
/// Neither its own answers nor a copy another writer mirrors back of one wakes it, and a message
/// carried in for someone who is no member does.
#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one scenario, end to end, reads as one test"
)]
fn a_filter_narrows_a_change_by_what_it_holds_and_where_it_came_from() {
    use crate::shortcut::{ChangeFilter, Test};
    let not_its_own = || {
        [
            ("by", Test::IsNot(json!("agent:scout"))),
            ("origin", Test::IsNot(json!("agent:scout"))),
        ]
    };
    let watch = |kind: &str, field: &str, test: Test| TriggerSpec::ResourceChanged {
        kind: kind.into(),
        actions: vec![],
        fields: vec![],
        by: ActorFilter::Anyone,
        filter: ChangeFilter::of(not_its_own().into_iter().chain([(field, test)])),
    };
    let mention = watch(
        "chat.workspace",
        "arguments.body",
        Test::Has(json!("agent:scout")),
    );
    let direct = watch("chat.direct", "item.person", Test::Is(json!("agent:scout")));
    let assigned = watch(
        "tasks.issues",
        "item.assignee",
        Test::Is(json!("agent:scout")),
    );
    let arrived = watch(
        "tl.so.tilo.mail.Message",
        "incoming_message",
        Test::Is(json!(true)),
    );
    let shortcut = program(vec![mention, direct, assigned, arrived]);
    let fired = |event: &DocumentEvent| {
        dispatch(&as_shortcut(&shortcut), Event::Record(event))
            .firings
            .iter()
            .map(|firing| firing.trigger_index)
            .collect::<Vec<_>>()
    };
    let said = |actor: Acting, kind: &str, body: Value, item: Value| DocumentEvent {
        kind: kind.into(),
        arguments: Map::from_iter([("body".to_owned(), body)]),
        after: Some(item),
        ..post(actor, &"c".repeat(64), None)
    };

    let mentioned = said(
        person("bob"),
        "chat.workspace",
        json!({"rich": [{"text": "hi "}, {"mention": "agent:scout"}]}),
        json!({}),
    );
    assert_eq!(fired(&mentioned), [0], "a mention anywhere in the body");
    let unmentioned = said(person("bob"), "chat.workspace", json!("hi all"), json!({}));
    assert!(fired(&unmentioned).is_empty(), "a message naming nobody");
    let in_direct = said(
        person("bob"),
        "chat.direct",
        json!("hi"),
        json!({"person": "agent:scout"}),
    );
    assert_eq!(
        fired(&in_direct),
        [1],
        "a message in its direct conversation"
    );
    let others_direct = said(
        person("bob"),
        "chat.direct",
        json!("hi"),
        json!({"person": "ada"}),
    );
    assert!(
        fired(&others_direct).is_empty(),
        "someone else's direct message"
    );
    let given = said(
        person("ada"),
        "tasks.issues",
        Value::Null,
        json!({"assignee": "agent:scout", "title": "Tidy"}),
    );
    assert_eq!(fired(&given), [2], "a task assigned to it");
    let received = said(
        person("bob"),
        "tl.so.tilo.mail.Message",
        Value::Null,
        json!({"subject": "Hello"}),
    );
    assert_eq!(fired(&received), [3], "a message the workspace received");
    let written = said(
        person("bob"),
        "tl.so.tilo.mail.Message",
        Value::Null,
        json!({"subject": "Hello"}),
    );
    assert!(
        fired(&DocumentEvent {
            incoming_message: false,
            ..written
        })
        .is_empty(),
        "a message written here, that nobody received"
    );

    // Its own answer in its direct conversation, and a copy of it mirrored back by whatever
    // carries the conversation elsewhere, fire nothing.
    let answer = said(
        Acting::agent("scout").acting_for(person("bob")),
        "chat.direct",
        json!("done: agent:scout here"),
        json!({"person": "agent:scout"}),
    );
    assert!(fired(&answer).is_empty(), "its own answer");
    let mirrored = DocumentEvent {
        origin: Some("agent:scout".into()),
        ..said(
            Acting::by_app("so.tilo.relay"),
            "chat.workspace",
            json!({"rich": [{"mention": "agent:scout"}]}),
            json!({}),
        )
    };
    assert!(fired(&mirrored).is_empty(), "a copy of its own answer");

    // A message carried in for someone outside the workspace is matched on the record and what
    // it says, not on a member author.
    let outside = said(
        Acting::by_app("so.tilo.relay"),
        "chat.workspace",
        json!({"rich": [{"text": "@scout?"}, {"mention": "agent:scout"}], "by": "Visitor"}),
        json!({}),
    );
    assert_eq!(fired(&outside), [0], "an outside author's mention");
}

#[test]
fn a_change_filter_reads_back_from_its_text() {
    use crate::shortcut::{ChangeFilter, Test};
    let trigger = TriggerSpec::ResourceChanged {
        kind: "chat.direct".into(),
        actions: vec!["send".into()],
        fields: vec![],
        by: ActorFilter::Anyone,
        filter: ChangeFilter::of([
            ("by", Test::IsNot(json!("agent:scout"))),
            ("item.person", Test::Is(json!("agent:scout"))),
            ("arguments.body", Test::Has(json!("scout"))),
            ("origin", Test::Lacks(json!("agent:"))),
            ("incoming_message", Test::Is(json!(true))),
        ]),
    };
    let text = crate::text::trigger_line(&trigger);
    assert_eq!(
        crate::text::trigger_of_line(&text),
        Ok(trigger),
        "the filter reads back from `{text}`"
    );
}
