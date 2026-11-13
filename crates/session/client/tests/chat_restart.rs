//! A public workspace's chat across its host's restart: every write the host keeps of the chat
//! has every change it builds on, so the chat reads whole once the host is back.
//!
//! Members post to the chat's record and to its newest part, each post building on what the
//! member's page last heard of the others' (so one member's write depends on another's). The
//! host folds each record's history once a few dozen writes built up. Mid-stream, with posts still
//! on their way, the host stops (closed, or dropped as a killed process is) and starts again on
//! what it kept on disk. Then:
//! - every record the host holds reads whole: no write waits for a change the host lacks;
//! - the host's history keeps no write beside the fold that replaced it;
//! - every post the host told its author was durable is in the chat;
//! - members post on, the host folds again, and a newcomer reads the chat whole.
//!
//! A start once carried every write the workspace's link kept and the relay's history no longer
//! held back into that history, the writes a fold had replaced among them: they landed after the
//! fold, and a page that opened the chat took them in first and then refused the fold as covering
//! writes it did not hold, so the chat never read for it.
//!
//! The point the host stops at, and how, is drawn at random each round; `TILO_CHAT_RESTART_SEED`
//! repeats a round, `TILO_CHAT_RESTART_ROUNDS` runs more of them.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::too_many_lines,
    clippy::significant_drop_tightening,
    clippy::arithmetic_side_effects,
    clippy::print_stderr,
    clippy::indexing_slicing,
    clippy::cast_possible_truncation,
    clippy::format_push_string,
    reason = "in a test these are the assertion, not a hazard"
)]

tilo_automerge_isolate::test_worker!();

use std::collections::BTreeSet;
use std::time::{Duration, Instant};

use automerge::{AutoCommit, ReadDoc as _, transaction::Transactable as _};
use collab_types::DocumentMutation;
use ed25519_dalek::SigningKey;
use getrandom::{SysRng, rand_core::UnwrapErr};
use tilo_peer_state::document_frame::DocumentRoute;
use tilo_relay_client::Host;
use tilo_session_client::device::{Entering, door_entry};
use tilo_session_client::engine::RecordReading;
use tilo_session_client::{Cmd, Link, MemoryPeerStore, Outbox as _};
use tilo_user_identity::UserIdentity;

use crate::support::{
    TestHost, durable, enrol, enter_page, start_workspace_host_relaying, wait_keyed,
};
use collab_types::WorkspaceId;

/// The chain the chat keeps its posts in.
const CHAT_CHAIN: &str = "messages";
/// Members posting at once.
const MEMBERS: usize = 3;

fn new_key() -> SigningKey {
    SigningKey::generate(&mut UnwrapErr(SysRng))
}

/// A small deterministic generator, so a failing round repeats from its seed.
struct Draw(u64);

impl Draw {
    fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        x
    }

    fn below(&mut self, n: u64) -> u64 {
        self.next() % n.max(1)
    }
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

/// What `page`'s replica holds of `route` now, when it reads.
fn held(page: &Link, route: &DocumentRoute) -> Option<AutoCommit> {
    let bytes = page
        .engine()
        .record(&route.cluster_id, &route.app_id, &route.record)
        .ok()??;
    AutoCommit::load(&bytes).ok()
}

/// One member, with its own copy of each record it writes: what its page heard merged in before
/// each post, so the post builds on the other members' writes.
struct Member {
    page: Link,
    chat: AutoCommit,
    part: AutoCommit,
}

impl Member {
    /// Post `key` to `route` (the chat's record, or its part), built on what the page heard of it,
    /// and wait until the host keeps it when `wait`: then it is acknowledged.
    async fn post(&mut self, route: &DocumentRoute, is_part: bool, key: &str, wait: bool) {
        let copy = if is_part {
            &mut self.part
        } else {
            &mut self.chat
        };
        if let Some(mut heard) = held(&self.page, route) {
            tilo_automerge::ApplyIncoming::merge_incoming(copy, &mut heard).unwrap();
        }
        let before = copy.get_heads();
        copy.put(automerge::ROOT, key, key).unwrap();
        copy.commit();
        let mutation = if before.is_empty() {
            DocumentMutation::Snapshot(copy.save())
        } else {
            DocumentMutation::Incremental(copy.save_after(&before))
        };
        self.page.send(Cmd::OpenRoute(route.clone())).unwrap();
        wait_keyed(&self.page, route).await;
        self.page.write(route.clone(), &mutation).unwrap();
        if wait {
            durable(&self.page).await;
        }
    }
}

