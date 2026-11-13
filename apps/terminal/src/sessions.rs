//! How a terminal tab reaches the sessions it opens, and what it knows about
//! the places they can run.

use std::cell::RefCell;
use std::rc::Rc;

use crate::inner::{OpenError, Requester, TerminalBackend};
use crate::target::{EnvironmentSummary, RecentTarget, SessionTarget};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum OpenState {
    Fixed,
    Unopened,
    Opening,
    Open,
    /// Another device's process, or another participant's, runs the
    /// session: the tab shows it and starts a shell of its own only when
    /// the person asks (Restart shell).
    Joined,
    AwaitingAllow,
    Failed(String),
    NoRuntime(String),
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Directory {
    #[default]
    Unasked,
    Reached,
    Unreachable,
    NotHere,
}

pub(crate) type OpenAnswer = (
    Requester,
    Option<String>,
    Result<TerminalBackend, OpenError>,
);

enum Source {
    None,
    Standalone,
    Hosted {
        first: Option<crate::HostedProcessServices>,
    },
    #[cfg(test)]
    Scripted(Box<ScriptedOpen>),
}

#[cfg(test)]
type ScriptedOpen =
    dyn FnMut(&SessionTarget, Requester, Option<&str>) -> Result<TerminalBackend, OpenError>;

impl std::fmt::Debug for Source {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::None => "None",
            Self::Standalone => "Standalone",
            Self::Hosted { .. } => "Hosted",
            #[cfg(test)]
            Self::Scripted(_) => "Scripted",
        })
    }
}

pub struct Sessions {
    source: Source,
    pending: Option<crate::async_open::PendingOpen>,
    pub state: OpenState,
    pub target: Option<SessionTarget>,
    recent: RecentTarget,
    pub environments: Vec<EnvironmentSummary>,
    pub directory: Directory,
    pub legacy_vms: Vec<String>,
    allow: Option<Rc<RefCell<ui::widget::HostShellAllow>>>,
    intent: Option<ui::widget::AppHostIntent>,
    pub host_requested: bool,
    listing: Option<std::sync::mpsc::Receiver<Listing>>,
    window: Option<ui::hosted_window::HostedWindow>,
}

struct Listing {
    environments: Result<Vec<EnvironmentSummary>, String>,
    legacy_vms: Vec<String>,
}

impl std::fmt::Debug for Sessions {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Sessions")
            .field("source", &self.source)
            .field("state", &self.state)
            .field("target", &self.target)
            .finish_non_exhaustive()
    }
}

impl Default for Sessions {
    fn default() -> Self {
        Self::none()
    }
}

impl Sessions {
    fn with(source: Source, state: OpenState, recent: RecentTarget) -> Self {
        Self {
            source,
            pending: None,
            state,
            target: None,
            recent,
            environments: Vec::new(),
            directory: Directory::Unasked,
            legacy_vms: Vec::new(),
            allow: None,
            intent: None,
            host_requested: false,
            listing: None,
            window: None,
        }
    }

    /// A tab that never opens a session.
    pub fn none() -> Self {
        Self::with(Source::None, OpenState::Fixed, RecentTarget::none())
    }

    pub fn standalone() -> Self {
        Self::with(
            Source::Standalone,
            OpenState::Unopened,
            RecentTarget::none(),
        )
    }

    pub fn hosted(first: crate::HostedProcessServices) -> Self {
        Self::with(
            Source::Hosted { first: Some(first) },
            OpenState::Unopened,
            RecentTarget::none(),
        )
    }

    pub(crate) fn hosted_deferred() -> Self {
        Self::with(
            Source::Hosted { first: None },
            OpenState::Unopened,
            RecentTarget::none(),
        )
    }

    #[cfg(test)]
    pub fn scripted(
        recent: RecentTarget,
        open: impl FnMut(&SessionTarget, Requester, Option<&str>) -> Result<TerminalBackend, OpenError>
        + 'static,
    ) -> Self {
        Self::with(
            Source::Scripted(Box::new(open)),
            OpenState::Unopened,
            recent,
        )
    }

    pub(crate) fn in_window(&mut self, window: Option<ui::hosted_window::HostedWindow>) {
        self.window = window;
    }

    pub(crate) fn workspace(&self) -> Option<String> {
        match &self.window {
            Some(window) => window.workspace(),
            None => std::env::var("TILO_WORKSPACE_ID").ok(),
        }
        .filter(|workspace| !workspace.is_empty())
    }

    pub fn is_live(&self) -> bool {
        !matches!(self.source, Source::None)
    }

    pub fn environments_available(&self) -> bool {
        match self.source {
            Source::None => false,
            Source::Standalone => crate::inner::environments_available(),
            Source::Hosted { .. } => true,
            #[cfg(test)]
            Source::Scripted(_) => true,
        }
    }

    pub fn default_target(&self) -> SessionTarget {
        crate::target::default_target(self.recent.load().as_ref(), self.environments_available())
    }

    pub(crate) fn recent_in(&mut self, recent: RecentTarget) {
        self.recent = recent;
    }

