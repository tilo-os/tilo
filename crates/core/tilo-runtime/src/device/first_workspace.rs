//! The person's workspace on this computer: the one its login names, or, on a computer with no
//! login, their own workspace held here. That one is founded in the step that makes the person's
//! root, on the device that makes it ([`super::services::found_account_person`]), and nowhere
//! else, never later: every other device of theirs, one that was handed their root among them,
//! enters it from a device that has it. A computer that holds the person's root but not their
//! workspace founds no second one beside it: it says how to add it.

use std::sync::{Arc, OnceLock, Weak};

use tilo_desktop_model::WorkspaceId;

use super::take_work::files::Replicas;
use super::workspaces::ServiceWorkspaces;

/// What the workspace founded with a person's root is called.
pub(crate) const FOUNDED_NAME: &str = "Home";

/// What the daemon does for its person once their workspace is here: what its start does for a
/// person whose workspace it already holds. It answers whether it did it: not while this device
/// keeps no copy of their workspace yet, which leaves it to be done when the copy arrives.
pub(crate) type Settling = Arc<
    dyn Fn() -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<bool, String>> + Send>>
        + Send
        + Sync,
>;

/// The person's workspace on this computer, once there is one.
pub(crate) struct FirstWorkspace {
    /// Whether a login names it: a computer with none keeps its person's account itself, once it
    /// holds their workspace.
    login: bool,
    id: OnceLock<String>,
    workspaces: Arc<ServiceWorkspaces>,
    /// Where the files of the workspaces this computer hosts are shown, once they are.
    replicas: OnceLock<Weak<Replicas>>,
    /// What the daemon does once the workspace is here ([`Self::when_here`]).
    settling: OnceLock<Settling>,
    /// Whether that was done, once the workspace was here.
    settled: tokio::sync::OnceCell<()>,
}

impl FirstWorkspace {
    /// The workspace `login` names, or, with no login, the person's own when this computer holds
    /// it.
    pub(crate) fn open(workspaces: Arc<ServiceWorkspaces>, login: Option<&str>) -> Arc<Self> {
        let named = if let Some(login) = login {
            Some(login.to_owned())
        } else {
            let own = own_workspace(&workspaces.device_cert().user_public_key);
            let kept = Arc::clone(&workspaces);
            super::workspaces::block_on_owned(async move { kept.keeps(own).await })
                .then(|| own.to_string())
        };
        let id = OnceLock::new();
        if let Some(named) = named {
            let _ = id.set(named);
        }
        Arc::new(Self {
            login: login.is_some(),
            id,
            workspaces,
            replicas: OnceLock::new(),
            settling: OnceLock::new(),
            // A workspace here when the daemon starts is settled as one founded later is: the
            // start settles it ([`Self::settle`]), and so does whatever reaches it first.
            settled: tokio::sync::OnceCell::new(),
        })
    }

    /// The workspace `workspace`, held on a computer with no login.
    #[cfg(test)]
    pub(crate) fn founded(workspaces: Arc<ServiceWorkspaces>, workspace: WorkspaceId) -> Arc<Self> {
        let id = OnceLock::new();
        let _ = id.set(workspace.to_string());
        Arc::new(Self {
            login: false,
            id,
            workspaces,
            replicas: OnceLock::new(),
            settling: OnceLock::new(),
            settled: tokio::sync::OnceCell::new_with(Some(())),
        })
    }

    /// Show the files of the workspaces this computer founds or takes in, once it does, in the
    /// files daemon's trees.
    pub(crate) fn show_files_in(&self, replicas: &Arc<Replicas>) {
        let _ = self.replicas.set(Arc::downgrade(replicas));
    }

    /// Have `settling` done once the person's workspace is here: when whatever first reaches it
    /// finds it came in from their desktop ([`Self::settle`]), and as the daemon starts when it is
    /// here already.
    pub(crate) fn when_here(&self, settling: Settling) {
        let _ = self.settling.set(settling);
    }

    /// Do once what the daemon does for its person once their workspace is here
    /// ([`Self::when_here`]): this device listed with their others before anything signs as it,
    /// their root marked, and their root-only asks served. Nothing while there is no workspace.
    ///
    /// # Errors
    /// What settling does failed: the device is not listed with its person's, or their root is
    /// not marked, and a device that goes on unlisted would be refused where it is asked to act.
    pub(crate) async fn settle(&self) -> Result<(), String> {
        let (Some(_), Some(settling)) = (self.get(), self.settling.get().cloned()) else {
            return Ok(());
        };
        // A settling that failed, or found no copy of the workspace here to list this device in,
        // is not kept as done: the next caller tries it again. Only a refusal is answered.
        let settled = self
            .settled
            .get_or_try_init(|| async {
                match settling().await {
                    Ok(true) => Ok(()),
                    Ok(false) => Err(None),
                    Err(error) => Err(Some(error)),
                }
            })
            .await;
        match settled {
            Ok(()) | Err(None) => Ok(()),
            Err(Some(error)) => Err(error),
        }
    }

    /// [`Self::settle`], from a caller that does not run on the runtime.
    ///
    /// # Errors
    /// As [`Self::settle`].
    pub(crate) fn settle_blocking(self: &Arc<Self>) -> Result<(), String> {
        if self.settled.initialized() {
            return Ok(());
        }
        let first = Arc::clone(self);
        super::workspaces::block_on_owned(async move { first.settle().await })
    }

    /// Whether this computer keeps its person's account itself: it has no login, and holds their
    /// workspace, which was founded here with their root. A computer that logged in reaches the
    /// account through the device it entered from, and one holding their root without their
    /// workspace keeps nothing of it.
    pub(crate) fn keeps_account(&self) -> bool {
        !self.login && self.get().is_some()
    }