/// What the host retains of `route` and what its copy holds, write by write: what a failure
/// names to say which write a reader lacks.
async fn history(
    host: &TestHost,
    workspace: &str,
    workspace_id: WorkspaceId,
    route: &DocumentRoute,
) -> String {
    let mut out = String::from("retained:");
    for retained in host
        .relay
        .workspace_records()
        .retained(workspace_id)
        .await
        .unwrap_or_default()
    {
        let Ok(neighbour_wire::NeighbourMsg::AppDocument {
            record_id,
            envelope,
            ..
        }) = postcard::from_bytes::<neighbour_wire::NeighbourMsg>(&retained.body)
        else {
            continue;
        };
        if record_id != route.record {
            continue;
        }
        let covers = collab_types::SealedCompaction::parse(&envelope)
            .map(|compaction| {
                compaction
                    .covers
                    .iter()
                    .map(|(author, seq)| format!("{}<={seq}", hex::encode(&author.as_bytes()[..3])))
                    .collect::<Vec<_>>()
                    .join(",")
            })
            .unwrap_or_default();
        out.push_str(&format!(
            " {}#{}{:?}[{covers}]",
            hex::encode(&envelope.author.as_bytes()[..3]),
            envelope.seq,
            envelope.kind
        ));
    }
    out.push_str("\ngovernor:");
    if let Ok(Some(held)) = host
        .relay
        .workspaces()
        .unwrap()
        .record_history(workspace, &route.app_id, &route.record)
        .await
    {
        for write in held.writes {
            out.push_str(&format!(
                " {}#{}",
                hex::encode(&write.author[..3]),
                write.seq
            ));
        }
    }
    out
}

/// The writes of `route` the host retains beside a compaction of it that already covers them: a
/// fold replaced them, so one kept after it is history no reader takes in.
async fn covered_twice(host: &TestHost, workspace_id: WorkspaceId, route: &DocumentRoute) -> Vec<String> {
    let mut writes = Vec::new();
    let mut floors = std::collections::HashMap::<[u8; 32], u64>::new();
    for retained in host
        .relay
        .workspace_records()
        .retained(workspace_id)
        .await
        .unwrap_or_default()
    {
        let Ok(neighbour_wire::NeighbourMsg::AppDocument {
            record_id,
            envelope,
            ..
        }) = postcard::from_bytes::<neighbour_wire::NeighbourMsg>(&retained.body)
        else {
            continue;
        };
        if record_id != route.record {
            continue;
        }
        match collab_types::SealedCompaction::parse(&envelope) {
            Ok(compaction) => {
                for (author, floor) in compaction.covers {
                    let at = floors.entry(*author.as_bytes()).or_default();
                    *at = (*at).max(floor);
                }
            }
            Err(_) => writes.push((*envelope.author.as_bytes(), envelope.seq)),
        }
    }
    writes
        .into_iter()
        .filter(|(author, seq)| floors.get(author).is_some_and(|floor| seq <= floor))
        .map(|(author, seq)| format!("{}#{seq}", hex::encode(&author[..3])))
        .collect()
}

/// The host's records that do not read whole, with why.
async fn unread(host: &TestHost, workspace: &str) -> Vec<((String, String), String)> {
    host.relay
        .workspaces()
        .unwrap()
        .whole_records(workspace, None)
        .await
        .unwrap()
        .unread
}

