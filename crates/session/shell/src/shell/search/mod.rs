//! Cross-app search protocol.
//!
//! The shell's global palette fans out to any number of [`SearchProvider`]s,
//! each owning a domain (shell actions, notes, filesystem, an app's own
//! resources). Providers answer a single `query` call per keystroke — no
//! re-query per resource kind, so a network-backed provider gets exactly one
//! round-trip per input. [`SearchScope::ActiveApp`] narrows the fanout to
//! whichever provider owns the focused tab ([`SearchProvider::owns_tab`])
//! and the providers that keep to the focused app themselves
//! ([`SearchProvider::narrows_to_active_app`]), and nothing else;
//! `Workspace` and `All` fan out to every provider, but `Workspace` ranks
//! resources from the focused app ahead of equally relevant global results.

pub mod action_api;
pub mod app_content_index;
pub mod files_index;
pub mod invocation_wake;
pub mod launcher;
pub mod providers;
pub mod recent_places;
#[cfg(test)]
mod relevance_tests;
pub mod usage;
pub mod visibility;

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::rc::Rc;

use automerge::{AutoCommit, ObjId};

use crate::shell::chrome_state::{Chrome, ChromeState, SearchScope};
use crate::shell::i18n::t;
use action_api::{ActionArgs, ActionError, ActionParam};

use ui::search::system::{self, query_input_is_bounded};

/// Portable search vocabulary and provider page contract, retained at the native API path.
pub use ui::search::system::{Availability, ResourceKind, SearchBudget, SearchCursor};
pub type SearchPage = system::SearchPage<SearchResult>;

/// One page of a retained [`tilo_search::SearchIndex`] query. The cursor
/// carries the next offset, bound to `needle` and `revision`; rows fill the
/// byte budget, and a hit `row` declines still advances the offset.
pub(crate) fn index_page(
    revision: String,
    cursor: Option<&SearchCursor>,
    needle: &str,
    budget: SearchBudget,
    search: impl FnOnce(tilo_search::Page) -> tilo_search::Hits,
    mut row: impl FnMut(&tilo_search::Hit) -> Option<SearchResult>,
) -> SearchPage {
    let offset = match cursor {
        None => 0,
        Some(cursor) if cursor.revision != revision => return SearchPage::stale(revision),
        Some(cursor) => match cursor
            .token
            .split_once('\u{1f}')
            .filter(|(_, query)| *query == needle)
            .and_then(|(offset, _)| offset.parse::<usize>().ok())
        {
            Some(offset) => offset,
            None => return SearchPage::stale(revision),
        },
    };
    let found = search(tilo_search::Page {
        offset,
        limit: budget.max_work.min(budget.max_results),
    });
    let mut considered = 0usize;
    let mut bytes = 0usize;
    let mut results = Vec::new();
    for hit in &found.hits {
        if let Some(row) = row(hit) {
            let row_bytes = row
                .id
                .len()
                .saturating_add(row.title.len())
                .saturating_add(row.secondary_len());
            if results.len() >= budget.max_results
                || bytes.saturating_add(row_bytes) > budget.max_bytes
            {
                break;
            }
            bytes = bytes.saturating_add(row_bytes);
            results.push(row);
        }
        considered = considered.saturating_add(1);
    }
    let exhausted = !found.more && considered == found.hits.len();
    let next = offset.saturating_add(considered);
    SearchPage::continuing(
        revision,
        results,
        (!exhausted).then(|| format!("{next}\u{1f}{needle}")),
    )
}
pub type KindGroup = system::KindGroup<SearchResult>;

/// Map a field rank onto the provider scale; see
/// [`ui::search::system::provider_score`].
pub(crate) use ui::search::system::provider_score;

/// The score of a row another app's own search already matched for
/// `needle`: how the needle meets its visible title, else its subtitle, on
/// the provider scale and never below 1 (the app matched it somewhere).
pub(crate) fn remote_score(needle: &str, title: &str, subtitle: &str) -> i32 {
    use ui::search::{FieldWeight, fold_search_text, rank::match_tier};
    let needle = fold_search_text(needle.trim());
    if needle.is_empty() {
        return 0;
    }
    let rank = match_tier(&needle, &fold_search_text(title))
        .and_then(|tier| tier.rank_in(FieldWeight::TITLE))
        .or_else(|| {
            match_tier(&needle, &fold_search_text(subtitle))?.rank_in(FieldWeight::SUBTITLE)
        })
        .unwrap_or(0);
    provider_score(rank).max(1)
}

/// Keep the `rows` a provider built for one query that match `needle`,
/// each scored by its rank on the provider scale. `fields` holds each row's
/// searchable text, in row order; `index` keeps it indexed while it stays
/// the same.
pub(crate) fn rank_rows(
    index: &ui::search::ListIndex,
    needle: &str,
    rows: Vec<SearchResult>,
    fields: &[Vec<(String, ui::search::FieldWeight)>],
) -> Vec<SearchResult> {
    let ranks = index.ranks(
        needle,
        fields
            .iter()
            .map(|row| row.iter().map(|(text, weight)| (text.as_str(), *weight))),
    );
    rows.into_iter()
        .zip(ranks)
        .filter_map(|(mut row, rank)| {
            row.score = provider_score(rank?);
            Some(row)
        })
        .collect()
}

/// [`rank_rows`] over rows a provider keeps between keystrokes: only the rows
/// that match are copied out.
pub(crate) fn rank_kept_rows(
    index: &ui::search::ListIndex,
    needle: &str,
    rows: &[SearchResult],
    fields: &[Vec<(String, ui::search::FieldWeight)>],
) -> Vec<SearchResult> {
    let ranks = index.ranks(
        needle,
        fields
            .iter()
            .map(|row| row.iter().map(|(text, weight)| (text.as_str(), *weight))),
    );
    rows.iter()
        .zip(ranks)
        .filter_map(|(row, rank)| {
            Some(SearchResult {
                score: provider_score(rank?),
                ..row.clone()
            })
        })
        .collect()
}

/// The chip strip's and group headers' wording for a [`ResourceKind`], in
/// the desktop's language. [`ResourceKind::id`] stays the stable English
/// address; `Custom` kinds surface their own string verbatim.
pub trait ResourceKindText {
    fn localized_label(&self) -> String;
}

impl ResourceKindText for ResourceKind {
    fn localized_label(&self) -> String {
        match self {
            ResourceKind::Action => t("search-kind-actions"),
            ResourceKind::Application => t("search-kind-applications"),
            ResourceKind::Note => t("search-kind-notes"),
            ResourceKind::Record => t("search-kind-records"),
            ResourceKind::File => t("search-kind-files"),
            ResourceKind::Folder => t("search-kind-folders"),
            ResourceKind::Creator => t("search-kind-creators"),
            ResourceKind::Video => t("search-kind-videos"),
            ResourceKind::Playlist => t("search-kind-playlists"),
            ResourceKind::Tab => t("search-kind-tabs"),
            ResourceKind::WindowGroup => t("search-kind-window-groups"),
            ResourceKind::Custom(s) => s.clone(),
        }
    }
}

/// Fired by the shell after the user picks a result, with the live
/// Automerge doc, its root, and the chrome handle to mutate persisted state
/// directly. Returns a [`Result`] because an action can be addressed
/// correctly and still have nothing to act on — "it ran" and "there was
/// nothing here" are different answers a headless caller must tell apart.
pub type OpenFn =
    dyn Fn(&mut ui::widget::Edits<'_>, &ObjId, &Chrome, &ActionArgs) -> Result<(), ActionError>;
pub type InvokeFn = dyn Fn(
    &mut ui::widget::Edits<'_>,
    &ObjId,
    &Chrome,
    &ActionArgs,
) -> Result<ActionReceipt, ActionError>;
/// The longest an exact-address resolve waits for a provider's retained
/// index to finish its first build. An address is valid whether or not a
/// search ran before it; the wait only happens while a build is in flight.
pub const RESOLVE_INDEX_WAIT: std::time::Duration = std::time::Duration::from_secs(3);

/// A non-blocking admission poll. `None` means the broker has not answered
/// yet; the compositor keeps the ticket and polls it again when
/// [`invocation_wake`] wakes it, or when something else does.
pub type PendingInvocation = Rc<dyn Fn() -> Result<Option<ActionReceipt>, ActionError>>;
pub enum InvocationDisposition {
    Accepted(ActionReceipt),
    Pending(PendingInvocation),
}

impl std::fmt::Debug for InvocationDisposition {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Accepted(receipt) => f.debug_tuple("Accepted").field(receipt).finish(),
            Self::Pending(_) => f.write_str("Pending(<admission ticket>)"),
        }
    }
}
pub type BeginInvokeFn = dyn Fn(
    &mut ui::widget::Edits<'_>,
    &ObjId,
    &Chrome,
    &ActionArgs,
) -> Result<InvocationDisposition, ActionError>;

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ActionReceipt {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub broker_invocation_id: Option<u64>,
    pub update: tilo_actions::InvocationUpdate,
}

impl ActionReceipt {
    pub fn completed() -> Self {
        Self {
            broker_invocation_id: None,
            update: tilo_actions::InvocationUpdate::Succeeded {
                invocation: tilo_actions::InvocationId::new(),
                output: tilo_actions::ActionOutput::default(),
            },
        }
    }

    /// A completed local action that answers with `value`.
    pub fn completed_with(value: serde_json::Value) -> Self {
        Self {
            broker_invocation_id: None,
            update: tilo_actions::InvocationUpdate::Succeeded {
                invocation: tilo_actions::InvocationId::new(),
                output: tilo_actions::ActionOutput {
                    value: Some(value),
                    resources: Vec::new(),
                },
            },
        }
    }
}

/// One row the palette will render. `score` is provider-supplied, higher
/// ranks first; a stable scale is expected — prefix matches ~100,
/// substring matches ~50, fuzzy subsequence ~10.
#[derive(Clone)]
pub struct SearchResult {
    pub kind: ResourceKind,
    /// What invoking the row changes. Content/open rows are local views;
    /// canonical app and shell actions preserve their declared operation kind.
    pub operation_kind: tilo_actions::OperationKind,
    pub provider_id: String,
    /// Stable identifier within the provider, used for de-dup.
    pub id: String,
    pub title: String,
    /// Optional secondary line under the title — path, preview snippet,
    /// creator handle.
    pub subtitle: Option<String>,
    /// The secondary line as separate facts (where it lives, what kind it
    /// is, when), laid out as runs of their own with space between them.
    /// Empty: the line is `subtitle`. Never join facts into one string.
    pub facts: Vec<String>,
    /// Optional short live status rendered by trusted shell chrome.
    pub badge: Option<String>,
    /// The keyboard shortcut that does the same, spelled as the active
    /// keyboard layout reads it, shown beside the row.
    pub shortcut: Option<String>,
    pub score: i32,
    /// What an invocation must carry; empty for the nullary majority (see
    /// [`SearchResult::nullary`]).
    pub params: Vec<ActionParam>,
    /// Full operational semantics when the provider represents a canonical
    /// Tilo action. Ordinary content/open rows leave this absent.
    pub contract: Option<tilo_actions::ActionContract>,
    pub on_open: Rc<OpenFn>,
    pub on_invoke: Option<Rc<InvokeFn>>,
    /// Used only when admission itself is asynchronous. It must not claim an
    /// accepted action until its retained ticket resolves current authority.
    pub on_begin_invoke: Option<Rc<BeginInvokeFn>>,
}

const HEADLESS_MAX_PAGES: usize = 256;
const HEADLESS_MAX_RESULTS: usize = 16_384;
const HEADLESS_MAX_BYTES: usize = 16 * 1024 * 1024;
impl SearchResult {
    /// Bytes on the secondary line, whether it is `subtitle` or facts.
    #[must_use]
    pub fn secondary_len(&self) -> usize {
        self.facts.iter().fold(
            self.subtitle.as_ref().map_or(0, String::len),
            |bytes, fact| bytes.saturating_add(fact.len()),
        )
    }

    /// Show the secondary line as these facts ([`Self::facts`]): each its
    /// own run, never one string joined by a glyph.
    #[must_use]
    pub fn with_facts(mut self, facts: impl IntoIterator<Item = String>) -> Self {
        self.facts = facts
            .into_iter()
            .filter(|fact| !fact.trim().is_empty())
            .collect();
        self
    }

    /// Build a row whose invocation takes no arguments — most rows: opening
    /// a note, launching an app, toggling an overlay.
    pub fn nullary(
        kind: ResourceKind,
        provider_id: impl Into<String>,
        id: impl Into<String>,
        title: impl Into<String>,
        subtitle: Option<String>,
        score: i32,
        on_open: impl Fn(&mut ui::widget::Edits<'_>, &ObjId, &Chrome) + 'static,
    ) -> Self {
        Self {
            kind,
            operation_kind: tilo_actions::OperationKind::View,
            provider_id: provider_id.into(),
            id: id.into(),
            title: title.into(),
            subtitle,
            facts: Vec::new(),
            badge: None,
            shortcut: None,
            score,
            params: Vec::new(),
            contract: None,
            on_open: Rc::new(move |doc, root, chrome, _args| {
                on_open(doc, root, chrome);
                Ok(())
            }),
            on_invoke: None,
            on_begin_invoke: None,
        }
    }

    /// [`SearchResult::nullary`] for a row that can refuse — a launch can
    /// still fail (missing package, declined sandbox, no display), and the
    /// return value is how a control-socket caller learns that.
    pub fn nullary_fallible(
        kind: ResourceKind,
        provider_id: impl Into<String>,
        id: impl Into<String>,
        title: impl Into<String>,
        subtitle: Option<String>,
        score: i32,
        on_open: impl Fn(&mut ui::widget::Edits<'_>, &ObjId, &Chrome) -> Result<(), ActionError>
        + 'static,
    ) -> Self {
        Self {
            kind,
            operation_kind: tilo_actions::OperationKind::Effect,
            provider_id: provider_id.into(),
            id: id.into(),
            title: title.into(),
            subtitle,
            facts: Vec::new(),
            badge: None,
            shortcut: None,
            score,
            params: Vec::new(),
            contract: None,
            on_open: Rc::new(move |doc, root, chrome, _args| on_open(doc, root, chrome)),
            on_invoke: None,
            on_begin_invoke: None,
        }
    }
}

impl SearchResult {
    /// A nullary row that shows something and answers with what it showed:
    /// the palette runs it like any other row, and an action caller receives
    /// the value as the receipt's typed output.
    pub fn view_with_output(
        kind: ResourceKind,
        provider_id: impl Into<String>,
        id: impl Into<String>,
        title: impl Into<String>,
        subtitle: Option<String>,
        score: i32,
        open: impl Fn(&Chrome) -> Result<serde_json::Value, ActionError> + 'static,
    ) -> Self {
        let open = Rc::new(open);
        let invoke = Rc::clone(&open);
        let mut row = Self::nullary_fallible(kind, provider_id, id, title, subtitle, score, {
            move |_doc, _root, chrome| open(chrome).map(drop)
        });
        row.operation_kind = tilo_actions::OperationKind::View;
        row.on_invoke = Some(Rc::new(move |_doc, _root, chrome, _args| {
            invoke(chrome).map(ActionReceipt::completed_with)
        }));
        row
    }
}

impl std::fmt::Debug for SearchResult {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SearchResult")
            .field("kind", &self.kind)
            .field("operation_kind", &self.operation_kind)
            .field("provider_id", &self.provider_id)
            .field("id", &self.id)
            .field("title", &self.title)
            .field("subtitle", &self.subtitle)
            .field("badge", &self.badge)
            .field("shortcut", &self.shortcut)
            .field("score", &self.score)
            .field("params", &self.params)
            .field("contract", &self.contract)
            .field("has_invoke_receipt", &self.on_invoke.is_some())
            .field("has_pending_invoke", &self.on_begin_invoke.is_some())
            .finish_non_exhaustive()
    }
}

/// Why a bounded catalogue traversal has not established exhaustion.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum QueryIncompleteReason {
    Pending,
    Refused,
    Unavailable,
    TimedOut,
    StaleCursor,
    BudgetExhausted,
    Continuation,
    InvalidInput,
}