    pub fn remember(&self, target: &SessionTarget) {
        self.recent.remember(target);
    }

    pub fn open(
        &mut self,
        target: &SessionTarget,
        requested_by: Requester,
        relay_session: Option<&str>,
    ) -> Result<TerminalBackend, OpenError> {
        if requested_by == Requester::Agent && target.is_host() {
            return Err(OpenError::Refused(
                "agents may not open the device's own shell".into(),
            ));
        }
        let workspace = self.workspace();
        match &mut self.source {
            Source::None => Err(OpenError::Unavailable(
                "this terminal has no process service".into(),
            )),
            Source::Standalone => {
                TerminalBackend::open(target, requested_by, relay_session, workspace.as_deref())
            }
            Source::Hosted { first } => crate::HostedProcessServices::open_session(
                first.take(),
                target,
                requested_by,
                relay_session,
                workspace.as_deref(),
                None,
            ),
            #[cfg(test)]
            Source::Scripted(open) => open(target, requested_by, relay_session),
        }
    }

    pub(crate) fn begin_open(
        &mut self,
        target: &SessionTarget,
        requester: Requester,
        attach: Option<&str>,
        waker: std::sync::Arc<std::sync::Mutex<ui::WakeHandle>>,
    ) -> Option<Result<TerminalBackend, OpenError>> {
        {
            self.pending = None;
            let workspace = self.workspace();
            if let Source::Hosted { first } = &mut self.source {
                if requester == Requester::Agent && target.is_host() {
                    return Some(Err(OpenError::Refused(
                        "agents may not open the device's own shell".into(),
                    )));
                }
                match crate::async_open::PendingOpen::start(
                    first.take(),
                    target.clone(),
                    requester,
                    attach.map(str::to_owned),
                    workspace,
                    waker,
                ) {
                    Ok(pending) => {
                        self.pending = Some(pending);
                        self.state = OpenState::Opening;
                        return None;
                    }
                    Err(error) => return Some(Err(error)),
                }
            }
        }
        Some(self.open(target, requester, attach))
    }

    pub(crate) fn take_open_answer(&mut self) -> Option<OpenAnswer> {
        {
            let pending = self.pending.as_ref()?;
            let result = pending.take_answer()?;
            let requester = pending.requester;
            let attach = pending.attach.clone();
            self.pending = None;
            Some((requester, attach, result))
        }
    }

    pub fn request_allow(&mut self) -> bool {
        #[cfg(test)]
        let scripted = matches!(self.source, Source::Scripted(_));
        #[cfg(not(test))]
        let scripted = false;
        if scripted || matches!(self.source, Source::Hosted { .. } | Source::Standalone) {
            let reply = Rc::new(RefCell::new(ui::widget::HostShellAllow::default()));
            self.intent = Some(ui::widget::AppHostIntent::AllowHostShell {
                reply: Rc::downgrade(&reply),
            });
            self.allow = Some(reply);
            return true;
        }
        false
    }

    pub fn take_allow_answer(&mut self) -> Option<bool> {
        let answer = self.allow.as_ref()?.borrow().answer?;
        self.allow = None;
        Some(answer)
    }

    pub fn take_intent(&mut self) -> Option<ui::widget::AppHostIntent> {
        self.intent.take()
    }

    pub fn refresh_environments(&mut self) {
        if !self.is_live() {
            return;
        }
        if matches!(self.source, Source::Hosted { .. }) {
            self.legacy_vms = tilo_paths::xdg::data_home()
                .map(|data| legacy_vms_under(&data))
                .unwrap_or_default();
        }
        if !self.environments_available() {
            self.environments.clear();
            self.directory = Directory::NotHere;
            return;
        }
        match self.source {
            Source::Hosted { .. } => {
                let listed =
                    self.directory_request(&serde_json::json!({"op": "list-environments"}));
                self.take_listing(listed);
            }
            Source::Standalone => self.directory = Directory::NotHere,
            Source::None => {}
            #[cfg(test)]
            Source::Scripted(_) => {}
        }
    }

    pub(crate) fn refresh_environments_in_background(
        &mut self,
        waker: std::sync::Arc<std::sync::Mutex<ui::WakeHandle>>,
    ) {
        if !matches!(self.source, Source::Hosted { .. }) || self.listing.is_some() {
            return;
        }
        let (send, receive) = std::sync::mpsc::channel();
        let workspace = self.workspace();
        let spawned = std::thread::Builder::new()
            .name("tilo-terminal-list-places".into())
            .spawn(move || {
                let legacy_vms = tilo_paths::xdg::data_home()
                    .map(|data| legacy_vms_under(&data))
                    .unwrap_or_default();
                let environments = crate::HostedProcessServices::ask(
                    &serde_json::json!({"op": "list-environments"}),
                    workspace.as_deref(),
                )
                .and_then(|reply| parse_directory_reply(&reply));
                let _ = send.send(Listing {
                    environments,
                    legacy_vms,
                });
                crate::inner::request_terminal_wake(&waker);
            });
        if spawned.is_ok() {
            self.listing = Some(receive);
        }
    }

