use super::*;
use crate::store::ContentStore;
use crate::test_bytes::pseudo;
use iroh::endpoint::{Endpoint, presets};
use iroh::protocol::Router;

/// A peer serving committed content, and a hydrator on another endpoint
/// that has none of it.
struct Remote {
    router: Router,
    source: Endpoint,
    client: Endpoint,
    store: ContentStore,
    hydrator: Arc<Hydrator>,
}

impl Remote {
    async fn new() -> Self {
        let source = Endpoint::bind(presets::Minimal).await.unwrap();
        let store = ContentStore::memory();
        let router = Router::builder(source.clone())
            .accept(iroh_blobs::ALPN, store.blobs_protocol(None))
            .spawn();
        let client = Endpoint::bind(presets::Minimal).await.unwrap();
        let hydrator = Arc::new(Hydrator::new(
            client.clone(),
            Arc::new(ContentStore::memory()),
        ));
        hydrator.add_peer(source.addr());
        Self {
            router,
            source,
            client,
            store,
            hydrator,
        }
    }

    async fn content(&self, bytes: &[u8]) -> Hash {
        self.store.write_content(bytes).await.unwrap().unwrap()
    }

    async fn close(self) {
        drop(self.hydrator);
        self.router.shutdown().await.unwrap();
        self.source.close().await;
        self.client.close().await;
    }
}

const JOB_BYTES: usize = 2 * 1024 * 1024;

fn new_key() -> JobKey {
    JobKey {
        ino: 7,
        id: uuid::Uuid::new_v4(),
    }
}

