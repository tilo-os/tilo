//! A device a host placed for records of a workspace: its certificate, which the host signed with
//! its own key under its person's placement grant, or as one of their devices, names the records it
//! reaches, and every replica admits its writes to those records and nothing else, as its
//! person's, without listing it anywhere.

use super::*;
use tilo_user_identity::{
    PlacedRecords, prepare_placed_device_cert, prepare_placing_grant, sign_device_cert,
};

const WORKSPACE: &str = "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa";
const OTHER_WORKSPACE: &str = "bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb";
const APP: &str = "test.app";
/// The two records the device is placed for, and one it is not.
const PLACED: [&str; 2] = ["tab:1", "tab:2"];
const ELSEWHERE: &str = "tab:3";

/// `records` of `workspace`.
fn records_of(workspace: &str, records: &[&str]) -> PlacedRecords {
    PlacedRecords::new(
        *uuid::Uuid::parse_str(workspace).unwrap().as_bytes(),
        records
            .iter()
            .map(|record| neighbour_wire::app_document_group(APP, record)),
    )
    .unwrap()
}

/// A host that is none of the person's devices, holding the placement grant their root (`[1; 32]`,
/// as [`fixtures`]' devices) signed it for `records` of `workspace`.
fn granted_host(workspace: &str, records: &[&str]) -> (SigningKey, DeviceCert) {
    let root = SigningKey::from_bytes(&[1; 32]);
    let key = SigningKey::from_bytes(&[40; 32]);
    let mut grant = prepare_placing_grant(
        root.verifying_key().to_bytes(),
        key.verifying_key().to_bytes(),
        records_of(workspace, records),
        "tilo.example",
        1,
        None,
    );
    sign_device_cert(&mut grant, &root).unwrap();
    (key, grant)
}

/// A device the host `(host_key, host)` placed for `records` of `workspace`, valid from a second
/// before `now` for `lifetime`.
fn placed_by(
    (host_key, host): &(SigningKey, DeviceCert),
    workspace: &str,
    records: &[&str],
    now: SystemTime,
    lifetime: Duration,
) -> (SigningKey, DeviceCert) {
    let key = SigningKey::from_bytes(&[41; 32]);
    let issued = ns_since_epoch(now).saturating_sub(1_000_000_000);
    let mut cert = prepare_placed_device_cert(
        host,
        key.verifying_key().to_bytes(),
        records_of(workspace, records),
        "Placed",
        issued,
        issued.saturating_add(u64::try_from(lifetime.as_nanos()).unwrap()),
    );
    sign_device_cert(&mut cert, host_key).unwrap();
    (key, cert)
}

/// A device placed for `records` of `workspace` by a host under the person's grant for them.
fn placed(
    workspace: &str,
    records: &[&str],
    now: SystemTime,
    lifetime: Duration,
) -> (SigningKey, DeviceCert) {
    placed_by(
        &granted_host(workspace, records),
        workspace,
        records,
        now,
        lifetime,
    )
}

/// A replica of [`WORKSPACE`], founded on the person's laptop, keeping three records of
/// [`APP`].
fn replica() -> (StatePeer, SigningKey, DeviceCert, SystemTime) {
    let (a, ac, _, _, now) = fixtures();
    let mut peer = StatePeer::new(
        WORKSPACE,
        ac.clone(),
        vec![ac.clone()],
        RevocationList::new(),
        now,
    )
    .unwrap();
    let documents = PLACED
        .iter()
        .chain([&ELSEWHERE])
        .map(|record| {
            let mut document = AutoCommit::new();
            document.put(ROOT, "value", "seeded").unwrap();
            document.commit_edit("fixture", "value", &ac.user_public_key);
            SeedDocument {
                app_id: APP.into(),
                document_id: (*record).into(),
                kind: "test".into(),
                bytes: document.save(),
            }
        })
        .collect();
    peer.install_seed(
        SeedState {
            workspace_changes: seed_workspace(WORKSPACE, &ac, std::slice::from_ref(&ac)).unwrap(),
            documents,
        },
        &a,
        now,
    )
    .unwrap();
    (peer, a, ac, now)
}

/// The placed device's hello: its certificate, as every link it opens begins with.
fn hello(cert: &DeviceCert) -> NeighbourMsg {
    NeighbourMsg::IdentityHello {
        protocol: neighbour_wire::PEER_PROTOCOL_VERSION,
        cert: Box::new(cert.clone()),
        revocations: vec![],
    }
}

/// A change `key` signs to `record` for `person`, written in `author`, a copy of the replica.
fn write(
    author: &mut StatePeer,
    key: &SigningKey,
    person: &[u8; 32],
    record: &str,
    now: SystemTime,
    name: &str,
) -> NeighbourMsg {
    let mut doc = AutoCommit::load(&author.document(APP, record).unwrap()).unwrap();
    doc.set_actor(automerge::ActorId::from(
        key.verifying_key().to_bytes().to_vec(),
    ));
    let heads = doc.get_heads();
    doc.put(ROOT, name, name).unwrap();
    doc.commit_edit("fixture", "fixture", person);
    author
        .publication_mut()
        .publish_incremental(APP, record, doc.save_after(&heads), key, now)
        .unwrap()
}

