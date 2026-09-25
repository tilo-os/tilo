use super::super::{CallPhaseV1, CallV1, DirectionsV1, MediaPolicyError};
use super::*;
use crate::native_footprint_vendor_tests::assert_precharged;
use crate::resources::native::NativeBatch;
use automerge::resource_limits::{NativeAllocationSet, NativeAllowance};
use automerge::{
    ActorId, AutoCommit, ObjType, ROOT, ReadDoc, ScalarValue, Value, transaction::Transactable,
};
use std::collections::BTreeMap;
fn fixture() -> AutoCommit {
    let mut doc = AutoCommit::new();
    doc.set_actor(ActorId::from(vec![17; 32]));
    let channels = doc.put_object(ROOT, "channels", ObjType::Map).unwrap();
    let voice = doc.put_object(&channels, "voice", ObjType::Map).unwrap();
    doc.put(&voice, "kind", "voice").unwrap();
    doc.put(&voice, "workspace", "main").unwrap();
    doc.commit();
    doc
}
fn context() -> (NativeAllowance, NativeAllocationSet, NativeWorkTurn) {
    let allowance = NativeAllowance::new(64 * 1024 * 1024).unwrap();
    let registry = NativeAllocationSet::new(allowance.clone()).unwrap();
    let turn = registry.work_turn(allowance.clone()).unwrap();
    (allowance, registry, turn)
}
fn stored(
    doc: &mut AutoCommit,
    registry: &mut NativeAllocationSet,
    turn: &NativeWorkTurn,
) -> NativeDocument {
    NativeBatch::new(registry, turn, 8 * 1024 * 1024)
        .unwrap()
        .load(&doc.save())
        .unwrap()
}
#[test]
fn media_binding_retains_exact_native_object_strings_after_document_drop() {
    let mut doc = fixture();
    let expected = historical_resolve("so.tilo.chat", "tab:voice", "voice", &doc).unwrap();
    let (allowance, mut registry, turn) = context();
    let native = stored(&mut doc, &mut registry, &turn);
    let binding = assert_precharged(&allowance, || {
        resolve_native("so.tilo.chat", "tab:voice", "voice", &native, &turn)
    })
    .unwrap();
    assert!(binding.matches(&expected));
    drop(native);
    drop(turn);
    drop(registry);
    drop(doc);
    assert!(binding.matches(&expected));
    let before = allowance.available_bytes();
    drop(binding);
    assert!(allowance.available_bytes() > before);
}
#[test]
fn media_binding_matches_original_archival_wrong_type_conflict_and_route_errors() {
    for mode in 0..7 {
        let mut doc = fixture();
        let channels = doc.get(ROOT, "channels").unwrap().unwrap().1;
        let voice = doc.get(&channels, "voice").unwrap().unwrap().1;
        match mode {
            0 => {
                doc.put(&voice, "archived", true).unwrap();
            }
            1 => {
                doc.put(&voice, "kind", "text").unwrap();
            }
            2 => {
                doc.put(&voice, "workspace", true).unwrap();
            }
            3 => {
                doc.delete(&channels, "voice").unwrap();
            }
            4 => {
                let mut fork = doc.fork();
                doc.put(&voice, "archived", false).unwrap();
                doc.commit();
                fork.put(&voice, "archived", false).unwrap();
                fork.commit();
                doc.merge(&mut fork).unwrap();
            }
            5 => {
                doc.put(&voice, "archived", false).unwrap();
            }
            _ => {}
        }
        doc.commit();
        let app = if mode == 6 {
            "wrong.app"
        } else {
            "so.tilo.chat"
        };
        let expected = historical_resolve(app, "", "voice", &doc);
        let (allowance, mut registry, turn) = context();
        let native = stored(&mut doc, &mut registry, &turn);
        let actual = assert_precharged(&allowance, || {
            resolve_native(app, "", "voice", &native, &turn)
        });
        match expected {
            Ok(expected) => assert!(actual.unwrap().matches(&expected)),
            Err(error) => assert!(matches!(actual,Err(Error::Policy(actual)) if actual==error)),
        }
    }
}
#[test]
fn media_binding_active_exact_route_rejects_replaced_object_but_ignores_ended_unrelated_calls() {
    let mut doc = fixture();
    let binding = historical_resolve("so.tilo.chat", "", "voice", &doc).unwrap();
    let mut policy = RoomMediaV1 {
        version: 1,
        room: [1; 16],
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
    };
    let channels = doc.get(ROOT, "channels").unwrap().unwrap().1;
    let voice = doc.put_object(&channels, "voice", ObjType::Map).unwrap();
    doc.put(&voice, "kind", "voice").unwrap();
    doc.put(&voice, "workspace", "main").unwrap();
    doc.commit();
    let (allowance, mut registry, turn) = context();
    let native = stored(&mut doc, &mut registry, &turn);
    assert_eq!(
        assert_precharged(&allowance, || validate_native(
            &policy,
            "so.tilo.chat",
            "",
            &native,
            &turn
        )),
        Err(Error::Policy(MediaPolicyError::Binding))
    );
    assert_eq!(
        assert_precharged(&allowance, || validate_native(
            &policy,
            "so.tilo.chat",
            "other",
            &native,
            &turn
        )),
        Ok(())
    );
    policy.calls.values_mut().next().unwrap().phase = CallPhaseV1::Ended;
    assert_eq!(
        assert_precharged(&allowance, || validate_native(
            &policy,
            "so.tilo.chat",
            "",
            &native,
            &turn
        )),
        Ok(())
    );
}
#[test]
fn media_binding_rejects_actual_native_orphan_history_before_channel_lookup() {
    let mut doc = fixture();
    let before = doc.get_heads();
    let channels = doc.get(ROOT, "channels").unwrap().unwrap().1;
    let voice = doc.get(&channels, "voice").unwrap().unwrap().1;
    doc.put(&voice, "workspace", "later").unwrap();
    doc.commit();
    let child = doc.get_changes(&before)[0].raw_bytes().to_vec();
    let mut orphan = AutoCommit::new();
    orphan.load_incremental(&child).unwrap();
    assert!(!orphan.get_missing_deps(&[]).is_empty());
    assert_eq!(
        historical_resolve("so.tilo.chat", "", "voice", &orphan),
        Err(MediaPolicyError::Binding)
    );
    let (allowance, mut registry, turn) = context();
    let native = stored(&mut orphan, &mut registry, &turn);
    assert!(native.has_missing_dependencies());
    assert!(matches!(
        assert_precharged(&allowance, || resolve_native(
            "so.tilo.chat",
            "",
            "voice",
            &native,
            &turn
        )),
        Err(Error::Policy(MediaPolicyError::Binding))
    ));
}
#[test]
fn media_binding_exact_object_limit_preserves_workspace_error_precedence() {
    for (actor_bytes, wrong_workspace) in [(127, false), (128, false), (128, true)] {
        let mut doc = AutoCommit::new();
        doc.set_actor(ActorId::from(vec![17; actor_bytes]));
        let channels = doc.put_object(ROOT, "channels", ObjType::Map).unwrap();
        let voice = doc.put_object(&channels, "voice", ObjType::Map).unwrap();
        doc.put(&voice, "kind", "voice").unwrap();
        if wrong_workspace {
            doc.put(&voice, "workspace", true).unwrap();
        } else {
            doc.put(&voice, "workspace", "main").unwrap();
        }
        doc.commit();
        let expected = historical_resolve("so.tilo.chat", "", "voice", &doc);
        let (allowance, mut registry, turn) = context();
        let native = stored(&mut doc, &mut registry, &turn);
        let actual = assert_precharged(&allowance, || {
            resolve_native("so.tilo.chat", "", "voice", &native, &turn)
        });
        match expected {
            Ok(expected) => {
                assert_eq!(expected.channel_object.len(), 256);
                assert!(actual.unwrap().matches(&expected));
            }
            Err(error) => {
                assert_eq!(
                    error,
                    if wrong_workspace {
                        MediaPolicyError::Malformed
                    } else {
                        MediaPolicyError::Binding
                    }
                );
                assert!(matches!(actual,Err(Error::Policy(actual)) if actual==error));
            }
        }
    }
}
#[test]
fn media_binding_new_turn_quota_refuses_without_spending_document_residence_credit() {
    let mut doc = fixture();
    let (residence, mut registry, turn) = context();
    let native = stored(&mut doc, &mut registry, &turn);
    let residence_before = residence.available_bytes();
    let current = NativeAllowance::new(512).unwrap();
    let current_turn = registry.work_turn(current.clone()).unwrap();
    let before = current.available_bytes();
    let result = assert_precharged(&current, || {
        resolve_native("so.tilo.chat", "", "voice", &native, &current_turn)
    });
    assert!(matches!(result, Err(Error::Resource(_))));
    assert_eq!(residence.available_bytes(), residence_before);
    assert_eq!(current.available_bytes(), before);
    assert!(resolve_native("so.tilo.chat", "", "voice", &native, &turn).is_ok());
}

