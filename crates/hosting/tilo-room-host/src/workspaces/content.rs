//! Content a governed workspace's records reference that this host keeps: what
//! its bots fetched from outside (a bridged person's avatar, a file posted
//! elsewhere), the surfaces of the apps it decided changes with, whichever
//! version those changes were written by, which members decide the host's
//! changes against, and the files of a workspace its members wrote and left
//! ([`crate::workspaces::ContentKeeper`]).
//!
//! A member asks the workspace for content its record lacks with the workspace's
//! presence ([`NeighbourMsg::RecordContentWant`]), as it asks any member.
//! The host answers from everything it keeps, to that member's device alone, in
//! the same bounded pieces a member answers with; the member keeps the
//! content once every piece arrived and the bytes hash to what it asked.
//!
//! The host asks too: it keeps a copy of the files members post
//! ([`ContentKeeper`]), so they stay in the workspace after the member who
//! posted them leaves, and of the app surfaces a change it must decide names
//! and it lacks ([`super::surfaces`]). It asks the workspace on its own
//! presence, and the pieces members answer it with come to it, not to another
//! device.
//!
//! Several keepers keep for a host at once (its bots' files, its workspaces'
//! files), each of them saying which of the pieces members answer the host
//! with are its own; and several sources serve, each answering with the
//! content it keeps and leaving the rest to the others.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::{Arc, Mutex, RwLock, Weak};

use neighbour_wire::NeighbourMsg;
use neighbour_wire::content::ContentWants;
use tilo_peer_state::peer_handle;

use super::{CollaborationError, Workspaces};
use tilo_lock::{MutexRecover as _, RwLockRecover as _};

/// The bytes of one piece, as members answer.
const PIECE_BYTES: usize = 192 * 1024;
/// Content the host answers with at most.
const MAX_CONTENT_BYTES: usize = 64 * 1024 * 1024;

/// Word from the host `host` that it keeps none of what `body`, a
/// member's `NeighbourMsg::RecordContentWant`, asks for, as a frame for the
/// member's device alone: the member stops counting on this host for it.
/// `None` for any other presence.
pub(crate) fn lacks_answer(host: &[u8; 32], body: &[u8]) -> Option<Vec<u8>> {
    let Ok(NeighbourMsg::RecordContentWant {
        requester,
        app_id,
        cluster_id,
        hash,
        ..
    }) = tilo_failure::ignored!(
        "presence carries other formats, and only a content want is answered",
        postcard::from_bytes(body)
    )
    else {
        return None;
    };
    let hash = hash.to_ascii_lowercase();
    if !tilo_content::valid_hash(&hash) {
        return None;
    }
    postcard::to_allocvec(&NeighbourMsg::RecordContentLacks {
        responder: peer_handle(host),
        requester,
        app_id,
        cluster_id,
        hash,
    })
    .ok()
}

/// Where a host's content comes from.
pub trait WorkspaceContent: Send + Sync {
    /// The bytes named `hash` (lowercase BLAKE3 hex) that a record of `app`
    /// in `workspace` may reference, when this host keeps them.
    fn read(&self, workspace: &str, app: &str, hash: &str) -> Option<Vec<u8>>;
}

/// Where content members answer the host with goes.
pub trait ContentKeeper: Send + Sync {
    /// `frame`, a `NeighbourMsg::RecordContentPiece` the member device
    /// `from` sent this host in `workspace`.
    fn piece(&self, workspace: &str, from: [u8; 32], frame: &[u8]);

    /// Whether `frame` answers a request this host made in `workspace` and
    /// would keep: such a piece counts against what the keeper keeps, not
    /// the member's general content budget.
    fn expects(&self, workspace: &str, frame: &[u8]) -> bool {
        let _ = (workspace, frame);
        false
    }
}

