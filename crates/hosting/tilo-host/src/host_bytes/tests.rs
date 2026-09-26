use super::*;
#[test]
fn bulk_preserves_eight_control_buffers_and_control_can_consume_them() {
    let raw = Arc::new(Semaphore::new(CONTROL_RESERVE_BYTES + 4096));
    let pool = HostBytes::from_ingress(raw.clone());
    let bulk = pool.try_acquire_bulk(4096).unwrap();
    assert!(pool.try_acquire_bulk(1).is_none());
    let control = pool.try_acquire_control(CONTROL_RESERVE_BYTES).unwrap();
    assert_eq!(raw.available_permits(), 0);
    assert!(pool.try_acquire_control(1).is_none());
    drop(bulk);
    drop(control);
    assert_eq!(raw.available_permits(), CONTROL_RESERVE_BYTES + 4096);
    assert!(pool.try_acquire_bulk(usize::MAX).is_none());
}
#[test]
fn split_and_shared_backing_retain_exact_same_pool_charge_until_last_drop() {
    let pool = HostBytes::from_ingress(Arc::new(Semaphore::new(CONTROL_RESERVE_BYTES + 100)));
    assert!(pool.same_pool(&pool.clone()));
    assert!(!pool.same_pool(&HostBytes::from_ingress(Arc::new(Semaphore::new(1)))));
    let mut credit = pool.try_acquire_bulk(100).unwrap();
    assert!(credit.split(101).is_none());
    let retained = Arc::new(credit.split(40).unwrap());
    assert_eq!(credit.bytes(), 60);
    assert_eq!(retained.bytes(), 40);
    let shared = retained.clone();
    drop(credit);
    drop(retained);
    assert_eq!(pool.available_bytes(), CONTROL_RESERVE_BYTES + 60);
    drop(shared);
    assert_eq!(pool.available_bytes(), CONTROL_RESERVE_BYTES + 100);
}
#[test]
fn concurrent_bulk_admission_cannot_race_through_control_floor() {
    let pool = HostBytes::from_ingress(Arc::new(Semaphore::new(CONTROL_RESERVE_BYTES + 4096)));
    let start = Arc::new(std::sync::Barrier::new(33));
    let done = Arc::new(std::sync::Barrier::new(33));
    let held = Arc::new(Mutex::new(Vec::new()));
    std::thread::scope(|scope| {
        for _ in 0..32 {
            let pool = pool.clone();
            let start = start.clone();
            let done = done.clone();
            let held = held.clone();
            scope.spawn(move || {
                start.wait();
                if let Some(credit) = pool.try_acquire_bulk(4096) {
                    held.lock().unwrap().push(credit);
                }
                done.wait();
            });
        }
        start.wait();
        done.wait();
        assert_eq!(held.lock().unwrap().len(), 1);
        assert_eq!(pool.available_bytes(), CONTROL_RESERVE_BYTES);
        assert!(pool.try_acquire_control(CONTROL_RESERVE_BYTES).is_some());
    });
    held.lock().unwrap().clear();
    assert_eq!(pool.available_bytes(), CONTROL_RESERVE_BYTES + 4096);
}
