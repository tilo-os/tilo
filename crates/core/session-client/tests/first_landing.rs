//! A page that holds none of a record is never told the record landed before its replica holds
//! it, and reads a record whole when all of its content arrives in one landing.
//!
//! A page opening a record asks the workspace for it and for its channel's keys in the same
//! breath. The keys can be ready before the record's first write is: a page told the record
//! landed then reads a record its replica does not hold, and the read is refused. Nothing read
//! the record at that landing, so a record whose only landing is that one — its content came in
//! one piece and nothing followed — stays empty in every window that opened it.
//!
//! Here a page enters a public workspace holding nothing of it, opens a record the workspace has
//! none of and a record whose whole content is one snapshot, and reads at every landing. Every
//! read must be answered.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::significant_drop_tightening,
    clippy::future_not_send,
    clippy::arithmetic_side_effects,
    reason = "in a test these are the assertion, not a hazard"
)]

use crate::support::arrive;
use crate::support::changes_since;
use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use collab_types::DocumentMutation;

use tilo_peer_state::document_frame::DocumentRoute;
use tilo_server_client::ServerAddress;
use tilo_server_protocol::WorkspaceId;
use tilo_session_client::{Cmd, Event, Events, Link, Outbox as _};
use tilo_user_identity::UserIdentity;

use crate::support::{
    enrol, person_of, set_field, start_workspace_host_relaying, wait_keyed, written,
};

/// A record of the workspace's notes, named by `record`.
fn notes(lounge: &str, record: &str) -> DocumentRoute {
    DocumentRoute {
        app_id: "so.tilo.notes".into(),
        cluster_id: lounge.to_owned(),
        document: record.to_owned(),
    }
}

/// A change to `doc` setting `key`, as the write a page publishes: the first write of a record is
/// the whole document, every one after it the change since.
fn say(doc: &mut automerge::AutoCommit, writer: &Link, key: &str, text: &str) -> DocumentMutation {
    let before = doc.get_heads();
    set_field(doc, person_of(writer), "note", key, text);
    changes_since(doc, &before)
}

/// A page's reads at the landings it is told of: every one of `watched`'s landings is read, as
/// the page's replication reads it, and a refused read is kept.
struct Reader {
    events: Events,
    watched: Vec<DocumentRoute>,
    refusals: Vec<String>,
    reads: BTreeMap<String, usize>,
    /// What the workspace answered for each record the page opened: the frames it sent.
    opened: BTreeMap<String, u32>,
}

impl Reader {
    fn of(page: &Link, watched: Vec<DocumentRoute>) -> Self {
        Self {
            events: page.events(),
            watched,
            refusals: Vec::new(),
            reads: BTreeMap::new(),
            opened: BTreeMap::new(),
        }
    }

    /// Where `page` stands with `record`: its keys when it reads, and what it holds.
    fn holds(page: &Link, record: &DocumentRoute) -> Vec<String> {
        let Some(bytes) = crate::support::record_of(page, record) else {
            return Vec::new();
        };
        automerge::AutoCommit::load(&bytes).map_or_else(
            |_| Vec::new(),
            |doc| {
                use automerge::ReadDoc as _;
                let mut keys: Vec<String> = doc.keys(automerge::ROOT).collect();
                keys.sort_unstable();
                keys
            },
        )
    }

    /// Read every landing the page was told of since the last call.
    fn take(&mut self, page: &Link) {
        for event in self.events.drain() {
            if let Event::DocumentOpened {
                route: Some(route),
                answer: tilo_session_client::DocumentAnswer::Sent { frames },
                ..
            } = event
            {
                if self.watched.contains(&route) {
                    self.opened.insert(route.document, frames);
                }
                continue;
            }
            let Event::Landed { route, .. } = event else {
                continue;
            };
            if !self.watched.contains(&route) {
                continue;
            }
            match crate::support::reading_of(page, &route, None) {
                Ok(_) => *self.reads.entry(route.document.clone()).or_default() += 1,
                Err(error) => self
                    .refusals
                    .push(format!("{}/{}: {error}", route.app_id, route.document)),
            }
        }
    }
}

/// Drain the page's landings, reading each, until `done` says so.
async fn until(
    what: &str,
    reader: &mut Reader,
    page: &Link,
    done: impl Fn(&Reader, &Link) -> bool,
) {
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        reader.take(page);
        if done(reader, page) {
            return;
        }
        assert!(Instant::now() < deadline, "the page never {what}");
        tokio::time::sleep(Duration::from_millis(2)).await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 8)]
