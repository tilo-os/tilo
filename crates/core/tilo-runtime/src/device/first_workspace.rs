//! The person's workspace on this computer: the one its login names, or, on a computer with no
//! login, the one this device hosts for its person. That one is not founded when the daemon
//! starts: the person's desktop may still hand over the workspace it kept
//! (`account/workspaces#import`), which then is theirs here. It is founded when the person first
//! needs one and nothing came in for them, at the id the person's key names (the one every device
//! of theirs enters), and named in the device's directory from then on.

use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock, Weak};

use tilo_desktop_model::WorkspaceId;

use super::take_work::files::Replicas;
use super::workspaces::ServiceWorkspaces;

/// Where a computer with no login keeps the id of its person's workspace, in the device's
/// directory: this, then the person's key in hex. A root that comes to this computer later
/// founds a workspace of its own, never enters one another person founded here.
pub(crate) const FOUNDED: &str = "workspace-of-";
/// What the workspace founded on a computer with no login is called.
pub(crate) const FOUNDED_NAME: &str = "Home";

/// What the daemon does for its person once their workspace is here: what its start does for a
/// person whose workspace it already holds.
pub(crate) type Settling =
    Arc<dyn Fn() -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send>> + Send + Sync>;

/// The person's workspace on this computer, once there is one.
pub(crate) struct FirstWorkspace {
    /// The file naming it on a computer with no login; `None` with a login, which names it.
    file: Option<PathBuf>,
    id: tokio::sync::OnceCell<String>,
    workspaces: Arc<ServiceWorkspaces>,
    /// Where the files of the workspaces this computer hosts are shown, once they are.
    replicas: OnceLock<Weak<Replicas>>,
    /// What the daemon does once the workspace is here ([`Self::when_here`]).
    settling: OnceLock<Settling>,
    /// Whether that was done, once the workspace was here.
    settled: tokio::sync::OnceCell<()>,
}