// Original public resolver retained as independent identity/shape oracle.
use super::super::{strict_map, strict_string};
fn historical_resolve(
    app: &str,
    record: &str,
    channel: &str,
    candidate: &impl ReadDoc,
) -> Result<ChatCallBindingV1, MediaPolicyError> {
    if !candidate.get_missing_deps(&[]).is_empty() {
        return Err(MediaPolicyError::Binding);
    }
    let channels = strict_map(candidate, &ROOT, "channels")?;
    let object = strict_map(candidate, &channels, channel)?;
    if strict_string(candidate, &object, "kind")? != "voice" {
        return Err(MediaPolicyError::Binding);
    }
    let archived = candidate
        .get_all(&object, "archived")
        .map_err(|_| MediaPolicyError::Malformed)?;
    if archived.len() > 1 || archived.iter().any(|(value,_)| !matches!(value, Value::Scalar(value) if matches!(value.as_ref(),ScalarValue::Boolean(false)))) {
        return Err(MediaPolicyError::Binding);
    }
    let binding = ChatCallBindingV1 {
        app_id: app.into(),
        record_id: record.into(),
        channel_id: channel.into(),
        channel_object: object.to_string(),
        chat_workspace_id: strict_string(candidate, &object, "workspace")?,
    };
    binding.validate()?;
    Ok(binding)
}
