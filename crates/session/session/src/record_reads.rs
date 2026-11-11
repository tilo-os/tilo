//! The records this device reads outside their apps, as the broker's `NativeDocumentReads`: an
//! app's Open of a record the workspace holds, and an agent's task reading its workspace's records.
//!
//! The read is the engine's ([`Engine::read_record`]), the one path the desk, search and a branch
//! also read a record with, for a workspace that commits its records and one that seals them. A
//! record this device holds nothing of is the app's own channel: an Open only waits for what the
//! workspace's host sends of it — its replay, or the record itself where the host sends records
//! once they are opened ([`NativeDocumentReads::await_arriving_record`]).
//!
//! A chain's head is the one route with no document behind it: it names whichever of the chain's
//! parts is the newest, so opening it asks the host for that part, which arrives on the part's own
//! route, and is answered by which part it sent or by none at all
//! ([`NativeDocumentReads::chain_head_answer`]).

use std::sync::Arc;

use tilo_action_broker::route::CrdtRoute;
use tilo_action_broker::services::{
    AwaitingRecord, ChainHeadAnswer, DocumentBytes, NativeDocumentRead, NativeDocumentReadContext,
    NativeDocumentReads, RecordRead,
};
use tilo_broker_core::custody::OwnerCustody as _;
use tilo_desktop_model::WorkspaceId;
use ui::record::RecordId;

use crate::engine::{Engine, RecordReading};

/// This device's workspaces, as the record read outside an app is answered from.
pub struct Records {
    engine: Arc<Engine>,
}

impl Records {
    #[must_use]
    pub fn new(engine: Arc<Engine>) -> Self {
        Self { engine }
    }

    /// Ask the hosts of `route`'s workspace for its record, where the workspace sends records only
    /// once a device opens them: what they hold of it, and their changes to it from then on. A host
    /// sends nothing of a record before it is asked, so a device that never asks never hears of it
    /// again. The shared desktop is replayed to every device and is never asked for, and a device
    /// that hosts the workspace itself holds everything the workspace retains.
    fn ask_for(&self, route: &CrdtRoute) {
        if route.app_id == tilo_desktop_model::ownership::DESKTOP_APP
            && route.record_id == tilo_desktop_model::ownership::DESKTOP_RECORD
        {
            return;
        }
        if !self.engine.records_when_opened(&route.cluster_id) {
            return;
        }
        self.engine
            .open_record(&route.cluster_id, &route.app_id, &route.record_id);
    }
}

impl NativeDocumentReads for Records {
    /// A workspace this device's owner commits answers every read; one whose records are its
    /// members' sealed channels answers the records it holds.
    fn requires_workspace(&self, workspace: &str) -> bool {
        WorkspaceId::parse(workspace)
            .is_some_and(|workspace| self.engine.commits_documents(workspace))
    }

    fn seals_records(&self, workspace: &str) -> bool {
        WorkspaceId::parse(workspace).is_some_and(|workspace| {
            self.engine.holds(workspace) && !self.engine.commits_documents(workspace)
        })
    }

    fn record_read(&self, workspace: &str, app: &str, record: &str) -> RecordRead {
        let Some(id) = WorkspaceId::parse(workspace) else {
            return RecordRead::Own;
        };
        if !self.engine.holds_record(workspace, app, record) {
            return RecordRead::Own;
        }
        if self.engine.commits_documents(id) {
            RecordRead::Committed
        } else {
            RecordRead::Sealed
        }
    }

