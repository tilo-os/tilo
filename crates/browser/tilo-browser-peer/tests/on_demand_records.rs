//! A joining page is replayed a room's eager records and the head of each
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

#[path = "support/steward.rs"]
mod steward;
mod support;

use std::collections::BTreeMap;

use collab_types::DocumentMutation;
use tilo_browser_peer::{DocumentRoute, Host, PeerEvent, PeerHandle};
use tilo_user_identity::{DeviceCapabilities, UserIdentity};

use support::{enrol, ready, route, start_host, start_peer, wait_for, workspace};

fn record(record: String) -> DocumentRoute {
    DocumentRoute { record, ..route() }
}

fn part(index: u64) -> DocumentRoute {
    record(neighbour_wire::record_part("chat", index))
}

fn change(text: &str) -> DocumentMutation {
    DocumentMutation::Incremental(text.as_bytes().to_vec())
}

/// A record's first publication, which the host keeps as its checkpoint.
fn first(text: &str) -> DocumentMutation {
    DocumentMutation::Snapshot(text.as_bytes().to_vec())
}

/// What a page received, by record.
#[derive(Default)]
struct Received {
    documents: BTreeMap<String, Vec<DocumentMutation>>,
    opened: Vec<(String, u32)>,
}

impl Received {
    fn take(&mut self, peer: &PeerHandle) {
        for event in peer.drain() {
            match event {
                PeerEvent::Document(document) => {
                    if let Some(mutation) = document.mutation() {
                        self.documents
                            .entry(document.route.record.clone())
                            .or_default()
                            .push(mutation);
                    }
                }
                PeerEvent::RecordOpened { route, frames, .. } => {
                    self.opened.push((route.record, frames));
                }
                _ => {}
            }
        }
    }

    fn of(&self, route: &DocumentRoute) -> &[DocumentMutation] {
        self.documents.get(&route.record).map_or(&[], Vec::as_slice)
    }
}

/// Take `peer`'s events until `done` holds.
async fn until(
    what: &str,
    peer: &PeerHandle,
    received: &mut Received,
    done: impl Fn(&Received) -> bool,
) {
    wait_for(what, || {
        received.take(peer);
        let finished = done(received);
        async move { finished.then_some(()) }
    })
    .await;
}

async fn durable(peer: &PeerHandle) {
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
    // Records are sealed: the room's steward admits each page to them.
    let _steward = steward::Steward::start(&host.relay, &root).await;
    let peer = |device: support::Device| {
        start_peer(
            Host::Https(host.https()),
            device.cert,
            host_device.cert.clone(),
            device.key,
        )
    };
    let author = peer(author);
    author.join(workspace()).unwrap();
    ready(&[&author]).await;
    let note = route();
    let archive = record(neighbour_wire::on_demand_record("uuid:archive"));
    author.publish(note.clone(), &first("note")).unwrap();
    author.publish(archive.clone(), &first("archive")).unwrap();
    author.publish(part(0), &first("part 0")).unwrap();
    author.publish(part(1), &first("part 1")).unwrap();
    durable(&author).await;

    // Two pages join: each is replayed the note and the chain's head only.
    let early = peer(early);
    let reader = peer(reader);
    early.join(workspace()).unwrap();
    reader.join(workspace()).unwrap();
    let (mut at_early, mut at_reader) = (Received::default(), Received::default());
    for (page, received) in [(&early, &mut at_early), (&reader, &mut at_reader)] {
        until("the eager records", page, received, |received| {
            !received.of(&note).is_empty() && !received.of(&part(1)).is_empty()
        })
        .await;
        ready(&[page]).await;
        received.take(page);
        assert!(received.of(&part(0)).is_empty(), "an earlier part waits");
        assert!(
            received.of(&archive).is_empty(),
            "an on-demand record waits"
        );
    }

    // The reader opens the earlier part and the archive: their history
    // arrives, and then their changes.
    reader.open_record(workspace(), part(0)).unwrap();
    reader.open_record(workspace(), archive.clone()).unwrap();
    // The host's answer comes once it sent the history; the history opens
    // once the steward admitted the page to each record.
    until("the opened records", &reader, &mut at_reader, |received| {
        received.opened.len() == 2
            && !received.of(&part(0)).is_empty()
            && !received.of(&archive).is_empty()
    })
    .await;
    assert_eq!(at_reader.of(&part(0)), [first("part 0")]);
    assert_eq!(at_reader.of(&archive), [first("archive")]);
    assert!(at_reader.opened.contains(&(part(0).record, 1)));

    author.publish(part(0), &change("an edit")).unwrap();
    author.publish(archive.clone(), &change("filed")).unwrap();
    // A later part starts the chain anew; the part before it is still
    // followed by the pages that held it.
    author.publish(part(2), &first("part 2")).unwrap();
    author.publish(part(1), &change("a reaction")).unwrap();
    author.publish(note.clone(), &change("marker")).unwrap();
    durable(&author).await;
    for (page, received) in [(&early, &mut at_early), (&reader, &mut at_reader)] {
        // The new part opens once the steward admitted the page to it; a
        // change that reached a page it should not have would have opened
        // by then too.
        until("the marker and the new part", page, received, |received| {
            received.of(&note).len() == 2
                && received.of(&part(1)).len() == 2
                && !received.of(&part(2)).is_empty()
        })
        .await;
        assert_eq!(received.of(&part(2)), [first("part 2")]);
        assert_eq!(
            received.of(&part(1)),
            [first("part 1"), change("a reaction")]
        );
    }
    assert_eq!(at_reader.of(&part(0)), [first("part 0"), change("an edit")]);
    assert_eq!(at_reader.of(&archive), [first("archive"), change("filed")]);
    assert!(
        at_early.of(&part(0)).is_empty() && at_early.of(&archive).is_empty(),
        "a page that opened neither record gets none of their changes"
    );

    // A page joining now is replayed the new head, not the part before it.
    let late = peer(late);
    late.join(workspace()).unwrap();
    let mut at_late = Received::default();
    until("the new head", &late, &mut at_late, |received| {
        received.of(&note).len() == 2 && !received.of(&part(2)).is_empty()
    })
    .await;
    ready(&[&late]).await;
    at_late.take(&late);
    assert!(at_late.of(&part(1)).is_empty() && at_late.of(&part(0)).is_empty());
}
