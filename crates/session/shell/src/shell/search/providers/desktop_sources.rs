//! Desktop search's own sources as palette providers: what a query that is
//! not a name answers (a sum, a conversion, money, an address) and the web
//! searches it can be. The rows are [`tilo_desktop_search::providers`]'s,
//! the same every desktop offers; this module runs what choosing one asks.

use tilo_desktop_search::providers::{Answers, WebSearch, answers_kind, web_kind, websites_kind};
use tilo_desktop_search::{Hit, Source as _, Target};

use crate::shell::chrome_state::Chrome;
use crate::shell::i18n::t;
use crate::shell::search::action_api::ActionError;
use crate::shell::search::{ResourceKind, SearchContext, SearchProvider, SearchResult};
use crate::shell::uri_open::{self, UriOpenError};

/// A desktop-search row as the palette's: choosing it does what its target
/// asks.
fn row(hit: Hit) -> SearchResult {
    let target = hit.target;
    let mut row = SearchResult::nullary_fallible(
        hit.kind,
        hit.provider,
        hit.id,
        hit.title,
        hit.subtitle,
        hit.score,
        move |_doc, _root, chrome| perform(&target, chrome),
    );
    row.contract = Some(tilo_actions::ActionContract::view());
    row
}

/// Do what choosing a row with `target` asks.
fn perform(target: &Target, chrome: &Chrome) -> Result<(), ActionError> {
    match target {
        Target::Copy { text } => {
            ui::text_selection::clipboard_copy(text);
            chrome.borrow_mut().acknowledge(t("search-answer-copied"));
            Ok(())
        }
        Target::Open { uri, fallback } => {
            match (uri_open::open_in_workspace(uri, chrome), fallback) {
                // Nothing here takes the scheme (`web+search:`, an app removed
                // since the row was built): the fallback opens.
                (Err(UriOpenError::NoHandler { .. }), Some(fallback)) => {
                    uri_open::open_in_workspace(fallback, chrome)
                        .map(drop)
                        .map_err(uri_refusal)
                }
                (result, _) => result.map(drop).map_err(uri_refusal),
            }
        }
        _ => Err(ActionError::Refused("not a row this provider runs".into())),
    }
}

/// What a person reads when an address could not be opened.
fn uri_refusal(error: UriOpenError) -> ActionError {
    ActionError::Explained(match error {
        UriOpenError::NoHandler { .. } => t("search-answer-no-browser"),
        UriOpenError::NoWorkspace => t("doc-open-no-workspace"),
        UriOpenError::InvalidUri => t("search-answer-bad-address"),
        UriOpenError::Launch(error) => error.user_message(),
    })
}

pub const QUICK_ANSWERS_ID: &str = "quick-answers";

/// What was typed, worked out: money at the rates last fetched
/// ([`crate::shell::net_answers`]).
pub struct QuickAnswersProvider {
    kinds: Vec<ResourceKind>,
}

impl Default for QuickAnswersProvider {
    fn default() -> Self {
        Self {
            kinds: vec![answers_kind()],
        }
    }
}

impl QuickAnswersProvider {
    pub fn new() -> Self {
        Self::default()
    }

    fn source() -> Answers {
        Answers {
            rates: crate::shell::net_answers::rates().map(|kept| {
                tilo_desktop_search::answers::currency::Rates {
                    base: kept.value.base,
                    rates: kept.value.rates,
                    fetched_at: kept.fetched_at,
                }
            }),
            now: tilo_clock::unix_now(),
        }
    }
}

impl SearchProvider for QuickAnswersProvider {
    fn id(&self) -> &'static str {
        QUICK_ANSWERS_ID
    }

    fn name(&self) -> &'static str {
        "Quick answers"
    }

    fn display_name(&self) -> String {
        t("search-provider-quick-answers")
    }

    fn resource_kinds(&self) -> &[ResourceKind] {
        &self.kinds
    }

    fn search_revision(&self, _ctx: &SearchContext<'_>) -> String {
        format!("answers:{}", crate::shell::net_answers::revision())
    }

    fn resolve(&self, id: &str, _ctx: SearchContext<'_>) -> Option<SearchResult> {
        Self::source().resolve(id).map(row)
    }

    fn query(&self, ctx: SearchContext<'_>) -> Vec<SearchResult> {
        if !self.kinds.iter().any(|kind| ctx.wanted.contains(kind)) {
            return Vec::new();
        }
        Self::source()
            .query(ctx.needle)
            .into_iter()
            .map(row)
            .collect()
    }
}

pub const WEB_ID: &str = "web";

/// The installed app that claims `scheme`, by its name; `None` when none
/// does. Read from the launcher's last enumeration, never the registry, so
/// a keystroke does no disk work.
fn installed_app_for_scheme(scheme: &str) -> Option<String> {
    crate::shell::launcher::installed_apps_without_waiting()
        .into_iter()
        .find(|app| app.opens_uri_schemes.iter().any(|claim| claim == scheme))
        .map(|app| app.display_name)
}

/// The web: "Search the web for …" for every query, last, and the web
/// services a query names, searched in the app that serves one when it is
/// installed.
pub struct WebProvider {
    /// `[web search, services]`.
    kinds: Vec<ResourceKind>,
    source: WebSearch,
}

impl Default for WebProvider {
    fn default() -> Self {
        Self::with_installed(installed_app_for_scheme)
    }
}

impl WebProvider {
    pub fn new() -> Self {
        Self::default()
    }

    /// A provider that sees the installed apps `app_for_scheme` reports.
    pub(crate) fn with_installed(app_for_scheme: fn(&str) -> Option<String>) -> Self {
        Self {
            kinds: vec![web_kind(), websites_kind()],
            source: WebSearch { app_for_scheme },
        }
    }
}

impl SearchProvider for WebProvider {
    fn id(&self) -> &'static str {
        WEB_ID
    }

    fn name(&self) -> &'static str {
        "Web"
    }

    fn display_name(&self) -> String {
        t("search-provider-web")
    }

    fn resource_kinds(&self) -> &[ResourceKind] {
        &self.kinds
    }

    /// Which app serves a service changes as apps come and go.
    fn search_revision(&self, _ctx: &SearchContext<'_>) -> String {
        format!("web:{}", crate::shell::launcher::installed_apps_epoch())
    }

    fn is_fallback(&self, result: &SearchResult) -> bool {
        result.id.starts_with("search:")
    }

    fn intent_strength(&self, result: &SearchResult, _needle: &str) -> f32 {
        self.source
            .resolve(&result.id)
            .map_or(0.0, |hit| self.source.intent(&hit))
    }

    fn row_icon(&self, result: &SearchResult) -> Option<ui::icons::Icon> {
        self.source.resolve(&result.id)?.icon
    }

    fn resolve(&self, id: &str, _ctx: SearchContext<'_>) -> Option<SearchResult> {
        self.source.resolve(id).map(row)
    }

    fn query(&self, ctx: SearchContext<'_>) -> Vec<SearchResult> {
        self.source
            .query(ctx.needle)
            .into_iter()
            .filter(|hit| ctx.wanted.contains(&hit.kind))
            .map(row)
            .collect()
    }
}
