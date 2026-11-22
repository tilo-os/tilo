//! The resources of a person's store that one of their devices alone writes.
//!
//! Most of what a person keeps is theirs on every device: any device of
//! theirs writes it, and the store merges what each wrote. Some of it is one
//! device's: its own name and kind, what it is doing, the networks it joined.
//! Those resources sit in the device's own subtree of the store
//! ([`device_resource`]), and the store's policy gives that subtree to the
//! device whose key names it: a write there signed by any other device of the
//! person is refused, as a write the policy does not allow is anywhere.
//!
//! A device's own resource still reaches a sibling that device never met: each
//! device keeps the newest whole state the owner signed of it and hands that
//! on as the owner signed it, so whoever holds it can pass it along and
//! nobody can change it on the way.

/// The start of every resource of a device's own subtree.
const SUBTREE: &str = "device/";

/// The resource `name` of `device`'s own subtree: written by that device
/// alone, and read by all of the person's devices.
#[must_use]
pub fn device_resource(device: &[u8; 32], name: &str) -> String {
    format!("{SUBTREE}{}/{name}", hex::encode(device))
}

/// The device whose own subtree `resource` is in, when it is in one.
#[must_use]
pub fn resource_owner(resource: &str) -> Option<[u8; 32]> {
    let rest = resource.strip_prefix(SUBTREE)?;
    let (device, name) = rest.split_once('/')?;
    if name.is_empty() {
        return None;
    }
    let mut key = [0; 32];
    hex::decode_to_slice(device, &mut key).ok()?;
    // Only the form `device_resource` writes names a device: an upper-case
    // spelling of the same key is another resource, which no device owns.
    (hex::encode(key) == device).then_some(key)
}

#[cfg(test)]
mod tests {
    use super::super::PersonalStore;
    use super::*;
    use crate::NeighbourMsg;
    use automerge::transaction::Transactable as _;
    use automerge::{AutoCommit, ReadDoc as _};
    use collab_types::CommitNamed as _;
    use collab_types::DocumentMutation;
    use collab_types::signer::DeviceSigner as _;
    use ed25519_dalek::SigningKey;
    use tilo_user_identity::{DeviceCapabilities, DeviceCert, sign_device_cert};
    use web_time::SystemTime;

    const APP: &str = "so.tilo.settings";

    fn now() -> SystemTime {
        tilo_clock::wall()
    }

    /// Device `n` of the person whose root is `root`, with its store.
    fn device(root: &SigningKey, n: u8) -> (SigningKey, PersonalStore) {
        let key = SigningKey::from_bytes(&[n; 32]);
        let mut cert = DeviceCert {
            user_public_key: root.verifying_key().to_bytes(),
            device_public_key: key.device_public_key(),
            device_label: format!("device-{n}"),
            capabilities: DeviceCapabilities::member(),
            issued_at_ns: 1,
            expires_at_ns: None,
            signature: [0; 64],
            issuer: None,
        };
        sign_device_cert(&mut cert, root).unwrap();
        let store = PersonalStore::open(root.verifying_key().to_bytes(), cert, now()).unwrap();
        (key, store)
    }

    /// `to` takes what `from` hands a device it met: who it is, then every
    /// resource it holds.
    fn meet(from: (&SigningKey, &mut PersonalStore), to: (&SigningKey, &mut PersonalStore)) {
        let (from_key, from) = from;
        let (to_key, to) = to;
        from.receive(from_key, to_key.device_public_key(), to.hello(), now())
            .unwrap();
        for message in from
            .catch_up(from_key, to_key.device_public_key(), now())
            .unwrap()
        {
            to.receive(to_key, from_key.device_public_key(), message, now())
                .unwrap();
        }
    }

    fn whole(field: &str, value: &str) -> DocumentMutation {
        let mut doc = AutoCommit::new();
        doc.put(automerge::ROOT, field, value).unwrap();
        doc.commit_edit("settings", field);
        DocumentMutation::Snapshot(doc.save())
    }

    /// A change of `held` setting `field` to `value`.
    fn change(held: &[u8], field: &str, value: &str) -> DocumentMutation {
        let mut doc = AutoCommit::load(held).unwrap();
        let heads = doc.get_heads();
        doc.put(automerge::ROOT, field, value).unwrap();
        doc.commit_edit("settings", field);
        DocumentMutation::Incremental(doc.save_after(&heads))
    }

    fn read(store: &PersonalStore, resource: &str, field: &str) -> Option<String> {
        let doc = AutoCommit::load(&store.document(APP, resource)?).unwrap();
        let (value, _) = doc.get(automerge::ROOT, field).unwrap()?;
        value.into_string().ok()
    }

