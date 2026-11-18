//! A device ended while changes arrive hears its workspace again once it starts.
//!
//! A page keeps what it holds of a workspace in its profile, and its program is ended at any
//! instant: a phone ends an app in the background, a person quits one. What its profile holds
//! then is what it had accepted up to some write, which can be less than its server sent it.
//! Started again over that profile, the device must take in the next change somebody else makes,
//! wherever the end fell, and whatever became of the note's history on the server meanwhile.
//!
//! The profile here keeps every write it accepts, in order. While a burst of changes arrives,
//! the device's session writes into it, and afterwards the device is started again over each
//! state the profile passed through, as an end at that write leaves it: between a change
//! received, applied and kept. Each time another device then makes one more change, and the
//! device started again must come to read it. An end the device never hears again after is
//! named with the write it fell at and what its replica refused.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::print_stderr,
    clippy::too_many_lines,
    clippy::significant_drop_tightening,
    clippy::arithmetic_side_effects,
    clippy::indexing_slicing,
    reason = "in a test these are the assertion, not a hazard"
)]

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use automerge::AutoCommit;
use tilo_session_client::{Cmd, Link, Outbox as _, PeerConfig, PeerEntry, PeerStore};
use tilo_user_identity::{DeviceCapabilities, DeviceCert, RevocationList, UserIdentity};

use crate::support::{
    Device, TestHost as Server, WORKSPACE, durable, enrol, holds, ready, refused, route, said, say,
    start_host as start_server, start_peer, wait_for, wait_keyed, workspace,
};

/// Changes that arrive while the device takes them in, four at a time.
const BURST: usize = 16;
/// Changes written while the device is away, enough for the server to have the note folded.
const MEANWHILE: usize = 64;
/// The most ends tried, spread evenly over the writes the profile accepted during the burst.
const MOST_ENDS: usize = 24;
/// After this many ends the device never heard again from, the rest are not tried.
const MOST_DEAF: usize = 5;
/// How soon a device started again reads the next change.
const HEARS_WITHIN: Duration = Duration::from_secs(12);
/// How much longer one is given before it counts as never hearing it: a loaded machine is slow,
/// not deaf.
const SLOW_WITHIN: Duration = Duration::from_secs(25);

/// What a device's profile holds of its session.
#[derive(Clone, Default)]
struct Kept {
    entry: Option<PeerEntry>,
    keys: Option<Vec<u8>>,
    whole: Option<Vec<u8>>,
    beside: BTreeMap<String, Vec<u8>>,
    parts: BTreeMap<String, Vec<u8>>,
}

/// One write a profile accepted.
#[derive(Clone)]
enum Write {
    Entry(Box<PeerEntry>),
    Keys(Vec<u8>),
    Whole(Vec<u8>),
    Beside(String, Option<Vec<u8>>),
    Part(String, Vec<u8>),
    Appended(String, Vec<u8>),
}

impl Write {
    fn apply_to(self, kept: &mut Kept) {
        match self {
            Self::Entry(entry) => kept.entry = Some(*entry),
            Self::Keys(bytes) => kept.keys = Some(bytes),
            Self::Whole(bytes) => kept.whole = Some(bytes),
            Self::Beside(name, Some(bytes)) => {
                kept.beside.insert(name, bytes);
            }
            Self::Beside(name, None) => {
                kept.beside.remove(&name);
            }
            Self::Part(name, bytes) => {
                kept.parts.insert(name, bytes);
            }
            Self::Appended(name, bytes) => {
                kept.parts
                    .entry(name)
                    .or_default()
                    .extend_from_slice(&bytes);
            }
        }
    }

    /// What the write was, for whoever reads a failure.
    fn named(&self) -> String {
        // A part's name starts with what it is: the index, a snapshot (w), a journal (l).
        let part = |name: &str| match name.chars().next() {
            Some('i') => "the index of what it holds",
            Some('w') => "a snapshot of the workspace",
            Some('l') => "the journal after the snapshot",
            _ => "a part",
        };
        match self {
            Self::Entry(_) => "the device's entry".into(),
            Self::Keys(bytes) => format!("its keys, {} bytes", bytes.len()),
            Self::Whole(bytes) => format!("everything it holds whole, {} bytes", bytes.len()),
            Self::Beside(_, Some(bytes)) => format!("its own writes kept, {} bytes", bytes.len()),
            Self::Beside(_, None) => "its own writes let go".into(),
            Self::Part(name, bytes) => format!("{} written, {} bytes", part(name), bytes.len()),
            Self::Appended(name, bytes) => {
                format!("{} appended to, {} bytes", part(name), bytes.len())
            }
        }
    }
}

/// A device's profile, as a page's is: it keeps what its session writes, and remembers every
/// write in order, so the device can be started again over what any of them left.
struct Profile {
    /// The key what it keeps of its keys is sealed under: the device's, whichever session it is in.
    vault: [u8; 32],
    kept: Mutex<Kept>,
    writes: Mutex<Vec<Write>>,
}

impl Profile {
    fn new() -> Arc<Self> {
        let mut vault = [0; 32];
        getrandom::fill(&mut vault).unwrap();
        Arc::new(Self {
            vault,
            kept: Mutex::default(),
            writes: Mutex::default(),
        })
    }

