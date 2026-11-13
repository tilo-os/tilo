//! The work a fresh guest's page does to take in a public workspace's chat: the chat's newest
//! part, 200 posts, and its record, which holds an introduction from each of 300 people who
//! opened it, each of those a write of its own.
//!
//! A page takes in every write it is sent one at a time: it admits it, opens it (the chat is
//! sealed), applies its changes, and reads the record again as it lands. Sent the chat's history
//! write by write, a page spent seconds of its main thread on it before Chat showed anything.
//! The workspace's host folds a record's history into one snapshot once a few dozen writes
//! built up, so a newcomer is sent that snapshot and the writes since; and a page reads a record
//! whole as a saved document, which loads at once, rather than as its changes one by one, which
//! load by applying each in turn.
//!
//! Counted here, for a guest that opens the chat as Chat does as it enters:
//! - the writes the host sends it of the chat ([`Event::RecordOpened`]'s frames),
//! - the writes its replica takes in ([`tilo_session_client::arrivals`]),
//! - the reads the page makes, once per batch of landings, as the page's replication reads,
//! - the bytes of the whole read the page hands its windows (and its mentions, and its store).

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::too_many_lines,
    clippy::significant_drop_tightening,
    clippy::arithmetic_side_effects,
    clippy::print_stderr,
    reason = "in a test these are the assertion, not a hazard"
)]

tilo_automerge_isolate::test_worker!();

use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use automerge::{AutoCommit, ReadDoc as _, transaction::Transactable as _};
use collab_types::DocumentMutation;
use ed25519_dalek::SigningKey;
use getrandom::{SysRng, rand_core::UnwrapErr};
use tilo_automerge::ApplyIncoming as _;
use tilo_peer_state::document_frame::DocumentRoute;
use tilo_relay_client::Host;
use tilo_relay_protocol::WorkspaceId;
use tilo_session_client::device::{Entering, door_entry};
use tilo_session_client::engine::RecordReading;
use tilo_session_client::{Cmd, Event, Link, MemoryPeerStore, Outbox as _};
use tilo_user_identity::UserIdentity;

use crate::support::{enrol, enter_page, start_workspace_host_relaying, wait_keyed, written};

/// The chain the chat keeps its posts in.
const CHAT_CHAIN: &str = "messages";
/// The posts in the chat's newest part.
const POSTS: usize = 200;
/// The people who introduced themselves in the chat's record.
const INTRODUCED: usize = 300;
/// How many people's devices write those introductions, in turn.
const INTRODUCERS: usize = 6;

/// The writes of the chat a guest may be sent: per record, the host's compaction and the writes
/// made since it, fewer than the few dozen after which the host folds again. Sent write by
/// write, the chat was 500.
const SENT_BUDGET: u32 = 2 * (1 + 32);
/// The writes of the chat a guest's replica may take in.
const TAKEN_IN_BUDGET: u64 = 2 * (1 + 32);
/// The reads a guest's page may make of the chat before it reads whole: one per batch of
/// landings, so a history that lands write by write is read again and again.
const READS_BUDGET: usize = 8;
/// The most bytes the whole read of the chat's part may be: its posts saved. Its posts as one
/// change after another, as the page was handed them, are several times that.
const WHOLE_READ_BUDGET: usize = 64 * 1024;

fn new_key() -> SigningKey {
    SigningKey::generate(&mut UnwrapErr(SysRng))
}

/// A page of `name` entering `workspace` through its door.
async fn arrive(
    host: Host,
    host_cert: &tilo_user_identity::DeviceCert,
    workspace: &str,
    name: &str,
) -> Link {
    let (root, key) = (new_key(), new_key());
    let (cert, entry) = door_entry(
        host_cert,
        workspace,
        &key,
        Entering {
            held: None,
            root: Some(&root),
            label: "Browser",
            name,
            token: None,
        },
    )
    .await
    .unwrap();
    enter_page(
        host,
        cert,
        host_cert.clone(),
        key,
        std::sync::Arc::new(MemoryPeerStore::default()),
        entry,
    )
}

/// A post of a few lines, as people write them: text that saves smaller than it is written.
fn post_text(post: usize) -> String {
    const WORDS: [&str; 12] = [
        "lounge", "record", "window", "desk", "share", "relay", "sync", "music", "photos",
        "terminal", "people", "notes",
    ];
    (0..150)
        .filter_map(|word| WORDS.get((post * 7 + word * 5) % WORDS.len()).copied())
        .collect::<Vec<_>>()
        .join(" ")
}

