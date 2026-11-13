//! Opening the next file of a folder through the mount's open path, with
//! the content on a real peer: what predictive hydration has made local by
//! then, and how long the next file's first byte takes.

#![allow(
    clippy::arithmetic_side_effects,
    clippy::cast_precision_loss,
    reason = "test fixtures, a simulated link's timing and a timing report"
)]

use super::*;
use crate::namespace::ROOT_INO;
use crate::store::ContentStore;
use iroh::endpoint::{Endpoint, presets};
use iroh::protocol::Router;
use iroh_blobs::provider::events::{
    EventMask, EventSender, ProviderMessage, RequestMode, ThrottleMode,
};
use std::time::Instant;

use crate::smart::history::TOGETHER_WINDOW;
use crate::smart::kept_history::KeptHistory;

const PAGE_BYTES: usize = 2 * 1024 * 1024;
const FIRST_READ: u64 = 64 * 1024;
const BENCH_ENV: &str = "TILO_FS_PREFETCH_BENCH";
const DWELL: Duration = Duration::from_secs(1);
const BENCH_LINK: Link = Link {
    round_trip: Duration::from_millis(40),
    bytes_per_second: 50_000_000 / 8,
};

fn page_bytes(index: usize) -> Vec<u8> {
    let seed = u64::try_from(index).unwrap() * 2 + 1;
    crate::test_bytes::pseudo(PAGE_BYTES, seed)
}

const WORKSPACE: uuid::Uuid = uuid::Uuid::from_u128(0x7110_0951);
const NEVER: Duration = Duration::from_secs(3600);

fn files_paths(files: &[(&str, String)]) -> Vec<String> {
    files
        .iter()
        .map(|(dir, name)| format!("{dir}/{name}"))
        .collect()
}

fn page_names(count: usize) -> Vec<(&'static str, String)> {
    (1..=count)
        .map(|page| ("scans", format!("page-{page}.pdf")))
        .collect()
}

#[derive(Clone, Copy)]
struct Link {
    round_trip: Duration,
    bytes_per_second: u64,
}

impl Link {
    fn events(self) -> EventSender {
        let mask = EventMask {
            get: RequestMode::Intercept,
            get_many: RequestMode::Intercept,
            throttle: ThrottleMode::Intercept,
            ..EventMask::DEFAULT
        };
        let (sender, mut messages) = EventSender::channel(64, mask);
        let free_at = Arc::new(Mutex::new(tokio::time::Instant::now()));
        tokio::spawn(async move {
            while let Some(message) = messages.recv().await {
                let round_trip = self.round_trip;
                match message {
                    ProviderMessage::GetRequestReceived(request) => {
                        tokio::spawn(async move {
                            tokio::time::sleep(round_trip).await;
                            request.tx.send(Ok(())).await.ok();
                        });
                    }
                    ProviderMessage::GetManyRequestReceived(request) => {
                        tokio::spawn(async move {
                            tokio::time::sleep(round_trip).await;
                            request.tx.send(Ok(())).await.ok();
                        });
                    }
                    ProviderMessage::Throttle(chunk) => {
                        let on_the_wire = Duration::from_secs_f64(
                            chunk.inner.size as f64 / self.bytes_per_second as f64,
                        );
                        let sent_at = {
                            let mut free_at = free_at.lock().unwrap();
                            let sent_at = (*free_at).max(tokio::time::Instant::now()) + on_the_wire;
                            *free_at = sent_at;
                            sent_at
                        };
                        tokio::spawn(async move {
                            tokio::time::sleep_until(sent_at).await;
                            chunk.tx.send(Ok(())).await.ok();
                        });
                    }
                    _ => {}
                }
            }
        });
        sender
    }
}

struct RemoteFolder {
    fs: QuickFS,
    files: Vec<(u64, Hash)>,
    paths: Vec<String>,
    source_store: ContentStore,
    router: Router,
    source: Endpoint,
    client: Endpoint,
}

