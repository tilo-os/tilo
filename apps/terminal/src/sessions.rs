//! How a terminal tab reaches the sessions it opens, and what it knows about
//! the places they can run.
//!
//! A tab opens its session lazily, at its first sync, because only then does
//! it know its record — and the record names the target. A new tab has none
//! yet and takes the default ([`crate::target::default_target`]).

use std::cell::RefCell;
use std::rc::Rc;

use crate::inner::{OpenError, Requester, TerminalBackend};
use crate::target::{EnvironmentSummary, RecentTarget, SessionTarget};

/// Where this tab's session stands.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum OpenState {
    /// No session will be opened here (headless rendering, replicas, a fixed
    /// command).
    Fixed,
    /// Waiting for the first sync to learn the target.
    Unopened,
    /// A supervisor is opening the session off the UI thread.
    Opening,
    /// A session is attached.
    Open,
    /// The device's own shell needs the person's allow; the shell is asking.
    AwaitingAllow,
    /// The target could not be opened; the reason is shown on the tab.
    Failed(String),
    /// No environment can be built on this device (no `env-runtime` image is
    /// installed). The tab says so and offers the device's own shell.
    NoRuntime(String),
}

pub(crate) type OpenAnswer = (
    Requester,
    Option<String>,
    Result<TerminalBackend, OpenError>,
);

enum Source {
    /// Never opens a session.
    None,
    /// The standalone app: [`TerminalBackend::open`] routes per target.
    Standalone,
    /// Hosted in-process by the shell: the session supervisor below the
    /// runtime directory. The first connection was handed over at
    /// construction; later ones are made the same way the host made it.
    Hosted {
        first: Option<crate::HostedProcessServices>,
    },
    /// Opens through a script — for tests. Behaves as a shell-hosted tab.
    #[cfg(test)]
    Scripted(Box<ScriptedOpen>),
}

/// How a scripted source answers an open.
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

