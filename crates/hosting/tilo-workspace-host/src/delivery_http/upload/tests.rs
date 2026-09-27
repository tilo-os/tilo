use super::*;
use crate::{https::ingress_channel, store::SessionStore};
struct Fixture {
    _temp: tempfile::TempDir,
    _store: SessionStore,
    pool: HostBytes,
    directory: crate::store::StagingDirectory,
}
impl Fixture {
    fn new() -> Self {
        let temp = tempfile::tempdir().unwrap();
        let store = SessionStore::create(&temp.path().join("owner")).unwrap();
        let (ingress, _) = ingress_channel();
        let pool = ingress.host_bytes();
        let inventory = ingress.host_work();
        let directory = store.staging_directory(&inventory, 7).unwrap();
        Self {
            _temp: temp,
            _store: store,
            pool,
            directory,
        }
    }
    fn identity(&self, bytes: &[u8]) -> StageIdentity {
        StageIdentity {
            workspace: 7,
            device: [1; 32],
            delivery: [2; 16],
            digest: *blake3::hash(bytes).as_bytes(),
            total: bytes.len() as u64,
        }
    }
    fn open(&self, identity: StageIdentity, now: u64) -> UploadStage {
        UploadStage::open(self.directory.reserve(identity).unwrap(), &self.pool, now).unwrap()
    }
    fn chunk(&self, identity: StageIdentity, offset: u64, bytes: &[u8]) -> UploadChunk {
        UploadChunk::copy(
            &StateItemChunk::new(
                identity.delivery,
                identity.digest,
                identity.total,
                offset,
                bytes,
            )
            .unwrap(),
            &self.pool,
        )
        .unwrap()
    }
}
#[test]
fn upload_real_files_resume_overlap_and_forward_only_expiry() {
    let f = Fixture::new();
    let data = vec![7; 400 * 1024];
    let identity = f.identity(&data);
    let stage = f.open(identity, 100);
    let stage = stage
        .append(f.chunk(identity, 0, &data[..200 * 1024]), 200)
        .unwrap();
    assert_eq!(stage.progress().unwrap().next_offset(), 200 * 1024);
    let expiry = stage.metadata.expires_at_ns;
    let stage = stage
        .append(f.chunk(identity, 0, &data[..200 * 1024]), 300)
        .unwrap();
    assert_eq!(stage.metadata.expires_at_ns, expiry);
    let stage = stage
        .append(
            f.chunk(identity, 100 * 1024, &data[100 * 1024..300 * 1024]),
            400,
        )
        .unwrap();
    assert_eq!(stage.progress().unwrap().next_offset(), 300 * 1024);
    assert_eq!(stage.metadata.expires_at_ns, 400 + UPLOAD_IDLE_NS);
    drop(stage);
    let stage = f.open(identity, 500);
    assert_eq!(stage.progress().unwrap().next_offset(), 300 * 1024);
    let stage = stage
        .append(f.chunk(identity, 300 * 1024, &data[300 * 1024..]), 600)
        .unwrap();
    let complete = stage.complete().unwrap();
    assert_eq!(*complete.identity(), identity);
    let (mut file, _, _reservation, _credit) = complete.into_parts();
    let mut restored = Vec::new();
    file.read_to_end(&mut restored).unwrap();
    assert_eq!(restored, data);
}
#[test]
fn upload_conflict_and_final_digest_mismatch_preserve_durable_prefix() {
    let f = Fixture::new();
    let data = vec![3; 300 * 1024];
    let identity = f.identity(&data);
    let stage = f
        .open(identity, 100)
        .append(f.chunk(identity, 0, &data[..200 * 1024]), 200)
        .unwrap();
    assert!(matches!(
        stage.append(f.chunk(identity, 100, &[4; 20]), 300),
        Err(UploadFailure::Conflict)
    ));
    let stage = f.open(identity, 400);
    assert_eq!(stage.metadata.prefix, 200 * 1024);
    assert!(matches!(
        stage.append(f.chunk(identity, 200 * 1024, &vec![4; 100 * 1024]), 500),
        Err(UploadFailure::Conflict)
    ));
    let stage = f.open(identity, 600);
    assert_eq!(stage.metadata.prefix, 200 * 1024);
    assert_eq!(stage.file.metadata().unwrap().len(), 200 * 1024);
}
#[test]
fn upload_reopen_discards_only_uncommitted_tail_and_refuses_expired_stage() {
    let f = Fixture::new();
    let data = vec![3; 300 * 1024];
    let identity = f.identity(&data);
    let mut stage = f
        .open(identity, 100)
        .append(f.chunk(identity, 0, &data[..200 * 1024]), 200)
        .unwrap();
    stage.file.seek(std::io::SeekFrom::End(0)).unwrap();
    stage.file.write_all(&[9; 1024]).unwrap();
    stage.file.sync_all().unwrap();
    let expiry = stage.metadata.expires_at_ns;
    drop(stage);
    let stage = f.open(identity, 300);
    assert_eq!(stage.file.metadata().unwrap().len(), 200 * 1024);
    drop(stage);
    assert!(matches!(
        UploadStage::open(f.directory.reserve(identity).unwrap(), &f.pool, expiry),
        Err(UploadFailure::Expired)
    ));
}

