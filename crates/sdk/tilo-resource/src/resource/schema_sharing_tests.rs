use super::*;

#[test]
fn shared_schema_reloads_do_not_retain_or_repopulate_stale_generation() {
    let cache = Mutex::new(CompiledSchemas::default());
    let key = (TypeId::of::<()>(), TypeSchema::Apply, String::new());
    let first = cached_schema(&cache, key.clone(), &|| {
        Some(serde_json::json!({"minimum":1}))
    })
    .unwrap();
    let reused = cached_schema(&cache, key.clone(), &|| {
        panic!("cached type must not rebuild")
    })
    .unwrap();
    assert!(Arc::ptr_eq(&first, &reused));
    {
        let mut store = cache.lock().unwrap();
        store.epoch += 1;
        store.entries.clear();
    }
    let old_build = cached_schema(&cache, key.clone(), &|| {
        let mut store = cache.lock().unwrap();
        store.epoch += 1;
        store.entries.clear();
        drop(store);
        Some(serde_json::json!({"minimum":2}))
    })
    .unwrap();
    assert!(
        cache.lock().unwrap().entries.is_empty(),
        "old build cannot repopulate new generation"
    );
    let current = cached_schema(&cache, key, &|| Some(serde_json::json!({"minimum":3}))).unwrap();
    assert_eq!(first["minimum"], 1);
    assert_eq!(old_build["minimum"], 2);
    assert_eq!(current["minimum"], 3);
    assert!(!Arc::ptr_eq(&first, &current));
}
