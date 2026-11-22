//! A call binds to the exact Chat voice channel it was made in, read from the items of an
//! admitted Chat document.

use super::super::{
    CallPhaseV1, CallV1, ChatCallBindingV1, DirectionsV1, MediaPolicyError, WorkspaceMediaV1,
    resolve_chat_binding, validate_bound_chat_change,
};
use super::{CHAT_APP as APP, CONVERSATIONS, put_voice_channel};
use automerge::{ActorId, AutoCommit, ObjType, ROOT, ReadDoc, transaction::Transactable};
use std::collections::BTreeMap;
use tilo_automerge::ApplyIncoming;

const DOCUMENT: &str = "";

fn on(actor: Vec<u8>) -> AutoCommit {
    let mut doc = AutoCommit::new();
    doc.set_actor(ActorId::from(actor));
    doc
}
fn fixture() -> AutoCommit {
    let mut doc = on(vec![17; 32]);
    put_voice_channel(&mut doc, "voice", "Lounge");
    doc.commit();
    doc
}
fn items(doc: &AutoCommit) -> automerge::ObjId {
    let records = doc.get(ROOT, "tl").unwrap().unwrap().1;
    let app = doc.get(&records, APP).unwrap().unwrap().1;
    let resource = doc.get(&app, CONVERSATIONS).unwrap().unwrap().1;
    doc.get(&resource, "$items").unwrap().unwrap().1
}
fn voice_value(doc: &AutoCommit) -> automerge::ObjId {
    doc.get(items(doc), "voice").unwrap().unwrap().1
}
fn binding(doc: &AutoCommit) -> Result<ChatCallBindingV1, MediaPolicyError> {
    resolve_chat_binding(APP, DOCUMENT, "voice", doc)
}
fn policy_for(binding: ChatCallBindingV1) -> WorkspaceMediaV1 {
    WorkspaceMediaV1 {
        version: 1,
        workspace: [1; 16],
        authorization_revision: 1,
        authorization_basis: [1; 32],
        calls: BTreeMap::from([(
            [2; 32],
            CallV1 {
                binding,
                epoch: 1,
                phase: CallPhaseV1::Active,
                allowed: DirectionsV1 {
                    publish: true,
                    subscribe: true,
                },
                denied: BTreeMap::new(),
            },
        )]),
    }
}

