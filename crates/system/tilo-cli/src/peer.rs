//! This computer as a peer of the person's workspaces: each workspace's one
//! folder, at `~/<workspace>` by default, kept in sync both ways.
//!
//! The device `tilo login` made holds one session with the workspaces' host
//! ([`crate::mcp::held`]): the same session every `tilo mcp` of this computer
//! is a connection to. Over it this device holds each workspace's file tree,
//! the Files app's record ([`tree`]), with the bytes its files are made of
//! ([`content`]), and shows it as a folder ([`mount`]): a FUSE mount of the
//! filesystem Tilo OS mounts at `/var/workspace`, where this user can mount
//! one. Which workspaces get a folder, where, and whether it syncs is the
//! person's ([`folders`], `tilo folders`).
//!
//! So `cp`, `ls`, `grep`, an editor and an agent work on the workspace's real
//! files, each write is named for who made it ([`tilo_fs::writer`]), and what
//! lands in the folder is on the person's other devices and page within a
//! second or two.

pub(crate) mod content;
pub(crate) mod folders;
pub(crate) mod mount;
pub(crate) mod tree;

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde::Serialize;
use tilo_server_protocol::WorkspaceId;
use tilo_server_session::workspaces::PersonalList;
use tilo_session_client::device::Device as Entered;

use crate::mcp::held::UNMOUNTING;
use tree::FileBytes;

/// One workspace, as this peer shows it.
struct Held {
    workspace: WorkspaceId,
    name: String,
    /// What the workspace takes from this device's writes ([`tilo_fs::admission`]).
    admission: Arc<tilo_fs::admission::FileAdmission>,
    showing: mount::Showing,
    replica: tree::TreeReplica,
    _content: Arc<content::SessionContent>,
}

/// What `tilo status` and `tilo folders` say of one workspace's folder.
#[derive(Clone, Debug, Serialize)]
pub(crate) struct Mounted {
    pub(crate) workspace: String,
    pub(crate) name: String,
    pub(crate) at: PathBuf,
    pub(crate) shown: mount::Shown,
    pub(crate) read_only: bool,
    /// Bytes of its files this computer holds.
    pub(crate) local_bytes: u64,
    /// The device's session with the workspace is live: what the folder's status says of what was
    /// sent holds only while it is, and a folder with none is not in sync, whatever it last sent.
    pub(crate) connected: bool,
    #[serde(flatten)]
    pub(crate) status: tree::TreeStatus,
}

/// A workspace of the person's, as their list names it.
#[derive(Clone, Debug)]
struct Listed {
    workspace: WorkspaceId,
    /// `None` until the person's list named it: a folder is called after
    /// its workspace's name, never its id.
    name: Option<String>,
    role: Option<String>,
}

/// The workspaces this computer holds the files of, while this lives.
pub(crate) struct Peer {
    held: tokio::sync::Mutex<Vec<Held>>,
    store: Arc<tilo_fs::ContentStore>,
    endpoint: iroh::Endpoint,
    writers: Arc<tilo_fs::writer::WriterResolver>,
    home: PathBuf,
    /// The device's directory, where [`folders`] are kept.
    device_dir: PathBuf,
    state: PathBuf,
    /// Whether to mount with FUSE where this user can.
    mount: bool,
    /// The person's workspaces as their host last sent them, and whether it
    /// answered the whole list: a list that did not answer holds only the
    /// workspace logged into, and nothing may be dropped because of it.
    listed: tokio::sync::Mutex<Option<(Vec<Listed>, bool)>>,
    /// Why a workspace is not shown, by workspace.
    failed: std::sync::Mutex<Vec<(String, String)>>,
}

