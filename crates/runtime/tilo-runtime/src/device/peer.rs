//! This computer as a peer of the person's workspaces: each workspace's one
//! folder, at `~/<workspace>` by default, served through the platform's file provider.
//!
//! The device `tilo login` made runs its own workspace service, which holds
//! each of the person's workspaces as a member copy ([`super::workspaces`]),
//! synced with the device that keeps it. The person's list of workspaces, and
//! where to enter one this device does not keep yet, come from their server's
//! account ([`super::account`]), which this peer follows ([`Peer::keep`]). In
//! that copy this device holds each workspace's
//! file tree, the Files app's record ([`tree`]), with the bytes its files are
//! made of ([`shelf`]), and shows it at a folder ([`mount`]): a FUSE mount, Linux's
//! file provider, on Tilo OS and every other distribution alike. Which
//! workspaces get a folder, where, and whether it syncs is the person's
//! ([`folders`], `tilo folders`).
//!
//! So `cp`, `ls`, `grep`, an editor and an agent work on the workspace's real
//! files, each write is named for who made it ([`tilo_fs::writer`]), and what
//! lands in the folder is on the person's other devices and page within a
//! second or two.

pub(crate) mod mount;

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde::Serialize;
use tilo_lock::MutexRecover as _;
use tilo_server_protocol::WorkspaceId;
use tilo_server_session::workspaces::PersonalList;
use tilo_workspace_service::files;

use tilo_api::daemon::folders;

use super::account::Account;
use super::serve::UNMOUNTING;
use super::workspaces::ServiceWorkspaces;

/// One workspace, as this peer shows it.
struct Held {
    workspace: WorkspaceId,
    name: String,
    /// What the workspace takes from this device's writes ([`tilo_fs::admission`]).
    admission: Arc<tilo_fs::admission::FileAdmission>,
    showing: mount::Showing,
    replica: tilo_fs::tree_sync::TreeSync,
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
    pub(crate) status: tilo_fs::tree_sync::Status,
}

/// A workspace of the person's, as their list names it.
#[derive(Clone, Debug)]
struct Listed {
    workspace: WorkspaceId,
    /// `None` until the person's list named it: a folder is called after
    /// its workspace's name, never its id.
    name: Option<String>,
    role: Option<String>,
    /// Where it is entered, when the list names it.
    entered_at: Option<tilo_peer_state::join::TransportHint>,
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
    /// The content of the files, routed by this device's own workspace service over `store`:
    /// made once that service is first at hand, and held for as long as this peer is, since the
    /// service keeps what answers its members only weakly.
    routed: std::sync::OnceLock<tilo_workspace_service::content::RouteContentService>,
    /// The person's workspaces as their server last answered them: until it
    /// has, the workspaces this device keeps, and nothing may be dropped
    /// because of that.
    listed: tokio::sync::Mutex<Option<Vec<Listed>>>,
    /// Whether the person's server answered the last time it was asked.
    connected: std::sync::atomic::AtomicBool,
    /// Why a workspace is not shown, by workspace.
    failed: std::sync::Mutex<Vec<(String, String)>>,
    /// The workspaces this device could not enter, and when each is tried again: a workspace
    /// whose server does not answer is not asked again at every look, each of which would wait
    /// for it while no folder is added, moved or removed.
    unentered: std::sync::Mutex<std::collections::HashMap<WorkspaceId, Unentered>>,
}

/// A workspace this device could not enter: why, how often in a row, and when it is tried again.
struct Unentered {
    why: String,
    tries: u32,
    next: Instant,
}

/// The longest one attempt to enter a workspace is waited for: its server's answer, and following
/// what it started, otherwise take minutes when the server does not answer.
const ENTERING_ONE: Duration = Duration::from_secs(30);
/// How long a workspace that could not be entered waits before it is tried again, the first
/// time: twice as long after each attempt in a row that failed, up to [`ENTERED_AT_MOST_EVERY`].
const ENTERED_AGAIN_AFTER: Duration = Duration::from_secs(30);
const ENTERED_AT_MOST_EVERY: Duration = Duration::from_secs(600);
/// How long a read waits for content the workspace is asked for.
const CONTENT_WAIT: Duration = Duration::from_secs(20);
/// How often the folders look at `tilo folders` again, and a server not reached is asked again.
const PEER_LOOK: Duration = Duration::from_secs(5);