/// The change `edit` makes to `doc`, as the write that carries it: the record whole the first
/// time, what it changed after.
fn write(doc: &mut AutoCommit, edit: impl FnOnce(&mut AutoCommit)) -> DocumentMutation {
    let before = doc.get_heads();
    edit(doc);
    doc.commit();
    if before.is_empty() {
        DocumentMutation::Snapshot(doc.save())
    } else {
        DocumentMutation::Incremental(doc.save_after(&before))
    }
}

/// What the guest did to take the chat in.
#[derive(Debug, Default)]
struct Work {
    /// Writes the host sent, per record.
    sent: BTreeMap<String, u32>,
    /// Writes the guest's replica took in.
    taken_in: u64,
    /// Reads the page made of the chat.
    reads: usize,
    /// Of those, reads refused because the record was still arriving.
    refused: usize,
    /// Bytes of the last whole read of the chat's part.
    whole_read: usize,
    took: Duration,
}

/// The guest opens the chat as Chat does as it enters (its record and its chain's head), and
/// reads each record as the page's replication does: once per batch of landings, the changes
/// since the heads it read last where it read before, until both read whole.
async fn guest_takes_in(
    page: &Link,
    workspace_id: WorkspaceId,
    chat: &DocumentRoute,
    part: &DocumentRoute,
) -> Work {
    let head = DocumentRoute {
        record: neighbour_wire::chain_head(CHAT_CHAIN),
        ..part.clone()
    };
    let landed_before = *page.engine().arrivals().watching().borrow();
    let opened = Instant::now();
    page.send(Cmd::Join(workspace_id)).unwrap();
    for route in [chat, part] {
        page.send(Cmd::OpenRoute(route.clone())).unwrap();
    }
    page.send(Cmd::OpenRecordAt(workspace_id, chat.clone()))
        .unwrap();
    page.send(Cmd::OpenRecordAt(workspace_id, head.clone()))
        .unwrap();
    let mut work = Work::default();
    let mut read_so_far: BTreeMap<String, (Vec<[u8; 32]>, AutoCommit)> = BTreeMap::new();
    let deadline = Instant::now() + Duration::from_secs(120);
    loop {
        let mut landed = std::collections::BTreeSet::new();
        for event in page.drain() {
            match event {
                Event::Landed { route, .. } if route == *chat || route == *part => {
                    landed.insert(route.record.clone());
                }
                Event::RecordOpened {
                    route: Some(route),
                    frames,
                    ..
                } if route == *chat || route == head => {
                    work.sent.insert(route.record.clone(), frames);
                }
                _ => {}
            }
        }
        for record in landed {
            let route = if record == chat.record { chat } else { part };
            let heads = read_so_far.get(&record).map(|(heads, _)| heads.clone());
            work.reads += 1;
            let read = page.engine().record_update(
                &route.cluster_id,
                &route.app_id,
                &route.record,
                heads.as_deref(),
            );
            // A read refused while the record is still arriving (a write that builds on one
            // not here yet) is work the page did for nothing: it reads again at the next landing.
            let Ok(read) = read else {
                work.refused += 1;
                continue;
            };
            match read {
                RecordReading::Captured { bytes, heads, .. } => {
                    if record == part.record {
                        work.whole_read = bytes.len();
                    }
                    read_so_far.insert(record, (heads, AutoCommit::load(&bytes).unwrap()));
                }
                RecordReading::Changes { bytes, heads, .. } => {
                    let (kept, doc) = read_so_far.get_mut(&record).unwrap();
                    doc.load_incoming(&bytes).unwrap();
                    *kept = heads;
                }
                RecordReading::Unchanged(_) | RecordReading::Pending => {}
            }
        }
        let read_whole = |record: &str, keys: usize| {
            read_so_far
                .get(record)
                .is_some_and(|(_, doc)| doc.keys(automerge::ROOT).count() >= keys)
        };
        if read_whole(&part.record, POSTS) && read_whole(&chat.record, INTRODUCED) {
            work.took = opened.elapsed();
            work.taken_in = page
                .engine()
                .arrivals()
                .watching()
                .borrow()
                .wrapping_sub(landed_before);
            return work;
        }
        assert!(
            Instant::now() < deadline,
            "the chat never read whole at the guest: {work:?}"
        );
        tokio::time::sleep(Duration::from_millis(1)).await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 8)]