impl Peer {
    /// A peer keeping its files' state under `device_dir` and showing them
    /// below `home`. Must be called on a tokio runtime.
    ///
    /// # Errors
    /// The content store cannot be opened.
    pub(crate) async fn open(device_dir: &Path, home: &Path, mount: bool) -> anyhow::Result<Self> {
        let files = device_dir.join("files");
        std::fs::create_dir_all(&files)?;
        let store = Arc::new(tilo_fs::ContentStore::open(files.join("content")).await?);
        // The filesystem's hydrator dials peers of a sync protocol this device has
        // none of: everything it lacks comes over the session. The endpoint
        // binds loopback alone and reaches nothing by itself.
        let endpoint = iroh::Endpoint::builder(iroh::endpoint::presets::Minimal)
            .relay_mode(iroh::RelayMode::Disabled)
            .clear_ip_transports()
            .bind_addr_with_opts(
                std::net::SocketAddr::from((std::net::Ipv4Addr::LOCALHOST, 0)),
                iroh::endpoint::BindOpts::default(),
            )?
            .bind()
            .await?;
        Ok(Self {
            held: tokio::sync::Mutex::new(Vec::new()),
            store,
            endpoint,
            writers: Arc::new(tilo_fs::writer::WriterResolver::default()),
            home: home.to_path_buf(),
            device_dir: device_dir.to_path_buf(),
            state: files,
            mount,
            listed: tokio::sync::Mutex::new(None),
            failed: std::sync::Mutex::new(Vec::new()),
        })
    }

    /// The person's workspaces on the host `device` entered: the list their
    /// host sent ([`Self::keep_personal`]), or the one asked for once while
    /// none arrived. What was listed, and whether the host answered the whole
    /// list.
    async fn listed(&self, device: &Entered) -> (Vec<Listed>, bool) {
        let held = self.listed.lock().await.clone();
        if let Some((list, complete)) = held {
            return (list, complete);
        }
        let (list, complete) = match device
            .personal_list(
                &tilo_server_session::workspaces::WorkspaceLabels::new(),
                None,
            )
            .await
        {
            Ok(personal) => (Self::listed_from(device.workspace_id(), &personal), true),
            Err(refusal) => {
                tracing::warn!(%refusal, "the person's workspaces are not listed; only the one logged into is shown");
                (
                    vec![Listed {
                        workspace: device.workspace_id(),
                        name: None,
                        role: None,
                    }],
                    false,
                )
            }
        };
        *self.listed.lock().await = Some((list.clone(), complete));
        (list, complete)
    }

    /// A session has just started: nothing has been sent over it yet, so the
    /// list it carries is asked for once, and a workspace made while this
    /// computer was away is not missed.
    pub(crate) async fn forget_personal(&self) {
        *self.listed.lock().await = None;
    }

    /// The host sent the person's workspaces as they are now: their folders
    /// follow on the peer's next look, without asking the host again.
    pub(crate) async fn keep_personal(&self, device: &Entered, personal: &PersonalList) {
        *self.listed.lock().await =
            Some((Self::listed_from(device.workspace_id(), personal), true));
    }

    /// `personal`, as the peer lists it: the workspace this device entered
    /// first, so its folder exists before any list names it, then each
    /// workspace the host named.
    fn listed_from(entered: WorkspaceId, personal: &PersonalList) -> Vec<Listed> {
        let mut list = vec![Listed {
            workspace: entered,
            name: None,
            role: None,
        }];
        for entry in &personal.workspaces {
            let Some(workspace) = WorkspaceId::parse(&entry.workspace) else {
                continue;
            };
            let item = Listed {
                workspace,
                name: Some(entry.name.clone()).filter(|name| !name.trim().is_empty()),
                role: entry.role.clone(),
            };
            match list.iter_mut().find(|known| known.workspace == workspace) {
                Some(known) => *known = item,
                None => list.push(item),
            }
        }
        list
    }

