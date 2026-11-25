//! A person removes one of their devices. What that device wrote before it was removed is the
//! workspace's history, and stays readable for everyone: a device paired afterwards, a device
//! and a member that were away while it happened, and whoever joins once the person's server
//! has restarted, also for a note the removed device alone wrote.
//!
//! The workspace here is a person's own, kept by their server: a laptop and a phone write into
//! its Chat and into notes, each on what the other wrote, and the phone is removed the way
//! Settings removes it (the root signs the removal, a session of the person announces it).

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::too_many_lines,
    clippy::significant_drop_tightening,
    clippy::print_stderr,
    reason = "in a test these are the assertion, not a hazard; a scenario is one story, in order; the server lives until it ends"
)]

use std::collections::BTreeSet;
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

use automerge::{ROOT, transaction::Transactable as _};
use collab_types::DocumentMutation;
use ed25519_dalek::SigningKey;
use tilo_peer_state::StatePeer;
use tilo_peer_state::document_frame::DocumentRoute;
use tilo_session_client::{Cmd, Link, MemoryPeerStore, Outbox as _};
use tilo_user_identity::{DeviceCapabilities, RevocationEntry, RevocationReason, UserIdentity};

use crate::support::{
    Device, TestHost as Server, WORKSPACE, commit_change, durable, enrol, entry_grant,
    in_workspace, ready, redeem, refused, resource_of, said, start_host_admitting as start_server,
    start_peer_with_store, wait_for, wait_keyed, workspace,
};

fn note() -> DocumentRoute {
    crate::support::route()
}

/// A note the phone founds.
fn second_note() -> DocumentRoute {
    DocumentRoute {
        document: "note-2".into(),
        ..note()
    }
}

/// The workspace's Chat.
fn chat() -> DocumentRoute {
    DocumentRoute {
        app_id: "so.tilo.chat".into(),
        cluster_id: WORKSPACE.into(),
        document: String::new(),
    }
}

/// The part of the chain Chat keeps its messages in.
fn chat_messages() -> DocumentRoute {
    DocumentRoute {
        document: neighbour_wire::document_part("messages", 0),
        ..chat()
    }
}

fn shared() -> [DocumentRoute; 3] {
    [note(), chat(), chat_messages()]
}

fn fresh() -> Arc<MemoryPeerStore> {
    Arc::new(MemoryPeerStore::default())
}

/// Start `device` as `root`'s server on what `state` keeps, letting the people `guests`
/// connect.
async fn serve(root: &SigningKey, device: &Device, state: &Path, guests: &[[u8; 32]]) -> Server {
    start_server(
        root,
        device,
        state,
        "127.0.0.1:0".parse().unwrap(),
        None,
        guests,
    )
    .await
}

/// A person's server with their workspace, and what was said in each of its resources so far.
struct World {
    root: SigningKey,
    server_device: Device,
    state: tempfile::TempDir,
    server: Option<Server>,
    guests: Vec<[u8; 32]>,
    written: Vec<(DocumentRoute, BTreeSet<String>)>,
}

impl World {
    /// Ada's server, which lets the people `guests` connect.
    async fn start(guests: &[[u8; 32]]) -> Self {
        let (root, _) = UserIdentity::generate("Ada", 1);
        let server_device = enrol(&root, "home-server", DeviceCapabilities::member());
        let state = crate::support::scratch_dir();
        let server = serve(&root, &server_device, state.path(), guests).await;
        Self {
            root,
            server_device,
            state,
            server: Some(server),
            guests: guests.to_vec(),
            written: Vec::new(),
        }
    }

    fn server(&self) -> &Server {
        self.server.as_ref().expect("the server is up")
    }

    /// One more device of Ada's, as her root certifies it when it is paired.
    fn device(&self, label: &str) -> Device {
        enrol(&self.root, label, DeviceCapabilities::member())
    }

    /// `device`'s session in the workspace, over what it keeps in `store`.
    fn session(&self, device: &Device, store: Arc<MemoryPeerStore>) -> Link {
        let peer = start_peer_with_store(
            self.server().iroh(),
            device.cert.clone(),
            self.server_device.cert.clone(),
            device.key.clone(),
            store,
        );
        peer.join_placed(WORKSPACE, "Workspace").unwrap();
        peer
    }

    /// Say `text` in `route` from `peer`'s device, on what its replica holds there.
    async fn write(&mut self, peer: &Link, route: &DocumentRoute, text: &str) {
        let mut copy = resource_of(peer, route).unwrap_or_default();
        let before = copy.get_heads();
        copy.put(ROOT, text, true).unwrap();
        commit_change(&mut copy);
        let mutation = if before.is_empty() {
            DocumentMutation::Snapshot(copy.save())
        } else {
            DocumentMutation::Incremental(copy.save_after(&before))
        };
        peer.send(Cmd::OpenRoute(route.clone())).unwrap();
        wait_keyed(peer, route).await;
        peer.write(route.clone(), &mutation).unwrap();
        match self.written.iter_mut().find(|(known, _)| known == route) {
            Some((_, said)) => {
                said.insert(text.to_owned());
            }
            None => self
                .written
                .push((route.clone(), BTreeSet::from([text.to_owned()]))),
        }
    }