/// Inert completion metadata; it carries no action or resource authority.
#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize)]
pub struct QueryCompletion {
    pub complete: bool,
    pub incomplete: Vec<ProviderIncomplete>,
}

/// One selected provider whose observation did not finish.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
pub struct ProviderIncomplete {
    pub provider: String,
    pub reason: QueryIncompleteReason,
}

impl QueryCompletion {
    fn invalid_input() -> Self {
        Self {
            complete: false,
            incomplete: vec![ProviderIncomplete {
                provider: "registry".into(),
                reason: QueryIncompleteReason::InvalidInput,
            }],
        }
    }
}

/// A provider that turns a query into results, owning its own
/// domain-specific matching logic — a heavy backend (network, disk index)
/// spawns its own worker(s) inside `query`.
pub trait SearchProvider {
    /// Stable id, used to route results back for de-dup and to build the
    /// per-scope filter (`ActiveApp` looks up providers by this id).
    fn id(&self) -> &str;
    /// Stable English name, independent of the desktop's language.
    fn name(&self) -> &str;
    /// The provider as people read it (e.g. in "Disable Notes search"), in
    /// the desktop's language.
    fn display_name(&self) -> String {
        self.name().to_string()
    }
    /// The set of kinds this provider CAN return; contributes chips to
    /// the palette's filter strip.
    fn resource_kinds(&self) -> &[ResourceKind];
    /// The launcher section this provider's answer to the empty query
    /// fills ([`launcher`]): what global search shows before anything is
    /// typed. `None` for a provider that has nothing to offer there.
    fn launcher_section(&self) -> Option<launcher::LauncherSection> {
        None
    }
    /// The rows this provider shows in its [`Self::launcher_section`] for
    /// `ctx` (its needle empty; its scope the current workspace or every
    /// workspace). By default every page of its answer to the empty query.
    fn launcher_rows(&self, ctx: SearchContext<'_>) -> Vec<SearchResult> {
        launcher::every_page(self, ctx)
    }
    /// Run the search. `wanted` is the set of kinds currently asked for
    /// (filter chips + scope); a remote-backed provider should dispatch
    /// ONE query and split the response into the wanted kinds.
    fn query(&self, ctx: SearchContext<'_>) -> Vec<SearchResult>;

    /// Resolve one provider-local id from current provider state. The default
    /// is suitable for small providers whose empty query is their complete
    /// action surface: it follows that query's pages to the end, so an id
    /// past the first palette page still resolves. Retained or remote
    /// providers override this so selecting an already-discovered address
    /// does not issue a different search query and discard the page that
    /// disclosed it.
    fn resolve(&self, id: &str, ctx: SearchContext<'_>) -> Option<SearchResult> {
        let mut cursor: Option<SearchCursor> = None;
        for _ in 0..HEADLESS_MAX_PAGES {
            let page = self.query_page(ctx, cursor.as_ref(), SearchBudget::PALETTE);
            if let Some(found) = page.results.into_iter().find(|result| result.id == id) {
                return Some(found);
            }
            if page.exhausted || page.stale_cursor {
                return None;
            }
            cursor = Some(page.next_cursor?);
        }
        None
    }

    /// The picture for one of this provider's rows when the row has one of
    /// its own (a settings page's, an action's); `None` keeps its kind's
    /// glyph. A picture tells rows apart faster than their text does.
    fn row_icon(&self, _result: &SearchResult) -> Option<ui::icons::Icon> {
        None
    }

    /// The artwork for one of this provider's rows that the palette cannot
    /// find by the row's kind and id: an app this device does not have yet
    /// wears its catalogue icon. `None` keeps the icon.
    fn row_image(&self, _result: &SearchResult) -> Option<ui::icons::AppImage> {
        None
    }

    /// The people a row of this provider is about, worn as faces at the
    /// row's end (who is in a workspace, in a call). Empty for most rows.
    fn row_faces(&self, _result: &SearchResult) -> Vec<ui::widget::command_palette::RowFace> {
        Vec::new()
    }

    /// What choosing a row of this provider does, shown at its end
    /// ("Join", "Answer"), for a row whose outcome its title does not say.
    fn row_outcome(&self, _result: &SearchResult) -> Option<String> {
        None
    }

    /// The other things to do with `result`, listed when it is opened with
    /// "More actions" (an app to install: show it in the App Store). Empty
    /// for a row with none.
    fn secondary_actions(&self, _result: &SearchResult) -> Vec<SearchResult> {
        Vec::new()
    }

    /// Resource hierarchy for one result already admitted to the caller's
    /// catalog. Providers derive identity from their authoritative source,
    /// never presentation labels. This does not resolve or invoke an action.
    fn action_hierarchy(&self, _id: &str) -> Option<action_api::ActionHierarchy> {
        None
    }

    /// Non-query maintenance hook for retained indexes. The compositor calls
    /// this from its frame/timer tick, before an input event is routed. A
    /// large provider may begin or poll a deduplicated background build here;
    /// `query_page` must only ever observe ready immutable snapshots.
    fn refresh_search_index(&self, _ctx: &SearchContext<'_>) {}

    /// Start one machine observation without retaining earlier reply failures.
    fn begin_catalogue_query(&self) {}

    /// Existing outstanding work or a failed reply, when the provider owns it.
    /// Page exhaustion remains required even when this returns `None`.
    fn query_incomplete_reason(&self, _ctx: &SearchContext<'_>) -> Option<QueryIncompleteReason> {
        None
    }

    /// Version of the retained index used by [`Self::query_page`].  A
    /// provider whose data is immutable may keep the default.  Mutable
    /// providers must change it whenever their ordering or disclosure can
    /// change; cursors are then rejected instead of silently skipping rows.
    fn search_revision(&self, _ctx: &SearchContext<'_>) -> String {
        "static".into()
    }

    /// Whether the retained index behind [`Self::query_page`] still has
    /// work outstanding for `ctx` — a build in flight, a source reply not yet
    /// in — so an answer given now may be missing rows that are on their
    /// way. A provider that reports settling must wake the loop that asked
    /// when that work lands (`invocation_wake`), since that is what a
    /// waiting caller sleeps on.
    fn index_settling(&self, _ctx: &SearchContext<'_>) -> bool {
        false
    }

    /// Bracket one exhaustive, machine-facing enumeration: while on, a
    /// provider that keeps resources behind a drill-down (an app's pages,
    /// their blocks) may also list what sits one level inside them, within
    /// its own bounds. The palette never turns it on, so typing stays
    /// cheap and a person drills down as before.
    fn set_enumerating(&self, _on: bool) {}

    /// Page a retained search index.  This is the canonical shell query
    /// surface.  `query` remains for small, direct callers and backwards
    /// compatible provider tests; registry/palette/agent discovery always
    /// call this method.
    ///
    /// The conservative default is intentionally only suitable for tiny,
    /// static providers. Keeping the fallback lets third-party providers
    /// upgrade without an ABI-breaking trait change; shell providers that
    /// can grow must override it with retained-index paging.
    fn query_page(
        &self,
        ctx: SearchContext<'_>,
        cursor: Option<&SearchCursor>,
        budget: SearchBudget,
    ) -> SearchPage {
        let revision = self.search_revision(&ctx);
        if cursor.is_some_and(|cursor| cursor.revision != revision || !cursor.token.is_empty()) {
            return SearchPage::stale(revision);
        }
        let mut results = self.query(ctx);
        let total = results.len();
        let emitted = total.min(budget.max_results);
        results.truncate(emitted);
        let exhausted = emitted == total;
        SearchPage::continuing(
            revision,
            results,
            (!exhausted).then(|| "legacy-truncated".into()),
        )
    }
    /// Top of this provider's own score scale. Scores are only meaningful
    /// within one provider — files award 120 for a leaf-name prefix while
    /// applications cap at 100, so raw numbers would rank `Calculator.h`
    /// above the Calculator app. [`SearchRegistry::query`] divides by this.
    fn max_score(&self) -> i32 {
        100
    }
    /// Does this provider own the tab at `tab_id`? Used by
    /// [`SearchScope::ActiveApp`] to narrow the fanout.
    fn owns_tab(&self, _tab_id: u64, _chrome: &ChromeState) -> bool {
        false
    }
    /// Whether this provider keeps to the focused app under
    /// [`SearchScope::ActiveApp`]: it reads [`SearchContext::effective_scope`]
    /// and answers only with the focused app's own rows. A search scoped to
    /// an app shows that app and nothing else; system actions, settings,
    /// people, windows and other apps come back only when the person clears
    /// the scope. So only a provider that narrows itself says yes; one that
    /// serves the whole desktop stays out of the scope entirely.
    fn narrows_to_active_app(&self) -> bool {
        false
    }

    /// Contextual location of one result. Providers know what their opaque
    /// ids mean; the registry does not. Workspace scope uses this only as a
    /// bounded ranking preference, never as a visibility filter.
    fn proximity(&self, _result: &SearchResult, _chrome: &ChromeState) -> SearchProximity {
        SearchProximity::Global
    }

    /// Whether `result` opens the app whose windows announce `app_id` (a
    /// package id, a Wayland app id). Search ranks an app's row beside the
    /// rows of its open windows; each stays its own row.
    fn opens_app(&self, _result: &SearchResult, _app_id: &str) -> bool {
        false
    }

    /// This provider's row that starts the app whose windows announce
    /// `app_id`, whatever was typed: what a new window of an open window's
    /// app runs. `None` when the app is not this provider's.
    fn app_row(&self, _app_id: &str) -> Option<SearchResult> {
        None
    }

    /// Whether a person typing `needle` should not be offered `result`: a
    /// row for agents and the control socket only (reading a value the row
    /// beside it already shows), or an action with nothing to act on right
    /// now (closing a sheet that is not open). It still resolves by its
    /// address and is listed for agents.
    fn hidden_from_people(
        &self,
        _result: &SearchResult,
        _needle: &str,
        _chrome: &ChromeState,
    ) -> bool {
        false
    }

    /// How far `result` is from the person: theirs, a Tilo app that opens
    /// at once, or an app they would install first. The merge ranks the
    /// last after everything they have, whatever its match
    /// ([`system::order_hits`]).
    fn availability(&self, _result: &SearchResult) -> Availability {
        Availability::OnDevice
    }

    /// Whether `result` is the row that answers any query, after every
    /// match (searching the web for it): a person typing sees it last, below
    /// whatever matched, and it never takes a row from a match.
    fn is_fallback(&self, _result: &SearchResult) -> bool {
        false
    }

    /// What `needle` says about `result` beyond the lexical fit of its title,
    /// added to its strength at the merge: a row the query names by another
    /// word ("youtube cats" for "Search `YouTube` for cats") ranks like an
    /// exact name, and a row that stands in for an app's own (a web service
    /// an app serves) gives way to it. Zero for most rows.
    fn intent_strength(&self, _result: &SearchResult, _needle: &str) -> f32 {
        0.0
    }

    /// The word `result` matched `needle` through when neither its title nor
    /// its subtitle shows it: a keyword, a name in another language. The
    /// palette says so ("Matches “darker”"), or the row looks unrelated.
    fn match_reason(&self, _result: &SearchResult, _needle: &str) -> Option<String> {
        None
    }

    /// Whether activating `result` flips a switch the person watches: the
    /// palette stays open and rebuilds its rows at once, so the row's state
    /// pill shows the value it was just switched to and the next activation
    /// switches it back. Every switch row says yes, whichever provider
    /// serves it.
    fn toggles_in_place(&self, _result: &SearchResult) -> bool {
        false
    }

    /// Whether ←/→ step `result`'s value in place ([`Self::adjust`]).
    fn adjustable(&self, _result: &SearchResult) -> bool {
        false
    }

    /// Step `result`'s value one notch up (`delta` 1) or down (-1), as its
    /// own action would set it.
    ///
    /// # Errors
    ///
    /// The row has no value to step, or its action refused the new one.
    fn adjust(
        &self,
        _result: &SearchResult,
        _delta: i8,
        _doc: &mut ui::widget::Edits<'_>,
        _root: &ObjId,
        _chrome: &Chrome,
    ) -> Result<(), ActionError> {
        Err(ActionError::Refused("this row has no value to step".into()))
    }
}

/// Longest hidden word a row is said to match through; a longer field is
/// prose, which says nothing as a reason.
const MATCH_REASON_MAX_SCALARS: usize = 40;

/// [`SearchProvider::match_reason`] over a row's searchable `fields`: the
/// first name or keyword holding a word of `needle`, when the row's title and
/// subtitle hold none. Identities (ids, paths) are never a reason.
pub(crate) fn match_reason_in(
    needle: &str,
    result: &SearchResult,
    fields: &[(String, ui::search::FieldWeight)],
) -> Option<String> {
    use ui::search::FieldWeight;
    let words: Vec<&str> = needle.split_whitespace().collect();
    let shows = |text: &str, weight: FieldWeight| {
        words
            .iter()
            .any(|word| tilo_search::locate(word, text, weight).is_some())
    };
    if words.is_empty()
        || shows(&result.title, FieldWeight::TITLE)
        || result
            .subtitle
            .as_deref()
            .is_some_and(|subtitle| shows(subtitle, FieldWeight::SUBTITLE))
    {
        return None;
    }
    fields
        .iter()
        .filter(|(_, weight)| {
            [
                FieldWeight::TITLE,
                FieldWeight::KEYWORD,
                FieldWeight::SUBTITLE,
                FieldWeight::METADATA,
            ]
            .contains(weight)
        })
        .map(|(text, weight)| (text.trim(), *weight))
        // An address ("quick-settings.volume.set") is no reason a person reads.
        .filter(|(text, _)| !text.contains(['.', ':', '/']))
        .find(|(text, weight)| {
            text.chars().count() <= MATCH_REASON_MAX_SCALARS && shows(text, *weight)
        })
        .map(|(text, _)| text.to_owned())
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SearchProximity {
    CurrentWorkspace,
    #[default]
    Global,
}

/// Bundle of everything a provider might need to answer a query. Passed
/// by value so providers don't have to store the borrows.
#[derive(Clone, Copy)]
pub struct SearchContext<'a> {
    /// Effective registry scope, independent of the palette's current filter.
    pub query_scope: Option<SearchScope>,
    /// Explicit machine discovery; ordinary palette queries keep this false.
    pub include_programmatic: bool,
    pub needle: &'a str,
    pub wanted: &'a [ResourceKind],
    pub chrome: &'a ChromeState,
    pub doc: &'a AutoCommit,
    pub doc_root: &'a ObjId,
}

impl SearchContext<'_> {
    pub fn effective_scope(&self) -> SearchScope {
        self.query_scope.unwrap_or(self.chrome.search_scope)
    }
}

/// Arguments for [`SearchRegistry::query`]/[`SearchRegistry::query_grouped`]
/// — distinct from [`SearchContext`], which is the narrower, per-provider
/// view built fresh for each provider inside the fan-out loop.
pub struct QueryArgs<'a> {
    pub needle: &'a str,
    pub scope: SearchScope,
    pub active_provider_id: Option<&'a str>,
    pub wanted_kinds: &'a [ResourceKind],
    pub chrome: &'a ChromeState,
    pub doc: &'a AutoCommit,
    pub doc_root: &'a ObjId,
}

fn estimated_result_bytes(result: &SearchResult) -> usize {
    result
        .provider_id
        .len()
        .saturating_add(result.id.len())
        .saturating_add(result.title.len())
        .saturating_add(result.secondary_len())
        .saturating_add(result.params.len().saturating_mul(64))
}

/// Durable, user-owned search-source choices. Provider ids are opaque stable
/// strings, so preferences also survive a provider being temporarily absent.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SearchPreferences {
    disabled_providers: HashSet<String>,
}

