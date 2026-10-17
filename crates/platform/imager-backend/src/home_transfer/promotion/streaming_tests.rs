//! Real private-workspace promotion/replay qualification. No granted source or
//! successful adoption is manufactured by substituting a test operation.
use super::*;
use std::os::unix::{
    ffi::OsStringExt,
    fs::{MetadataExt, OpenOptionsExt},
};
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct LegacyPlan {
    version: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    root_acl: Option<root_acl::Intent>,
    #[serde(default)]
    applications: bool,
    quarantine: durable::Identity,
    home: durable::Identity,
    source_items: Vec<NativePath>,
    entries: Vec<LegacyEntry>,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct LegacyEntry {
    path: NativePath,
    fact: Fact,
    items: Vec<NativePath>,
    parent_mode: u32,
    kind: Kind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    seed: Option<seeds::Witness>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    structure: Option<Vec<structure::Item>>,
}
fn fixture() -> tempfile::TempDir {
    let root = PathBuf::from(
        std::env::var_os("TILO_FS_NATIVE_STORE").expect("owned supported native store required"),
    );
    let directory = tempfile::Builder::new()
        .prefix("promotion-stream-")
        .permissions(std::fs::Permissions::from_mode(0o700))
        .tempdir_in(root)
        .unwrap();
    for path in [
        "quarantine",
        "target",
        "target/.config/tilo",
        "target/.local/state/tilo",
        "workspace",
    ] {
        std::fs::create_dir_all(directory.path().join(path)).unwrap();
        std::fs::set_permissions(
            directory.path().join(path),
            std::fs::Permissions::from_mode(0o700),
        )
        .unwrap();
    }
    directory
}
#[test]
fn streaming_v1_nested_spill_reopens_real_moves_and_keeps_native_fidelity() {
    let fixture = fixture();
    let q = fixture.path().join("quarantine");
    let h = fixture.path().join("target");
    let j = fixture.path().join("workspace/adoption");
    let nested = Path::new("large/a/b/c/d/e/f/g");
    std::fs::create_dir_all(q.join(nested)).unwrap();
    for index in (0u32..1100).rev() {
        std::fs::write(
            q.join(nested).join(format!("file-{index:04}")),
            index.to_le_bytes(),
        )
        .unwrap();
    }
    let raw = std::ffi::OsString::from_vec(vec![b'r', 0xff, b'w']);
    std::fs::write(q.join("large").join(&raw), b"native bytes").unwrap();
    std::fs::hard_link(q.join("large").join(&raw), q.join("large/alias")).unwrap();
    std::os::unix::fs::symlink(&raw, q.join("large/link")).unwrap();
    let mut sparse = std::fs::File::create(q.join("large/sparse")).unwrap();
    sparse.seek(SeekFrom::Start(8 * 1024 * 1024)).unwrap();
    sparse.write_all(b"extent").unwrap();
    sparse.sync_all().unwrap();
    drop(sparse);
    std::fs::write(q.join("later"), b"second move").unwrap();
    let review = PromotionReview::review(&q, &h, &j).unwrap();
    let canonical = serde_json::to_vec(&review.plan).unwrap();
    // The retained v1 wire has ordinary array values, never cache ranges or a
    // second protocol. Independent serde Value emits identical canonical bytes.
    let legacy: LegacyPlan = serde_json::from_slice(&canonical).unwrap();
    assert_eq!(serde_json::to_vec(&legacy).unwrap(), canonical);
    let wire: serde_json::Value = serde_json::from_slice(&canonical).unwrap();
    assert_eq!(wire["version"], 1);
    assert!(wire["source_items"].as_array().unwrap().len() > 1100);
    assert!(
        wire["entries"]
            .as_array()
            .unwrap()
            .iter()
            .all(|entry| entry["items"].is_array())
    );
    assert_eq!(
        plan_digest(&review.plan).unwrap(),
        <[u8; 32]>::from(Sha256::digest(&canonical))
    );
    review.move_one_for_test(Path::new("large")).unwrap();
    assert!(!q.join("large").exists());
    assert!(q.join("later").exists());
    let durable = std::fs::read(&j).unwrap();
    assert_eq!(
        durable.split(|byte| *byte == b'\n').next().unwrap(),
        canonical
    );
    // A partial checkpoint after a durable move is not promoted to completion.
    std::fs::OpenOptions::new()
        .append(true)
        .open(&j)
        .unwrap()
        .write_all(b"999")
        .unwrap();
    let reopened = PromotionReview::recover(&q, &h, &j).unwrap();
    assert_eq!(
        plan_digest(&reopened.plan).unwrap(),
        plan_digest(&review.plan).unwrap()
    );
    assert_eq!(std::fs::read(&j).unwrap(), durable);
    let report = reopened.run(true, &AtomicBool::new(false)).unwrap();
    assert!(report.complete());
    assert!(report.installed.len() > 1100);
    assert_eq!(
        report.summary(false).unwrap()["installed"]
            .as_array()
            .unwrap()
            .len(),
        8
    );
    let mut offset = 0usize;
    let mut observed = Vec::new();
    loop {
        let page = report.result_page("installed", offset, 16, false).unwrap();
        assert_eq!(page["phase"], "finished");
        assert_eq!(page["verified"], true);
        assert_eq!(
            page["total"].as_u64().unwrap(),
            report.installed.len() as u64
        );
        let items = page["items"].as_array().unwrap();
        assert!(items.len() <= 16);
        for item in items {
            let native: NativePath = serde_json::from_value(item["native"].clone()).unwrap();
            observed.push(native.decode().unwrap());
        }
        match page["next"].as_u64() {
            Some(next) => {
                assert!(next > offset as u64);
                offset = next as usize;
            }
            None => break,
        }
    }
    assert_eq!(
        observed,
        report
            .installed
            .iter()
            .collect::<io::Result<Vec<_>>>()
            .unwrap()
    );
    assert!(observed.contains(&Path::new("large").join(&raw)));
    assert!(
        report
            .result_page("installed", report.installed.len() + 1, 1, false)
            .is_err()
    );
    assert!(report.result_page("will_install", 0, 1, false).is_err());
    reopened.verify_completed().unwrap();
    assert_eq!(
        std::fs::read(h.join("large").join(&raw)).unwrap(),
        b"native bytes"
    );
    assert_eq!(
        std::fs::metadata(h.join("large").join(&raw)).unwrap().ino(),
        std::fs::metadata(h.join("large/alias")).unwrap().ino()
    );
    assert_eq!(
        std::fs::read_link(h.join("large/link")).unwrap(),
        PathBuf::from(raw)
    );
    let sparse = std::fs::metadata(h.join("large/sparse")).unwrap();
    assert_eq!(sparse.len(), 8 * 1024 * 1024 + 6);
    assert!(sparse.blocks() * 512 < sparse.len() / 2);
    assert_eq!(std::fs::read(h.join("later")).unwrap(), b"second move");
}
#[test]
fn streaming_replay_refuses_oversize_row_and_quota_without_publishing_receipts() {
    let fixture = fixture();
    let q = fixture.path().join("quarantine");
    let h = fixture.path().join("target");
    let j = fixture.path().join("workspace/adoption");
    std::fs::write(q.join("document"), b"owned data").unwrap();
    let review = PromotionReview::review(&q, &h, &j).unwrap();
    let (_, _, parent) = roots(&q, &h, &j).unwrap();
    let mut malicious = serde_json::to_value(&review.plan).unwrap();
    malicious["source_items"][0]["units"] =
        serde_json::Value::Array(vec![serde_json::json!(97); 600_000]);
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&j)
        .unwrap();
    serde_json::to_writer(&mut file, &malicious).unwrap();
    file.write_all(b"\n").unwrap();
    file.sync_all().unwrap();
    drop(file);
    assert!(
        PromotionReview::recover(&q, &h, &j)
            .err()
            .unwrap()
            .contains("metadata row exceeds bounds")
    );
    assert_eq!(std::fs::read(q.join("document")).unwrap(), b"owned data");
    assert!(!h.join("document").exists());
    let scratch = crate::migration_review::InventoryScratch::review(parent.clone(), 4096).unwrap();
    let store = Store::new(scratch.clone(), Arc::new(AtomicBool::new(false))).unwrap();
    let mut rows = store.builder();
    rows.push(NativePath::encode(Path::new("one"))).unwrap();
    let large = "a".repeat(4096);
    assert!(rows.push(NativePath::encode(Path::new(&large))).is_err());
    assert!(store.freeze().is_err());
    drop(rows);
    drop(store);
    let allocation = scratch.allocate(4096).unwrap();
    assert_eq!(allocation.file.metadata().unwrap().len(), 4096);
    drop(allocation);
    assert!(
        PromotionReview::review_with_storage(
            &q,
            &h,
            &fixture.path().join("workspace/cancelled"),
            scratch.clone(),
            &AtomicBool::new(true)
        )
        .is_err()
    );
    assert!(!fixture.path().join("workspace/cancelled").exists());
    let cancelled = Arc::new(AtomicBool::new(true));
    assert!(Store::new(scratch, cancelled).is_err());
}

