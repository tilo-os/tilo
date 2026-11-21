//! A workspace's Browser, opened in the workspace's VM on the VM host that
//! runs it (`docs/hosted-browser.md`, "Placement and routing").
//!
//! Browser runs as a headless session in the workspace's own VM, the one its
//! shells run in ([`linux_vm::hosted::browser`]). This host only routes: it
//! asks the placer for the host of that VM, as a shell does, and asks that
//! host's member device, over the device action service as this host's own device, to
//! open Browser there ([`tilo_actions::remote::browser`]), handing it what the
//! session joins the workspace with. While every host is full the open waits,
//! and says so once; after [`OPEN_PATIENCE`] it says why instead of waiting
//! forever.

use std::sync::Arc;
use std::time::{Duration, Instant};

use tilo_actions::remote::browser::{self, OpenBrowser, OpenedBrowser};
use tilo_actions::remote::linux_apps::{self, AppLaunch, OpenApp};
use tilo_actions::remote::server_account::Refusal;
use tilo_actions::remote::{
    ActionCallError, ActionProvider as _, Caller, InvokeArgument, InvokeRequest, ReceiptStatus,
};
use tilo_actions::{InvocationId, InvocationUpdate};
use tilo_peer_state::actions::{ActionService, RemoteDevice};
use tilo_server_protocol::hosted::Material;

use super::hosted_browser::{AppRefusal, Window};
use super::placed_shells::{self, PlacedShells};

/// How long an open waits for a host with room.
const OPEN_PATIENCE: Duration = Duration::from_secs(5 * 60);

/// A workspace's Browser, up.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Opened {
    /// The VM host running it.
    pub host: String,
    /// What opening took, when this call opened it; `None` when it attached to
    /// the session already running.
    pub opened_ms: Option<u64>,
    /// The provider token of the window the session opened, which
    /// page actions on it name.
    pub provider: String,
}

/// Why a workspace's Browser did not open.
#[derive(Debug)]
pub enum OpenError {
    /// It will not open: in words for the person who asked.
    Refused(String),
    /// Asking went wrong: the placer, a member, or this host's own device.
    Error(String),
}

impl std::fmt::Display for OpenError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Refused(words) | Self::Error(words) => f.write_str(words),
        }
    }
}

/// Where a workspace's Browser runs when its host has VM hosts: the
/// workspace's VM, wherever its shells run.
pub struct PlacedBrowsers {
    placed: Arc<PlacedShells>,
    /// How long an open waits for room.
    patience: Duration,
}

impl PlacedBrowsers {
    /// The placed browsers the host's environment names: the same placer,
    /// members file and account as its placed shells.
    ///
    /// # Errors
    ///
    /// A placer is named but the members or the account are not usable.
    pub fn from_env() -> Result<Option<Self>, String> {
        Ok(PlacedShells::shared_from_env()?.map(Self::new))
    }

    #[must_use]
    pub fn new(placed: Arc<PlacedShells>) -> Self {
        Self {
            placed,
            patience: OPEN_PATIENCE,
        }
    }

    /// Wait this long for a host with room before refusing.
    #[must_use]
    pub const fn with_patience(mut self, patience: Duration) -> Self {
        self.patience = patience;
        self
    }

    /// Ask as `action_service`'s device, from `endpoint`: this host's own, holding `workspaces`
    /// read on `runtime`.
    pub fn ask_from(
        &self,
        action_service: ActionService,
        endpoint: iroh::Endpoint,
        workspaces: crate::workspaces::Workspaces,
        runtime: tokio::runtime::Handle,
    ) {
        self.placed
            .ask_from(action_service, endpoint, workspaces, runtime);
    }