#[test]
fn upload_rejects_foreign_chunk_budget_before_file_mutation() {
    let f = Fixture::new();
    let identity = f.identity(b"abc");
    let stage = f.open(identity, 100);
    let (foreign, _) = ingress_channel();
    let chunk = UploadChunk::copy(
        &StateItemChunk::new(
            identity.delivery,
            identity.digest,
            identity.total,
            0,
            b"abc",
        )
        .unwrap(),
        &foreign.host_bytes(),
    )
    .unwrap();
    assert!(matches!(
        stage.append(chunk, 200),
        Err(UploadFailure::Conflict)
    ));
    assert_eq!(f.open(identity, 300).metadata.prefix, 0);
    assert!(matches!(
        UploadStage::open(
            f.directory.reserve(identity).unwrap(),
            &foreign.host_bytes(),
            400
        ),
        Err(UploadFailure::Conflict)
    ));
}
#[test]
fn upload_streams_actual_64_mib_file_with_bounded_chunks() {
    let f = Fixture::new();
    let bytes = vec![7; 240 * 1024];
    let total = 64 * 1024 * 1024;
    let mut hash = blake3::Hasher::new();
    let mut offset = 0;
    while offset < total {
        let n = (total - offset).min(bytes.len());
        hash.update(&bytes[..n]);
        offset += n;
    }
    let identity = StageIdentity {
        workspace: 7,
        device: [1; 32],
        delivery: [2; 16],
        digest: *hash.finalize().as_bytes(),
        total: total as u64,
    };
    let mut stage = f.open(identity, 100);
    let mut offset = 0;
    while offset < total {
        let n = (total - offset).min(bytes.len());
        stage = stage
            .append(
                f.chunk(identity, offset as u64, &bytes[..n]),
                200 + offset as u64,
            )
            .unwrap();
        offset += n;
    }
    let complete = stage.complete().unwrap();
    assert_eq!(complete.identity().total, total as u64);
    let (file, _, reservation, credit) = complete.into_parts();
    assert_eq!(file.metadata().unwrap().len(), total as u64);
    drop(file);
    reservation.remove().unwrap();
    drop(credit);
}
#[test]
fn cancelled_queued_real_upload_worker_keeps_backing_credit_until_completion() {
    let f = Fixture::new();
    let identity = f.identity(&vec![7; 400 * 1024]);
    let baseline = f.pool.available_bytes();
    let stage = f.open(identity, 100);
    let chunk = f.chunk(identity, 0, &vec![7; 200 * 1024]);
    let charged = f.pool.available_bytes();
    assert!(charged < baseline);
    let runtime = tokio::runtime::Builder::new_current_thread()
        .max_blocking_threads(1)
        .build()
        .unwrap();
    let (entered_tx, entered_rx) = std::sync::mpsc::channel();
    let (release_tx, release_rx) = std::sync::mpsc::channel();
    let blocker = runtime.spawn_blocking(move || {
        entered_tx.send(()).unwrap();
        release_rx.recv().unwrap();
    });
    entered_rx
        .recv_timeout(std::time::Duration::from_secs(5))
        .unwrap();
    let (result_tx, result_rx) = std::sync::mpsc::sync_channel(1);
    drop(result_rx);
    let (done_tx, done_rx) = std::sync::mpsc::channel();
    let worker = runtime.spawn_blocking(move || {
        let result = stage.append(chunk, 200);
        drop(result_tx.send(result));
        done_tx.send(()).unwrap();
    });
    drop(worker);
    assert_eq!(f.pool.available_bytes(), charged);
    release_tx.send(()).unwrap();
    done_rx
        .recv_timeout(std::time::Duration::from_secs(5))
        .unwrap();
    assert_eq!(f.pool.available_bytes(), baseline);
    runtime.block_on(blocker).unwrap();
    assert_eq!(
        f.open(identity, 300).metadata.prefix,
        200 * 1024,
        "cancelled delivery did not fabricate a receipt or lose actual durable prefix"
    );
}