/// The tab's session source, target and the places it can run.
pub struct Sessions {
    source: Source,
    pending: Option<crate::async_open::PendingOpen>,
    pub state: OpenState,
    /// The target the current (or last attempted) session runs on.
    pub target: Option<SessionTarget>,
    recent: RecentTarget,
    /// Environments the picker offers, as last listed.
    pub environments: Vec<EnvironmentSummary>,
    /// The person's relays an environment can run on, as last listed.
    pub relays: Vec<crate::target::RelaySummary>,
    /// Legacy VMs the legacy VM service reports. Empty until that service
    /// exists, which keeps them out of the picker.
    pub legacy_vms: Vec<String>,
    allow: Option<Rc<RefCell<ui::widget::HostShellAllow>>>,
    intent: Option<ui::widget::AppHostIntent>,
    /// The person took the tab's offer of the device's own shell; opened at
    /// the next sync.
    pub host_requested: bool,
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
            legacy_vms: Vec::new(),
            relays: Vec::new(),
            allow: None,
            intent: None,
            host_requested: false,
        }
    }

    /// A tab that never opens a session.
    pub fn none() -> Self {
        Self::with(Source::None, OpenState::Fixed, RecentTarget::none())
    }

    /// The standalone app.
    pub fn standalone() -> Self {
        Self::with(
            Source::Standalone,
            OpenState::Unopened,
            RecentTarget::discover(),
        )
    }

    /// Hosted by the shell, with the first supervisor connection.
    pub fn hosted(first: crate::HostedProcessServices) -> Self {
        Self::with(
            Source::Hosted { first: Some(first) },
            OpenState::Unopened,
            RecentTarget::discover(),
        )
    }

    /// A real host grants discovery, without connecting on its UI thread.
    pub(crate) fn hosted_deferred() -> Self {
        Self::with(
            Source::Hosted { first: None },
            OpenState::Unopened,
            RecentTarget::discover(),
        )
    }

    /// A tab whose sessions open through `open` — for tests.
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

    /// Whether this tab opens sessions at all.
    pub fn is_live(&self) -> bool {
        !matches!(self.source, Source::None)
    }

    /// Whether an environment can run from here.
    pub fn environments_available(&self) -> bool {
        match self.source {
            Source::None => false,
            Source::Standalone => crate::inner::environments_available(),
            Source::Hosted { .. } => true,
            #[cfg(test)]
            Source::Scripted(_) => true,
        }
    }

    /// The target a new tab opens on.
    pub fn default_target(&self) -> SessionTarget {
        crate::target::default_target(self.recent.load().as_ref(), self.environments_available())
    }

    /// Remember an environment the person opened, for the next new tab.
    pub fn remember(&self, target: &SessionTarget) {
        self.recent.remember(target);
    }

    /// Open a session on `target`.
    ///
    /// # Errors
    /// Returns why it did not open; see [`OpenError`].
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
        match &mut self.source {
            Source::None => Err(OpenError::Unavailable(
                "this terminal has no process service".into(),
            )),
            Source::Standalone => TerminalBackend::open(target, requested_by, relay_session),
            Source::Hosted { first } => crate::HostedProcessServices::open_session(
                first.take(),
                target,
                requested_by,
                relay_session,
                None,
            ),
            #[cfg(test)]
            Source::Scripted(open) => open(target, requested_by, relay_session),
        }
    }

    /// Start a hosted open on a worker; other sources retain their own routing.
    pub(crate) fn begin_open(
        &mut self,
        target: &SessionTarget,
        requester: Requester,
        attach: Option<&str>,
        waker: std::sync::Arc<std::sync::Mutex<ui::WakeHandle>>,
    ) -> Option<Result<TerminalBackend, OpenError>> {
        {
            // Cancels any superseded request before consuming another connection.
            self.pending = None;
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

    /// Ask the shell to put the device's own shell to the person, once.
    /// Hosted and native tabs both use the trusted session shell; an unavailable
    /// shell answers with a refusal.
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

    /// The person's answer to [`Self::request_allow`], once given.
    pub fn take_allow_answer(&mut self) -> Option<bool> {
        let answer = self.allow.as_ref()?.borrow().answer?;
        self.allow = None;
        Some(answer)
    }

    /// The pending request for the shell, if any.
    pub fn take_intent(&mut self) -> Option<ui::widget::AppHostIntent> {
        self.intent.take()
    }

    /// Refresh the environment list from the supervisor. A failure keeps
    /// the last list: the picker always offers the default environment.
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
            return;
        }
        if let Ok(list) = self.directory_request(&serde_json::json!({"op": "list-environments"})) {
            self.environments = list;
        }
        if matches!(self.source, Source::Hosted { .. })
            && let Ok(reply) =
                crate::HostedProcessServices::ask(&serde_json::json!({"op": "list-relays"}))
        {
            self.relays = parse_relays_reply(&reply);
        }
    }

    /// Create an environment named `name`; returns its record key.
    ///
    /// # Errors
    /// Returns the supervisor's reason.
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
        self.environments
            .iter()
            .find(|env| !before.contains(&env.key))
            .map(|env| env.key.clone())
            .ok_or_else(|| "the environment was not created".into())
    }

    /// Rename the environment `record`.
    ///
    /// # Errors
    /// Returns the supervisor's reason.
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
    ///
    /// # Errors
    /// Returns the supervisor's reason.
    pub fn grant_folder(&mut self, record: &str, path: &str, writable: bool) -> Result<(), String> {
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
        // Only a shell-hosted tab reaches the supervisor's directory, and
        // never over its first connection: that one carries the session.
        if !matches!(self.source, Source::Hosted { .. }) {
            return Err("environments are managed through Tilo's shell runtime".into());
        }
        parse_directory_reply(&crate::HostedProcessServices::ask(request)?)
    }
}

/// Decode the supervisor's environment list.
///
/// # Errors
/// Returns the supervisor's reason, or why the reply was not understood.
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

/// The legacy VMs this user has, as the legacy VM service keeps them: one per
/// guest OS, under `<data home>/tilo/legacy/<guest>/`, present once its disk
/// exists. None until that service has made one.
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

/// Decode the supervisor's list of the person's relays; anything else is none.
pub(crate) fn parse_relays_reply(reply: &str) -> Vec<crate::target::RelaySummary> {
    #[derive(serde::Deserialize)]
    struct Entry {
        relay: String,
        #[serde(default)]
        label: String,
    }
    #[derive(serde::Deserialize)]
    #[serde(tag = "result", rename_all = "kebab-case")]
    enum Reply {
        Relays { relays: Vec<Entry> },
    }
    match serde_json::from_str::<Reply>(reply.trim()) {
        Ok(Reply::Relays { relays }) => relays
            .into_iter()
            .filter(|entry| {
                crate::target::Location::Relay {
                    relay: entry.relay.clone(),
                }
                .is_valid()
            })
            .map(|entry| crate::target::RelaySummary {
                relay: entry.relay,
                label: entry.label,
            })
            .collect(),
        Err(_) => Vec::new(),
    }
}