    fn accept(&self, write: Write) {
        write.clone().apply_to(&mut self.kept.lock().unwrap());
        self.writes.lock().unwrap().push(write);
    }

    /// How many writes it accepted so far.
    fn accepted(&self) -> usize {
        self.writes.lock().unwrap().len()
    }

    /// The profile as an end of the device's program after its first `count` writes leaves it.
    fn ended_after(&self, count: usize) -> Arc<Self> {
        let mut kept = Kept::default();
        for write in self.writes.lock().unwrap().iter().take(count) {
            write.clone().apply_to(&mut kept);
        }
        Arc::new(Self {
            vault: self.vault,
            kept: Mutex::new(kept),
            writes: Mutex::default(),
        })
    }

    /// What the last of its first `count` writes was.
    fn last_of(&self, count: usize) -> String {
        count
            .checked_sub(1)
            .and_then(|last| self.writes.lock().unwrap().get(last).map(Write::named))
            .unwrap_or_else(|| "nothing".into())
    }
}

impl PeerStore for Profile {
    fn load(&self) -> Option<PeerEntry> {
        self.kept.lock().unwrap().entry.clone()
    }

    fn save(&self, entry: &PeerEntry) -> Result<(), String> {
        self.accept(Write::Entry(Box::new(entry.clone())));
        Ok(())
    }

    fn load_channels(&self) -> Option<Vec<u8>> {
        self.kept.lock().unwrap().keys.clone()
    }

    fn save_channels(&self, bytes: &[u8]) -> Result<(), String> {
        self.accept(Write::Keys(bytes.to_vec()));
        Ok(())
    }

    fn channel_vault(&self) -> Option<[u8; 32]> {
        Some(self.vault)
    }

    fn load_workspaces(&self) -> Option<Vec<u8>> {
        self.kept.lock().unwrap().whole.clone()
    }

    fn save_workspaces(&self, bytes: &[u8]) -> Result<(), String> {
        self.accept(Write::Whole(bytes.to_vec()));
        Ok(())
    }

    fn save_workspaces_run(&self, name: &str, bytes: Option<&[u8]>) -> Result<(), String> {
        self.accept(Write::Beside(name.to_owned(), bytes.map(<[u8]>::to_vec)));
        Ok(())
    }

    fn load_workspaces_run(&self, name: &str) -> Option<Vec<u8>> {
        self.kept.lock().unwrap().beside.get(name).cloned()
    }

    fn keeps_workspaces(&self) -> bool {
        true
    }

    fn load_workspaces_part(&self, name: &str) -> Option<Vec<u8>> {
        self.kept.lock().unwrap().parts.get(name).cloned()
    }

    fn save_workspaces_part(&self, name: &str, bytes: &[u8]) -> Result<(), String> {
        self.accept(Write::Part(name.to_owned(), bytes.to_vec()));
        Ok(())
    }

    fn append_workspaces_part(&self, name: &str, bytes: &[u8]) -> Result<(), String> {
        self.accept(Write::Appended(name.to_owned(), bytes.to_vec()));
        Ok(())
    }

    fn keeps_workspaces_parts(&self) -> bool {
        true
    }
}

/// `device`'s session over `profile`, in the workspace with the note open.
fn started(
    server: &Server,
    server_cert: &DeviceCert,
    device: &Device,
    profile: Arc<Profile>,
) -> Link {
    let link = tilo_session_client::start_session(
        PeerConfig {
            host: server.iroh(),
            cert: device.cert.clone(),
            server_cert: server_cert.clone(),
            revocations: RevocationList::new(),
            entering: None,
        },
        Arc::new(device.key.clone()),
        profile,
    );
    link.join_placed(WORKSPACE, "Workspace").unwrap();
    link.send(Cmd::OpenRoute(route())).unwrap();
    link
}

/// How many of `author`'s writes to the note the server keeps as they were written: a fold of
/// the note takes their place.
fn kept_as_written(state: &Path, author: &[u8; 32]) -> usize {
    let note = blake3::hash(b"app\0so.tilo.notes\0note-1");
    let writes = state
        .join("workspaces")
        .join(workspace().to_string())
        .join("retained/v2")
        .join(format!(
            "{}-{}",
            hex::encode(author),
            hex::encode(note.as_bytes())
        ));
    let Ok(name) = std::fs::read_to_string(writes.join("CURRENT")) else {
        return 0;
    };
    std::fs::read_dir(writes.join(name.trim())).map_or(0, |entries| {
        entries
            .filter_map(Result::ok)
            .filter(|entry| entry.file_name().to_string_lossy().ends_with(".delta"))
            .count()
    })
}