#[test]
fn upload_review_reopened_prefix_requires_successful_recovery_directory_barrier() {
    let f = Fixture::new();
    let data = vec![7; 400 * 1024];
    let identity = f.identity(&data);
    let idle = f.pool.available_bytes();
    let stage = f.open(identity, 100);
    let failed = f
        .directory
        .with_barrier_fault(|| stage.append(f.chunk(identity, 0, &data[..200 * 1024]), 200));
    assert!(matches!(failed, Err(UploadFailure::Storage)));
    assert_eq!(
        f.pool.available_bytes(),
        idle,
        "failed worker must retire memory, not its physical reservation"
    );
    // The real rename occurred: read exactly what the helper left on disk.
    let persisted = {
        let reservation = f.directory.reserve(identity).unwrap();
        reservation.read_manifest().unwrap().unwrap()
    };
    assert_eq!(persisted.prefix, 200 * 1024);
    assert_eq!(persisted.expires_at_ns, 200 + UPLOAD_IDLE_NS);
    for now in [300, 400] {
        let retry = f.directory.with_barrier_fault(|| {
            UploadStage::open(f.directory.reserve(identity).unwrap(), &f.pool, now)
        });
        assert!(
            matches!(retry, Err(UploadFailure::Storage)),
            "no reopened FlowCredit before a successful directory barrier"
        );
        assert_eq!(f.pool.available_bytes(), idle);
    }
    let stage = f.open(identity, 500);
    assert_eq!(stage.metadata, persisted);
    assert_eq!(stage.progress().unwrap().next_offset(), 200 * 1024);
}

#[test]
fn upload_review_generic_deserialize_cannot_bypass_frame_validation_before_copy() {
    fn raw_bytes<S: serde::Serializer>(bytes: &&[u8], serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_bytes(bytes)
    }
    #[derive(serde::Serialize)]
    struct RawChunk<'a> {
        version: u8,
        delivery: [u8; 16],
        input_digest: [u8; 32],
        canonical_size: u64,
        offset: u64,
        #[serde(serialize_with = "raw_bytes")]
        bytes: &'a [u8],
    }
    let f = Fixture::new();
    let oversized = vec![7; 256 * 1024 + 1];
    for (version, total, offset, bytes) in [
        (1, oversized.len() as u64, 0, oversized.as_slice()),
        (2, 3, 0, b"abc".as_slice()),
        (1, 1, 0, b"".as_slice()),
        (1, 1, u64::MAX, b"a".as_slice()),
    ] {
        let encoded = postcard::to_stdvec(&RawChunk {
            version,
            delivery: [1; 16],
            input_digest: [2; 32],
            canonical_size: total,
            offset,
            bytes,
        })
        .unwrap();
        // Actual public Deserialize creates an inert typed value without checked decode().
        let typed: StateItemChunk<'_> = postcard::from_bytes(&encoded).unwrap();
        assert!(typed.encoded_len().is_err());
        let idle = f.pool.available_bytes();
        assert!(
            UploadChunk::copy(&typed, &f.pool).is_err(),
            "typed inert value must be validated before allocating its copy"
        );
        assert_eq!(f.pool.available_bytes(), idle);
    }
    let valid = StateItemChunk::new([1; 16], [2; 32], 3, 0, b"abc").unwrap();
    assert!(UploadChunk::copy(&valid, &f.pool).is_ok());
}
