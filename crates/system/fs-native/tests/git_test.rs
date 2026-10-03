//! Git works natively on a captured tree, and every capture is a state git
//! can recover from. Nothing in the data plane knows about git; this test
//! checks the general guarantees with git as the witness.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use fs_native::capture::{CaptureConfig, ChangeOp, EpochCapture, EpochScheduler};
use fs_native::journal::{ChangeJournal, JournalConfig};

use crate::support::fanotify_scratch;

fn git(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["-c", "gc.auto=0", "-c", "core.fsync=all"])
        .args(args)
        .env("GIT_AUTHOR_NAME", "Test")
        .env("GIT_AUTHOR_EMAIL", "test@example.invalid")
        .env("GIT_COMMITTER_NAME", "Test")
        .env("GIT_COMMITTER_EMAIL", "test@example.invalid")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("HOME", dir)
        .output()
        .expect("git runs");
    assert!(
        out.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn git_ok(dir: &Path, args: &[&str]) -> Result<String, String> {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("HOME", dir)
        .output()
        .map_err(|e| e.to_string())?;
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    } else {
        Err(String::from_utf8_lossy(&out.stderr).into_owned())
    }
}

/// Where git keeps a loose object.
fn object_path(hex: &str) -> PathBuf {
    PathBuf::from(format!("repo/.git/objects/{}/{}", &hex[..2], &hex[2..]))
}

struct Observed {
    epochs: usize,
    snapshot_failures: Vec<String>,
    order_violations: Vec<String>,
    refs_checked: usize,
}

#[test]
fn git_runs_natively_while_capture_runs_and_every_epoch_is_a_valid_repository() {
    let Some(s) = fanotify_scratch("git") else {
        return;
    };
    assert!(
        Command::new("git").arg("--version").output().is_ok(),
        "git is not installed"
    );
    let mut journal = ChangeJournal::open(&s.store, JournalConfig::new(&s.live)).unwrap();
    let mut capture = EpochCapture::new(CaptureConfig::new(&s.live, &s.epochs)).unwrap();
    capture.capture(&journal.take()).unwrap();

    let stop = Arc::new(AtomicBool::new(false));
    let observed = Arc::new(Mutex::new(Observed {
        epochs: 0,
        snapshot_failures: Vec::new(),
        order_violations: Vec::new(),
        refs_checked: 0,
    }));
    let capturer = spawn_capturer(journal, capture, Arc::clone(&stop), Arc::clone(&observed));

    let repo = s.live.join("repo");
    std::fs::create_dir(&repo).unwrap();
    git(&repo, &["init", "-q", "-b", "main"]);
    let t = Instant::now();
    for i in 0..40 {
        let dir = repo.join(format!("src/m{}", i % 7));
        std::fs::create_dir_all(&dir).unwrap();
        for f in 0..5 {
            std::fs::write(
                dir.join(format!("f{f}.rs")),
                format!("// {i} {f}\n").repeat(50),
            )
            .unwrap();
        }
        git(&repo, &["add", "-A"]);
        git(&repo, &["commit", "-q", "-m", &format!("commit {i}")]);
        if i % 10 == 0 {
            assert_eq!(git(&repo, &["status", "--porcelain"]), "");
        }
    }
    let elapsed = t.elapsed();
    assert_eq!(git(&repo, &["status", "--porcelain"]), "");
    assert_eq!(git(&repo, &["rev-list", "--count", "HEAD"]).trim(), "40");
    git(&repo, &["fsck", "--strict"]);
    std::thread::sleep(Duration::from_millis(300));
    stop.store(true, Ordering::Relaxed);
    capturer.join().unwrap();

    let obs = observed.lock().unwrap();
    eprintln!(
        "40 commits in {elapsed:?} under capture; {} epochs, {} ref updates checked",
        obs.epochs, obs.refs_checked
    );
    assert!(obs.epochs >= 3, "capture did not run concurrently");
    assert!(obs.refs_checked >= 1);
    assert!(
        obs.snapshot_failures.is_empty(),
        "{:#?}",
        obs.snapshot_failures
    );
    assert!(
        obs.order_violations.is_empty(),
        "{:#?}",
        obs.order_violations
    );
}

/// Pumps the journal and cuts epochs on the scheduler's signal until
/// `stop`, checking every epoch as it is published.
fn spawn_capturer(
    mut journal: ChangeJournal,
    mut capture: EpochCapture,
    stop: Arc<AtomicBool>,
    observed: Arc<Mutex<Observed>>,
) -> std::thread::JoinHandle<()> {
    std::thread::spawn(move || {
        let mut scheduler = EpochScheduler::new(
            Duration::from_millis(50),
            Duration::from_millis(200),
            Duration::from_millis(100),
            Duration::from_millis(500),
        );
        // Objects published so far, in application order.
        let mut published: HashSet<PathBuf> = HashSet::new();
        // Journal entries already shown to the scheduler.
        let mut seen = 0usize;
        let mut last = false;
        loop {
            let finishing = stop.load(Ordering::Relaxed);
            journal.pump(Some(Duration::from_millis(20))).unwrap();
            let now = Instant::now();
            for e in journal.pending().iter().skip(seen) {
                scheduler.observe(now, &e.op, e.path.as_deref(), e.is_dir);
            }
            seen = journal.pending().len();
            if !(scheduler.due(Instant::now()) || finishing) {
                continue;
            }
            // git keeps committing while the epoch is cut.
            let cs = capture.capture_from(&mut journal).unwrap();
            seen = journal.pending().len();
            scheduler.cut();
            let snap = capture.last_snapshot().unwrap().to_path_buf();
            let mut obs = observed.lock().unwrap();
            obs.epochs += 1;
            check_epoch(&snap, &cs.ops, &mut published, &mut obs);
            drop(obs);
            if finishing {
                if last {
                    break;
                }
                last = true;
            }
        }
        capture.release().unwrap();
    })
}

/// V2: the snapshot is a repository git accepts. V3: a branch ref is
/// published only after the commit object it names.
fn check_epoch(
    snap: &Path,
    ops: &[ChangeOp],
    published: &mut HashSet<PathBuf>,
    obs: &mut Observed,
) {
    let repo = snap.join("repo");
    if repo.join(".git/HEAD").exists() {
        let head = git_ok(&repo, &["rev-parse", "-q", "--verify", "HEAD"]);
        if let Ok(commit) = head
            && let Err(e) = git_ok(&repo, &["fsck", "--connectivity-only", "--no-dangling"])
        {
            obs.snapshot_failures.push(format!(
                "{}: fsck at {}: {e}",
                snap.display(),
                commit.trim()
            ));
        }
    }
    for op in ops {
        let ChangeOp::Upsert { path, node } = op else {
            continue;
        };
        if path.starts_with("repo/.git/objects") {
            published.insert(path.clone());
            continue;
        }
        if path != Path::new("repo/.git/refs/heads/main") {
            continue;
        }
        if node.manifest.is_none() {
            continue;
        }
        let Ok(bytes) = std::fs::read(snap.join(path)) else {
            continue;
        };
        let hex = String::from_utf8_lossy(&bytes).trim().to_owned();
        if hex.len() != 40 {
            continue;
        }
        obs.refs_checked += 1;
        if !published.contains(&object_path(&hex)) {
            obs.order_violations
                .push(format!("main -> {hex} published before its commit object"));
        }
    }
}