tilo_test_env::scenario! {
    fn a_call_binds_its_exact_voice_channel_or_is_refused;

    phase a_voice_channel_binds_its_object;
    {
        let doc = fixture();
        let bound = binding(&doc).unwrap();
        assert_eq!(bound.app_id, APP);
        assert_eq!(bound.channel_id, "voice");
        assert_eq!(bound.channel_object, voice_value(&doc).to_string());
        assert_eq!(bound.chat_workspace_id, "");
        bound.validate().unwrap();
    }

    phase a_channel_that_is_removed_or_not_voice_binds_nothing;
    {
        type Change = fn(&mut AutoCommit);
        let cases: Vec<(&str, Change)> = vec![
            ("removed", |doc| {
                let voice = voice_value(doc);
                doc.put(&voice, "$at", r#"{"removed":true}"#).unwrap();
            }),
            ("in a removed category", |doc| {
                let items = items(doc);
                let category = doc.put_object(&items, "team", ObjType::Map).unwrap();
                doc.put(&category, "$at", r#"{"removed":true}"#).unwrap();
                let voice = voice_value(doc);
                doc.put(&voice, "$at", r#"{"in":"team","f":"channels","k":"a"}"#).unwrap();
            }),
            ("in a category that is not there", |doc| {
                let voice = voice_value(doc);
                doc.put(&voice, "$at", r#"{"in":"gone","f":"channels","k":"a"}"#).unwrap();
            }),
            ("a text channel", |doc| {
                let voice = voice_value(doc);
                doc.put(&voice, "$type", "q7llygsa").unwrap();
            }),
            ("not an item map", |doc| {
                let items = items(doc);
                doc.put(&items, "voice", "text").unwrap();
            }),
            ("no channel", |doc| {
                let items = items(doc);
                doc.delete(&items, "voice").unwrap();
            }),
            ("another app's", |doc| {
                let records = doc.get(ROOT, "tl").unwrap().unwrap().1;
                doc.delete(&records, APP).unwrap();
                let other = doc.put_object(&records, "so.tilo.lang.notes", ObjType::Map).unwrap();
                let resource = doc.put_object(&other, "notes", ObjType::Map).unwrap();
                let items = doc.put_object(&resource, "$items", ObjType::Map).unwrap();
                let item = doc.put_object(&items, "voice", ObjType::Map).unwrap();
                doc.put(&item, "$type", super::VOICE_CHANNEL_TYPE).unwrap();
            }),
        ];
        for (what, change) in cases {
            let mut doc = fixture();
            change(&mut doc);
            doc.commit();
            assert_eq!(binding(&doc), Err(MediaPolicyError::Binding), "{what}");
        }
        let mut inside = fixture();
        let items = items(&inside);
        let category = inside.put_object(&items, "team", ObjType::Map).unwrap();
        inside.put(&category, "$at", r#"{"f":"channels","k":"a"}"#).unwrap();
        let voice = voice_value(&inside);
        inside.put(&voice, "$at", r#"{"in":"team","f":"channels","k":"a"}"#).unwrap();
        inside.commit();
        assert!(binding(&inside).is_ok(), "a category's voice channel binds");
    }

    phase a_type_that_is_not_text_is_malformed_not_unbound;
    {
        let mut doc = fixture();
        let voice = voice_value(&doc);
        doc.put(&voice, "$type", true).unwrap();
        doc.commit();
        assert_eq!(binding(&doc), Err(MediaPolicyError::Malformed));
    }

    phase a_channel_remade_keeps_its_call_but_one_that_stops_being_voice_loses_it;
    {
        let mut doc = fixture();
        let policy = policy_for(binding(&doc).unwrap());
        let voice = put_voice_channel(&mut doc, "voice", "Lounge");
        doc.commit();
        assert_eq!(
            validate_bound_chat_change(&policy, APP, DOCUMENT, &doc),
            Ok(()),
            "the channel id names the call's channel, whatever object holds it"
        );
        doc.put(&voice, "$type", "q7llygsa").unwrap();
        doc.commit();
        assert_eq!(
            validate_bound_chat_change(&policy, APP, DOCUMENT, &doc),
            Err(MediaPolicyError::Binding)
        );
        assert_eq!(
            validate_bound_chat_change(&policy, APP, "tab:other", &doc),
            Ok(()),
            "another document's channels are not this call's"
        );
        let mut ended = policy_for(binding(&fixture()).unwrap());
        ended.calls.values_mut().next().unwrap().phase = CallPhaseV1::Ended;
        assert_eq!(
            validate_bound_chat_change(&ended, APP, DOCUMENT, &doc),
            Ok(()),
            "an ended call restricts nothing"
        );
    }

    phase two_devices_that_each_made_chat_bind_its_channels_the_same;
    {
        let mut first = on(vec![17; 32]);
        put_voice_channel(&mut first, "voice", "Lounge");
        first.commit();
        let mut second = on(vec![18; 32]);
        put_voice_channel(&mut second, "standup", "Standup");
        second.commit();
        let mut merged_first = first.fork();
        merged_first.merge_incoming(&mut second.fork()).unwrap();
        let mut merged_second = second.fork();
        merged_second.merge_incoming(&mut first.fork()).unwrap();
        for channel in ["voice", "standup"] {
            let on_first = resolve_chat_binding(APP, DOCUMENT, channel, &merged_first).unwrap();
            let on_second = resolve_chat_binding(APP, DOCUMENT, channel, &merged_second).unwrap();
            assert_eq!(on_first, on_second, "{channel}");
        }
    }

    phase a_history_that_is_missing_its_dependencies_binds_nothing;
    {
        let mut doc = fixture();
        let before = doc.get_heads();
        let voice = voice_value(&doc);
        doc.put(&voice, "name", "Later").unwrap();
        doc.commit();
        let child = doc.get_changes(&before)[0].raw_bytes().to_vec();
        let mut orphan = AutoCommit::new();
        orphan.load_incoming(&child).unwrap();
        assert!(!orphan.get_missing_deps(&[]).is_empty());
        assert_eq!(binding(&orphan), Err(MediaPolicyError::Binding));
    }

    phase a_channel_object_past_its_bound_binds_nothing;
    {
        for (actor_bytes, binds) in [(127, true), (128, false)] {
            let mut doc = on(vec![17; actor_bytes]);
            put_voice_channel(&mut doc, "voice", "Lounge");
            doc.commit();
            let bound = binding(&doc);
            if binds {
                let bound = bound.unwrap();
                assert_eq!(bound.channel_object.len(), 256);
                bound.validate().unwrap();
            } else {
                assert!(
                    matches!(bound, Err(MediaPolicyError::Binding)),
                    "an object name of its own past 256 bytes cannot be bound"
                );
            }
        }
    }
}