    pub(crate) fn poll_environment_refresh(&mut self) -> bool {
        let Some(receive) = self.listing.as_ref() else {
            return false;
        };
        let listing = match receive.try_recv() {
            Ok(listing) => listing,
            Err(std::sync::mpsc::TryRecvError::Empty) => return false,
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                self.listing = None;
                return false;
            }
        };
        self.listing = None;
        self.legacy_vms = listing.legacy_vms;
        self.take_listing(listing.environments);
        true
    }

    fn take_listing(&mut self, listed: Result<Vec<EnvironmentSummary>, String>) {
        match listed {
            Ok(list) => {
                self.environments = list;
                self.directory = Directory::Reached;
            }
            Err(reason) => {
                if self.directory != Directory::Unreachable {
                    tracing::warn!(%reason, "terminal: the shell runtime did not list the environments");
                }
                self.directory = Directory::Unreachable;
            }
        }
    }

    pub fn create_environment(&mut self, name: &str) -> Result<String, String> {
        let name = name.trim();
        if name.is_empty() {
            return Err("an environment needs a name".into());
        }
        let before: Vec<String> = self.environments.iter().map(|e| e.key.clone()).collect();
        self.environments = self.directory_request(&serde_json::json!({
            "op": "create-environment",
            "name": name,
        }))?;
        self.directory = Directory::Reached;
        self.environments
            .iter()
            .find(|env| !before.contains(&env.key))
            .map(|env| env.key.clone())
            .ok_or_else(|| "the environment was not created".into())
    }

    pub fn rename_environment(&mut self, record: &str, name: &str) -> Result<(), String> {
        if !tilo_environment::is_record_key(record) {
            return Err("not an environment record".into());
        }
        self.environments = self.directory_request(&serde_json::json!({
            "op": "environment-action",
            "record": record,
            "action": "rename",
            "args": {"name": name},
        }))?;
        Ok(())
    }

    /// Grant the environment `record` a folder, read-write or read-only.
    pub fn grant_folder(
        &mut self,
        record: &str,
        path: &ui::tilo_resource::AbsolutePath,
        writable: bool,
    ) -> Result<(), String> {
        if !tilo_environment::is_record_key(record) {
            return Err("not an environment record".into());
        }
        self.environments = self.directory_request(&serde_json::json!({
            "op": "environment-action",
            "record": record,
            "action": "grant-dir",
            "args": {"path": path, "write": writable},
        }))?;
        Ok(())
    }

    fn directory_request(
        &self,
        request: &serde_json::Value,
    ) -> Result<Vec<EnvironmentSummary>, String> {
        // A shell-hosted tab asks over a fresh connection of its own, never its
        // first: that one carries the session. Terminal in a process of its
        // own asks over the control connection the launcher opened for it.
        let reply = match self.source {
            Source::Hosted { .. } => {
                crate::HostedProcessServices::ask(request, self.workspace().as_deref())?
            }
            Source::Standalone => {
                crate::inner::ush_service_ask(request, self.workspace().as_deref())?
            }
            Source::None => {
                return Err("environments are managed through Tilo's shell runtime".into());
            }
            #[cfg(test)]
            Source::Scripted(_) => {
                return Err("environments are managed through Tilo's shell runtime".into());
            }
        };
        parse_directory_reply(&reply)
    }
}

pub(crate) fn with_workspace(request: &mut serde_json::Value, workspace: Option<&str>) {
    if let (Some(fields), Some(workspace)) = (request.as_object_mut(), workspace) {
        fields.insert("workspace".into(), serde_json::json!(workspace));
    }
}

pub(crate) fn parse_directory_reply(reply: &str) -> Result<Vec<EnvironmentSummary>, String> {
    #[derive(serde::Deserialize)]
    struct Entry {
        key: String,
        name: String,
        #[serde(default)]
        tools: Vec<String>,
        #[serde(default)]
        grants: Vec<(String, bool)>,
    }
    #[derive(serde::Deserialize)]
    #[serde(tag = "result", rename_all = "kebab-case")]
    enum Reply {
        Environments { environments: Vec<Entry> },
        Denied { reason: String },
    }
    match serde_json::from_str::<Reply>(reply.trim()).map_err(|e| e.to_string())? {
        Reply::Environments { environments } => Ok(environments
            .into_iter()
            .filter(|entry| tilo_environment::is_record_key(&entry.key))
            .map(|entry| EnvironmentSummary {
                key: entry.key,
                name: entry.name,
                tools: entry.tools,
                grants: entry.grants,
            })
            .collect()),
        Reply::Denied { reason } => Err(reason),
    }
}

pub(crate) fn legacy_vms_under(data_home: &std::path::Path) -> Vec<String> {
    ["linux", "windows"]
        .into_iter()
        .filter(|guest| {
            data_home
                .join("tilo/legacy")
                .join(guest)
                .join("disk.qcow2")
                .is_file()
        })
        .map(str::to_owned)
        .collect()
}
