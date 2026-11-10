//! What a governed workspace's direct link brings its host, how a reader
//! that fell behind it catches up, and how a member's write crosses between
//! the link and the relay's sessions, once each way.
//!
//! A member device of a workspace governed here syncs with the host over the
//! workspace's direct link, with no relay session in between: what it writes
//! there is admitted into the host's replica by the link itself, and the host
//! engine follows it here, as it follows the writes a member sends through a
//! relay session.
//!
//! The link hands each admitted frame to a channel that keeps the newest ones
//! and drops the oldest for a reader that falls behind. A dropped frame is a
//! member's write the host never followed, which is why a reader that lost
//! frames takes the writes among them from the history the link keeps, and
//! goes on reading, rather than ending with the workspace's link unread for as
//! long as the relay runs.

use super::{GovernedWorkspace, WeakWorkspaces, Workspaces};
use neighbour_wire::NeighbourMsg;
use std::sync::Arc;
use tilo_peer_state::store::{self, Budget, Cursor};
use tokio::sync::broadcast::error::RecvError;

/// Carry `body`, `author`'s write the relay admitted for `workspace` from a
/// relay session, to the devices on the workspace's link
/// ([`Workspaces::carry_to_links`]).
///
/// Called where the write is admitted, not on a reader's own schedule: a device
/// linked now reads it live, and one that joins later reads the history as it
/// stands then, the writes in between included.
pub(crate) async fn carry_admitted(
    workspace: &crate::workspace::SharedWorkspace,
    author: [u8; 32],
    body: &[u8],
) {
    let authority = workspace.lock().await.workspace_authority.clone();
    let Some(authority) = authority else {
        return;
    };
    let Some(workspaces) = authority.workspaces() else {
        return;
    };
    // A body that does not decode is one the governor's follow reports.
    let Ok(message) = postcard::from_bytes::<neighbour_wire::NeighbourMsg>(body) else {
        // ignored-failure: the workspace's own follow reports the body that does not decode.
        return;
    };
    workspaces.carry_to_links(authority.workspace(), author, &message);
}

impl Workspaces {
    /// Carry `message`, `author`'s write the relay admitted for `workspace`
    /// from a relay session, to the devices on the workspace's link: live to
    /// those linked now, and kept for those that join later. A member reaches
    /// the workspace on whatever carrier its runtime runs, and every member
    /// reads every other's records and channel governance on its own. What a
    /// member's desk publishes of its windows reaches the link as the
    /// workspace's governance once the governor took it in.
    pub(super) fn carry_to_links(
        &self,
        workspace: &str,
        author: [u8; 32],
        message: &neighbour_wire::NeighbourMsg,
    ) {
        if !matches!(message, neighbour_wire::NeighbourMsg::AppDocument { .. })
            && super::channel_workspace(message).is_none()
        {
            return;
        }
        let Ok(number) = crate::room_workspace_id(workspace) else {
            return;
        };
        if let Some(link) = self.inner.direct.link(number) {
            link.carry(author, message);
        }
    }

    /// Carry `message`, `author`'s record write the workspace's link
    /// admitted, to the members that take part through relay sessions: the
    /// relay retains it in `author`'s name and sends it to every session, as
    /// it does a session's own write. Only the link's first copy of a frame
    /// comes here ([`tilo_peer_state::direct::Heard::fresh`]), so nothing a
    /// session sent reaches the sessions twice.
    async fn carry_to_sessions(
        &self,
        workspace: &str,
        author: [u8; 32],
        message: &neighbour_wire::NeighbourMsg,
    ) {
        if !matches!(message, neighbour_wire::NeighbourMsg::AppDocument { .. }) {
            return;
        }
        let (Ok(number), Some((channel, retention))) = (
            crate::room_workspace_id(workspace),
            neighbour_wire::retention(message, false),
        ) else {
            return;
        };
        let Ok(body) = postcard::to_allocvec(message) else {
            return;
        };
        let commit = crate::workspace_records::HostCommit {
            body,
            channel,
            checkpoint: retention == neighbour_wire::Retention::Checkpoint,
        };
        if let Err(error) = self.inner.host.records.carry(number, author, commit).await {
            tracing::warn!(%workspace, %error, "a linked member's write did not reach the relay's sessions");
        }
    }