impl FirstWorkspace {
    /// The workspace `login` names, or, with no login, the one named in the device's directory
    /// `dir` for `person` when an earlier start had one.
    ///
    /// # Errors
    /// The file naming it cannot be read, or names no workspace.
    pub(crate) fn open(
        workspaces: Arc<ServiceWorkspaces>,
        login: Option<&str>,
        dir: &Path,
        person: &[u8; 32],
    ) -> std::io::Result<Arc<Self>> {
        let file = login
            .is_none()
            .then(|| dir.join(format!("{FOUNDED}{}", hex::encode(person))));
        let named = match (login, &file) {
            (Some(login), _) => Some(login.to_owned()),
            (None, Some(file)) => match std::fs::read_to_string(file) {
                Ok(kept) => Some(
                    WorkspaceId::parse(kept.trim())
                        .ok_or_else(|| {
                            std::io::Error::new(
                                std::io::ErrorKind::InvalidData,
                                format!("{} names no workspace", file.display()),
                            )
                        })?
                        .to_string(),
                ),
                // A start that founded it before it was named here holds it all the same.
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                    let own = own_workspace(person);
                    let kept = Arc::clone(&workspaces);
                    super::workspaces::block_on_owned(async move { kept.keeps(own).await })
                        .then(|| own.to_string())
                }
                Err(error) => return Err(error),
            },
            (None, None) => None,
        };
        // A workspace here when the daemon starts is settled by the start itself.
        let settled = if named.is_some() {
            tokio::sync::OnceCell::new_with(Some(()))
        } else {
            tokio::sync::OnceCell::new()
        };
        Ok(Arc::new(Self {
            file,
            id: named.map_or_else(tokio::sync::OnceCell::new, |named| {
                tokio::sync::OnceCell::new_with(Some(named))
            }),
            workspaces,
            replicas: OnceLock::new(),
            settling: OnceLock::new(),
            settled,
        }))
    }

    /// The workspace `workspace`, founded earlier on a computer with no login.
    #[cfg(test)]
    pub(crate) fn founded(workspaces: Arc<ServiceWorkspaces>, workspace: WorkspaceId) -> Arc<Self> {
        Arc::new(Self {
            file: Some(PathBuf::from(FOUNDED)),
            id: tokio::sync::OnceCell::new_with(Some(workspace.to_string())),
            workspaces,
            replicas: OnceLock::new(),
            settling: OnceLock::new(),
            settled: tokio::sync::OnceCell::new_with(Some(())),
        })
    }

    /// Show the files of the workspace this computer founds, once it does, in the files daemon's trees.
    pub(crate) fn show_files_in(&self, replicas: &Arc<Replicas>) {
        let _ = self.replicas.set(Arc::downgrade(replicas));
    }

    /// Have `settling` done once the person's workspace is here: when it is founded, and when
    /// whatever first reaches it finds it came in from their desktop ([`Self::settle`]). A
    /// workspace here when the daemon started is the start's own to settle.
    pub(crate) fn when_here(&self, settling: Settling) {
        let _ = self.settling.set(settling);
    }

    /// Do once what the daemon does for its person once their workspace is here
    /// ([`Self::when_here`]): this device listed with their others before anything signs as it,
    /// their root marked, and their root-only asks served. Nothing while there is no workspace.
    pub(crate) async fn settle(&self) {
        let (Some(_), Some(settling)) = (self.get(), self.settling.get().cloned()) else {
            return;
        };
        self.settled.get_or_init(|| settling()).await;
    }

    /// [`Self::settle`], from a caller that does not run on the runtime.
    pub(crate) fn settle_blocking(self: &Arc<Self>) {
        if self.settled.initialized() {
            return;
        }
        let first = Arc::clone(self);
        super::workspaces::block_on_owned(async move { first.settle().await });
    }

    /// Whether this computer founds its person's workspace itself: it has no login.
    pub(crate) fn founds(&self) -> bool {
        self.file.is_some()
    }

    /// The person's workspace, when there is one yet.
    pub(crate) fn get(&self) -> Option<&str> {
        self.id.get().map(String::as_str)
    }

    /// The person's workspace, founded now when there is none yet.
    ///
    /// # Errors
    /// The service did not found it, or its name cannot be kept.
    pub(crate) async fn found(&self) -> Result<String, String> {
        let founded = self
            .id
            .get_or_try_init(|| async {
                let Some(file) = &self.file else {
                    return Err("this computer's login names no workspace".to_owned());
                };
                let workspace = own_workspace(&self.workspaces.device_cert().user_public_key);
                self.workspaces.found(workspace, FOUNDED_NAME).await?;
                self.name(file, workspace)?;
                let _ = self.show(&[workspace]).await;
                Ok(workspace.to_string())
            })
            .await
            .cloned()?;
        // Founded now or before: this device is listed with its person's, and their root marked,
        // once it holds the workspace — one write, from [`Self::settle`].
        self.settle().await;
        Ok(founded)
    }

    /// [`Self::found`], from a caller that does not run on the runtime.
    ///
    /// # Errors
    /// As [`Self::found`].
    pub(crate) fn found_blocking(self: &Arc<Self>) -> Result<String, String> {
        if let Some(id) = self.get() {
            // One that came in from the person's desktop is settled by the first that reaches it.
            self.settle_blocking();
            return Ok(id.to_owned());
        }
        let first = Arc::clone(self);
        super::workspaces::block_on_owned(async move { first.found().await })
    }

    /// Found the person's workspace when `workspace` names it and this computer founds it but has
    /// none yet: what a call of theirs in it does the first time they need one. One that came in
    /// from their desktop first is theirs here, and is settled instead ([`Self::settle`]). Any
    /// other workspace is left as it is.
    ///
    /// # Errors
    /// As [`Self::found`].
    pub(crate) fn found_if_theirs(self: &Arc<Self>, workspace: &str) -> Result<(), String> {
        if !self.founds()
            || workspace
                != own_workspace(&self.workspaces.device_cert().user_public_key).to_string()
        {
            return Ok(());
        }
        self.found_blocking().map(drop)
    }

    /// The workspaces `moved` came in from the person's desktop: the person's own among them is
    /// their workspace here, when they have none yet, and every one's files are shown.
    ///
    /// # Errors
    /// The name of the person's workspace cannot be kept.
    pub(crate) fn moved_in(&self, moved: &[WorkspaceId]) -> Result<(), String> {
        if let Some(file) = &self.file {
            let person = &self.workspaces.device_cert().user_public_key;
            let person = tilo_server_protocol::servers::person_workspace_id(person);
            if let Some(workspace) = moved
                .iter()
                .find(|workspace| workspace.to_string() == person)
                && self.id.get().is_none()
            {
                self.name(file, *workspace)?;
                let _ = self.id.set(person);
            }
            // The import that hands them over runs on the runtime; they show once started.
            drop(self.show(moved));
        }
        Ok(())
    }

    fn name(&self, file: &Path, workspace: WorkspaceId) -> Result<(), String> {
        tilo_files::native::atomic::write_with_mode(file, workspace.to_string().as_bytes(), 0o600)
            .map_err(|error| format!("{}: {error}", file.display()))?;
        // What waits for a workspace to be kept here (host search) looks again.
        self.workspaces.entered().notify_waiters();
        Ok(())
    }

    /// Show the files of `workspaces` that are held here, on a thread of its own: starting a
    /// workspace's files reads its tree through the hydrator's synchronous bridge, which a task of
    /// the runtime (founding runs on one) must not wait on. Answers once they are shown.
    fn show(&self, workspaces: &[WorkspaceId]) -> tokio::sync::oneshot::Receiver<()> {
        let (shown, answer) = tokio::sync::oneshot::channel();
        let Some(replicas) = self.replicas.get().and_then(Weak::upgrade) else {
            let _ = shown.send(());
            return answer;
        };
        let held: Vec<_> = self
            .workspaces
            .held()
            .into_iter()
            .filter(|(workspace, _, _)| workspaces.contains(workspace))
            .map(|(workspace, name, _)| (workspace, name))
            .collect();
        if let Err(error) = std::thread::Builder::new()
            .name("tilo-files-show".into())
            .spawn(move || {
                for (workspace, name) in held {
                    let _ = super::services::hold_files(&replicas, &name, workspace);
                }
                let _ = shown.send(());
            })
        {
            tracing::warn!(%error, "a thread to show a workspace's files did not start");
        }
        answer
    }
}

