use super::*;
use automerge::{ActorId, ObjType, Value};
use std::collections::{BTreeMap, BTreeSet};
use tilo_automerge::ApplyIncoming;

// Literal pre-extraction private checks and governance tuple, independent of
// the shared engine and its new borrowed certificate/streaming-hash adapters.
#[expect(
    clippy::similar_names,
    reason = "`root` and `room` are the domain's own names"
)]
#[expect(
    clippy::too_many_lines,
    reason = "the reference implementation, kept whole for parity"
)]
fn original(doc: &AutoCommit, peer: &crate::StatePeer) -> Result<(String, Vec<u64>), String> {
    let mut expiries = Vec::new();
    let roots = doc.get_all(ROOT, "grants").map_err(|e| e.to_string())?;
    if roots.len() > 1 {
        return Err("ambiguous private policy grants".into());
    }
    if let Some((value, object)) = roots.into_iter().next() {
        if !matches!(value, Value::Object(ObjType::Map)) {
            return Err("invalid private policy grants".into());
        }
        for key in doc.keys(&object) {
            let values = doc.get_all(&object, &key).map_err(|e| e.to_string())?;
            if values.len() != 1 {
                return Err("ambiguous private policy grant".into());
            }
            let (value, _) = values
                .into_iter()
                .next()
                .ok_or("missing private policy grant")?;
            let Value::Scalar(value) = value else {
                return Err("invalid private policy grant".into());
            };
            let ScalarValue::Bytes(bytes) = value.as_ref() else {
                return Err("invalid private policy grant".into());
            };
            let grant: tilo_action_policy::Grant =
                serde_json::from_slice(bytes).map_err(|_| "invalid private policy grant")?;
            match grant.lifetime {
                tilo_action_policy::Lifetime::Once | tilo_action_policy::Lifetime::Session(_) => {
                    return Err("nonpersistent governed private policy grant".into());
                }
                tilo_action_policy::Lifetime::Until(at) => expiries.push(at.div_ceil(1000)),
                _ => {}
            }
        }
    }
    expiries.sort_unstable();
    expiries.dedup();
    let owners = doc.get_all(ROOT, "owner").map_err(|e| e.to_string())?;
    let rooms = doc.get_all(ROOT, "room").map_err(|e| e.to_string())?;
    if owners.len() != 1 || rooms.len() != 1 || rooms[0].0.as_str() != Some(peer.room()) {
        return Err("ambiguous canonical room/owner".into());
    }
    let owner = owners[0].0.as_str().ok_or("invalid canonical owner")?;
    let expected = collab_types::caps::Principal::person_from_key(
        collab_types::Ed25519PublicKey::from_bytes(peer.authority().user_public_key),
    );
    if owner != expected.id() {
        return Err("owner differs from pinned authority root".into());
    }
    let mut grants = BTreeMap::new();
    let mut members = BTreeMap::new();
    let mut peers = BTreeMap::new();
    let mut identities = BTreeMap::new();
    for (field, kind) in [
        ("grants", 0),
        ("peers", 1),
        ("peer_identities_v1", 2),
        ("members", 3),
    ] {
        if let Ok(Some((Value::Object(_), object))) = doc.get(ROOT, field) {
            for key in doc.keys(&object) {
                if let Ok(Some((Value::Scalar(value), _))) = doc.get(&object, &key) {
                    match (kind, value.as_ref()) {
                        (0, ScalarValue::Bytes(bytes)) => {
                            grants.insert(key, bytes.clone());
                        }
                        (3, ScalarValue::Bytes(bytes)) => {
                            members.insert(key, bytes.clone());
                        }
                        (1, ScalarValue::Str(name)) => {
                            peers.insert(key, name.to_string());
                        }
                        (2, ScalarValue::Bytes(bytes)) => {
                            identities.insert(key, bytes.clone());
                        }
                        _ => (),
                    }
                }
            }
        }
    }
    let values = doc
        .get_all(ROOT, crate::room::media::FIELD)
        .map_err(|_| "unreadable governed media policy")?;
    let mut fingerprints = BTreeSet::new();
    let tag = u8::from(!values.is_empty());
    for (value, id) in values {
        let mut hash = blake3::Hasher::new();
        match value {
            Value::Scalar(value) => match value.as_ref() {
                ScalarValue::Bytes(bytes) => {
                    hash.update(b"bytes\0");
                    hash.update(bytes);
                }
                ScalarValue::Str(text) => {
                    hash.update(b"string\0");
                    hash.update(text.as_bytes());
                }
                value => {
                    hash.update(b"invalid-scalar\0");
                    hash.update(format!("{value:?}").as_bytes());
                }
            },
            Value::Object(kind) => {
                hash.update(b"invalid-object\0");
                hash.update(format!("{kind:?}").as_bytes());
                hash.update(&id.to_bytes());
            }
        }
        fingerprints.insert(*hash.finalize().as_bytes());
    }
    let media = (tag, fingerprints.into_iter().collect::<Vec<_>>());
    let bytes = postcard::to_allocvec(&(
        peer.room(),
        peer.authority(),
        owner.to_owned(),
        members,
        grants,
        peers,
        identities,
        media,
        peer.certificates(),
        peer.revocations(),
    ))
    .map_err(|e| e.to_string())?;
    let revision = blake3::Hasher::new_derive_key("Tilo agent governance revision v1")
        .update(&bytes)
        .finalize()
        .to_hex()
        .to_string();
    Ok((revision, expiries))
}
#[expect(
    clippy::panic_in_result_fn,
    reason = "test helper: asserts parity with the original before returning the value"
)]
fn compare(doc: &AutoCommit, peer: &crate::StatePeer) -> Result<(String, Vec<u64>), String> {
    let expected = original(doc, peer);
    let certificates = peer.certificates();
    let revocations = peer.revocations();
    let got = legacy(
        &LegacyRead(doc),
        Metadata {
            room: peer.room(),
            authority: peer.authority(),
            certificates: certificates.iter(),
            revocations: revocations.iter(),
        },
    )
    .map(|value| (value.revision, value.expiries))
    .map_err(Error::legacy);
    assert_eq!(got, expected);
    let result = native(
        &LegacyRead(doc),
        Metadata {
            room: peer.room(),
            authority: peer.authority(),
            certificates: certificates.iter(),
            revocations: revocations.iter(),
        },
        Limits {
            collections: 4 * 1024 * 1024,
            json: JsonLimits {
                allocation_bytes: 128 * 1024,
                collection_entries: 65536,
            },
        },
    );
    let got = result
        .map(|value| (value.revision.clone(), value.expiries))
        .map_err(Error::legacy);
    assert_eq!(got, expected);
    expected
}
#[test]
fn shared_private_policy_preserves_literal_malformed_and_error_precedence_matrix() {
    let peer = fixture();
    let base = peer.admitted_room().unwrap();
    for mode in 0..15 {
        let mut doc = base.clone();
        match mode {
            0 => {
                doc.delete(ROOT, "grants").unwrap();
            }
            1 => {
                doc.put(ROOT, "grants", false).unwrap();
            }
            2..=4 => {
                doc.put_object(
                    ROOT,
                    "grants",
                    [ObjType::List, ObjType::Text, ObjType::Table][mode - 2],
                )
                .unwrap();
            }
            5 => {
                let caps = doc.get(ROOT, "grants").unwrap().unwrap().1;
                doc.put(&caps, "0", "wrong-type").unwrap();
            }
            6 => {
                let caps = doc.get(ROOT, "grants").unwrap().unwrap().1;
                doc.put(&caps, "0", b"not-json".to_vec()).unwrap();
            }
            7..=8 => {
                let caps = doc.get(ROOT, "grants").unwrap().unwrap().1;
                let grant = editor_grant(peer.room()).with_lifetime(if mode == 7 {
                    tilo_action_policy::Lifetime::Session("session".into())
                } else {
                    tilo_action_policy::Lifetime::Once
                });
                doc.put(&caps, "0", serde_json::to_vec(&grant).unwrap())
                    .unwrap();
            }
            9 => {
                doc.put(ROOT, "owner", false).unwrap();
            }
            10 => {
                doc.put(ROOT, "room", false).unwrap();
                doc.put(ROOT, "owner", false).unwrap();
            }
            11 => {
                doc.put(ROOT, "owner", "different root").unwrap();
            }
            12 => {
                doc.delete(ROOT, "owner").unwrap();
            }
            13 => {
                doc.put(ROOT, "owner", false).unwrap();
                doc.put(ROOT, "grants", false).unwrap();
            }
            _ => {
                let caps = doc.get(ROOT, "grants").unwrap().unwrap().1;
                doc.put_object(&caps, "0", ObjType::Map).unwrap();
            }
        }
        doc.commit();
        let result = compare(&doc, &peer);
        if mode == 0 {
            assert!(result.is_ok());
        } else {
            assert!(result.is_err());
        }
        if mode == 13 {
            assert_eq!(result.unwrap_err(), "invalid private policy grants");
        }
    }
}
#[test]
fn shared_private_revision_preserves_ignored_fields_and_invalid_media_fingerprints() {
    let peer = fixture();
    let base = peer.admitted_room().unwrap();
    let expected = compare(base, &peer).unwrap();
    let mut renamed = base.clone();
    renamed.put(ROOT, "name", "ordinary rename").unwrap();
    renamed.commit();
    assert_eq!(compare(&renamed, &peer).unwrap(), expected);
    for field in ["peers", "peer_identities_v1"] {
        for kind in [ObjType::List, ObjType::Text, ObjType::Table] {
            let mut doc = base.clone();
            doc.put_object(ROOT, field, kind).unwrap();
            doc.commit();
            assert!(compare(&doc, &peer).is_ok());
        }
    }
    for kind in [ObjType::Map, ObjType::Table, ObjType::List, ObjType::Text] {
        let mut doc = base.clone().with_actor(ActorId::from(vec![77; 4096]));
        doc.put_object(ROOT, crate::room::media::FIELD, kind)
            .unwrap();
        doc.commit();
        assert_ne!(compare(&doc, &peer).unwrap(), expected);
    }
    for value in [
        ScalarValue::Counter(7.into()),
        ScalarValue::Boolean(false),
        ScalarValue::Unknown {
            type_code: 15,
            bytes: vec![23; 8192],
        },
    ] {
        let counter = matches!(value, ScalarValue::Counter(_));
        let mut doc = base.clone();
        doc.put(ROOT, crate::room::media::FIELD, value).unwrap();
        if counter {
            doc.increment(ROOT, crate::room::media::FIELD, 4).unwrap();
        }
        doc.commit();
        assert_ne!(compare(&doc, &peer).unwrap(), expected);
    }
}
#[test]
fn shared_private_policy_preserves_real_capability_and_owner_conflicts() {
    let peer = fixture();
    let base = peer.admitted_room().unwrap();
    for root in [true, false] {
        let mut a = base.clone().with_actor(ActorId::from(vec![41]));
        let mut b = base.clone().with_actor(ActorId::from(vec![42]));
        if root {
            a.put_object(ROOT, "grants", ObjType::Map).unwrap();
            b.put(ROOT, "grants", false).unwrap();
        } else {
            let caps = a.get(ROOT, "grants").unwrap().unwrap().1;
            a.put(&caps, "0", b"invalid".to_vec()).unwrap();
            b.put(&caps, "0", false).unwrap();
        }
        a.commit();
        b.commit();
        a.merge_incoming(&mut b).unwrap();
        a.put(ROOT, "owner", false).unwrap();
        a.commit();
        assert_eq!(
            compare(&a, &peer).unwrap_err(),
            if root {
                "ambiguous private policy grants"
            } else {
                "ambiguous private policy grant"
            }
        );
    }
    let mut a = base.clone().with_actor(ActorId::from(vec![51]));
    let mut b = base.clone().with_actor(ActorId::from(vec![52]));
    a.put(ROOT, "owner", "one").unwrap();
    b.put(ROOT, "owner", "two").unwrap();
    a.commit();
    b.commit();
    a.merge_incoming(&mut b).unwrap();
    assert_eq!(
        compare(&a, &peer).unwrap_err(),
        "ambiguous canonical room/owner"
    );
}