/// The content source a host's workspaces answer from, and the keeper of what
/// members answer the host with, once it has them.
#[derive(Default)]
pub(super) struct ContentSlot {
    pub(super) sources: RwLock<Vec<Arc<dyn WorkspaceContent>>>,
    /// Held weakly: a keeper keeps the workspaces that answer members' pieces
    /// (a host's files do), so a strong handle here would keep the keeper and
    /// the workspaces alive — and the host's endpoint with them — for as long
    /// as the process runs.
    pub(super) keepers: RwLock<Vec<Weak<dyn ContentKeeper>>>,
    /// The app surfaces this host asked the workspace for, by hash
    /// ([`Workspaces::ask_surface`]).
    pub(super) surfaces: Mutex<ContentWants<()>>,
    /// The surfaces the host lacks and asks for until they arrive, by hash:
    /// the workspace and app each is of.
    pub(super) missing: Mutex<HashMap<[u8; 32], (String, String)>>,
}

impl ContentSlot {
    fn get(&self) -> Vec<Arc<dyn WorkspaceContent>> {
        self.sources.read_recover().clone()
    }

    fn keepers(&self) -> Vec<Arc<dyn ContentKeeper>> {
        self.keepers
            .read_recover()
            .iter()
            .filter_map(Weak::upgrade)
            .collect()
    }
}

impl Workspaces {
    /// Answer members' requests for content from `content`, beside whatever
    /// else this host serves.
    pub fn serve_content(&self, content: Arc<dyn WorkspaceContent>) {
        let mut sources = self.inner.content.sources.write_recover();
        if !sources.iter().any(|served| Arc::ptr_eq(served, &content)) {
            sources.push(content);
        }
    }

    /// The uuid of the workspace governed here whose channels are
    /// `workspace`; `None` for one not governed here.
    pub async fn workspace_identity(&self, workspace: u64) -> Option<String> {
        let workspaces = self
            .inner
            .workspaces
            .lock()
            .await
            .iter()
            .map(|(identity, governed)| (identity.clone(), governed.clone()))
            .collect::<Vec<_>>();
        for (identity, governed) in workspaces {
            if governed.lock().await.workspace_id() == workspace {
                return Some(identity);
            }
        }
        None
    }

    /// The name of the workspace governed here whose channels are
    /// `workspace`; `None` for one not governed here.
    pub async fn workspace_name(&self, workspace: u64) -> Option<String> {
        let identity = self.workspace_identity(workspace).await?;
        let governed = self.inner.workspaces.lock().await.get(&identity)?.clone();
        let name = governed.lock().await.record.name.clone();
        Some(name)
    }

    /// The bytes named `hash` this host keeps for the workspace of `workspace`
    /// governed here, checked against the hash: what its bots fetched and
    /// the files it keeps for members. `None` for a workspace not governed here
    /// or content it does not keep.
    pub async fn kept_content(&self, workspace: u64, hash: &str) -> Option<Vec<u8>> {
        let sources = self.inner.content.get();
        let workspaces = self
            .inner
            .workspaces
            .lock()
            .await
            .iter()
            .map(|(identity, governed)| (identity.clone(), governed.clone()))
            .collect::<Vec<_>>();
        for (identity, governed) in workspaces {
            if governed.lock().await.workspace_id() == workspace {
                return sources.iter().find_map(|source| {
                    source
                        .read(&identity, "", hash)
                        .filter(|bytes| tilo_content::verify(hash, bytes))
                });
            }
        }
        None
    }