    /// A device's own subtree is written by that device alone: its siblings
    /// read it, and a write there by any other device of the person is
    /// refused, made on that device or sent by it.
    #[test]
    fn only_the_device_a_subtree_names_writes_it() {
        let root = SigningKey::from_bytes(&[1; 32]);
        let (a_key, mut a) = device(&root, 2);
        let (b_key, mut b) = device(&root, 3);
        meet((&b_key, &mut b), (&a_key, &mut a));
        meet((&a_key, &mut a), (&b_key, &mut b));
        let own = device_resource(&a_key.device_public_key(), "settings");

        let frame = a
            .publish(APP, &own, &whole("label", "Laptop"), &a_key, now())
            .unwrap();
        b.receive(&b_key, a_key.device_public_key(), frame, now())
            .unwrap();
        assert_eq!(read(&b, &own, "label").as_deref(), Some("Laptop"));

        // B's own store refuses B's write to A's subtree.
        let held = b.document(APP, &own).unwrap();
        let refused = b.publish(APP, &own, &change(&held, "label", "Mine"), &b_key, now());
        assert!(refused.is_err(), "{refused:?}");
        assert_eq!(read(&b, &own, "label").as_deref(), Some("Laptop"));

        // A write B signs for A's subtree, sent to A, is refused there.
        let (_, mut lax) = device(&root, 3);
        let NeighbourMsg::AppDocument { envelope, .. } = lax
            .publish(APP, "elsewhere", &whole("label", "Forged"), &b_key, now())
            .unwrap()
        else {
            panic!("a resource write is a document frame");
        };
        let mut forged = (*envelope).clone();
        forged.artifact = collab_types::document_artifact_id(APP, &a.store_key(), &own);
        forged.attestation_id = collab_types::document_attestation_id(APP, &a.store_key(), &own);
        collab_types::sign_op(&mut forged, &b_key).unwrap();
        let sent = NeighbourMsg::AppDocument {
            peer: crate::peer_handle(&b_key.device_public_key()),
            app_id: APP.into(),
            cluster_id: a.store_key(),
            document_id: own.clone(),
            envelope: Box::new(forged),
            readers: None,
        };
        let refused = a.receive(&a_key, b_key.device_public_key(), sent, now());
        assert!(refused.is_err(), "{refused:?}");
        assert_eq!(read(&a, &own, "label").as_deref(), Some("Laptop"));

        // B's own subtree is B's to write, and A reads it.
        let b_own = device_resource(&b_key.device_public_key(), "settings");
        let frame = b
            .publish(APP, &b_own, &whole("label", "Phone"), &b_key, now())
            .unwrap();
        a.receive(&a_key, b_key.device_public_key(), frame, now())
            .unwrap();
        assert_eq!(read(&a, &b_own, "label").as_deref(), Some("Phone"));
    }

    /// A device the owner never met reads the owner's own resource from a
    /// sibling that holds it, as the owner signed it last, also after that
    /// sibling restarted; an older state handed on later changes nothing.
    #[test]
    fn a_devices_own_resource_is_handed_on_as_its_owner_signed_it() {
        let root = SigningKey::from_bytes(&[1; 32]);
        let (a_key, mut a) = device(&root, 2);
        let (b_key, mut b) = device(&root, 3);
        let (c_key, mut c) = device(&root, 4);
        meet((&b_key, &mut b), (&a_key, &mut a));
        meet((&a_key, &mut a), (&b_key, &mut b));
        let own = device_resource(&a_key.device_public_key(), "settings");

        let first = a
            .publish(APP, &own, &whole("label", "Laptop"), &a_key, now())
            .unwrap();
        b.receive(&b_key, a_key.device_public_key(), first.clone(), now())
            .unwrap();
        // A change of the owner's subtree goes out as the whole resource.
        let held = a.document(APP, &own).unwrap();
        let renamed = a
            .publish(APP, &own, &change(&held, "label", "Study"), &a_key, now())
            .unwrap();
        let NeighbourMsg::AppDocument { envelope, .. } = &renamed else {
            panic!("a resource write is a document frame: {renamed:?}");
        };
        assert!(matches!(
            DocumentMutation::decode(&envelope.payload),
            Ok(DocumentMutation::Snapshot(_))
        ));
        b.receive(&b_key, a_key.device_public_key(), renamed, now())
            .unwrap();

        // B restarts, then meets C, which never met A.
        let kept = b.durable().unwrap();
        let mut b = PersonalStore::restore(&kept, b.device(), now()).unwrap();
        c.admit_device(a.device(), now()).unwrap();
        meet((&c_key, &mut c), (&b_key, &mut b));
        meet((&b_key, &mut b), (&c_key, &mut c));
        assert_eq!(read(&c, &own, "label").as_deref(), Some("Study"));

        // The older state, handed on late, is no news and no refusal.
        let NeighbourMsg::AppDocument { envelope, .. } = first else {
            panic!("a resource write is a document frame");
        };
        let relayed = NeighbourMsg::AppDocument {
            peer: crate::peer_handle(&a_key.device_public_key()),
            app_id: APP.into(),
            cluster_id: c.store_key(),
            document_id: own.clone(),
            envelope,
            readers: None,
        };
        let outcome = c
            .receive(&c_key, b_key.device_public_key(), relayed, now())
            .unwrap();
        assert!(outcome.duplicate && !outcome.changed);
        assert_eq!(read(&c, &own, "label").as_deref(), Some("Study"));
        assert!(c.write_refusals().is_empty());
    }

    #[test]
    fn a_device_resource_names_its_device_and_nothing_else_does() {
        let device = [0xab; 32];
        let resource = device_resource(&device, "settings");
        assert_eq!(resource_owner(&resource), Some(device));
        for other in [
            "settings".to_owned(),
            "device/".to_owned(),
            format!("device/{}", hex::encode(device)),
            format!("device/{}/", hex::encode(device)),
            format!("device/{}/settings", hex::encode(device).to_uppercase()),
            "device/ab/settings".to_owned(),
        ] {
            assert_eq!(resource_owner(&other), None, "{other}");
        }
    }
}
