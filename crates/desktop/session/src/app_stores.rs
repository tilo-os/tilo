//! The resources of the apps the session's services run over, kept in the workspace the person
//! is in once the device's daemon holds it: the store the app's windows open, so the services,
//! the windows, an agent's programs and the person's other devices read and change one copy.

use tilo_lock::MutexRecover as _;

impl crate::Shared {
    /// Keep the apps' resources the services run over in the workspace the person is in, once the
    /// session holds it: the store the app's windows open, so the service, the windows, an agent's
    /// programs and the person's other devices read and change one copy.
    pub(crate) fn keep_app_stores(
        &self,
        runtime: &tokio::runtime::Handle,
        dispatch: Option<&dyn crate::app_wire::WorkspaceActions>,
    ) {
        if *self.app_stores.borrow() == AppStores::Kept {
            return;
        }
        let waits = |on: &'static str| *self.app_stores.borrow_mut() = AppStores::Waiting(on);
        let Some(workspace) = self.handle.read(|tree| {
            tree.active_workspace()
                .map(tilo_desktop_model::Workspace::id)
        }) else {
            return waits("the session shows no workspace");
        };
        // A workspace the daemon does not hold yet (one still being founded) would refuse every
        // record moved there.
        let Some(standing) = self
            .view
            .as_ref()
            .and_then(|view| view.holds(workspace).then(|| view.revision()))
        else {
            return waits("the device's daemon does not hold the workspace the session shows yet");
        };
        // A move the workspace refused waits for how the workspace stands to move on, rather
        // than being asked again on every pass.
        if matches!(&*self.app_stores.borrow(), AppStores::Refused(held, _) if *held == standing) {
            return;
        }
        let workspace = workspace.to_string();
        // An app's records are the workspace's only once it runs the app: each it runs no version
        // of yet is installed first, as opening the app there installs it, so every device
        // holding the workspace decides the app's writes by the surface it installed.
        match self.install_apps(&workspace, standing, dispatch) {
            Installed::All => {}
            Installed::Waits(on) => return waits(on),
            Installed::Refused(why) => {
                tracing::warn!(%why, "the apps the session runs were not installed in the workspace");
                *self.app_stores.borrow_mut() = AppStores::Refused(standing, why);
                return;
            }
        }
        let Some(store) = (self.held_stores)(workspace.clone(), runtime) else {
            return waits("this device's workspace service holds no store of the workspace");
        };
        let kept = match self.services.borrow_mut().keep_in_workspace(&store) {
            Ok(_) => AppStores::Kept,
            Err(error) => {
                tracing::warn!(%error, "the apps' resources are not kept in the workspace");
                AppStores::Refused(standing, error)
            }
        };
        if kept == AppStores::Kept {
            // What the apps' code calls of other apps goes over a connection the daemon vouches
            // for as that app, decided as their windows' calls are.
            if let Some(apps) = self.launch.clone() {
                let runtime = runtime.clone();
                let calls_in = workspace.clone();
                let connect: tilo_service::ConnectApp =
                    std::rc::Rc::new(move |from: &tilo_service::CallsFrom| {
                        let connection = apps.connection_as(&from.app, &calls_in)?;
                        AppConnection::over(connection, from, &runtime).map(|calls| {
                            std::rc::Rc::new(calls) as std::rc::Rc<dyn tilo_service::AppCalls>
                        })
                    });
                self.services
                    .borrow_mut()
                    .reach_workspace(&connect, &workspace, &self.wake_handle);
            } else {
                tracing::warn!("the apps' calls of other apps reach no workspace");
            }
        }
        *self.app_stores.borrow_mut() = kept;
    }
}

impl crate::Shared {
    /// Install, through the workspace's own `install` as an open does
    /// ([`tilo_app_host::install_request`]), each app the session's services run that `workspace`
    /// runs no version of: whether all of them run there now, the move waits, or the workspace
    /// refused one.
    fn install_apps(
        &self,
        workspace: &str,
        standing: u64,
        dispatch: Option<&dyn crate::app_wire::WorkspaceActions>,
    ) -> Installed {
        if let AppStores::Installing(asked) = &*self.app_stores.borrow() {
            let answers = asked.answers.lock_recover();
            if let Some(Err(why)) = answers.iter().find(|answer| answer.is_err()) {
                return Installed::Refused(why.clone());
            }
            if answers.len() < asked.count {
                return Installed::Waits("the workspace installs the apps the session runs");
            }
        }
        let Some(policy) = self
            .view
            .as_ref()
            .and_then(|view| view.policy(workspace).ok())
        else {
            return Installed::Waits("the workspace's policy has not reached this device yet");
        };
        let releases: Vec<_> = self
            .services
            .borrow()
            .apps()
            .iter()
            .filter_map(|app| tilo_app_host::release_to_install(&policy, app))
            .collect();
        if releases.is_empty() {
            return Installed::All;
        }
        // Installed, and the workspace's policy is yet to say so here: its grants arrive with
        // the next standing of the workspace.
        if matches!(&*self.app_stores.borrow(), AppStores::Installing(asked) if asked.standing == standing)
        {
            return Installed::Waits("the workspace's policy has not shown the apps installed yet");
        }
        let Some(dispatch) = dispatch else {
            return Installed::Waits("the session has no dispatch to install the apps with");
        };
        let asked = InstallsAsked {
            standing,
            count: releases.len(),
            answers: std::sync::Arc::default(),
        };
        for release in &releases {
            let answers = std::sync::Arc::clone(&asked.answers);
            let app = release.app.clone();
            dispatch
                .follow(
                    workspace.to_owned(),
                    tilo_app_host::install_request(workspace, release),
                )
                .then(move |answer| {
                    let answer = answer
                        .unwrap_or_else(|| Err("the device's dispatch never answered".to_owned()))
                        .map(drop)
                        .map_err(|why| format!("{app} was not installed: {why}"));
                    answers.lock_recover().push(answer);
                });
        }
        *self.app_stores.borrow_mut() = AppStores::Installing(asked);
        Installed::Waits("the workspace installs the apps the session runs")
    }
}

