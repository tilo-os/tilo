//! Installed native task launches use the existing signed supervisor and original action custody.
mod config;
mod custody;
mod worker;
use super::{
    NativeCertificates, NativeOwnerCommitService, NativeWorkspaceAuthority, WorkspaceTreeHandle,
};
use std::{
    path::PathBuf,
    sync::{Arc, Mutex, OnceLock, mpsc},
};
use tilo_action_broker::services::{
    ManagedTaskLaunchContext, ManagedTaskLaunchFuture, ManagedTaskLaunches, WorkspaceMemberships,
};

static INSTALLATION: OnceLock<Option<Arc<config::Installation>>> = OnceLock::new();

/// Admit one private host installation before any app or collaborator launches work.
/// # Errors
/// Refuses duplicate flags, malformed/untrusted installations or non-private configuration.
pub fn install_from_args(
    arguments: impl IntoIterator<Item = std::ffi::OsString>,
) -> Result<(), String> {
    let mut arguments = arguments.into_iter();
    let mut selected = None;
    while let Some(argument) = arguments.next() {
        let path = if argument == "--managed-tasks" {
            Some(PathBuf::from(arguments.next().ok_or(
                "--managed-tasks requires a private installation path",
            )?))
        } else {
            argument
                .to_str()
                .and_then(|argument| argument.strip_prefix("--managed-tasks="))
                .map(PathBuf::from)
        };
        if let Some(path) = path {
            if selected.is_some() || !path.is_absolute() {
                return Err("managed task installation path must be unique and absolute".into());
            }
            selected = Some(path);
        }
    }
    let installation = selected
        .map(|path| config::load(&path).map(Arc::new))
        .transpose()?;
    INSTALLATION
        .set(installation)
        .map_err(|_| "managed task installation already selected".into())
}

pub(super) fn service(
    tree: WorkspaceTreeHandle,
    network: Option<Arc<crate::shell::network::ShellNetwork>>,
) -> Option<Arc<dyn ManagedTaskLaunches>> {
    let installation = INSTALLATION.get()?.clone()?;
    let owner = Arc::new(NativeOwnerCommitService::new(tree.clone(), network));
    Some(Arc::new(NativeTasks {
        installation,
        custody: Arc::new(custody::Custody {
            owner,
            certificates: NativeCertificates::default(),
        }),
        membership: Arc::new(NativeWorkspaceAuthority(tree)),
        worker: Mutex::new(None),
    }))
}
struct NativeTasks {
    installation: Arc<config::Installation>,
    custody: Arc<custody::Custody>,
    membership: Arc<NativeWorkspaceAuthority>,
    worker: Mutex<Option<mpsc::SyncSender<worker::Request>>>,
}
impl NativeTasks {
    fn submit(
        &self,
        context: ManagedTaskLaunchContext,
    ) -> Result<tokio::sync::oneshot::Receiver<Result<serde_json::Value, String>>, String> {
        self.custody.person(&context)?;
        let (reply, receiver) = tokio::sync::oneshot::channel();
        let mut worker = self
            .worker
            .lock()
            .map_err(|_| "native launch worker unavailable")?;
        let sender = if let Some(sender) = worker.as_ref() {
            sender.clone()
        } else {
            let sender = worker::start(
                self.installation.clone(),
                self.custody.clone(),
                self.membership.clone(),
                context.clone(),
            )?;
            *worker = Some(sender.clone());
            sender
        };
        drop(worker);
        sender
            .try_send(worker::Request { context, reply })
            .map_err(|_| "native launch worker stopped or bounded queue full")?;
        Ok(receiver)
    }
}
impl ManagedTaskLaunches for NativeTasks {
    fn available(&self, room: &str) -> bool {
        self.custody.owner.requires_room(room) && self.custody.owner.health().is_ok()
    }
    fn launch(&self, context: ManagedTaskLaunchContext) -> ManagedTaskLaunchFuture {
        let submitted = self.submit(context);
        Box::pin(async move {
            submitted?
                .await
                .map_err(|_| "native launch worker ended; original attempt retained".to_owned())?
        })
    }
}