    /// Take in what `workspace`'s link admits and answer what its members ask
    /// there: a frame the link admitted is what a member's write through a
    /// relay session was, so it is followed as one ([`Self::absorb`]) and
    /// carried to the relay's sessions ([`Self::carry_to_sessions`]), and each
    /// ask for content is answered with what this host keeps of it, or word
    /// that it keeps none, addressed to whoever asked. The link carries each
    /// ask on to the workspace's other members itself.
    ///
    /// A reader that falls behind the link's frames takes in the writes it
    /// missed before it reads on ([`Self::catch_up_link`]): a member is told
    /// its write landed once the link admitted it, so the host following it is
    /// not the member's to retry. Only the link ending stops the reads.
    pub(super) fn follow_link(
        &self,
        workspace: &GovernedWorkspace,
        link: &tilo_peer_state::direct::DirectSync,
    ) {
        let name = workspace.workspace().to_owned();
        let workspaces = WeakWorkspaces(Arc::downgrade(&self.inner));
        let link = link.clone();
        // Subscribed, standing where the replica stands, before the task runs:
        // every frame the link admits from here on is this reader's, and what
        // the replica held before is not read again.
        let mut heard = link.heard();
        let mut cursors = link.cursors();
        tokio::spawn(async move {
            loop {
                let frame = match heard.recv().await {
                    Ok(frame) => frame,
                    Err(RecvError::Lagged(missed)) => {
                        tracing::warn!(
                            %name,
                            missed,
                            "the host fell behind the frames its link admitted; it takes in the writes it missed"
                        );
                        let Some(workspaces) = workspaces.upgrade() else {
                            return;
                        };
                        workspaces.catch_up_link(&name, &link, &mut cursors).await;
                        continue;
                    }
                    // The link is gone: nothing more comes through it.
                    Err(RecvError::Closed) => return,
                };
                // The host this link served is gone: so is the answer.
                let Some(workspaces) = workspaces.upgrade() else {
                    return;
                };
                // Where this reader stands in the history its link keeps, for
                // the catch-up a reader that falls behind takes.
                store::advance(&mut cursors, std::slice::from_ref(&frame.message));
                let asking = matches!(frame.message, NeighbourMsg::RecordContentWant { .. });
                let answering = matches!(
                    frame.message,
                    NeighbourMsg::RecordContentPiece { .. }
                        | NeighbourMsg::RecordContentLacks { .. }
                );
                let Ok(body) = postcard::to_allocvec(&frame.message) else {
                    continue;
                };
                // The members on relay sessions read it as the linked members
                // do, the first time the link takes it in.
                if frame.fresh && frame.report.is_ok() {
                    workspaces
                        .carry_to_sessions(&name, frame.author, &frame.message)
                        .await;
                }
                if let Err(error) = workspaces.absorb(&name, frame.author, &body).await {
                    tracing::warn!(%name, %error, "the host did not keep what its link admitted");
                }
                // What a member answers the host with answers a request the
                // host made, and is the host's to keep: a device that keeps
                // its workspace's files reads the pieces here
                // ([`Self::content_for_host`]).
                if answering {
                    let (workspaces, name, author) =
                        (workspaces.clone(), name.clone(), frame.author);
                    let body = body.clone();
                    tokio::task::spawn_blocking(move || {
                        workspaces.content_for_host(&name, author, &body);
                    });
                    continue;
                }
                if !asking {
                    continue;
                }
                for answer in workspaces.content_answer(&name, &body) {
                    match postcard::from_bytes::<NeighbourMsg>(&answer) {
                        Ok(message) => {
                            let _ = link.send(message);
                        }
                        Err(error) => {
                            tracing::warn!(%error, "a content answer did not encode");
                        }
                    }
                }
            }
        });
    }