/// Wait, bounded, until `key`'s job leaves `Hydrating`.
async fn settled(jobs: &HistoryJobs, key: JobKey) -> Option<State> {
    let started = std::time::Instant::now();
    loop {
        let state = jobs.state(key);
        if !matches!(state, Some(State::Hydrating { .. })) {
            return state;
        }
        assert!(
            started.elapsed() < tilo_test_env::hang_bound(),
            "history job never settled: {state:?}"
        );
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn progress_reported() {
    const FIRST: usize = 6 * 1024 * 1024;
    const SECOND: usize = 2 * 1024 * 1024;
    let remote = Remote::new().await;
    let first = remote.content(&pseudo(FIRST, 0x248)).await;
    let second = remote.content(&pseudo(SECOND, 0x249)).await;
    let total = (FIRST as u64).saturating_add(SECOND as u64);
    let mut seen = Vec::new();
    assert!(hydrate(&remote.hydrator, &[first, second], |state| seen.push(state)).await);
    let progress: Vec<(u64, u64)> = seen
        .iter()
        .map(|state| match *state {
            State::Hydrating { done, total } => (done, total),
            other => panic!("progress reports only hydration, got {other:?}"),
        })
        .collect();
    assert!(
        progress.iter().all(|(_, of)| *of == total),
        "every report names the total of all versions: {progress:?}"
    );
    assert!(
        progress.windows(2).all(|pair| pair[0].0 <= pair[1].0),
        "progress never goes back: {progress:?}"
    );
    assert!(
        progress.iter().any(|(done, of)| done < of),
        "a report before the end: {progress:?}"
    );
    assert_eq!(progress.last(), Some(&(total, total)));
    assert_eq!(remote.hydrator.local_len(first).await, Some(FIRST as u64));
    assert_eq!(remote.hydrator.local_len(second).await, Some(SECOND as u64));
    remote.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn remote_content_hydrates_in_a_job_and_the_next_request_commits() {
    let remote = Remote::new().await;
    let bytes = pseudo(JOB_BYTES, 0x24a);
    let hash = remote.content(&bytes).await;
    let jobs = HistoryJobs::default();
    let gate = jobs.hold();
    let key = new_key();

    // The request returns while the job has not fetched a byte.
    assert_eq!(
        jobs.ensure_local(key, &remote.hydrator, &[hash]),
        Readiness::Hydrating
    );
    assert_eq!(
        jobs.state(key),
        Some(State::Hydrating { done: 0, total: 0 })
    );
    assert_eq!(remote.hydrator.local_len(hash).await, None);
    // Repeating it does not start a second download.
    assert_eq!(
        jobs.ensure_local(key, &remote.hydrator, &[hash, hash]),
        Readiness::Hydrating
    );
    // Another request under the same id for other content waits its turn.
    let other = remote.content(b"other").await;
    assert_eq!(
        jobs.ensure_local(key, &remote.hydrator, &[other]),
        Readiness::Mismatch
    );

    gate.add_permits(1);
    assert_eq!(settled(&jobs, key).await, Some(State::Ready));
    assert_eq!(
        remote.hydrator.local_len(hash).await,
        Some(bytes.len() as u64)
    );
    // The ready job is consumed by the request that commits.
    assert_eq!(
        jobs.ensure_local(key, &remote.hydrator, &[hash]),
        Readiness::Local
    );
    assert_eq!(jobs.state(key), None);
    // Local content never starts a job.
    let local = new_key();
    assert_eq!(
        jobs.ensure_local(local, &remote.hydrator, &[hash]),
        Readiness::Local
    );
    assert_eq!(jobs.state(local), None);
    remote.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cancel_stops_hydration() {
    let remote = Remote::new().await;
    let hash = remote.content(&pseudo(JOB_BYTES, 0x24b)).await;
    let jobs = HistoryJobs::default();
    let gate = jobs.hold();
    let key = new_key();
    assert_eq!(
        jobs.ensure_local(key, &remote.hydrator, &[hash]),
        Readiness::Hydrating
    );
    assert!(jobs.cancel(key));
    assert!(
        !jobs.cancel(key),
        "a cancelled job cannot be cancelled again"
    );
    assert_eq!(jobs.state(key), Some(State::Cancelled));
    // Releasing the hold must not resurrect the aborted job: once its task
    // has ended, nobody is left to take the released permit.
    tilo_test_env::within("the cancelled job's task to end", async {
        while !jobs.lock().get(&key).unwrap().task.is_finished() {
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
    })
    .await;
    gate.add_permits(1);
    assert_eq!(
        gate.available_permits(),
        1,
        "the aborted job took the permit"
    );
    assert_eq!(jobs.state(key), Some(State::Cancelled));
    assert_eq!(remote.hydrator.local_len(hash).await, None);
    // The cancellation is reported once; the next request starts afresh.
    assert_eq!(
        jobs.ensure_local(key, &remote.hydrator, &[hash]),
        Readiness::Cancelled
    );
    assert_eq!(jobs.state(key), None);
    assert_eq!(
        jobs.ensure_local(key, &remote.hydrator, &[hash]),
        Readiness::Hydrating
    );
    assert_eq!(settled(&jobs, key).await, Some(State::Ready));
    remote.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn unreachable_content_fails_once_and_jobs_are_bounded() {
    let remote = Remote::new().await;
    let jobs = HistoryJobs::default();
    // Content no peer has: the job fails, and says so once.
    let missing = Hash::new(b"no peer has this manifest");
    let key = new_key();
    assert_eq!(
        jobs.ensure_local(key, &remote.hydrator, &[missing]),
        Readiness::Hydrating
    );
    assert_eq!(settled(&jobs, key).await, Some(State::Failed));
    assert_eq!(
        jobs.ensure_local(key, &remote.hydrator, &[missing]),
        Readiness::Failed
    );
    assert_eq!(jobs.state(key), None);

    // Downloads past the bound are refused rather than queued; finished
    // jobs nobody came back for make room.
    let _gate = jobs.hold();
    let hash = remote.content(b"bounded").await;
    let first = new_key();
    assert_eq!(
        jobs.ensure_local(first, &remote.hydrator, &[hash]),
        Readiness::Hydrating
    );
    for _ in 1..MAX_HISTORY_JOBS {
        assert_eq!(
            jobs.ensure_local(new_key(), &remote.hydrator, &[hash]),
            Readiness::Hydrating
        );
    }
    assert_eq!(
        jobs.ensure_local(new_key(), &remote.hydrator, &[hash]),
        Readiness::Busy
    );
    assert!(jobs.cancel(first));
    assert_eq!(
        jobs.ensure_local(new_key(), &remote.hydrator, &[hash]),
        Readiness::Hydrating
    );
    remote.close().await;
}
