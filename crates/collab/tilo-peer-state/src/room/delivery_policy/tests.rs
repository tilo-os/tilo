use super::*;
use automerge::{ObjType, ROOT, ReadDoc, ScalarValue, Value, transaction::Transactable};
use tilo_action_policy::{Answer, Grant, KindSet, Lifetime, Role, Subject, TreePath};
#[derive(Debug, PartialEq, Eq)]
enum Failure {
    Unavailable,
    CurrentDenied,
    Capacity,
    Decode,
}
fn failure(e: Error) -> Failure {
    match e {
        Error::Unavailable => Failure::Unavailable,
        Error::CurrentDenied => Failure::CurrentDenied,
        Error::Capacity => Failure::Capacity,
        Error::Decode(_) => Failure::Decode,
    }
}
// Literal winner/count/encoded-byte sorting/tuple oracle. Deliberately retains
// owned allocations here; production counting/hash helpers are unused.
#[expect(
    clippy::too_many_lines,
    reason = "the reference implementation, kept whole for parity"
)]
fn original_with_length(
    doc: &AutoCommit,
    peer: &crate::StatePeer,
    workspace: u64,
    device: &DeviceCert,
    host: &DeviceCert,
    permit: bool,
) -> Result<([u8; 32], usize), Failure> {
    let mut total = 0usize;
    let mut add = |n: usize| {
        total = total
            .checked_add(n)
            .filter(|n| *n <= 1024 * 1024)
            .ok_or(Failure::Capacity)?;
        Ok::<_, Failure>(())
    };
    let encode = |v: Vec<u8>| {
        if v.len() > 1024 * 1024 {
            Err(Failure::Capacity)
        } else {
            Ok(v)
        }
    };
    let governor = match doc.get(ROOT, "owner").map_err(|_| Failure::Unavailable)? {
        Some((Value::Scalar(v), _)) => {
            let text = v.as_str().ok_or(Failure::Unavailable)?;
            add(text.len())?;
            text.to_owned()
        }
        _ => return Err(Failure::Unavailable),
    };
    let person = device.user_public_key;
    let roots = [
        device.user_public_key,
        peer.authority().user_public_key,
        host.user_public_key,
    ];
    let devices = [
        device.device_public_key,
        peer.authority().device_public_key,
        host.device_public_key,
    ];
    let mut grants = vec![];
    if let Some((Value::Object(ObjType::Map), map)) =
        doc.get(ROOT, "grants").map_err(|_| Failure::Unavailable)?
    {
        for key in doc.keys(&map) {
            add(key.len())?;
            let Some((Value::Scalar(value), _)) =
                doc.get(&map, &key).map_err(|_| Failure::Unavailable)?
            else {
                return Err(Failure::Unavailable);
            };
            let ScalarValue::Bytes(bytes) = value.as_ref() else {
                return Err(Failure::Unavailable);
            };
            if bytes.len() > 1024 * 1024 {
                return Err(Failure::Capacity);
            }
            let grant: Grant = serde_json::from_slice(bytes).map_err(|_| Failure::Unavailable)?;
            if grant.node.room_id() != Some(peer.room())
                || !super::consumer_subject(&grant.subject, &person)
            {
                continue;
            }
            let encoded = encode(postcard::to_allocvec(&grant).unwrap())?;
            add(encoded.len())?;
            grants.push((key, encoded));
        }
    } else if doc
        .get(ROOT, "grants")
        .map_err(|_| Failure::Unavailable)?
        .is_some()
    {
        return Err(Failure::Unavailable);
    }
    grants.sort_unstable();
    let mut role = None;
    match doc.get(ROOT, "members").map_err(|_| Failure::Unavailable)? {
        Some((Value::Object(ObjType::Map), map)) => {
            if let Some((value, _)) = doc
                .get(&map, hex::encode(person))
                .map_err(|_| Failure::Unavailable)?
            {
                let Value::Scalar(value) = value else {
                    return Err(Failure::Unavailable);
                };
                let ScalarValue::Bytes(bytes) = value.as_ref() else {
                    return Err(Failure::Unavailable);
                };
                add(bytes.len())?;
                role = Some(bytes.clone());
            }
        }
        Some(_) => return Err(Failure::Unavailable),
        None => {}
    }
    let mut certificates = Vec::new();
    for cert in peer
        .certificates()
        .iter()
        .filter(|cert| roots.contains(&cert.user_public_key))
    {
        let bytes = encode(postcard::to_allocvec(&cert).unwrap())?;
        add(bytes.len())?;
        certificates.push(bytes);
    }
    certificates.sort_unstable();
    let mut revocations = Vec::new();
    for entry in peer
        .revocations()
        .iter()
        .filter(|entry| devices.contains(&entry.revoked_device_public_key))
    {
        let bytes = encode(postcard::to_allocvec(&entry).unwrap())?;
        add(bytes.len())?;
        revocations.push(bytes);
    }
    revocations.sort_unstable();
    if !permit {
        return Err(Failure::CurrentDenied);
    }
    let bytes = encode(
        postcard::to_allocvec(&(
            3_u8,
            peer.room(),
            workspace,
            peer.authority(),
            host,
            device,
            governor,
            certificates,
            revocations,
            grants,
            role,
        ))
        .unwrap(),
    )?;
    Ok((
        *blake3::Hasher::new()
            .update(b"tilo-delivery-v3-room-policy-v3\0")
            .update(&bytes)
            .finalize()
            .as_bytes(),
        bytes.len(),
    ))
}
fn original(
    doc: &AutoCommit,
    peer: &crate::StatePeer,
    workspace: u64,
    device: &DeviceCert,
    host: &DeviceCert,
    permit: bool,
) -> Result<[u8; 32], Failure> {
    original_with_length(doc, peer, workspace, device, host, permit).map(|v| v.0)
}
#[expect(
    clippy::panic_in_result_fn,
    reason = "test helper: asserts parity with the original before returning the value"
)]
fn compare(
    doc: &AutoCommit,
    peer: &crate::StatePeer,
    host: &DeviceCert,
    device: &DeviceCert,
    permit: bool,
) -> Result<[u8; 32], Failure> {
    let expected = original(doc, peer, 37, device, host, permit);
    let certs = peer.certificates();
    let revoked = peer.revocations();
    let meta = || Metadata {
        room: peer.room(),
        workspace: 37,
        authority: peer.authority(),
        host,
        device,
        certificates: certs.iter(),
        revocations: revoked.iter(),
    };
    let check = || {
        if permit {
            Ok(())
        } else {
            Err(Error::CurrentDenied)
        }
    };
    let got = legacy(&LegacyRead(doc), meta(), check).map_err(failure);
    assert_eq!(got, expected);
    let got = native(
        &LegacyRead(doc),
        meta(),
        Limits {
            collections: 8 * 1024 * 1024,
            json: JsonLimits {
                allocation_bytes: 2 * 1024 * 1024,
                collection_entries: 65536,
            },
        },
        check,
    )
    .map_err(failure);
    assert_eq!(got, expected);
    expected
}
#[test]
fn delivery_projection_preserves_exact_tuple_and_distinct_host_device_order() {
    let (peer, _, _, device, _) = crate::admission_tests::seeded();
    let mut host = peer.authority().clone();
    host.device_label = "distinct host label".into();
    tilo_user_identity::sign_device_cert(
        &mut host,
        &ed25519_dalek::SigningKey::from_bytes(&[1; 32]),
    )
    .unwrap();
    let doc = peer.admitted_room().unwrap();
    let digest = compare(doc, &peer, &host, &device, true).unwrap();
    let swapped = original(doc, &peer, 37, &host, &device, true).unwrap();
    assert_ne!(digest, swapped);
}
fn put_grant(doc: &mut AutoCommit, key: &str, grant: &Grant) {
    let grants = doc.get(ROOT, "grants").unwrap().unwrap().1;
    doc.put(
        &grants,
        key,
        ScalarValue::Bytes(serde_json::to_vec(grant).unwrap()),
    )
    .unwrap();
    doc.commit();
}
fn room_grant(room: &str, subject: Subject) -> Grant {
    Grant::new(
        "grant",
        subject,
        TreePath::room(room),
        KindSet::READ,
        Answer::Allow,
        Subject::Role(Role::Owner),
    )
}
#[test]
fn delivery_projection_preserves_nonpersistent_lifetimes_and_denial_order() {
    let (peer, _, _, device, _) = crate::admission_tests::seeded();
    let mut doc = peer.admitted_room().unwrap().clone();
    for lifetime in [Lifetime::Once, Lifetime::Session("session".into())] {
        let grant = room_grant(peer.room(), Subject::Role(Role::Editor)).with_lifetime(lifetime);
        put_grant(&mut doc, "extra", &grant);
        assert!(compare(&doc, &peer, peer.authority(), &device, true).is_ok());
    }
    let grants = doc.get(ROOT, "grants").unwrap().unwrap().1;
    doc.put(&grants, "extra", ScalarValue::Bytes(b"not JSON".to_vec()))
        .unwrap();
    doc.commit();
    assert_eq!(
        compare(&doc, &peer, peer.authority(), &device, false),
        Err(Failure::Unavailable)
    );
}

