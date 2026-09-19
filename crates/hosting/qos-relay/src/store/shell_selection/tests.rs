use super::*;
use crate::store::shell_selection_tests::{producer_frame, producer_metadata};
fn retained(store: &WorkspaceStore, bytes: &[u8]) {
    let (_, payload) = retained::payload(bytes, store.workspace_id).unwrap();
    match payload {
        CoeditPayload::Checkpoint { channel, .. } => {
            store.retain_checkpoint(&channel, bytes).unwrap()
        }
        CoeditPayload::RetainedDelta { channel, .. } => {
            store.retain_delta(&channel, 1, bytes).unwrap()
        }
    }
}
fn setup() -> (tempfile::TempDir, WorkspaceStore) {
    let tmp = tempfile::tempdir().unwrap();
    let meta = producer_metadata();
    let authority = RetentionAuthority::new(
        meta["room"].as_str().unwrap(),
        serde_json::from_value(meta["authority"].clone()).unwrap(),
    )
    .unwrap();
    let id = crate::room_workspace_id(authority.room()).unwrap();
    let mut store = WorkspaceStore::open(tmp.path(), id, true).unwrap();
    store.bind_shell_authority(authority).unwrap();
    for row in meta["baseline"].as_array().unwrap() {
        retained(&store, &producer_frame(&meta, row, 1));
    }
    // Trusted fixture import models admitted bytes durable before selection.
    let raw = WorkspaceStore::open(tmp.path(), id, true).unwrap();
    for row in meta["checkpoint2"].as_array().unwrap() {
        retained(&raw, &producer_frame(&meta, row, 2));
    }
    (tmp, store)
}
#[test]
fn selected_switch_failure_keeps_previous_durable_basis_and_retires_no_input() {
    let (_tmp, store) = setup();
    let before = retained::current(&store.root.join(DIRECTORY)).unwrap();
    let scratch = store.root.join("store-work-v1").join(format!(
        "retain-{}",
        hex::encode(rand::random::<[u8; 16]>())
    ));
    let mut selector = Selection::new(&store, scratch, None).unwrap();
    while !matches!(selector.phase, Phase::Commit) {
        assert!(matches!(
            selector
                .step(WorkLimit {
                    entries: 1,
                    ..Default::default()
                })
                .unwrap(),
            Step::More
        ));
    }
    let moved = selector.staged.with_extension("unavailable");
    fs::rename(&selector.staged, &moved).unwrap();
    assert!(
        selector.step(WorkLimit::default()).is_err(),
        "required directory sync fails before selector switch"
    );
    assert_eq!(
        retained::current(&store.root.join(DIRECTORY)).unwrap(),
        before
    );
    let mut plan = File::open(moved.join("gc.manifest")).unwrap();
    let mut count = 0;
    while let Some(meta) = read_meta(&mut plan).unwrap() {
        assert!(store.root.join(meta.source).exists());
        count += 1
    }
    assert!(count > 0);
    fs::rename(moved, &selector.staged).unwrap();
    while matches!(selector.step(WorkLimit::default()).unwrap(), Step::More) {}
    assert_ne!(
        retained::current(&store.root.join(DIRECTORY)).unwrap(),
        before
    );
}
#[test]
fn durable_retirement_plan_preserves_reused_path_and_selected_references_survive_gc() {
    let (_tmp, store) = setup();
    let scratch = store.root.join("store-work-v1").join(format!(
        "retain-{}",
        hex::encode(rand::random::<[u8; 16]>())
    ));
    let mut selector = Selection::new(&store, scratch, None).unwrap();
    while !matches!(selector.phase, Phase::Gc) {
        selector.step(WorkLimit::default()).unwrap();
    }
    let mut plan = File::open(selector.staged.join("gc.manifest")).unwrap();
    let target = read_meta(&mut plan).unwrap().unwrap();
    let path = store.root.join(target.source);
    let bytes = fs::read(&path).unwrap();
    fs::remove_file(&path).unwrap();
    fs::write(&path, &bytes).unwrap();
    while matches!(
        selector
            .step(WorkLimit {
                entries: 1,
                ..Default::default()
            })
            .unwrap(),
        Step::More
    ) {}
    assert_eq!(
        fs::read(path).unwrap(),
        bytes,
        "new inode at a retired name is not the captured target"
    );
    let mut selected = SelectedRead::new(&store).unwrap().unwrap();
    while matches!(selected.step().unwrap(), Step::More) {}
    assert!(selected.basis.complete());
}
#[test]
fn shell_selection_spills_history_metadata_and_preserves_unknown_files() {
    let (_tmp, store) = setup();
    let raw = WorkspaceStore::open(
        store.root.parent().unwrap().parent().unwrap(),
        store.workspace_id,
        true,
    )
    .unwrap();
    let bytes = crate::transport::encode(&crate::transport::Envelope::Delta {
        workspace_id: store.workspace_id,
        author_device_public_key: [77; 32],
        authored_at_ns: 1,
        signature: [0; 64],
        delta: CoeditPayload::RetainedDelta {
            channel: [88; 32],
            body: vec![9; 64],
        }
        .encode()
        .unwrap(),
    })
    .unwrap();
    raw.retain_delta(&[88; 32], 1, &bytes).unwrap();
    let lineage = store.root.join("retained/v2").join(format!(
        "{}-{}",
        hex::encode([77; 32]),
        hex::encode([88; 32])
    ));
    let active = retained::current(&lineage).unwrap().unwrap();
    for i in 2..602 {
        fs::write(active.join(format!("{i:016x}.delta")), &bytes).unwrap();
    }
    let scratch = store.root.join("store-work-v1").join(format!(
        "retain-{}",
        hex::encode(rand::random::<[u8; 16]>())
    ));
    let mut selector = Selection::new(&store, scratch, None).unwrap();
    let mut steps = 0;
    loop {
        let result = selector
            .step(WorkLimit {
                entries: 1,
                ..Default::default()
            })
            .unwrap();
        assert!(selector.stats().last_step_work <= 1);
        steps += 1;
        if matches!(result, Step::Done(_)) {
            break;
        }
    }
    assert!(steps > 600);
    assert!(selector.stats().peak_metadata_entries <= 128);
    assert!(selector.stats().peak_metadata_bytes <= 512 * 1024);
    assert_eq!(
        fs::read_dir(active).unwrap().count(),
        601,
        "opaque foreign history has no retirement proof"
    );
}

