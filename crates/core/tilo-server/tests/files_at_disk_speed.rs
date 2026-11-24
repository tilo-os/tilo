//! A workspace's files move from a device to the server that hosts the workspace about as fast
//! as a plain copy of the same bytes over the same link.
//!
//! The device is a laptop's own workspace service holding the workspace as a member copy, with
//! its Files tree synced by the same tree sync and its content answered from the same content
//! store a mounted folder uses. The server keeps what the tree names, asking the laptop for each
//! blob it lacks, as it does for any member. Both run in this process and talk over loopback.
//!
//! What the move takes is measured against a copy of the same files, read from one directory,
//! streamed over a loopback socket and written into another, timed in the same run on the same
//! machine: a fixed number would pass on a fast machine and fail on a slow one for no reason
//! the code gave.
//!
//! The suite runs a size that takes seconds. `TILO_FILES_SPEED=full` runs the full size (100k
//! pieces, 3 GB), which the `files-at-disk-speed` check runs as a benchmark.

use std::io::{Read as _, Write as _};
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

use tilo_server::WorkspaceId;
use tilo_user_identity::DeviceCapabilities;

use crate::common::{TestUser, own_service, spawn_server};

const WORKSPACE: WorkspaceId = WorkspaceId::from_u128(0x6669_6c65_7370_6565_64);

/// How long the move may take against the plain copy of the same bytes.
const WITHIN_COPY: u32 = 2;
/// The least time the move is given: a copy of a few hundred megabytes takes a fraction of a
/// second, and setting up the first asks of a sync is not what is measured.
const AT_LEAST: Duration = Duration::from_secs(3);

/// How long a record written while the files move takes to reach the server: a frame. Content
/// travels apart from the workspace's records, so a bulk transfer holds no write back.
const LIVE_WITHIN: Duration = Duration::from_millis(16);

/// The files a run moves.
struct Size {
    files: usize,
    /// Every `large_every`th file is large enough to be kept as chunks a manifest lists.
    large_every: usize,
    large: usize,
}

impl Size {
    /// What the run moves: the suite's size, or the full one.
    fn chosen() -> Self {
        if std::env::var("TILO_FILES_SPEED").as_deref() == Ok("full") {
            Self {
                files: 100_000,
                large_every: 1_000,
                large: 4 * 1024 * 1024,
            }
        } else {
            Self {
                files: 10_000,
                large_every: 500,
                large: 2 * 1024 * 1024,
            }
        }
    }

    /// The length of the `index`th file: most are small, as a workspace's files are, from 1 KiB
    /// to about 60 KiB, and every so often one is large.
    fn len_of(&self, index: usize) -> usize {
        if index % self.large_every == self.large_every - 1 {
            return self.large;
        }
        1024 + (index.wrapping_mul(7919) % 59) * 1024 + index % 1000
    }
}

/// The bytes of the `index`th file: unlike any other file's, and not compressible, so neither
/// side saves work the real files would not let it save.
fn bytes_of(index: usize, len: usize) -> Vec<u8> {
    let mut bytes = vec![0_u8; len];
    blake3::Hasher::new()
        .update(&(index as u64).to_le_bytes())
        .finalize_xof()
        .fill(&mut bytes);
    bytes
}

/// Copy every file under `from` into `to` the plain way: each is read, streamed over a loopback
/// socket with its name and length, and written on the other side. How long it took.
fn plain_copy(from: &Path, to: &Path, names: &[String]) -> Duration {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let to = to.to_path_buf();
    let count = names.len();
    let started = Instant::now();
    let receiver = std::thread::spawn(move || {
        let (stream, _) = listener.accept().unwrap();
        let mut stream = std::io::BufReader::with_capacity(1 << 20, stream);
        for _ in 0..count {
            let mut head = [0_u8; 16];
            stream.read_exact(&mut head).unwrap();
            let name_len =
                usize::try_from(u64::from_le_bytes(head[..8].try_into().unwrap())).unwrap();
            let len = usize::try_from(u64::from_le_bytes(head[8..].try_into().unwrap())).unwrap();
            let mut name = vec![0_u8; name_len];
            stream.read_exact(&mut name).unwrap();
            let mut bytes = vec![0_u8; len];
            stream.read_exact(&mut bytes).unwrap();
            std::fs::write(to.join(String::from_utf8(name).unwrap()), bytes).unwrap();
        }
    });
    let mut stream =
        std::io::BufWriter::with_capacity(1 << 20, std::net::TcpStream::connect(address).unwrap());
    for name in names {
        let bytes = std::fs::read(from.join(name)).unwrap();
        stream
            .write_all(&(name.len() as u64).to_le_bytes())
            .unwrap();
        stream
            .write_all(&(bytes.len() as u64).to_le_bytes())
            .unwrap();
        stream.write_all(name.as_bytes()).unwrap();
        stream.write_all(&bytes).unwrap();
    }
    stream.flush().unwrap();
    drop(stream);
    receiver.join().unwrap();
    started.elapsed()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 8)]
