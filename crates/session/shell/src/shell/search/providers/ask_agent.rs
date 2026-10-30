//! Asking an agent: when nothing the person has matches what they typed,
//! the first suggestion asks the workspace's agent.
//!
//! "Ask an agent: …" answers every query, as "Search the web for …" does
//! ([`SearchProvider::is_fallback`]). Choosing it asks the agent about what
//! is focused, the selection in the focused window or else the window, in
//! the agent composer beside it, where the answer shows
//! ([`ChromeState::ask_agent_in_place`]): the same way the ask-agent chord
//! and a sentence typed into a context menu ask. What the agent takes on as
//! work becomes an issue in Tasks, as all its work does. Nothing here keeps
//! a task of its own.
//!
//! Asking makes a new agent for the task ([`crate::shell::task_agents`]),
//! so asking is possible whenever an agent runtime can be launched and a
//! model is configured. When it cannot be (no agent runtime is installed,
//! no model is set up, or the model is on the internet and this device is
//! offline) the row says why in plain words, ranks after searching the web
//! ([`SearchProvider::fallback_rank`]), and opens the place that fixes it;
//! the composer offers the same fix in place ([`open_fix`]).
//!
//! [`ChromeState::ask_agent_in_place`]: crate::shell::chrome_state::ChromeState::ask_agent_in_place

use std::cell::RefCell;

use agent_provider_config::{EndpointLocation, ProviderConfig};
use ui::icons::Icon;

use crate::shell::chrome_state::{Chrome, ChromeState, WsTab};
use crate::shell::i18n::{t, t_args};
use crate::shell::search::action_api::ActionError;
use crate::shell::search::{ResourceKind, SearchContext, SearchProvider, SearchResult};
use crate::shell::state::RoomId;

pub const PROVIDER_ID: &str = "ask-agent";

/// The row's address: `ask:<query>`.
const ASK_ID: &str = "ask";

/// Whether the workspace's agent can take a request now, and if not, why.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Readiness {
    /// A new agent made for the task takes requests in `room`.
    Ready { room: RoomId, agent: String },
    /// No agent can run: the agent runtime is not installed, or there is no
    /// workspace in front.
    NoAgent,
    /// The workspace's agent has no model to plan with.
    NoModel,
    /// The agent's model is on the internet and this device is offline.
    Offline,
}

impl Readiness {
    /// The readiness of the workspace in front for a new agent made for the
    /// task: whether an agent runtime can be launched (`runtime`), where the
    /// agents' model runs (`model_of`, `None` with no model), and whether
    /// the device's link is down. An unknown link is not called offline.
    pub fn of(
        chrome: &ChromeState,
        runtime: bool,
        model_of: impl FnOnce(&str) -> Option<EndpointLocation>,
    ) -> Self {
        let room = chrome
            .tree()
            .desktop
            .active_workspace()
            .map(crate::shell::state::Workspace::room);
        let Some(room) = room.filter(|_| runtime) else {
            return Self::NoAgent;
        };
        let agent = crate::shell::task_agents::NEW_TASK_AGENT.to_owned();
        let offline = chrome
            .quick_settings
            .status
            .link
            .as_ref()
            .is_some_and(|link| link.kind == os_actions::LinkKind::Offline);
        match model_of(&agent) {
            None => Self::NoModel,
            Some(EndpointLocation::Hosted) if offline => Self::Offline,
            Some(_) => Self::Ready { room, agent },
        }
    }

    fn ready(&self) -> bool {
        matches!(self, Self::Ready { .. })
    }

    /// What keeps a new agent from being asked, as the composer says it.
    #[must_use]
    pub(crate) fn blocker(&self) -> Option<crate::shell::agent_quick_composer::AskBlocker> {
        use crate::shell::agent_quick_composer::AskBlocker;
        match self {
            Self::Ready { .. } => None,
            Self::NoAgent => Some(AskBlocker::NoRuntime),
            Self::NoModel => Some(AskBlocker::NoModel),
            Self::Offline => Some(AskBlocker::Offline),
        }
    }

