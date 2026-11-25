//! The content this device's apps write, pushed to the host of its workspace.
//!
//! A record never carries its binaries: a picture, an attachment, a file's
//! bytes live in the content store beside it, and the record names them by
//! reference. The device that wrote them keeps the only copy until the host
//! keeps one too, so the writer hands them over as it keeps them
//! ([`Workspaces::push_content`]): unasked, in pieces, beside the record, and
//! never holding up the write itself. The host never reads a record to find
//! what it names; it could not read a sealed one anyway.
//!
//! The host answers each content it kept whole with
//! [`NeighbourMsg::DocumentContentKept`]. Until then the push is going
//! ([`Workspaces::content_going`]): pushed again when a link to the host
//! starts and when it went unanswered a while, and what leaving the workspace
//! or removing the device waits for ([`Workspaces::content_delivered`]).
//!
//! Each push is the member's hold on the content at the host. When this
//! device's own write drops a record's last reference to content, as this
//! device sees its records, it releases that hold
//! ([`NeighbourMsg::DocumentContentReleased`]); the host lets the content go
//! once no member holds it.

use std::collections::{BTreeMap, BTreeSet};
use std::time::Duration;

use automerge::{AutoCommit, ObjId, ObjType, ROOT, ReadDoc as _, ScalarValueRef, ValueRef};
use neighbour_wire::NeighbourMsg;
use neighbour_wire::content::{PIECE_BYTES, Packer, Piece};
use tilo_lock::MutexRecover as _;
use tilo_peer_state::peer_handle;
use tilo_server_protocol::WorkspaceId;

use super::Workspaces;

/// How long a push goes unanswered before it is pushed again: the host answers
/// a whole content at once, and a piece lost on the way leaves it waiting.
const PUSH_PATIENCE_MS: u64 = 30_000;
/// How often the pushes still going are looked at for one due again.
const PUSH_LOOK_EVERY: Duration = Duration::from_secs(5);

/// The content this device pushed to its workspaces' hosts, and what its own
/// records name.
#[derive(Default)]
pub(super) struct Pushes {
    state: std::sync::Mutex<Pushed>,
    /// Woken as a push is answered or let go.
    moved: tokio::sync::Notify,
}

#[derive(Default)]
struct Pushed {
    /// Content pushed and not answered yet, by workspace and hash: its app,
    /// and when it was last pushed.
    going: BTreeMap<String, BTreeMap<String, Going>>,
    /// The content each record this device wrote names, as it last read the
    /// record, by workspace, app and record.
    named: BTreeMap<(String, String, String), BTreeSet<String>>,
}

struct Going {
    app: String,
    /// The host it goes to.
    host: [u8; 32],
    pushed_ms: u64,
}

impl Workspaces {
    /// Push `hash` of `app`, content an app on this device wrote for a record
    /// of `workspace`, to the host serving this device's copy of it, and keep
    /// it going until the host says it keeps it. A workspace this device
    /// governs itself is its own host: nothing goes anywhere.
    pub fn push_content(&self, workspace: &str, app: &str, hash: &str) {
        let Some(host) = WorkspaceId::parse(workspace)
            .filter(|id| self.member_link(*id).is_some())
            .and_then(|id| self.member_material(id))
            .map(|material| material.hint.host.device_public_key)
        else {
            return;
        };
        self.push_to(workspace, host, app, hash);
    }

    /// Push `hash` of `app` of `workspace` to the host `host`, and keep it
    /// going until that host says it keeps it.
    fn push_to(&self, workspace: &str, host: [u8; 32], app: &str, hash: &str) {
        let Some(id) = WorkspaceId::parse(workspace) else {
            return;
        };
        let hash = hash.to_ascii_lowercase();
        self.inner
            .content
            .pushes
            .state
            .lock_recover()
            .going
            .entry(workspace.to_owned())
            .or_default()
            .insert(
                hash.clone(),
                Going {
                    app: app.to_owned(),
                    host,
                    pushed_ms: tilo_clock::unix_now_ms(),
                },
            );
        self.going_moved(workspace);
        self.send_push(id, host, app, &hash);
    }