#[test]
fn delivery_view_excludes_other_peoples_grants_but_keeps_own_and_role_grants() {
    let (peer, _, _, device, _) = crate::admission_tests::seeded();
    let mut doc = peer.admitted_room().unwrap().clone();
    let base = compare(&doc, &peer, peer.authority(), &device, true).unwrap();
    let other = [0x5a; 32];
    put_grant(
        &mut doc,
        "other-person",
        &room_grant(peer.room(), Subject::Person(hex::encode(other))),
    );
    put_grant(
        &mut doc,
        "other-room",
        &room_grant(
            "bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb",
            Subject::Person(hex::encode(device.user_public_key)),
        ),
    );
    assert_eq!(
        compare(&doc, &peer, peer.authority(), &device, true).unwrap(),
        base,
        "another person's grants, and grants on another room, cannot change this consumer's view"
    );
    put_grant(
        &mut doc,
        "own-person",
        &room_grant(
            peer.room(),
            Subject::Person(hex::encode(device.user_public_key)),
        ),
    );
    let own = compare(&doc, &peer, peer.authority(), &device, true).unwrap();
    assert_ne!(
        own, base,
        "a grant addressed to this person is part of its view"
    );
    put_grant(
        &mut doc,
        "role",
        &room_grant(peer.room(), Subject::Role(Role::Viewer)),
    );
    let role = compare(&doc, &peer, peer.authority(), &device, true).unwrap();
    assert_ne!(role, own);
    // The consumer's own role is part of the view; another member's is not.
    let members = doc.get(ROOT, "members").unwrap().unwrap().1;
    doc.put(
        &members,
        hex::encode(other),
        ScalarValue::Bytes(serde_json::to_vec(&Role::Editor).unwrap()),
    )
    .unwrap();
    doc.commit();
    assert_eq!(
        compare(&doc, &peer, peer.authority(), &device, true).unwrap(),
        role
    );
    doc.put(
        &members,
        hex::encode(device.user_public_key),
        ScalarValue::Bytes(serde_json::to_vec(&Role::Commenter).unwrap()),
    )
    .unwrap();
    doc.commit();
    assert_ne!(
        compare(&doc, &peer, peer.authority(), &device, true).unwrap(),
        role
    );
}

mod parity;