    /// Every record of `workspace` the governor's copy holds (or of `only`
    /// among them), each saved with all of its history: what tells a host
    /// which content the workspace still references, now or in a version it
    /// can go back to. Read from a copy of the governor's state, taken in a
    /// moment: the workspace takes writes while its records are read.
    /// A record that cannot be read whole (sealed for a channel this host has
    /// no keys of, or with sealed writes that do not open or merge yet) is
    /// logged with why and listed in [`WholeRecords::unread`], and the rest
    /// are read all the same: a caller that must see every reference checks
    /// [`WholeRecords::is_whole`]. A person's own record sealed for their
    /// devices alone is listed in [`WholeRecords::private`] instead.
    ///
    /// # Errors
    ///
    /// The workspace is not governed here.
    pub async fn whole_records(
        &self,
        workspace: &str,
        only: Option<&BTreeSet<(String, String)>>,
    ) -> Result<WholeRecords, CollaborationError> {
        let state = self.workspace(workspace).await?;
        let (peer, device) = {
            let governed = state.lock().await;
            let peer = governed.peer().clone();
            (peer, governed.device.clone())
        };
        let routes: Vec<(String, String)> = peer
            .plaintext_routes()
            .into_iter()
            .chain(peer.sealed_routes())
            .filter(|route| only.is_none_or(|only| only.contains(route)))
            .collect();
        let mut read = WholeRecords::default();
        for (app, record) in routes {
            #[cfg(any(feature = "agents", feature = "web"))]
            let document = super::sealed_records::whole_document(
                &peer,
                device.as_ref(),
                workspace,
                (&app, &record),
            )
            .await;
            #[cfg(not(any(feature = "agents", feature = "web")))]
            let document = {
                let _ = &device;
                if peer.publication().is_sealed_record(&app, &record) {
                    Err("it is sealed and this host opens no sealed records".to_owned())
                } else {
                    peer.document(&app, &record)
                        .ok_or_else(|| "the host holds no copy of it".to_owned())
                }
            };
            let document = document.and_then(|document| {
                peer.record_revision(&app, &record)
                    .map(|revision| (document, revision))
                    .ok_or_else(|| "it has no revision yet".to_owned())
            });
            match document {
                Ok((document, revision)) => {
                    read.revisions
                        .insert((app.clone(), record.clone()), revision);
                    read.documents.push(((app, record), document));
                }
                // A person's own record sealed for their devices alone
                // ([`tilo_action_policy::member_record`], their drafts) is not
                // the host's to read: it is named apart, not as unread.
                Err(why)
                    if tilo_action_policy::member_record(&record).is_some()
                        && peer.publication().is_sealed_record(&app, &record) =>
                {
                    tracing::debug!(workspace, %app, %record, %why, "a person's own record is not the host's to read");
                    read.private.push((app, record));
                }
                Err(why) => {
                    tracing::warn!(
                        workspace,
                        %app,
                        %record,
                        %why,
                        "a record of the workspace does not read whole"
                    );
                    read.unread.push(((app, record), why));
                }
            }
        }
        Ok(read)
    }

    /// Run `act` while `workspace` takes no write, if none of the records
    /// `read` read has changed since and none was added: `Ok(Ok(_))` with
    /// what it answered, or `Ok(Err(_))` with the records that changed or
    /// appeared, for the caller to read again. The workspace waits only as
    /// long as `act` takes.
    ///
    /// # Errors
    ///
    /// The workspace is not governed here.
    pub async fn while_unwritten<R>(
        &self,
        workspace: &str,
        read: &WholeRecords,
        act: impl FnOnce() -> R,
    ) -> Result<Result<R, BTreeSet<(String, String)>>, CollaborationError> {
        let state = self.workspace(workspace).await?;
        let governed = state.lock().await;
        // Members' writes are taken into the replica under its own lock: held
        // too, nothing lands while `act` runs.
        let peer = governed.peer();
        let changed: BTreeSet<(String, String)> = peer
            .plaintext_routes()
            .into_iter()
            .chain(peer.sealed_routes())
            .filter(|(app, record)| {
                read.revisions.get(&(app.clone(), record.clone()))
                    != peer.record_revision(app, record).as_ref()
            })
            .collect();
        let answer = if changed.is_empty() {
            Ok(act())
        } else {
            Err(changed)
        };
        drop(peer);
        drop(governed);
        Ok(answer)
    }

    /// Hand the pieces of content members send this host to `keeper`, beside
    /// whatever else this host keeps. The keeper is not held: whoever runs it
    /// owns it, and its pieces stop coming here when it goes.
    pub fn keep_content(&self, keeper: &Arc<dyn ContentKeeper>) {
        let mut keepers = self.inner.content.keepers.write_recover();
        keepers.retain(|kept| kept.strong_count() > 0);
        let keeper = Arc::downgrade(keeper);
        if !keepers.iter().any(|kept| Weak::ptr_eq(kept, &keeper)) {
            keepers.push(keeper);
        }
    }

