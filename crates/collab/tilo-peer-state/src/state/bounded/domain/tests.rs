use super::*;
use crate::StatePeer;
use tilo_user_identity::RevocationList;
fn restore_with(
    peer: &StatePeer,
    now: SystemTime,
    limits: &PeerLimits,
) -> Result<BoundedStatePeer, BoundedPeerError> {
    let bytes = peer.durable().unwrap();
    BoundedStatePeer::restore(
        &bytes,
        peer.room(),
        peer.authority(),
        &[],
        &RevocationList::new(),
        now,
        limits,
    )
}
#[test]
fn peer_domain_reads_empty_and_nonempty_restored_peers() {
    let (source, _, _, _, now) = crate::admission_tests::seeded();
    let empty = StatePeer::new(
        source.room(),
        source.authority().clone(),
        source.certificates(),
        RevocationList::new(),
        now,
    )
    .unwrap();
    let peer = restore_with(&source, now, &PeerLimits::default()).unwrap();
    let empty_peer = restore_with(&empty, now, &PeerLimits::default()).unwrap();
    let read = peer.read();
    assert_eq!(read.room(), source.room());
    assert_eq!(
        read.authority_device(),
        source.authority().device_public_key
    );
    assert!(read.document_count() > 0);
    assert_eq!(empty_peer.read().document_count(), 0);
    assert_eq!(empty_peer.read().room(), empty.room());
}
#[test]
fn peer_domain_document_bytes_share_one_restore_limit() {
    let (source, _, _, _, now) = crate::admission_tests::seeded();
    let bytes = source.durable().unwrap();
    let durable: super::super::super::DurableV4 = postcard::from_bytes(&bytes).unwrap();
    let total: usize = durable
        .base
        .room_bytes
        .iter()
        .map(Vec::len)
        .chain(durable.base.documents.iter().map(|doc| doc.bytes.len()))
        .sum();
    assert!(total > 0);
    let exact = PeerLimits {
        normalized_bytes: total,
        ..PeerLimits::default()
    };
    assert!(restore_with(&source, now, &exact).is_ok());
    let short = PeerLimits {
        normalized_bytes: total - 1,
        ..PeerLimits::default()
    };
    assert!(matches!(
        restore_with(&source, now, &short),
        Err(BoundedPeerError::Capacity)
    ));
}