async fn a_guest_takes_in_a_crowded_chat_as_a_snapshot_and_the_writes_since() {
    let (owner, _) = UserIdentity::generate("Owner", 1);
    let host_device = enrol(
        &owner,
        "lounge-host",
        tilo_user_identity::DeviceCapabilities::host(),
    );
    let state = crate::support::scratch_dir();
    let host = start_workspace_host_relaying(&owner, &host_device, state.path(), false, true).await;
    let workspaces = host.relay.workspaces().unwrap();
    let lounge = workspaces.create("Lounge").await.unwrap();
    workspaces
        .publish(
            &lounge.workspace,
            &host_device.cert,
            tilo_relay_protocol::directory::Listing {
                description: "Everyone".into(),
                default_role: tilo_relay_protocol::workspaces::Role::Editor,
            },
        )
        .await
        .unwrap();
    let address = host.relay.node_addr();
    let relay_url = address.relay_urls().next().cloned();
    let page_host = || Host::through(iroh::EndpointAddr::new(address.id), relay_url.clone());
    let chat = DocumentRoute {
        app_id: "so.tilo.chat".into(),
        cluster_id: lounge.workspace.clone(),
        record: String::new(),
    };
    let part = DocumentRoute {
        record: neighbour_wire::record_part(CHAT_CHAIN, 0),
        ..chat.clone()
    };

    // People who opened the chat introduced themselves in its record, each a write; one of them
    // posted the part's posts, each a write.
    let mut introducers = Vec::new();
    for n in 0..INTRODUCERS {
        let page = arrive(
            page_host(),
            &host_device.cert,
            &lounge.workspace,
            &format!("Member {n}"),
        )
        .await;
        page.send(Cmd::Join(lounge.workspace_id)).unwrap();
        introducers.push(page);
    }
    let mut founding = AutoCommit::new();
    let founded = write(&mut founding, |doc| {
        doc.put(automerge::ROOT, "kind", "chat").unwrap();
    });
    written(&introducers[0], &chat, &founded).await;
    let founded = founding.save();
    let mut copies: Vec<AutoCommit> = introducers
        .iter()
        .map(|_| AutoCommit::load(&founded).unwrap())
        .collect();
    for person in 0..INTRODUCED {
        let by = person % INTRODUCERS;
        let introduction = write(&mut copies[by], |doc| {
            doc.put(
                automerge::ROOT,
                format!("person-{person}"),
                format!("Member {person}"),
            )
            .unwrap();
        });
        written(&introducers[by], &chat, &introduction).await;
    }
    let author = &introducers[0];
    let mut part_doc = AutoCommit::new();
    for post in 0..POSTS {
        let posted = write(&mut part_doc, |doc| {
            doc.put(automerge::ROOT, format!("post-{post}"), post_text(post))
                .unwrap();
        });
        written(author, &part, &posted).await;
    }
    wait_keyed(author, &part).await;
    // The host folds each record's history once the chat is quiet: wait until it folded both
    // (a host that folds neither, or one, runs on to the guest when the wait is up).
    let compactor = host.relay.compactor();
    let deadline = Instant::now() + Duration::from_secs(60);
    while compactor.rounds() < 2 && Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    let rounds = compactor.rounds();

    let guest = arrive(page_host(), &host_device.cert, &lounge.workspace, "Guest").await;
    let work = guest_takes_in(&guest, lounge.workspace_id, &chat, &part).await;
    eprintln!("CHAT-LANDING-WORK host_rounds={rounds} {work:?}");
    let sent: u32 = work.sent.values().sum();
    assert!(
        sent <= SENT_BUDGET,
        "the guest was sent {sent} writes of the chat (budget {SENT_BUDGET}): {work:?}"
    );
    assert!(
        work.taken_in <= TAKEN_IN_BUDGET,
        "the guest's replica took in {} writes (budget {TAKEN_IN_BUDGET}): {work:?}",
        work.taken_in
    );
    assert!(
        work.reads <= READS_BUDGET,
        "the guest's page read the chat {} times before it read whole (budget {READS_BUDGET}): {work:?}",
        work.reads
    );
    assert!(
        work.whole_read <= WHOLE_READ_BUDGET,
        "the guest's whole read of the chat's part was {} bytes (budget {WHOLE_READ_BUDGET}): {work:?}",
        work.whole_read
    );
    guest.close();
    for page in introducers {
        page.close();
    }
}