    /// `frame`, a piece of content the member device `from` sent this host
    /// in `workspace`: to the keeper that asked for it, or, when it is a
    /// surface the host asked for, into the content plane the host decides
    /// with. Dropped when the host expects neither.
    pub fn content_for_host(&self, workspace: &str, from: [u8; 32], frame: &[u8]) {
        for keeper in self.inner.content.keepers() {
            if keeper.expects(workspace, frame) {
                keeper.piece(workspace, from, frame);
                return;
            }
        }
        // The asker of a surface is this host: a piece is kept when it is
        // addressed to the host's own device, whoever among the members
        // answered with it.
        let device = self.inner.host.cert.device_public_key;
        let arrived = self.inner.content.surfaces.lock_recover().piece(
            &device,
            frame,
            tilo_clock::unix_now_ms(),
        );
        if let Some(arrived) = arrived {
            crate::surfaces::keep(&arrived.hash, &arrived.bytes);
        }
    }

    /// Whether `frame`, a piece of content a member sent this host in
    /// `workspace`, answers a request this host made: a keeper's
    /// ([`ContentKeeper::expects`]) or its own for a surface.
    pub(crate) fn host_expects(&self, workspace: &str, frame: &[u8]) -> bool {
        let device = self.inner.host.cert.device_public_key;
        self.inner
            .content
            .keepers()
            .iter()
            .any(|keeper| keeper.expects(workspace, frame))
            || self
                .inner
                .content
                .surfaces
                .lock_recover()
                .expects(&device, frame)
    }

    /// Ask `workspace`'s members for the app surface `hash` of `app` this host
    /// lacks, unless it asked lately ([`ContentWants::ask`]): the changes held
    /// for it are decided once a member answers with it
    /// ([`crate::surfaces::arrivals`]). The number of devices it reached.
    pub async fn ask_surface(&self, workspace: &str, app: &str, hash: &str) -> usize {
        let device = self.inner.host.cert.device_public_key;
        let asked = self.inner.content.surfaces.lock_recover().ask(
            &device,
            (),
            (app, workspace),
            hash,
            tilo_clock::unix_now_ms(),
        );
        let Some(request) = asked else {
            return 0;
        };
        match self.ask_members(workspace, request).await {
            Ok(reached) => reached,
            Err(error) => {
                tracing::debug!(%workspace, %hash, %error, "could not ask the workspace for a surface");
                0
            }
        }
    }

    /// Publish `body`, a `NeighbourMsg::RecordContentWant` naming this
    /// host, to `workspace`'s members: the ones keeping the content answer the
    /// host. A member reaches the workspace's other devices only through this
    /// host, and the host asks them the same way they ask it — on the live
    /// plane for the members reaching it there, and on the workspace's own
    /// link for those directly connected, which is where their answers come
    /// from. The number of devices it reached on the live plane.
    ///
    /// # Errors
    ///
    /// The workspace is not governed here, or its workspace not hosted here.
    pub async fn ask_members(&self, workspace: &str, body: Vec<u8>) -> Result<usize, String> {
        let workspace_id = self
            .workspace(workspace)
            .await
            .map_err(|error| error.to_string())?
            .lock()
            .await
            .workspace_id();
        let reached = self
            .inner
            .host
            .records
            .host_presence(workspace_id, body.clone())
            .await?;
        if let Ok(message) = tilo_failure::ignored!(
            "the body is the host's own request, which it just encoded",
            postcard::from_bytes::<NeighbourMsg>(&body)
        ) && let Some(link) = self.inner.direct.link(workspace_id)
            && let Err(error) = link.send(message)
        {
            tracing::debug!(%workspace, %error, "a workspace's link did not carry the host's request");
        }
        Ok(reached)
    }

