//! The records this device reads outside their apps, as the broker's `NativeDocumentReads`: an
//! app's Open of a record the workspace holds, and an agent's task reading its workspace's records.
//!
//! The read is the engine's ([`Engine::read_record`]), the one path the desk, search and a branch
//! also read a record with, for a workspace that commits its records and one that seals them. A
//! record this device holds nothing of is the app's own channel: an Open only waits for the
//! workspace's host to replay one it retains ([`NativeDocumentReads::await_sealed_history`]).

use std::sync::Arc;

use tilo_action_broker::services::{
    AwaitingHistory, DocumentBytes, NativeDocumentRead, NativeDocumentReadContext,
    NativeDocumentReads, RecordRead,
};
use tilo_broker_core::custody::OwnerCustody as _;
use tilo_desktop_model::WorkspaceId;

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

    /// A record of a sealed workspace this device holds nothing of may have been written on
    /// another device, or before this session: the workspace's host retains it and replays it once
    /// asked. The Open decides the record is new only after that landed — deciding earlier would
    /// have the app make a second one.
    fn await_sealed_history(&self, context: &NativeDocumentReadContext) -> Option<AwaitingHistory> {
        let route = context.route();
        // A workspace this device hosts itself keeps everything it retains here: nothing is
        // replayed to it, so a record it holds nothing of is new.
        if !self.seals_records(&route.cluster_id)
            || !self.engine.reaches_host()
            || self.engine.hosts_itself(&route.cluster_id)
            || self
                .engine
                .holds_record(&route.cluster_id, &route.app_id, &route.record_id)
        {
            return None;
        }
        let engine = Arc::clone(&self.engine);
        let (workspace, app, record) = (
            route.cluster_id.clone(),
            route.app_id.clone(),
            route.record_id.clone(),
        );
        let mut landed = self.engine.arrivals().watching();
        let mut opens = self.engine.record_opens()?;
        // A record the host sends only when asked is decided by its answer; one it sends of its
        // own accord by the end of its replay. Nobody asked, or the replay done, answers now.
        let asked = Engine::sent_when_asked(&record);
        if asked {
            if !engine.request_record(&workspace, &app, &record) {
                return None;
            }
        } else if engine.host_replayed(&workspace) {
            return None;
        }
        Some(Box::pin(async move {
            // The host's replay reaches this device as the record's writes, and then says it is
            // done: a record still not held then is one the workspace has none of.
            let answered = |engine: &Engine| {
                if asked {
                    engine.record_answered(&workspace, &app, &record)
                } else {
                    engine.host_replayed(&workspace)
                }
            };
            while !engine.holds_record(&workspace, &app, &record) && !answered(&engine) {
                let gone = tokio::select! {
                    changed = landed.changed() => changed.is_err(),
                    changed = opens.changed() => changed.is_err(),
                };
                if gone {
                    // The session's own events are gone: decide on what this device holds.
                    return;
                }
            }
        }))
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
    use tilo_action_broker::route::CrdtRoute;
    use tilo_action_broker::services::{
        NativeDocumentRead, NativeDocumentReadContext, NativeDocumentReads as _,
    };
    use tilo_desktop_model::{WorkspaceId, WorkspaceSlot};
    use tilo_peer_state::owner::testing;
    use tilo_user_identity::DeviceCert;

    use super::*;
    use crate::engine::Engine;

    const APP: &str = "so.tilo.notes";
    /// A record a host sends only once asked for it.
    const ON_DEMAND: &str = "derived:on-demand@history";
    const RECORD: &str = "note-1";
    /// What one read may hand out: more than these tests' records hold.
    const MAXIMUM: usize = 1024 * 1024;

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
                .await_sealed_history(&context(workspace, "another-note", cert))
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
                .await_sealed_history(&context(workspace, "terminal-session", cert))
                .is_none(),
            "nothing is waited for"
        );
        assert!(said.try_recv().is_err(), "and nobody is asked");
    }

    /// A host that says it sent all it retains of a record answers the read waiting for it: a
    /// record still not held then is one the workspace has none of.
    #[test]
    fn a_hosts_word_that_it_sent_a_record_whole_answers_the_wait() {
        let state = tempfile::tempdir().expect("a state directory");
        let (engine, _cert, key) = device(state.path(), 0x55);
        let workspace = workspace_with_record(&engine, &key);
        let (link, mut said) = planed(&engine);
        let id = workspace.to_string();
        let opens = engine.record_opens().expect("a plane is running");
        assert!(
            engine.request_record(&id, APP, ON_DEMAND),
            "the host is asked"
        );
        assert!(
            matches!(
                said.try_recv(),
                Ok(tilo_session_client::Cmd::OpenRecord { .. })
            ),
            "the record is asked for on the plane"
        );
        assert!(
            engine.request_record(&id, APP, ON_DEMAND),
            "asked already: still answered"
        );
        assert!(said.try_recv().is_err(), "a read asks once");
        assert!(!engine.record_answered(&id, APP, ON_DEMAND));
        link.record_opened(
            tilo_session_client::plane::topic_of(workspace),
            neighbour_wire::app_document_channel(APP, ON_DEMAND),
        );
        assert!(
            opens.has_changed().expect("the plane's word"),
            "the wait wakes"
        );
        assert!(engine.record_answered(&id, APP, ON_DEMAND));
        assert!(
            !engine.record_answered(&id, APP, "derived:on-demand@another"),
            "only the record it said that of"
        );
        assert!(
            !engine.request_record(&id, APP, RECORD),
            "a record held here is asked of nobody"
        );
        // A record the host sends of its own accord is decided by the end of its replay.
        assert!(!engine.host_replayed(&id));
        link.host_ready(tilo_session_client::plane::topic_of(workspace));
        assert!(engine.host_replayed(&id), "the replay ended");
    }
}
