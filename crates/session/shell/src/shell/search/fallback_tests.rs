//! Where the rows that answer any query go: asking an agent and searching
//! the web follow a direct match, and lead when nothing matched directly,
//! so Enter (the first row) runs the match or hands the query on.

use std::rc::Rc;

use agent_provider_config::EndpointLocation;
use automerge::AutoCommit;
use ui::search::system::SearchScope;

use crate::shell::chrome_state::{Chrome, ChromeState};
use crate::shell::i18n::{t, t_args};
use crate::shell::search::action_api::ActionArgs;
use crate::shell::search::providers::ask_agent::PROVIDER_ID as ASK;
use crate::shell::search::providers::desktop_sources::WEB_ID as WEB;
use crate::shell::search::providers::{AskAgentProvider, WebProvider};
use crate::shell::search::{
    QueryArgs, ResourceKind, SearchContext, SearchProvider, SearchRegistry, SearchResult,
};
use crate::shell::state::FreshId as _;
use crate::shell::state::{Workspace, WorkspaceId, WorkspaceTree, WorkspaceTreeHandle};

const NOTES: &str = "notes";

/// A workspace's notes, found by any word of their titles, as a content
/// provider finds what is inside them.
struct Notes {
    kinds: Vec<ResourceKind>,
}

impl SearchProvider for Notes {
    fn id(&self) -> &str {
        NOTES
    }
    fn name(&self) -> &str {
        NOTES
    }
    fn resource_kinds(&self) -> &[ResourceKind] {
        &self.kinds
    }
    fn query(&self, ctx: SearchContext<'_>) -> Vec<SearchResult> {
        let words: Vec<String> = ctx
            .needle
            .split_whitespace()
            .map(str::to_lowercase)
            .collect();
        ["Quarterly report", "Offsite plan"]
            .into_iter()
            .filter(|title| {
                let title = title.to_lowercase();
                words.iter().any(|word| title.contains(word.as_str()))
            })
            .map(|title| {
                SearchResult::nullary(
                    ResourceKind::Note,
                    NOTES,
                    title,
                    title,
                    None,
                    // Found by a word inside, as content is: low on the
                    // provider's scale.
                    20,
                    |_, _, _| {},
                )
            })
            .collect()
    }
}

/// The desktop in front: one workspace, with no agent admitted to it:
/// asking makes a new agent for the task.
fn desktop() -> Chrome {
    let mut tree = WorkspaceTree::default();
    let id = WorkspaceId::new();
    let mut workspace = Workspace::new(id, "Plans");
    workspace.set_owner("shell-fixture");
    tree.desktop.workspaces.push(workspace);
    tree.desktop.active = id;
    ChromeState::new_with_fixture_handle(
        WorkspaceTreeHandle::new(tree),
        crate::shell::hosted_inputs::HostedTileInputs::default(),
    )
}

fn offline(chrome: &Chrome) {
    chrome.borrow_mut().quick_settings.status.link = Some(os_actions::LinkStatus {
        kind: os_actions::LinkKind::Offline,
        name: None,
        signal: 0,
    });
}

/// Search with the notes, agents whose model is at `model` (none without),
/// an agent runtime when `runtime`, and the web.
fn search(runtime: bool, model: Option<EndpointLocation>) -> SearchRegistry {
    let mut registry = SearchRegistry::new();
    registry.register(Rc::new(Notes {
        kinds: vec![ResourceKind::Note],
    }));
    registry.register(Rc::new(AskAgentProvider::with_runtime_and_models(
        move || runtime,
        move |_| model,
    )));
    registry.register(Rc::new(WebProvider::with_installed(|_| None)));
    registry
}

fn rows(registry: &SearchRegistry, chrome: &Chrome, needle: &str) -> Vec<SearchResult> {
    let document = AutoCommit::new();
    let state = chrome.borrow();
    registry.query(&QueryArgs {
        needle,
        scope: SearchScope::All,
        active_provider_id: None,
        wanted_kinds: &[],
        chrome: &state,
        doc: &document,
        doc_root: &automerge::ROOT,
    })
}

fn order(rows: &[SearchResult]) -> Vec<(&str, &str)> {
    rows.iter()
        .map(|row| (row.provider_id.as_str(), row.title.as_str()))
        .collect()
}

/// Enter: run the first row.
fn enter(rows: &[SearchResult], chrome: &Chrome) {
    let first = rows.first().expect("a row to run");
    let mut doc = AutoCommit::new();
    (first.on_open)(
        &mut ui::widget::Edits::lend(&mut doc),
        &automerge::ROOT,
        chrome,
        &ActionArgs::none(),
    )
    .expect("the first row ran");
}

fn ask_row(query: &str) -> String {
    t_args("search-ask-agent", &[("query", query.into())])
}