    /// The answer to `body`, a presence `workspace`'s member published, when it
    /// asks for content: the pieces of what this host keeps, or word that
    /// it keeps none, as frames for the member's device alone. Empty for
    /// any other presence. What it keeps of app surfaces is the workspace's
    /// content plane, never a registry.
    pub fn content_answer(&self, workspace: &str, body: &[u8]) -> Vec<Vec<u8>> {
        let Ok(NeighbourMsg::RecordContentWant {
            requester,
            app_id,
            cluster_id,
            hash,
            have,
        }) = tilo_failure::ignored!(
            "presence carries other formats, and only a content want is answered",
            postcard::from_bytes(body)
        )
        else {
            return Vec::new();
        };
        let hash = hash.to_ascii_lowercase();
        if cluster_id != workspace || !tilo_content::valid_hash(&hash) {
            return Vec::new();
        }
        let responder = peer_handle(&self.inner.host.cert.device_public_key);
        let Some(bytes) = self
            .app_file(workspace, &hash)
            .or_else(|| {
                self.inner
                    .content
                    .get()
                    .iter()
                    .find_map(|source| source.read(workspace, &app_id, &hash))
            })
            // A record started by an older version of an app reads its writes
            // with that version's surface, which the workspace's content plane
            // keeps ([`crate::surfaces`]).
            .or_else(|| crate::surfaces::kept(&app_id, &hash))
            .filter(|bytes| bytes.len() <= MAX_CONTENT_BYTES)
        else {
            return lacks_answer(&self.inner.host.cert.device_public_key, body)
                .into_iter()
                .collect();
        };
        let total = u64::try_from(bytes.len()).unwrap_or(u64::MAX);
        let piece = |offset: u64, chunk: &[u8]| NeighbourMsg::RecordContentPiece {
            responder,
            requester,
            app_id: app_id.clone(),
            cluster_id: cluster_id.clone(),
            hash: hash.clone(),
            total,
            offset,
            bytes: chunk.to_vec(),
        };
        // At most a window of what the asker lacks: it asks again for the
        // rest, so a large file crosses a session's outbound queue in windows
        // instead of filling it and losing what it cannot carry
        // ([`neighbour_wire::content::answer`] makes the same answer for the
        // content a keeper holds).
        let pieces = if bytes.is_empty() {
            vec![piece(0, &[])]
        } else {
            let mut pieces = Vec::new();
            for (index, chunk) in bytes.chunks(PIECE_BYTES).enumerate() {
                if pieces.len() >= neighbour_wire::content::PIECES_PER_ANSWER {
                    break;
                }
                let offset = u64::try_from(index.saturating_mul(PIECE_BYTES)).unwrap_or(u64::MAX);
                let len = u64::try_from(chunk.len()).unwrap_or(u64::MAX);
                if neighbour_wire::content::holds(&have, offset, len) {
                    continue;
                }
                pieces.push(piece(offset, chunk));
            }
            pieces
        };
        pieces
            .iter()
            .filter_map(|message| postcard::to_allocvec(message).ok())
            .collect()
    }
}

/// Records of a workspace read whole ([`Workspaces::whole_records`]), with
/// the revision each was read at, and those that did not read whole.
#[derive(Default)]
pub struct WholeRecords {
    /// Each record, by its app and id, saved with all of its history.
    pub documents: Vec<((String, String), Vec<u8>)>,
    /// Each record that did not read whole, by its app and id, with why.
    pub unread: Vec<((String, String), String)>,
    /// People's own records sealed for their devices alone, by app and id:
    /// not the host's to read, and no part of what it reads whole.
    pub private: Vec<(String, String)>,
    revisions: BTreeMap<(String, String), [u8; 32]>,
}

impl WholeRecords {
    /// Every record the host is to read read whole: what they name is all the
    /// workspace names that the host keeps for it. A person's own sealed
    /// record is theirs, not one the host reads.
    #[must_use]
    pub fn is_whole(&self) -> bool {
        self.unread.is_empty()
    }