    /// Show the folders [`folders`] names, and no other: offer a folder to
    /// each workspace that has none yet, then mount, move, pause and remove as
    /// the person set them.
    pub(crate) async fn reconcile(&self, device: &Entered) {
        let (listed, complete) = self.listed(device).await;
        let mut wanted = match folders::Folders::load(&self.device_dir) {
            Ok(wanted) => wanted,
            Err(error) => {
                tracing::warn!(%error, "the synced folders' settings are unreadable; nothing changes");
                return;
            }
        };
        let mut changed = false;
        // A workspace the person's list no longer has is gone, and its folder's
        // place with it: nothing to show, and nothing `tilo folders` should
        // name. Only a list the host answered says that; one that answered
        // nothing means the host could not be reached, not that the rest of
        // the person's workspaces went.
        if complete {
            let ids: Vec<String> = listed
                .iter()
                .map(|entry| entry.workspace.to_string())
                .collect();
            changed |= wanted.forget_unlisted(&ids);
        }
        let mut moves = Vec::new();
        for entry in &listed {
            let Some(name) = entry.name.as_deref() else {
                continue;
            };
            let id = entry.workspace.to_string();
            let own = entry.role.as_deref().is_none_or(|role| role == "owner");
            changed |= wanted.offer(&id, name, own, &self.home);
            if wanted
                .workspaces
                .get(&id)
                .is_some_and(|folder| folder.name != name)
            {
                changed = true;
                moves.extend(wanted.follow_rename(&id, name, &self.home));
            }
        }
        if changed && let Err(error) = wanted.save(&self.device_dir) {
            tracing::warn!(%error, "the synced folders' settings are not kept");
        }
        // Removed, moved, or no longer the person's: unmounted.
        self.held.lock().await.retain(|shown| {
            let id = shown.workspace.to_string();
            let still = listed
                .iter()
                .any(|entry| entry.workspace == shown.workspace);
            wanted
                .workspaces
                .get(&id)
                .is_some_and(|folder| still && !folder.removed && folder.path == shown.showing.at)
        });
        // A renamed workspace's folder, unmounted above, goes to its new name
        // with what it holds (a synced folder's copies, or nothing where it was
        // a mount point).
        for (old, new) in moves {
            if std::fs::symlink_metadata(&new).is_err()
                && let Err(error) = std::fs::rename(&old, &new)
            {
                tracing::warn!(%error, from = %old.display(), to = %new.display(), "the renamed workspace's folder did not move; it is shown at its new name");
            }
        }
        self.failed.lock().map(|mut failed| failed.clear()).ok();
        for entry in &listed {
            let id = entry.workspace.to_string();
            let Some(folder) = wanted.workspaces.get(&id).filter(|folder| !folder.removed) else {
                if entry.name.is_none() {
                    self.fail(
                        entry.workspace,
                        "its folder waits for the workspace's name, which the person's workspace list has not given yet",
                    );
                }
                continue;
            };
            let shown = self
                .held
                .lock()
                .await
                .iter_mut()
                .find(|shown| shown.workspace == entry.workspace)
                .map(|shown| {
                    shown.replica.set_paused(folder.paused);
                    shown.name.clone_from(&folder.name);
                })
                .is_some();
            if shown {
                continue;
            }
            if entry.workspace != device.workspace_id()
                && let Err(refusal) = device.enter_workspace(&id, "").await
            {
                self.fail(entry.workspace, &format!("not entered: {refusal}"));
                continue;
            }
            match self.hold(device, entry, (&folder.name, &folder.path)).await {
                Ok(shown) => {
                    shown.replica.set_paused(folder.paused);
                    self.held.lock().await.push(shown);
                }
                Err(error) => self.fail(entry.workspace, &format!("{error:#}")),
            }
        }
    }

    fn fail(&self, workspace: WorkspaceId, why: &str) {
        tracing::warn!(%workspace, why, "a workspace's files are not shown here");
        if let Ok(mut failed) = self.failed.lock() {
            failed.push((workspace.to_string(), why.to_owned()));
        }
    }

