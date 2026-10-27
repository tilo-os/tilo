//! Asking an agent: when nothing the person has matches what they typed,
//! the first suggestion hands it to an agent as a task.
//!
//! "Ask an agent: …" answers every query, as "Search the web for …" does
//! ([`SearchProvider::is_fallback`]). Choosing it asks the workspace's agent
//! through the one way the desktop asks an agent for work
//! ([`ChromeState::ask_agent`]): an issue in Tasks assigned to the agent,
//! whose run the asking starts (`ask-agent`), and the issue's page opens so
//! the person watches it. Nothing here keeps a task of its own.
//!
//! When the agent cannot take it (the workspace has none, its model is not
//! set up, or the model is on the internet and this device is offline) the
//! row says why in plain words, ranks after searching the web
//! ([`SearchProvider::fallback_rank`]), and opens the place that fixes it.
//!
//! [`ChromeState::ask_agent`]: crate::shell::chrome_state::ChromeState::ask_agent

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

/// The longest task title, in characters; the whole query is its
/// description.
const TITLE_CHARS: usize = 120;

/// Whether the workspace's agent can take a request now, and if not, why.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Readiness {
    /// `agent` takes requests in `room`.
    Ready { room: RoomId, agent: String },
    /// No agent works in the workspace in front.
    NoAgent,
    /// The workspace's agent has no model to plan with.
    NoModel,
    /// The agent's model is on the internet and this device is offline.
    Offline,
}

impl Readiness {
    /// The readiness of the workspace in front: its first agent, where that
    /// agent's model runs (`model_of`, `None` with no model), and whether
    /// the device's link is down. An unknown link is not called offline.
    pub fn of(
        chrome: &ChromeState,
        model_of: impl FnOnce(&str) -> Option<EndpointLocation>,
    ) -> Self {
        let found = {
            let tree = chrome.tree();
            tree.desktop.active_workspace().and_then(|workspace| {
                let (agent, _) = workspace.agent_members().into_iter().next()?;
                Some((workspace.room(), agent))
            })
        };
        let Some((room, agent)) = found else {
            return Self::NoAgent;
        };
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

    /// What the row says under its title: whom it asks, or why it cannot.
    fn subtitle(&self) -> String {
        match self {
            Self::Ready { agent, .. } => t_args(
                "search-ask-agent-subtitle",
                &[("agent", agent.clone().into())],
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
        Self {
            kinds: vec![ResourceKind::Custom(t("search-kind-agent"))],
            model_of: Box::new(model_of),
        }
    }

    fn kind(&self) -> ResourceKind {
        self.kinds
            .first()
            .cloned()
            .unwrap_or_else(|| ResourceKind::Custom(t("search-kind-agent")))
    }

    fn readiness(&self, chrome: &ChromeState) -> Readiness {
        Readiness::of(chrome, |agent| (self.model_of)(agent))
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
    let settings = |page: &str| {
        crate::shell::uri_open::open_in_workspace(&format!("settings://{page}"), chrome)
            .map(drop)
            .map_err(|error| ActionError::Explained(error.user_message()))
    };
    match readiness {
        Readiness::Ready { room, agent } => {
            let (title, body) = request(query);
            let mut state = chrome.borrow_mut();
            state.ask_agent_and_show(*room, agent.clone(), (title, body));
            state.bump();
            Ok(())
        }
        Readiness::NoAgent => {
            let mut state = chrome.borrow_mut();
            state.open_workspace_settings(WsTab::Agents);
            state.bump();
            Ok(())
        }
        Readiness::NoModel => settings("agent_models"),
        Readiness::Offline => settings("network"),
    }
}

/// The task a query asks for: its first line as the title, the whole query
/// as what the agent is asked.
fn request(query: &str) -> (String, String) {
    let query = query.trim();
    let title = query
        .lines()
        .next()
        .unwrap_or_default()
        .chars()
        .take(TITLE_CHARS)
        .collect();
    (title, query.to_owned())
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
    fn the_task_is_titled_by_the_querys_first_line_and_asks_all_of_it() {
        assert_eq!(
            request("  what changed in the Q3 numbers?  "),
            (
                "what changed in the Q3 numbers?".into(),
                "what changed in the Q3 numbers?".into()
            )
        );
        let long = "a".repeat(400);
        let (title, body) = request(&long);
        assert_eq!(title.chars().count(), TITLE_CHARS);
        assert_eq!(body, long);
    }

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