/// `person`'s own workspace, at the id their key names.
pub(super) fn own_workspace(person: &[u8; 32]) -> WorkspaceId {
    WorkspaceId::from(collab_types::person_workspace(person))
}

/// What the daemon's start does for a person whose workspace it holds, done where their workspace
/// comes to be held later: `cert`, this device, listed with their others in the devices record
/// of their workspace, and the root this account keeps marked there.
pub(super) async fn hold_person(
    copies: &tilo_workspace_service::workspaces::Workspaces,
    cert: &tilo_user_identity::DeviceCert,
) {
    use tilo_workspace_service::workspaces::person_workspace::devices;
    // A device confined to one workspace is listed by the root that certified it, in the same
    // step, and keeps no root to mark: it writes nothing of its person's.
    if super::confined::workspace_of(cert).is_some() {
        return;
    }
    // One write lists this device. The root is this account's own, read by the account it runs
    // as and not looked up through a sign-in record; the mark that it keeps the root is kept in
    // the same record as the entry that lists it. A second write founded the record again from
    // what it read before the first was carried, at the same author's first number.
    let marking = match super::services::device_root_record() {
        Ok(record) => tilo_identity::store::load_from_path(record).ok(),
        Err(_) => None,
    };
    let listed = match &marking {
        Some(root) => devices::mark_root_holder(copies, root, cert).await,
        None => devices::keep(copies, cert).await,
    };
    if let Err(error) = listed {
        if marking.is_some() {
            tracing::warn!(%error, "this device's root is not marked in its person's devices");
        } else {
            tracing::warn!(%error, "this device is not listed with its person's");
        }
    } else if marking.is_some() {
        // The device keeping the person's root also writes their list, from what this host knows
        // of the workspaces they are in.
        copies.list_known_for(&cert.user_public_key).await;
    }
}