pub type SearchPreferencesHandle = Rc<RefCell<SearchPreferences>>;
pub type SearchProviderCatalogHandle = Rc<RefCell<Vec<(String, String)>>>;

impl SearchPreferences {
    const SECTION: &'static str = "search";
    const DISABLED_KEY: &'static str = "disabled_providers";

    pub fn from_values(values: &serde_json::Map<String, serde_json::Value>) -> Self {
        let disabled_providers = values
            .get(Self::DISABLED_KEY)
            .and_then(serde_json::Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(serde_json::Value::as_str)
            .filter(|id| *id != "actions" && !id.is_empty() && id.chars().count() <= 256)
            .map(str::to_string)
            .collect();
        Self { disabled_providers }
    }

    pub fn load() -> Self {
        Self::from_values(&settings_store::load_section(Self::SECTION))
    }

    pub fn provider_enabled(&self, provider_id: &str) -> bool {
        !self.disabled_providers.contains(provider_id)
    }

    pub fn set_provider_enabled(&mut self, provider_id: &str, enabled: bool) {
        if enabled {
            self.disabled_providers.remove(provider_id);
        } else if provider_id != "actions"
            && !provider_id.is_empty()
            && provider_id.chars().count() <= 256
        {
            self.disabled_providers.insert(provider_id.to_string());
        }
    }

    /// Persist atomically through the shared settings service while preserving
    /// unrelated keys in both the search section and the rest of the document.
    ///
    /// # Errors
    ///
    /// Returns any error from the settings service reading or writing the
    /// settings document.
    pub fn save(&self) -> std::io::Result<()> {
        let mut ids: Vec<_> = self.disabled_providers.iter().cloned().collect();
        ids.sort();
        let mut values = serde_json::Map::new();
        values.insert(
            Self::DISABLED_KEY.into(),
            serde_json::Value::Array(ids.into_iter().map(serde_json::Value::String).collect()),
        );
        settings_store::update_section(Self::SECTION, values)
    }
}

/// Registry that fans a query out to every provider under the current
/// scope, then merges + sorts.
pub struct SearchRegistry {
    providers: Vec<Rc<dyn SearchProvider>>,
    preferences: SearchPreferencesHandle,
    provider_catalog: SearchProviderCatalogHandle,
}

// Keep each source's order while visiting continuations fairly across sources.
struct ProviderQuery<'a> {
    provider: &'a dyn SearchProvider,
    wanted: Vec<ResourceKind>,
    cursor: Option<SearchCursor>,
    seen: HashSet<SearchCursor>,
    results: Vec<SearchResult>,
    done: bool,
    incomplete: Option<QueryIncompleteReason>,
}

impl ProviderQuery<'_> {
    fn note_page(&mut self, page: &SearchPage) {
        self.incomplete = if page.stale_cursor {
            Some(QueryIncompleteReason::StaleCursor)
        } else if page.exhausted {
            None
        } else if page.budget_exhausted {
            Some(QueryIncompleteReason::BudgetExhausted)
        } else {
            Some(QueryIncompleteReason::Continuation)
        };
    }

    fn report_incomplete(
        &self,
        args: &QueryArgs<'_>,
        include_programmatic: bool,
        target: &mut Vec<ProviderIncomplete>,
    ) {
        let ctx = SearchContext {
            query_scope: Some(args.scope),
            include_programmatic,
            needle: args.needle,
            wanted: &self.wanted,
            chrome: args.chrome,
            doc: args.doc,
            doc_root: args.doc_root,
        };
        if let Some(reason) = self
            .provider
            .query_incomplete_reason(&ctx)
            .or(self.incomplete)
        {
            target.push(ProviderIncomplete {
                provider: self.provider.id().into(),
                reason,
            });
        }
    }
}

impl Default for SearchRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl SearchRegistry {
    pub fn new() -> Self {
        Self {
            providers: Vec::new(),
            preferences: Rc::new(RefCell::new(SearchPreferences::default())),
            provider_catalog: Rc::new(RefCell::new(Vec::new())),
        }
    }

    pub fn with_preferences(preferences: &SearchPreferences) -> Self {
        Self::with_preferences_handle(Rc::new(RefCell::new(preferences.clone())))
    }

    pub fn with_preferences_handle(preferences: SearchPreferencesHandle) -> Self {
        Self {
            providers: Vec::new(),
            preferences,
            provider_catalog: Rc::new(RefCell::new(Vec::new())),
        }
    }

    pub fn preferences_handle(&self) -> SearchPreferencesHandle {
        Rc::clone(&self.preferences)
    }

    pub fn provider_catalog_handle(&self) -> SearchProviderCatalogHandle {
        Rc::clone(&self.provider_catalog)
    }

    /// Apply the user's provider preferences. Disabled sources are removed at
    /// the fan-out boundary, so indexing, kind discovery, interactive search,
    /// and agent paging all observe the same choice.
    pub fn set_provider_enabled(&self, provider_id: &str, enabled: bool) {
        self.preferences
            .borrow_mut()
            .set_provider_enabled(provider_id, enabled);
    }

    pub fn provider_enabled(&self, provider_id: &str) -> bool {
        self.preferences.borrow().provider_enabled(provider_id)
    }

    fn enabled_providers(&self) -> impl Iterator<Item = &Rc<dyn SearchProvider>> {
        self.providers
            .iter()
            .filter(|provider| self.provider_enabled(provider.id()))
    }

    /// Results are re-sorted by score before display, so registration
    /// order only affects which provider's ID wins the de-dup tiebreak.
    pub fn register(&mut self, provider: Rc<dyn SearchProvider>) {
        let provider_id = provider.id().to_string();
        let descriptor = (provider_id.clone(), provider.display_name());
        let mut catalog = self.provider_catalog.borrow_mut();
        if let Some(existing) = catalog.iter_mut().find(|(id, _)| id == &descriptor.0) {
            *existing = descriptor;
        } else {
            catalog.push(descriptor);
        }
        drop(catalog);
        if let Some(existing) = self
            .providers
            .iter_mut()
            .find(|existing| existing.id() == provider_id)
        {
            *existing = provider;
        } else {
            self.providers.push(provider);
        }
    }

    /// Install the current owner for a stable provider id.
    ///
    /// Session bridges can be rebound after login or in a headless harness.
    /// Keeping an older provider beside its replacement would expose duplicate
    /// rows whose callbacks still target the retired bridge.
    pub fn register_or_replace(&mut self, provider: Rc<dyn SearchProvider>) {
        let id = provider.id();
        self.providers.retain(|existing| existing.id() != id);
        self.providers.push(provider);
    }