    /// A record this device holds nothing of may have been written on another device, or before
    /// this session. In a workspace whose records are sealed channels the workspace's host
    /// retains it and replays it with the rest of the workspace's history as the link starts; in
    /// one whose host sends each record once a device opens it, it is asked for now
    /// ([`Self::ask_for`]) and arrives after. Either way the Open decides the record is new only
    /// after everything the host sends of it came — deciding earlier would have the app make a
    /// second copy of the workspace's own. A chain's head never arrives itself: the part the host
    /// answers with arrives on the part's own route, so any word about it settles the wait
    /// ([`Self::chain_head_answer`]).
    fn await_arriving_record(&self, context: &NativeDocumentReadContext) -> Option<AwaitingRecord> {
        let route = context.route();
        // A workspace this device hosts itself keeps everything it retains here: nothing is
        // replayed to it and no other device sends its records, so a record it holds nothing of is
        // new.
        if !self.engine.reaches_host()
            || self.engine.hosts_itself(&route.cluster_id)
            || self
                .engine
                .holds_record(&route.cluster_id, &route.app_id, &route.record_id)
        {
            return None;
        }
        let when_opened = self.engine.records_when_opened(&route.cluster_id);
        if !when_opened && !self.seals_records(&route.cluster_id) {
            // Records of this workspace reach this device as they are written, and this one is
            // new: the workspace has none of it.
            return None;
        }
        if when_opened {
            self.ask_for(route);
        }
        // A chain's head names the chain's newest part and holds nothing of its own: it never arrives
        // here, and the part comes on its own route. Any word from the host settles it — which part
        // it sent, or none — and `chain_head_answer` says which.
        let head = RecordId::parse(&route.record_id).is_some_and(|id| id.is_chain_head());
        let engine = Arc::clone(&self.engine);
        let (workspace, app, record) = (
            route.cluster_id.clone(),
            route.app_id.clone(),
            route.record_id.clone(),
        );
        // Subscribed before the first check, so a word that came while the wait was being built is
        // read rather than slept through.
        let mut landed = self.engine.arrivals().watching();
        let mut replays = self.engine.replays()?;
        let mut answers = self.engine.record_answers()?;
        Some(Box::pin(async move {
            loop {
                // The record is here: it reads.
                if engine.holds_record(&workspace, &app, &record) {
                    return;
                }
                if when_opened {
                    // A host sends everything it holds of a record it was asked for, and then says
                    // how much that was. None of it means the workspace has none of the record,
                    // which is new here; the record is on its way until then.
                    match engine.record_answer(&workspace, &app, &record) {
                        Some(_) if head => return,
                        Some(0) => return,
                        _ => {}
                    }
                } else if engine.host_replayed(&workspace) {
                    // A workspace whose host replays everything says when its replay ended: a
                    // record still not here then is one the workspace has none of.
                    return;
                }
                let changed = if when_opened {
                    tokio::select! {
                        changed = landed.changed() => changed,
                        changed = answers.changed() => changed,
                    }
                } else {
                    tokio::select! {
                        changed = landed.changed() => changed,
                        changed = replays.changed() => changed,
                    }
                };
                if changed.is_err() {
                    // The session's own events are gone: decide on what this device holds.
                    return;
                }
            }
        }))
    }

    /// A chain's head names whichever part of the chain is its newest and holds nothing of its own,
    /// so what the workspace's hosts answered is which part they sent — it arrives on the part's own
    /// route — or that it has no part at all, and the chain starts its first. A workspace that
    /// replays everything answered with its replay: what arrived is all of the chain there is here.
    fn chain_head_answer(&self, context: &NativeDocumentReadContext) -> Option<ChainHeadAnswer> {
        let route = context.route();
        RecordId::parse(&route.record_id).filter(RecordId::is_chain_head)?;
        let answered =
            self.engine
                .record_answer(&route.cluster_id, &route.app_id, &route.record_id);
        let no_part = match answered {
            Some(frames) => frames == 0,
            // Nobody owed an answer this device asked for: this workspace replays every record, its
            // replay is that answer, and this device hosts it or reaches no host at all.
            None => !self.engine.records_when_opened(&route.cluster_id),
        };
        Some(if no_part {
            ChainHeadAnswer::NoPart
        } else {
            ChainHeadAnswer::NewestPartOnItsWay
        })
    }

    fn authorize(&self, context: &NativeDocumentReadContext) -> Result<(), String> {
        context.revalidate()?;
        let route = context.route();
        self.engine.authorizes_read(
            &route.cluster_id,
            context.certificate(),
            &route.app_id,
            &route.record_id,
        )?;
        context.revalidate()
    }