/// The follow of the person's list, ended when it is let go.
struct Following(tokio::task::JoinHandle<()>);

impl Drop for Following {
    fn drop(&mut self) {
        self.0.abort();
    }
}

impl Peer {
    /// A peer keeping its files' state under `device_dir` and showing them
    /// below `home`. Must be called on a tokio runtime.
    ///
    /// # Errors
    /// The content store cannot be opened.
    pub(crate) async fn open(device_dir: &Path, home: &Path) -> anyhow::Result<Self> {
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
            routed: std::sync::OnceLock::new(),
            listed: tokio::sync::Mutex::new(None),
            connected: std::sync::atomic::AtomicBool::new(false),
            failed: std::sync::Mutex::new(Vec::new()),
            unentered: std::sync::Mutex::new(std::collections::HashMap::new()),
        })
    }

    /// The person's workspaces as their server last answered them, and whether it did: until it
    /// has, the workspaces `own` keeps, each with the name its governance gives it.
    async fn listed(&self, own: &ServiceWorkspaces) -> (Vec<Listed>, bool) {
        let answered = self.listed.lock().await.clone();
        if let Some(listed) = answered {
            return (listed, true);
        }
        let kept = own
            .held()
            .into_iter()
            .map(|(workspace, name, _)| Listed {
                workspace,
                name: Some(name).filter(|name| !name.trim().is_empty()),
                role: None,
                entered_at: None,
            })
            .collect();
        (kept, false)
    }

    /// Keep the folders of the person's workspaces as `own` holds them and `account`, their
    /// server's, lists them, for as long as this runs: each list the server answers is taken in
    /// as it comes and the folders follow it at once, and `tilo folders` within [`PEER_LOOK`]. A
    /// server not reached is asked again then, and the folders of what this device keeps stay
    /// shown meanwhile.
    pub(crate) async fn keep(self: Arc<Self>, own: Arc<ServiceWorkspaces>, account: Arc<Account>) {
        let woken = Arc::new(tokio::sync::Notify::new());
        // The follow ends with this, when the task keeping the folders is let go.
        let _following = Following(tokio::spawn(
            Arc::clone(&self).follow(Arc::clone(&account), Arc::clone(&woken)),
        ));
        loop {
            self.reconcile(&own, Some(&account)).await;
            tokio::select! {
                () = woken.notified() => {}
                () = tokio::time::sleep(PEER_LOOK) => {}
            }
        }
    }

    /// Take in each list `account` answers, waking `woken` when it moved.
    async fn follow(self: Arc<Self>, account: Arc<Account>, woken: Arc<tokio::sync::Notify>) {
        let mut version: Option<String> = None;
        loop {
            match account.list(version.as_deref()).await {
                Ok(listed) => {
                    self.connected
                        .store(true, std::sync::atomic::Ordering::Relaxed);
                    *super::serve::NOT_IN.lock_recover() = None;
                    if version.as_deref() != Some(listed.version.as_str()) {
                        *self.listed.lock().await = Some(Self::listed_from(&listed.list));
                        woken.notify_one();
                    }
                    version = Some(listed.version);
                }
                Err(why) => {
                    tracing::debug!(%why, "the person's workspaces are not listed now");
                    self.connected
                        .store(false, std::sync::atomic::Ordering::Relaxed);
                    *super::serve::NOT_IN.lock_recover() = Some(why);
                    tokio::time::sleep(PEER_LOOK).await;
                }
            }
        }
    }

    /// `personal`, as the peer lists it.
    fn listed_from(personal: &PersonalList) -> Vec<Listed> {
        personal
            .workspaces
            .iter()
            .filter_map(|entry| {
                Some(Listed {
                    workspace: WorkspaceId::parse(&entry.workspace)?,
                    name: Some(entry.name.clone()).filter(|name| !name.trim().is_empty()),
                    role: entry.role.clone(),
                    entered_at: entry.entered_at().ok(),
                })
            })
            .collect()
    }

    /// Show the folders [`folders`] names, and no other: offer a folder to
    /// each workspace that has none yet, then mount, move, pause and remove as
    /// the person set them. Each folder's tree is held in `own`, this device's
    /// own workspace service; a workspace it does not keep yet is entered
    /// where the list, or `account`, names.
    pub(crate) async fn reconcile(&self, own: &ServiceWorkspaces, account: Option<&Account>) {
        let (listed, complete) = self.listed(own).await;
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
            if let Err(why) = self.enter(own, account, entry).await {
                self.not_shown(entry.workspace, &why);
                continue;
            }
            match self.hold_in(own, entry, (&folder.name, &folder.path)).await {
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
        self.not_shown(workspace, why);
    }

    /// Note why `workspace` is not shown, for `tilo status`, without saying it in the log again.
    fn not_shown(&self, workspace: WorkspaceId, why: &str) {
        if let Ok(mut failed) = self.failed.lock() {
            failed.push((workspace.to_string(), why.to_owned()));
        }
    }

    /// Have `own` keep `entry`'s workspace, entering it as a member copy where the list, else
    /// `account`, names it is kept, waiting at most [`ENTERING_ONE`]; why not, when it was not
    /// entered. A workspace that could not be entered is tried again only once its wait is over,
    /// and until then answers why it was not.
    async fn enter(
        &self,
        own: &ServiceWorkspaces,
        account: Option<&Account>,
        entry: &Listed,
    ) -> Result<(), String> {
        let workspace = entry.workspace;
        if own.keeps(workspace).await {
            return Ok(());
        }
        let now = Instant::now();
        if let Some(unentered) = self
            .unentered
            .lock_recover()
            .get(&workspace)
            .filter(|unentered| now < unentered.next)
        {
            return Err(format!(
                "{}; tried again in {}s",
                unentered.why,
                unentered
                    .next
                    .saturating_duration_since(now)
                    .as_secs()
                    .max(1)
            ));
        }
        let entering = async {
            let hint = match (&entry.entered_at, account) {
                (Some(hint), _) => hint.clone(),
                (None, Some(account)) => account.address(workspace).await?,
                (None, None) => {
                    return Err("no server of the person's names where it is kept".to_owned());
                }
            };
            own.copies().hold_as_member(workspace, &hint).await
        };
        let why = match tokio::time::timeout(ENTERING_ONE, entering).await {
            Ok(Ok(_)) => {
                self.unentered.lock_recover().remove(&workspace);
                return Ok(());
            }
            Ok(Err(refusal)) => format!("not entered: {refusal}"),
            Err(_) => format!(
                "not entered: the workspace's server did not answer within {}s",
                ENTERING_ONE.as_secs()
            ),
        };
        let tries = self
            .unentered
            .lock_recover()
            .get(&workspace)
            .map_or(1, |unentered| unentered.tries.saturating_add(1));
        let wait = ENTERED_AGAIN_AFTER
            .saturating_mul(2_u32.saturating_pow(tries.saturating_sub(1).min(16)))
            .min(ENTERED_AT_MOST_EVERY);
        tracing::warn!(%workspace, why, tries, again_in = wait.as_secs(), "a workspace's files are not shown here");
        self.unentered.lock_recover().insert(
            workspace,
            Unentered {
                why: why.clone(),
                tries,
                next: Instant::now().checked_add(wait).unwrap_or(now),
            },
        );
        Err(why)
    }

    /// Hold `entry`'s files at `at`, their tree in `own`'s copy of the workspace.
    async fn hold_in(
        &self,
        own: &ServiceWorkspaces,
        entry: &Listed,
        (folder_name, at): (&str, &Path),
    ) -> anyhow::Result<Held> {
        let workspace = entry.workspace;
        // Every write is the workspace's to take, as its policy reads on this device's copy now: a
        // role changed or a grant taken back answers at the next write, never at mount time.
        let admission = Arc::new(
            tilo_fs::admission::FileAdmission::new(
                &hex::encode(own.device_cert().user_public_key),
                Box::new(own.policy_of(workspace)),
            )
            .ok_or_else(|| anyhow::anyhow!("this device's person has no valid key"))?,
        );
        let key = own.device_cert().device_public_key;
        let person = own.device_cert().user_public_key;
        let workspaces = own.copies();
        let routed = self.routed.get_or_init(|| {
            tilo_workspace_service::content::routes_content(
                workspaces,
                Arc::new(files::StoreShelf(Arc::clone(&self.store))),
                key,
            )
        });
        let state = self.state.join(workspace.to_string());
        let replica = files::start(
            workspaces,
            workspace,
            &state.join("tree"),
            (key, person),
            Some(Arc::clone(&self.store)),
            Some(Arc::clone(&routed.content)),
        )
        .await?;
        let actor = fs_protocol::FileActor {
            person: collab_types::Ed25519PublicKey(person),
            device: collab_types::Ed25519PublicKey(key),
        };
        // The keys the files' content is sealed with are the tree's group's, as this device's
        // key service holds them, with the workspace's history beside them.
        let route = tilo_peer_state::document_frame::DocumentRoute {
            cluster_id: workspace.to_string(),
            app_id: fs_protocol::tree::FILES_APP.to_owned(),
            document: fs_protocol::tree::document_id(),
        };
        let (copies, runtime) = (workspaces.clone(), tokio::runtime::Handle::current());
        let keys = tilo_fs::hydration::FileKeys::from_session(workspace, move || {
            match runtime.block_on(copies.own_route_keys(&route, false)) {
                Some(Ok(keys)) => Ok(keys),
                Some(Err(why)) => {
                    anyhow::bail!("this device cannot read the workspace's files: {why}")
                }
                None => anyhow::bail!("the Files content keys are not ready"),
            }
        });
        let content: Arc<dyn tilo_fs::hydration::ContentSource> =
            Arc::new(files::WorkspaceSource::new(
                Arc::clone(&routed.content),
                &workspace.to_string(),
                CONTENT_WAIT,
            ));
        let parts = mount::Parts {
            keys,
            replica: &replica,
            store: Arc::clone(&self.store),
            content,
            actor,
            writers: Arc::clone(&self.writers),
            admission: Arc::clone(&admission),
            state: &state,
        };
        let showing = mount::show(|| mount::filesystem(&parts), at, &state.join("folder"))?;
        Ok(Held {
            workspace,
            name: folder_name.to_owned(),
            admission,
            showing,
            replica,
        })
    }

    /// What this peer shows, and where each stands.
    ///
    /// Answered from what the sync last counted, never by reading the tree or the content store:
    /// a folder being written into in bulk has a step over its whole tree under the tree's lock
    /// and keeps the store writing, and a status ask waits behind neither.
    pub(crate) async fn mounted(&self) -> Vec<Mounted> {
        let connected = self.connected.load(std::sync::atomic::Ordering::Relaxed);
        self.held
            .lock()
            .await
            .iter()
            .map(|shown| {
                let status = shown.replica.status();
                Mounted {
                    workspace: shown.workspace.to_string(),
                    name: shown.name.clone(),
                    at: shown.showing.at.clone(),
                    shown: shown.showing.shown,
                    read_only: !shown.admission.admits(
                        None,
                        None,
                        fs_protocol::file_action::verbs::WRITE,
                    ),
                    local_bytes: status.local_bytes,
                    connected,
                    status,
                }
            })
            .collect()
    }

    /// The person's workspaces as last listed: id, name and role, and whether
    /// the host answered the whole list.
    pub(crate) async fn workspaces(&self) -> (Vec<serde_json::Value>, bool) {
        let listed = self.listed.lock().await;
        let complete = listed.is_some();
        let rows = listed.as_ref().map_or_else(Vec::new, |list| {
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

#[cfg(test)]
mod folder_tests;

#[cfg(test)]
mod tests {
    use super::*;
    use tilo_server_session::workspaces::{KeepingDeviceAddress, PersonalWorkspace};

    /// The server's list names each folder after its workspace.
    #[test]
    fn a_list_sent_by_the_host_names_the_folders() {
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
                enter_at: None,
                made_at_ms: None,
            }],
            devices: Vec::new(),
        };
        let listed = Peer::listed_from(&personal);
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].workspace, WorkspaceId::parse(second).unwrap());
        assert_eq!(listed[0].name.as_deref(), Some("Second"));
        assert_eq!(listed[0].role.as_deref(), Some("owner"));
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
