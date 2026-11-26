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
//! Only the `files-at-disk-speed` check runs it, until the content path meets the bound: it runs
//! 10k files by default, and the full size (100k pieces, 3 GB) with `TILO_FILES_SPEED=full`.

use std::io::{Read as _, Write as _};
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

use tilo_server_protocol::WorkspaceId;
use tilo_user_identity::DeviceCapabilities;

use crate::keep::common::{TestUser, own_service, spawn_server};

const WORKSPACE: WorkspaceId = WorkspaceId::from_u128(0x0066_696c_6573_7065_6564);

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
    /// What the run moves: 10k files, or the full size.
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
        if index.checked_rem(self.large_every) == Some(self.large_every.saturating_sub(1)) {
            return self.large;
        }
        1024_usize
            .saturating_add(
                index
                    .wrapping_mul(7919)
                    .checked_rem(59)
                    .unwrap_or(0)
                    .saturating_mul(1024),
            )
            .saturating_add(index.checked_rem(1000).unwrap_or(0))
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

/// Where the host signs it is reached, as the server names its own address.
async fn host_hint(
    hosted: &tilo_workspace_service::workspaces::Workspaces,
    workspace: &str,
    server_key: [u8; 32],
) -> tilo_peer_state::join::TransportHint {
    hosted
        .hosts(workspace)
        .await
        .expect("the server signs where it is reached")
        .into_iter()
        .find(|hint| hint.host.device_public_key == server_key)
        .expect("the server names its own address")
}

/// `TILO_FILES_SPEED_LOG` names what to log while the files move (a tracing filter).
fn init_speed_log() {
    if let Ok(filter) = std::env::var("TILO_FILES_SPEED_LOG") {
        let _ = tracing_subscriber::fmt()
            .with_ansi(false)
            .with_env_filter(filter)
            .with_writer(std::io::stderr)
            .try_init();
    }
}

/// How long the move may take, or longer when `TILO_FILES_SPEED_WAIT` (seconds) says so: a run
/// that waits past the bound measures how long a slower move is.
fn waited_for(allowed: Duration) -> Duration {
    std::env::var("TILO_FILES_SPEED_WAIT")
        .ok()
        .and_then(|seconds| seconds.parse().ok())
        .map_or(allowed, Duration::from_secs)
        .max(allowed)
}

/// What a move measured, for its report.
struct Figures {
    files: usize,
    pieces: usize,
    total: u64,
    copy_took: Duration,
    naming_took: Duration,
    arrived: usize,
    took: Duration,
    rate: f64,
    live: Option<Duration>,
}

fn move_report(figures: &Figures) -> String {
    let Figures {
        files,
        pieces,
        total,
        copy_took,
        naming_took,
        arrived,
        took,
        rate,
        live,
    } = *figures;
    format!(
        "{} files, {pieces} pieces, {} MB: the plain copy took {copy_took:.2?}; the move named \
         them in {naming_took:.2?} and the server kept {arrived} of {pieces} pieces in {took:.2?} \
         ({rate:.0} pieces/s, {:.1} MB/s, {:.1}x the copy); a record written meanwhile took \
         {live:.2?}",
        files,
        total / 1_000_000,
        (f64::from(u32::try_from(total).unwrap_or(u32::MAX))
            * f64::from(u32::try_from(arrived).unwrap_or(u32::MAX))
            / f64::from(u32::try_from(pieces).unwrap_or(u32::MAX)))
            / 1_000_000.0
            / took.as_secs_f64(),
        took.as_secs_f64() / copy_took.as_secs_f64(),
    )
}

/// The move met the bound, and the record written while it moved reached the server within a
/// frame.
fn assert_move(
    lacking: &[String],
    took: Duration,
    allowed: Duration,
    live: Option<Duration>,
    report: &str,
) {
    assert!(
        lacking.is_empty() && took <= allowed,
        "the move took more than {WITHIN_COPY}x the plain copy ({allowed:.2?}): {report}"
    );
    let live = live.expect("a record was written while the files moved");
    assert!(
        live <= LIVE_WITHIN,
        "a record written while the files moved took longer than a frame: {report}"
    );
}

