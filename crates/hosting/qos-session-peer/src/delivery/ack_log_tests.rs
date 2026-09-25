//! Recovery predicate of consumer ACK side records.
use super::*;

fn sign(owner: &SessionOwner, record: &AckRecordV1) -> Vec<u8> {
    let payload = postcard::to_stdvec(record).unwrap();
    let signature = owner
        .identity
        .native
        .sign(digest(&payload).as_bytes())
        .to_bytes()
        .to_vec();
    postcard::to_stdvec(&SignedAckRecord { payload, signature }).unwrap()
}

/// An activated owner with one consumer and its ledger's latest replay entry.
fn fixture() -> (crate::delivery_http::tests::Fixture, AckRecordV1) {
    let mut f = crate::delivery_http::tests::Fixture::new();
    let device = [7; 32];
    let PublicationStorage::V5 { delivery, .. } = &mut f.owner.aggregate.publication else {
        panic!("fixture owner has no delivery ledger");
    };
    let (sequence, entry) = delivery.replay.last_key_value().expect("replay entry");
    let committed = CommitRef {
        sequence: *sequence,
        digest: entry.digest,
    };
    delivery.consumers.insert(
        device,
        ConsumerState {
            id: [9; 16],
            device,
            root: [8; 32],
            authorization_view: [6; 32],
            acknowledged: None,
        },
    );
    let record = AckRecordV1 {
        version: 1,
        room: f.owner.aggregate.room.clone(),
        store_generation: delivery.store_generation,
        device,
        consumer: [9; 16],
        authorization_view: [6; 32],
        committed,
    };
    (f, record)
}
fn acknowledged(aggregate: &Aggregate, device: [u8; 32]) -> Option<CommitRef> {
    let PublicationStorage::V5 { delivery, .. } = &aggregate.publication else {
        unreachable!()
    };
    delivery.consumers[&device].acknowledged.clone()
}

#[test]
fn genuine_current_record_advances_once_and_is_idempotent() {
    let (f, record) = fixture();
    let bytes = sign(&f.owner, &record);
    let mut aggregate = f.owner.aggregate.clone();
    let records = vec![bytes.clone(), bytes];
    assert_eq!(
        apply_recovered(&mut aggregate, &f.owner.identity, &records).unwrap(),
        1
    );
    assert_eq!(
        acknowledged(&aggregate, record.device),
        Some(record.committed.clone())
    );
    // Already folded: inert.
    assert_eq!(
        apply_recovered(&mut aggregate, &f.owner.identity, &records).unwrap(),
        0
    );
    assert_eq!(
        acknowledged(&aggregate, record.device),
        Some(record.committed)
    );
}

#[test]
fn superseded_or_foreign_records_are_inert() {
    let (f, record) = fixture();
    let variants: Vec<Box<dyn Fn(&mut AckRecordV1)>> = vec![
        Box::new(|r| r.room.push('x')),
        Box::new(|r| r.store_generation[0] ^= 1),
        Box::new(|r| r.consumer[0] ^= 1),
        Box::new(|r| r.authorization_view[0] ^= 1),
        Box::new(|r| r.device[0] ^= 1),
        Box::new(|r| r.committed.digest[0] ^= 1),
        Box::new(|r| r.committed.sequence += 1_000_000),
        Box::new(|r| r.committed.sequence = 0),
    ];
    for (index, mutate) in variants.iter().enumerate() {
        let mut changed = AckRecordV1 {
            room: record.room.clone(),
            committed: record.committed.clone(),
            ..record
        };
        mutate(&mut changed);
        let mut aggregate = f.owner.aggregate.clone();
        let applied = apply_recovered(
            &mut aggregate,
            &f.owner.identity,
            &[sign(&f.owner, &changed)],
        )
        .unwrap();
        assert_eq!(applied, 0, "variant {index}");
        assert_eq!(
            acknowledged(&aggregate, record.device),
            None,
            "variant {index}"
        );
    }
    // A cursor at or behind the recorded one is never moved backwards.
    let mut aggregate = f.owner.aggregate.clone();
    let PublicationStorage::V5 { delivery, .. } = &mut aggregate.publication else {
        unreachable!()
    };
    delivery
        .consumers
        .get_mut(&record.device)
        .unwrap()
        .acknowledged = Some(record.committed.clone());
    let older = sign(&f.owner, &record);
    assert_eq!(
        apply_recovered(&mut aggregate, &f.owner.identity, &[older]).unwrap(),
        0
    );
}

#[test]
fn unauthenticated_records_fail_recovery_closed() {
    let (f, record) = fixture();
    let mut signed: SignedAckRecord = postcard::from_bytes(&sign(&f.owner, &record)).unwrap();
    signed.signature[0] ^= 1;
    let forged = postcard::to_stdvec(&signed).unwrap();
    let mut aggregate = f.owner.aggregate.clone();
    assert!(apply_recovered(&mut aggregate, &f.owner.identity, &[forged]).is_err());
    assert!(apply_recovered(&mut aggregate, &f.owner.identity, &[b"junk".to_vec()]).is_err());
}