fn has_marker(peer: &StatePeer, record: &str, name: &str) -> bool {
    AutoCommit::load(&peer.document(APP, record).unwrap())
        .unwrap()
        .get(ROOT, name)
        .unwrap()
        .is_some()
}

/// The person's root revokes `key`, and their laptop tells `peer`.
fn revoke(peer: &mut StatePeer, laptop: &DeviceCert, key: [u8; 32], now: SystemTime) {
    let root = SigningKey::from_bytes(&[1; 32]);
    let mut revoked = tilo_user_identity::RevocationEntry {
        user_public_key: root.verifying_key().to_bytes(),
        revoked_device_public_key: key,
        revoked_at_ns: ns_since_epoch(now).saturating_sub(500_000_000),
        reason: tilo_user_identity::RevocationReason::Compromised,
        signature: [0; 64],
    };
    tilo_user_identity::sign_revocation_entry(&mut revoked, &root).unwrap();
    peer.receive_from(
        TransportOrigin::UntrustedPresence(laptop.device_public_key),
        NeighbourMsg::IdentityHello {
            protocol: neighbour_wire::PEER_PROTOCOL_VERSION,
            cert: Box::new(laptop.clone()),
            revocations: vec![revoked],
        },
        now,
    )
    .expect("the person's laptop brings the revocation");
}