    /// Start `launch` in `workspace`'s Linux apps session in its VM, starting
    /// that session with `material` when none runs.
    ///
    /// # Errors
    ///
    /// As [`Self::open`], and the plans' typed refusal when the account may
    /// not run Linux.
    pub fn open_app(
        &self,
        workspace: &str,
        material: &str,
        launch: &AppLaunch,
    ) -> Result<Opened, AppRefusal> {
        let words = AppRefusal::Words;
        let until = Instant::now().checked_add(self.patience);
        let stopping = move || until.is_some_and(|until| Instant::now() >= until);
        let mut waited = None;
        let host = self
            .placed
            .placed(workspace, &mut waited, Some(&stopping))
            .map_err(words)?
            .ok_or_else(|| {
                words(format!(
                    "{} No machine has room.",
                    wait_words(waited.as_deref())
                ))
            })?;
        let address = self.member(&host).map_err(words)?;
        let asked = OpenApp {
            workspace: workspace.to_owned(),
            account: self.placed.account().to_owned(),
            material: material.to_owned(),
            launch: launch.clone(),
        };
        let device = self
            .placed
            .member_device(&address, workspace)
            .map_err(words)?;
        let opened: OpenedBrowser = follow(&device, &asked, linux_apps::target())
            .map_err(|error| match error {
                OnMember::Plan(refusal) => AppRefusal::Refused(refusal),
                OnMember::Refused(words) | OnMember::Error(words) => AppRefusal::Words(words),
            })?
            .unwrap_or_default();
        Ok(Opened {
            host,
            opened_ms: opened.opened_ms,
            provider: opened.provider,
        })
    }

    /// End the Linux apps session in `workspace`'s VM now.
    ///
    /// # Errors
    ///
    /// The placer or the member could not be asked, or the member refused.
    pub fn stop_apps(&self, workspace: &str) -> Result<(), String> {
        let host = self
            .placed
            .placed(workspace, &mut None, None)?
            .ok_or_else(|| "the placer named no host".to_owned())?;
        let address = self.member(&host)?;
        let device = self.placed.member_device(&address, workspace)?;
        let asked = OpenBrowser {
            workspace: workspace.to_owned(),
            account: String::new(),
            material: String::new(),
        };
        follow(&device, &asked, linux_apps::stop_target())
            .map(|_| ())
            .map_err(|error| format!("{host}: {}", error.words()))
    }

    /// Open `workspace`'s Browser in its VM, handing the session `material`
    /// (what it joins the workspace with, opaque here). A session already
    /// running is attached to instead.
    ///
    /// # Errors
    ///
    /// No host had room in time, the placer or the member could not be
    /// asked, or the member refused.
    pub fn open(&self, workspace: &str, material: &str) -> Result<Opened, OpenError> {
        let until = Instant::now().checked_add(self.patience);
        let stopping = move || until.is_some_and(|until| Instant::now() >= until);
        let mut waited = None;
        let host = self
            .placed
            .placed(workspace, &mut waited, Some(&stopping))
            .map_err(OpenError::Error)?
            .ok_or_else(|| {
                OpenError::Refused(format!(
                    "{} No machine has room.",
                    wait_words(waited.as_deref())
                ))
            })?;
        let address = self.member(&host).map_err(OpenError::Error)?;
        let asked = OpenBrowser {
            workspace: workspace.to_owned(),
            account: self.placed.account().to_owned(),
            material: material.to_owned(),
        };
        let device = self
            .placed
            .member_device(&address, workspace)
            .map_err(OpenError::Error)?;
        let opened = open_on(&device, &asked).map_err(|error| match error {
            OnMember::Refused(words) => OpenError::Refused(words),
            OnMember::Plan(refusal) => OpenError::Refused(refusal.message),
            OnMember::Error(words) => OpenError::Error(words),
        })?;
        Ok(Opened {
            host,
            opened_ms: opened.opened_ms,
            provider: opened.provider,
        })
    }

    /// End the browser session in `workspace`'s VM now.
    ///
    /// # Errors
    ///
    /// The placer or the member could not be asked, or the member refused.
    pub fn stop(&self, workspace: &str) -> Result<(), String> {
        let host = self
            .placed
            .placed(workspace, &mut None, None)?
            .ok_or_else(|| "the placer named no host".to_owned())?;
        let address = self.member(&host)?;
        let device = self.placed.member_device(&address, workspace)?;
        stop_on(&device, workspace).map_err(|words| format!("{host}: {words}"))
    }

    /// Who pays for the workspaces' machines, as this host knows it.
    pub(crate) fn account(&self) -> &str {
        self.placed.account()
    }

