//! A joining page is replayed a workspace's eager records and the head of each
//! chained record only. It gets an on-demand record, or an earlier part of a
//! chain, when it opens it, and from then on that record's changes too; a
//! page that never opened it gets none of them. A page keeps following the
//! part that was its chain's head when it joined after a later part starts.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::too_many_lines,
    clippy::significant_drop_tightening,
    clippy::future_not_send,
    reason = "in a test these are the assertion, not a hazard"
)]

use std::collections::BTreeMap;
use tilo_session_client::{Cmd, Event, Link, Outbox};

use automerge::AutoCommit;
use collab_types::DocumentMutation;
use tilo_peer_state::document_frame::DocumentRoute;
use tilo_user_identity::{DeviceCapabilities, UserIdentity};

use crate::support::{
    enrol, hears, ready, route, say, start_host, start_peer, wait_for, workspace,
};

fn record(record: String) -> DocumentRoute {
    DocumentRoute { record, ..route() }
}

fn part(index: u64) -> DocumentRoute {
    record(neighbour_wire::record_part("chat", index))
}

/// Each record's document as its author writes it, by record.
#[derive(Default)]
struct Author(BTreeMap<String, AutoCommit>);

impl Author {
    fn change(&mut self, route: &DocumentRoute, text: &str) -> DocumentMutation {
        say(self.0.entry(route.record.clone()).or_default(), text)
    }

    /// A record's first publication, which the host keeps as its checkpoint.
    fn first(&mut self, route: &DocumentRoute, text: &str) -> DocumentMutation {
        let doc = self.0.entry(route.record.clone()).or_default();
        say(doc, text);
        DocumentMutation::Snapshot(doc.save())
    }
}

/// What landed in a page's replica, by record: how many writes, and the records
/// it opened.
#[derive(Default)]
struct Received {
    documents: BTreeMap<String, usize>,
    opened: Vec<(String, u32)>,
}

impl Received {
    fn take(&mut self, peer: &Link) {
        for event in peer.drain() {
            match event {
                Event::Landed {
                    route,
                    author: Some(_),
                } => {
                    let landed = self.documents.entry(route.record).or_default();
                    *landed = landed.saturating_add(1);
                }
                Event::RecordOpened {
                    route: Some(route),
                    frames,
                    ..
                } => {
                    self.opened.push((route.record, frames));
                }
                _ => {}
            }
        }
    }

    fn of(&self, route: &DocumentRoute) -> usize {
        self.documents.get(&route.record).copied().unwrap_or(0)
    }
}

/// Take `peer`'s events until `done` holds.
async fn until(what: &str, peer: &Link, received: &mut Received, done: impl Fn(&Received) -> bool) {
    wait_for(what, || {
        received.take(peer);
        let finished = done(received);
        async move { finished.then_some(()) }
    })
    .await;
}