tilo_test_env::scenario! {
    fn a_placed_device_writes_its_records_and_nothing_else;

    phase writes_to_either_of_its_records_are_its_persons_and_a_third_is_refused;
    {
        let (mut peer, _, laptop, now) = replica();
        // The device's own writes, made in a copy that has not heard of it: what a page that
        // escapes its sandbox signs, whatever its own copy would refuse.
        let mut author = peer.clone();
        let (key, cert) = placed(WORKSPACE, &PLACED, now, Duration::from_secs(3600));
        let device = cert.device_public_key;
        let origin = TransportOrigin::ServerDelta(device);
        peer.receive_from(origin, hello(&cert), now)
            .expect("a placed device's certificate is taken in");
        let placed_device = peer
            .placed_device(&device, now)
            .expect("the replica knows the device as placed")
            .expect("it is admitted");
        assert_eq!(placed_device.person, laptop.user_public_key, "history names its person");
        assert_eq!(placed_device.records, cert.capabilities.records.unwrap());
        for (at, record) in PLACED.iter().enumerate() {
            let name = format!("by the placed device {at}");
            let change = write(&mut author, &key, &laptop.user_public_key, record, now, &name);
            peer.receive_from(origin, change, now)
                .unwrap_or_else(|error| panic!("a write to {record} is admitted: {error}"));
            assert!(has_marker(&peer, record, &name));
        }
        let change = write(&mut author, &key, &laptop.user_public_key, ELSEWHERE, now, "outside");
        assert!(
            peer.receive_from(origin, change, now).is_err(),
            "a write to a record its certificate does not name is refused"
        );
        assert!(!has_marker(&peer, ELSEWHERE, "outside"));
        // It reads what it may write, and nothing else.
        for record in PLACED {
            peer.check_document_read(device, APP, record, now).unwrap();
        }
        assert!(peer.check_document_read(device, APP, ELSEWHERE, now).is_err());
        // It is listed nowhere: no member list, no member's devices, no keys.
        assert!(peer.certificates().iter().all(|known| known.device_public_key != device));
        assert!(peer.permissions(device, now).is_err());
        assert!(!peer.governance().key_holders(now).unwrap().contains(&device));
        assert!(
            !peer
                .governance()
                .governed_device_access(now)
                .unwrap()
                .contains_key(&device)
        );
    }

    phase a_host_that_is_the_persons_device_places_without_a_grant;
    {
        let (mut peer, _, laptop, now) = replica();
        let mut author = peer.clone();
        // The person's laptop places a device itself, with its own key.
        let laptop_key = SigningKey::from_bytes(&[2; 32]);
        assert_eq!(laptop_key.verifying_key().to_bytes(), laptop.device_public_key);
        let (key, cert) = placed_by(
            &(laptop_key, laptop.clone()),
            WORKSPACE,
            &PLACED,
            now,
            Duration::from_secs(3600),
        );
        let device = cert.device_public_key;
        let origin = TransportOrigin::ServerDelta(device);
        peer.receive_from(origin, hello(&cert), now)
            .expect("a device the person's laptop placed is taken in");
        let change = write(&mut author, &key, &laptop.user_public_key, PLACED[0], now, "placed");
        peer.receive_from(origin, change, now).unwrap();
        assert!(has_marker(&peer, PLACED[0], "placed"));
    }

    phase it_reaches_nothing_once_its_certificate_ends;
    {
        let (mut peer, _, laptop, now) = replica();
        let mut author = peer.clone();
        let (key, cert) = placed(WORKSPACE, &PLACED, now, Duration::from_secs(60));
        let device = cert.device_public_key;
        let origin = TransportOrigin::ServerDelta(device);
        peer.receive_from(origin, hello(&cert), now).unwrap();
        let later = now + Duration::from_secs(120);
        assert!(matches!(peer.placed_device(&device, later), Some(Err(_))));
        let change = write(&mut author, &key, &laptop.user_public_key, PLACED[0], later, "after its end");
        assert!(peer.receive_from(origin, change, later).is_err());
        assert!(!has_marker(&peer, PLACED[0], "after its end"));
        assert!(peer.check_document_read(device, APP, PLACED[0], later).is_err());
    }

    phase it_reaches_nothing_once_its_hosts_grant_is_revoked;
    {
        let (mut peer, _, laptop, now) = replica();
        let mut author = peer.clone();
        let host = granted_host(WORKSPACE, &PLACED);
        let (key, cert) = placed_by(&host, WORKSPACE, &PLACED, now, Duration::from_secs(3600));
        let device = cert.device_public_key;
        let origin = TransportOrigin::ServerDelta(device);
        peer.receive_from(origin, hello(&cert), now).unwrap();
        assert!(matches!(peer.placed_device(&device, now), Some(Ok(_))));
        // The person revokes the grant they gave the host.
        revoke(&mut peer, &laptop, host.1.device_public_key, now);
        assert!(matches!(peer.placed_device(&device, now), Some(Err(_))));
        let change = write(
            &mut author,
            &key,
            &laptop.user_public_key,
            PLACED[0],
            now,
            "after the revocation",
        );
        assert!(peer.receive_from(origin, change, now).is_err());
        assert!(!has_marker(&peer, PLACED[0], "after the revocation"));
        assert!(peer.check_document_read(device, APP, PLACED[0], now).is_err());
        assert!(
            peer.receive_from(origin, hello(&cert), now).is_err(),
            "a placed device whose host's grant is revoked is refused when it says hello again"
        );
    }

    phase a_device_placed_in_another_workspace_is_refused_here;
    {
        let (mut peer, _, laptop, now) = replica();
        let mut author = peer.clone();
        let (key, cert) = placed(OTHER_WORKSPACE, &PLACED, now, Duration::from_secs(3600));
        let device = cert.device_public_key;
        let origin = TransportOrigin::ServerDelta(device);
        assert!(peer.receive_from(origin, hello(&cert), now).is_err());
        assert!(peer.placed_device(&device, now).is_none());
        let change = write(&mut author, &key, &laptop.user_public_key, PLACED[0], now, "from elsewhere");
        let _ = peer.receive_from(origin, change, now);
        assert!(!has_marker(&peer, PLACED[0], "from elsewhere"));
    }

    phase a_listing_of_its_certificate_makes_no_member_of_it;
    {
        let (a, ac, _, _, now) = fixtures();
        let (_, cert) = placed(WORKSPACE, &PLACED, now, Duration::from_secs(3600));
        let device = cert.device_public_key;
        let listed = vec![ac.clone(), cert];
        let mut peer =
            StatePeer::new(WORKSPACE, ac.clone(), listed.clone(), RevocationList::new(), now)
                .unwrap();
        peer.install_seed(
            SeedState {
                workspace_changes: seed_workspace(WORKSPACE, &ac, &listed).unwrap(),
                documents: Vec::new(),
            },
            &a,
            now,
        )
        .unwrap();
        // The workspace's rules are here, and its person's own device is a member by them.
        assert!(peer.permissions(ac.device_public_key, now).is_ok());
        // Listed, it is still no member's device: it has no permissions of its own, and reads
        // nothing until it says hello, decided then by its certificate.
        assert!(peer.permissions(device, now).is_err());
        assert!(peer.check_document_read(device, APP, PLACED[0], now).is_err());
        assert!(peer.placed_device(&device, now).is_none());
    }

    phase another_persons_placed_device_is_refused;
    {
        let (mut peer, _, _, now) = replica();
        let stranger = SigningKey::from_bytes(&[50; 32]);
        let host_key = SigningKey::from_bytes(&[51; 32]);
        let mut grant = prepare_placing_grant(
            stranger.verifying_key().to_bytes(),
            host_key.verifying_key().to_bytes(),
            records_of(WORKSPACE, &PLACED[..1]),
            "tilo.example",
            1,
            None,
        );
        sign_device_cert(&mut grant, &stranger).unwrap();
        let (_, cert) = placed_by(
            &(host_key, grant),
            WORKSPACE,
            &PLACED[..1],
            now,
            Duration::from_secs(3600),
        );
        let device = cert.device_public_key;
        assert!(
            peer.receive_from(TransportOrigin::ServerDelta(device), hello(&cert), now)
                .is_err(),
            "a person who is no member here places no device in it"
        );
        assert!(peer.placed_device(&device, now).is_none());
    }
}