    fn capture(
        &self,
        context: &NativeDocumentReadContext,
        known_revision: Option<[u8; 32]>,
        maximum: usize,
    ) -> Result<NativeDocumentRead, String> {
        let route = context.route();
        // A workspace whose host sends records once they are opened follows only the records its
        // devices asked for: an Open is where this device asks, whether it holds a copy of the
        // record already or not, and its changes reach it from then on.
        self.ask_for(route);
        let read = self.engine.read_record(
            &route.cluster_id,
            &route.app_id,
            &route.record_id,
            known_revision,
            maximum,
        )?;
        Ok(match read {
            RecordReading::Captured {
                revision,
                heads,
                bytes,
            } => NativeDocumentRead::Captured {
                revision,
                heads,
                body: Box::new(Captured(bytes)),
            },
            RecordReading::Unchanged(revision) => NativeDocumentRead::Unchanged(revision),
            RecordReading::Pending => NativeDocumentRead::Pending,
        })
    }
}

/// The bytes of one bounded capture, held until the broker has sent them.
struct Captured(Vec<u8>);

impl DocumentBytes for Captured {
    fn bytes(&self) -> &[u8] {
        &self.0
    }
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    #![allow(
        clippy::unwrap_used,
        clippy::expect_used,
        reason = "a test: a failed step is the assertion"
    )]

    tilo_automerge_isolate::test_worker!();

    use ed25519_dalek::SigningKey;
    use futures_util::FutureExt as _;
    use tilo_action_broker::route::CrdtRoute;
    use tilo_action_broker::services::{
        NativeDocumentRead, NativeDocumentReadContext, NativeDocumentReads as _,
    };
    use tilo_desktop_model::{WorkspaceId, WorkspaceSlot};
    use tilo_peer_state::owner::testing::{self, governor::TestGovernor};
    use tilo_session_client::Cmd;
    use tilo_session_client::plane::topic_of;
    use tilo_user_identity::DeviceCert;

    use super::*;
    use crate::engine::Engine;

    const APP: &str = "so.tilo.notes";
    /// A record a host sends only once asked for it.
    const ON_DEMAND: &str = "derived:on-demand@history";
    const RECORD: &str = "note-1";
    /// What one read may hand out: more than these tests' records hold.
    const MAXIMUM: usize = 1024 * 1024;
    /// A workspace another device hosts.
    const JOINED: &str = "bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb";

    /// This device's engine under `state`, with its certificate.
    fn device(state: &std::path::Path, seed: u8) -> (Arc<Engine>, DeviceCert, SigningKey) {
        let root = SigningKey::from_bytes(&[0x40; 32]);
        let key = SigningKey::from_bytes(&[seed; 32]);
        let cert = testing::member_certificate(&root, &key, "Laptop");
        let engine = Arc::new(
            Engine::open(
                tilo_identity_host::custody::HostIdentity::kept(cert.clone(), key.clone()),
                Some(state),
                std::iter::empty(),
                crate::platform::custody::of_this_desk(),
            )
            .expect("this device's engine opens"),
        );
        (engine, cert, key)
    }

    /// A workspace this device founded, holding one record of `APP` written under `key`.
    fn workspace_with_record(engine: &Engine, key: &SigningKey) -> WorkspaceId {
        let workspace = WorkspaceId::new();
        engine.found(WorkspaceSlot(1), workspace, "Desk", None);
        assert!(engine.holds(workspace), "the desk's workspace is held here");
        testing::start_record(engine.owner(), workspace, APP, RECORD, key, testing::now());
        workspace
    }

    /// A read of `record` of `APP` in `workspace` by `certificate`'s device.
    fn context(
        workspace: WorkspaceId,
        record: &str,
        certificate: DeviceCert,
    ) -> NativeDocumentReadContext {
        NativeDocumentReadContext::for_reader(
            CrdtRoute::record(APP, workspace.to_string(), record),
            certificate,
        )
    }

    /// A record this device holds nothing of is the app's own channel, in a workspace it holds and
    /// one it does not. A session with no collaboration plane reaches no host, so it decides on
    /// what it holds.
    #[test]
    fn a_record_this_device_holds_nothing_of_is_the_apps_own_channel() {
        let state = tempfile::tempdir().expect("a state directory");
        let (engine, cert, key) = device(state.path(), 0x51);
        let records = Records::new(Arc::clone(&engine));
        let elsewhere = WorkspaceId::new().to_string();
        assert_eq!(
            records.record_read(&elsewhere, APP, RECORD),
            RecordRead::Own
        );
        assert!(!records.seals_records(&elsewhere));
        assert!(!records.requires_workspace(&elsewhere));
        let elsewhere_read = NativeDocumentReadContext::for_reader(
            CrdtRoute::record(APP, elsewhere, RECORD),
            cert.clone(),
        );
        assert!(
            records.authorize(&elsewhere_read).is_err(),
            "a read of a workspace this device does not hold is refused"
        );

        // A workspace this device holds, of a record it holds nothing of: the app's channel too.
        let workspace = workspace_with_record(&engine, &key);
        assert_eq!(
            records.record_read(&workspace.to_string(), APP, "another-note"),
            RecordRead::Own
        );
        assert!(!engine.reaches_host());
        assert!(
            records
                .await_arriving_record(&context(workspace, "another-note", cert))
                .is_none(),
            "nothing is waited for where no host is reached"
        );
    }

    /// A record the workspace holds reads from this device's own copy; a device the workspace does
    /// not certify reads nothing.
    #[test]
    fn a_record_the_workspace_holds_reads_from_this_devices_copy() {
        let state = tempfile::tempdir().expect("a state directory");
        let (engine, cert, key) = device(state.path(), 0x52);
        let workspace = workspace_with_record(&engine, &key);
        let records = Records::new(Arc::clone(&engine));
        // A workspace whose host takes part in its channels seals its records.
        assert!(records.seals_records(&workspace.to_string()));
        assert!(!records.requires_workspace(&workspace.to_string()));
        assert_eq!(
            records.record_read(&workspace.to_string(), APP, RECORD),
            RecordRead::Sealed
        );
        assert!(engine.holds_record(&workspace.to_string(), APP, RECORD));

        let reading = context(workspace, RECORD, cert);
        records.authorize(&reading).expect("this device reads it");
        let NativeDocumentRead::Captured {
            revision,
            heads,
            body,
        } = records
            .capture(&reading, None, MAXIMUM)
            .expect("the record reads")
        else {
            panic!("the workspace holds the record whole");
        };
        // The record was founded empty: no heads yet, and its document still reads back.
        assert!(heads.is_empty(), "an empty record has no heads");
        assert!(!body.bytes().is_empty(), "the record reads back");
        assert!(
            matches!(
                records
                    .capture(&reading, Some(revision), MAXIMUM)
                    .expect("the record reads"),
                NativeDocumentRead::Unchanged(stood) if stood == revision
            ),
            "a record that did not change since the caller's revision is answered as it"
        );

        let stranger = testing::member_certificate(
            &SigningKey::from_bytes(&[0x41; 32]),
            &SigningKey::from_bytes(&[0x53; 32]),
            "Someone else's laptop",
        );
        assert!(
            records
                .authorize(&context(workspace, RECORD, stranger))
                .is_err(),
            "a device the workspace does not certify reads nothing"
        );
    }

    /// A plane this engine asks its workspaces' hosts on, the commands it says kept.
    fn planed(
        engine: &Engine,
    ) -> (
        Arc<tilo_session_client::channels::ChannelLink>,
        tokio::sync::mpsc::UnboundedReceiver<tilo_session_client::Cmd>,
    ) {
        let link = tilo_session_client::channels::ChannelLink::new();
        let (said, heard) = tokio::sync::mpsc::unbounded_channel();
        link.attach(Arc::new(said));
        engine.attach_link(Arc::clone(&link));
        (link, heard)
    }

    /// A workspace this device hosts itself keeps everything it retains here: an app's Open of a
    /// record it holds nothing of is answered as new at once, with a host reached, never held for a
    /// replay no device sends (a new Terminal session's record waited for one for good).
    #[test]
    fn an_open_in_a_workspace_this_device_hosts_waits_for_nobody() {
        let state = tempfile::tempdir().expect("a state directory");
        let (engine, cert, key) = device(state.path(), 0x54);
        let workspace = workspace_with_record(&engine, &key);
        let (_link, mut said) = planed(&engine);
        let records = Records::new(Arc::clone(&engine));
        assert!(engine.reaches_host(), "a plane is running");
        assert!(
            engine.hosts_itself(&workspace.to_string()),
            "it was founded here"
        );
        assert!(
            records
                .await_arriving_record(&context(workspace, "terminal-session", cert))
                .is_none(),
            "nothing is waited for"
        );
        assert!(said.try_recv().is_err(), "and nobody is asked");
    }

    /// Every record comes with the workspace's history, asked for or not: a read of a record this
    /// device holds nothing of — a part of Chat's messages a send opens — is never left pending on
    /// asking the host for that record alone, which no host answers on the workspace's link (a
    /// send waited on it for good), and a waiting Open wakes when the host's replay ends.
    #[test]
    fn a_record_not_held_is_asked_of_nobody_and_the_replay_answers_for_it() {
        const PART: &str = "derived:part.0@derived:messages@derived:default";
        let state = tempfile::tempdir().expect("a state directory");
        let (engine, _cert, key) = device(state.path(), 0x55);
        let workspace = workspace_with_record(&engine, &key);
        let (link, mut said) = planed(&engine);
        let id = workspace.to_string();
        let replays = engine.replays().expect("a plane is running");
        for record in [ON_DEMAND, PART] {
            assert!(!engine.holds_record(&id, APP, record));
            assert!(
                !matches!(
                    engine.read_record(&id, APP, record, None, 1 << 20),
                    Ok(RecordReading::Pending)
                ),
                "a read of {record} is not left waiting"
            );
        }
        assert!(said.try_recv().is_err(), "nobody is asked for a record");
        assert!(!engine.host_replayed(&id));
        link.host_ready(tilo_session_client::plane::topic_of(workspace), false);
        assert!(
            replays.has_changed().expect("the plane's word"),
            "a waiting Open wakes"
        );
        assert!(engine.host_replayed(&id), "the replay ended");
    }

    /// A workspace this device is a member of and another device hosts, and that host's own engine:
    /// this device holds the workspace, and the records it holds nothing of are the host's to send.
    fn joined(engine: &Engine, cert: &DeviceCert, key: &SigningKey) -> (WorkspaceId, Host) {
        let root = SigningKey::from_bytes(&[0x42; 32]);
        let host_key = SigningKey::from_bytes(&[0x43; 32]);
        let host_cert = testing::certificate(&root, &host_key, "The workspace's host");
        let state = tempfile::TempDir::new().expect("the host's state directory");
        let host = Arc::new(
            Engine::open(
                tilo_identity_host::custody::HostIdentity::kept(
                    host_cert.clone(),
                    host_key.clone(),
                ),
                Some(state.path()),
                std::iter::empty(),
                crate::platform::custody::of_this_desk(),
            )
            .expect("the host's engine opens"),
        );
        let governor = TestGovernor::empty_interactive(host_cert.clone(), host_key.clone(), JOINED)
            .expect("another device governs the workspace");
        let governor = testing::enroll_into(governor, host.owner(), &host_key, &host_cert)
            .expect("the host is in its own workspace");
        testing::enroll_into(governor, engine.owner(), key, cert)
            .expect("the workspace admits this device");
        let host = Host {
            engine: host,
            key: host_key,
            _state: state,
        };
        (WorkspaceId::parse(JOINED).expect("a workspace"), host)
    }

    /// The device that hosts a workspace this device joined.
    struct Host {
        engine: Arc<Engine>,
        key: SigningKey,
        _state: tempfile::TempDir,
    }

    impl Host {
        /// `app`'s `record` of `workspace` as it reaches this device: what the host sends a device
        /// that opened a record it retains.
        fn writes(
            &self,
            workspace: WorkspaceId,
            app: &str,
            record: &str,
        ) -> neighbour_wire::NeighbourMsg {
            self.engine
                .owner()
                .with_current(workspace, |view| {
                    view.peer().clone().publication_mut().publish_document(
                        app,
                        record,
                        automerge::AutoCommit::new().save(),
                        &self.key,
                        testing::now(),
                    )
                })
                .expect("the host writes its record")
        }
    }

    /// A workspace whose host sends each record once a device opens it: opening a record this
    /// device holds nothing of asks that host for it, and the record is on its way meanwhile. The
    /// host's answer that it holds none of it is what makes the record new here — deciding before
    /// that answer would have the app found a second copy of the workspace's own record.
    #[test]
    fn a_record_a_host_sends_once_opened_waits_for_the_host_and_is_new_only_once_it_answers() {
        let state = tempfile::tempdir().expect("a state directory");
        let (engine, cert, key) = device(state.path(), 0x56);
        let (workspace, _host) = joined(&engine, &cert, &key);
        let (link, mut said) = planed(&engine);
        let records = Records::new(Arc::clone(&engine));
        let id = workspace.to_string();
        assert!(!engine.hosts_itself(&id), "another device hosts it");
        link.host_ready(topic_of(workspace), true);
        assert!(!engine.holds_record(&id, APP, RECORD));

        let channel = neighbour_wire::app_document_channel(APP, RECORD);
        let mut arriving = records
            .await_arriving_record(&context(workspace, RECORD, cert))
            .expect("the record may be the workspace's, which the host has not sent yet");
        assert!(
            matches!(
                said.try_recv(),
                Ok(Cmd::OpenRecord { ws, channel: asked })
                    if ws == topic_of(workspace) && asked == channel
            ),
            "the host is asked for the record"
        );
        assert!(said.try_recv().is_err(), "and asked once");
        assert!(
            arriving.as_mut().now_or_never().is_none(),
            "the open waits for the host's word rather than founding a record"
        );

        // The host sent everything it holds of the record, which is none of it: the record is new.
        link.record_opened(topic_of(workspace), channel, 0);
        assert!(
            arriving.as_mut().now_or_never().is_some(),
            "the host's word settles the open"
        );
        assert_eq!(records.record_read(&id, APP, RECORD), RecordRead::Own);
    }

    /// The host has the record this device opened: what it sent arrives, and the open reads it
    /// instead of deciding it is new.
    #[test]
    fn a_record_the_host_sends_reaches_the_open_that_asked_for_it() {
        let state = tempfile::tempdir().expect("a state directory");
        let (engine, cert, key) = device(state.path(), 0x57);
        let (workspace, host) = joined(&engine, &cert, &key);
        let (link, mut said) = planed(&engine);
        let records = Records::new(Arc::clone(&engine));
        let id = workspace.to_string();
        link.host_ready(topic_of(workspace), true);
        let channel = neighbour_wire::app_document_channel(APP, RECORD);
        let mut arriving = records
            .await_arriving_record(&context(workspace, RECORD, cert.clone()))
            .expect("the record asks for itself");
        assert!(matches!(said.try_recv(), Ok(Cmd::OpenRecord { .. })));
        assert!(
            arriving.as_mut().now_or_never().is_none(),
            "the record is not new while the host has not answered"
        );

        // The record the host opened for this device arrives, and then the host's word that it
        // sent everything it holds of it.
        let message = host.writes(workspace, APP, RECORD);
        engine
            .owner()
            .receive(
                workspace,
                tilo_peer_state::TransportOrigin::RetainedRelayDelta(
                    host.key.verifying_key().to_bytes(),
                ),
                message,
                None,
                testing::now(),
            )
            .expect("this device takes what the host sent");
        link.record_opened(topic_of(workspace), channel, 1);
        assert!(
            arriving.as_mut().now_or_never().is_some(),
            "the record arrived and the open settles"
        );
        assert!(engine.holds_record(&id, APP, RECORD), "the record is here");
        assert_ne!(
            records.record_read(&id, APP, RECORD),
            RecordRead::Own,
            "the record is the workspace's, not the app's own to found"
        );
        assert!(
            matches!(
                records
                    .capture(&context(workspace, RECORD, cert), None, MAXIMUM)
                    .expect("the record reads"),
                NativeDocumentRead::Captured { .. }
            ),
            "the open reads what the host sent"
        );
    }

    /// A chat's chain of message parts: the name of its newest part, and part 1.
    fn chain_records() -> (String, String) {
        let chain = RecordId::derived("messages", &RecordId::workspace_default());
        (
            chain.chain_head().as_str().to_owned(),
            chain.part(1).as_str().to_owned(),
        )
    }

    /// The head of a chain names whichever of the chain's parts is the newest, and holds nothing of
    /// its own: a device holding no part asks the workspace for it, and is answered with none — the
    /// chain starts its first — or with that part, which arrives on the part's own route and is the
    /// chain's from then on.
    #[test]
    fn a_chains_head_is_answered_with_its_newest_part_or_with_none() {
        let state = tempfile::tempdir().expect("a state directory");
        let (engine, cert, key) = device(state.path(), 0x58);
        let (workspace, host) = joined(&engine, &cert, &key);
        let (link, mut said) = planed(&engine);
        let records = Records::new(Arc::clone(&engine));
        let id = workspace.to_string();
        let (head_record, newest_record) = chain_records();
        let channel = neighbour_wire::app_document_channel(APP, &head_record);
        link.host_ready(topic_of(workspace), true);
        let head = context(workspace, &head_record, cert.clone());

        let mut arriving = records
            .await_arriving_record(&head)
            .expect("the head may be answered with a part");
        assert!(
            matches!(said.try_recv(), Ok(Cmd::OpenRecord { channel: asked, .. }) if asked == channel),
            "the workspace is asked for the chain's head"
        );
        assert_eq!(
            records.chain_head_answer(&head),
            Some(ChainHeadAnswer::NewestPartOnItsWay),
            "a part may still be on its way"
        );
        assert!(
            arriving.as_mut().now_or_never().is_none(),
            "the head waits for the host's word"
        );

        // The host sent the newest part, and then its word: what it holds of the head is that part,
        // which the chain follows on its own route.
        let message = host.writes(workspace, APP, &newest_record);
        engine
            .owner()
            .receive(
                workspace,
                tilo_peer_state::TransportOrigin::RetainedRelayDelta(
                    host.key.verifying_key().to_bytes(),
                ),
                message,
                None,
                testing::now(),
            )
            .expect("this device takes what the host sent");
        link.record_opened(topic_of(workspace), channel, 1);
        assert!(
            arriving.as_mut().now_or_never().is_some(),
            "the host's word settles the head"
        );
        assert_eq!(
            records.chain_head_answer(&head),
            Some(ChainHeadAnswer::NewestPartOnItsWay),
            "a part came, so the chain does not start its first"
        );
        assert!(
            engine.holds_record(&id, APP, &newest_record),
            "the newest part is here"
        );
        assert!(
            !engine.holds_record(&id, APP, &head_record),
            "the head itself is no document"
        );
    }

    /// A chain the workspace holds no part of at all: its head is answered with nothing, and the
    /// chain starts its first part rather than waiting for one forever.
    #[test]
    fn a_chains_head_the_workspace_has_no_part_of_is_answered_with_none() {
        let state = tempfile::tempdir().expect("a state directory");
        let (engine, cert, key) = device(state.path(), 0x59);
        let (workspace, _host) = joined(&engine, &cert, &key);
        let (link, mut said) = planed(&engine);
        let records = Records::new(Arc::clone(&engine));
        let (head_record, _) = chain_records();
        let channel = neighbour_wire::app_document_channel(APP, &head_record);
        link.host_ready(topic_of(workspace), true);
        let head = context(workspace, &head_record, cert);
        let mut arriving = records
            .await_arriving_record(&head)
            .expect("the head asks for itself");
        assert!(matches!(said.try_recv(), Ok(Cmd::OpenRecord { .. })));
        assert!(arriving.as_mut().now_or_never().is_none());
        link.record_opened(topic_of(workspace), channel, 0);
        assert!(
            arriving.as_mut().now_or_never().is_some(),
            "the host's word settles the head"
        );
        assert_eq!(
            records.chain_head_answer(&head),
            Some(ChainHeadAnswer::NoPart),
            "the workspace has no part of the chain, so the app starts the first"
        );
    }

    /// A workspace that replays every record answered for its chains' heads with that replay: what
    /// this device holds is all of the chain there is, so nothing is asked and a chain with no part
    /// starts its first.
    #[test]
    fn a_workspace_that_replays_every_record_answered_for_the_heads_too() {
        let state = tempfile::tempdir().expect("a state directory");
        let (engine, cert, key) = device(state.path(), 0x5a);
        let (workspace, _host) = joined(&engine, &cert, &key);
        let (link, mut said) = planed(&engine);
        let records = Records::new(Arc::clone(&engine));
        link.host_ready(topic_of(workspace), false);
        let (head_record, _) = chain_records();
        let head = context(workspace, &head_record, cert);
        let arriving = records.await_arriving_record(&head);
        assert!(
            arriving.is_none_or(|wait| wait.now_or_never().is_some()),
            "nothing is waited for: the replay answered for the chain"
        );
        assert!(said.try_recv().is_err(), "and nobody is asked");
        assert_eq!(
            records.chain_head_answer(&head),
            Some(ChainHeadAnswer::NoPart)
        );
    }
}