    /// Used by the shell's scope-narrow helper to look up which provider owns a tab.
    #[expect(
        clippy::redundant_closure_for_method_calls,
        reason = "the closure shortens the provider trait object's lifetime, which the method path cannot"
    )]
    pub fn providers_iter(&self) -> impl Iterator<Item = &(dyn SearchProvider + '_)> + '_ {
        self.enabled_providers().map(|provider| provider.as_ref())
    }

    /// Resolve one exact provider-local id under the same scope and kind
    /// routing used by search. This keeps invocation attached to the source
    /// that disclosed the id while allowing that provider to revalidate its
    /// current retained state: its index is refreshed first, because an
    /// invocation need not follow any search or open palette that would
    /// otherwise take in a finished build.
    pub fn resolve(
        &self,
        provider_id: &str,
        id: &str,
        args: &QueryArgs<'_>,
    ) -> Option<SearchResult> {
        let provider = self
            .providers
            .iter()
            .find(|provider| provider.id() == provider_id)?;
        if !scope_allows(provider.as_ref(), args.scope, args.active_provider_id) {
            return None;
        }
        let wanted = if args.wanted_kinds.is_empty() {
            provider.resource_kinds().to_vec()
        } else {
            provider
                .resource_kinds()
                .iter()
                .filter(|kind| args.wanted_kinds.contains(kind))
                .cloned()
                .collect()
        };
        if wanted.is_empty() {
            return None;
        }
        let ctx = SearchContext {
            query_scope: Some(args.scope),
            include_programmatic: false,
            needle: "",
            wanted: &wanted,
            chrome: args.chrome,
            doc: args.doc,
            doc_root: args.doc_root,
        };
        provider.refresh_search_index(&ctx);
        provider.resolve(id, ctx).filter(|result| {
            result.provider_id == provider_id && result.id == id && wanted.contains(&result.kind)
        })
    }

    /// Poll/start retained index maintenance without routing a query. This is
    /// intentionally separate from `query`, `query_revision`, and
    /// `query_provider_page`: a cold 100k source must never be flattened on
    /// the keystroke path.
    pub fn refresh_indexes(&self, args: &QueryArgs<'_>) -> bool {
        if !query_input_is_bounded(args.needle) {
            return false;
        }
        let mut changed = false;
        for provider in self.enabled_providers() {
            if !scope_allows(provider.as_ref(), args.scope, args.active_provider_id) {
                continue;
            }
            let wanted = if args.wanted_kinds.is_empty() {
                provider.resource_kinds().to_vec()
            } else {
                provider
                    .resource_kinds()
                    .iter()
                    .filter(|kind| args.wanted_kinds.contains(kind))
                    .cloned()
                    .collect()
            };
            if wanted.is_empty() {
                continue;
            }
            let ctx = SearchContext {
                query_scope: Some(args.scope),
                include_programmatic: false,
                needle: args.needle,
                wanted: &wanted,
                chrome: args.chrome,
                doc: args.doc,
                doc_root: args.doc_root,
            };
            let before = provider.search_revision(&ctx);
            provider.refresh_search_index(&ctx);
            changed |= before != provider.search_revision(&ctx);
        }
        changed
    }

    /// Whether any provider `args` reaches is still settling its retained
    /// index ([`SearchProvider::index_settling`]): an answer read now may be
    /// incomplete. Routed exactly like [`Self::refresh_indexes`].
    pub fn indexes_settling(&self, args: &QueryArgs<'_>) -> bool {
        if !query_input_is_bounded(args.needle) {
            return false;
        }
        self.enabled_providers().any(|provider| {
            if !scope_allows(provider.as_ref(), args.scope, args.active_provider_id) {
                return false;
            }
            let wanted = if args.wanted_kinds.is_empty() {
                provider.resource_kinds().to_vec()
            } else {
                provider
                    .resource_kinds()
                    .iter()
                    .filter(|kind| args.wanted_kinds.contains(kind))
                    .cloned()
                    .collect()
            };
            !wanted.is_empty()
                && provider.index_settling(&SearchContext {
                    query_scope: Some(args.scope),
                    include_programmatic: false,
                    needle: args.needle,
                    wanted: &wanted,
                    chrome: args.chrome,
                    doc: args.doc,
                    doc_root: args.doc_root,
                })
        })
    }

    /// Page one named source without flattening every provider into a single
    /// transient vector.  This is the agent-facing primitive behind
    /// hierarchy/navigation UIs: callers retain the opaque cursor and repeat
    /// only the provider they are currently exploring.
    pub fn query_provider_page(
        &self,
        provider_id: &str,
        args: &QueryArgs<'_>,
        cursor: Option<&SearchCursor>,
        budget: SearchBudget,
    ) -> Option<SearchPage> {
        if !query_input_is_bounded(args.needle) {
            return Some(SearchPage::exhausted("oversized-query", Vec::new()));
        }
        let provider = self
            .enabled_providers()
            .find(|provider| provider.id() == provider_id)?;
        if !scope_allows(provider.as_ref(), args.scope, args.active_provider_id) {
            return None;
        }
        let wanted = if args.wanted_kinds.is_empty() {
            provider.resource_kinds().to_vec()
        } else {
            provider
                .resource_kinds()
                .iter()
                .filter(|kind| args.wanted_kinds.contains(kind))
                .cloned()
                .collect()
        };
        if wanted.is_empty() {
            return Some(SearchPage::exhausted("not-wanted", Vec::new()));
        }
        Some(provider.query_page(
            SearchContext {
                query_scope: Some(args.scope),
                include_programmatic: false,
                needle: args.needle,
                wanted: &wanted,
                chrome: args.chrome,
                doc: args.doc,
                doc_root: args.doc_root,
            },
            cursor,
            budget,
        ))
    }

    /// Resolve one current row without surveying unrelated providers.
    pub fn query_provider_exact(
        &self,
        provider_id: &str,
        id: &str,
        args: &QueryArgs<'_>,
    ) -> Option<SearchResult> {
        let mut cursor = None;
        let mut seen = std::collections::HashSet::new();
        for _ in 0..HEADLESS_MAX_PAGES {
            let page = self.query_provider_page(
                provider_id,
                args,
                cursor.as_ref(),
                SearchBudget::PALETTE,
            )?;
            if page.stale_cursor {
                return None;
            }
            if let Some(found) = page
                .results
                .into_iter()
                .find(|result| result.provider_id == provider_id && result.id == id)
            {
                return Some(found);
            }
            if page.exhausted {
                return None;
            }
            let next = page.next_cursor?;
            if !seen.insert(next.clone()) {
                return None;
            }
            cursor = Some(next);
        }
        None
    }

    /// Union of every registered provider's [`ResourceKind`]s, ordered by
    /// `ResourceKind::sort_key` — one chip per kind in the palette.
    /// The picture the provider that returned `result` gives it, if any
    /// ([`SearchProvider::row_icon`]).
    pub fn row_icon(&self, result: &SearchResult) -> Option<ui::icons::Icon> {
        self.providers
            .iter()
            .find(|provider| provider.id() == result.provider_id)?
            .row_icon(result)
    }

    /// The artwork the provider that returned `result` gives it, if any
    /// ([`SearchProvider::row_image`]).
    pub fn row_image(&self, result: &SearchResult) -> Option<ui::icons::AppImage> {
        self.providers
            .iter()
            .find(|provider| provider.id() == result.provider_id)?
            .row_image(result)
    }

    /// The faces the provider that returned `result` gives it
    /// ([`SearchProvider::row_faces`]).
    pub fn row_faces(&self, result: &SearchResult) -> Vec<ui::widget::command_palette::RowFace> {
        self.providers
            .iter()
            .find(|provider| provider.id() == result.provider_id)
            .map(|provider| provider.row_faces(result))
            .unwrap_or_default()
    }

    /// What choosing `result` does, as its provider says
    /// ([`SearchProvider::row_outcome`]).
    pub fn row_outcome(&self, result: &SearchResult) -> Option<String> {
        self.providers
            .iter()
            .find(|provider| provider.id() == result.provider_id)
            .and_then(|provider| provider.row_outcome(result))
    }

    /// The other actions the provider that returned `result` offers for it
    /// ([`SearchProvider::secondary_actions`]).
    pub fn secondary_actions(&self, result: &SearchResult) -> Vec<SearchResult> {
        self.providers
            .iter()
            .find(|provider| provider.id() == result.provider_id)
            .map(|provider| provider.secondary_actions(result))
            .unwrap_or_default()
    }

    pub fn registered_kinds(&self) -> Vec<ResourceKind> {
        let mut out: Vec<ResourceKind> = Vec::new();
        for provider in self.enabled_providers() {
            for kind in provider.resource_kinds() {
                if !out.contains(kind) {
                    out.push(kind.clone());
                }
            }
        }
        out.sort_by_key(ResourceKind::sort_key);
        out
    }

    /// Snapshot the revisions of exactly the providers a query can reach.
    /// Overlay refresh uses this together with its text field, so live app
    /// resources appearing/disappearing update an open palette without
    /// making the user type a throwaway character.
    pub fn query_revision(&self, args: &QueryArgs<'_>) -> String {
        if !query_input_is_bounded(args.needle) {
            return "oversized-query".into();
        }
        // Scope, focused provider and kind filters affect routing or ranking
        // even when no provider index changed. Include them in the refresh
        // identity so switching a pill can never reuse stale palette rows.
        let mut out = format!(
            "scope:{:?}\0active:{}\0wanted:{:?}\0",
            args.scope,
            args.active_provider_id.unwrap_or(""),
            args.wanted_kinds
        );
        for provider in self.enabled_providers() {
            if !scope_allows(provider.as_ref(), args.scope, args.active_provider_id) {
                continue;
            }
            out.push_str(provider.id());
            out.push('\0');
            out.push_str(&provider.search_revision(&SearchContext {
                query_scope: Some(args.scope),
                include_programmatic: false,
                needle: args.needle,
                wanted: args.wanted_kinds,
                chrome: args.chrome,
                doc: args.doc,
                doc_root: args.doc_root,
            }));
            out.push('\0');
        }
        out
    }

    /// Fan out `needle` to every provider whose id passes the scope
    /// filter. `wanted_kinds` filters results by kind; empty means "any
    /// kind" so a naive caller can just pass `&[]`.
    pub fn query(&self, args: &QueryArgs<'_>) -> Vec<SearchResult> {
        self.query_inner(args, false, false).0
    }

    /// Exhaust every provider continuation for a render-free action listing.
    /// Interactive palette queries deliberately remain one bounded page.
    pub fn query_exhaustive(&self, args: &QueryArgs<'_>) -> Vec<SearchResult> {
        self.query_inner(args, true, false).0
    }

    pub fn begin_catalogue_query(&self) {
        for provider in self.enabled_providers() {
            provider.begin_catalogue_query();
        }
    }

    /// Exhaust the existing machine traversal while retaining its completion.
    pub fn query_catalogue(
        &self,
        args: &QueryArgs<'_>,
        include_programmatic: bool,
    ) -> (Vec<SearchResult>, QueryCompletion) {
        self.query_inner(args, true, include_programmatic)
    }

    #[allow(
        clippy::too_many_lines,
        reason = "one planning pass over every provider round"
    )]
    fn query_rounds(
        &self,
        args: &QueryArgs<'_>,
        exhaustive: bool,
        include_programmatic: bool,
    ) -> Vec<ProviderQuery<'_>> {
        let mut sources: Vec<_> = self
            .enabled_providers()
            .filter(|provider| scope_allows(provider.as_ref(), args.scope, args.active_provider_id))
            .filter_map(|provider| {
                let wanted: Vec<_> = provider
                    .resource_kinds()
                    .iter()
                    .filter(|kind| args.wanted_kinds.is_empty() || args.wanted_kinds.contains(kind))
                    .cloned()
                    .collect();
                (!wanted.is_empty()).then(|| ProviderQuery {
                    provider: provider.as_ref(),
                    wanted,
                    cursor: None,
                    seen: HashSet::new(),
                    results: Vec::new(),
                    done: false,
                    incomplete: Some(QueryIncompleteReason::Continuation),
                })
            })
            .collect();
        let mut pages = 0usize;
        let mut results = 0usize;
        let mut bytes = 0usize;
        if exhaustive {
            for source in &sources {
                source.provider.set_enumerating(true);
            }
        }
        // A file index may need hundreds of sparse pages. Survey every source
        // before following continuations so it cannot hide an app or action.
        'rounds: loop {
            let mut advanced = false;
            for source in &mut sources {
                if source.done {
                    continue;
                }
                if exhaustive
                    && (pages >= HEADLESS_MAX_PAGES
                        || results >= HEADLESS_MAX_RESULTS
                        || bytes >= HEADLESS_MAX_BYTES)
                {
                    break 'rounds;
                }
                let mut budget = SearchBudget::PALETTE;
                if exhaustive {
                    budget.max_results = budget
                        .max_results
                        .min(HEADLESS_MAX_RESULTS.saturating_sub(results));
                    budget.max_bytes = budget
                        .max_bytes
                        .min(HEADLESS_MAX_BYTES.saturating_sub(bytes));
                }
                let page = source.provider.query_page(
                    SearchContext {
                        query_scope: Some(args.scope),
                        include_programmatic,
                        needle: args.needle,
                        wanted: &source.wanted,
                        chrome: args.chrome,
                        doc: args.doc,
                        doc_root: args.doc_root,
                    },
                    source.cursor.as_ref(),
                    budget,
                );
                pages = pages.saturating_add(1);
                advanced = true;
                source.note_page(&page);
                if page.stale_cursor {
                    // Keep any earlier valid page, including legacy providers'
                    // first page when their synthetic continuation is stale.
                    source.done = true;
                    continue;
                }
                for result in page.results {
                    let size = estimated_result_bytes(&result);
                    if exhaustive
                        && (results >= HEADLESS_MAX_RESULTS
                            || size > HEADLESS_MAX_BYTES.saturating_sub(bytes))
                    {
                        source.done = true;
                        source.incomplete = Some(QueryIncompleteReason::BudgetExhausted);
                        break;
                    }
                    results = results.saturating_add(1);
                    bytes = bytes.saturating_add(size);
                    source.results.push(result);
                }
                if source.done || !exhaustive || page.exhausted {
                    source.done = true;
                } else if let Some(cursor) = page.next_cursor
                    && source.seen.insert(cursor.clone())
                {
                    source.cursor = Some(cursor);
                } else {
                    source.done = true;
                }
            }
            if !exhaustive || !advanced {
                break;
            }
        }
        for source in &mut sources {
            if !source.done {
                source.incomplete = Some(QueryIncompleteReason::BudgetExhausted);
            }
        }
        if exhaustive {
            for source in &sources {
                source.provider.set_enumerating(false);
            }
        }
        sources
    }

    fn query_inner(
        &self,
        args: &QueryArgs<'_>,
        exhaustive: bool,
        include_programmatic: bool,
    ) -> (Vec<SearchResult>, QueryCompletion) {
        if !query_input_is_bounded(args.needle) {
            return (Vec::new(), QueryCompletion::invalid_input());
        }
        let QueryArgs { needle, chrome, .. } = *args;
        // A person typing, as opposed to an agent listing the catalogue:
        // what they chose before and which windows are open shape the order.
        let interactive = !exhaustive && !include_programmatic;
        let mut merged: Vec<(SearchResult, f32)> = Vec::new();
        let mut fallbacks: Vec<(SearchResult, f32)> = Vec::new();
        let mut loose: HashSet<(String, String)> = HashSet::new();
        let mut canonical_rows: HashMap<(String, String), usize> = HashMap::new();
        let mut incomplete = Vec::new();
        for source in self.query_rounds(args, exhaustive, include_programmatic) {
            let provider = source.provider;
            source.report_incomplete(args, include_programmatic, &mut incomplete);
            let top = provider.max_score().max(1);
            for r in source.results {
                if interactive && provider.hidden_from_people(&r, needle, chrome) {
                    continue;
                }
                if interactive && provider.is_fallback(&r) {
                    fallbacks.push((r, 0.0));
                    continue;
                }
                // A score above `top` exceeds strength 1.0 and silently
                // outranks every other provider's perfect match.
                debug_assert!(
                    r.score <= top,
                    "provider {:?} returned {} but declares max_score() = {top}; \
                     raise max_score() or bound the score",
                    provider.id(),
                    r.score,
                );
                let proximity = proximity_boost(provider, &r, args);
                let strength = system::merged_strength(
                    needle,
                    &r.kind,
                    &r.title,
                    r.subtitle.as_deref(),
                    r.score,
                    top,
                    proximity,
                ) + provider.intent_strength(&r, needle)
                    + if interactive {
                        window_intent(needle, &r, top) + usage_strength(needle, &r)
                    } else {
                        0.0
                    };
                let key = (r.provider_id.clone(), r.id.clone());
                if interactive && only_loosely_matched(needle, &r, top) {
                    loose.insert(key.clone());
                }
                #[expect(
                    clippy::indexing_slicing,
                    reason = "canonical_rows only stores merged.len() right before the matching push, so every stored index is in bounds"
                )]
                if let Some(&index) = canonical_rows.get(&key) {
                    if strength > merged[index].1 {
                        merged[index] = (r, strength);
                    }
                } else {
                    canonical_rows.insert(key, merged.len());
                    merged.push((r, strength));
                }
            }
        }
        if interactive && !needle.trim().is_empty() {
            loose_matches_give_way(&mut merged, &loose);
            self.apps_beside_their_windows(&mut merged, chrome);
            files_follow_what_was_named(&mut merged, needle);
        }
        let providers: HashMap<&str, &dyn SearchProvider> = self
            .enabled_providers()
            .map(|provider| (provider.id(), provider.as_ref()))
            .collect();
        system::order_hits(&mut merged, |row| {
            let availability = providers
                .get(row.provider_id.as_str())
                .map_or(Availability::OnDevice, |provider| {
                    provider.availability(row)
                });
            (&row.kind, availability)
        });
        // The rows that answer any query (searching the web for it) come
        // last, after every match, in their own group.
        if interactive && !needle.trim().is_empty() {
            merged.append(&mut fallbacks);
        }
        let completion = QueryCompletion {
            complete: incomplete.is_empty(),
            incomplete,
        };
        (merged.into_iter().map(|(r, _)| r).collect(), completion)
    }

    /// An open app is two kinds of row: the app, which starts it (for most
    /// apps a new window), and each of its windows, which switches to that
    /// window. They rank together: an app and the windows it opened take
    /// the strongest of their strengths, so the app sits right beside its
    /// windows instead of somewhere below them.
    fn apps_beside_their_windows(&self, merged: &mut [(SearchResult, f32)], chrome: &ChromeState) {
        let windows: Vec<(usize, String)> = {
            let tree = chrome.tree();
            merged
                .iter()
                .enumerate()
                .filter_map(|(at, (row, _))| {
                    if row.provider_id != tilo_desktop::search::WINDOWS_PROVIDER_ID
                        || row.kind != ResourceKind::Tab
                        || row.operation_kind != tilo_actions::OperationKind::View
                    {
                        return None;
                    }
                    let app = tilo_desktop::search::window_row_app_id(&tree.desktop, &row.id)?;
                    Some((at, app))
                })
                .collect()
        };
        if windows.is_empty() {
            return;
        }
        let providers: HashMap<&str, &dyn SearchProvider> = self
            .enabled_providers()
            .map(|provider| (provider.id(), provider.as_ref()))
            .collect();
        for app_at in 0..merged.len() {
            let Some((app_row, app_strength)) = merged.get(app_at) else {
                continue;
            };
            if !matches!(app_row.kind, ResourceKind::Application) {
                continue;
            }
            let Some(provider) = providers.get(app_row.provider_id.as_str()) else {
                continue;
            };
            let together: Vec<usize> = windows
                .iter()
                .filter(|(_, app)| provider.opens_app(app_row, app))
                .map(|(at, _)| *at)
                .chain(std::iter::once(app_at))
                .collect();
            if together.len() < 2 {
                continue;
            }
            let strongest = together
                .iter()
                .filter_map(|at| merged.get(*at).map(|(_, strength)| *strength))
                .fold(*app_strength, f32::max);
            for at in together {
                if let Some((_, strength)) = merged.get_mut(at) {
                    *strength = strongest;
                }
            }
        }
    }

    /// The row that starts the app whose windows announce `app_id`, from
    /// the first provider that has it ([`SearchProvider::app_row`]).
    pub fn app_row(&self, app_id: &str) -> Option<SearchResult> {
        self.enabled_providers()
            .find_map(|provider| provider.app_row(app_id))
    }

    /// Whether activating `result` flips a switch in place; see
    /// [`SearchProvider::toggles_in_place`].
    pub fn toggles_in_place(&self, result: &SearchResult) -> bool {
        self.enabled_providers()
            .find(|provider| provider.id() == result.provider_id)
            .is_some_and(|provider| provider.toggles_in_place(result))
    }

    /// How far `result` is from the person; see
    /// [`SearchProvider::availability`].
    pub fn availability(&self, result: &SearchResult) -> Availability {
        self.enabled_providers()
            .find(|provider| provider.id() == result.provider_id)
            .map_or(Availability::OnDevice, |provider| {
                provider.availability(result)
            })
    }

    /// Whether ←/→ step `result`'s value; see [`SearchProvider::adjustable`].
    pub fn adjustable(&self, result: &SearchResult) -> bool {
        self.enabled_providers()
            .find(|provider| provider.id() == result.provider_id)
            .is_some_and(|provider| provider.adjustable(result))
    }

    /// Step `result`'s value; see [`SearchProvider::adjust`].
    ///
    /// # Errors
    ///
    /// The provider's refusal, or that no enabled provider owns the row.
    pub fn adjust(
        &self,
        result: &SearchResult,
        delta: i8,
        doc: &mut ui::widget::Edits<'_>,
        root: &ObjId,
        chrome: &Chrome,
    ) -> Result<(), ActionError> {
        self.enabled_providers()
            .find(|provider| provider.id() == result.provider_id)
            .ok_or_else(|| ActionError::Refused("no provider owns this row".into()))?
            .adjust(result, delta, doc, root, chrome)
    }

    /// Why `result` matched `needle`, when its title and subtitle don't show
    /// it; see [`SearchProvider::match_reason`].
    pub fn match_reason(&self, result: &SearchResult, needle: &str) -> Option<String> {
        self.enabled_providers()
            .find(|provider| provider.id() == result.provider_id)?
            .match_reason(result, needle)
    }

    /// [`Self::query`], arranged into contiguous per-kind groups and
    /// trimmed to a `row_budget` shared round-robin: the palette paints
    /// ~10 rows, and an outscoring kind (forty file hits) can otherwise
    /// push a correctly-ranked app off the bottom. A lone group is exempt
    /// — narrowing to one kind means "show me all of these".
    ///
    /// With nothing typed and no kind chosen, the desktop-wide palette is
    /// the [`launcher`]; scoped to one app it still lists that app's
    /// commands.
    pub fn query_grouped(&self, args: &QueryArgs<'_>, row_budget: usize) -> Vec<KindGroup> {
        if launcher::wanted(args) {
            return launcher::kind_groups(self, args);
        }
        let store = in_the_store();
        let ranked: Vec<(ResourceKind, SearchResult)> = self
            .query(args)
            .into_iter()
            .map(|row| {
                // A third-party app's Store page is not something the person
                // has: its own heading, after their content (the merge puts
                // every one of them last), never a second "Applications".
                let heading = if self.availability(&row) == Availability::ThirdPartyCatalogue {
                    store.clone()
                } else {
                    row.kind.clone()
                };
                (heading, row)
            })
            .collect();
        system::group_by_kind(ranked, |(heading, _)| heading, row_budget)
            .into_iter()
            .map(|group| KindGroup {
                kind: group.kind,
                total: group.total,
                results: group.results.into_iter().map(|(_, row)| row).collect(),
            })
            .collect()
    }
}

/// The heading third-party apps' Store pages are listed under, apart from
/// the apps the person has and the Tilo apps that open at once.
#[must_use]
pub fn in_the_store() -> ResourceKind {
    ResourceKind::Custom(t("search-group-in-the-store"))
}

/// How much nearer the person a row is in Workspace scope: from the focused
/// app's provider, and in the workspace being looked at.
fn proximity_boost(provider: &dyn SearchProvider, row: &SearchResult, args: &QueryArgs<'_>) -> f32 {
    if args.scope != SearchScope::Workspace {
        return 0.0;
    }
    let active_app = if args.active_provider_id == Some(provider.id()) {
        system::ACTIVE_APP_PROXIMITY_BOOST
    } else {
        0.0
    };
    let workspace = if provider.proximity(row, args.chrome) == SearchProximity::CurrentWorkspace {
        system::ACTIVE_APP_PROXIMITY_BOOST
    } else {
        0.0
    };
    active_app + workspace
}

/// Queries this short are what launchers live on: the thing chosen most
/// leads every other prefix match.
const SHORT_QUERY_SCALARS: usize = 3;
/// How much a row's whole frecency adds at the merge, for a short query and
/// for a longer one. For a short query it outweighs the gap between an exact
/// name and a prefix (1.5) and an app's launch intent; for a longer one it
/// only settles near ties.
const SHORT_QUERY_USAGE: f32 = ui::tokens::search::SHORT_QUERY_USAGE;
const LONG_QUERY_USAGE: f32 = 0.4;
/// What a row chosen before for the same typed text adds: it leads.
const SAME_TEXT_CHOICE: f32 = ui::tokens::search::SAME_TEXT_CHOICE;
/// A title matched from a word start: the hits frecency may reorder.
const WORD_START_LEXICAL: f32 = 1.5;