    /// The person's workspace, when there is one yet.
    pub(crate) fn get(&self) -> Option<&str> {
        self.id.get().map(String::as_str)
    }

    /// Found the person's workspace, at the id their key names, on the computer that just made
    /// their root, which no login names: the one step that founds it
    /// ([`super::services::found_account_person`]). A workspace already held there is theirs as
    /// it is.
    ///
    /// # Errors
    /// The service did not found it, or this device could not be listed with its person's.
    pub(crate) async fn found(&self) -> Result<String, String> {
        let workspace = own_workspace(&self.workspaces.device_cert().user_public_key);
        if self.get().is_none() {
            self.workspaces.found(workspace, FOUNDED_NAME).await?;
            let _ = self.id.set(workspace.to_string());
            // What waits for a workspace to be kept here (host search) looks again.
            self.workspaces.entered().notify_waiters();
            let _ = self.show(&[workspace]).await;
        }
        // This device is listed with its person's, and their root marked, once it holds the
        // workspace: one write, from [`Self::settle`].
        self.settle().await?;
        Ok(workspace.to_string())
    }

    /// The person's workspace held here, settled ([`Self::settle`]).
    ///
    /// # Errors
    /// This computer holds the person's root but not their workspace, which only the device that
    /// made the root founds; or settling it failed.
    pub(crate) async fn here(&self) -> Result<String, String> {
        let workspace = self.get().ok_or_else(not_held)?.to_owned();
        self.settle().await?;
        Ok(workspace)
    }

    /// [`Self::here`], from a caller that does not run on the runtime.
    ///
    /// # Errors
    /// As [`Self::here`].
    pub(crate) fn here_blocking(self: &Arc<Self>) -> Result<String, String> {
        let workspace = self.get().ok_or_else(not_held)?.to_owned();
        // One that came in from the person's desktop is settled by the first that reaches it.
        self.settle_blocking()?;
        Ok(workspace)
    }

    /// Settle the person's workspace when `workspace` names it on a computer with no login: what
    /// a call of theirs in it does the first time, for one that came in from their desktop too
    /// ([`Self::settle`]). Any other workspace is left as it is.
    ///
    /// # Errors
    /// `workspace` is the person's own and this computer does not hold it, or settling it failed.
    pub(crate) fn settle_if_theirs(self: &Arc<Self>, workspace: &str) -> Result<(), String> {
        if self.login
            || workspace
                != own_workspace(&self.workspaces.device_cert().user_public_key).to_string()
        {
            return Ok(());
        }
        self.here_blocking().map(drop)
    }

    /// The workspaces `moved` came in from the person's desktop: the person's own among them is
    /// their workspace here, when they have none yet, and every one's files are shown.
    pub(crate) fn moved_in(&self, moved: &[WorkspaceId]) {
        if self.login {
            return;
        }
        let person = own_workspace(&self.workspaces.device_cert().user_public_key);
        if moved.contains(&person) && self.id.set(person.to_string()).is_ok() {
            self.workspaces.entered().notify_waiters();
        }
        // The import that hands them over runs on the runtime; they show once started.
        drop(self.show(moved));
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
#[expect(
    clippy::expect_used,
    reason = "the person's workspace id is a UUID by its derivation"
)]
pub(super) fn own_workspace(person: &[u8; 32]) -> WorkspaceId {
    WorkspaceId::parse(&tilo_action_policy::roles::person_workspace_id(person))
        .expect("a person's workspace id is a workspace id")
}

/// Why a computer holding the person's root but not their workspace does nothing in it: only the
/// device that made the root founded it, and a second one founded here would be another
/// workspace under the same id, which none of their other devices reads.
pub(crate) fn not_held() -> String {
    "This computer has your Tilo identity but not your workspace, which only the device that \
     made your identity founds. Add this computer from a device of yours that has it: run `tilo \
     device add` there, then `tilo login <ticket>` here."
        .to_owned()
}

/// What the daemon's start does for a person whose workspace it holds, done where their workspace
/// comes to be held later: `cert`, this device, listed with their others in the devices record
/// of their workspace, and the root this account keeps marked there.
///
/// # Errors
/// The record refused this device. The device goes on unlisted, and every host it is not listed
/// with refuses what it signs: the caller hears the refusal instead of a daemon that serves as
/// nobody.
pub(super) async fn hold_person(
    copies: &tilo_workspace_service::workspaces::Workspaces,
    cert: &tilo_user_identity::DeviceCert,
) -> Result<(), String> {
    use tilo_workspace_service::workspaces::person_workspace::devices;
    // A device confined to one workspace is listed by the root that certified it, in the same
    // step, and keeps no root to mark: it writes nothing of its person's.
    if super::confined::workspace_of(cert).is_some() {
        return Ok(());
    }
    // One write lists this device. The root is this account's own, read by the account it runs
    // as and not looked up through a sign-in record; the mark that it keeps the root is kept in
    // the same record as the entry that lists it. A second write founded the record again from
    // what it read before the first was carried, at the same author's first number.
    let marking = match super::services::device_root_record() {
        Ok(record) => tilo_identity::store::load_from_path(record).ok(),
        Err(_) => None,
    };
    match &marking {
        Some(root) => devices::mark_root_holder(copies, root, cert)
            .await
            .map_err(|error| {
                format!("this device's root is not marked in its person's devices: {error}")
            })?,
        None => devices::keep(copies, cert)
            .await
            .map_err(|error| format!("this device is not listed with its person's: {error}"))?,
    }
    if marking.is_some() {
        // The device keeping the person's root also writes their list, from what this host knows
        // of the workspaces they are in.
        copies.list_known_for(&cert.user_public_key).await;
    }
    Ok(())
}