    /// Hold `entry`'s files at `at`.
    async fn hold(
        &self,
        device: &Entered,
        entry: &Listed,
        (folder_name, at): (&str, &Path),
    ) -> anyhow::Result<Held> {
        let workspace = entry.workspace;
        let link = device.session().clone();
        // Every write is the workspace's to take, as its policy reads on this device now: a
        // role changed or a grant taken back answers at the next write, never at mount time.
        let engine = Arc::clone(link.engine());
        let admission = Arc::new(
            tilo_fs::admission::FileAdmission::new(
                &hex::encode(device.cert().user_public_key),
                Box::new(move || engine.policy(&workspace.to_string()).ok()),
            )
            .ok_or_else(|| anyhow::anyhow!("this device's person has no valid key"))?,
        );
        let key = device.cert().device_public_key;
        let content = content::SessionContent::new(workspace, key, &link, Arc::clone(&self.store));
        let state = self.state.join(workspace.to_string());
        let replica =
            tree::TreeReplica::start(link, workspace, &state, key, Arc::clone(&content)).await?;
        let actor = fs_protocol::FileActor {
            person: collab_types::Ed25519PublicKey(device.cert().user_public_key),
            device: collab_types::Ed25519PublicKey(key),
        };
        let engine = Arc::clone(device.session().engine());
        let route = tilo_session_client::DocumentRoute {
            cluster_id: workspace.to_string(),
            app_id: fs_protocol::tree::FILES_APP.to_owned(),
            document: fs_protocol::tree::document_id(),
        };
        let keys = tilo_fs::hydration::FileKeys::from_session(workspace, move || {
            use tilo_session_client::engine::SealKeys;
            anyhow::ensure!(
                engine.document_read_policy(workspace)(&route.app_id, &route.document),
                "this device cannot read the workspace's files"
            );
            match engine
                .document_seal_keys(&route)
                .map_err(anyhow::Error::msg)?
            {
                SealKeys::Here(keys) => {
                    let keys = if keys.workspace() != workspace
                        && let Ok(SealKeys::Here(history)) = engine.seal_keys(workspace)
                    {
                        keys.with_workspace_history(history)
                    } else {
                        keys
                    };
                    Ok(keys)
                }
                _ => anyhow::bail!("the Files content keys are not ready"),
            }
        });
        let parts = mount::Parts {
            keys,
            replica: &replica,
            store: Arc::clone(&self.store),
            endpoint: self.endpoint.clone(),
            content: Arc::clone(&content),
            actor,
            writers: Arc::clone(&self.writers),
            admission: Arc::clone(&admission),
            state: &state,
        };
        let showing = mount::show(
            || mount::filesystem(&parts),
            at,
            &state.join("folder"),
            self.mount,
        )?;
        Ok(Held {
            workspace,
            name: folder_name.to_owned(),
            admission,
            showing,
            replica,
            _content: content,
        })
    }

    /// What this peer shows, and where each stands.
    ///
    /// Answered from what the sync last counted, never by reading the tree: a folder being
    /// written into in bulk has a step over its whole tree under the tree's lock, and a status
    /// ask waits behind nothing.
    pub(crate) async fn mounted(&self) -> Vec<Mounted> {
        let shown: Vec<(Mounted, Vec<FileBytes>)> = self
            .held
            .lock()
            .await
            .iter()
            .map(|shown| {
                let mounted = Mounted {
                    workspace: shown.workspace.to_string(),
                    name: shown.name.clone(),
                    at: shown.showing.at.clone(),
                    shown: shown.showing.shown,
                    read_only: !shown.admission.admits(
                        None,
                        None,
                        fs_protocol::file_action::verbs::WRITE,
                    ),
                    local_bytes: 0,
                    connected: true,
                    status: shown.replica.status(),
                };
                (mounted, shown.replica.files())
            })
            .collect();
        let mut mounted = Vec::with_capacity(shown.len());
        for (mut folder, hashes) in shown {
            for (hash, size) in hashes {
                let here = match hash {
                    Some(hash) => self.store.has(hash).await,
                    None => true,
                };
                if here {
                    folder.local_bytes = folder.local_bytes.saturating_add(size);
                }
            }
            mounted.push(folder);
        }
        mounted
    }

    /// The person's workspaces as last listed: id, name and role, and whether
    /// the host answered the whole list.
    pub(crate) async fn workspaces(&self) -> (Vec<serde_json::Value>, bool) {
        let listed = self.listed.lock().await;
        let complete = listed.as_ref().is_some_and(|(_, complete)| *complete);
        let rows = listed.as_ref().map_or_else(Vec::new, |(list, _)| {
            list.iter()
                .map(|entry| {
                    serde_json::json!({
                        "workspace": entry.workspace.to_string(),
                        "name": entry.name,
                        "role": entry.role,
                    })
                })
                .collect()
        });
        drop(listed);
        (rows, complete)
    }