async fn durable(peer: &Link) {
    let receipt = peer.barrier().unwrap();
    wait_for("durable", || async { receipt.try_recv().ok() })
        .await
        .unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_join_replays_eager_records_and_a_page_opens_the_rest() {
    let (root, _) = UserIdentity::generate("Ada", 1);
    let host_device = enrol(&root, "home-server", DeviceCapabilities::host());
    let author = enrol(&root, "browser", DeviceCapabilities::member());
    let early = enrol(&root, "tablet", DeviceCapabilities::member());
    let reader = enrol(&root, "phone", DeviceCapabilities::member());
    let late = enrol(&root, "laptop", DeviceCapabilities::member());
    let state = tempfile::tempdir().unwrap();
    let host = start_host(
        &root,
        &host_device,
        state.path(),
        "127.0.0.1:0".parse().unwrap(),
    )
    .await;
    // Records are sealed: a participant admits each page to them.
    let _laptop = crate::participant::Participant::start(&host.relay, &root).await;
    let peer = |device: crate::support::Device| {
        start_peer(
            host.iroh(),
            device.cert,
            host_device.cert.clone(),
            device.key,
        )
    };
    let author = peer(author);
    author
        .join_placed(crate::support::WORKSPACE, "Workspace")
        .unwrap();
    ready(&[&author]).await;
    let note = route();
    let archive = record(neighbour_wire::on_demand_record("uuid:archive"));
    let mut writes = Author::default();
    author
        .write(note.clone(), &writes.first(&note, "note"))
        .unwrap();
    author
        .write(archive.clone(), &writes.first(&archive, "archive"))
        .unwrap();
    author
        .write(part(0), &writes.first(&part(0), "part 0"))
        .unwrap();
    author
        .write(part(1), &writes.first(&part(1), "part 1"))
        .unwrap();
    durable(&author).await;

    // Two pages join: each is replayed the note and the chain's head only.
    let early = peer(early);
    let reader = peer(reader);
    early
        .join_placed(crate::support::WORKSPACE, "Workspace")
        .unwrap();
    reader
        .join_placed(crate::support::WORKSPACE, "Workspace")
        .unwrap();
    let (mut at_early, mut at_reader) = (Received::default(), Received::default());
    for (page, received) in [(&early, &mut at_early), (&reader, &mut at_reader)] {
        until("the eager records", page, received, |received| {
            received.of(&note) > 0 && received.of(&part(1)) > 0
        })
        .await;
        ready(&[page]).await;
        received.take(page);
        assert_eq!(received.of(&part(0)), 0, "an earlier part waits");
        assert_eq!(received.of(&archive), 0, "an on-demand record waits");
    }

    // The reader opens the earlier part and the archive: their history
    // arrives, and then their changes.
    reader
        .send(Cmd::OpenRecordAt(workspace(), part(0)))
        .unwrap();
    reader
        .send(Cmd::OpenRecordAt(workspace(), archive.clone()))
        .unwrap();
    // The host's answer comes once it sent the history; the history opens
    // once a participant admitted the page to each record.
    until("the opened records", &reader, &mut at_reader, |received| {
        received.opened.len() == 2 && received.of(&part(0)) > 0 && received.of(&archive) > 0
    })
    .await;
    hears(&reader, &part(0), "part 0").await;
    hears(&reader, &archive, "archive").await;
    assert!(at_reader.opened.contains(&(part(0).record, 1)));

    author
        .write(part(0), &writes.change(&part(0), "an edit"))
        .unwrap();
    author
        .write(archive.clone(), &writes.change(&archive, "filed"))
        .unwrap();
    // A later part starts the chain anew; the part before it is still
    // followed by the pages that held it.
    author
        .write(part(2), &writes.first(&part(2), "part 2"))
        .unwrap();
    author
        .write(part(1), &writes.change(&part(1), "a reaction"))
        .unwrap();
    author
        .write(note.clone(), &writes.change(&note, "marker"))
        .unwrap();
    durable(&author).await;
    for (page, received) in [(&early, &mut at_early), (&reader, &mut at_reader)] {
        // The new part opens once a participant admitted the page to it; a
        // change that reached a page it should not have would have opened
        // by then too.
        until("the marker and the new part", page, received, |received| {
            received.of(&note) == 2 && received.of(&part(1)) == 2 && received.of(&part(2)) > 0
        })
        .await;
        hears(page, &part(2), "part 2").await;
        hears(page, &part(1), "a reaction").await;
    }
    hears(&reader, &part(0), "an edit").await;
    hears(&reader, &archive, "filed").await;
    at_early.take(&early);
    assert!(
        at_early.of(&part(0)) == 0 && at_early.of(&archive) == 0,
        "a page that opened neither record gets none of their changes"
    );

    // A page joining now is replayed the new head, not the part before it.
    let late = peer(late);
    late.join_placed(crate::support::WORKSPACE, "Workspace")
        .unwrap();
    let mut at_late = Received::default();
    until("the new head", &late, &mut at_late, |received| {
        received.of(&note) > 0 && received.of(&part(2)) > 0
    })
    .await;
    ready(&[&late]).await;
    at_late.take(&late);
    assert!(at_late.of(&part(1)) == 0 && at_late.of(&part(0)) == 0);
}