    /// Take in the writes `workspace`'s link admitted while this reader was
    /// behind: the history the link keeps from `cursors` on, in the order the
    /// link wrote it down, each followed as the frame itself was. What the
    /// link never keeps — the presence between the workspace's devices, an ask
    /// for content and its answer — is asked again by the device that wants
    /// it, so nothing here waits on a frame that is gone.
    async fn catch_up_link(
        &self,
        workspace: &str,
        link: &tilo_peer_state::direct::DirectSync,
        cursors: &mut Vec<Cursor>,
    ) {
        let Some(history) = link.store() else {
            tracing::warn!(%workspace, "the link keeps no history to catch up from");
            return;
        };
        let Some(number) = tilo_peer_state::join::room_workspace_id(workspace).ok() else {
            tracing::warn!(%workspace, "the workspace names no number to catch up from");
            return;
        };
        let mut taken = 0_usize;
        loop {
            let replay = match history.replay(number, cursors, Budget::default()) {
                Ok(replay) => replay,
                Err(error) => {
                    tracing::warn!(
                        %workspace,
                        %error,
                        "the history the link keeps could not be read"
                    );
                    return;
                }
            };
            if !replay.floors.is_empty() {
                // A floor is history the link retired: a write the host missed
                // that not even the store still holds. Its author's next write
                // builds on it, which the host cannot follow.
                tracing::warn!(
                    %workspace,
                    floors = replay.floors.len(),
                    "the link retired history the host missed"
                );
            }
            for message in &replay.frames {
                let Some(author) = store::author_of(message) else {
                    continue;
                };
                let Ok(body) = postcard::to_allocvec(message) else {
                    continue;
                };
                if let Err(error) = self.absorb(workspace, author, &body).await {
                    tracing::warn!(
                        %workspace,
                        %error,
                        "the host did not keep what its link admitted"
                    );
                }
                taken = taken.saturating_add(1);
            }
            store::advance(cursors, &replay.frames);
            if !replay.more {
                break;
            }
        }
        if taken > 0 {
            tracing::info!(%workspace, taken, "the host took in the writes it missed");
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        reason = "a fixture failing is its test failing"
    )]
    #![expect(
        clippy::significant_drop_tightening,
        reason = "the host a test binds has to outlive every step it drives, and its shutdown takes it by value"
    )]

    use super::*;
    use crate::workspaces::fixtures::{admit_view_reader, governor_with, test_host_taking_part};
    use automerge::{ROOT, transaction::Transactable as _};
    use collab_types::{CommitNamed as _, ContentRetention, DocumentMutation, OpKind, SignedOp};
    use ed25519_dalek::SigningKey;
    use tilo_peer_state::direct::DirectSync;
    use tilo_user_identity::{DeviceCapabilities, sign_device_cert};

    const APP: &str = "so.tilo.notes";
    const RECORD: &str = "welcome";

    /// A member device writing one record of a workspace sealed, as its page
    /// publishes it: the first write the whole record, every later one a
    /// change on the one before.
    struct SealedWriter {
        key: SigningKey,
        keys: channel_crypto::ChannelContentKeys,
        route: (String, String),
        document: automerge::AutoCommit,
        seq: u64,
    }

    impl SealedWriter {
        /// A writer of the record `(app, record)` of `workspace`, holding the
        /// channel keys the record's writes are sealed under.
        async fn open(
            workspaces: &Workspaces,
            workspace: &str,
            key: SigningKey,
            (app, record): (&str, &str),
        ) -> Self {
            let route = tilo_peer_state::document_frame::DocumentRoute {
                app_id: app.to_owned(),
                cluster_id: workspace.to_owned(),
                record: record.to_owned(),
            };
            let keys = workspaces
                .own_route_keys(&route, true)
                .await
                .expect("the host takes part in the record's channel")
                .expect("the record's channel opens");
            Self {
                key,
                keys,
                route: (app.to_owned(), record.to_owned()),
                document: automerge::AutoCommit::new(),
                seq: 0,
            }
        }

        /// The next write of the record: `change` made to this device's
        /// replica, sealed, and the whole record when `whole`.
        fn write(
            &mut self,
            workspace: &str,
            whole: bool,
            change: impl FnOnce(&mut automerge::AutoCommit),
        ) -> NeighbourMsg {
            let (app, record) = (self.route.0.clone(), self.route.1.clone());
            let heads = self.document.get_heads();
            change(&mut self.document);
            let mutation = if whole {
                DocumentMutation::Snapshot(self.document.save())
            } else {
                DocumentMutation::Incremental(self.document.save_after(&heads))
            };
            let artifact = collab_types::document_artifact_id(&app, workspace, &record);
            let retention = if whole {
                ContentRetention::Checkpoint
            } else {
                ContentRetention::Delta
            };
            let (metadata, payload) = self
                .keys
                .seal_content(artifact, retention, &mutation.encode().unwrap())
                .expect("the record's keys seal the write");
            let author =
                collab_types::Ed25519PublicKey::from_bytes(self.key.verifying_key().to_bytes());
            self.seq = self.seq.saturating_add(1);
            let mut envelope = SignedOp::new_unsigned_with_encryption(
                payload,
                artifact,
                author,
                collab_types::document_attestation_id(&app, workspace, &record),
                OpKind::Change,
                std::time::SystemTime::now(),
                self.seq,
                metadata,
            );
            collab_types::sign_op(&mut envelope, &self.key).unwrap();
            NeighbourMsg::AppDocument {
                peer: tilo_peer_state::peer_handle(author.as_bytes()),
                app_id: app,
                cluster_id: workspace.to_owned(),
                record_id: record,
                envelope: Box::new(envelope),
            }
        }
    }

    /// A reader that falls behind the frames its link admits takes in the
    /// writes among them from the history the link keeps, and reads on: without
    /// it, the link dropped those writes for the host for as long as the relay
    /// ran, and a member whose write the link admitted was never followed.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn a_reader_that_fell_behind_takes_in_the_writes_it_missed() {
        let dir = tempfile::tempdir().unwrap();
        let (key, cert) = governor_with(tilo_user_identity::cloud_enrollment::host_capabilities());
        let relay = test_host_taking_part(dir.path(), &key, &cert).await;
        let workspaces = relay.workspaces().clone();
        let created = workspaces.create("Lounge").await.unwrap();
        let member_key = SigningKey::from_bytes(&[3; 32]);
        let mut member = cert.clone();
        member.device_public_key = member_key.verifying_key().to_bytes();
        member.capabilities = DeviceCapabilities::member();
        sign_device_cert(&mut member, &SigningKey::from_bytes(&[1; 32])).unwrap();
        admit_view_reader(&workspaces, &created.workspace, &key, &member).await;
        let mut writer = SealedWriter::open(
            &workspaces,
            &created.workspace,
            member_key.clone(),
            (APP, RECORD),
        )
        .await;

        // The link the host serves, with room for two frames heard and not
        // read: the reader this test holds falls behind the burst below.
        let serving = workspaces
            .direct()
            .link(created.workspace_id)
            .expect("the host serves the workspace's direct link");
        let link = serving.with_heard_capacity(2);
        let state = workspaces.workspace(&created.workspace).await.unwrap();
        let held = state.lock().await;
        workspaces.follow_link(&held, &link);
        let mut followed = workspaces.record_writes();

        let member_link = DirectSync::new(link.read(Clone::clone), member_key);
        let mut caught_up = link.caught_up();
        let pairing = tokio::spawn({
            let (member, host) = (member_link.clone(), link.clone());
            async move { DirectSync::pair(&member, &host).await }
        });
        // Both sides carry what the other publishes from the moment they
        // paired: a write sent before waits in their outgoing frames, not lost.
        tokio::time::timeout(
            std::time::Duration::from_secs(10),
            caught_up.wait_for(|caught_up| *caught_up),
        )
        .await
        .expect("the devices pair")
        .expect("the link's catch-up");
        // While the workspace is held, the reader cannot take a frame in, and
        // the link drops what arrives: the record founded and changed four
        // times over.
        member_link
            .send(writer.write(&created.workspace, true, |document| {
                document.put(ROOT, "title", "one").unwrap();
                document.commit_edit("fixture", "title");
            }))
            .unwrap();
        for title in ["two", "three", "four", "five"] {
            member_link
                .send(writer.write(&created.workspace, false, |document| {
                    document.put(ROOT, "title", title).unwrap();
                    document.commit_edit("fixture", "title");
                }))
                .unwrap();
        }
        drop(held);
        // Every write the link dropped reaches the host's engine all the same,
        // and the reader goes on reading what arrives after.
        member_link
            .send(writer.write(&created.workspace, false, |document| {
                document.put(ROOT, "title", "six").unwrap();
                document.commit_edit("fixture", "title");
            }))
            .unwrap();
        let last = tilo_test_env::within("the host follows the writes", async {
            let mut last = None;
            for _ in 0..6 {
                last = Some(next_write(&mut followed).await);
            }
            last
        })
        .await
        .expect("the host follows every write");

        assert_eq!(last.record, RECORD);
        assert_eq!(last.workspace, created.workspace);
        pairing.abort();
        drop(workspaces);
        relay.shutdown().await;
    }

    /// The next record write the relay took in, from `followed`: a receiver that
    /// had no room for some of them skips them, which this test's count does
    /// not need.
    async fn next_write(
        followed: &mut tokio::sync::broadcast::Receiver<crate::workspaces::RecordWrite>,
    ) -> crate::workspaces::RecordWrite {
        loop {
            match followed.recv().await {
                Ok(write) => return write,
                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {}
                Err(error @ tokio::sync::broadcast::error::RecvError::Closed) => {
                    panic!("the relay's record writes ended: {error}")
                }
            }
        }
    }
}