    /// Send every piece of `hash` of `app` to `host`, a host of `workspace`,
    /// waiting for room on the link: the one serving this device's copy when
    /// `host` serves it, and the workspace's own link otherwise.
    fn send_push(&self, workspace: WorkspaceId, host: [u8; 32], app: &str, hash: &str) {
        let serving = self
            .member_material(workspace)
            .is_some_and(|material| material.hint.host.device_public_key == host);
        let link = if serving {
            self.member_link(workspace)
        } else {
            self.inner.direct.link(workspace)
        };
        let Some(link) = link else {
            return;
        };
        let device = self.device_cert().device_public_key;
        let workspaces = self.downgrade();
        let (cluster, app, hash) = (workspace.to_string(), app.to_owned(), hash.to_owned());
        tokio::spawn(async move {
            let Some(reading) = workspaces.upgrade() else {
                return;
            };
            // A file's bytes may be read from its Files daemon: on a thread of its own.
            let (cluster_read, app_read, hash_read) = (cluster.clone(), app.clone(), hash.clone());
            let read = tokio::task::spawn_blocking(move || {
                reading.read_kept(&cluster_read, &app_read, &hash_read)
            })
            .await;
            let Ok(Some(bytes)) = read else {
                tracing::debug!(%cluster, %hash, "content to push is no longer kept here");
                return;
            };
            let mut frames = Vec::new();
            let mut packer = Packer::new(
                peer_handle(&device),
                peer_handle(&host),
                &cluster,
                |frame| {
                    frames.push(frame);
                },
            );
            let total = u64::try_from(bytes.len()).unwrap_or(u64::MAX);
            let mut offset = 0_usize;
            loop {
                let end = offset.saturating_add(PIECE_BYTES).min(bytes.len());
                packer.piece(Piece {
                    app_id: app.clone(),
                    hash: hash.clone(),
                    total,
                    offset: u64::try_from(offset).unwrap_or(u64::MAX),
                    bytes: bytes.get(offset..end).unwrap_or_default().to_vec(),
                });
                offset = end;
                if offset >= bytes.len() {
                    break;
                }
            }
            packer.finish();
            for frame in frames {
                if let Err(error) = link.send_content(frame).await {
                    tracing::debug!(%cluster, %hash, %error, "a push did not reach the host");
                    return;
                }
            }
        });
    }

    /// Push again what is still going to the host of `workspace`: all of it
    /// when `all`, as a link to the host starts, and otherwise what went
    /// unanswered past its patience.
    pub(super) fn push_again(&self, workspace: &str, all: bool) {
        let Some(id) = WorkspaceId::parse(workspace) else {
            return;
        };
        let now = tilo_clock::unix_now_ms();
        let due: Vec<([u8; 32], String, String)> = {
            let mut state = self.inner.content.pushes.state.lock_recover();
            state
                .going
                .get_mut(workspace)
                .map(|going| {
                    going
                        .iter_mut()
                        .filter(|(_, going)| {
                            all || now.saturating_sub(going.pushed_ms) >= PUSH_PATIENCE_MS
                        })
                        .map(|(hash, going)| {
                            going.pushed_ms = now;
                            (going.host, going.app.clone(), hash.clone())
                        })
                        .collect()
                })
                .unwrap_or_default()
        };
        for (host, app, hash) in due {
            self.send_push(id, host, &app, &hash);
        }
    }

    /// Keep pushing what goes to the host of `workspace` over `link`, the link
    /// serving this device's copy of it, for as long as the link lasts.
    pub(super) fn keep_pushing(&self, workspace: &str, link: &tilo_peer_state::direct::DirectSync) {
        let mut linked = link.content_linked();
        let workspaces = self.downgrade();
        let workspace = workspace.to_owned();
        tokio::spawn(async move {
            let mut look = tokio::time::interval(PUSH_LOOK_EVERY);
            look.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            loop {
                let all = tokio::select! {
                    changed = linked.changed() => {
                        if changed.is_err() {
                            return;
                        }
                        true
                    }
                    _ = look.tick() => false,
                };
                let Some(workspaces) = workspaces.upgrade() else {
                    return;
                };
                workspaces.push_again(&workspace, all);
            }
        });
    }