    /// [`Self::write`], kept by the server before it returns.
    async fn post(&mut self, peer: &Link, route: &DocumentRoute, text: &str) {
        self.write(peer, route, text).await;
        durable(peer).await;
    }

    /// Wait until `peer`'s replica says everything said in the workspace so far; when it does
    /// not within the hang bound, what it lacks and what its session says of itself.
    async fn reads(&self, peer: &Link) -> Result<(), String> {
        for (route, _) in &self.written {
            peer.send(Cmd::OpenRoute(route.clone())).unwrap();
        }
        let deadline = tilo_test_env::hang_deadline();
        loop {
            let lacking = self
                .written
                .iter()
                .filter_map(|(route, texts)| {
                    let said = said(peer, route);
                    let lacks = texts.difference(&said).collect::<Vec<_>>();
                    (!lacks.is_empty()).then(|| {
                        format!(
                            "{}/{:?} lacks {} of {}: {lacks:?}",
                            route.app_id,
                            route.document,
                            lacks.len(),
                            texts.len()
                        )
                    })
                })
                .collect::<Vec<_>>();
            if lacking.is_empty() {
                return Ok(());
            }
            if Instant::now() >= deadline {
                let status = peer.status();
                return Err(format!(
                    "{}\nits session says:\n{}\nfailures: {:?}",
                    lacking.join("\n"),
                    status.key_state,
                    status.failures
                ));
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }

    /// [`Self::reads`], for a device nothing was taken from: it must.
    async fn hears(&self, peer: &Link) {
        if let Err(unread) = self.reads(peer).await {
            panic!("a device that is still the person's does not read the workspace: {unread}");
        }
    }

    /// The workspace as Ada's server holds it.
    async fn held(&self) -> Option<StatePeer> {
        self.server()
            .server
            .workspaces()
            .unwrap()
            .state(WORKSPACE)
            .await
    }

    /// Remove `device` as Settings does: Ada's root signs the removal as of now, and `by`, a
    /// session of hers, announces it. Returns once her server has taken it into the workspace.
    async fn remove(
        &self,
        by: &Link,
        device: &Device,
        reason: RevocationReason,
    ) -> RevocationEntry {
        let entry = tilo_server_client::revoke_device(
            &self.root,
            device.cert.device_public_key,
            tilo_clock::unix_now_ns(),
            reason,
        )
        .await
        .unwrap();
        by.send(Cmd::Revoke(entry.clone())).unwrap();
        wait_for("the server to take the removal in", || async {
            self.held()
                .await?
                .revocations()
                .into_iter()
                .find(|held| held == &entry)
        })
        .await;
        entry
    }

    /// What the server holds that does not read whole, with why.
    async fn unread(&self) -> Vec<((String, String), String)> {
        self.server()
            .server
            .workspaces()
            .unwrap()
            .whole_documents(WORKSPACE, None)
            .await
            .unwrap()
            .unread
    }

    async fn stop(&mut self) {
        if let Some(server) = self.server.take() {
            drop(server.web);
            server.server.shutdown().await;
        }
    }

    /// Stop the server and start it again on what it kept.
    async fn restart(&mut self) {
        self.stop().await;
        tokio::time::sleep(Duration::from_millis(200)).await;
        self.server = Some(
            serve(
                &self.root,
                &self.server_device,
                self.state.path(),
                &self.guests,
            )
            .await,
        );
    }
}

/// Ada's laptop and phone in her workspace, both having written into Chat and into notes, each
/// on what the other wrote; the phone founded one of the notes.
async fn laptop_and_phone(world: &mut World) -> ((Device, Link), (Device, Link)) {
    let (laptop, phone) = (world.device("laptop"), world.device("phone"));
    let a = world.session(&laptop, fresh());
    let b = world.session(&phone, fresh());
    ready(&[&a, &b]).await;
    for route in shared() {
        world.post(&a, &route, "the laptop founded it").await;
        world.hears(&b).await;
        world
            .post(&b, &route, "the phone wrote before it was removed")
            .await;
        world.hears(&a).await;
        world
            .post(&a, &route, "the laptop built on what the phone wrote")
            .await;
    }
    world.post(&b, &second_note(), "the phone founded it").await;
    world.hears(&a).await;
    world
        .post(
            &a,
            &second_note(),
            "the laptop built on what the phone founded",
        )
        .await;
    world.hears(&b).await;
    ((laptop, a), (phone, b))
}

/// What a removed phone writes into the note after its removal.
const FROM_THE_REMOVED: &str = "the phone wrote after it was removed";

/// `peer`'s device, removed, says `text` in `route` all the same: whether its session still
/// takes the write or not, nobody else may.
fn attempt(peer: &Link, route: &DocumentRoute, text: &str) {
    let mut copy = resource_of(peer, route).unwrap_or_default();
    let before = copy.get_heads();
    copy.put(ROOT, text, true).unwrap();
    commit_change(&mut copy);
    let _ = peer.write(
        route.clone(),
        &DocumentMutation::Incremental(copy.save_after(&before)),
    );
}

/// The laptop writes into everything once more, after the removal.
async fn laptop_writes_on(world: &mut World, a: &Link) {
    for route in shared().into_iter().chain([second_note()]) {
        world
            .post(a, &route, "the laptop wrote after the removal")
            .await;
    }
}

/// The phone is removed as `reason`, and a tablet is paired afterwards: what it does not read
/// of the workspace, or what it refused on the way.
async fn paired_after(reason: RevocationReason) -> Result<(), String> {
    let mut world = World::start(&[]).await;
    let ((_laptop, a), (phone, b)) = laptop_and_phone(&mut world).await;
    world.remove(&a, &phone, reason).await;
    // The phone, removed, writes on: nothing of it reaches anyone.
    attempt(&b, &note(), FROM_THE_REMOVED);
    laptop_writes_on(&mut world, &a).await;

    let tablet = world.device("tablet");
    let c = world.session(&tablet, fresh());
    ready(&[&c]).await;
    let mut read = world
        .reads(&c)
        .await
        .map_err(|unread| format!("removed as {reason:?}, a device paired afterwards: {unread}"));
    if read.is_ok()
        && let Some(line) = refused(&c)
    {
        read = Err(format!(
            "removed as {reason:?}, a device paired afterwards reads everything and says: {line}"
        ));
    }
    if read.is_ok() {
        // It is a device like any other: what it writes reaches the laptop.
        world
            .post(&c, &note(), "the tablet wrote, paired after the removal")
            .await;
        world.hears(&a).await;
        for (who, peer) in [("the laptop", &a), ("the tablet", &c)] {
            if said(peer, &note()).contains(FROM_THE_REMOVED) {
                read = Err(format!(
                    "removed as {reason:?}, {who} took what the phone wrote after its removal"
                ));
            }
        }
    }
    for peer in [&a, &b, &c] {
        peer.close();
    }
    world.stop().await;
    read
}

#[tokio::test(flavor = "multi_thread", worker_threads = 8)]
async fn a_device_paired_after_a_removal_reads_what_the_removed_device_wrote() {
    let (retired, lost, stolen, compromised) = tokio::join!(
        paired_after(RevocationReason::Retired),
        paired_after(RevocationReason::Lost),
        paired_after(RevocationReason::Stolen),
        paired_after(RevocationReason::Compromised),
    );
    let unread = [retired, lost, stolen, compromised]
        .into_iter()
        .filter_map(Result::err)
        .collect::<Vec<_>>();
    assert!(unread.is_empty(), "{}", unread.join("\n\n"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_device_that_was_away_during_a_removal_reads_what_the_removed_device_wrote() {
    let mut world = World::start(&[]).await;
    let ((_laptop, a), (phone, b)) = laptop_and_phone(&mut world).await;
    let desktop = world.device("desktop");
    let kept = fresh();
    let d = world.session(&desktop, kept.clone());
    ready(&[&d]).await;
    world.hears(&d).await;
    d.close();

    // While the desktop is away the phone writes on, and is then removed.
    for route in shared() {
        world
            .post(&b, &route, "the phone wrote while the desktop was away")
            .await;
    }
    world.hears(&a).await;
    world.remove(&a, &phone, RevocationReason::Retired).await;
    laptop_writes_on(&mut world, &a).await;

    let d = world.session(&desktop, kept);
    ready(&[&d]).await;
    let read = world.reads(&d).await;
    assert!(
        read.is_ok(),
        "a device that was away during the removal: {}",
        read.unwrap_err()
    );
    assert_eq!(refused(&d), None);
    for peer in [&a, &b, &d] {
        peer.close();
    }
    world.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_member_who_was_away_during_a_removal_reads_what_the_removed_device_wrote() {
    let (bo, _) = UserIdentity::generate("Bo", 1);
    let bo_person = bo.verifying_key().to_bytes();
    let mut world = World::start(&[bo_person]).await;
    let ((_laptop, a), (phone, b)) = laptop_and_phone(&mut world).await;

    // Bo is a second member of Ada's workspace, with a device of their own.
    let guest = enrol(&bo, "bo-laptop", DeviceCapabilities::member());
    let entry = entry_grant(
        world.server(),
        &world.root,
        &world.server_device,
        WORKSPACE,
        bo_person,
    )
    .await;
    redeem(world.server(), entry, &guest.cert, &guest.key).await;
    let kept = fresh();
    let arrive = |world: &World, store| {
        let peer = start_peer_with_store(
            world.server().iroh(),
            guest.cert.clone(),
            world.server_device.cert.clone(),
            guest.key.clone(),
            store,
        );
        peer.send(Cmd::Join(workspace())).unwrap();
        peer
    };
    let g = arrive(&world, kept.clone());
    in_workspace(&g, workspace()).await;
    world.hears(&g).await;
    g.close();

    for route in shared() {
        world
            .post(&b, &route, "the phone wrote while Bo was away")
            .await;
    }
    world.hears(&a).await;
    world.remove(&a, &phone, RevocationReason::Retired).await;
    laptop_writes_on(&mut world, &a).await;

    let g = arrive(&world, kept);
    in_workspace(&g, workspace()).await;
    let read = world.reads(&g).await;
    assert!(
        read.is_ok(),
        "a member who was away during the removal: {}",
        read.unwrap_err()
    );
    assert_eq!(refused(&g), None);
    for peer in [&a, &b, &g] {
        peer.close();
    }
    world.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_server_restarted_after_a_removal_serves_what_the_removed_device_wrote() {
    let mut world = World::start(&[]).await;
    let ((laptop, a), (phone, b)) = laptop_and_phone(&mut world).await;
    world.remove(&a, &phone, RevocationReason::Retired).await;
    laptop_writes_on(&mut world, &a).await;
    let unread_before = world.unread().await;
    for peer in [&a, &b] {
        peer.close();
    }

    world.restart().await;
    let unread_after = world.unread().await;
    eprintln!(
        "what the server does not read whole before its restart: {unread_before:?}; after: {unread_after:?}"
    );
    assert!(
        unread_after
            .iter()
            .all(|lacking| unread_before.contains(lacking)),
        "the restarted server no longer reads what it read before: {unread_after:?}"
    );

    // The laptop comes back with nothing kept, and a tablet is paired.
    let a = world.session(&laptop, fresh());
    let tablet = world.device("tablet");
    let c = world.session(&tablet, fresh());
    ready(&[&a, &c]).await;
    for (who, peer) in [("the laptop", &a), ("a device paired afterwards", &c)] {
        let read = world.reads(peer).await;
        assert!(
            read.is_ok(),
            "after the server restarted, {who}: {}",
            read.unwrap_err()
        );
        assert_eq!(refused(peer), None, "{who}");
    }
    for peer in [&a, &c] {
        peer.close();
    }
    world.stop().await;
}

/// A note the removed phone founded and alone wrote, in more writes than a server keeps before it
/// folds them. A person's own server holds the workspace's keys and signs the checkpoints of
/// what it keeps itself, so whichever way the note reaches a device paired afterwards, as the
/// phone's own writes or under the server's checkpoint of them, it reads whole. A checkpoint the
/// removed device signed itself is the replica's own case
/// (`removing_a_device_takes_its_future_and_keeps_its_persons_past` in `tilo-peer-state`).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_note_only_the_removed_device_wrote_reads_whole_for_a_device_paired_afterwards() {
    /// More writes than the server keeps of a note before it folds them into a checkpoint.
    const WRITES: usize = 48;
    let mut world = World::start(&[]).await;
    let phone = world.device("phone");
    let b = world.session(&phone, fresh());
    ready(&[&b]).await;

    // Alone in the workspace, the phone writes a note's whole history.
    world.post(&b, &note(), "the phone founded it").await;
    for n in 0..WRITES {
        world
            .write(&b, &note(), &format!("the phone wrote {n}"))
            .await;
    }
    durable(&b).await;

    let laptop = world.device("laptop");
    let a = world.session(&laptop, fresh());
    ready(&[&a]).await;
    world.hears(&a).await;
    world
        .post(&a, &note(), "the laptop built on what the phone wrote")
        .await;
    world.remove(&a, &phone, RevocationReason::Retired).await;
    world
        .post(&a, &note(), "the laptop wrote after the removal")
        .await;

    let tablet = world.device("tablet");
    let c = world.session(&tablet, fresh());
    ready(&[&c]).await;
    let read = world.reads(&c).await;
    assert!(
        read.is_ok(),
        "a device paired after the removal of the device that alone wrote the note: {}",
        read.unwrap_err()
    );
    assert_eq!(refused(&c), None);
    for peer in [&a, &b, &c] {
        peer.close();
    }
    world.stop().await;
}