#[test]
fn checkpoint_membership_rebuilds_exact_durable_ordinals_under_shared_quota() {
    let fixture = fixture();
    let q = fixture.path().join("quarantine");
    let h = fixture.path().join("target");
    let j = fixture.path().join("workspace/adoption");
    for name in ["first", "middle", "last"] {
        std::fs::write(q.join(name), name.as_bytes()).unwrap();
    }
    let review = PromotionReview::review(&q, &h, &j).unwrap();
    // Deliberately checkpoint the final ordinal first. Membership is not a
    // prefix counter and cannot infer that earlier unperformed moves happened.
    review.move_one_for_test(Path::new("middle")).unwrap();
    let (_, _, parent) = roots(&q, &h, &j).unwrap();
    let log = Log::open(
        &parent,
        j.file_name().unwrap(),
        review.plan.entries.store().scratch(),
    )
    .unwrap();
    assert_eq!(log.done.len(), 1);
    for (index, entry) in log.plan.entries.iter().enumerate() {
        let entry = entry.unwrap();
        assert_eq!(
            log.done.contains(index).unwrap(),
            decoded(&entry.path).unwrap() == Path::new("middle")
        );
    }
    assert!(log.done.contains(log.plan.entries.len()).is_err());
    drop(log);
    let recovered = PromotionReview::recover(&q, &h, &j).unwrap();
    assert!(
        recovered
            .run(true, &AtomicBool::new(false))
            .unwrap()
            .complete()
    );
    recovered.verify_completed().unwrap();
    for name in ["first", "middle", "last"] {
        assert_eq!(std::fs::read(h.join(name)).unwrap(), name.as_bytes());
    }
    let scratch = review.plan.entries.store().scratch();
    let mut derived = Done::new(3, scratch.clone()).unwrap();
    assert!(derived.insert(2).unwrap());
    assert!(!derived.insert(2).unwrap());
    assert_eq!(derived.len(), 1);
    assert!(!derived.contains(0).unwrap());
    // Losing private scratch custody invalidates every cached query, even when
    // the anonymous membership bytes themselves remain readable.
    std::fs::rename(
        fixture.path().join("workspace"),
        fixture.path().join("retained-workspace"),
    )
    .unwrap();
    std::fs::create_dir(fixture.path().join("workspace")).unwrap();
    assert!(derived.contains(2).is_err());
}