    /// Why a workspace is not shown, by workspace.
    pub(crate) fn failures(&self) -> Vec<(String, String)> {
        self.failed
            .lock()
            .map(|failed| failed.clone())
            .unwrap_or_default()
    }

    /// Unmount everything, so no write lands after the session ends.
    pub(crate) async fn close(&self) {
        self.held.lock().await.clear();
    }

    /// Why this peer should not end now, if it should not: a folder is answering requests or took
    /// one within [`QUIET`] (a checkout writing thousands of files, a `git fsck` reading packs),
    /// or changes made here have not all reached the workspace yet. A holder asked to hand the
    /// device over to a newer Tilo keeps it while this says why: ending would answer the
    /// requests under way with I/O errors.
    pub(crate) async fn busy(&self) -> Option<String> {
        let folders: Vec<_> = self
            .held
            .lock()
            .await
            .iter()
            .map(|shown| {
                (
                    shown.showing.at.clone(),
                    Arc::clone(&shown.showing.activity),
                    shown.replica.status(),
                )
            })
            .collect();
        for (at, activity, status) in folders {
            if !activity.quiet(QUIET) {
                return Some(format!(
                    "{} is in use ({} requests under way, the last {} ms ago)",
                    at.display(),
                    activity.in_flight(),
                    activity.quiet_for().as_millis()
                ));
            }
            if !status.paused && status.opened && !(status.sent && status.durable) {
                return Some(format!(
                    "changes made in {} are still going out to the workspace",
                    at.display()
                ));
            }
        }
        None
    }

    /// Stop showing the folders and keep their trees on disk: called when the holder stops, so
    /// no write lands after the session ends and nothing made here is lost with the process.
    /// Each step is bounded, and all of them together by `within`: a step still walking a tree
    /// cannot be interrupted, and a stop ends inside the service's timeout whether or not one
    /// is running.
    pub(crate) async fn stop(&self, within: Duration) {
        let held: Vec<Held> = std::mem::take(&mut *self.held.lock().await);
        let deadline = Instant::now()
            .checked_add(within)
            .unwrap_or_else(Instant::now);
        // Each folder's requests under way are answered first, and the folder given a moment of
        // quiet: unmounting in the middle of one answers it with an I/O error (a program reading
        // a mapped file dies of SIGBUS). A folder kept busy past the bound is unmounted anyway,
        // and said so.
        let draining = deadline
            .saturating_duration_since(Instant::now())
            .min(DRAINING);
        let drained = drain(held.iter().map(|shown| &shown.showing), draining).await;
        for shown in held {
            let Held {
                showing, replica, ..
            } = shown;
            if !drained {
                tracing::error!(
                    at = %showing.at.display(),
                    in_flight = showing.activity.in_flight(),
                    waited_ms = draining.as_millis(),
                    "a folder was still in use when the holder stopped; requests under way there \
                     fail with an I/O error"
                );
            }
            // Unmount first, so no write lands after the session ends; a mount whose thread is
            // inside a step on the tree is given up on rather than waited for.
            let unmounting = deadline
                .saturating_duration_since(Instant::now())
                .min(UNMOUNTING);
            if !drop_within(showing, unmounting).await {
                tracing::warn!(
                    "a folder did not unmount in time; the mounts are cleared when next shown"
                );
            }
            let keeping = deadline.saturating_duration_since(Instant::now());
            replica.stop(keeping).await;
        }
    }

    /// Let go of the files' store and this device's endpoint, so the next holder opens the store
    /// as soon as this process lets its lock go rather than when this process ends: the
    /// databases the store holds are locked until then, and that outlasts the holder's own
    /// lock. The endpoint says it is going rather than being dropped mid-flight: iroh aborts
    /// every connection of an endpoint dropped without this, which leaves the server waiting out
    /// a silence.
    pub(crate) async fn shutdown(&self, within: Duration) {
        if let Err(error) = self.store.shutdown().await {
            tracing::warn!(%error, "the workspaces' files did not close cleanly");
        }
        if tokio::time::timeout(within, self.endpoint.close())
            .await
            .is_err()
        {
            tracing::warn!("the device's endpoint did not close in time");
        }
    }
}

