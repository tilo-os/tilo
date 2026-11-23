//! A hosted VM's shared folder as a program in the guest sees it: the guest
//! agent's FUSE mount ([`linux_vm_agent::remote_fs`]) talking to the host's
//! share server ([`linux_vm::hosted::files`]) as it does over vsock, here over
//! a socket pair.
//!
//! A walk over a tree nothing changed (what `git status` does to a checkout)
//! is answered by the guest's kernel without asking the host, a file keeps
//! its inode across mounts so git's index stays valid, and what another
//! member changes on the host still shows in the guest.
#![cfg(target_os = "linux")]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "a failed unwrap or index in a test is its assertion"
)]
#![allow(
    clippy::arithmetic_side_effects,
    reason = "counts and deadlines a test keeps, far from any bound"
)]

use std::collections::BTreeMap;
use std::io::{self, Read, Write};
use std::os::unix::fs::MetadataExt as _;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use linux_vm::hosted::files::{Served, serve};
use linux_vm_agent::remote_fs::{Remote, mount_at};
use linux_vm_protocol::files::FsRequest;
use linux_vm_protocol::recv;
use tilo_test_env::SpawnUnderEnvLock as _;

/// The guest's end of the share's connection, counting the requests it
/// sends by kind.
struct Counted {
    stream: UnixStream,
    asks: Asks,
}

type Asks = Arc<Mutex<BTreeMap<String, usize>>>;

impl Read for Counted {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        self.stream.read(buf)
    }
}

impl Write for Counted {
    /// A request is written whole, in one frame.
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        if let Ok(Some(request)) = recv::<_, FsRequest>(&mut &*buf) {
            let kind = format!("{request:?}");
            let kind = kind.split([' ', '(', '{']).next().unwrap_or_default();
            *self
                .asks
                .lock()
                .unwrap()
                .entry(kind.to_owned())
                .or_default() += 1;
        }
        self.stream.write(buf)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.stream.flush()
    }
}

/// A mounted share, unmounted when dropped.
struct Mounted {
    _session: Box<dyn std::any::Any>,
    asks: Asks,
}

impl Mounted {
    /// The requests sent since the last call, by kind, but for the asks
    /// what changed, which come with time rather than with use.
    fn asked(&self) -> BTreeMap<String, usize> {
        let mut asks = std::mem::take(&mut *self.asks.lock().unwrap());
        asks.remove("Changes");
        asks
    }
}

fn total(asks: &BTreeMap<String, usize>) -> usize {
    asks.values().sum()
}

/// `host` shared and mounted at `at` as the guest agent mounts it, or `None`
/// where this machine cannot mount FUSE.
fn mount(host: &Path, at: &Path) -> Option<Mounted> {
    let (host_end, guest_end) = UnixStream::pair().unwrap();
    let shares = vec![Served {
        path: host.to_path_buf(),
        read_only: false,
    }];
    std::thread::spawn(move || serve(host_end, &shares));
    let asks = Asks::default();
    let remote = Remote::open(
        Counted {
            stream: guest_end,
            asks: Arc::clone(&asks),
        },
        &host.display().to_string(),
    )
    .unwrap();
    std::fs::create_dir_all(at).unwrap();
    let uid = rustix::process::getuid().as_raw();
    let gid = rustix::process::getgid().as_raw();
    match mount_at(remote, at, false, uid, gid) {
        Ok(session) => Some(Mounted {
            _session: Box::new(session),
            asks,
        }),
        Err(error) => {
            tilo_test_env::capability_unavailable(
                tilo_test_env::Capability::Fuse,
                format_args!("the share was not mounted at {}: {error}", at.display()),
            );
            None
        }
    }
}

/// A checkout's shape: `files` files, ten to a directory four deep.
fn tree(root: &Path, files: usize) -> Vec<PathBuf> {
    let mut made = Vec::with_capacity(files);
    'all: for a in 0..20 {
        for b in 0..10 {
            for c in 0..10 {
                let dir = root.join(format!("crate{a:02}/src{b}/module{c}"));
                std::fs::create_dir_all(&dir).unwrap();
                for f in 0..10 {
                    if made.len() == files {
                        break 'all;
                    }
                    let file = dir.join(format!("file{f}.rs"));
                    std::fs::write(&file, format!("// {a} {b} {c} {f}\n")).unwrap();
                    made.push(file.strip_prefix(root).unwrap().to_path_buf());
                }
            }
        }
    }
    made
}

/// What `git status` does to a checkout: list every directory, and look at
/// every file. Returns how many entries it saw.
fn walk(root: &Path) -> usize {
    let mut seen = 0;
    let mut dirs = vec![root.to_path_buf()];
    while let Some(dir) = dirs.pop() {
        for entry in std::fs::read_dir(&dir).unwrap() {
            let path = entry.unwrap().path();
            let meta = std::fs::symlink_metadata(&path).unwrap();
            seen += 1;
            if meta.is_dir() {
                dirs.push(path);
            }
        }
    }
    seen
}

fn timed<T>(what: &str, run: impl FnOnce() -> T) -> (T, Duration) {
    let started = Instant::now();
    let out = run();
    let took = started.elapsed();
    eprintln!("{what}: {took:?}");
    (out, took)
}

