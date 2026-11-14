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
use tilo_relay_protocol::WorkspaceId;
use tilo_session_client::device::Device as Entered;

/// How often the person's workspace list is asked for again.
const LIST_AGAIN: Duration = Duration::from_secs(60);

/// One workspace, as this peer shows it.
struct Held {
    workspace: WorkspaceId,
    name: String,
    read_only: bool,
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
    #[serde(flatten)]
    pub(crate) standing: tree::Standing,
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
    listed: tokio::sync::Mutex<Option<(Instant, Vec<Listed>)>>,
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
        // The filesystem's hydrator dials peers of a sync plane this device has
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

    /// The person's workspaces on the host `device` entered, asked again
    /// once [`LIST_AGAIN`] passed.
    async fn listed(&self, device: &Entered) -> Vec<Listed> {
        let mut listed = self.listed.lock().await;
        if let Some((at, list)) = listed.as_ref()
            && at.elapsed() < LIST_AGAIN
        {
            return list.clone();
        }
        let mut list = vec![Listed {
            workspace: device.workspace_id(),
            name: None,
            role: None,
        }];
        match device
            .personal_list(
                &tilo_relay_session::workspaces::WorkspaceLabels::new(),
                None,
            )
            .await
        {
            Ok(personal) => {
                for entry in personal.workspaces {
                    let Some(workspace) = WorkspaceId::parse(&entry.workspace) else {
                        continue;
                    };
                    let item = Listed {
                        workspace,
                        name: Some(entry.name).filter(|name| !name.trim().is_empty()),
                        role: entry.role,
                    };
                    match list.iter_mut().find(|known| known.workspace == workspace) {
                        Some(known) => *known = item,
                        None => list.push(item),
                    }
                }
            }
            Err(refusal) => {
                tracing::warn!(%refusal, "the person's workspaces are not listed; only the one logged into is shown");
            }
        }
        *listed = Some((Instant::now(), list.clone()));
        list
    }

    /// Show the folders [`folders`] names, and no other: offer a folder to
    /// each workspace that has none yet, then mount, move, pause and remove as
    /// the person set them.
    pub(crate) async fn reconcile(&self, device: &Entered) {
        let listed = self.listed(device).await;
        let mut wanted = match folders::Folders::load(&self.device_dir) {
            Ok(wanted) => wanted,
            Err(error) => {
                tracing::warn!(%error, "the synced folders' settings are unreadable; nothing changes");
                return;
            }
        };
        let mut changed = false;
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
                && let Err(refusal) = device.enter_workspace(&id, device.label()).await
            {
                self.fail(entry.workspace, &format!("not entered: {refusal}"));
                continue;
            }
            let read_only = entry
                .role
                .as_deref()
                .is_some_and(|role| matches!(role, "viewer" | "commenter"));
            match self.hold(device, entry, (&folder.name, &folder.path), read_only) {
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
    fn hold(
        &self,
        device: &Entered,
        entry: &Listed,
        (folder_name, at): (&str, &Path),
        read_only: bool,
    ) -> anyhow::Result<Held> {
        let workspace = entry.workspace;
        let link = device.session().clone();
        let key = device.cert().device_public_key;
        let content = content::SessionContent::new(workspace, key, &link, Arc::clone(&self.store));
        let state = self.state.join(workspace.to_string());
        let replica = tree::TreeReplica::start(link, workspace, &state, key, Arc::clone(&content))?;
        let actor = fs_protocol::FileActor {
            person: collab_types::Ed25519PublicKey(device.cert().user_public_key),
            device: collab_types::Ed25519PublicKey(key),
        };
        let parts = mount::Parts {
            replica: &replica,
            store: Arc::clone(&self.store),
            endpoint: self.endpoint.clone(),
            content: Arc::clone(&content),
            actor,
            writers: Arc::clone(&self.writers),
            state: &state,
        };
        let showing = mount::show(
            || mount::filesystem(&parts),
            at,
            &state.join("folder"),
            self.mount,
            read_only,
        )?;
        Ok(Held {
            workspace,
            name: folder_name.to_owned(),
            read_only,
            showing,
            replica,
            _content: content,
        })
    }

    /// What this peer shows, and where each stands.
    pub(crate) async fn mounted(&self) -> Vec<Mounted> {
        let shown: Vec<(Mounted, Vec<(iroh_blobs::Hash, u64)>)> = self
            .held
            .lock()
            .await
            .iter()
            .map(|shown| {
                let namespace = shown
                    .replica
                    .namespace
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                let hashes = namespace
                    .live_node_ids()
                    .iter()
                    .filter_map(|id| namespace.entry_for_node(id))
                    .filter_map(|entry| Some((entry.hash?, entry.size)))
                    .collect();
                drop(namespace);
                let mounted = Mounted {
                    workspace: shown.workspace.to_string(),
                    name: shown.name.clone(),
                    at: shown.showing.at.clone(),
                    shown: shown.showing.shown,
                    read_only: shown.read_only,
                    local_bytes: 0,
                    standing: shown.replica.standing(),
                };
                (mounted, hashes)
            })
            .collect();
        let mut mounted = Vec::with_capacity(shown.len());
        for (mut folder, hashes) in shown {
            for (hash, size) in hashes {
                if self.store.has(hash).await {
                    folder.local_bytes = folder.local_bytes.saturating_add(size);
                }
            }
            mounted.push(folder);
        }
        mounted
    }

    /// The person's workspaces as last listed: id, name and role.
    pub(crate) async fn workspaces(&self) -> Vec<serde_json::Value> {
        self.listed
            .lock()
            .await
            .as_ref()
            .map_or_else(Vec::new, |(_, list)| {
                list.iter()
                    .map(|entry| {
                        serde_json::json!({
                            "workspace": entry.workspace.to_string(),
                            "name": entry.name,
                            "role": entry.role,
                        })
                    })
                    .collect()
            })
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
        "This computer is {} in Tilo, a device of person {}…, through {}.\n",
        text("device"),
        text("person").get(..12).unwrap_or("?"),
        text("host"),
    );
    if let Some(why) = status.get("not_in").and_then(serde_json::Value::as_str) {
        let _ = writeln!(said, "Not in the workspace: {why}");
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
    let state = if field("paused") {
        "paused"
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