    /// What the row says under its title: whom it asks, or why it cannot.
    fn subtitle(&self) -> String {
        match self {
            Self::Ready { agent, .. } => t_args(
                "search-ask-agent-subtitle",
                &[(
                    "agent",
                    if agent == crate::shell::task_agents::NEW_TASK_AGENT {
                        t("agent-quick-new-agent")
                    } else {
                        agent.clone()
                    }
                    .into(),
                )],
            ),
            Self::NoAgent => t("search-ask-agent-no-agent"),
            Self::NoModel => t("search-ask-agent-no-model"),
            Self::Offline => t("search-ask-agent-offline"),
        }
    }
}

/// Where `agent`'s model runs, from the person's provider settings: the one
/// chosen for it, else the default (`ProviderConfig::selection_for`, the
/// same choice the agent is started with).
fn model_location(config: &ProviderConfig, agent: &str) -> Option<EndpointLocation> {
    config
        .selection_for(agent)
        .map(|(profile, _)| profile.location)
}

/// Where an agent's model runs, or `None` while it has none.
type ModelOf = Box<dyn Fn(&str) -> Option<EndpointLocation>>;

pub struct AskAgentProvider {
    kinds: Vec<ResourceKind>,
    model_of: ModelOf,
    /// Whether an agent runtime can be launched here.
    runtime: Box<dyn Fn() -> bool>,
}

impl Default for AskAgentProvider {
    fn default() -> Self {
        // Read again only when the provider settings change: a keystroke
        // never reads the settings file.
        let cached: RefCell<Option<(u64, ProviderConfig)>> = RefCell::new(None);
        Self::with_models(move |agent| {
            let epoch = crate::shell::settings_records::provider_settings_epoch();
            let mut cached = cached.borrow_mut();
            if cached.as_ref().is_none_or(|(seen, _)| *seen != epoch) {
                *cached = Some((epoch, ProviderConfig::load()));
            }
            cached
                .as_ref()
                .and_then(|(_, config)| model_location(config, agent))
        })
    }
}

impl AskAgentProvider {
    pub fn new() -> Self {
        Self::default()
    }

    /// A provider that reads where an agent's model runs from `model_of`.
    pub(crate) fn with_models(
        model_of: impl Fn(&str) -> Option<EndpointLocation> + 'static,
    ) -> Self {
        Self::with_runtime_and_models(
            crate::shell::launcher::task_agent_runtime_installed,
            model_of,
        )
    }

    /// A provider that asks `runtime` whether an agent can be launched and
    /// reads where an agent's model runs from `model_of`.
    pub(crate) fn with_runtime_and_models(
        runtime: impl Fn() -> bool + 'static,
        model_of: impl Fn(&str) -> Option<EndpointLocation> + 'static,
    ) -> Self {
        Self {
            kinds: vec![ResourceKind::Custom(t("search-kind-agent"))],
            model_of: Box::new(model_of),
            runtime: Box::new(runtime),
        }
    }

    fn kind(&self) -> ResourceKind {
        self.kinds
            .first()
            .cloned()
            .unwrap_or_else(|| ResourceKind::Custom(t("search-kind-agent")))
    }

    fn readiness(&self, chrome: &ChromeState) -> Readiness {
        Readiness::of(chrome, (self.runtime)(), |agent| (self.model_of)(agent))
    }

    fn row(&self, query: &str, readiness: Readiness) -> SearchResult {
        let subtitle = readiness.subtitle();
        let typed = query.to_owned();
        SearchResult::nullary_fallible(
            self.kind(),
            PROVIDER_ID,
            format!("{ASK_ID}:{query}"),
            t_args("search-ask-agent", &[("query", query.into())]),
            Some(subtitle),
            1,
            move |_doc, _root, chrome| choose(&typed, &readiness, chrome),
        )
    }
}

/// What choosing the row does: ask the agent, or open the place that makes
/// asking possible.
fn choose(query: &str, readiness: &Readiness, chrome: &Chrome) -> Result<(), ActionError> {
    match readiness.blocker() {
        // Asked about what is focused, in the composer beside it, where
        // the answer shows; work it becomes is an issue in Tasks as ever.
        None => {
            let mut state = chrome.borrow_mut();
            state.ask_agent_in_place(query.trim().to_owned());
            state.bump();
            Ok(())
        }
        Some(blocker) => open_fix(blocker, chrome),
    }
}