async fn every_landing_a_page_is_told_of_is_a_record_it_holds() {
    let (owner, _) = UserIdentity::generate("Owner", 1);
    let host_device = enrol(
        &owner,
        "lounge-host",
        tilo_user_identity::DeviceCapabilities::member(),
    );
    let state = crate::support::scratch_dir();
    // A workspace anyone may walk into: it sends a device its records when it opens them, so a
    // page that just came in holds none of them.
    let host = start_workspace_host_relaying(&owner, &host_device, state.path(), false, true).await;
    let workspaces = host.server.workspaces().unwrap();
    let lounge = workspaces.create("Lounge").await.unwrap();
    workspaces
        .found_entry(&lounge.workspace, &host_device.cert, |entry| {
            entry.listing = Some(tilo_action_policy::ListingPolicy {
                description: "Everyone".into(),
                default_role: tilo_server_protocol::workspaces::Role::Editor,
            });
        })
        .await
        .unwrap();
    let address = host.server.node_addr();
    let relay_url = address.relay_urls().next().cloned();
    let server_address =
        || ServerAddress::through(iroh::EndpointAddr::new(address.id), relay_url.clone());
    let workspace_id = WorkspaceId::parse(&lounge.workspace).unwrap();
    let written_document = notes(&lounge.workspace, "");
    let fresh_record = notes(
        &lounge.workspace,
        &neighbour_wire::on_demand_document("uuid:fresh"),
    );

    // A member wrote one record whole before anyone else came: one snapshot, and nothing after.
    let author = arrive(
        server_address(),
        &host_device.cert,
        &lounge.workspace,
        "Member",
    )
    .await;
    author.send(Cmd::Join(workspace_id)).unwrap();
    let mut doc = automerge::AutoCommit::new();
    let whole = say(&mut doc, &author, "supplier", "A contract");
    written(&author, &written_document, &whole).await;

    // A page comes in holding nothing and opens both records, one of which the workspace has
    // none of yet.
    let page = arrive(
        server_address(),
        &host_device.cert,
        &lounge.workspace,
        "Guest",
    )
    .await;
    let mut reader = Reader::of(&page, vec![written_document.clone(), fresh_record.clone()]);
    page.send(Cmd::Join(workspace_id)).unwrap();
    page.send(Cmd::OpenDocumentAt(workspace_id, written_document.clone()))
        .unwrap();
    page.send(Cmd::OpenDocumentAt(workspace_id, fresh_record.clone()))
        .unwrap();
    // What a window on the first record shows: the member's note, arrived whole in the landing
    // the page read it at.
    until(
        "read the record written whole",
        &mut reader,
        &page,
        |_, page| Reader::holds(page, &written_document) == ["supplier"],
    )
    .await;
    // The workspace answered the page's second record with nothing: it is one nobody wrote.
    until(
        "opened the record nobody wrote",
        &mut reader,
        &page,
        |reader, _| reader.opened.get(&fresh_record.document) == Some(&0),
    )
    .await;
    assert!(
        !reader.reads.contains_key(&fresh_record.document),
        "a record nobody wrote never landed"
    );
    assert!(
        Reader::holds(&page, &fresh_record).is_empty(),
        "the page holds none of a record nobody wrote"
    );

    // Now the record nobody wrote is written whole. However many landings the page was told of
    // for it, every one of them was a record the page held.
    let mut fresh = automerge::AutoCommit::new();
    let founding = say(&mut fresh, &author, "first", "one note");
    written(&author, &fresh_record, &founding).await;
    until(
        "read the record as it was written",
        &mut reader,
        &page,
        |_, page| Reader::holds(page, &fresh_record) == ["first"],
    )
    .await;

    // Two changes written back to back reach the page together: one landing, and one read of it
    // holds both.
    let second = say(&mut fresh, &author, "second", "another");
    let third = say(&mut fresh, &author, "third", "and more");
    author.send(Cmd::OpenRoute(fresh_record.clone())).unwrap();
    wait_keyed(&author, &fresh_record).await;
    author.write(fresh_record.clone(), &second).unwrap();
    author.write(fresh_record.clone(), &third).unwrap();
    crate::support::durable(&author).await;
    until(
        "read both changes that landed together",
        &mut reader,
        &page,
        |_, page| Reader::holds(page, &fresh_record) == ["first", "second", "third"],
    )
    .await;

    assert!(
        reader.refusals.is_empty(),
        "every landing the page was told of was a record it holds; these reads were refused: {:?}",
        reader.refusals
    );
    page.close();
    author.close();
}