/// Whether the apps the session runs run in the workspace it moves their records to.
enum Installed {
    /// Every one of them runs there.
    All,
    /// The move waits on this.
    Waits(&'static str),
    /// The workspace refused to install one, saying why.
    Refused(String),
}

/// The installs a session asked of a workspace before moving its apps' records there, and their
/// answers as they come.
#[derive(Clone, Debug)]
pub(crate) struct InstallsAsked {
    /// How the workspace stood on the device's daemon when they were asked.
    standing: u64,
    /// How many were asked.
    count: usize,
    /// Each one's answer, as it ended.
    answers: std::sync::Arc<std::sync::Mutex<Vec<Result<(), String>>>>,
}

impl PartialEq for InstallsAsked {
    fn eq(&self, other: &Self) -> bool {
        std::sync::Arc::ptr_eq(&self.answers, &other.answers)
    }
}

impl Eq for InstallsAsked {}

/// Where moving the apps' resources into the workspace the person is in stands.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum AppStores {
    /// Not moved yet, and what the move waits on.
    Waiting(&'static str),
    /// Waiting for the workspace to install the apps the session runs, as asked.
    Installing(InstallsAsked),
    /// The workspace refused them, saying why, while it stood here on the device's daemon.
    Refused(u64, String),
    /// Every app's resources are kept there.
    Kept,
}

impl Default for AppStores {
    fn default() -> Self {
        Self::Waiting("the session has not yet offered them")
    }
}

impl std::fmt::Display for AppStores {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Waiting(on) => write!(f, "waiting: {on}"),
            Self::Installing(_) => {
                f.write_str("waiting: the workspace installs the apps the session runs")
            }
            Self::Refused(_, why) => write!(f, "refused: {why}"),
            Self::Kept => f.write_str("kept"),
        }
    }
}

/// The store of a workspace the device's daemon keeps, none while the session reaches no daemon.
pub(crate) type HeldStores = std::rc::Rc<
    dyn Fn(
        String,
        &tokio::runtime::Handle,
    ) -> Option<std::rc::Rc<dyn tilo_service::WorkspaceStore>>,
>;

/// The stores of the workspaces the daemon `daemon` keeps, asked on the session's link to it,
/// each deciding what runs into it by the person's access as `view` reads the workspace's policy.
/// None without a view: a session that reads no workspace's policy runs nothing into one.
pub(crate) fn held_stores(
    daemon: tilo_session_client::daemon_documents::DaemonShell,
    view: Option<std::sync::Arc<tilo_session_client::workspace_view::WorkspaceView>>,
) -> HeldStores {
    std::rc::Rc::new(move |workspace, _runtime| {
        daemon.link().ok()?;
        Some(std::rc::Rc::new(HeldStore {
            daemon: daemon.clone(),
            view: std::sync::Arc::clone(view.as_ref()?),
            workspace,
        }))
    })
}

/// The store of one workspace the device's daemon keeps, as the apps the session runs itself
/// write and read their resources in it: each command runs at the daemon, as the device's person.
struct HeldStore {
    daemon: tilo_session_client::daemon_documents::DaemonShell,
    /// The workspaces as this device holds them: the workspace's policy, which decides every
    /// action the session's apps run into it.
    view: std::sync::Arc<tilo_session_client::workspace_view::WorkspaceView>,
    workspace: String,
}

impl tilo_service::WorkspaceStore for HeldStore {
    fn command(&self, app: &str, command: &[u8]) -> Result<Vec<u8>, String> {
        use runtime_collab::wire::shell::{ShellCall, ShellReply};
        let command = std::str::from_utf8(command)
            .map_err(|_| "a resource command is not text".to_owned())?
            .to_owned();
        match self.daemon.ask_blocking(ShellCall::Resources {
            workspace: self.workspace.clone(),
            app: app.to_owned(),
            command,
        })? {
            ShellReply::Resources(reply) => Ok(reply?.into_bytes()),
            other => Err(format!(
                "the daemon answered a resource command with {other:?}"
            )),
        }
    }