/// Open the place that fixes `blocker`: the palette's row and the composer's
/// Set up button both go here.
///
/// # Errors
/// The settings page could not be opened.
pub(crate) fn open_fix(
    blocker: crate::shell::agent_quick_composer::AskBlocker,
    chrome: &Chrome,
) -> Result<(), ActionError> {
    use crate::shell::agent_quick_composer::AskBlocker;
    let settings = |page: &str| {
        crate::shell::uri_open::open_in_workspace(&format!("settings://{page}"), chrome)
            .map(drop)
            .map_err(|error| ActionError::Explained(error.user_message()))
    };
    match blocker {
        AskBlocker::NoRuntime => {
            let mut state = chrome.borrow_mut();
            state.open_workspace_settings(WsTab::Agents);
            state.bump();
            Ok(())
        }
        AskBlocker::NoModel => settings("agent_models"),
        AskBlocker::Offline => settings("network"),
    }
}

/// What keeps a new agent made for the task from being asked in front now,
/// read as the palette's row reads it: `None` when nothing does.
#[must_use]
pub(crate) fn task_agent_blocker(
    chrome: &ChromeState,
) -> Option<crate::shell::agent_quick_composer::AskBlocker> {
    let config = ProviderConfig::load();
    Readiness::of(
        chrome,
        crate::shell::launcher::task_agent_runtime_installed(),
        |agent| model_location(&config, agent),
    )
    .blocker()
}

impl SearchProvider for AskAgentProvider {
    fn id(&self) -> &'static str {
        PROVIDER_ID
    }

    fn name(&self) -> &'static str {
        "Ask an agent"
    }

    fn display_name(&self) -> String {
        t("search-provider-ask-agent")
    }

    fn resource_kinds(&self) -> &[ResourceKind] {
        &self.kinds
    }

    /// Whether the agent can take a request changes with the workspace in
    /// front, its agents, the provider settings and the link.
    fn search_revision(&self, ctx: &SearchContext<'_>) -> String {
        format!("ask-agent:{:?}", self.readiness(ctx.chrome))
    }

    fn is_fallback(&self, _result: &SearchResult) -> bool {
        true
    }

    /// First among the rows that answer any query while the agent can take
    /// it; after searching the web while it cannot.
    fn fallback_rank(&self, result: &SearchResult, chrome: &ChromeState) -> u8 {
        let ready = result
            .id
            .split_once(':')
            .is_some_and(|(row, _)| row == ASK_ID)
            && self.readiness(chrome).ready();
        if ready { 0 } else { 2 }
    }

    fn row_icon(&self, _result: &SearchResult) -> Option<Icon> {
        Some(Icon::Bot)
    }

    fn resolve(&self, id: &str, ctx: SearchContext<'_>) -> Option<SearchResult> {
        let (row, query) = id.split_once(':')?;
        (row == ASK_ID && !query.trim().is_empty())
            .then(|| self.row(query, self.readiness(ctx.chrome)))
    }

    fn query(&self, ctx: SearchContext<'_>) -> Vec<SearchResult> {
        let typed = ctx.needle.trim();
        if typed.is_empty() || !ctx.wanted.contains(&self.kind()) {
            return Vec::new();
        }
        vec![self.row(typed, self.readiness(ctx.chrome))]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_agent_is_ready_with_a_model_it_can_reach() {
        use agent_provider_config::{ModelDescriptor, ProviderProfile, ProviderProtocol};
        let mut profile = ProviderProfile::hosted(ProviderProtocol::OpenAiCompatible);
        profile.models = vec![ModelDescriptor::new("small")];
        let mut config = ProviderConfig {
            default_profile: Some(profile.id.clone()),
            profiles: vec![profile],
            ..ProviderConfig::default()
        };
        assert_eq!(
            model_location(&config, "tide"),
            None,
            "a provider with no default model gives the agent none"
        );
        config.profiles[0].default_model = Some("small".into());
        assert_eq!(
            model_location(&config, "tide"),
            Some(EndpointLocation::Hosted)
        );
        config.profiles[0].enabled = false;
        assert_eq!(model_location(&config, "tide"), None);
    }
}