    /// The host of `workspace` said it keeps `hash`, which this device pushed:
    /// it goes no more.
    pub(super) fn content_pushed(&self, workspace: &str, hash: &str) {
        let answered = self
            .inner
            .content
            .pushes
            .state
            .lock_recover()
            .going
            .get_mut(workspace)
            .and_then(|going| going.remove(&hash.to_ascii_lowercase()))
            .is_some();
        if answered {
            self.going_moved(workspace);
        }
    }

    /// What goes to the hosts of `workspace` moved: whoever waits for it to
    /// arrive looks again, and the workspace's copy status says how much of
    /// its content is on this device alone.
    fn going_moved(&self, workspace: &str) {
        self.inner.content.pushes.moved.notify_waiters();
        let going = u32::try_from(self.content_going(workspace)).unwrap_or(u32::MAX);
        let (workspaces, workspace) = (self.clone(), workspace.to_owned());
        tokio::spawn(async move { workspaces.lacks_content(&workspace, going).await });
    }

    /// Push `server`, a server taking `workspace` over from this device, the
    /// content this device keeps of it, and wait for the server to say it
    /// keeps it all: what its records name, as far as they open here, and
    /// what this device keeps for the members who pushed it here.
    ///
    /// # Errors
    ///
    /// The server did not keep all of it within [`super::hosts`]' patience.
    pub(super) async fn hand_content_to(
        &self,
        workspace: &str,
        server: [u8; 32],
    ) -> Result<(), String> {
        let mut kept: BTreeSet<(String, String)> = self
            .inner
            .content
            .keepers()
            .iter()
            .flat_map(|keeper| keeper.holdings(workspace))
            .collect();
        for (app, record) in self
            .workspace_document_routes(workspace)
            .await
            .unwrap_or_default()
        {
            let Ok(Some(bytes)) = self.resource_document(workspace, &app, &record).await else {
                continue;
            };
            for hash in content_of(&app, &bytes).unwrap_or_default() {
                kept.insert((app.clone(), hash));
            }
        }
        for (app, hash) in kept {
            if self.read_kept(workspace, &app, &hash).is_some() {
                self.push_to(workspace, server, &app, &hash);
            }
        }
        match self
            .content_delivered(workspace, super::hosts::CONTENT_SETTLES_WITHIN)
            .await
        {
            0 => Ok(()),
            going => Err(format!(
                "the server {} does not keep {going} files of the workspace yet",
                hex::encode(server)
            )),
        }
    }

    /// Push `hash` of `app`, content a member pushed to this host, on to the
    /// other holders of `workspace`, all but the devices in `pushed_by`, which
    /// keep it already: each holder keeps the workspace's content whole,
    /// whichever of them a member reaches.
    pub async fn hand_on_content(
        &self,
        workspace: &str,
        app: &str,
        hash: &str,
        pushed_by: &[[u8; 32]],
    ) {
        let Ok(hosts) = self.hosts(workspace).await else {
            return;
        };
        let own = self.device_cert().device_public_key;
        for hint in hosts {
            let host = hint.host.device_public_key;
            if host != own && !pushed_by.contains(&host) {
                self.push_to(workspace, host, app, hash);
            }
        }
    }

    /// This host let go of `hash` of `app` of `workspace`, which no member
    /// holds any more: it pushes it on no more, and the workspace's other
    /// holders hear that this host lets go of its own hold on it.
    pub fn content_let_go(&self, workspace: &str, app: &str, hash: &str) {
        let Some(id) = WorkspaceId::parse(workspace) else {
            return;
        };
        let dropped = self
            .inner
            .content
            .pushes
            .state
            .lock_recover()
            .going
            .get_mut(workspace)
            .and_then(|going| going.remove(hash))
            .is_some();
        if dropped {
            self.going_moved(workspace);
        }
        let Some(link) = self.inner.direct.link(id) else {
            return;
        };
        let release = NeighbourMsg::DocumentContentReleased {
            requester: peer_handle(&self.device_cert().device_public_key),
            app_id: app.to_owned(),
            cluster_id: workspace.to_owned(),
            hash: hash.to_owned(),
        };
        if let Err(error) = link.send(release) {
            tracing::debug!(%workspace, %error, "the other holders did not hear the host let go");
        }
    }