#[test]
fn retirement_pin_survives_failed_source_barrier_on_retry_and_reopen() {
    let (_tmp, store) = setup();
    let scratch = store.root.join("store-work-v1").join(format!(
        "retain-{}",
        hex::encode(rand::random::<[u8; 16]>())
    ));
    let mut selector = Selection::new(&store, scratch, None).unwrap();
    while !matches!(selector.phase, Phase::Gc) {
        selector.step(WorkLimit::default()).unwrap();
    }
    let generation = selector.staged.clone();
    let mut plan = File::open(generation.join("gc.manifest")).unwrap();
    let target = read_meta(&mut plan).unwrap().unwrap();
    let source = store.root.join(target.source);
    let pin = generation.join(format!("gc-{:016x}.delta", target.pin));
    let fault = crate::store::test_barrier::fail(source.parent().unwrap(), "sync-dir");
    assert!(selector.step(WorkLimit::default()).is_err());
    assert!(
        !source.exists(),
        "injected barrier failure follows the real source unlink"
    );
    assert!(
        pin.exists(),
        "failed source barrier retains its durable witness"
    );
    assert!(
        selector.step(WorkLimit::default()).is_err(),
        "NotFound retry still requires source barrier"
    );
    assert!(
        pin.exists(),
        "retry must not retire witness without source barrier"
    );
    drop(selector);
    let mut reopened_store = WorkspaceStore::open(
        store.root.parent().unwrap().parent().unwrap(),
        store.workspace_id,
        true,
    )
    .unwrap();
    reopened_store
        .bind_shell_authority(store.shell_authority().unwrap().clone())
        .unwrap();
    let mut reopened = SelectedRead::new(&reopened_store).unwrap().unwrap();
    loop {
        match reopened.step() {
            Ok(Step::More) => (),
            Ok(Step::Done(())) => panic!("reopen bypassed failed source barrier"),
            Err(_) => break,
        }
    }
    assert!(
        pin.exists(),
        "reopen must retain witness while source barrier fails"
    );
    drop(fault);
    while matches!(reopened.step().unwrap(), Step::More) {}
    assert!(!pin.exists());
    assert!(!source.exists());
    assert!(reopened.basis.complete());
}