async fn a_workspaces_files_reach_its_server_within_twice_a_plain_copy() {
    // `TILO_FILES_SPEED_LOG` names what to log while the files move (a tracing filter).
    if let Ok(filter) = std::env::var("TILO_FILES_SPEED_LOG") {
        let _ = tracing_subscriber::fmt()
            .with_ansi(false)
            .with_env_filter(filter)
            .with_writer(std::io::stderr)
            .try_init();
    }
    let size = Size::chosen();
    let ada = TestUser::fresh("Ada");
    let server_device = ada.enrol("home-server", DeviceCapabilities::member());
    let laptop = ada.enrol("laptop", DeviceCapabilities::primary());
    let server_key = server_device.device_pubkey();
    let server = spawn_server(&ada, server_device, &[WORKSPACE], &[&laptop.cert]).await;
    let hosted = server
        .server
        .workspaces()
        .expect("the server holds workspaces");
    let workspace = WORKSPACE.to_string();
    let hint = hosted
        .hosts(&workspace)
        .await
        .expect("the server signs where it is reached")
        .into_iter()
        .find(|hint| hint.host.device_public_key == server_key)
        .expect("the server names its own address");

    let state = tempfile::tempdir().unwrap();
    let (service, keys) = own_service(state.path(), &laptop).await;
    let copies = service.workspaces().clone();
    tilo_test_env::within(
        "the laptop holds the workspace as a member copy",
        copies.hold_as_member(WORKSPACE, &hint),
    )
    .await
    .expect("the server admits the laptop's membership");

    // The laptop's files, as its mounted folder keeps them: their bytes in its content store,
    // answered to whoever asks the workspace for them, and the tree that names them synced.
    let store = Arc::new(
        tilo_fs::ContentStore::open(state.path().join("content"))
            .await
            .expect("the laptop's content store opens"),
    );
    let routed = tilo_workspace_service::content::routes_content(
        &copies,
        Arc::new(tilo_workspace_service::files::StoreShelf(Arc::clone(
            &store,
        ))),
        laptop.device_pubkey(),
    );
    let tree = tilo_workspace_service::files::start(
        &copies,
        WORKSPACE,
        &state.path().join("tree"),
        (laptop.device_pubkey(), laptop.cert.user_public_key),
        Some(Arc::clone(&store)),
        Some(Arc::clone(&routed.content)),
    )
    .await
    .expect("the laptop's tree opens");

    // The same files twice: in a directory, for the plain copy, and in the laptop's store.
    let plain = tempfile::tempdir().unwrap();
    let (from, to) = (plain.path().join("from"), plain.path().join("to"));
    std::fs::create_dir_all(&from).unwrap();
    std::fs::create_dir_all(&to).unwrap();
    let mut names = Vec::with_capacity(size.files);
    let mut files = Vec::with_capacity(size.files);
    let mut blobs: Vec<String> = Vec::new();
    let mut total = 0_u64;
    for index in 0..size.files {
        let bytes = bytes_of(index, size.len_of(index));
        let name = format!("file-{index:06}.bin");
        std::fs::write(from.join(&name), &bytes).unwrap();
        let hash = store
            .write_content(&bytes)
            .await
            .expect("the laptop keeps the file")
            .expect("a file with bytes has content");
        let manifest = store.manifest(hash).await.expect("its manifest is kept");
        blobs.push(hash.to_hex().to_string());
        blobs.extend(
            manifest
                .chunks()
                .iter()
                .map(|chunk| hex::encode(chunk.hash)),
        );
        total += bytes.len() as u64;
        files.push((name.clone(), hash, bytes.len() as u64));
        names.push(name);
    }
    blobs.sort_unstable();
    blobs.dedup();
    let pieces = blobs.len();

    let copied = plain_copy(&from, &to, &names);
    drop(plain);

    // Subscribed before the move: the write made while the files move is the first the server
    // takes after it, since moving content writes no record.
    let mut writes = hosted.document_writes();
    // The move: the tree names the files, and the server takes in all they are made of.
    let started = Instant::now();
    tree.change(None, |namespace| {
        let mut folder = tilo_fs::namespace::ROOT_INO;
        for (index, (name, hash, len)) in files.iter().enumerate() {
            if index % 1_000 == 0 {
                folder = namespace
                    .create_dir(
                        tilo_fs::namespace::ROOT_INO,
                        format!("folder-{}", index / 1_000),
                        1,
                    )
                    .ok_or("a folder could not be made")?;
            }
            namespace
                .create_file(folder, name, Some(hash), *len, 1)
                .ok_or("a file could not be made")?;
        }
        Ok(())
    })
    .await
    .expect("the workspace takes the laptop's tree");
    let named = started.elapsed();

    let kept = server
        .server
        .content()
        .get(&workspace)
        .expect("the server keeps the workspace's content");
    let allowed = copied.saturating_mul(WITHIN_COPY).max(AT_LEAST);
    // `TILO_FILES_SPEED_WAIT` (seconds) waits longer than the move is allowed, to measure how
    // long it takes when it is slower than that.
    let waited = std::env::var("TILO_FILES_SPEED_WAIT")
        .ok()
        .and_then(|seconds| seconds.parse().ok())
        .map_or(allowed, Duration::from_secs)
        .max(allowed);
    let mut lacking = blobs;
    let mut told = Instant::now();
    let mut live = None;
    while !lacking.is_empty() && started.elapsed() < waited {
        lacking.retain(|blob| !kept.has(blob));
        // Once a quarter of the pieces are kept, with the rest on their way, the laptop writes a
        // record, and the server takes it in as fast as when nothing else moves.
        if live.is_none() && lacking.len() * 4 <= pieces * 3 && !lacking.is_empty() {
            while writes.try_recv().is_ok() {}
            let edited = Instant::now();
            tree.change(None, |namespace| {
                namespace
                    .create_dir(tilo_fs::namespace::ROOT_INO, "written-while-files-move", 1)
                    .map(|_| ())
                    .ok_or_else(|| "the folder could not be made".to_owned())
            })
            .await
            .expect("the workspace takes the laptop's write");
            tilo_test_env::within("the server takes in the record written meanwhile", async {
                loop {
                    match writes.recv().await {
                        Ok(write) if write.workspace == workspace => return,
                        Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                            panic!("the server stopped telling its writes")
                        }
                        _ => {}
                    }
                }
            })
            .await;
            live = Some(edited.elapsed());
        }
        if told.elapsed() >= Duration::from_secs(1) {
            told = Instant::now();
            eprintln!(
                "{:>6.1?}: {} of {pieces} pieces kept",
                started.elapsed(),
                pieces - lacking.len()
            );
        }
        // Each look costs a lookup per piece still lacking: often enough to time the move
        // to a quarter second, rarely enough not to slow it.
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
    lacking.retain(|blob| !kept.has(blob));
    let took = started.elapsed();
    let arrived = pieces - lacking.len();
    let rate = arrived as f64 / took.as_secs_f64();
    let report = format!(
        "{} files, {pieces} pieces, {} MB: the plain copy took {copied:.2?}; the move named \
         them in {named:.2?} and the server kept {arrived} of {pieces} pieces in {took:.2?} \
         ({rate:.0} pieces/s, {:.1} MB/s, {:.1}x the copy); a record written meanwhile took \
         {live:.2?}",
        size.files,
        total / 1_000_000,
        (total as f64 * arrived as f64 / pieces as f64) / 1_000_000.0 / took.as_secs_f64(),
        took.as_secs_f64() / copied.as_secs_f64(),
    );
    eprintln!("{report}");
    assert!(
        lacking.is_empty() && took <= allowed,
        "the move took more than {WITHIN_COPY}x the plain copy ({allowed:.2?}): {report}"
    );
    let live = live.expect("a record was written while the files moved");
    assert!(
        live <= LIVE_WITHIN,
        "a record written while the files moved took longer than a frame: {report}"
    );

    drop(tree);
    drop(routed);
    drop(copies);
    service.shutdown().await;
    drop(keys);
}