    /// How many contents this device pushed to the host of `workspace` that
    /// the host has not said it keeps: what is on this device alone.
    #[must_use]
    pub fn content_going(&self, workspace: &str) -> usize {
        self.inner
            .content
            .pushes
            .state
            .lock_recover()
            .going
            .get(workspace)
            .map_or(0, BTreeMap::len)
    }

    /// Wait, at most `within`, until the host of `workspace` keeps everything
    /// this device pushed to it: what leaving the workspace or removing the
    /// device waits for. How many contents are still going when it stops.
    pub async fn content_delivered(&self, workspace: &str, within: Duration) -> usize {
        let delivered = tokio::time::timeout(within, async {
            loop {
                let moved = self.inner.content.pushes.moved.notified();
                let going = self.content_going(workspace);
                if going == 0 {
                    return;
                }
                tracing::info!(%workspace, going, "files are still going to the workspace's server");
                moved.await;
            }
        })
        .await;
        match delivered {
            Ok(()) => 0,
            Err(_) => self.content_going(workspace),
        }
    }

    /// This device's own write reached `app`'s `record` of `workspace`: the
    /// content the record named and names no more, and that no other record
    /// this device wrote names, is released at the host.
    pub(super) async fn own_write_named(&self, workspace: &str, app: &str, record: &str) {
        let Some(id) = WorkspaceId::parse(workspace) else {
            return;
        };
        let Ok(Some(bytes)) = self.resource_document(workspace, app, record).await else {
            return;
        };
        let Ok(names) = content_of(app, &bytes) else {
            return;
        };
        let names: BTreeSet<String> = names.into_iter().collect();
        let released: Vec<String> = {
            let mut state = self.inner.content.pushes.state.lock_recover();
            let key = (workspace.to_owned(), app.to_owned(), record.to_owned());
            let Some(before) = state.named.insert(key, names.clone()) else {
                return;
            };
            let dropped: Vec<String> = before
                .difference(&names)
                .filter(|hash| {
                    !state
                        .named
                        .iter()
                        .any(|((held, ..), named)| held == workspace && named.contains(*hash))
                })
                .cloned()
                .collect();
            if let Some(going) = state.going.get_mut(workspace) {
                for hash in &dropped {
                    going.remove(hash);
                }
            }
            drop(state);
            dropped
        };
        self.going_moved(workspace);
        let Some(link) = self.member_link(id) else {
            return;
        };
        let requester = peer_handle(&self.device_cert().device_public_key);
        for hash in released {
            let release = NeighbourMsg::DocumentContentReleased {
                requester,
                app_id: app.to_owned(),
                cluster_id: workspace.to_owned(),
                hash,
            };
            if let Err(error) = link.send(release) {
                tracing::debug!(%workspace, %error, "a release did not reach the host");
            }
        }
    }

    /// Whether the device `device` may write `app`'s records in `workspace`
    /// now: what a host takes content pushed to it from.
    pub async fn may_push_content(&self, workspace: &str, device: [u8; 32], app: &str) -> bool {
        use tilo_action_policy::{ActionKind, TreePath};
        let Some(peer) = self.state(workspace).await else {
            return false;
        };
        let now = super::clock::system_time(tilo_clock::unix_now_ns());
        peer.permissions(device, now).is_ok_and(|member| {
            member.allows(
                &TreePath::workspace(workspace).app(app),
                ActionKind::Undoable,
                now,
            )
        })
    }
}

