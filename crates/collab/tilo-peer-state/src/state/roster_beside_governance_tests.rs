//! A join answer's roster names only what its governance does not carry: a
//! replica built from the short roster and the governance holds every
//! certificate, member and permission one built from the whole roster does.
use super::*;
use crate::admission_tests::{add_member_with, seeded};

/// `peer`'s certificates, in a stable order.
fn held(peer: &StatePeer) -> Vec<DeviceCert> {
    let mut certificates = peer.certificates();
    certificates.sort_by_key(|cert| cert.device_public_key);
    certificates
}

/// A replica of `governor`'s workspace built from `roster` and the governance
/// it publishes, as a newcomer's page builds one from its bootstrap and the
/// replay after it.
fn replica(
    governor: &mut StatePeer,
    key: &SigningKey,
    roster: Vec<DeviceCert>,
    now: SystemTime,
) -> StatePeer {
    let host = governor.owner_certificates()[0].clone();
    let mut replica = StatePeer::from_join(
        governor.workspace_id(),
        host.clone(),
        roster,
        &governor.revocations(),
        now,
    )
    .unwrap();
    for message in governor
        .publication_mut()
        .bootstrap_messages(key, now)
        .unwrap()
    {
        if matches!(message, NeighbourMsg::AppDocument { .. }) {
            continue;
        }
        replica
            .receive_from(
                crate::TransportOrigin::RetainedRelayDelta(host.device_public_key),
                message,
                now,
            )
            .unwrap();
    }
    replica
}

#[test]
fn a_replica_from_the_roster_beside_governance_holds_what_the_whole_roster_gives() {
    let (mut governor, key, _, _, now) = seeded();
    let members: Vec<DeviceCert> = (0..6)
        .map(|n| add_member_with(&mut governor, &key, now, 40 + n, 60 + n).1)
        .collect();
    let newcomer = &members[0];
    let host = governor.owner_certificates()[0].clone();

    let whole = governor.certificates();
    let short = governor
        .roster_beside_governance(&[newcomer.device_public_key, host.device_public_key], now);
    // The newcomer and its host stay; every other member's certificate is
    // the governance's to carry.
    assert!(short.contains(newcomer));
    assert!(short.contains(&host));
    for member in &members[1..] {
        assert!(governor.governance_certifies(member, now));
        assert!(!short.contains(member), "{member:?} sent twice");
    }
    assert!(short.len() < whole.len());

    let from_whole = replica(&mut governor, &key, whole, now);
    let from_short = replica(&mut governor, &key, short, now);
    assert_eq!(held(&from_short), held(&from_whole));
    assert_eq!(
        from_short.governance().governed_members(now).unwrap(),
        from_whole.governance().governed_members(now).unwrap()
    );
    for member in &members {
        assert_eq!(
            from_short
                .permissions(member.device_public_key, now)
                .map(|authority| authority.role)
                .map_err(|error| error.to_string()),
            from_whole
                .permissions(member.device_public_key, now)
                .map(|authority| authority.role)
                .map_err(|error| error.to_string()),
        );
    }
}

#[test]
fn governance_certifies_only_the_certificate_its_row_names_while_it_is_valid() {
    let (mut governor, key, _, _, now) = seeded();
    let (_, member, _) = add_member_with(&mut governor, &key, now, 40, 60);
    assert!(governor.governance_certifies(&member, now));
    // Another certificate for the same device is not the row's.
    let mut other = member.clone();
    other.device_label = "Elsewhere".into();
    assert!(!governor.governance_certifies(&other, now));
    // A device the governance never listed.
    let (_, stranger, _) = add_member_with(&mut seeded().0, &key, now, 41, 61);
    assert!(!governor.governance_certifies(&stranger, now));
    // Before its certificate's time, as the import would refuse it.
    let before = SystemTime::UNIX_EPOCH;
    assert!(!governor.governance_certifies(&member, before));
}