    /// Where `host`'s member device is reached.
    fn member(&self, host: &str) -> Result<iroh::EndpointAddr, String> {
        let member = self.placed.members().get(host).ok_or_else(|| {
            format!("the machine that runs this workspace is not reachable ({host})")
        })?;
        placed_shells::member_address(member)
    }
}

/// The line a waiting open printed, if it printed one.
fn wait_words(waited: Option<&str>) -> String {
    waited
        .map(|words| {
            words
                .trim_start_matches("ush: ")
                .replace(" Waiting; Ctrl+C cancels.", "")
        })
        .unwrap_or_default()
}

/// What went wrong asking a member.
enum OnMember {
    Refused(String),
    /// The account's plan refused it, as the VM host's plans file words it.
    Plan(Refusal),
    Error(String),
}

impl OnMember {
    fn words(self) -> String {
        match self {
            Self::Refused(words) | Self::Error(words) => words,
            Self::Plan(refusal) => refusal.message,
        }
    }
}

/// Open `asked`'s workspace's Browser on `device`, and what its session
/// said.
fn open_on(device: &RemoteDevice, asked: &OpenBrowser) -> Result<OpenedBrowser, OnMember> {
    follow(device, asked, browser::target()).map(Option::unwrap_or_default)
}

/// End the browser session in `workspace`'s VM on `device`.
fn stop_on(device: &RemoteDevice, workspace: &str) -> Result<(), String> {
    let asked = OpenBrowser {
        workspace: workspace.to_owned(),
        account: String::new(),
        material: String::new(),
    };
    follow(device, &asked, browser::stop_target())
        .map(|_| ())
        .map_err(OnMember::words)
}

/// Ask `device` to do `asked` at `target`, and follow it until it ends.
fn follow(
    device: &RemoteDevice,
    asked: &impl serde::Serialize,
    target: tilo_actions::remote::RemoteActionTarget,
) -> Result<Option<OpenedBrowser>, OnMember> {
    let serde_json::Value::Object(argument) =
        serde_json::to_value(asked).map_err(|error| OnMember::Error(error.to_string()))?
    else {
        return Err(OnMember::Error("a Browser open is an object".to_owned()));
    };
    let invocation = InvocationId::new();
    let request = InvokeRequest {
        invocation,
        target,
        argument: InvokeArgument::Json(argument),
        context: None,
        confirmation: None,
        revision: None,
        on_behalf_of: None,
        wait_ms: None,
        appearance: None,
    };
    // Who asks is the action service's device: the caller here is only what the trait
    // asks for, never sent.
    let caller = Caller::local_app("tilo.server");
    let until = Instant::now().checked_add(OPEN_PATIENCE);
    // The same invocation asked again runs once: a machine that did not answer is asked
    // again within the open's patience. Any other answer is the answer.
    let receipt = loop {
        let asked = placed_shells::settled(device.invoke(&caller, request.clone()));
        let again = match &asked {
            Err(_) | Ok(Err(ActionCallError::Unanswered)) => Some(Duration::ZERO),
            Ok(_) => None,
        };
        let in_time = |wait: Duration| {
            until.is_none_or(|until| {
                Instant::now()
                    .checked_add(wait)
                    .is_some_and(|at| at < until)
            })
        };
        match (asked, again) {
            (Ok(Ok(receipt)), _) => break receipt,
            (asked, Some(wait)) if in_time(wait) => {
                tracing::info!(
                    ?asked,
                    ?wait,
                    "a machine did not take a session's start yet; asking again"
                );
                std::thread::sleep(wait);
            }
            (asked, _) => {
                tracing::warn!(?asked, "a machine did not start a session");
                return Err(OnMember::Error("The machine couldn't start it.".to_owned()));
            }
        }
    };
    if let ReceiptStatus::Rejected { error } = receipt.status {
        tracing::warn!(%error, "a machine refused a session");
        return Err(OnMember::Refused(
            "The machine couldn't start it.".to_owned(),
        ));
    }
    loop {
        if until.is_some_and(|until| Instant::now() >= until) {
            return Err(OnMember::Error(
                "It is taking too long to start.".to_owned(),
            ));
        }
        if let Some(mut change) = device.next_change(&caller, &invocation) {
            while let Err(again) = change.wait_timeout(Duration::from_millis(100)) {
                change = again;
            }
        } else {
            std::thread::sleep(Duration::from_millis(50));
        }
        let Some(state) = device.observe(&caller, &invocation) else {
            continue;
        };
        match state.update {
            InvocationUpdate::Succeeded { output, .. } => {
                return Ok(output
                    .value
                    .and_then(|value| serde_json::from_value(value).ok()));
            }
            InvocationUpdate::Failed { error, .. } => {
                // The plans refused it: the typed refusal travels as the
                // failure's details, and says what would let it through.
                let plan = (error.code == linux_apps::REFUSED)
                    .then(|| error.details.clone())
                    .flatten()
                    .and_then(|details| serde_json::from_value::<Refusal>(details).ok());
                if plan.is_none() {
                    tracing::warn!(%error.message, "a machine failed to start a session");
                }
                return Err(plan.map_or_else(
                    || OnMember::Error("The machine couldn't start it.".to_owned()),
                    OnMember::Plan,
                ));
            }
            InvocationUpdate::Cancelled { .. } => {
                return Err(OnMember::Error("The start was cancelled.".to_owned()));
            }
            InvocationUpdate::Accepted { .. }
            | InvocationUpdate::Progress { .. }
            | InvocationUpdate::Item { .. } => {}
        }
    }
}

