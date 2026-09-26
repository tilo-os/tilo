use super::*;
use qos_peer_state::transport::Retention;
fn reference(pool: &mut PublicationPool, bytes: &[u8]) -> PublicationRef {
    PublicationRef {
        payload: pool.intern(bytes).unwrap(),
        kind: Retention::Delta,
    }
}
#[test]
fn publication_custody_actual_roots_survive_pool_drop_with_original_host_credit() {
    let (ingress, _rx) = crate::https::ingress_channel();
    let host = ingress.host_bytes();
    let mut pool = PublicationPool::new(4096).unwrap();
    let refs = [reference(&mut pool, b"exact original signed bytes")];
    let pointer = pool.resolve(&refs[0].payload).unwrap().as_ptr();
    let available = host.available_bytes();
    let custody = PublicationCustody::from_pool(&pool, refs.iter(), &host)
        .expect("actual pool must produce genuinely charged custody");
    assert!(custody.same_pool(&host));
    assert_eq!(custody.len(), 1);
    assert_eq!(custody.reference(0), Some(&refs[0]));
    assert_eq!(custody.get(0).unwrap().bytes.as_ptr(), pointer);
    assert!(host.available_bytes() < available);
    drop(pool);
    drop(ingress);
    assert_eq!(
        custody.get(0).unwrap().bytes,
        b"exact original signed bytes"
    );
    assert_eq!(custody.get(0).unwrap().bytes.as_ptr(), pointer);
    assert!(host.available_bytes() < available);
    drop(custody);
    // The original ingress inventory may also retire; pin credit must be gone.
    assert!(host.available_bytes() >= available);
}
#[test]
fn publication_custody_duplicate_roots_pay_one_actual_backing_and_preserve_original_order() {
    let (ingress, _rx) = crate::https::ingress_channel();
    let host = ingress.host_bytes();
    let mut pool = PublicationPool::new(1024 * 1024).unwrap();
    let a = reference(&mut pool, &[1; 65536]);
    let b = reference(&mut pool, b"different original");
    let _unselected = reference(&mut pool, &[8; 262144]);
    let available = host.available_bytes();
    let one = PublicationCustody::from_pool(&pool, [&a].into_iter(), &host).unwrap();
    let one_cost = available - host.available_bytes();
    assert!(
        one_cost < 128 * 1024,
        "unselected pool backing must not be charged as pinned"
    );
    drop(one);
    assert_eq!(host.available_bytes(), available);
    let repeated = PublicationCustody::from_pool(&pool, [&a, &b, &a].into_iter(), &host).unwrap();
    let repeated_cost = available - host.available_bytes();
    assert!(
        repeated_cost - one_cost < 1024,
        "a repeated64KiB backing must not be paid twice"
    );
    assert_eq!(
        repeated.get(0).unwrap().bytes.as_ptr(),
        repeated.get(2).unwrap().bytes.as_ptr()
    );
    assert_eq!(repeated.get(1).unwrap().bytes, b"different original");
    assert_eq!(repeated.reference(2), Some(&a));
    let (foreign, _rx) = crate::https::ingress_channel();
    assert!(!repeated.same_pool(&foreign.host_bytes()));
    drop(repeated);
    assert_eq!(host.available_bytes(), available);
}
#[test]
fn publication_custody_capacity_missing_refs_and_false_iterator_lengths_release_all_credit() {
    let (ingress, _rx) = crate::https::ingress_channel();
    let host = ingress.host_bytes();
    let mut pool = PublicationPool::new(1024 * 1024).unwrap();
    let a = reference(&mut pool, &[1; 65536]);
    let missing = PublicationRef {
        payload: [99; 32],
        kind: Retention::Delta,
    };
    let available = host.available_bytes();
    assert!(matches!(
        PublicationCustody::from_pool(&pool, [&a, &missing].into_iter(), &host),
        Err(PublicationCustodyFailure::InvalidPayload)
    ));
    assert_eq!(host.available_bytes(), available);
    let pressure = host
        .try_acquire_bulk(available - crate::host_bytes::CONTROL_RESERVE_BYTES - 1024)
        .unwrap();
    let pressured = host.available_bytes();
    assert!(matches!(
        PublicationCustody::from_pool(&pool, [&a].into_iter(), &host),
        Err(PublicationCustodyFailure::Capacity)
    ));
    assert_eq!(host.available_bytes(), pressured);
    drop(pressure);
    struct Liar<'a> {
        reference: &'a PublicationRef,
        left: usize,
        claimed: usize,
    }
    impl<'a> Iterator for Liar<'a> {
        type Item = &'a PublicationRef;
        fn next(&mut self) -> Option<Self::Item> {
            if self.left == 0 {
                None
            } else {
                self.left -= 1;
                Some(self.reference)
            }
        }
        fn size_hint(&self) -> (usize, Option<usize>) {
            (self.claimed, Some(self.claimed))
        }
    }
    impl ExactSizeIterator for Liar<'_> {
        fn len(&self) -> usize {
            self.claimed
        }
    }
    for (left, claimed) in [(2, 1), (1, 2), (1, 65537), (0, 0)] {
        assert!(
            PublicationCustody::from_pool(
                &pool,
                Liar {
                    reference: &a,
                    left,
                    claimed
                },
                &host
            )
            .is_err()
        );
        assert_eq!(host.available_bytes(), available);
    }
}
