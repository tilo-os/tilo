//! A call binds to the exact Chat voice channel it was made in, read from
//! the channel tree of an admitted document.

use super::super::{
    CallPhaseV1, CallV1, ChatCallBindingV1, DirectionsV1, MediaPolicyError, WorkspaceMediaV1,
    resolve_chat_binding, validate_bound_chat_change,
};
use automerge::{ActorId, AutoCommit, ObjType, ROOT, ReadDoc, transaction::Transactable};
use std::collections::BTreeMap;
use tilo_automerge::ApplyIncoming;

const APP: &str = "so.tilo.chat";
const DOCUMENT: &str = "tab:voice";

fn fixture() -> AutoCommit {
    let mut doc = AutoCommit::new();
    doc.set_actor(ActorId::from(vec![17; 32]));
    let channels = doc.put_object(ROOT, "channels", ObjType::Map).unwrap();
    let voice = {
        let entry = doc.put_object(&channels, "voice", ObjType::Map).unwrap();
        doc.put_object(&entry, "v", ObjType::Map).unwrap()
    };
    doc.put(&voice, "kind", "voice").unwrap();
    doc.put(&voice, "workspace", "main").unwrap();
    doc.commit();
    doc
}
fn voice_value(doc: &AutoCommit) -> automerge::ObjId {
    let channels = doc.get(ROOT, "channels").unwrap().unwrap().1;
    let entry = doc.get(&channels, "voice").unwrap().unwrap().1;
    doc.get(&entry, "v").unwrap().unwrap().1
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

    phase a_voice_channel_binds_its_object_and_the_workspace_it_once_named;
    {
        let doc = fixture();
        let bound = binding(&doc).unwrap();
        assert_eq!(bound.channel_id, "voice");
        assert_eq!(bound.channel_object, voice_value(&doc).to_string());
        assert_eq!(bound.chat_workspace_id, "main");
        bound.validate().unwrap();
    }

    phase a_channel_that_is_archived_or_not_voice_binds_nothing;
    {
        type Change = fn(&mut AutoCommit);
        let cases: Vec<(&str, Change)> = vec![
            ("archived", |doc| {
                let voice = voice_value(doc);
                doc.put(&voice, "archived", true).unwrap();
            }),
            ("another kind", |doc| {
                let voice = voice_value(doc);
                doc.put(&voice, "kind", "text").unwrap();
            }),
            ("not a channel map", |doc| {
                let channels = doc.get(ROOT, "channels").unwrap().unwrap().1;
                doc.put(&channels, "voice", "text").unwrap();
            }),
            ("no channel", |doc| {
                let channels = doc.get(ROOT, "channels").unwrap().unwrap().1;
                doc.delete(&channels, "voice").unwrap();
            }),
        ];
        for (what, change) in cases {
            let mut doc = fixture();
            change(&mut doc);
            doc.commit();
            assert_eq!(binding(&doc), Err(MediaPolicyError::Binding), "{what}");
        }
    }

    phase a_workspace_that_is_not_text_is_malformed_not_unbound;
    {
        let mut doc = fixture();
        let voice = voice_value(&doc);
        doc.put(&voice, "workspace", true).unwrap();
        doc.commit();
        assert_eq!(binding(&doc), Err(MediaPolicyError::Malformed));
    }

    phase a_channel_remade_keeps_its_call_but_one_that_stops_being_voice_loses_it;
    {
        let mut doc = fixture();
        let policy = policy_for(binding(&doc).unwrap());
        let channels = doc.get(ROOT, "channels").unwrap().unwrap().1;
        let voice = {
            let entry = doc.put_object(&channels, "voice", ObjType::Map).unwrap();
            doc.put_object(&entry, "v", ObjType::Map).unwrap()
        };
        doc.put(&voice, "kind", "voice").unwrap();
        doc.commit();
        assert_eq!(
            validate_bound_chat_change(&policy, APP, DOCUMENT, &doc),
            Ok(()),
            "the channel id names the call's channel, whatever object holds it"
        );
        doc.put(&voice, "kind", "text").unwrap();
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

    phase a_chat_that_named_no_workspace_binds_an_empty_one;
    {
        let mut doc = fixture();
        let earlier = binding(&doc).unwrap();
        assert_eq!(earlier.chat_workspace_id, "main");
        let voice = voice_value(&doc);
        doc.delete(&voice, "workspace").unwrap();
        doc.commit();
        let now = binding(&doc).unwrap();
        assert_eq!(now.chat_workspace_id, "");
        assert!(now.same_channel(&earlier));
        assert_eq!(
            validate_bound_chat_change(&policy_for(earlier), APP, DOCUMENT, &doc),
            Ok(()),
            "the call stays the channel's once its workspace is gone"
        );
    }

    phase two_devices_making_the_same_channel_bind_it_the_same;
    {
        let mut first = AutoCommit::new();
        first.set_actor(ActorId::from(vec![17; 32]));
        let mut second = AutoCommit::new();
        second.set_actor(ActorId::from(vec![18; 32]));
        for doc in [&mut first, &mut second] {
            let channels = doc.put_object(ROOT, "channels", ObjType::Map).unwrap();
            let entry = doc.put_object(&channels, "voice", ObjType::Map).unwrap();
            let voice = doc.put_object(&entry, "v", ObjType::Map).unwrap();
            doc.put(&voice, "kind", "voice").unwrap();
            doc.put(&voice, "archived", false).unwrap();
            doc.commit();
        }
        let mut merged_first = first.fork();
        merged_first.merge_incoming(&mut second.fork()).unwrap();
        let mut merged_second = second.fork();
        merged_second.merge_incoming(&mut first.fork()).unwrap();
        let on_first = binding(&merged_first).unwrap();
        let on_second = binding(&merged_second).unwrap();
        assert_eq!(on_first, on_second);
    }

    phase a_history_that_is_missing_its_dependencies_binds_nothing;
    {
        let mut doc = fixture();
        let before = doc.get_heads();
        let voice = voice_value(&doc);
        doc.put(&voice, "workspace", "later").unwrap();
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
            let mut doc = AutoCommit::new();
            doc.set_actor(ActorId::from(vec![17; actor_bytes]));
            let channels = doc.put_object(ROOT, "channels", ObjType::Map).unwrap();
            let entry = doc.put_object(&channels, "voice", ObjType::Map).unwrap();
            let voice = doc.put_object(&entry, "v", ObjType::Map).unwrap();
            doc.put(&voice, "kind", "voice").unwrap();
            doc.put(&voice, "workspace", "main").unwrap();
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