/// How long a folder must have taken no request for this peer to count it as quiet.
const QUIET: Duration = Duration::from_secs(5);
/// How long a stop waits for the folders' requests under way to be answered and for a moment of
/// quiet before it unmounts them: part of the stop's own bound, inside the unit's stop timeout.
pub(crate) const DRAINING: Duration = Duration::from_secs(10);
/// The quiet a stop waits for before it unmounts a folder.
const DRAINED: Duration = Duration::from_millis(300);

/// Wait until every folder of `showing` has no request under way and took none for [`DRAINED`],
/// at most `within`: whether they all did. How long it took is said either way.
async fn drain<'a>(showing: impl Iterator<Item = &'a mount::Showing>, within: Duration) -> bool {
    let activities: Vec<_> = showing.map(|shown| Arc::clone(&shown.activity)).collect();
    let started = Instant::now();
    let quiet = || activities.iter().all(|activity| activity.quiet(DRAINED));
    let waiting = async {
        while !quiet() {
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    };
    let drained = tokio::time::timeout(within, waiting).await.is_ok();
    let waited_ms = started.elapsed().as_millis();
    if drained {
        tracing::info!(
            waited_ms,
            "the folders' requests were answered before they unmount"
        );
    }
    drained
}

/// Drop `what` on the blocking pool, waiting at most `within` for it: unmounting a folder
/// waits on steps that cannot be interrupted, and a stop must end inside the service's timeout
/// whether or not one is still under way.
async fn drop_within<T: Send + 'static>(what: T, within: Duration) -> bool {
    tokio::time::timeout(within, tokio::task::spawn_blocking(move || drop(what)))
        .await
        .is_ok()
}

/// `n` bytes as a person reads them: one decimal in the largest unit that
/// holds a whole one.
pub(crate) fn size(n: u64) -> String {
    let units = [
        ("TB", 1_000_000_000_000_u64),
        ("GB", 1_000_000_000),
        ("MB", 1_000_000),
        ("KB", 1_000),
    ];
    units.iter().find(|(_, scale)| n >= *scale).map_or_else(
        || format!("{n} B"),
        |(unit, scale)| {
            let tenths = n.saturating_mul(10).checked_div(*scale).unwrap_or(0);
            format!("{}.{} {unit}", tenths / 10, tenths % 10)
        },
    )
}

/// What `tilo status` prints of the peer's answer to its status request.
pub(crate) fn describe(status: &serde_json::Value) -> String {
    use std::fmt::Write as _;
    let text = |key: &str| {
        status
            .get(key)
            .and_then(serde_json::Value::as_str)
            .unwrap_or("?")
    };
    let mut said = format!(
        "This computer is \"{}\" in Tilo, one of the person's computers, through {}.\n",
        text("device"),
        text("host"),
    );
    if let Some(why) = status.get("not_in").and_then(serde_json::Value::as_str) {
        let _ = writeln!(said, "{why}");
    }
    let mounted = status
        .get("mounted")
        .and_then(serde_json::Value::as_array)
        .cloned()
        .unwrap_or_default();
    if mounted.is_empty() {
        said.push_str("No workspace has a folder here yet.\n");
    }
    for workspace in &mounted {
        let _ = writeln!(said, "  {}", describe_folder(workspace));
    }
    for failed in status
        .get("failed")
        .and_then(serde_json::Value::as_array)
        .into_iter()
        .flatten()
    {
        let _ = writeln!(
            said,
            "  {} has no folder here: {}",
            failed
                .get("workspace")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("?"),
            failed
                .get("why")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("?"),
        );
    }
    said
}