/// Wait until the host's compactor ran `rounds` more rounds, or a while passed.
async fn folds(host: &TestHost, since: u64, rounds: u64, within: Duration) {
    let compactor = host.relay.compactor();
    let deadline = Instant::now() + within;
    while compactor.rounds() < since + rounds && Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

/// Wait until `page` reads `route` whole, holding every key of `keys`.
async fn reads_whole(
    page: &Link,
    route: &DocumentRoute,
    keys: &BTreeSet<String>,
    what: &str,
    (host, workspace, workspace_id): (&TestHost, &str, u64),
) {
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        let read =
            page.engine()
                .record_update(&route.cluster_id, &route.app_id, &route.record, None);
        if let Ok(RecordReading::Captured { bytes, .. }) = &read {
            let doc = AutoCommit::load(bytes).unwrap();
            let has: BTreeSet<String> = doc.keys(automerge::ROOT).collect();
            if keys.is_subset(&has) {
                return;
            }
            assert!(
                Instant::now() < deadline,
                "{what}: the chat's {:?} reads without {:?}",
                route.record,
                keys.difference(&has).collect::<Vec<_>>()
            );
        } else {
            if Instant::now() >= deadline {
                eprintln!(
                    "CHAT-RESTART history of {:?}:\n{}\npage: {}\nfailures: {:?}",
                    route.record,
                    history(host, workspace, workspace_id, route).await,
                    page.status().channels,
                    page.status().failures
                );
            }
            assert!(
                Instant::now() < deadline,
                "{what}: the chat's {:?} never read whole: {:?}",
                route.record,
                read.map(|read| matches!(read, RecordReading::Pending))
            );
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

/// One round: post, stop the host mid-stream, start it again, check, post on, check.
async fn round(seed: u64) {
    let mut draw = Draw(seed | 1);
    let began = Instant::now();
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
                default_role: tilo_relay_protocol::workspaces::WorkspaceRole::Editor,
            },
        )
        .await
        .unwrap();
    let chat = DocumentRoute {
        app_id: "so.tilo.chat".into(),
        cluster_id: lounge.workspace.clone(),
        record: String::new(),
    };
    let part = DocumentRoute {
        record: neighbour_wire::record_part(CHAT_CHAIN, 0),
        ..chat.clone()
    };
    let page_host = |host: &TestHost| {
        let address = host.relay.node_addr();
        let relay_url = address.relay_urls().next().cloned();
        Host::through(iroh::EndpointAddr::new(address.id), relay_url)
    };

    let mut members = Vec::new();
    for n in 0..MEMBERS {
        let page = arrive(
            page_host(&host),
            &host_device.cert,
            &lounge.workspace,
            &format!("Member {n}"),
        )
        .await;
        page.send(Cmd::Join(lounge.workspace_id)).unwrap();
        members.push(Member {
            page,
            chat: AutoCommit::new(),
            part: AutoCommit::new(),
        });
    }
    // The chat founded by one member, then heard by the rest before they post.
    members[0].post(&chat, false, "kind", true).await;
    members[0].post(&part, true, "post-founding", true).await;
    let mut acknowledged: [BTreeSet<String>; 2] = [
        BTreeSet::from(["kind".to_owned()]),
        BTreeSet::from(["post-founding".to_owned()]),
    ];

    // Enough posts that the host folds at least once before it stops, and stops at a point
    // drawn at random: after some acknowledged posts, with a few more on their way.
    let before_stop = 60 + draw.below(40) as usize;
    let in_flight = draw.below(6) as usize;
    let rounds_before = host.relay.compactor().rounds();
    for n in 0..before_stop + in_flight {
        let by = draw.below(members.len() as u64) as usize;
        let is_part = draw.below(4) != 0;
        let key = format!("post-{n}");
        let wait = n < before_stop;
        let route = if is_part { &part } else { &chat };
        members[by].post(route, is_part, &key, wait).await;
        if wait {
            acknowledged[usize::from(is_part)].insert(key);
        }
        // Now and then the chat goes quiet long enough for the host to fold it, and posting goes
        // on after the fold.
        if draw.below(24) == 0 {
            let rounds = host.relay.compactor().rounds();
            folds(&host, rounds, 1, Duration::from_secs(4)).await;
        }
    }
    folds(&host, rounds_before, 1, Duration::from_secs(60)).await;
    let unread_before = unread(&host, &lounge.workspace).await;
    assert!(
        unread_before.is_empty(),
        "seed {seed}: before the restart the host holds records that do not read whole: {unread_before:?}"
    );

    // The host stops: closed, or dropped as a process killed is, after a pause drawn at random
    // (the posts still on their way land, or do not).
    tokio::time::sleep(Duration::from_millis(draw.below(40))).await;
    let killed = draw.below(2) == 0;
    eprintln!(
        "CHAT-RESTART seed={seed} posts={before_stop} in_flight={in_flight} killed={killed} folds={} after {:?}",
        host.relay.compactor().rounds(),
        began.elapsed()
    );
    for member in &members {
        member.page.close();
    }
    if killed {
        drop(host);
    } else {
        drop(host.web);
        host.relay.shutdown().await;
    }
    tokio::time::sleep(Duration::from_millis(200)).await;

    let host = start_workspace_host_relaying(&owner, &host_device, state.path(), false, true).await;
    for route in [&chat, &part] {
        let twice = covered_twice(&host, lounge.workspace_id, route).await;
        assert!(
            twice.is_empty(),
            "seed {seed}: after the restart the host retains writes of {:?} beside the fold that \
             replaced them: {twice:?}",
            route.record
        );
    }
    let unread_after = unread(&host, &lounge.workspace).await;
    assert!(
        unread_after.is_empty(),
        "seed {seed}: after the restart the host holds records that do not read whole: {unread_after:?}"
    );

    // Members come back and post on; the host folds again; a newcomer reads the chat whole.
    let mut members = Vec::new();
    for n in 0..MEMBERS {
        let page = arrive(
            page_host(&host),
            &host_device.cert,
            &lounge.workspace,
            &format!("Returning {n}"),
        )
        .await;
        page.send(Cmd::Join(lounge.workspace_id)).unwrap();
        members.push(Member {
            page,
            chat: AutoCommit::new(),
            part: AutoCommit::new(),
        });
    }
    for member in &members {
        for route in [&chat, &part] {
            member.page.send(Cmd::OpenRoute(route.clone())).unwrap();
            member
                .page
                .send(Cmd::OpenRecordAt(lounge.workspace_id, route.clone()))
                .unwrap();
        }
    }
    for member in &members {
        reads_whole(
            &member.page,
            &chat,
            &acknowledged[0],
            "a returning member",
            (&host, &lounge.workspace, lounge.workspace_id),
        )
        .await;
        reads_whole(
            &member.page,
            &part,
            &acknowledged[1],
            "a returning member",
            (&host, &lounge.workspace, lounge.workspace_id),
        )
        .await;
    }
    let rounds_after = host.relay.compactor().rounds();
    for n in 0..48 {
        let by = draw.below(members.len() as u64) as usize;
        let is_part = draw.below(4) != 0;
        let key = format!("after-{n}");
        let route = if is_part { &part } else { &chat };
        members[by].post(route, is_part, &key, true).await;
        acknowledged[usize::from(is_part)].insert(key);
    }
    folds(&host, rounds_after, 1, Duration::from_secs(60)).await;
    let unread_later = unread(&host, &lounge.workspace).await;
    assert!(
        unread_later.is_empty(),
        "seed {seed}: posting on after the restart, the host holds records that do not read whole: {unread_later:?}"
    );
    let newcomer = arrive(
        page_host(&host),
        &host_device.cert,
        &lounge.workspace,
        "Newcomer",
    )
    .await;
    newcomer.send(Cmd::Join(lounge.workspace_id)).unwrap();
    for route in [&chat, &part] {
        newcomer.send(Cmd::OpenRoute(route.clone())).unwrap();
        newcomer
            .send(Cmd::OpenRecordAt(lounge.workspace_id, route.clone()))
            .unwrap();
    }
    reads_whole(
        &newcomer,
        &chat,
        &acknowledged[0],
        "a newcomer",
        (&host, &lounge.workspace, lounge.workspace_id),
    )
    .await;
    reads_whole(
        &newcomer,
        &part,
        &acknowledged[1],
        "a newcomer",
        (&host, &lounge.workspace, lounge.workspace_id),
    )
    .await;
    newcomer.close();
    for member in members {
        member.page.close();
    }
    eprintln!("CHAT-RESTART seed={seed} whole after {:?}", began.elapsed());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 8)]
async fn a_chat_reads_whole_after_its_host_restarts_mid_stream() {
    let rounds: u64 = std::env::var("TILO_CHAT_RESTART_ROUNDS")
        .ok()
        .and_then(|rounds| rounds.parse().ok())
        .unwrap_or(2);
    let first: u64 = std::env::var("TILO_CHAT_RESTART_SEED")
        .ok()
        .and_then(|seed| seed.parse().ok())
        .unwrap_or_else(|| {
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos() as u64
        });
    for n in 0..rounds {
        round(first.wrapping_add(n.wrapping_mul(0x9e37_79b9_7f4a_7c15))).await;
    }
}