impl RemoteFolder {
    async fn with_files(files: &[(&str, String)], link: Option<Link>) -> Self {
        let files_given = files;
        let source = Endpoint::bind(presets::Minimal).await.unwrap();
        let source_store = ContentStore::memory();
        let mut hashes = Vec::with_capacity(files.len());
        for index in 0..files.len() {
            hashes.push(
                source_store
                    .write_content(&page_bytes(index))
                    .await
                    .unwrap()
                    .unwrap(),
            );
        }
        let router = Router::builder(source.clone())
            .accept(
                iroh_blobs::ALPN,
                source_store.blobs_protocol(link.map(Link::events)),
            )
            .spawn();
        let client = Endpoint::bind(presets::Minimal).await.unwrap();
        let hydrator = Arc::new(Hydrator::new(
            client.clone(),
            Arc::new(ContentStore::memory()),
        ));
        hydrator.add_peer(source.addr());
        let namespace = Arc::new(Mutex::new(Namespace::new("prediction").unwrap()));
        let files = {
            let mut ns = namespace.lock().unwrap();
            let mut dirs = HashMap::new();
            files
                .iter()
                .zip(&hashes)
                .map(|((dir, name), hash)| {
                    let dir = *dirs
                        .entry(*dir)
                        .or_insert_with(|| ns.create_dir(ROOT_INO, dir, 1).unwrap());
                    let ino = ns
                        .create_file(dir, name, Some(hash), PAGE_BYTES as u64, 1)
                        .unwrap();
                    (ino, *hash)
                })
                .collect()
        };
        let heuristics = Arc::new(Heuristics::new(Arc::clone(&hydrator)));
        let fs = QuickFS::new(namespace, hydrator, heuristics);
        Self {
            fs,
            files,
            paths: files_paths(files_given),
            source_store,
            router,
            source,
            client,
        }
    }

    fn keep_history(self, cache_root: &std::path::Path, flush_delay: Duration) -> Self {
        Self {
            fs: self
                .fs
                .with_kept_prefetch_history_after(cache_root, WORKSPACE, flush_delay),
            ..self
        }
    }

    fn restart(self, kept: Option<(&std::path::Path, Duration)>) -> Self {
        let hydrator = Arc::clone(&self.fs.hydrator);
        let saved = self.fs.ns().save();
        let Self {
            fs,
            paths,
            source_store,
            router,
            source,
            client,
            ..
        } = self;
        drop(fs);
        let mut namespace = Namespace::from_bytes("prediction", &saved).unwrap();
        let mut files: Vec<(u64, Hash)> = paths
            .iter()
            .rev()
            .map(|path| {
                let ino = namespace.resolve_path_ino(path).unwrap();
                (ino, namespace.hash_for(ino).unwrap())
            })
            .collect();
        files.reverse();
        let heuristics = Arc::new(Heuristics::new(Arc::clone(&hydrator)));
        let fs = QuickFS::new(Arc::new(Mutex::new(namespace)), hydrator, heuristics);
        let fs = match kept {
            Some((root, delay)) => fs.with_kept_prefetch_history_after(root, WORKSPACE, delay),
            None => fs,
        };
        Self {
            fs,
            files,
            paths,
            source_store,
            router,
            source,
            client,
        }
    }

    fn kept(&self) -> &KeptHistory {
        self.fs.kept_history.as_deref().unwrap()
    }

    fn with_acl(self, acl: crate::acl::WorkspaceAcl, identity: crate::acl::IdentityKey) -> Self {
        Self {
            fs: self.fs.with_acl(acl, identity),
            ..self
        }
    }

    fn open_of(&self, index: usize) -> (u64, u64) {
        let ino = self.files[index].0;
        let dir = self.fs.ns().parent_ino(ino).unwrap();
        (dir, ino)
    }

    async fn open_and_time_first_byte(&self, index: usize) -> Duration {
        let (ino, hash) = self.files[index];
        self.fs.predict_after_open(ino);
        let started = Instant::now();
        let bytes = self.fs.hydrator.read(hash, 0, FIRST_READ).await.unwrap();
        let elapsed = started.elapsed();
        assert_eq!(
            bytes,
            page_bytes(index)[..usize::try_from(FIRST_READ).unwrap()]
        );
        elapsed
    }