/// One folder in a sentence: where, what state, and how much is here.
pub(crate) fn describe_folder(workspace: &serde_json::Value) -> String {
    let field = |key: &str| {
        workspace
            .get(key)
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false)
    };
    let number = |key: &str| {
        workspace
            .get(key)
            .and_then(serde_json::Value::as_u64)
            .unwrap_or(0)
    };
    let at = workspace
        .get("at")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("?");
    let name = workspace
        .get("name")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("?");
    // A peer that does not say is one that answers for its session itself.
    let connected = workspace
        .get("connected")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(true);
    let state = if field("paused") {
        "paused"
    } else if !connected {
        "not connected to the workspace; changes made here wait to go out"
    } else if !field("opened") {
        "waiting for the workspace's files"
    } else if field("sent") && field("durable") {
        "in sync"
    } else {
        "sending changes"
    };
    let read_only = if field("read_only") {
        ", read-only"
    } else {
        ""
    };
    format!(
        "{name}: {at} ({state}{read_only}; {} files, {} of {} on this computer)",
        number("files"),
        size(number("local_bytes")),
        size(number("bytes")),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use tilo_server_session::workspaces::{KeepingDeviceAddress, PersonalWorkspace};

    /// A folder whose device has no live session with the workspace is never "in sync": what it
    /// last sent says nothing of what is written there since.
    #[test]
    fn a_folder_without_a_session_is_not_in_sync() {
        let folder = |connected: bool| {
            serde_json::json!({"name": "Tilo", "at": "/home/ada/Tilo", "opened": true,
                "sent": true, "durable": true, "files": 1, "bytes": 1, "connected": connected})
        };
        assert!(describe_folder(&folder(true)).contains("(in sync;"));
        let offline = describe_folder(&folder(false));
        assert!(!offline.contains("in sync"), "{offline}");
        assert!(offline.contains("not connected"), "{offline}");
    }

    /// The host's list names each folder after its workspace, and the folder
    /// of the workspace this device entered exists before any list names it.
    #[test]
    fn a_list_sent_by_the_host_names_the_folders() {
        let entered = WorkspaceId::parse("11111111-1111-4111-8111-111111111111").unwrap();
        let second = "22222222-2222-4222-8222-222222222222";
        let personal = PersonalList {
            workspaces: vec![PersonalWorkspace {
                workspace: second.to_owned(),
                name: "Second".to_owned(),
                host: Some(KeepingDeviceAddress {
                    name: "tilo.test".to_owned(),
                    hint: None,
                }),
                role: Some("owner".to_owned()),
                labels: tilo_server_session::workspaces::WorkspaceLabels::default(),
                unattended: false,
            }],
            devices: Vec::new(),
        };
        let listed = Peer::listed_from(entered, &personal);
        assert_eq!(listed.len(), 2, "the workspace entered, then the named one");
        assert_eq!(listed[0].workspace, entered);
        assert_eq!(listed[0].name, None, "no list has named it yet");
        assert_eq!(listed[1].workspace, WorkspaceId::parse(second).unwrap());
        assert_eq!(listed[1].name.as_deref(), Some("Second"));
        assert_eq!(listed[1].role.as_deref(), Some("owner"));
    }

    /// A stop does not wait for a step that cannot be interrupted: unmounting a folder or
    /// keeping a tree whose step is still under way is given its budget and no more, rather
    /// than hanging until a repository's tree is walked and the holder being killed for it.
    #[test]
    fn a_stop_does_not_wait_for_a_step_that_cannot_be_interrupted() {
        /// What a mount or a tree is like to drop while one of its steps is under way.
        struct Walking;

        impl Drop for Walking {
            fn drop(&mut self) {
                std::thread::sleep(Duration::from_secs(1));
            }
        }

        let runtime = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .unwrap();
        let started = Instant::now();
        assert!(!runtime.block_on(drop_within(Walking, Duration::from_millis(100))));
        assert!(
            started.elapsed() < Duration::from_millis(800),
            "the stop waited {:?} for a step",
            started.elapsed()
        );
        assert!(
            runtime.block_on(drop_within((), Duration::from_secs(1))),
            "a step that ends is waited for"
        );
    }
}