/// The content `document`, a saved Automerge record of `app`, names now, by
/// the references records name content with: each value written as a kept
/// `Bytes` value ([`tilo_resource::Bytes::stored_address`]) or as a
/// `tilo.content:` address ([`tilo_actions::content_hash`]), a file's
/// `tilo-blob://` address, a rich text's picture under its `blob` attribute,
/// and, in a record of the Files tree, the content of each file not in its
/// trash and each version a branch kept. A value that only looks like an
/// address (a device key, an id) names nothing: no content stands behind it.
///
/// # Errors
///
/// Why it does not load.
pub fn content_of(app: &str, document: &[u8]) -> Result<Vec<String>, String> {
    let doc = AutoCommit::load(document).map_err(|error| error.to_string())?;
    let mut named = BTreeSet::new();
    references_in(&doc, &ROOT, &mut named);
    #[cfg(feature = "files")]
    if app == tilo_app_id::apps::FILE_EXPLORER {
        files_content(&doc, document, &mut named)?;
    }
    #[cfg(not(feature = "files"))]
    let _ = app;
    Ok(named.into_iter().collect())
}

/// Add the content every value under `obj` of `doc` names by reference to
/// `named`.
fn references_in(doc: &AutoCommit, obj: &ObjId, named: &mut BTreeSet<String>) {
    let values: Vec<(Option<ObjId>, Option<String>)> = match doc.object_type(obj) {
        Ok(ObjType::Map | ObjType::Table) => doc
            .map_range(obj, ..)
            .map(|item| value_of(item.id(), &item.key, &item.value))
            .collect(),
        Ok(ObjType::List) => doc
            .list_range(obj, ..)
            .map(|item| value_of(item.id(), "", &item.value))
            .collect(),
        Ok(ObjType::Text) | Err(_) => return,
    };
    for (child, text) in values {
        if let Some(address) = text {
            named.insert(address);
        }
        if let Some(child) = child {
            references_in(doc, &child, named);
        }
    }
}

/// An item's object, to read under it, or the content it names, kept under
/// `key` ([`tilo_automerge::rich_text::ATTR_BLOB`] names a picture's content).
fn value_of(id: ObjId, key: &str, value: &ValueRef<'_>) -> (Option<ObjId>, Option<String>) {
    match value {
        ValueRef::Object(_) => (Some(id), None),
        ValueRef::Scalar(ScalarValueRef::Str(text)) => (None, reference_address(key, text)),
        ValueRef::Scalar(_) => (None, None),
    }
}

/// The address the reference `text`, kept under `key`, names its content by,
/// as the content store keeps it: a `Bytes` reference, a `tilo.content:`
/// address, a file's `tilo-blob://` address, or, under a rich text's picture
/// attribute, the hash alone.
fn reference_address(key: &str, text: &str) -> Option<String> {
    tilo_resource::Bytes::stored_address(text)
        .or_else(|| tilo_actions::content_hash(text).and_then(tilo_content::content_address))
        .or_else(|| {
            text.strip_prefix(tilo_content::BLOB_SCHEME)
                .and_then(tilo_content::content_address)
        })
        .or_else(|| {
            (key == tilo_automerge::rich_text::ATTR_BLOB)
                .then(|| tilo_content::content_address(text))
                .flatten()
        })
}

/// Add the content the Files tree `document` (its index or one of its chunks)
/// names to `named`: each file's content, unless it is in the trash, and each
/// earlier version a branch of the workspace kept.
///
/// # Errors
///
/// The tree's nodes are malformed.
#[cfg(feature = "files")]
fn files_content(
    doc: &AutoCommit,
    document: &[u8],
    named: &mut BTreeSet<String>,
) -> Result<(), String> {
    use automerge::Value;
    let tree = fs_protocol::tree::Tree::of_document(document).map_err(|error| error.to_string())?;
    for (id, node) in &tree.nodes {
        if tree.trashed(id) {
            continue;
        }
        if let Some(address) = node.hash.as_deref().and_then(tilo_content::content_address) {
            named.insert(address);
        }
    }
    if let Ok(Some((Value::Object(ObjType::Map), aliases))) =
        doc.get(ROOT, fs_protocol::tree::FILE_VERSION_ALIASES_KEY)
    {
        for item in doc.map_range(&aliases, ..) {
            if let ValueRef::Scalar(ScalarValueRef::Str(version)) = item.value
                && fs_protocol::tree::is_file_content_hash(&version)
            {
                named.insert(version.to_string());
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests;