/// Wait until `seen` holds in the guest; what another member does shows
/// within the guest's change interval, give or take a loaded test machine.
fn eventually(what: &str, seen: impl Fn() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while !seen() {
        assert!(Instant::now() < deadline, "the guest never saw {what}");
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn git(dir: &Path, args: &[&str]) -> Option<String> {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .output_locked()
        .ok()?;
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    Some(String::from_utf8(out.stdout).unwrap())
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one checkout walked, changed and walked again, phase by phase"
)]
fn a_walk_over_an_unchanged_checkout_asks_the_host_nothing_and_sees_every_change() {
    const FILES: usize = 20_000;
    let scratch = tempfile::tempdir().unwrap();
    let host = scratch.path().join("host");
    let files = tree(&host, FILES);
    let (on_host, local) = timed("walk on the host's disk", || walk(&host));

    let at = scratch.path().join("guest");
    let Some(guest) = mount(&host, &at) else {
        return;
    };
    let (cold, cold_took) = timed("first walk in the guest", || walk(&at));
    assert_eq!(cold, on_host);
    let cold_asks = guest.asked();
    // Listing a directory makes its access time stale; the next look at the
    // directory asks for it, once.
    let (second, second_took) = timed("second walk in the guest", || walk(&at));
    assert_eq!(second, on_host);
    let second_asks = guest.asked();
    let (warm, warm_took) = timed("third walk in the guest", || walk(&at));
    assert_eq!(warm, on_host);
    let warm_asks = guest.asked();
    eprintln!(
        "{FILES} files: host {local:?}, guest first {cold_took:?} ({cold_asks:?}), \
         second {second_took:?} ({second_asks:?}), third {warm_took:?} ({warm_asks:?})"
    );
    assert!(
        total(&warm_asks) < FILES / 100,
        "an unchanged tree is walked from the kernel's cache: {warm_asks:?}"
    );

    // A file is one inode, the host's, across lookups and mounts: what
    // git's index recorded of it still holds.
    let probe = &files[FILES / 2];
    let inode = std::fs::symlink_metadata(at.join(probe)).unwrap().ino();
    assert_eq!(
        inode,
        std::fs::symlink_metadata(host.join(probe)).unwrap().ino()
    );

    // Another member's changes reach the host's folder: the guest sees them.
    let edited = host.join(&files[0]);
    std::fs::write(&edited, b"a member's longer edit\n").unwrap();
    eventually("an edit", || {
        std::fs::read(at.join(&files[0])).unwrap() == b"a member's longer edit\n"
    });
    // The guest keeps that a name is not there, until it comes.
    assert!(!at.join("crate00/src0/new.rs").exists());
    std::fs::write(host.join("crate00/src0/new.rs"), b"new\n").unwrap();
    eventually("a new file", || {
        at.join("crate00/src0/new.rs").is_file()
            && std::fs::read_dir(at.join("crate00/src0"))
                .unwrap()
                .any(|entry| entry.unwrap().file_name() == "new.rs")
    });
    std::fs::remove_file(host.join(&files[1])).unwrap();
    eventually("a removed file", || !at.join(&files[1]).exists());
    std::fs::rename(host.join("crate01"), host.join("renamed")).unwrap();
    eventually("a renamed directory", || {
        !at.join("crate01").exists() && at.join("renamed/src0/module0/file0.rs").is_file()
    });

    // What the guest writes is the host's at once, and its own view of it too.
    std::fs::write(at.join("crate00/from-guest.rs"), b"guest\n").unwrap();
    assert_eq!(
        std::fs::read(host.join("crate00/from-guest.rs")).unwrap(),
        b"guest\n"
    );
    assert_eq!(
        std::fs::metadata(at.join("crate00/from-guest.rs"))
            .unwrap()
            .len(),
        6
    );

    drop(guest);
    let Some(again) = mount(&host, &at) else {
        return;
    };
    assert_eq!(
        std::fs::symlink_metadata(at.join(probe)).unwrap().ino(),
        inode,
        "a mount after a VM wakes sees the same inode"
    );

    // git itself, where the machine has it, over a checkout the host made:
    // after the first `git status` records what the guest sees, the next
    // reads no file and asks the host for nearly nothing.
    if git(&host, &["--version"]).is_none() {
        return;
    }
    git(&host, &["init", "-q"]);
    git(&host, &["add", "-A"]);
    git(
        &host,
        &[
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@t",
            "commit",
            "-qm",
            "c",
        ],
    );
    again.asked();
    let (_, first) = timed("first git status in the guest", || {
        git(&at, &["status", "--porcelain"])
    });
    let first_asks = again.asked();
    let (_, second) = timed("second git status in the guest", || {
        git(&at, &["status", "--porcelain"])
    });
    let second_asks = again.asked();
    let (status, took) = timed("third git status in the guest", || {
        git(&at, &["status", "--porcelain"]).unwrap()
    });
    let asked = again.asked();
    eprintln!(
        "git status: first {first:?} ({} requests), second {second:?} ({second_asks:?}), \
         third {took:?} ({asked:?})",
        total(&first_asks)
    );
    assert_eq!(status, "");
    assert!(
        total(&asked) < FILES / 100,
        "git status over an unchanged checkout: {asked:?}"
    );
}
