//! A restart with a workspace's chain of a document's parts: the work it does to answer.
//!
//! A workspace's chain of parts is kept as documents of the workspace's own history, each
//! as large as the part it holds. A start reads them before it answers. What it reads for each
//! part, and what its first answer reads, are the same whether the chain is short or as long as
//! a chain grows to: the test counts that work, the system calls and bytes the process reads
//! and writes, so a loaded machine does not change what it measures. Only Linux counts a process's input and output where the
//! test can read it.
#![cfg(target_os = "linux")]

use super::*;
use automerge::ReadDoc as _;

/// The app the chain belongs to.
const APP: &str = "so.tilo.notes";
/// How many parts a short chain has.
const SHORT: usize = 2;
/// How many parts a long chain has.
const LONG: usize = 26;
/// How large each part's document is.
const BYTES: usize = 512 * 1024;

/// Incompressible text, so a part's document is as large as it says.
fn text(bytes: usize) -> String {
    let mut state = 0x9e37_79b9_7f4a_7c15_u64;
    let mut out = String::with_capacity(bytes);
    while out.len() < bytes {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        let _ = std::fmt::Write::write_fmt(&mut out, format_args!("{state:016x}"));
    }
    out.truncate(bytes);
    out
}

/// The input and output this process did so far, as the kernel counts it: system calls that read
/// or write, and the bytes they moved. Each test runs in a process of its own.
#[derive(Clone, Copy, Debug)]
struct Io {
    calls: u64,
    bytes: u64,
}

impl Io {
    fn now() -> Self {
        let counted = std::fs::read_to_string("/proc/self/io").unwrap();
        let field = |name: &str| {
            counted
                .lines()
                .find_map(|line| line.strip_prefix(name)?.trim().parse::<u64>().ok())
                .unwrap()
        };
        Self {
            calls: field("syscr:").saturating_add(field("syscw:")),
            bytes: field("rchar:").saturating_add(field("wchar:")),
        }
    }

    fn since(self, before: Self) -> Self {
        Self {
            calls: self.calls.saturating_sub(before.calls),
            bytes: self.bytes.saturating_sub(before.bytes),
        }
    }
}

/// What a restart of a workspace with a chain of `parts` parts did: to open, and to answer
/// its first read.
struct Restart {
    opened: Io,
    answered: Io,
}

fn restart_with_chain_of(parts: usize) -> Restart {
    use automerge::transaction::Transactable as _;
    let dir = tempfile::tempdir().unwrap();
    let (key, cert) = governor();
    let mut workspace = GovernedWorkspace::create(
        dir.path(),
        "Team",
        &cert,
        &key,
        None,
        (&[], &[]),
        SystemTime::now(),
    )
    .unwrap();
    let state = workspace.collaboration().clone();
    let chain = collab_types::document::DocumentId::parse(
        "derived:pages@derived:default@tab:0000000000000000",
    )
    .unwrap();
    // A document of its own, which a restart reads: nothing of the chain's.
    {
        let mut document = automerge::AutoCommit::new();
        document.put(automerge::ROOT, "title", "Welcome").unwrap();
        let mut candidate = workspace.peer().clone();
        candidate
            .publication_mut()
            .publish_document(APP, "welcome", document.save(), &key, SystemTime::now())
            .unwrap();
        workspace
            .commit(candidate, &key, SystemTime::now(), |_, _| Ok(state.clone()))
            .unwrap();
    }
    for index in 0..parts {
        let part = chain.part(u64::try_from(index).unwrap());
        let mut document = automerge::AutoCommit::new();
        document.put(automerge::ROOT, "text", text(BYTES)).unwrap();
        let mut candidate = workspace.peer().clone();
        candidate
            .publication_mut()
            .publish_document(APP, part.as_str(), document.save(), &key, SystemTime::now())
            .unwrap();
        workspace
            .commit(candidate, &key, SystemTime::now(), |_, _| Ok(state.clone()))
            .unwrap();
    }
    let path = workspace.path.clone();
    let held = std::fs::read_dir(path.with_extension("parts"))
        .unwrap()
        .count();
    assert!(held > 0, "the chain is kept in parts of its own");
    drop(workspace);

    // A hard stop: the next start opens what the parts hold and nothing more.
    let before = Io::now();
    let started = std::time::Instant::now();
    let reopened = GovernedWorkspace::open(path, None).unwrap();
    let reopened_in = started.elapsed();
    let opened = Io::now().since(before);
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let before = Io::now();
    let read = runtime
        .block_on(reopened.readable_document(APP, "welcome"))
        .unwrap()
        .unwrap();
    let answered = Io::now().since(before);
    println!(
        "a restart of {parts} parts of {BYTES} bytes opened in {reopened_in:?} with {opened:?}, \
         and answered with {answered:?}"
    );
    assert_eq!(
        automerge::AutoCommit::load(&read)
            .unwrap()
            .get(automerge::ROOT, "title")
            .unwrap()
            .and_then(|(value, _)| value.into_string().ok())
            .as_deref(),
        Some("Welcome"),
        "the document read is the one asked for"
    );
    assert!(
        reopened.peer().holds_document(APP, chain.part(0).as_str()),
        "the chain's parts are held"
    );
    Restart { opened, answered }
}

#[test]
fn a_restart_does_the_same_work_for_each_part_of_a_long_chain() {
    let short = restart_with_chain_of(SHORT);
    let long = restart_with_chain_of(LONG);
    // Per part, compared without division: the long chain's work times the short chain's
    // parts against the short chain's times the long chain's, within a quarter.
    let (short_parts, long_parts) = (u64::try_from(SHORT).unwrap(), u64::try_from(LONG).unwrap());
    let within = |long: u64, short: u64, slack: u64| {
        long.saturating_mul(short_parts).saturating_mul(4)
            <= short
                .saturating_mul(long_parts)
                .saturating_mul(5)
                .saturating_add(
                    slack
                        .saturating_mul(long_parts)
                        .saturating_mul(short_parts)
                        .saturating_mul(4),
                )
    };
    assert!(
        within(long.opened.calls, short.opened.calls, 1),
        "a restart's open made {} system calls for {LONG} parts, {} for {SHORT}",
        long.opened.calls,
        short.opened.calls
    );
    assert!(
        within(long.opened.bytes, short.opened.bytes, 0),
        "a restart's open moved {} bytes for {LONG} parts, {} for {SHORT}",
        long.opened.bytes,
        short.opened.bytes
    );
    // Its first answer reads the document asked for, none of the chain's.
    assert!(
        long.answered.calls <= short.answered.calls.saturating_add(4)
            && long.answered.bytes
                <= short
                    .answered
                    .bytes
                    .saturating_mul(5)
                    .div_ceil(4)
                    .saturating_add(4096),
        "a restart's first answer took {:?} after a chain of {LONG} parts, {:?} after {SHORT}",
        long.answered,
        short.answered
    );
}