fn long_path_fixture(root: &Path, count: u32) -> PathBuf {
    let mut nested = PathBuf::from("documents");
    for _ in 0..8 {
        nested.push("d".repeat(200));
    }
    std::fs::create_dir_all(root.join(&nested)).unwrap();
    for index in 0_u32..count {
        std::fs::write(
            root.join(&nested).join(format!("file-{index:05}")),
            index.to_le_bytes(),
        )
        .unwrap();
    }
    std::fs::hard_link(
        root.join(&nested).join("file-00000"),
        root.join(&nested).join("alias"),
    )
    .unwrap();
    std::os::unix::fs::symlink("file-00000", root.join(&nested).join("link")).unwrap();
    std::fs::write(root.join(&nested).join(".hidden"), b"hidden app data").unwrap();
    std::fs::write(root.join("later"), b"later actual move").unwrap();
    nested
}
fn verify_long_path_home(root: &Path, nested: &Path, count: u32) {
    assert_eq!(
        std::fs::read_dir(root.join(nested)).unwrap().count(),
        count as usize + 3
    );
    for index in [0_u32, 1, count - 1] {
        assert_eq!(
            std::fs::read(root.join(nested).join(format!("file-{index:05}"))).unwrap(),
            index.to_le_bytes()
        );
    }
    assert_eq!(
        std::fs::read(root.join(nested).join(".hidden")).unwrap(),
        b"hidden app data"
    );
    assert_eq!(
        std::fs::read_link(root.join(nested).join("link")).unwrap(),
        Path::new("file-00000")
    );
    assert_eq!(
        std::fs::metadata(root.join(nested).join("alias"))
            .unwrap()
            .ino(),
        std::fs::metadata(root.join(nested).join("file-00000"))
            .unwrap()
            .ino()
    );
    assert_eq!(
        std::fs::read(root.join("later")).unwrap(),
        b"later actual move"
    );
}
#[test]
fn segmented_long_path_home_reopens_real_partial_move_and_root_acl_receipt() {
    let started = std::time::Instant::now();
    let phase = |name: &str| {
        eprintln!(
            "segmented-native-phase {name} elapsed-ms={}",
            started.elapsed().as_millis()
        );
    };
    phase("fixture-start");
    let fixture = fixture();
    let q = fixture.path().join("quarantine");
    let h = fixture.path().join("target");
    let j = fixture.path().join("workspace/adoption");
    let nested = long_path_fixture(&q, 16_000);
    phase("source-created");
    let review = PromotionReview::review(&q, &h, &j)
        .unwrap()
        .with_home_root_acl()
        .unwrap();
    phase("review-complete");
    let digest = plan_digest(&review.plan).unwrap();
    phase("digest-complete");
    phase("first-move-start");
    review.move_one_for_test(Path::new("documents")).unwrap();
    phase("first-move-complete");
    assert!(!q.join("documents").exists());
    assert!(q.join("later").exists());
    let line = std::fs::read(&j).unwrap();
    let line = line.split(|b| *b == b'\n').next().unwrap();
    let header: serde_json::Value = serde_json::from_slice(line).unwrap();
    assert_eq!(header["version"], 2);
    assert!(header["bytes"].as_u64().unwrap() > MAX as u64);
    let segments = header["segments"].as_array().unwrap();
    assert!(segments.len() >= 2);
    for segment in segments {
        assert!(segment["bytes"].as_u64().unwrap() <= MAX as u64);
    }
    phase("recover-start");
    let recovered = PromotionReview::recover(&q, &h, &j).unwrap();
    phase("recover-complete");
    assert_eq!(plan_digest(&recovered.plan).unwrap(), digest);
    assert!(
        recovered
            .run(true, &AtomicBool::new(false))
            .unwrap()
            .complete()
    );
    phase("remaining-moves-complete");
    recovered.verify_completed().unwrap();
    phase("verify-complete");
    verify_long_path_home(&h, &nested, 16_000);
    phase("content-checks-complete");
    assert!(
        PromotionReview::recover(&q, &h, &j)
            .unwrap()
            .run(true, &AtomicBool::new(false))
            .unwrap()
            .complete()
    );
    phase("retry-complete");
}
#[test]
fn segmented_plan_actual_process_crashes_preserve_source_and_recover_committed_root() {
    const ROOT: &str = "TILO_SEGMENTED_ADOPTION_CRASH_ROOT";
    if let Some(root) = std::env::var_os(ROOT) {
        let root = PathBuf::from(root);
        let review = PromotionReview::review(
            &root.join("quarantine"),
            &root.join("target"),
            &root.join("workspace/adoption"),
        )
        .unwrap()
        .with_home_root_acl()
        .unwrap();
        // Must terminate inside the chosen genuine production boundary.
        let result = review.run(false, &AtomicBool::new(false));
        panic!(
            "requested real process-crash boundary not reached: {}",
            result.is_ok()
        );
    }
    for boundary in [
        "adoption_segment_published",
        "adoption_root_published",
        "adoption_moved_before_checkpoint",
    ] {
        let fixture = fixture();
        let q = fixture.path().join("quarantine");
        let h = fixture.path().join("target");
        let j = fixture.path().join("workspace/adoption");
        let nested = long_path_fixture(&q, 6_000);
        let selector = format!(
            "{}::segmented_plan_actual_process_crashes_preserve_source_and_recover_committed_root",
            module_path!().split_once("::").unwrap().1
        );
        // Native qualification executes the authenticated compiler artifact
        // from a sealed memfd. current_exe() reports its deleted display name;
        // this kernel path re-executes our actual admitted running inode.
        let mut child = std::process::Command::new("/proc/self/exe")
            .args(["--exact", &selector, "--nocapture"])
            .env(ROOT, fixture.path())
            .env("TILO_JOURNAL_BOUNDARY", boundary)
            .env("TILO_JOURNAL_BOUNDARY_COUNT", "1")
            .spawn()
            .unwrap();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(120);
        let status = loop {
            if let Some(status) = child.try_wait().unwrap() {
                break status;
            }
            if std::time::Instant::now() >= deadline {
                child.kill().unwrap();
                child.wait().unwrap();
                panic!("owned segmented crash worker timed out at {boundary}");
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        };
        assert_eq!(
            status.code(),
            Some(73),
            "actual boundary {boundary} must execute; selector cannot silently pass zero tests"
        );
        let review = if boundary == "adoption_segment_published" {
            assert!(!j.exists());
            verify_long_path_home(&q, &nested, 6_000);
            assert!(!h.join("documents").exists());
            PromotionReview::review(&q, &h, &j)
                .unwrap()
                .with_home_root_acl()
                .unwrap()
        } else {
            PromotionReview::recover(&q, &h, &j).unwrap()
        };
        assert!(
            review
                .run(j.exists(), &AtomicBool::new(false))
                .unwrap()
                .complete()
        );
        review.verify_completed().unwrap();
        verify_long_path_home(&h, &nested, 6_000);
    }
}
#[test]
fn legacy_small_home_still_publishes_and_reopens_exact_v1_journal() {
    let fixture = fixture();
    let q = fixture.path().join("quarantine");
    let h = fixture.path().join("target");
    let j = fixture.path().join("workspace/adoption");
    std::fs::write(q.join("document"), b"legacy semantic bytes").unwrap();
    let review = PromotionReview::review(&q, &h, &j)
        .unwrap()
        .with_home_root_acl()
        .unwrap();
    let expected = serde_json::to_vec(&review.plan).unwrap();
    assert!(
        review
            .run(false, &AtomicBool::new(false))
            .unwrap()
            .complete()
    );
    let actual = std::fs::read(&j).unwrap();
    assert_eq!(actual.split(|b| *b == b'\n').next().unwrap(), expected);
    // The historical producer committed via hard_link before its temporary
    // unlink. Reconstruct that actual inode topology; mutable locked-root
    // accounting must not strand previously valid v1 crash recovery.
    let old_temporary = j.with_file_name(".tilo-copy-legacy-crash");
    std::fs::hard_link(&j, &old_temporary).unwrap();
    assert_eq!(std::fs::metadata(&j).unwrap().nlink(), 2);
    PromotionReview::recover(&q, &h, &j)
        .unwrap()
        .verify_completed()
        .unwrap();
    assert_eq!(std::fs::read(&j).unwrap(), actual);
    assert_eq!(std::fs::read(&old_temporary).unwrap(), actual);
    assert_eq!(
        std::fs::read(h.join("document")).unwrap(),
        b"legacy semantic bytes"
    );
}

#[test]
fn actual_segment_digest_and_missing_segment_refuse_before_first_move() {
    for missing in [false, true] {
        let fixture = fixture();
        let q = fixture.path().join("quarantine");
        let h = fixture.path().join("target");
        let j = fixture.path().join("workspace/adoption");
        let nested = long_path_fixture(&q, 6_000);
        let review = PromotionReview::review(&q, &h, &j)
            .unwrap()
            .with_home_root_acl()
            .unwrap();
        let (_, _, parent) = roots(&q, &h, &j).unwrap();
        let log = Log::create(&parent, j.file_name().unwrap(), review.plan.clone()).unwrap();
        drop(log);
        let bytes = std::fs::read(&j).unwrap();
        let header: serde_json::Value =
            serde_json::from_slice(bytes.split(|b| *b == b'\n').next().unwrap()).unwrap();
        let segment = j
            .parent()
            .unwrap()
            .join(format!(
                ".tilo-adoption-plan-{}",
                header["nonce"].as_str().unwrap()
            ))
            .join("segment-0000");
        if missing {
            std::fs::remove_file(&segment).unwrap();
        } else {
            use std::os::unix::fs::FileExt;
            let file = std::fs::OpenOptions::new()
                .read(true)
                .write(true)
                .open(&segment)
                .unwrap();
            let length = file.metadata().unwrap().len();
            let mut prefix = vec![0; 65_536];
            file.read_exact_at(&mut prefix, 0).unwrap();
            let key = b"\"applications\":false";
            let position = prefix
                .windows(key.len())
                .position(|bytes| bytes == key)
                .unwrap();
            let value = position + b"\"applications\":".len();
            // Same length and valid JSON: only the authenticated consent value
            // changes. Refusal must come from segment authentication, not syntax.
            file.write_all_at(b"true ", value as u64).unwrap();
            file.sync_all().unwrap();
            assert_eq!(file.metadata().unwrap().len(), length);
        }
        let error = match PromotionReview::recover(&q, &h, &j) {
            Ok(_) => panic!("corrupt/missing actual committed segment must refuse recovery"),
            Err(error) => error,
        };
        if !missing {
            assert!(error.contains("Adoption segment digest changed"), "{error}");
        }
        verify_long_path_home(&q, &nested, 6_000);
        assert!(!h.join("documents").exists());
        assert_eq!(
            std::fs::read(&j).unwrap(),
            bytes,
            "refused recovery must not append/checkpoint or truncate committed intent"
        );
    }
}

#[test]
fn actual_durable_plan_charge_retains_quota_and_reopen_adds_no_free_allowance() {
    let fixture = fixture();
    let workspace = fixture.path().join("workspace");
    let pin = PinnedDirectory::open_absolute(&workspace).unwrap();
    let scratch = crate::migration_review::InventoryScratch::review(pin, 4096).unwrap();
    let mut writing = scratch.reserve_metadata().unwrap();
    writing.grow(4096).unwrap();
    assert!(
        scratch.allocate(1).is_err(),
        "live plan bytes retain the actual shared quota"
    );
    drop(writing);
    let file = std::fs::OpenOptions::new()
        .create_new(true)
        .read(true)
        .write(true)
        .mode(0o600)
        .open(workspace.join("actual-existing-plan"))
        .unwrap();
    file.set_len(4096).unwrap();
    file.sync_all().unwrap();
    let mut reopened = scratch.reserve_metadata().unwrap();
    reopened.retain_file(&file).unwrap();
    let allocation = scratch.allocate(4096).unwrap();
    assert!(
        scratch.allocate(1).is_err(),
        "existing bytes add equal admission and charge, never more available quota"
    );
    drop(reopened);
    assert!(
        scratch.allocate(1).is_err(),
        "retirement restores the original allowance while live anonymous metadata stays charged"
    );
    drop(allocation);
    assert!(scratch.allocate(4096).is_ok());
}

#[test]
fn actual_long_path_plan_replay_scans_only_consumed_json_bytes() {
    let fixture = fixture();
    let q = fixture.path().join("quarantine");
    let h = fixture.path().join("target");
    let j = fixture.path().join("workspace/adoption");
    let nested = long_path_fixture(&q, 64);
    let reviewed = PromotionReview::review(&q, &h, &j).unwrap();
    let expected_digest = plan_digest(&reviewed.plan).unwrap();
    let expected = serde_json::to_vec(&reviewed.plan).unwrap();
    assert!(
        expected.len() > 8192,
        "actual Plan must cross buffered windows"
    );
    let mut wire = expected.clone();
    wire.push(b'\n');
    let mut reader = std::io::BufReader::with_capacity(8192, std::io::Cursor::new(wire));
    let mut line = replay::Line::segmented(&mut reader);
    let store = Store::new(
        reviewed.plan.entries.store().scratch(),
        Arc::new(AtomicBool::new(false)),
    )
    .unwrap();
    let recovered = line.parse(store.clone()).unwrap();
    assert_eq!(line.bytes, expected.len() as u64 + 1);
    assert_eq!(
        line.scanned_bytes,
        expected.len() as u64,
        "real serde_json byte-reader must not repeatedly scan the unconsumed buffer suffix"
    );
    eprintln!(
        "segmented-linear-replay bytes={} inspected={}",
        expected.len(),
        line.scanned_bytes
    );
    drop(line);
    validate_plan(&recovered).unwrap();
    store.freeze().unwrap();
    assert_eq!(plan_digest(&recovered).unwrap(), expected_digest);
    assert_eq!(serde_json::to_vec(&recovered).unwrap(), expected);
    // Replay is metadata-only and cannot adopt/mutate source or target.
    verify_long_path_home(&q, &nested, 64);
    assert!(!j.exists());
    assert!(!h.join("documents").exists());
}