/// Whether `device`'s replica comes to say `text` in the note within `within`.
async fn hears_within(device: &Link, text: &str, within: Duration) -> bool {
    let deadline = Instant::now() + within;
    loop {
        if said(device, &route()).contains(text) {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

/// The ends after which the device, started again, never read the next change: a device takes a
/// burst of changes in, its program is ended at each write its profile accepted meanwhile, and
/// with `folded` the note's writer goes on until the server has the note's history folded
/// before the device is back.
async fn deaf_ends(folded: bool) -> Vec<String> {
    let (root, _) = UserIdentity::generate("Ada", 1);
    let server_device = enrol(&root, "home-server", DeviceCapabilities::member());
    let browser = enrol(&root, "browser", DeviceCapabilities::member());
    let phone = enrol(&root, "phone", DeviceCapabilities::member());
    let state = tempfile::tempdir().unwrap();
    let server = start_server(
        &root,
        &server_device,
        state.path(),
        "127.0.0.1:0".parse().unwrap(),
    )
    .await;
    let note = route();

    let browser_key = browser.cert.device_public_key;
    let writer = start_peer(
        server.iroh(),
        browser.cert,
        server_device.cert.clone(),
        browser.key,
    );
    writer.join_placed(WORKSPACE, "Workspace").unwrap();
    writer.send(Cmd::OpenRoute(note.clone())).unwrap();
    let profile = Profile::new();
    let first = started(&server, &server_device.cert, &phone, Arc::clone(&profile));
    ready(&[&writer, &first]).await;
    wait_keyed(&writer, &note).await;

    // The note is there and the device reads it before the burst.
    let mut doc = AutoCommit::new();
    let mut written = 1;
    writer
        .write(note.clone(), &say(&mut doc, "founding"))
        .unwrap();
    durable(&writer).await;
    holds(&first, &note, &doc).await;
    let before = profile.accepted();
    for group in 0..BURST / 4 {
        for change in 0..4 {
            writer
                .write(
                    note.clone(),
                    &say(&mut doc, &format!("burst {group}.{change}")),
                )
                .unwrap();
            written += 1;
        }
        durable(&writer).await;
        holds(&first, &note, &doc).await;
    }
    let after = profile.accepted();
    first.close();
    tokio::time::sleep(Duration::from_millis(300)).await;

    if folded {
        let compactor = server.server.compactor().clone();
        let folds = compactor.rounds();
        assert!(
            kept_as_written(state.path(), &browser_key) > BURST,
            "the server keeps the burst as it was written until the note is folded"
        );
        for change in 0..MEANWHILE {
            writer
                .write(note.clone(), &say(&mut doc, &format!("meanwhile {change}")))
                .unwrap();
            written += 1;
        }
        durable(&writer).await;
        wait_for("the server to have the note folded", || {
            let done = compactor.rounds() > folds
                && kept_as_written(state.path(), &browser_key) < MEANWHILE / 2;
            async move { done.then_some(()) }
        })
        .await;
    }

    // The ends tried, the last first: a device ended after everything it took in was kept is
    // the one that must hear whatever else holds.
    let count = after - before + 1;
    let tried: Vec<usize> = if count > MOST_ENDS {
        let mut spread: Vec<usize> = (0..MOST_ENDS)
            .map(|n| before + n * (count - 1) / (MOST_ENDS - 1))
            .collect();
        spread.dedup();
        spread
    } else {
        (before..=after).collect()
    };
    eprintln!(
        "KEPT-AFTER-KILL folded={folded}: the profile accepted {before} writes before the burst \
         and {after} by its end; {} ends tried",
        tried.len()
    );
    let mut deaf = Vec::new();
    for end in tried.into_iter().rev() {
        if deaf.len() >= MOST_DEAF {
            deaf.push(format!("the ends before write {end} were not tried"));
            break;
        }
        let again = started(
            &server,
            &server_device.cert,
            &phone,
            profile.ended_after(end),
        );
        let marker = format!("after an end at write {end}");
        writer.write(note.clone(), &say(&mut doc, &marker)).unwrap();
        written += 1;
        durable(&writer).await;
        let began = Instant::now();
        let heard = hears_within(&again, &marker, HEARS_WITHIN).await
            || hears_within(&again, &marker, SLOW_WITHIN).await;
        let fell_at = profile.last_of(end);
        if heard {
            eprintln!(
                "KEPT-AFTER-KILL folded={folded} end={end} ({fell_at}): heard after {} ms",
                began.elapsed().as_millis()
            );
        } else {
            let line = format!(
                "ended after write {end} of {after} ({fell_at}): it reads {} of the note's {written} \
                 changes; {}; failures {:?}",
                said(&again, &note).len(),
                refused(&again).unwrap_or_else(|| "its replica refused nothing".into()),
                again.status().failures,
            );
            eprintln!("KEPT-AFTER-KILL folded={folded} DEAF {line}");
            deaf.push(line);
        }
        again.close();
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    writer.close();
    server.server.shutdown().await;
    deaf
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_device_ended_while_changes_arrive_hears_the_next_change() {
    let deaf = Box::pin(deaf_ends(false)).await;
    assert!(
        deaf.is_empty(),
        "started again, the device never read the next change:\n{}",
        deaf.join("\n")
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_device_ended_while_changes_arrive_hears_on_once_the_note_was_folded() {
    let deaf = Box::pin(deaf_ends(true)).await;
    assert!(
        deaf.is_empty(),
        "started again after the note was folded, the device never read the next change:\n{}",
        deaf.join("\n")
    );
}