/// What the person chose before adds to a row at the merge: how often and
/// how recently they chose it, which leads a short query's prefix matches
/// ("t" is the Terminal they open daily, not whichever action sorts first),
/// and whether they chose it before for this very text.
fn usage_strength(needle: &str, row: &SearchResult) -> f32 {
    let typed = needle.trim();
    if typed.is_empty() {
        return 0.0;
    }
    let choice = usage::choice_share(typed, &row.provider_id, &row.id) * SAME_TEXT_CHOICE;
    let lexical = system::lexical_strength(typed, &row.kind, &row.title, None);
    if lexical < WORD_START_LEXICAL {
        return choice;
    }
    let weight = if typed.chars().count() <= SHORT_QUERY_SCALARS {
        SHORT_QUERY_USAGE
    } else {
        LONG_QUERY_USAGE
    };
    choice + usage::share(&row.provider_id, &row.id) * weight
}

/// A window named by a query is as much what the person reaches for as the
/// app it belongs to: it gets the launch intent an app row gets.
fn window_intent(needle: &str, row: &SearchResult, top: i32) -> f32 {
    if needle.trim().is_empty()
        || row.kind != ResourceKind::Tab
        || row.operation_kind != tilo_actions::OperationKind::View
    {
        return 0.0;
    }
    system::application_intent_boost(row.score, top)
}

/// What a row that matched every word of the query, spread over its fields
/// (the least a real match ranks), scores on its provider's scale, as a
/// share of the top. A row below it matched only by a typo.
#[allow(
    clippy::cast_precision_loss,
    reason = "provider scores are small integers"
)]
fn loose_share() -> f32 {
    provider_score(tilo_search::SPANNING_RANK) as f32 / system::MAX_PROVIDER_SCORE as f32
}

/// Whether `row` matched `needle` only loosely: by a typo, with nothing of
/// the query in its title. A row that holds every word the person typed,
/// spread over its fields ("gnome calculator": GNOME's Calculator, whose
/// title is "Calculator"), is a real match, not a loose one.
#[allow(
    clippy::cast_precision_loss,
    reason = "provider scores are small integers"
)]
fn only_loosely_matched(needle: &str, row: &SearchResult, top: i32) -> bool {
    (row.score as f32 / top.max(1) as f32) < loose_share()
        && system::lexical_strength(needle, &row.kind, &row.title, None) < 1.0
}

/// A typo is how a misspelt name is still found, not a reason to list
/// everything one letter away from a word that matched: once anything
/// matches the query as typed, the rows that only match it loosely go
/// ("move" lists the move actions, not "Mobile broadband").
fn loose_matches_give_way(
    merged: &mut Vec<(SearchResult, f32)>,
    loose: &HashSet<(String, String)>,
) {
    let key = |row: &SearchResult| (row.provider_id.clone(), row.id.clone());
    if merged.iter().any(|(row, _)| !loose.contains(&key(row))) {
        merged.retain(|(row, _)| !loose.contains(&key(row)));
    }
}

/// Whether typed text asks for a file by its shape: a path, a home-relative
/// path, or a name with an extension ("report.pdf").
fn looks_like_a_path(needle: &str) -> bool {
    let typed = needle.trim();
    if typed.contains('/') || typed.starts_with('~') {
        return true;
    }
    typed.rsplit_once('.').is_some_and(|(stem, extension)| {
        !stem.is_empty()
            && (1..=5).contains(&extension.len())
            && extension.chars().all(|c| c.is_ascii_alphanumeric())
            && extension.chars().any(|c| c.is_ascii_alphabetic())
    })
}

/// Files and folders follow every app, window, action and setting the query
/// names by a word start of its title, unless the query is shaped like a
/// file ([`looks_like_a_path`]). A launcher that leads with
/// `Settings-backup.txt` for "settings" is a filename grep; the files are
/// still there, below what the person most likely meant.
fn files_follow_what_was_named(merged: &mut [(SearchResult, f32)], needle: &str) {
    if looks_like_a_path(needle) {
        return;
    }
    let is_file = |kind: &ResourceKind| matches!(kind, ResourceKind::File | ResourceKind::Folder);
    let is_named = |kind: &ResourceKind| {
        matches!(
            kind,
            ResourceKind::Action
                | ResourceKind::Application
                | ResourceKind::Tab
                | ResourceKind::WindowGroup
                | ResourceKind::Custom(_)
        )
    };
    // The weakest best of the kinds whose title the query named: files sit
    // just below it, so no named group is ordered after them.
    let mut best: HashMap<(u8, String), f32> = HashMap::new();
    for (row, strength) in merged.iter() {
        if is_named(&row.kind)
            && system::lexical_strength(needle, &row.kind, &row.title, None) >= WORD_START_LEXICAL
        {
            let slot = best.entry(row.kind.sort_key()).or_insert(f32::MIN);
            *slot = slot.max(*strength);
        }
    }
    let Some(floor) = best.values().copied().reduce(f32::min) else {
        return;
    };
    let ceiling = floor - 0.01;
    for (row, strength) in merged.iter_mut() {
        if is_file(&row.kind) && *strength >= floor {
            *strength = ceiling;
        }
    }
}

fn scope_allows(
    provider: &dyn SearchProvider,
    scope: SearchScope,
    active_provider_id: Option<&str>,
) -> bool {
    match scope {
        SearchScope::All | SearchScope::Workspace => true,
        SearchScope::ActiveApp => match active_provider_id {
            // Only the app's own sources: the provider that owns its tab and
            // those that narrow themselves to it. The system's actions and
            // settings are not the app's; they return with a wider scope.
            Some(id) => provider.id() == id || provider.narrows_to_active_app(),
            // The overlay explicitly degrades to `All` when there is no
            // focused tab. Reaching this branch means a focused app exists
            // but its primary provider is not ready; fail narrow rather than
            // leaking unrelated workspace results into "This app".
            None => provider.narrows_to_active_app(),
        },
    }
}

impl system::SearchRow for SearchResult {
    fn kind(&self) -> &ResourceKind {
        &self.kind
    }
    fn score(&self) -> i32 {
        self.score
    }
    fn title(&self) -> &str {
        &self.title
    }
    fn subtitle(&self) -> Option<&str> {
        self.subtitle.as_deref()
    }
    fn shortcut(&self) -> Option<&str> {
        self.shortcut.as_deref()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shell::state::{WorkspaceTree, WorkspaceTreeHandle};
    use std::fmt::Write as _;
    use std::rc::Rc;

    struct FakeProvider {
        id: String,
        kinds: Vec<ResourceKind>,
        results: Vec<SearchResult>,
        /// Top of this fake's own score scale. `None` takes the trait default.
        max_score: Option<i32>,
    }

    impl SearchProvider for FakeProvider {
        fn id(&self) -> &str {
            &self.id
        }
        fn name(&self) -> &str {
            &self.id
        }
        fn resource_kinds(&self) -> &[ResourceKind] {
            &self.kinds
        }
        fn query(&self, ctx: SearchContext<'_>) -> Vec<SearchResult> {
            self.results
                .iter()
                .filter(|r| ctx.wanted.contains(&r.kind))
                .cloned()
                .collect()
        }
        fn max_score(&self) -> i32 {
            self.max_score.unwrap_or(100)
        }
    }

    fn mk_result(
        kind: ResourceKind,
        provider: &str,
        id: &str,
        title: &str,
        score: i32,
    ) -> SearchResult {
        SearchResult::nullary(kind, provider, id, title, None, score, |_, _, _| {})
    }

    fn mk_chrome() -> crate::shell::chrome_state::Chrome {
        let handle: WorkspaceTreeHandle =
            crate::shell::state::WorkspaceTreeHandle::new(WorkspaceTree::default());
        ChromeState::new_with_handle(handle)
    }

    fn mk_doc() -> (AutoCommit, ObjId) {
        let doc = AutoCommit::new();
        (doc, automerge::ROOT)
    }

    /// The interactive host refreshes retained providers before it routes a
    /// key event.  Keep public-registry tests on that same contract instead
    /// of teaching a cold `query()` to synchronously index a corpus.
    fn refresh_until_ready(registry: &SearchRegistry, args: &QueryArgs<'_>) {
        tilo_test_env::wait_until("the retained search index to become ready", || {
            registry.refresh_indexes(args);
            registry.query_revision(args).contains("ready:")
        });
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn oversized_and_exhaustive_queries_stay_bounded_for_every_provider() {
        // Phase `oversized_query_never_routes_to_a_shell_provider`.
        {
            use std::cell::Cell;

            struct CountingProvider(Rc<Cell<usize>>);
            impl SearchProvider for CountingProvider {
                fn id(&self) -> &'static str {
                    "counting"
                }
                fn name(&self) -> &'static str {
                    "Counting"
                }
                fn resource_kinds(&self) -> &[ResourceKind] {
                    &[ResourceKind::Action]
                }
                fn query(&self, _ctx: SearchContext<'_>) -> Vec<SearchResult> {
                    self.0.set(self.0.get().saturating_add(1));
                    Vec::new()
                }
            }

            let calls = Rc::new(Cell::new(0));
            let mut registry = SearchRegistry::new();
            registry.register(Rc::new(CountingProvider(Rc::clone(&calls))));
            let chrome = mk_chrome();
            let (doc, root) = mk_doc();
            let query = "夏".repeat(ui::search::MAX_SEARCH_QUERY_SCALARS + 1);
            let args = QueryArgs {
                needle: &query,
                scope: SearchScope::All,
                active_provider_id: None,
                wanted_kinds: &[],
                chrome: &chrome.borrow(),
                doc: &doc,
                doc_root: &root,
            };

            assert!(registry.query(&args).is_empty());
            assert_eq!(calls.get(), 0, "providers must not receive abusive input");
            let page = registry
                .query_provider_page("counting", &args, None, SearchBudget::PALETTE)
                .expect("registered provider");
            assert!(page.exhausted && page.results.is_empty());
            assert_eq!(calls.get(), 0, "direct paging has the same input gate");
        }

        // Phase `exhaustive_query_is_bounded_without_starving_later_providers`.
        {
            use std::cell::Cell;

            let calls = Rc::new(Cell::new(0));
            let mut registry = SearchRegistry::new();
            registry.register(Rc::new(PagedProvider {
                calls: Rc::clone(&calls),
                id: "endless",
                pages: None,
            }));
            let chrome = mk_chrome();
            let (doc, root) = mk_doc();
            let args = QueryArgs {
                needle: "",
                scope: SearchScope::All,
                active_provider_id: None,
                wanted_kinds: &[],
                chrome: &chrome.borrow(),
                doc: &doc,
                doc_root: &root,
            };

            let rows = registry.query_exhaustive(&args);
            assert_eq!(calls.get(), HEADLESS_MAX_PAGES);
            assert!(rows.len() <= HEADLESS_MAX_RESULTS);

            // A large earlier source can keep returning continuations even when
            // an agent is looking for one exact application from a later source.
            registry.register(Rc::new(FakeProvider {
                id: "applications".into(),
                kinds: vec![ResourceKind::Application],
                results: vec![mk_result(
                    ResourceKind::Application,
                    "applications",
                    "dev.tilo.calculator",
                    "Calculator",
                    100,
                )],
                max_score: Some(100),
            }));
            calls.set(0);
            let rows = registry.query_exhaustive(&args);
            assert!(
                rows.iter().any(|row| row.id == "dev.tilo.calculator"),
                "an earlier paged source must not hide a later application's search result"
            );
            assert!(calls.get() < HEADLESS_MAX_PAGES);
            assert!(rows.len() <= HEADLESS_MAX_RESULTS);

            let later_calls = Rc::new(Cell::new(0));
            registry.register(Rc::new(PagedProvider {
                calls: Rc::clone(&later_calls),
                id: "later",
                pages: Some(2),
            }));
            calls.set(0);
            let rows = registry.query_exhaustive(&args);
            assert_eq!(
                later_calls.get(),
                2,
                "later sources must also follow continuations"
            );
            assert_eq!(
                calls
                    .get()
                    .saturating_add(later_calls.get())
                    .saturating_add(1),
                HEADLESS_MAX_PAGES
            );
            let first_second = rows.iter().position(|row| row.id == "endless-1").unwrap();
            let later_first = rows.iter().position(|row| row.id == "later-0").unwrap();
            assert!(
                first_second < later_first,
                "equal-score ties retain provider order, not page visitation order"
            );
            assert!(rows.iter().any(|row| row.id == "later-1"));

            calls.set(0);
            later_calls.set(0);
            let _ = registry.query(&args);
            assert_eq!(
                calls.get(),
                1,
                "interactive search remains one page per source"
            );
            assert_eq!(later_calls.get(), 1);
        }

        // Phase `exhaustive_query_limits_legacy_provider_defaults`.
        {
            let chrome = mk_chrome();
            let (doc, root) = mk_doc();
            let args = QueryArgs {
                needle: "",
                scope: SearchScope::All,
                active_provider_id: None,
                wanted_kinds: &[],
                chrome: &chrome.borrow(),
                doc: &doc,
                doc_root: &root,
            };
            let mut legacy = SearchRegistry::new();
            legacy.register(Rc::new(FakeProvider {
                id: "legacy".into(),
                kinds: vec![ResourceKind::Action],
                results: (0..60)
                    .map(|index| {
                        mk_result(
                            ResourceKind::Action,
                            "legacy",
                            &format!("legacy-{index}"),
                            "legacy",
                            1,
                        )
                    })
                    .collect(),
                max_score: None,
            }));
            let rows = legacy.query_exhaustive(&args);
            assert!(rows.iter().any(|row| row.id == "legacy-0"));
            assert_eq!(
                rows.iter()
                    .filter(|row| row.provider_id == "legacy")
                    .count(),
                48
            );
        }
    }

    #[test]
    fn catalogue_completion_retains_bounds_and_exhaustion() {
        let chrome = mk_chrome();
        let (doc, root) = mk_doc();
        let args = QueryArgs {
            needle: "",
            scope: SearchScope::All,
            active_provider_id: None,
            wanted_kinds: &[],
            chrome: &chrome.borrow(),
            doc: &doc,
            doc_root: &root,
        };
        let mut registry = SearchRegistry::new();
        registry.register(Rc::new(PagedProvider {
            calls: Rc::new(std::cell::Cell::new(0)),
            id: "finite",
            pages: Some(2),
        }));
        let (rows, completion) = registry.query_catalogue(&args, true);
        assert_eq!(rows.len(), 2);
        assert!(
            completion.complete,
            "an exhausted last page completes despite its page budget flag"
        );
        registry.register(Rc::new(PagedProvider {
            calls: Rc::new(std::cell::Cell::new(0)),
            id: "endless",
            pages: None,
        }));
        let (_, completion) = registry.query_catalogue(&args, true);
        assert!(!completion.complete);
        assert!(
            completion
                .incomplete
                .iter()
                .any(|source| source.provider == "endless"
                    && source.reason == super::QueryIncompleteReason::BudgetExhausted)
        );
    }

