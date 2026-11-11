//! A workspace's Browser, opened in the workspace's VM on the VM host that
//! runs it (`docs/hosted-browser.md`, "Placement and routing").
//!
//! Browser runs as a headless session in the workspace's own VM, the one its
//! shells run in ([`legacy_vm::hosted::browser`]). This host only routes: it
//! asks the placer for the host of that VM, as a shell does, and asks that
//! host's member device, over the device plane as this host's own device, to
//! open Browser there ([`tilo_actions::remote::browser`]), handing it what the
//! session joins the workspace with. While every host is full the open waits,
//! and says so once; after [`OPEN_PATIENCE`] it says why instead of waiting
//! forever.

use std::sync::Arc;
use std::time::{Duration, Instant};

use tilo_actions::remote::browser::{self, OpenBrowser, OpenedBrowser};
use tilo_actions::remote::{ActionHost as _, Caller, InvokeArgument, InvokeRequest, ReceiptStatus};
use tilo_actions::{InvocationId, InvocationUpdate};
use tilo_peer_state::actions::{ActionPlane, RemoteDevice};
use tilo_relay_protocol::hosted::Material;

use super::hosted_browser::Window;
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
        Ok(PlacedShells::from_env()?.map(|placed| Self::new(Arc::new(placed))))
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

    /// Ask as `plane`'s device, from `endpoint`: this host's own.
    pub fn ask_from(&self, plane: ActionPlane, endpoint: iroh::Endpoint) {
        self.placed.ask_from(plane, endpoint);
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
        let (plane, endpoint) = self.placed.asking().map_err(OpenError::Error)?;
        let device = plane.remote(endpoint, &address, workspace);
        let opened = open_on(&device, &asked).map_err(|error| match error {
            OnMember::Refused(words) => OpenError::Refused(words),
            OnMember::Error(words) => OpenError::Error(format!("{host}: {words}")),
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
        let (plane, endpoint) = self.placed.asking()?;
        let device = plane.remote(endpoint, &address, workspace);
        stop_on(&device, workspace).map_err(|words| format!("{host}: {words}"))
    }

    /// Who pays for the workspaces' machines, as this host knows it.
    pub(crate) fn account(&self) -> &str {
        self.placed.account()
    }

    /// Where `host`'s member device is reached.
    fn member(&self, host: &str) -> Result<iroh::EndpointAddr, String> {
        let member = self.placed.members().get(host).ok_or_else(|| {
            format!("the placer named {host}, which is no VM host this host knows")
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
    Error(String),
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
        .map_err(|error| match error {
            OnMember::Refused(words) | OnMember::Error(words) => words,
        })
}

/// Ask `device` to do `asked` at `target`, and follow it until it ends.
fn follow(
    device: &RemoteDevice,
    asked: &OpenBrowser,
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
    // Who asks is the plane's device: the caller here is only what the trait
    // asks for, never sent.
    let caller = Caller::local_app("tilo.relay");
    let receipt = placed_shells::settled(device.invoke(&caller, request))
        .map_err(OnMember::Error)?
        .map_err(|error| OnMember::Error(format!("the VM host refused it: {error}")))?;
    if let ReceiptStatus::Rejected { error } = receipt.status {
        return Err(OnMember::Refused(format!(
            "the VM host refused it: {error}"
        )));
    }
    let until = Instant::now().checked_add(OPEN_PATIENCE);
    loop {
        if until.is_some_and(|until| Instant::now() >= until) {
            return Err(OnMember::Error(
                "the VM host did not answer in time".to_owned(),
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
            InvocationUpdate::Failed { error, .. } => return Err(OnMember::Error(error.message)),
            InvocationUpdate::Cancelled { .. } => {
                return Err(OnMember::Error("the open was cancelled".to_owned()));
            }
            InvocationUpdate::Accepted { .. }
            | InvocationUpdate::Progress { .. }
            | InvocationUpdate::Item { .. } => {}
        }
    }
}

/// What the relay's Browser asks of a workspace's VM, on the VM host the
/// placer names.
impl super::hosted_browser::BrowserSessions for PlacedBrowsers {
    /// Hand `material` to the session in the workspace's VM, and answer its
    /// window once it is up.
    fn start(&self, material: &Material) -> Result<Window, String> {
        let written = serde_json::to_string(material)
            .map_err(|error| format!("Browser's material: {error}"))?;
        let opened = self
            .open(&material.workspace, &written)
            .map_err(|error| format!("Browser did not open: {error}"))?;
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
}

impl std::fmt::Debug for PlacedBrowsers {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PlacedBrowsers").finish_non_exhaustive()
    }
}