    fn access(&self) -> Option<tilo_actions::policy::MemberAccess> {
        let policy = self.view.policy(&self.workspace).ok()?;
        Some(tilo_actions::policy::MemberAccess::person(
            policy,
            hex::encode(self.view.person()),
        ))
    }
}

/// One app's connection to its workspace, which the device's daemon vouches for as that app: its
/// calls are sent and followed on `runtime`, off the session's loop.
struct AppConnection {
    /// The app's record its calls are decided on, on the connection.
    record: runtime_collab::RecordHandle,
    runtime: tokio::runtime::Handle,
    /// Set once the connection ended: a call is then sent over a new one.
    ended: std::sync::Arc<std::sync::atomic::AtomicBool>,
}

/// How long one follow of a call waits for it to move before it is asked again.
const FOLLOW_MS: u32 = 60_000;

impl AppConnection {
    fn over(
        connection: std::os::fd::OwnedFd,
        from: &tilo_service::CallsFrom,
        runtime: &tokio::runtime::Handle,
    ) -> Result<Self, String> {
        let _entered = runtime.enter();
        let (client, end) = runtime_collab::CollabClient::from_connected_fd(connection)
            .map_err(|error| format!("the app's connection to its workspace: {error}"))?;
        let ended = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let marks = std::sync::Arc::clone(&ended);
        runtime.spawn(async move {
            end.await;
            marks.store(true, std::sync::atomic::Ordering::Release);
        });
        Ok(Self {
            record: client.record(&from.record, &from.kind),
            runtime: runtime.clone(),
            ended,
        })
    }
}

impl tilo_service::AppCalls for AppConnection {
    fn send(
        &self,
        request: tilo_actions::remote::InvokeRequest,
        ended: Box<dyn FnOnce(tilo_actions::remote::ReceiptStatus) + Send>,
    ) -> Result<(), String> {
        if self.ended.load(std::sync::atomic::Ordering::Acquire) {
            return Err("the app's connection to its workspace ended".to_owned());
        }
        let record = self.record.clone();
        self.runtime.spawn(async move {
            ended(followed(&record, request).await);
        });
        Ok(())
    }
}

/// How the call `request` ended, sent on the app's `record` and followed while it runs or waits for the
/// person's answer.
async fn followed(
    record: &runtime_collab::RecordHandle,
    request: tilo_actions::remote::InvokeRequest,
) -> tilo_actions::remote::ReceiptStatus {
    use tilo_actions::InvocationUpdate;
    use tilo_actions::remote::{
        ActionCallError, ReceiptStatus, RemoteActionFrame, RemoteActionMessage, UpdateMark,
    };
    let unavailable = |reason: String| ReceiptStatus::Rejected {
        error: ActionCallError::Unavailable { reason },
    };
    let invocation = request.invocation;
    let ask = |message: RemoteActionMessage| async move {
        let frame = RemoteActionFrame::new(message)
            .encode()
            .map_err(|error| error.to_string())?;
        let answer = record
            .remote_action(&frame)
            .await
            .map_err(|error| error.to_string())?;
        RemoteActionFrame::decode(&answer)
            .map(|frame| frame.message)
            .map_err(|error| error.to_string())
    };
    let mut update = match ask(RemoteActionMessage::Invoke(request)).await {
        Ok(RemoteActionMessage::Receipt(receipt)) => match receipt.status {
            ReceiptStatus::Accepted => InvocationUpdate::Accepted { invocation },
            status => return status,
        },
        Ok(other) => return unavailable(format!("the workspace answered a call with {other:?}")),
        Err(error) => return unavailable(error),
    };
    loop {
        match update {
            InvocationUpdate::Succeeded { output, .. } => {
                return ReceiptStatus::Completed { output };
            }
            InvocationUpdate::Failed { error, .. } => return ReceiptStatus::Failed { error },
            InvocationUpdate::Cancelled { message, .. } => {
                return ReceiptStatus::Cancelled { message };
            }
            InvocationUpdate::Accepted { .. }
            | InvocationUpdate::Progress { .. }
            | InvocationUpdate::Item { .. } => {}
        }
        let observe = RemoteActionMessage::Observe {
            correlation: 0,
            invocation,
            wait_ms: Some(FOLLOW_MS),
            seen: Some(UpdateMark::of(&update)),
        };
        update = match ask(observe).await {
            Ok(RemoteActionMessage::Update {
                state: Some(state), ..
            }) => state.update,
            Ok(RemoteActionMessage::Update { state: None, .. }) => {
                return unavailable("the workspace lost the call".to_owned());
            }
            Ok(other) => {
                return unavailable(format!("the workspace answered a follow with {other:?}"));
            }
            Err(error) => return unavailable(error),
        };
    }
}