/// What the server's Browser asks of a workspace's VM, on the VM host the
/// placer names.
impl super::hosted_browser::BrowserSessions for PlacedBrowsers {
    /// Hand `material` to the session in the workspace's VM, and answer its
    /// window once it is up.
    fn start(&self, material: &Material) -> Result<Window, String> {
        let written = serde_json::to_string(material).map_err(|error| {
            tracing::warn!(%error, "the Browser's material could not be written");
            "Browser couldn't start. Try again in a minute.".to_owned()
        })?;
        // A machine that did not answer the start yet is asked again under the same
        // invocation ([`follow`]); a start that failed, or a refusal (no room, the plan), is
        // said at once.
        let opened = match self.open(&material.workspace, &written) {
            Ok(opened) => opened,
            Err(OpenError::Refused(words)) => return Err(words),
            Err(OpenError::Error(words)) => {
                tracing::warn!(%words, "opening the workspace's Browser failed");
                return Err("Browser couldn't start. Open it again to try once more.".to_owned());
            }
        };
        Ok(Window {
            device: material.device.device_public_key,
            provider: opened.provider,
        })
    }

    /// End the browser session in the workspace's VM now.
    fn stop(&self, workspace: &str) {
        if let Err(words) = PlacedBrowsers::stop(self, workspace) {
            tracing::warn!(%workspace, %words, "a browser session did not stop");
        }
    }

    /// Hand `launch` to the workspace's Linux apps session, started with
    /// `material` when none runs.
    fn start_app(&self, material: &Material, launch: &AppLaunch) -> Result<Window, AppRefusal> {
        let written = serde_json::to_string(material).map_err(|error| {
            tracing::warn!(%error, "the apps session's material could not be written");
            AppRefusal::Words("The app couldn't start. Try again in a minute.".to_owned())
        })?;
        let opened = self.open_app(&material.workspace, &written, launch)?;
        Ok(Window {
            device: material.device.device_public_key,
            provider: opened.provider,
        })
    }

    /// End the Linux apps session in the workspace's VM now.
    fn stop_apps(&self, workspace: &str) {
        if let Err(words) = PlacedBrowsers::stop_apps(self, workspace) {
            tracing::warn!(%workspace, %words, "a Linux apps session did not stop");
        }
    }

    fn ask_from(
        &self,
        action_service: ActionService,
        endpoint: iroh::Endpoint,
        workspaces: crate::workspaces::Workspaces,
        runtime: tokio::runtime::Handle,
    ) {
        PlacedBrowsers::ask_from(self, action_service, endpoint, workspaces, runtime);
    }
}

impl std::fmt::Debug for PlacedBrowsers {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PlacedBrowsers").finish_non_exhaustive()
    }
}
