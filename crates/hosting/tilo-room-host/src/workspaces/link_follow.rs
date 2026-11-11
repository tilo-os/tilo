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

use super::follow::Followed;
use super::{GovernedWorkspace, WeakWorkspaces, Workspaces};
use neighbour_wire::NeighbourMsg;
use std::sync::Arc;

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
    /// The direct links of the workspaces governed here, as the host's own
    /// device: a member device of a workspace syncs with it here, and catches
    /// up from the history the host keeps.
    #[must_use]
    pub fn direct(&self) -> tilo_peer_state::direct::Serving {
        self.inner.direct.clone()
    }

    /// Serve `workspace`'s replica over the direct link, keeping its history
    /// at the host's store, and take in what its members send there.
    pub(super) fn serve_direct(&self, workspace: &GovernedWorkspace) -> Result<(), String> {
        let records = self.inner.host.records.clone();
        let workspace_id = workspace.workspace_id();
        let link = tilo_peer_state::direct::DirectSync::over(
            workspace.peer.clone(),
            self.inner.host.key.clone(),
        )
        .with_store(Arc::clone(&self.inner.store))
        .with_channel_history(Arc::new(move || {
            let records = records.clone();
            Box::pin(async move { records.channel_governance(workspace_id).await })
        }));
        #[cfg(test)]
        let link = match self
            .inner
            .link_heard_capacity
            .load(std::sync::atomic::Ordering::Acquire)
        {
            0 => link,
            capacity => link.with_heard_capacity(capacity),
        };
        self.inner.direct.serve(link.clone())?;
        self.follow_link(workspace, &link);
        Ok(())
    }

    /// Give the links of the workspaces served from now on room for only
    /// `capacity` frames heard and not read yet: what a test that makes the
    /// host's reader of a link fall behind sets.
    #[cfg(all(test, feature = "agents"))]
    pub(crate) fn bound_link_readers(&self, capacity: usize) {
        self.inner
            .link_heard_capacity
            .store(capacity, std::sync::atomic::Ordering::Release);
    }

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
    /// it does a session's own write, trying again until it is durable there
    /// ([`Self::carry_until_landed`]). Only the link's first copy of a frame
    /// comes here ([`tilo_peer_state::direct::Heard::fresh`]), so nothing a
    /// session sent reaches the sessions twice.
    fn carry_to_sessions(
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
        self.carry_until_landed(workspace, number, author, commit);
    }

    /// Carry to the relay's sessions every record write `workspace`'s link
    /// keeps that the relay's own history lacks, in its author's name, as the
    /// link's first copy of a write is carried ([`Self::carry_to_sessions`]):
    /// writes the link kept before the relay carried each one, or that did
    /// not reach the relay as they came.
    ///
    /// A page that opens a record, a part behind its chain's head or an
    /// on-demand one, is sent what the relay's history holds of it, and a
    /// record that history lacks is one the page's app makes anew. The
    /// relay's history answers for the workspace only once it holds what the
    /// link holds, as the link's holds what the relay retains
    /// ([`Self::recall`]). Returns how many writes it carried.
    pub(super) async fn keep_link_history(&self, workspace: &GovernedWorkspace) -> usize {
        let workspace_id = workspace.workspace_id();
        let retained = match self.inner.host.records.retained(workspace_id).await {
            Ok(retained) => retained,
            Err(error) => {
                tracing::warn!(workspace = %workspace.workspace(), %error, "the relay's history was not read; what only the link kept stays there");
                return 0;
            }
        };
        let held: std::collections::HashSet<(tilo_peer_state::store::Lineage, u64)> = retained
            .iter()
            .filter_map(|retained| {
                // ignored-failure: a retained body that does not decode is one recall reports, and holds no write the link could match
                postcard::from_bytes::<NeighbourMsg>(&retained.body).ok()
            })
            .filter_map(|message| {
                // ignored-failure: only sealed record writes are what the link keeps; anything else matches none of them
                tilo_peer_state::store::Frame::of(&message).ok()
            })
            .map(|frame| (frame.lineage, frame.seq))
            .collect();
        let mut cursors = Vec::new();
        let mut carried = 0_usize;
        loop {
            let replay = match self.inner.store.replay(
                workspace_id,
                &cursors,
                tilo_peer_state::store::Budget::default(),
            ) {
                Ok(replay) => replay,
                Err(error) => {
                    tracing::warn!(workspace = %workspace.workspace(), %error, "the link's history was not read; the relay's lacks what only the link kept");
                    return carried;
                }
            };
            for message in &replay.frames {
                let (Ok(frame), Some(author)) = (
                    tilo_peer_state::store::Frame::of(message),
                    tilo_peer_state::store::author_of(message),
                ) else {
                    continue;
                };
                if held.contains(&(frame.lineage, frame.seq)) {
                    continue;
                }
                self.carry_to_sessions(workspace.workspace(), author, message);
                carried = carried.saturating_add(1);
            }
            tilo_peer_state::store::advance(&mut cursors, &replay.frames);
            if !replay.more || replay.frames.is_empty() {
                break;
            }
        }
        if carried > 0 {
            tracing::info!(workspace = %workspace.workspace(), carried, "the relay's history took what only the link kept");
        }
        carried
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
    /// missed before it reads on ([`tilo_peer_state::direct::Reader`]), and
    /// carries those first heard on the link to the sessions: a member is told
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
        let mut reader = link.reader();
        tokio::spawn(async move {
            // The link is gone: nothing more comes through it.
            while let Some(frame) = reader.recv().await {
                // The host this link served is gone: so is the answer.
                let Some(workspaces) = workspaces.upgrade() else {
                    return;
                };
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
                    workspaces.carry_to_sessions(&name, frame.author, &frame.message);
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
}

impl Workspaces {
    /// Replay every record write the relay retains for `workspace` into the
    /// governor's copy, and keep what it took in.
    pub(super) async fn recall(&self, workspace: &mut GovernedWorkspace) -> Result<(), String> {
        let retained = self
            .inner
            .host
            .records
            .retained(workspace.workspace_id())
            .await?;
        self.recall_retained(workspace, retained);
        Ok(())
    }

    /// Take `retained`, the record writes the relay retains for `workspace`, into
    /// the governor's copy, and keep what it took in. A write held for an
    /// app surface not here yet may be let go past the held queue's bound,
    /// and so is every later write of its author to its record, which
    /// would otherwise overtake it for good. The relay still retains them
    /// all: they are replayed again, in the order their authors wrote them,
    /// once the surface arrives ([`Self::follow`]) or something they waited
    /// for lands ([`Followed::replay_wanted`]).
    pub(super) fn recall_retained(
        &self,
        workspace: &mut GovernedWorkspace,
        retained: Vec<crate::workspace_records::RetainedBody>,
    ) {
        let (mut changed, mut again) = (false, false);
        for retained in retained {
            // What the relay's device wrote is retained under its key, the
            // governor's own commits and its sessions' writes alike: the
            // first are the governor's copy already, the second it took in
            // as they came (`absorb`). Neither is replayed into it.
            if retained.author == self.inner.host.cert.device_public_key {
                continue;
            }
            let message: neighbour_wire::NeighbourMsg = match postcard::from_bytes(&retained.body) {
                Ok(message) => message,
                Err(error) => {
                    tracing::warn!(
                        author = %hex::encode(retained.author),
                        %error,
                        "a retained write does not decode; it is not replayed"
                    );
                    continue;
                }
            };
            let followed = workspace.follow(retained.author, message.clone());
            // The link keeps what the workspace holds from before a device
            // joins it, this start's replay included: a device that joins now
            // reads the history the workspace was founded with.
            self.carry_to_links(workspace.workspace(), retained.author, &message);
            let Followed {
                changed: took,
                replay_wanted,
                refused,
            } = followed;
            changed |= took;
            again |= replay_wanted;
            if let Some(reason) = refused {
                let filed = workspace.refused(
                    &self.inner.host.cert,
                    &self.inner.host.key,
                    &message,
                    &reason,
                );
                changed |= filed.changed;
            } else if took && self.landed(workspace, &message) {
                changed = true;
            }
        }
        if changed {
            workspace.keep_followed();
        }
        // What a write let go waited for landed in this replay: the relay's
        // copy is read again, with what reached it since this one was read.
        // Each further replay needs something new to land, so it ends.
        if again {
            self.replay_soon(workspace.workspace().to_owned());
        }
    }
}