fn web_row(query: &str) -> String {
    format!("Search the web for {query}")
}

#[test]
fn a_direct_match_leads_and_enter_runs_it_with_asking_and_the_web_after() {
    let chrome = desktop();
    let found = rows(
        &search(true, Some(EndpointLocation::Hosted)),
        &chrome,
        "quarterly report",
    );
    assert_eq!(
        order(&found),
        [
            (NOTES, "Quarterly report"),
            (ASK, ask_row("quarterly report").as_str()),
            (WEB, web_row("quarterly report").as_str()),
        ]
    );
    enter(&found, &chrome);
    assert!(
        chrome.borrow_mut().take_agent_asks_in_place().is_empty(),
        "Enter opened the note and asked no agent"
    );
}

#[test]
fn with_only_a_weak_match_enter_hands_the_query_to_the_agent() {
    let chrome = desktop();
    let query = "budget for the offsite";
    let found = rows(
        &search(true, Some(EndpointLocation::Hosted)),
        &chrome,
        query,
    );
    assert_eq!(
        order(&found),
        [
            (ASK, ask_row(query).as_str()),
            (WEB, web_row(query).as_str()),
            (NOTES, "Offsite plan"),
        ],
        "a note found by one word of a sentence is not what was asked for"
    );
    assert_eq!(
        found[0].subtitle.as_deref(),
        Some(
            t_args(
                "search-ask-agent-subtitle",
                &[("agent", t("agent-quick-new-agent").into())]
            )
            .as_str()
        )
    );
    enter(&found, &chrome);
    assert_eq!(
        chrome.borrow_mut().take_agent_asks_in_place(),
        [query.to_owned()],
        "the agent is asked the query about what is focused, in the composer there"
    );
    assert!(
        chrome.borrow_mut().take_agent_asks().is_empty(),
        "no page opens: the answer shows in place"
    );
}

#[test]
fn with_no_match_asking_the_agent_leads_and_the_web_follows() {
    let chrome = desktop();
    let found = rows(
        &search(true, Some(EndpointLocation::ThisDevice)),
        &chrome,
        "zzqx",
    );
    assert_eq!(
        order(&found),
        [
            (ASK, ask_row("zzqx").as_str()),
            (WEB, web_row("zzqx").as_str())
        ]
    );
    enter(&found, &chrome);
    assert_eq!(chrome.borrow_mut().take_agent_asks_in_place().len(), 1);
}

#[test]
fn an_agent_that_cannot_take_it_says_why_and_the_web_moves_up() {
    for (chrome, runtime, model, why) in [
        (
            desktop(),
            false,
            Some(EndpointLocation::Hosted),
            "search-ask-agent-no-agent",
        ),
        (desktop(), true, None, "search-ask-agent-no-model"),
        (
            {
                let chrome = desktop();
                offline(&chrome);
                chrome
            },
            true,
            Some(EndpointLocation::Hosted),
            "search-ask-agent-offline",
        ),
    ] {
        let found = rows(&search(runtime, model), &chrome, "zzqx");
        assert_eq!(
            order(&found),
            [
                (WEB, web_row("zzqx").as_str()),
                (ASK, ask_row("zzqx").as_str())
            ],
            "{why}"
        );
        assert_eq!(found[1].subtitle.as_deref(), Some(t(why).as_str()));
        // And after a direct match too.
        let found = rows(&search(runtime, model), &chrome, "offsite plan");
        assert_eq!(
            order(&found),
            [
                (NOTES, "Offsite plan"),
                (WEB, web_row("offsite plan").as_str()),
                (ASK, ask_row("offsite plan").as_str()),
            ],
            "{why}"
        );
    }
}

#[test]
fn a_model_on_this_device_answers_offline() {
    let chrome = desktop();
    offline(&chrome);
    let found = rows(
        &search(true, Some(EndpointLocation::ThisDevice)),
        &chrome,
        "zzqx",
    );
    assert_eq!(found[0].provider_id, ASK, "{:?}", order(&found));
}

#[test]
fn with_no_agent_the_row_opens_where_one_is_added() {
    let chrome = desktop();
    let found = rows(
        &search(false, Some(EndpointLocation::Hosted)),
        &chrome,
        "zzqx",
    );
    let ask = found
        .iter()
        .find(|row| row.provider_id == ASK)
        .expect("the row stays, saying why");
    enter(std::slice::from_ref(ask), &chrome);
    let state = chrome.borrow();
    assert!(state.overlays.ws_settings);
    assert_eq!(
        state.ws_settings_tab,
        crate::shell::chrome_state::WsTab::Agents
    );
    drop(state);
    assert!(chrome.borrow_mut().take_agent_asks().is_empty());
}