    /// Take in `newer`, records read again: their revisions are the ones
    /// read last, and each read again is unread only if it is unread now.
    pub fn extend(&mut self, newer: Self) {
        self.unread.retain(|(route, _)| {
            !newer.revisions.contains_key(route)
                && !newer.unread.iter().any(|(again, _)| again == route)
        });
        self.documents.extend(newer.documents);
        self.unread.extend(newer.unread);
        self.private
            .retain(|route| !newer.revisions.contains_key(route));
        for route in newer.private {
            if !self.private.contains(&route) {
                self.private.push(route);
            }
        }
        self.revisions.extend(newer.revisions);
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, reason = "in a test this is the assertion")]

    use super::*;

    struct One(Vec<u8>);

    impl WorkspaceContent for One {
        fn read(&self, workspace: &str, app: &str, hash: &str) -> Option<Vec<u8>> {
            (workspace == ROOM
                && app == "so.tilo.chat"
                && hash == blake3::hash(&self.0).to_hex().as_str())
            .then(|| self.0.clone())
        }
    }

    const ROOM: &str = "2f10f463-8f4c-43a3-b9dd-18df422eb0bd";

    fn want(workspace: &str, hash: &str) -> Vec<u8> {
        postcard::to_allocvec(&NeighbourMsg::RecordContentWant {
            requester: 42,
            app_id: "so.tilo.chat".into(),
            cluster_id: workspace.into(),
            hash: hash.into(),
            have: Vec::new(),
        })
        .unwrap()
    }

    #[tokio::test]
    #[allow(
        clippy::significant_drop_tightening,
        reason = "the relay stays live until its consuming shutdown"
    )]
    async fn a_member_asking_for_content_gets_it_in_pieces_or_word_that_it_is_not_kept() {
        let scratch = tempfile::tempdir().unwrap();
        let (key, cert) = crate::workspaces::tests::governor();
        let relay = crate::workspaces::tests::test_host(scratch.path(), &key, &cert).await;
        let workspaces = relay.workspaces().clone();
        let avatar = vec![7_u8; PIECE_BYTES + 10];
        let hash = blake3::hash(&avatar).to_hex().to_string();
        assert_eq!(
            lacks(&workspaces.content_answer(ROOM, &want(ROOM, &hash))),
            Some((peer_handle(&cert.device_public_key), 42)),
            "a host that serves no content keeps none"
        );
        workspaces.serve_content(Arc::new(One(avatar.clone())));

        let frames = workspaces.content_answer(ROOM, &want(ROOM, &hash));
        let mut gathered = Vec::new();
        for frame in &frames {
            let NeighbourMsg::RecordContentPiece {
                responder,
                requester,
                total,
                offset,
                bytes,
                ..
            } = postcard::from_bytes(frame).unwrap()
            else {
                panic!("a piece");
            };
            assert_eq!(responder, peer_handle(&cert.device_public_key));
            assert_eq!(requester, 42, "addressed to whoever asked");
            assert_eq!(total, u64::try_from(avatar.len()).unwrap());
            assert_eq!(offset, u64::try_from(gathered.len()).unwrap());
            gathered.extend(bytes);
        }
        assert_eq!(frames.len(), 2);
        assert_eq!(gathered, avatar);

        let other = "0b7e4c2a-9d1f-4e3b-8a6c-5f2d1e0c9b8a";
        assert!(
            workspaces
                .content_answer(other, &want(ROOM, &hash))
                .is_empty(),
            "another room's request"
        );
        assert_eq!(
            lacks(&workspaces.content_answer(ROOM, &want(ROOM, &"0".repeat(64)))),
            Some((peer_handle(&cert.device_public_key), 42)),
            "not kept: the member is told so"
        );
        assert!(workspaces.content_answer(ROOM, b"not a request").is_empty());
    }

    /// Who said, to whom, that it keeps none of what was asked: the one
    /// frame `frames` holds when it is such word.
    fn lacks(frames: &[Vec<u8>]) -> Option<(u64, u64)> {
        let [frame] = frames else {
            return None;
        };
        match postcard::from_bytes(frame).ok()? {
            NeighbourMsg::RecordContentLacks {
                responder,
                requester,
                ..
            } => Some((responder, requester)),
            _ => None,
        }
    }
}