/// Write this run's files twice — into a directory for the plain copy and into the laptop's
/// content store — time the plain copy, and name every file in the laptop's tree. Answers what
/// the store names, their bytes, the copy's time, and when the tree was named and how long that
/// took.
async fn prepare_files(
    store: &tilo_fs::ContentStore,
    size: &Size,
    tree: &tilo_fs::tree_sync::TreeSync,
) -> (Vec<String>, u64, Duration, Instant, Duration) {
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
        blobs.push(hash.to_hex().clone());
        blobs.extend(
            manifest
                .chunks()
                .iter()
                .map(|chunk| hex::encode(chunk.hash)),
        );
        total = total.saturating_add(bytes.len() as u64);
        files.push((name.clone(), hash, bytes.len() as u64));
        names.push(name);
    }
    blobs.sort_unstable();
    blobs.dedup();

    let copy_took = plain_copy(&from, &to, &names);
    drop(plain);

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
    let naming_took = started.elapsed();
    (blobs, total, copy_took, started, naming_took)
}

/// Wait until the server keeps the pieces the laptop's tree named, writing a record once a
/// quarter of them are kept: answers the pieces still lacking and how long that write took.
async fn wait_for_move(
    kept: &tilo_content::ContentStore,
    tree: &tilo_fs::tree_sync::TreeSync,
    writes: &mut tokio::sync::broadcast::Receiver<
        tilo_workspace_service::workspaces::ResourceWrite,
    >,
    workspace: &str,
    started: Instant,
    waited: Duration,
    blobs: Vec<String>,
    pieces: usize,
) -> (Vec<String>, Option<Duration>) {
    let mut lacking = blobs;
    let mut told = Instant::now();
    let mut live = None;
    while !lacking.is_empty() && started.elapsed() < waited {
        lacking.retain(|blob| !kept.has(blob));
        // Once a quarter of the pieces are kept, with the rest on their way, the laptop writes a
        // record, and the server takes it in as fast as when nothing else moves.
        if live.is_none()
            && lacking.len().saturating_mul(4) <= pieces.saturating_mul(3)
            && !lacking.is_empty()
        {
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
                pieces.saturating_sub(lacking.len())
            );
        }
        // Each look costs a lookup per piece still lacking: often enough to time the move
        // to a quarter second, rarely enough not to slow it.
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
    lacking.retain(|blob| !kept.has(blob));
    (lacking, live)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 8)]
#[ignore = "run by the files-at-disk-speed check"]
async fn a_workspaces_files_reach_its_server_within_twice_a_plain_copy() {
    init_speed_log();
    let size = Size::chosen();
    let ada = TestUser::fresh("Ada");
    let server_device = ada.enrol("home-server", DeviceCapabilities::member());
    let laptop = ada.enrol("laptop", DeviceCapabilities::primary());
    let server_key = server_device.device_pubkey();
    let server = spawn_server(&ada, server_device, &[WORKSPACE], &[&laptop]).await;
    let hosted = server
        .server
        .workspaces()
        .expect("the server holds workspaces");
    let workspace = WORKSPACE.to_string();
    let hint = host_hint(hosted, &workspace, server_key).await;

    let state = tempfile::tempdir().unwrap();
    let (service, keys) = own_service(state.path(), &laptop).await;
    let copies = service.workspaces().clone();
    tilo_test_env::within(
        "the laptop holds the workspace as a member copy",
        copies.hold_as_member(WORKSPACE, &hint),
    )
    .await
    .expect("the server admits the laptop's membership");

    // The laptop's files, in its content store, answered for the workspace and named by its tree.
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
    // Subscribed before the move, since moving content writes no record.
    let mut writes = hosted.document_writes();
    let (blobs, total, copy_took, started, naming_took) = prepare_files(&store, &size, &tree).await;
    let pieces = blobs.len();

    let kept = server
        .server
        .content()
        .get(&workspace)
        .expect("the server keeps the workspace's content");
    let allowed = copy_took.saturating_mul(WITHIN_COPY).max(AT_LEAST);
    let waited = waited_for(allowed);
    let (mut lacking, live) = wait_for_move(
        &kept,
        &tree,
        &mut writes,
        &workspace,
        started,
        waited,
        blobs,
        pieces,
    )
    .await;
    lacking.retain(|blob| !kept.has(blob));
    let took = started.elapsed();
    let arrived = pieces - lacking.len();
    let rate = f64::from(u32::try_from(arrived).unwrap_or(u32::MAX)) / took.as_secs_f64();
    let report = move_report(&Figures {
        files: size.files,
        pieces,
        total,
        copy_took,
        naming_took,
        arrived,
        took,
        rate,
        live,
    });
    eprintln!("{report}");
    assert_move(&lacking, took, allowed, live, &report);

    drop(tree);
    drop(routed);
    drop(copies);
    service.shutdown().await;
    drop(keys);
}
