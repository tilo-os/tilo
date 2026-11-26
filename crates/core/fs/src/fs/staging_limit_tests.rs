use super::*;
use crate::namespace::ROOT_INO;
use crate::store::ContentStore;

fn mount() -> (QuickFS, [u64; 3]) {
    let hydrator = Arc::new(Hydrator::new(Arc::new(ContentStore::memory())));
    let namespace = Arc::new(Mutex::new(Namespace::new("staging-limits").unwrap()));
    let inos = ["a.bin", "b.bin", "c.bin"].map(|name| {
        namespace
            .lock()
            .unwrap()
            .create_file(ROOT_INO, name, None, 0, 1)
            .unwrap()
    });
    (QuickFS::over(namespace, hydrator), inos)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn memory_writes_and_truncates_enforce_limits_without_allocating_holes() {
    let (fs, [a, b, c]) = mount();
    let huge = MAX_STAGED_FILE_BYTES.saturating_mul(1024);
    assert_eq!(fs.write_buffer(a, huge, b"x"), Err(EFBIG));
    assert_eq!(fs.staged_len(a), Some(0));
    assert_eq!(fs.truncate_inode(a, huge), Err(EFBIG));
    assert_eq!(fs.truncate_inode(b, huge), Err(EFBIG));
    assert_eq!(fs.staged_len(b), None);
    for ino in [a, b] {
        fs.write_buffer(ino, 0, b"x").unwrap();
        fs.truncate_inode(ino, MAX_STAGED_FILE_BYTES).unwrap();
    }
    fs.write_buffer(c, 0, b"").unwrap();
    assert_eq!(fs.write_buffer(c, 0, b"x"), Err(ENOSPC));
    assert_eq!(fs.truncate_inode(c, 1), Err(ENOSPC));
    assert_eq!(fs.staged_len(c), Some(0));
    assert_eq!(
        fs.truncate_inode(a, MAX_STAGED_FILE_BYTES.saturating_add(1)),
        Err(EFBIG)
    );
    assert_eq!(fs.staged_len(a), Some(MAX_STAGED_FILE_BYTES));
    let retained = fs.stage(a).unwrap();
    fs.drop_buffer(a);
    assert_eq!(fs.truncate_inode(c, 1), Err(ENOSPC));
    drop(retained);
    fs.write_buffer(c, 0, b"x").unwrap();
    assert_eq!(fs.staged_bytes(c).unwrap(), b"x");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn sparse_stages_are_not_subject_to_memory_limits_but_fallback_is() {
    let (fs, [a, b, c]) = mount();
    let dir = tempfile::tempdir().unwrap();
    let fs = fs.with_write_stage_dir(dir.path().join("stages"));
    let offset = MAX_STAGED_TOTAL_BYTES.saturating_add(1);
    fs.write_buffer(a, offset, b"x").unwrap();
    assert_eq!(fs.staged_len(a), Some(offset.saturating_add(1)));
    assert_eq!(
        fs.read_stage_blocking(&fs.stage(a).unwrap(), offset, 1)
            .unwrap(),
        b"x"
    );
    fs.write_buffer(b, 0, b"b").unwrap();
    fs.truncate_inode(b, offset).unwrap();
    assert_eq!(fs.staged_len(b), Some(offset));
    std::fs::remove_dir_all(fs.stage_dir.as_ref().unwrap()).unwrap();
    fs.buffers().insert(c, fs.empty_stage().shared());
    assert_eq!(fs.write_buffer(c, offset, b"x"), Err(EFBIG));
    assert_eq!(fs.truncate_inode(c, offset), Err(EFBIG));
    assert_eq!(fs.staged_len(c), Some(0));
}