    struct PagedProvider {
        calls: Rc<std::cell::Cell<usize>>,
        id: &'static str,
        pages: Option<usize>,
    }
    impl SearchProvider for PagedProvider {
        fn id(&self) -> &'static str {
            self.id
        }
        fn name(&self) -> &'static str {
            self.id
        }
        fn resource_kinds(&self) -> &[ResourceKind] {
            &[ResourceKind::Action]
        }
        fn query(&self, _ctx: SearchContext<'_>) -> Vec<SearchResult> {
            Vec::new()
        }
        fn query_page(
            &self,
            _ctx: SearchContext<'_>,
            cursor: Option<&SearchCursor>,
            _budget: SearchBudget,
        ) -> SearchPage {
            let page = cursor
                .and_then(|cursor| cursor.token.parse::<usize>().ok())
                .unwrap_or_default();
            self.calls.set(self.calls.get().saturating_add(1));
            SearchPage {
                revision: "endless-v1".into(),
                results: vec![mk_result(
                    ResourceKind::Action,
                    self.id,
                    &format!("{}-{page}", self.id),
                    "row",
                    1,
                )],
                next_cursor: Some(SearchCursor {
                    revision: "endless-v1".into(),
                    token: page.saturating_add(1).to_string(),
                }),
                exhausted: self
                    .pages
                    .is_some_and(|total| page.saturating_add(1) >= total),
                stale_cursor: false,
                budget_exhausted: true,
            }
        }
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn registry_merges_dedupes_and_ranks_rows_within_declared_ceilings() {
        // Phase `registry_merges_and_sorts_by_kind_then_score`.
        {
            let mut reg = SearchRegistry::new();
            reg.register(Rc::new(FakeProvider {
                id: "notes".into(),
                kinds: vec![ResourceKind::Note],
                results: vec![
                    // Above the loose-match share: these are real matches
                    // whose titles merely don't spell the query.
                    mk_result(ResourceKind::Note, "notes", "n1", "note b", 20),
                    mk_result(ResourceKind::Note, "notes", "n2", "note a", 50),
                ],
                max_score: None,
            }));
            reg.register(Rc::new(FakeProvider {
                id: "actions".into(),
                kinds: vec![ResourceKind::Action],
                results: vec![mk_result(ResourceKind::Action, "actions", "a1", "act", 30)],
                max_score: None,
            }));
            let chrome = mk_chrome();
            let (doc, root) = mk_doc();
            let out = reg.query(&QueryArgs {
                needle: "x",
                scope: SearchScope::All,
                active_provider_id: None,
                wanted_kinds: &[],
                chrome: &chrome.borrow(),
                doc: &doc,
                doc_root: &root,
            });
            assert_eq!(out.len(), 3);
            // Notes' strongest hit (50) beats the action's (30).
            assert_eq!(out[0].kind, ResourceKind::Note);
            assert_eq!(out[0].id, "n2");
            assert_eq!(out[1].id, "n1");
            assert_eq!(out[2].kind, ResourceKind::Action);
        }

        // Phase `exact_title_beats_a_high_scoring_metadata_match_across_providers`.
        {
            let mut reg = SearchRegistry::new();
            let mut settings = mk_result(
                ResourceKind::Action,
                "actions",
                "settings",
                "Open workspace settings",
                100,
            );
            settings.subtitle = Some("Manage notes integrations".into());
            reg.register(Rc::new(FakeProvider {
                id: "actions".into(),
                kinds: vec![ResourceKind::Action],
                results: vec![settings],
                max_score: None,
            }));
            reg.register(Rc::new(FakeProvider {
                id: "notes".into(),
                kinds: vec![ResourceKind::Note],
                results: vec![mk_result(ResourceKind::Note, "notes", "n1", "Notes", 0)],
                max_score: None,
            }));
            let chrome = mk_chrome();
            let (doc, root) = mk_doc();
            let out = reg.query(&QueryArgs {
                needle: "notes",
                scope: SearchScope::All,
                active_provider_id: None,
                wanted_kinds: &[],
                chrome: &chrome.borrow(),
                doc: &doc,
                doc_root: &root,
            });

            assert_eq!(out[0].title, "Notes");
        }

        // Phase `registry_collapses_duplicate_stable_rows_from_one_provider`.
        {
            let mut reg = SearchRegistry::new();
            reg.register(Rc::new(FakeProvider {
                id: "notes".into(),
                kinds: vec![ResourceKind::Note],
                results: vec![
                    mk_result(ResourceKind::Note, "notes", "same-note", "Old title", 20),
                    mk_result(
                        ResourceKind::Note,
                        "notes",
                        "same-note",
                        "Current title",
                        80,
                    ),
                ],
                max_score: None,
            }));
            let chrome = mk_chrome();
            let (doc, root) = mk_doc();
            let out = reg.query(&QueryArgs {
                needle: "title",
                scope: SearchScope::All,
                active_provider_id: None,
                wanted_kinds: &[],
                chrome: &chrome.borrow(),
                doc: &doc,
                doc_root: &root,
            });

            assert_eq!(out.len(), 1);
            assert_eq!(out[0].title, "Current title");
        }

        // Phase `a_provider_never_scores_above_its_declared_ceiling`.
        // Every provider's declared ceiling must actually bound it — the
        // invariant the whole cross-provider comparison rests on.
        {
            use crate::shell::search::providers::{
                actions::ActionsProvider, applications::ApplicationsProvider, files::FilesProvider,
            };
            let providers: Vec<Rc<dyn SearchProvider>> = vec![
                Rc::new(ActionsProvider::new()),
                Rc::new(ApplicationsProvider::new()),
                Rc::new(FilesProvider::new()),
            ];
            for p in providers {
                assert!(
                    p.max_score() > 0,
                    "{:?} declares a non-positive ceiling",
                    p.id()
                );
            }
        }

        // Phase `equally_strong_results_keep_the_providers_order`.
        // Equally-strong results keep their provider's ordering, not re-sorted by title.
        {
            let mut reg = SearchRegistry::new();
            reg.register(Rc::new(FakeProvider {
                id: "files".into(),
                kinds: vec![ResourceKind::File],
                // Emitted worst-alphabetical-first: a title sort would reverse this.
                results: vec![
                    mk_result(ResourceKind::File, "files", "f1", "zebra.rs", 80),
                    mk_result(ResourceKind::File, "files", "f2", "middle.rs", 80),
                    mk_result(ResourceKind::File, "files", "f3", "alpha.rs", 80),
                ],
                max_score: Some(120),
            }));
            let chrome = mk_chrome();
            let (doc, root) = mk_doc();
            let out = reg.query(&QueryArgs {
                needle: "rs",
                scope: SearchScope::Workspace,
                active_provider_id: None,
                wanted_kinds: &[],
                chrome: &chrome.borrow(),
                doc: &doc,
                doc_root: &root,
            });
            assert_eq!(
                out.iter().map(|r| r.id.as_str()).collect::<Vec<_>>(),
                vec!["f1", "f2", "f3"],
                "the provider ranked these; the registry must not re-alphabetise them"
            );
        }
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn search_preferences_and_provider_lifecycle_reach_the_live_registry() {
        // Phase `disabling_a_provider_removes_it_from_every_search_surface`.
        {
            let mut reg = SearchRegistry::new();
            reg.register(Rc::new(FakeProvider {
                id: "notes".into(),
                kinds: vec![ResourceKind::Note],
                results: vec![mk_result(ResourceKind::Note, "notes", "n1", "Note", 80)],
                max_score: None,
            }));
            reg.set_provider_enabled("notes", false);

            let chrome = mk_chrome();
            let (doc, root) = mk_doc();
            let args = QueryArgs {
                needle: "note",
                scope: SearchScope::All,
                active_provider_id: None,
                wanted_kinds: &[],
                chrome: &chrome.borrow(),
                doc: &doc,
                doc_root: &root,
            };

            assert!(!reg.provider_enabled("notes"));
            assert_eq!(reg.providers_iter().count(), 0);
            assert!(reg.registered_kinds().is_empty());
            assert!(!reg.query_revision(&args).contains("notes"));
            assert!(reg.query(&args).is_empty());
            assert!(
                reg.query_provider_page("notes", &args, None, SearchBudget::PALETTE)
                    .is_none(),
                "agent paging must honor the same preference as the palette"
            );

            reg.set_provider_enabled("notes", true);
            assert_eq!(reg.query(&args).len(), 1);
        }

        // Phase `search_preferences_parse_deduplicate_and_ignore_malformed_ids`.
        {
            let mut values = serde_json::Map::new();
            values.insert(
                SearchPreferences::DISABLED_KEY.into(),
                serde_json::json!(["files", "files", "actions", "", 7, "x".repeat(257)]),
            );

            let preferences = SearchPreferences::from_values(&values);
            assert!(!preferences.provider_enabled("files"));
            assert!(preferences.provider_enabled("notes"));
            assert_eq!(preferences.disabled_providers.len(), 1);

            let registry = SearchRegistry::with_preferences(&preferences);
            assert!(!registry.provider_enabled("files"));
            assert!(registry.provider_enabled("notes"));
        }

        // Phase `shared_preferences_change_a_live_registry_without_restarting`.
        {
            let preferences = Rc::new(RefCell::new(SearchPreferences::default()));
            let mut registry = SearchRegistry::with_preferences_handle(Rc::clone(&preferences));
            registry.register(Rc::new(FakeProvider {
                id: "files".into(),
                kinds: vec![ResourceKind::File],
                results: Vec::new(),
                max_score: None,
            }));

            assert_eq!(registry.providers_iter().count(), 1);
            preferences
                .borrow_mut()
                .set_provider_enabled("files", false);
            assert_eq!(registry.providers_iter().count(), 0);
        }

        // Phase `providers_attached_after_startup_join_the_customization_catalog`.
        {
            let mut registry = SearchRegistry::new();
            let catalog = registry.provider_catalog_handle();
            assert!(catalog.borrow().is_empty());

            registry.register(Rc::new(FakeProvider {
                id: "live-app-actions".into(),
                kinds: vec![ResourceKind::Action],
                results: Vec::new(),
                max_score: None,
            }));
            assert_eq!(
                catalog.borrow().as_slice(),
                &[("live-app-actions".into(), "live-app-actions".into())]
            );

            registry.set_provider_enabled("live-app-actions", false);
            assert_eq!(
                catalog.borrow().len(),
                1,
                "a disabled dynamic source must remain discoverable for re-enabling"
            );
            assert!(registry.providers_iter().next().is_none());
        }

        // Phase `reconnecting_provider_replaces_stale_resources_for_palette_and_agent_paging`.
        {
            let mut registry = SearchRegistry::new();
            registry.register(Rc::new(FakeProvider {
                id: "mail".into(),
                kinds: vec![ResourceKind::Record],
                results: vec![mk_result(
                    ResourceKind::Record,
                    "mail",
                    "old",
                    "Disconnected mailbox",
                    10,
                )],
                max_score: None,
            }));
            registry.register(Rc::new(FakeProvider {
                id: "mail".into(),
                kinds: vec![ResourceKind::Record],
                results: vec![mk_result(
                    ResourceKind::Record,
                    "mail",
                    "fresh",
                    "Reconnected mailbox",
                    20,
                )],
                max_score: None,
            }));
            let chrome = mk_chrome();
            let (doc, root) = mk_doc();
            let args = QueryArgs {
                needle: "mailbox",
                scope: SearchScope::All,
                active_provider_id: None,
                wanted_kinds: &[],
                chrome: &chrome.borrow(),
                doc: &doc,
                doc_root: &root,
            };

            assert_eq!(registry.providers_iter().count(), 1);
            let rows = registry.query(&args);
            assert_eq!(rows.len(), 1);
            assert_eq!(rows[0].id, "fresh");
            let page = registry
                .query_provider_page("mail", &args, None, SearchBudget::PALETTE)
                .expect("replacement provider page");
            assert_eq!(page.results.len(), 1);
            assert_eq!(page.results[0].id, "fresh");
        }

        // Phase `registered_kinds_is_deduped_and_ordered`.
        {
            let mut reg = SearchRegistry::new();
            reg.register(Rc::new(FakeProvider {
                id: "a".into(),
                kinds: vec![ResourceKind::File, ResourceKind::Note],
                results: vec![],
                max_score: None,
            }));
            reg.register(Rc::new(FakeProvider {
                id: "b".into(),
                kinds: vec![ResourceKind::Note, ResourceKind::Action],
                results: vec![],
                max_score: None,
            }));
            let kinds = reg.registered_kinds();
            assert_eq!(
                kinds,
                vec![ResourceKind::Action, ResourceKind::Note, ResourceKind::File,]
            );
        }
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn search_scope_and_kind_filters_narrow_without_widening() {
        // Phase `workspace_scope_prefers_the_focused_app_but_everywhere_is_neutral`.
        {
            let mut reg = SearchRegistry::new();
            reg.register(Rc::new(FakeProvider {
                id: "notes".into(),
                kinds: vec![ResourceKind::Note],
                results: vec![mk_result(ResourceKind::Note, "notes", "near", "Draft", 60)],
                max_score: None,
            }));
            reg.register(Rc::new(FakeProvider {
                id: "files".into(),
                kinds: vec![ResourceKind::Note],
                results: vec![mk_result(
                    ResourceKind::Note,
                    "files",
                    "global",
                    "Draft",
                    70,
                )],
                max_score: None,
            }));
            let chrome = mk_chrome();
            let (doc, root) = mk_doc();
            let query = |scope| {
                reg.query(&QueryArgs {
                    needle: "draft",
                    scope,
                    active_provider_id: Some("notes"),
                    wanted_kinds: &[],
                    chrome: &chrome.borrow(),
                    doc: &doc,
                    doc_root: &root,
                })
            };

            assert_eq!(query(SearchScope::Workspace)[0].id, "near");
            assert_eq!(query(SearchScope::All)[0].id, "global");
            assert_eq!(query(SearchScope::All).len(), 2);
        }

        // Phase `workspace_scope_uses_provider_owned_resource_location`.
        {
            struct LocatedProvider {
                id: &'static str,
                result: SearchResult,
                proximity: SearchProximity,
            }
            impl SearchProvider for LocatedProvider {
                fn id(&self) -> &str {
                    self.id
                }
                fn name(&self) -> &str {
                    self.id
                }
                fn resource_kinds(&self) -> &[ResourceKind] {
                    &[ResourceKind::Note]
                }
                fn query(&self, _ctx: SearchContext<'_>) -> Vec<SearchResult> {
                    vec![self.result.clone()]
                }
                fn proximity(
                    &self,
                    _result: &SearchResult,
                    _chrome: &ChromeState,
                ) -> SearchProximity {
                    self.proximity
                }
            }

            let mut reg = SearchRegistry::new();
            // Register global first so Everywhere's stable tie order proves the
            // workspace flip comes from proximity, not provider order.
            reg.register(Rc::new(LocatedProvider {
                id: "global",
                result: mk_result(ResourceKind::Note, "global", "far", "Draft", 60),
                proximity: SearchProximity::Global,
            }));
            reg.register(Rc::new(LocatedProvider {
                id: "local",
                result: mk_result(ResourceKind::Note, "local", "near", "Draft", 60),
                proximity: SearchProximity::CurrentWorkspace,
            }));
            let chrome = mk_chrome();
            let (doc, root) = mk_doc();
            let query = |scope| {
                reg.query(&QueryArgs {
                    needle: "draft",
                    scope,
                    active_provider_id: None,
                    wanted_kinds: &[],
                    chrome: &chrome.borrow(),
                    doc: &doc,
                    doc_root: &root,
                })
            };

            assert_eq!(query(SearchScope::All)[0].id, "far");
            assert_eq!(query(SearchScope::Workspace)[0].id, "near");
        }

        // Phase `query_revision_changes_with_scope_focus_and_kind_filter`.
        {
            let mut reg = SearchRegistry::new();
            reg.register(Rc::new(FakeProvider {
                id: "notes".into(),
                kinds: vec![ResourceKind::Note],
                results: Vec::new(),
                max_score: None,
            }));
            let chrome = mk_chrome();
            let (doc, root) = mk_doc();
            let revision = |scope, active_provider_id, wanted_kinds: &[ResourceKind]| {
                reg.query_revision(&QueryArgs {
                    needle: "draft",
                    scope,
                    active_provider_id,
                    wanted_kinds,
                    chrome: &chrome.borrow(),
                    doc: &doc,
                    doc_root: &root,
                })
            };

            let workspace = revision(SearchScope::Workspace, Some("notes"), &[]);
            assert_ne!(workspace, revision(SearchScope::All, Some("notes"), &[]));
            assert_ne!(workspace, revision(SearchScope::Workspace, None, &[]));
            assert_ne!(
                workspace,
                revision(SearchScope::Workspace, Some("notes"), &[ResourceKind::Note])
            );
        }

        // Phase `active_app_scope_narrows_to_matching_provider`.
        {
            let mut reg = SearchRegistry::new();
            reg.register(Rc::new(FakeProvider {
                id: "notes".into(),
                kinds: vec![ResourceKind::Note],
                results: vec![mk_result(ResourceKind::Note, "notes", "n1", "n", 10)],
                max_score: None,
            }));
            reg.register(Rc::new(FakeProvider {
                id: "files".into(),
                kinds: vec![ResourceKind::File],
                results: vec![mk_result(ResourceKind::File, "files", "f1", "f", 10)],
                max_score: None,
            }));
            let chrome = mk_chrome();
            let (doc, root) = mk_doc();
            let out = reg.query(&QueryArgs {
                needle: "x",
                scope: SearchScope::ActiveApp,
                active_provider_id: Some("notes"),
                wanted_kinds: &[],
                chrome: &chrome.borrow(),
                doc: &doc,
                doc_root: &root,
            });
            assert_eq!(out.len(), 1);
            assert_eq!(out[0].provider_id, "notes");
        }

        // Phase `active_app_scope_leaves_out_the_systems_actions`.
        // A search scoped to an app shows only the app's own sources; the
        // pinned journey is `a_search_scoped_to_an_app_shows_only_that_app_
        // until_the_scope_is_cleared` in the overlay host's tests.
        {
            use crate::shell::search::providers::actions::ActionsProvider;

            for active in [Some("notes"), None] {
                assert!(
                    !scope_allows(&ActionsProvider::new(), SearchScope::ActiveApp, active),
                    "the system's actions are not the app's own ({active:?})"
                );
            }
            assert!(scope_allows(
                &ActionsProvider::new(),
                SearchScope::Workspace,
                Some("notes")
            ));
        }

        // Phase `unresolved_active_app_scope_never_widens_to_unrelated_providers`.
        {
            let unrelated = FakeProvider {
                id: "files".into(),
                kinds: vec![ResourceKind::File],
                results: Vec::new(),
                max_score: None,
            };
            assert!(!scope_allows(&unrelated, SearchScope::ActiveApp, None));
        }

        // Phase `wanted_kinds_filter_hides_unrequested`.
        {
            let mut reg = SearchRegistry::new();
            reg.register(Rc::new(FakeProvider {
                id: "mixed".into(),
                kinds: vec![ResourceKind::Note, ResourceKind::File],
                results: vec![
                    mk_result(ResourceKind::Note, "mixed", "n", "a", 10),
                    mk_result(ResourceKind::File, "mixed", "f", "b", 10),
                ],
                max_score: None,
            }));
            let chrome = mk_chrome();
            let (doc, root) = mk_doc();
            let out = reg.query(&QueryArgs {
                needle: "x",
                scope: SearchScope::All,
                active_provider_id: None,
                wanted_kinds: &[ResourceKind::Note],
                chrome: &chrome.borrow(),
                doc: &doc,
                doc_root: &root,
            });
            assert_eq!(out.len(), 1);
            assert_eq!(out[0].kind, ResourceKind::Note);
        }
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn the_strongest_match_leads_whether_app_action_or_file() {
        // Phase `the_same_providers_flip_when_the_action_scores_highest`.
        // Same two providers, but now the action is the strongest hit and must lead.
        {
            let mut reg = SearchRegistry::new();
            reg.register(Rc::new(FakeProvider {
                id: "notes".into(),
                kinds: vec![ResourceKind::Note],
                results: vec![mk_result(ResourceKind::Note, "notes", "n1", "note b", 10)],
                max_score: None,
            }));
            reg.register(Rc::new(FakeProvider {
                id: "actions".into(),
                kinds: vec![ResourceKind::Action],
                results: vec![mk_result(ResourceKind::Action, "actions", "a1", "act", 90)],
                max_score: None,
            }));
            let chrome = mk_chrome();
            let (doc, root) = mk_doc();
            let out = reg.query(&QueryArgs {
                needle: "x",
                scope: SearchScope::All,
                active_provider_id: None,
                wanted_kinds: &[],
                chrome: &chrome.borrow(),
                doc: &doc,
                doc_root: &root,
            });
            assert_eq!(out[0].kind, ResourceKind::Action);
        }

        // Phase `a_matching_application_outranks_file_hits_for_the_same_word`.
        // A typed app name must put the app in the row Enter activates, not a
        // source file whose path happens to contain the word.
        {
            let mut reg = SearchRegistry::new();
            reg.register(Rc::new(FakeProvider {
                id: "files".into(),
                kinds: vec![ResourceKind::File],
                results: vec![
                    mk_result(ResourceKind::File, "files", "f1", "Calculator.h", 120),
                    mk_result(ResourceKind::File, "files", "f2", "calculator.rs", 120),
                ],
                max_score: Some(120),
            }));
            reg.register(Rc::new(FakeProvider {
                id: "applications".into(),
                kinds: vec![ResourceKind::Application],
                results: vec![mk_result(
                    ResourceKind::Application,
                    "applications",
                    "so.tilo.calculator",
                    "Open Calculator",
                    100,
                )],
                max_score: None,
            }));
            let chrome = mk_chrome();
            let (doc, root) = mk_doc();
            let out = reg.query(&QueryArgs {
                needle: "calculator",
                scope: SearchScope::Workspace,
                active_provider_id: None,
                wanted_kinds: &[],
                chrome: &chrome.borrow(),
                doc: &doc,
                doc_root: &root,
            });
            assert_eq!(
                out.first().map(|r| r.provider_id.as_str()),
                Some("applications"),
                "the installed app must be the row Enter activates, ahead of \
                 {} file hits: {:?}",
                out.iter().filter(|r| r.kind == ResourceKind::File).count(),
                out.iter().map(|r| (&r.kind, &r.title)).collect::<Vec<_>>()
            );
            assert_eq!(out[0].id, "so.tilo.calculator");
        }

        // Phase `an_exact_app_name_beats_an_action_that_only_matched_its_subtitle`.
        // A word that exactly names an app must not lose to a word buried in
        // another entry's subtitle, even though actions sort above applications.
        {
            for (needle, action_title, _action_subtitle, action_score) in [
                // "permissions" appears only in the action's subtitle (25).
                (
                    "permissions",
                    "Workspace settings",
                    "General, roles, files, permissions",
                    25,
                ),
                (
                    "settings",
                    "Workspace settings",
                    "General, roles, files, permissions",
                    50,
                ),
            ] {
                let mut reg = SearchRegistry::new();
                reg.register(Rc::new(FakeProvider {
                    id: "actions".into(),
                    kinds: vec![ResourceKind::Action],
                    results: vec![mk_result(
                        ResourceKind::Action,
                        "actions",
                        "open-ws-settings",
                        action_title,
                        action_score,
                    )],
                    max_score: None,
                }));
                reg.register(Rc::new(FakeProvider {
                    id: "applications".into(),
                    kinds: vec![ResourceKind::Application],
                    results: vec![mk_result(
                        ResourceKind::Application,
                        "applications",
                        "so.tilo.app",
                        needle,
                        100,
                    )],
                    max_score: None,
                }));
                let chrome = mk_chrome();
                let (doc, root) = mk_doc();
                let out = reg.query(&QueryArgs {
                    needle,
                    scope: SearchScope::Workspace,
                    active_provider_id: None,
                    wanted_kinds: &[],
                    chrome: &chrome.borrow(),
                    doc: &doc,
                    doc_root: &root,
                });
                assert_eq!(
                    out.first().map(|r| r.provider_id.as_str()),
                    Some("applications"),
                    "typing {needle:?} must offer the app first, not the \
                     action that scored {action_score} on its subtitle: {:?}",
                    out.iter().map(|r| (&r.kind, &r.title)).collect::<Vec<_>>()
                );
            }
        }

        // Phase `an_action_still_leads_when_it_is_the_strongest_match`.
        // The converse: this is a reordering, not a blanket demotion of actions.
        {
            let mut reg = SearchRegistry::new();
            reg.register(Rc::new(FakeProvider {
                id: "actions".into(),
                kinds: vec![ResourceKind::Action],
                results: vec![mk_result(
                    ResourceKind::Action,
                    "actions",
                    "open-ws-settings",
                    "Workspace settings",
                    100,
                )],
                max_score: None,
            }));
            reg.register(Rc::new(FakeProvider {
                id: "applications".into(),
                kinds: vec![ResourceKind::Application],
                results: vec![mk_result(
                    ResourceKind::Application,
                    "applications",
                    "so.tilo.wp",
                    "Word Processor",
                    20,
                )],
                max_score: None,
            }));
            let chrome = mk_chrome();
            let (doc, root) = mk_doc();
            let out = reg.query(&QueryArgs {
                needle: "workspace",
                scope: SearchScope::Workspace,
                active_provider_id: None,
                wanted_kinds: &[],
                chrome: &chrome.borrow(),
                doc: &doc,
                doc_root: &root,
            });
            assert_eq!(
                out.first().map(|r| r.provider_id.as_str()),
                Some("actions"),
                "the action is the strongest match here and must lead: {:?}",
                out.iter().map(|r| (&r.kind, &r.title)).collect::<Vec<_>>()
            );
        }

        // Phase `an_app_is_not_buried_by_a_note_of_the_same_name`.
        // A note titled exactly like an installed app must not take the row
        // Enter activates.
        {
            let mut reg = SearchRegistry::new();
            reg.register(Rc::new(FakeProvider {
                id: "notes".into(),
                kinds: vec![ResourceKind::Note],
                results: vec![mk_result(ResourceKind::Note, "notes", "n1", "Notes", 110)],
                max_score: Some(110),
            }));
            reg.register(Rc::new(FakeProvider {
                id: "applications".into(),
                kinds: vec![ResourceKind::Application],
                results: vec![mk_result(
                    ResourceKind::Application,
                    "applications",
                    "so.tilo.notes",
                    "Open Notes",
                    100,
                )],
                max_score: None,
            }));
            let chrome = mk_chrome();
            let (doc, root) = mk_doc();
            let out = reg.query(&QueryArgs {
                needle: "notes",
                scope: SearchScope::Workspace,
                active_provider_id: None,
                wanted_kinds: &[],
                chrome: &chrome.borrow(),
                doc: &doc,
                doc_root: &root,
            });
            assert_eq!(
                out.first().map(|r| r.provider_id.as_str()),
                Some("applications"),
                "both matched their own ceiling, so the fixed kind order \
                 decides and applications sits above notes: {:?}",
                out.iter().map(|r| (&r.kind, &r.title)).collect::<Vec<_>>()
            );
        }
    }

    #[test]
    fn result_groups_stay_contiguous_and_a_lone_group_is_never_trimmed() {
        // Phase `reordering_groups_keeps_each_kind_contiguous`.
        // Each kind renders as one contiguous block, not interleaved.
        {
            let mut reg = SearchRegistry::new();
            reg.register(Rc::new(FakeProvider {
                id: "files".into(),
                kinds: vec![ResourceKind::File],
                results: vec![
                    mk_result(ResourceKind::File, "files", "f1", "a.rs", 120),
                    mk_result(ResourceKind::File, "files", "f2", "b.rs", 80),
                ],
                max_score: Some(120),
            }));
            reg.register(Rc::new(FakeProvider {
                id: "applications".into(),
                kinds: vec![ResourceKind::Application],
                results: vec![
                    mk_result(
                        ResourceKind::Application,
                        "applications",
                        "a1",
                        "App One",
                        100,
                    ),
                    mk_result(
                        ResourceKind::Application,
                        "applications",
                        "a2",
                        "App Two",
                        25,
                    ),
                ],
                max_score: None,
            }));
            let chrome = mk_chrome();
            let (doc, root) = mk_doc();
            let out = reg.query(&QueryArgs {
                needle: "a",
                scope: SearchScope::Workspace,
                active_provider_id: None,
                wanted_kinds: &[],
                chrome: &chrome.borrow(),
                doc: &doc,
                doc_root: &root,
            });
            let kinds: Vec<&ResourceKind> = out.iter().map(|r| &r.kind).collect();
            let mut seen: Vec<&ResourceKind> = Vec::new();
            for k in &kinds {
                if seen.last() != Some(k) {
                    assert!(
                        !seen.contains(k),
                        "kind {k:?} appears in two separate runs: {kinds:?}"
                    );
                    seen.push(k);
                }
            }
        }

        // Phase `a_lone_group_is_never_trimmed`.
        // Narrowing to a single kind means "show me all of these" and must not be trimmed.
        {
            let mut reg = SearchRegistry::new();
            reg.register(Rc::new(FakeProvider {
                id: "files".into(),
                kinds: vec![ResourceKind::File],
                results: (0..30)
                    .map(|i| mk_result(ResourceKind::File, "files", &format!("f{i}"), "x.rs", 80))
                    .collect(),
                max_score: Some(120),
            }));
            let chrome = mk_chrome();
            let (doc, root) = mk_doc();
            let groups = reg.query_grouped(
                &QueryArgs {
                    needle: "x",
                    scope: SearchScope::Workspace,
                    active_provider_id: None,
                    wanted_kinds: &[ResourceKind::File],
                    chrome: &chrome.borrow(),
                    doc: &doc,
                    doc_root: &root,
                },
                12,
            );
            assert_eq!(groups.len(), 1);
            assert_eq!(
                groups[0].results.len(),
                30,
                "one kind on its own gets the whole budget"
            );
        }
    }

    #[test]
    #[serial_test::serial(process_env)]
    fn an_installed_apps_name_offers_the_app_first_even_in_a_capped_palette() {
        // Phase `typing_an_installed_apps_name_offers_the_app_first`.
        // Driven through the applications provider against an installed-app
        // registry on disk rather than the fixtures above.
        {
            use crate::shell::search::providers::applications::ApplicationsProvider;
            let tmp = tempfile::tempdir().expect("temp app registry");
            install_fake_app(tmp.path(), "so.tilo.calculator", "Calculator");
            install_fake_app(tmp.path(), "so.tilo.notes", "Notes");
            let mut env = tilo_test_env::ScopedEnv::new();
            env.set("TILO_APPS_DATA_DIR", tmp.path());
            crate::shell::launcher::invalidate_app_cache();

            let mut reg = SearchRegistry::new();
            // Registered after files, so winning cannot be an artefact of order.
            reg.register(Rc::new(FakeProvider {
                id: "files".into(),
                kinds: vec![ResourceKind::File],
                results: (0..30)
                    .map(|i| {
                        mk_result(
                            ResourceKind::File,
                            "files",
                            &format!("f{i}"),
                            &format!("calculator_{i}.rs"),
                            120,
                        )
                    })
                    .collect(),
                max_score: Some(120),
            }));
            reg.register(Rc::new(ApplicationsProvider::new()));

            let chrome = mk_chrome();
            let (doc, root) = mk_doc();
            let refresh_args = QueryArgs {
                needle: "calculator",
                scope: SearchScope::All,
                active_provider_id: None,
                wanted_kinds: &[],
                chrome: &chrome.borrow(),
                doc: &doc,
                doc_root: &root,
            };
            refresh_until_ready(&reg, &refresh_args);
            for scope in [SearchScope::All, SearchScope::Workspace] {
                let out = reg.query(&QueryArgs {
                    needle: "calculator",
                    scope,
                    active_provider_id: None,
                    wanted_kinds: &[],
                    chrome: &chrome.borrow(),
                    doc: &doc,
                    doc_root: &root,
                });
                let first = out
                    .first()
                    .unwrap_or_else(|| panic!("some result in {scope:?}"));
                assert_eq!(
                    first.provider_id,
                    "applications",
                    "in {scope:?} the row Enter activates must be the installed \
                     app, not one of the {} file hits: {:?}",
                    out.iter().filter(|r| r.kind == ResourceKind::File).count(),
                    out.iter().take(3).map(|r| &r.title).collect::<Vec<_>>()
                );
                assert_eq!(first.id, "so.tilo.calculator");
            }
            env.remove("TILO_APPS_DATA_DIR");
            crate::shell::launcher::invalidate_app_cache();
        }

        // Phase `a_capped_palette_still_leads_with_the_app`.
        // The app group must survive the trim that keeps a forty-hit file
        // group from filling the card.
        {
            use crate::shell::search::providers::applications::ApplicationsProvider;
            let tmp = tempfile::tempdir().expect("temp app registry");
            install_fake_app(tmp.path(), "so.tilo.hex-editor", "Hex Editor");
            let mut env = tilo_test_env::ScopedEnv::new();
            env.set("TILO_APPS_DATA_DIR", tmp.path());
            crate::shell::launcher::invalidate_app_cache();

            let mut reg = SearchRegistry::new();
            reg.register(Rc::new(FakeProvider {
                id: "files".into(),
                kinds: vec![ResourceKind::File],
                results: (0..40)
                    .map(|i| {
                        mk_result(
                            ResourceKind::File,
                            "files",
                            &format!("f{i}"),
                            &format!("editor_{i:02}.rs"),
                            120,
                        )
                    })
                    .collect(),
                max_score: Some(120),
            }));
            reg.register(Rc::new(ApplicationsProvider::new()));

            let chrome = mk_chrome();
            let (doc, root) = mk_doc();
            let refresh_args = QueryArgs {
                needle: "editor",
                scope: SearchScope::Workspace,
                active_provider_id: None,
                wanted_kinds: &[],
                chrome: &chrome.borrow(),
                doc: &doc,
                doc_root: &root,
            };
            refresh_until_ready(&reg, &refresh_args);
            // "editor" is a leaf-name prefix for all forty files but only a
            // substring of "Hex Editor", so Files legitimately outranks it.
            let groups = reg.query_grouped(
                &QueryArgs {
                    needle: "editor",
                    scope: SearchScope::Workspace,
                    active_provider_id: None,
                    wanted_kinds: &[],
                    chrome: &chrome.borrow(),
                    doc: &doc,
                    doc_root: &root,
                },
                12,
            );
            let files = groups
                .iter()
                .find(|g| g.kind == ResourceKind::File)
                .expect("a files group");
            assert_eq!(files.total, 40, "the count must report the untrimmed truth");
            assert!(
                files.results.len() < files.total,
                "the files group must be trimmed, not shown whole"
            );
            let apps = groups
                .iter()
                .find(|g| g.kind == ResourceKind::Application)
                .expect("the app must still be in the result set at all");
            assert_eq!(apps.results.len(), 1);

            // Must be reachable, not merely present: within the flattened rows.
            let flat: Vec<&SearchResult> = groups.iter().flat_map(|g| g.results.iter()).collect();
            let app_row = flat
                .iter()
                .position(|r| r.kind == ResourceKind::Application)
                .expect("the app is in the flattened list");
            assert!(
                app_row < 8,
                "the app landed at row {app_row}, past the visible list: {:?}",
                flat.iter().take(10).map(|r| &r.title).collect::<Vec<_>>()
            );
            env.remove("TILO_APPS_DATA_DIR");
            crate::shell::launcher::invalidate_app_cache();
        }
    }

    fn ask(
        registry: &SearchRegistry,
        chrome: &crate::shell::chrome_state::Chrome,
        needle: &str,
    ) -> Vec<SearchResult> {
        let (doc, root) = mk_doc();
        let state = chrome.borrow();
        registry.query(&QueryArgs {
            needle,
            scope: SearchScope::All,
            active_provider_id: None,
            wanted_kinds: &[],
            chrome: &state,
            doc: &doc,
            doc_root: &root,
        })
    }

    #[test]
    fn files_follow_the_app_the_query_names_unless_it_names_a_file() {
        let mut reg = SearchRegistry::new();
        reg.register(Rc::new(FakeProvider {
            id: "files".into(),
            kinds: vec![ResourceKind::File],
            results: vec![
                mk_result(
                    ResourceKind::File,
                    "files",
                    "f1",
                    "Settings-backup.txt",
                    120,
                ),
                mk_result(ResourceKind::File, "files", "f2", "settings", 120),
            ],
            max_score: Some(120),
        }));
        reg.register(Rc::new(FakeProvider {
            id: "applications".into(),
            kinds: vec![ResourceKind::Application],
            results: vec![mk_result(
                ResourceKind::Application,
                "applications",
                "so.tilo.settings",
                "Settings",
                40,
            )],
            max_score: None,
        }));
        let chrome = mk_chrome();
        let rows = ask(&reg, &chrome, "settings");
        assert_eq!(
            rows.first().map(|row| row.id.as_str()),
            Some("so.tilo.settings"),
            "a file named like the app comes after it: {:?}",
            rows.iter().map(|row| &row.title).collect::<Vec<_>>()
        );
        assert!(
            rows.iter().any(|row| row.id == "f1"),
            "the files are still offered"
        );
        assert!(looks_like_a_path("Settings-backup.txt"));
        assert!(looks_like_a_path("~/Documents"));
        assert!(!looks_like_a_path("settings"));
        assert!(!looks_like_a_path("v1.2"), "a version is not a file type");
    }

    #[test]
    fn a_row_found_through_a_hidden_word_names_it() {
        use ui::search::FieldWeight;
        let row = mk_result(
            ResourceKind::Action,
            "actions",
            "theme.dark",
            "Dark theme",
            50,
        );
        let fields = vec![
            ("Dark theme".to_owned(), FieldWeight::TITLE),
            ("appearance.theme.dark".to_owned(), FieldWeight::new(90)),
            ("dunkelmodus".to_owned(), FieldWeight::KEYWORD),
            ("night".to_owned(), FieldWeight::KEYWORD),
        ];
        assert_eq!(
            match_reason_in("night", &row, &fields).as_deref(),
            Some("night"),
            "the keyword it came through"
        );
        assert_eq!(
            match_reason_in("dark", &row, &fields),
            None,
            "the title shows it"
        );
        assert_eq!(
            match_reason_in("appearance", &row, &fields),
            None,
            "an address is no reason a person reads"
        );
    }

    #[test]
    fn typo_matches_give_way_once_anything_matches_as_typed() {
        let mut reg = SearchRegistry::new();
        reg.register(Rc::new(FakeProvider {
            id: "loose-actions".into(),
            kinds: vec![ResourceKind::Action],
            results: vec![
                mk_result(
                    ResourceKind::Action,
                    "loose-actions",
                    "m",
                    "Move window group left",
                    100,
                ),
                mk_result(
                    ResourceKind::Action,
                    "loose-actions",
                    "b",
                    "Mobile broadband",
                    10,
                ),
            ],
            max_score: None,
        }));
        let chrome = mk_chrome();
        let rows = ask(&reg, &chrome, "move");
        assert_eq!(
            rows.iter().map(|row| row.id.as_str()).collect::<Vec<_>>(),
            ["m"],
            "a typo match never crowds a real one"
        );
        let mut alone = SearchRegistry::new();
        alone.register(Rc::new(FakeProvider {
            id: "loose-alone".into(),
            kinds: vec![ResourceKind::Application],
            results: vec![mk_result(
                ResourceKind::Application,
                "loose-alone",
                "t",
                "Terminal",
                10,
            )],
            max_score: None,
        }));
        assert_eq!(
            ask(&alone, &chrome, "terminl").len(),
            1,
            "with nothing else, the misspelt name is still found"
        );
    }

    /// "gnome calculator" names Tilo's Calculator by an alias and GNOME's
    /// Calculator (title "Calculator", "gnome" in its id) by every word, spread
    /// over its fields. Both are real matches: the second is no typo and
    /// does not give way to the first.
    #[test]
    fn a_match_on_every_word_spread_over_fields_never_gives_way() {
        let mut reg = SearchRegistry::new();
        reg.register(Rc::new(FakeProvider {
            id: "spread-apps".into(),
            kinds: vec![ResourceKind::Application],
            results: vec![mk_result(
                ResourceKind::Application,
                "spread-apps",
                "so.tilo.calculator",
                "Calculator",
                100,
            )],
            max_score: None,
        }));
        reg.register(Rc::new(FakeProvider {
            id: "spread-catalogue".into(),
            kinds: vec![ResourceKind::Application],
            results: vec![mk_result(
                ResourceKind::Application,
                "spread-catalogue",
                "flathub:org.gnome.Calculator",
                "Calculator",
                provider_score(tilo_search::SPANNING_RANK),
            )],
            max_score: None,
        }));
        let chrome = mk_chrome();
        let rows = ask(&reg, &chrome, "gnome calculator");
        assert!(
            rows.iter()
                .any(|row| row.id == "flathub:org.gnome.Calculator"),
            "the app holding every word typed is listed: {:?}",
            rows.iter().map(|row| row.id.as_str()).collect::<Vec<_>>()
        );
        assert!(rows.iter().any(|row| row.id == "so.tilo.calculator"));
    }

    #[test]
    fn a_short_query_leads_with_what_is_chosen_most() {
        let mut reg = SearchRegistry::new();
        reg.register(Rc::new(FakeProvider {
            id: "usage-merge-apps".into(),
            kinds: vec![ResourceKind::Application],
            results: vec![
                mk_result(
                    ResourceKind::Application,
                    "usage-merge-apps",
                    "tube",
                    "Tube",
                    100,
                ),
                mk_result(
                    ResourceKind::Application,
                    "usage-merge-apps",
                    "terminal",
                    "Terminal",
                    100,
                ),
            ],
            max_score: None,
        }));
        reg.register(Rc::new(FakeProvider {
            id: "usage-merge-actions".into(),
            kinds: vec![ResourceKind::Action],
            results: vec![mk_result(
                ResourceKind::Action,
                "usage-merge-actions",
                "t",
                "T",
                100,
            )],
            max_score: None,
        }));
        let chrome = mk_chrome();
        usage::record_for_test("usage-merge-apps", "terminal", 12);
        let rows = ask(&reg, &chrome, "t");
        assert_eq!(
            rows.first().map(|row| row.id.as_str()),
            Some("terminal"),
            "the app opened every day leads \"t\": {:?}",
            rows.iter().map(|row| &row.title).collect::<Vec<_>>()
        );
        // The text a choice was made for leads next time.
        usage::record_choice("tu", "usage-merge-apps", "tube");
        let rows = ask(&reg, &chrome, "tu");
        assert_eq!(rows.first().map(|row| row.id.as_str()), Some("tube"));
    }

    /// A desktop showing two Alacritty windows, one titled after the app.
    fn two_alacritty_windows() -> Rc<RefCell<ChromeState>> {
        use crate::shell::state::{Tab, TileContent, WindowGroup, WindowWidth};
        let mut tree = WorkspaceTree::bootstrap_default();
        let window = |id: u64, title: &str| WindowGroup {
            id: id.saturating_mul(100),
            tabs: vec![Tab {
                id,
                title: title.into(),
                tile: TileContent::Foreign {
                    app_id: "Alacritty".into(),
                    surface_id: id,
                },
                app_title: None,
            }],
            active_tab: id,
            width: WindowWidth::Normal,
            maximized: false,
            focus_history: Vec::new(),
        };
        tree.desktop.workspaces[0].strips[0].window_groups =
            vec![window(1, "~/src"), window(2, "Alacritty")];
        tree.seed_doc_from_cache();
        ChromeState::new(tree)
    }

    #[test]
    fn an_open_app_is_its_own_row_beside_its_windows_and_still_starts_the_app() {
        use std::sync::atomic::{AtomicBool, Ordering};
        static STARTED: AtomicBool = AtomicBool::new(false);
        struct Apps;
        impl SearchProvider for Apps {
            fn id(&self) -> &'static str {
                "open-app-apps"
            }
            fn name(&self) -> &'static str {
                "apps"
            }
            fn resource_kinds(&self) -> &[ResourceKind] {
                &[ResourceKind::Application]
            }
            fn query(&self, _ctx: SearchContext<'_>) -> Vec<SearchResult> {
                vec![SearchResult::nullary(
                    ResourceKind::Application,
                    "open-app-apps",
                    "Alacritty",
                    "Alacritty",
                    None,
                    40,
                    |_, _, _| STARTED.store(true, Ordering::SeqCst),
                )]
            }
            fn opens_app(&self, result: &SearchResult, app_id: &str) -> bool {
                result.id == app_id
            }
        }
        struct Actions;
        impl SearchProvider for Actions {
            fn id(&self) -> &'static str {
                "open-app-actions"
            }
            fn name(&self) -> &'static str {
                "actions"
            }
            fn resource_kinds(&self) -> &[ResourceKind] {
                &[ResourceKind::Action]
            }
            fn query(&self, _ctx: SearchContext<'_>) -> Vec<SearchResult> {
                vec![mk_result(
                    ResourceKind::Action,
                    "open-app-actions",
                    "alacritty.settings",
                    "Alacritty settings",
                    70,
                )]
            }
        }
        let chrome = two_alacritty_windows();
        let mut reg = SearchRegistry::new();
        reg.register(Rc::new(Apps));
        reg.register(Rc::new(Actions));
        reg.register(Rc::new(providers::DesktopWindowsProvider::new()));
        let rows = ask(&reg, &chrome, "alacritty");
        let at = |pick: &dyn Fn(&SearchResult) -> bool| {
            rows.iter()
                .position(pick)
                .unwrap_or_else(|| panic!("{:?}", rows.iter().map(|r| &r.id).collect::<Vec<_>>()))
        };
        let app = at(&|row| row.provider_id == "open-app-apps");
        let windows: Vec<usize> = rows
            .iter()
            .enumerate()
            .filter(|(_, row)| row.provider_id == tilo_desktop::search::WINDOWS_PROVIDER_ID)
            .filter(|(_, row)| row.kind == ResourceKind::Tab)
            .map(|(index, _)| index)
            .collect();
        assert_eq!(windows.len(), 2, "both windows are rows of their own");
        let mut together = windows;
        together.push(app);
        together.sort_unstable();
        assert!(
            together.windows(2).all(|pair| pair[1] == pair[0] + 1),
            "the app and its windows sit together: {:?}",
            rows.iter().map(|row| &row.title).collect::<Vec<_>>()
        );
        assert_eq!(rows[app].title, "Alacritty");
        assert_eq!(rows[app].subtitle, None);

        let mut doc = AutoCommit::new();
        (rows[app].on_open)(
            &mut ui::widget::Edits::lend(&mut doc),
            &automerge::ROOT,
            &chrome,
            &action_api::ActionArgs::none(),
        )
        .expect("starting the app takes no arguments");
        assert!(STARTED.load(Ordering::SeqCst), "choosing the app starts it");
    }

    /// Write the on-disk shape `launcher::gui_apps` reads.
    fn install_fake_app(root: &std::path::Path, package_id: &str, display_name: &str) {
        let payload = root.join("payloads").join(package_id);
        std::fs::create_dir_all(payload.join("bin")).expect("payload dir");
        let manifest_path = payload.join("manifest.toml");
        std::fs::write(
            &manifest_path,
            crate::shell::test_support::canonical_manifest(
                &format!(
                    "id = \"{package_id}\"\nname = \"{display_name}\"\n\
                     version = \"1.2.3\"\nkind = \"app\"\ncategory = \"Utility\""
                ),
                "[entrypoint]\nbin = \"bin/app\"\ntype = \"gui\"\n",
            ),
        )
        .expect("manifest");
        let registry = root.join("registry.toml");
        let mut existing = std::fs::read_to_string(&registry).unwrap_or_default();
        write!(
            existing,
            "[[entry]]\nid = '{package_id}'\npayload_root = {:?}\nmanifest_path = {:?}\n",
            payload.display().to_string(),
            manifest_path.display().to_string()
        )
        .expect("write to String cannot fail");
        std::fs::write(&registry, existing).expect("registry");
        crate::shell::launcher::invalidate_app_cache();
    }
}