    async fn settle(&self) {
        tilo_test_env::within("predictive hydration settles", async {
            while self.fs.heuristics.pending_work() != (0, 0) {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await;
    }

    async fn is_local(&self, index: usize) -> bool {
        self.fs.hydrator.sync_state(self.files[index].1).await == fs_protocol::SyncState::Local
    }

    async fn close(self) {
        drop(self.fs);
        self.router.shutdown().await.unwrap();
        drop(self.source_store);
        self.source.close().await;
        self.client.close().await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn paging_through_a_folder_finds_each_next_page_local() {
    let folder = RemoteFolder::with_files(&page_names(12), None).await;
    for index in 0..folder.files.len() - 1 {
        folder.open_and_time_first_byte(index).await;
        folder.settle().await;
        assert!(
            folder.is_local(index + 1).await,
            "page-{} is local once pages 1..={} were opened in order",
            index + 2,
            index + 1
        );
    }
    folder.close().await;
}

fn seed_opened_together(fs: &QuickFS, opens: &[(u64, u64)]) {
    let now = Instant::now();
    let mut history = fs.access_history.lock().unwrap();
    for occasion in [3_u32, 2] {
        let at = now
            .checked_sub(TOGETHER_WINDOW * 2 * occasion)
            .expect("the monotonic clock is past the seeded occasions");
        for (offset, (dir, file)) in opens.iter().enumerate() {
            history.record(*dir, *file, at + Duration::from_secs(offset as u64));
        }
    }
}

fn report_and_chart() -> Vec<(&'static str, String)> {
    vec![
        ("scans", "report.pdf".to_owned()),
        ("figures", "chart.png".to_owned()),
    ]
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_file_opened_together_before_is_hydrated_with_it_only_if_readable() {
    {
        let folder = RemoteFolder::with_files(&report_and_chart(), None).await;
        let opens = [folder.open_of(0), folder.open_of(1)];
        seed_opened_together(&folder.fs, &opens);
        assert!(!folder.is_local(1).await);
        folder.open_and_time_first_byte(0).await;
        folder.settle().await;
        assert!(
            folder.is_local(1).await,
            "the chart opened with the report twice before arrives with it"
        );
        folder.close().await;
    }
    {
        const ME: [u8; 32] = [0xB7; 32];
        let folder = RemoteFolder::with_files(&report_and_chart(), None).await;
        let opens = [folder.open_of(0), folder.open_of(1)];
        let mut acl = crate::acl::WorkspaceAcl::new();
        acl.grant_read(ME, "/scans/report.pdf");
        let folder = folder.with_acl(acl, ME);
        seed_opened_together(&folder.fs, &opens);
        assert!(
            folder.fs.predictive_candidates(opens[0].1).is_empty(),
            "a partner outside the view is never a candidate"
        );
        folder.open_and_time_first_byte(0).await;
        folder.settle().await;
        assert!(
            !folder.is_local(1).await,
            "the history cannot carry content across the view's boundary"
        );
        folder.close().await;
    }
}

fn percentile(sorted: &[Duration], percent: usize) -> Duration {
    let rank = (sorted.len() * percent).div_ceil(100).max(1);
    sorted[rank - 1]
}

async fn timed_paging(scenario: &str, order: &[usize], predict: bool) {
    let folder = RemoteFolder::with_files(&page_names(order.len()), Some(BENCH_LINK)).await;
    if !predict {
        folder
            .fs
            .heuristics
            .set_policy(crate::smart::PredictivePolicy {
                enabled: false,
                ..crate::smart::PredictivePolicy::default()
            });
    }
    let mut next = Vec::new();
    let mut local = 0_usize;
    for (step, index) in order.iter().copied().enumerate() {
        let was_local = folder.is_local(index).await;
        let ttfb = folder.open_and_time_first_byte(index).await;
        if step > 0 {
            next.push(ttfb);
            local += usize::from(was_local);
        }
        tokio::time::sleep(DWELL).await;
    }
    next.sort();
    let (ranged, loose) = folder.fs.hydrator.received_bytes();
    eprintln!(
        "PERF {scenario} prefetch={} next-page ttfb p50 {:.2} ms p90 {:.2} ms max {:.2} ms; \
         local before open {local}/{}; received {:.1} MiB",
        if predict { "on" } else { "off" },
        percentile(&next, 50).as_secs_f64() * 1e3,
        percentile(&next, 90).as_secs_f64() * 1e3,
        next.last().unwrap().as_secs_f64() * 1e3,
        next.len(),
        (ranged + loose) as f64 / (1024.0 * 1024.0),
    );
    folder.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn time_to_first_byte_of_the_next_page() {
    if std::env::var_os(BENCH_ENV).is_none() {
        return;
    }
    let forwards: Vec<usize> = (0..12).collect();
    let backwards: Vec<usize> = (0..12).rev().collect();
    for (scenario, order) in [("forwards", &forwards), ("backwards", &backwards)] {
        for predict in [false, true] {
            timed_paging(scenario, order, predict).await;
        }
    }
}

fn predicts(folder: &RemoteFolder, from: usize, to: usize) -> bool {
    let (ino, _) = folder.files[from];
    let hash = folder.files[to].1;
    folder
        .fs
        .predictive_candidates(ino)
        .iter()
        .any(|(candidate, _)| *candidate == hash)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn files_opened_together_still_predict_each_other_after_a_restart() {
    {
        let folder = RemoteFolder::with_files(&report_and_chart(), None).await;
        seed_opened_together(&folder.fs, &[folder.open_of(0), folder.open_of(1)]);
        assert!(predicts(&folder, 0, 1), "warm before the restart");
        let folder = folder.restart(None);
        assert!(
            !predicts(&folder, 0, 1),
            "cold after a restart that forgets"
        );
        folder.close().await;
    }
    {
        let root = tempfile::tempdir().unwrap();
        let folder = RemoteFolder::with_files(&report_and_chart(), None)
            .await
            .keep_history(root.path(), NEVER);
        seed_opened_together(&folder.fs, &[folder.open_of(0), folder.open_of(1)]);
        let before: Vec<u64> = folder.files.iter().map(|(ino, _)| *ino).collect();
        let folder = folder.restart(Some((root.path(), NEVER)));
        let after: Vec<u64> = folder.files.iter().map(|(ino, _)| *ino).collect();
        assert_ne!(before, after, "the restart hands out new inodes");
        assert!(
            folder.kept().path().exists(),
            "the old mount kept its history"
        );
        assert!(!folder.is_local(1).await);
        folder.open_and_time_first_byte(0).await;
        folder.settle().await;
        assert!(
            folder.is_local(1).await,
            "the chart opened with the report before the restart arrives with it after"
        );
        assert!(predicts(&folder, 1, 0), "and the report with the chart");
        folder.close().await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn opening_files_never_writes_the_kept_history() {
    const BURST: usize = 20;
    {
        let root = tempfile::tempdir().unwrap();
        let folder = RemoteFolder::with_files(&report_and_chart(), None)
            .await
            .keep_history(root.path(), NEVER);
        for open in 0..BURST {
            folder.fs.predict_after_open(folder.files[open % 2].0);
            assert_eq!(folder.kept().writes(), 0, "open {open} wrote");
            assert!(!folder.kept().path().exists(), "open {open} wrote");
        }
        folder.close().await;
    }
    {
        let root = tempfile::tempdir().unwrap();
        let folder = RemoteFolder::with_files(&report_and_chart(), None)
            .await
            .keep_history(root.path(), NEVER);
        for open in 0..BURST {
            folder.fs.predict_after_open(folder.files[open % 2].0);
        }
        assert_eq!(folder.kept().writes(), 0);
        assert!(folder.kept().flush().unwrap(), "the burst is written");
        assert!(!folder.kept().flush().unwrap(), "and only once");
        assert_eq!(folder.kept().writes(), 1, "one burst, one write");
        assert!(folder.kept().path().exists());
        folder.close().await;
    }
    {
        let root = tempfile::tempdir().unwrap();
        let folder = RemoteFolder::with_files(&report_and_chart(), None)
            .await
            .keep_history(root.path(), Duration::from_millis(1));
        folder.fs.predict_after_open(folder.files[0].0);
        folder.fs.predict_after_open(folder.files[1].0);
        tilo_test_env::within("the keeper writes after the opens", async {
            while folder.kept().writes() == 0 {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await;
        assert!(folder.kept().path().exists());
        folder.close().await;
    }
}

fn kept_pairs(folder: &RemoteFolder) -> usize {
    folder
        .fs
        .access_history
        .lock()
        .unwrap()
        .pairs(Instant::now())
        .len()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn turning_prefetch_off_forgets_the_kept_history() {
    let root = tempfile::tempdir().unwrap();
    let folder = RemoteFolder::with_files(&report_and_chart(), None)
        .await
        .keep_history(root.path(), NEVER);
    seed_opened_together(&folder.fs, &[folder.open_of(0), folder.open_of(1)]);
    assert!(folder.kept().flush().unwrap());
    assert!(folder.kept().path().exists());

    let (policies, kept) = crate::cache_policy::CachePolicies::load(
        None,
        Arc::clone(&folder.fs.heuristics),
        Arc::clone(folder.fs.hydrator.store.files()),
    );
    kept.unwrap();
    policies
        .set(runtime_fs::storage::CachePolicy {
            prefetch: false,
            budget_bytes: None,
        })
        .unwrap();
    assert!(
        !folder.kept().path().exists(),
        "the kept file goes with the switch"
    );
    assert_eq!(kept_pairs(&folder), 0, "and so does what is in memory");

    for _ in 0..3 {
        folder.fs.predict_after_open(folder.files[0].0);
        folder.fs.predict_after_open(folder.files[1].0);
    }
    assert_eq!(
        kept_pairs(&folder),
        0,
        "nothing is learned while prefetching is off"
    );
    assert!(!folder.kept().flush().unwrap());
    assert!(!folder.kept().path().exists());

    let folder = folder.restart(Some((root.path(), NEVER)));
    assert!(!predicts(&folder, 0, 1), "a restart finds nothing");
    folder.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_corrupt_kept_history_is_discarded() {
    let root = tempfile::tempdir().unwrap();
    let mut folder = RemoteFolder::with_files(&report_and_chart(), None)
        .await
        .keep_history(root.path(), NEVER);
    seed_opened_together(&folder.fs, &[folder.open_of(0), folder.open_of(1)]);
    assert!(folder.kept().flush().unwrap());
    let path = folder.kept().path().to_owned();
    let valid = std::fs::read(&path).unwrap();
    let mut altered = valid.clone();
    altered[20] ^= 1;
    for (case, bytes) in [
        ("torn", valid[..valid.len() - 1].to_vec()),
        ("altered", altered),
        ("garbage", b"not a history".to_vec()),
    ] {
        folder = folder.restart(None);
        std::fs::write(&path, &bytes).unwrap();
        folder = folder.restart(Some((root.path(), NEVER)));
        assert!(!path.exists(), "the {case} file is deleted");
        assert_eq!(kept_pairs(&folder), 0, "nothing of the {case} file is used");
    }
    folder.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn first_read_after_restart() {
    const ROUNDS: usize = 7;
    if std::env::var_os(BENCH_ENV).is_none() {
        return;
    }
    for warm in [false, true] {
        let mut reads = Vec::with_capacity(ROUNDS);
        let mut local = 0_usize;
        for _ in 0..ROUNDS {
            let root = tempfile::tempdir().unwrap();
            let folder = RemoteFolder::with_files(&report_and_chart(), Some(BENCH_LINK))
                .await
                .keep_history(root.path(), NEVER);
            seed_opened_together(&folder.fs, &[folder.open_of(0), folder.open_of(1)]);
            let folder = folder.restart(warm.then_some((root.path(), NEVER)));
            folder.open_and_time_first_byte(0).await;
            tokio::time::sleep(DWELL).await;
            local += usize::from(folder.is_local(1).await);
            reads.push(folder.open_and_time_first_byte(1).await);
            folder.close().await;
        }
        reads.sort();
        eprintln!(
            "PERF after-restart history={} partner first read p50 {:.2} ms max {:.2} ms; \
             local before open {local}/{ROUNDS}",
            if warm { "kept" } else { "forgotten" },
            percentile(&reads, 50).as_secs_f64() * 1e3,
            reads.last().unwrap().as_secs_f64() * 1e3,
        );
    }
}
