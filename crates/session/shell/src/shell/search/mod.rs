//! Cross-app search protocol.
//!
//! The shell's global palette fans out to any number of [`SearchProvider`]s,
//! each owning a domain (shell actions, notes, filesystem, an app's own
//! resources). Providers answer a single `query` call per keystroke — no
//! re-query per resource kind, so a network-backed provider gets exactly one
//! round-trip per input. [`SearchScope::ActiveApp`] narrows the fanout to
//! whichever provider owns the focused tab ([`SearchProvider::owns_tab`]);
//! `Workspace` and `All` fan out to every provider, but `Workspace` ranks
//! resources from the focused app ahead of equally relevant global results.

pub mod action_api;
pub(crate) mod activation_trace;
pub mod files_index;
pub mod invocation_wake;
pub mod providers;
pub mod visibility;

use std::cell::RefCell;
use std::cmp::Ordering;
use std::collections::{HashMap, HashSet};
use std::rc::Rc;

use automerge::{AutoCommit, ObjId};

use crate::shell::chrome_state::{Chrome, ChromeState, SearchScope};
use crate::shell::i18n::t;
use action_api::{ActionArgs, ActionError, ActionParam};

use ui::search::system::{self, query_input_is_bounded};

/// Portable search vocabulary and provider page contract, retained at the native API path.
pub use ui::search::system::{ResourceKind, SearchBudget, SearchCursor};
pub type SearchPage = system::SearchPage<SearchResult>;
pub type KindGroup = system::KindGroup<SearchResult>;

/// Provider-independent lexical fit used at the global merge boundary. A
/// provider still contributes domain knowledge through its normalized score,
/// but an exact visible title must beat a weak metadata/fuzzy match from a
/// different source.
fn lexical_strength(needle: &str, result: &SearchResult) -> f32 {
    use ui::search::MatchTier;
    let needle = ui::search::fold_search_text(needle.trim());
    if needle.is_empty() {
        return 0.0;
    }
    let title = ui::search::fold_search_text(&result.title);
    // Application providers decorate the app name with the launch verb.
    // Compare its name on the same basis as a file or note title.
    let title = if matches!(
        result.kind,
        ResourceKind::Application | ResourceKind::HostApplication
    ) {
        title.strip_prefix("open ").unwrap_or(&title)
    } else {
        &title
    };
    match ui::search::rank::match_tier(&needle, title) {
        Some(MatchTier::Exact) => 4.0,
        Some(MatchTier::Prefix) => 2.5,
        Some(MatchTier::WordPrefix) => 1.5,
        Some(_) => 1.0,
        None => {
            result.subtitle.as_deref().map_or(0.0, |subtitle| {
                match ui::search::rank::match_tier(&needle, &ui::search::fold_search_text(subtitle))
                {
                    Some(MatchTier::Exact) => 0.7,
                    Some(_) => 0.4,
                    None => 0.0,
                }
            })
        }
    }
}

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

/// A deliberately small contextual preference: proximity should settle close
/// matches, never make a weak metadata hit beat an exact visible title.
const ACTIVE_APP_PROXIMITY_BOOST: f32 = 0.25;

/// A provider-perfect application-name match represents launch intent, not a
/// coincidental lexical occurrence. It must beat an exact filename/note with
/// the same word, while an exact action (strength 5.0) still beats an
/// unrelated app that merely received a weak provider score.
const PERFECT_APPLICATION_INTENT_BOOST: f32 = 3.0;

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
            ResourceKind::HostApplication => t("search-kind-host-apps"),
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
    /// Optional short live status rendered by trusted shell chrome.
    pub badge: Option<String>,
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
            badge: None,
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
            badge: None,
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
        SearchPage {
            revision: revision.clone(),
            results,
            next_cursor: (!exhausted).then(|| SearchCursor {
                revision: revision.clone(),
                token: "legacy-truncated".into(),
            }),
            exhausted,
            stale_cursor: false,
            budget_exhausted: !exhausted,
        }
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
    /// Keep this provider available while the palette is narrowed to the
    /// focused app. Shell-level commands are still useful in that scope: a
    /// person should not have to switch search scopes just to open Activity,
    /// Settings, or another piece of workspace chrome.
    fn available_in_active_app_scope(&self) -> bool {
        false
    }

    /// Contextual location of one result. Providers know what their opaque
    /// ids mean; the registry does not. Workspace scope uses this only as a
    /// bounded ranking preference, never as a visibility filter.
    fn proximity(&self, _result: &SearchResult, _chrome: &ChromeState) -> SearchProximity {
        SearchProximity::Global
    }
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
    pub action_scope: Option<&'a ActionDiscoveryScope>,
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

/// Exact live application discovery, separate from a free-text search needle.
/// This narrows enumeration; existing actor, route and visibility gates still
/// determine which descriptors the caller can observe.
#[derive(Clone, Debug, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
pub struct ActionDiscoveryScope {
    pub application: String,
    pub room: String,
    pub record: String,
    pub semantic_family: String,
}

impl ActionDiscoveryScope {
    pub fn is_valid(&self) -> bool {
        let identifier = |value: &str| {
            !value.is_empty()
                && value.len() <= 128
                && value
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b'_'))
        };
        identifier(&self.application)
            && identifier(&self.semantic_family)
            && uuid::Uuid::parse_str(&self.room).is_ok()
            && !self.record.is_empty()
            && self.record.len() <= 512
            && !self.record.chars().any(char::is_control)
    }

    pub fn matches_route(&self, application: &str, room: &str, record: &str) -> bool {
        self.application == application && self.room == room && self.record == record
    }

    fn allows_provider(provider: &dyn SearchProvider) -> bool {
        provider.id() == "app-actions"
    }
}

/// Arguments for [`SearchRegistry::query`]/[`SearchRegistry::query_grouped`]
/// — distinct from [`SearchContext`], which is the narrower, per-provider
/// view built fresh for each provider inside the fan-out loop.
pub struct QueryArgs<'a> {
    pub action_scope: Option<&'a ActionDiscoveryScope>,
    pub needle: &'a str,
    pub scope: SearchScope,
    pub active_provider_id: Option<&'a str>,
    pub wanted_kinds: &'a [ResourceKind],
    pub chrome: &'a ChromeState,
    pub doc: &'a AutoCommit,
    pub doc_root: &'a ObjId,
}

/// A [`SearchResult::score`]/[`SearchProvider::max_score`] value as a
/// fraction of `[0.0, 1.0]`-scale `f32`. Provider score scales are
/// small (files top out at 120, apps at 100 — see `max_score`'s doc),
/// well under `f32`'s 24-bit mantissa, so the precision this cast could
/// otherwise lose cannot happen for a supported provider.
#[allow(
    clippy::cast_precision_loss,
    reason = "provider score scales are small integers (<=120 by convention); exact precision isn't needed for a 0..1 strength ratio"
)]
fn i32_to_f32_lossy(n: i32) -> f32 {
    n as f32
}

fn estimated_result_bytes(result: &SearchResult) -> usize {
    result
        .provider_id
        .len()
        .saturating_add(result.id.len())
        .saturating_add(result.title.len())
        .saturating_add(result.subtitle.as_ref().map_or(0, String::len))
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
            action_scope: None,
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
            if args
                .action_scope
                .is_some_and(|_| !ActionDiscoveryScope::allows_provider(provider.as_ref()))
            {
                continue;
            }
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
                action_scope: args.action_scope,
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
                action_scope: args.action_scope,
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
            if args
                .action_scope
                .is_some_and(|_| !ActionDiscoveryScope::allows_provider(provider.as_ref()))
            {
                continue;
            }
            if !scope_allows(provider.as_ref(), args.scope, args.active_provider_id) {
                continue;
            }
            out.push_str(provider.id());
            out.push('\0');
            out.push_str(&provider.search_revision(&SearchContext {
                query_scope: Some(args.scope),
                action_scope: args.action_scope,
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

    /// Explicit machine enumeration includes programmatic actions within the
    /// same bounded provider, continuation and visibility machinery.
    pub fn query_with_programmatic(&self, args: &QueryArgs<'_>) -> Vec<SearchResult> {
        self.query_inner(args, true, true).0
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

    fn query_rounds(
        &self,
        args: &QueryArgs<'_>,
        exhaustive: bool,
        include_programmatic: bool,
    ) -> Vec<ProviderQuery<'_>> {
        let mut sources: Vec<_> = self
            .enabled_providers()
            .filter(|provider| {
                args.action_scope
                    .is_none_or(|_| ActionDiscoveryScope::allows_provider(provider.as_ref()))
            })
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
                        action_scope: args.action_scope,
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
        let QueryArgs {
            needle,
            scope,
            active_provider_id,
            chrome,
            ..
        } = *args;
        let mut merged: Vec<(SearchResult, f32)> = Vec::new();
        let mut canonical_rows: HashMap<(String, String), usize> = HashMap::new();
        let mut incomplete = Vec::new();
        for source in self.query_rounds(args, exhaustive, include_programmatic) {
            let provider = source.provider;
            source.report_incomplete(args, include_programmatic, &mut incomplete);
            let top = provider.max_score().max(1);
            for r in source.results {
                // A score above `top` exceeds strength 1.0 and silently
                // outranks every other provider's perfect match.
                debug_assert!(
                    r.score <= top,
                    "provider {:?} returned {} but declares max_score() = {top}; \
                     raise max_score() or bound the score",
                    provider.id(),
                    r.score,
                );
                let proximity = if scope == SearchScope::Workspace {
                    let active_app = if active_provider_id == Some(provider.id()) {
                        ACTIVE_APP_PROXIMITY_BOOST
                    } else {
                        0.0
                    };
                    let workspace =
                        if provider.proximity(&r, chrome) == SearchProximity::CurrentWorkspace {
                            ACTIVE_APP_PROXIMITY_BOOST
                        } else {
                            0.0
                        };
                    active_app + workspace
                } else {
                    0.0
                };
                let normalized_score = i32_to_f32_lossy(r.score) / i32_to_f32_lossy(top);
                let application_intent = if matches!(
                    r.kind,
                    ResourceKind::Application | ResourceKind::HostApplication
                ) && r.score >= top
                {
                    PERFECT_APPLICATION_INTENT_BOOST
                } else {
                    0.0
                };
                let strength = lexical_strength(needle, &r)
                    + normalized_score
                    + application_intent
                    + proximity;
                let key = (r.provider_id.clone(), r.id.clone());
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
        // Order groups by how well their best member matched, then the
        // fixed kind order: Enter activates the top row, so an app's name
        // must not open an action that merely mentions the word.
        let mut best: HashMap<(u8, String), f32> = HashMap::new();
        for (r, strength) in &merged {
            let slot = best.entry(r.kind.sort_key()).or_insert(f32::MIN);
            *slot = slot.max(*strength);
        }
        merged.sort_by(|(a, a_strength), (b, b_strength)| {
            // Every key looked up here was inserted by the loop above
            // (it walks the same `merged`), but `.get()` + a matching
            // fallback avoids an avoidable panic in a sort comparator
            // rather than relying on that invariant holding forever.
            let a_group = best.get(&a.kind.sort_key()).copied().unwrap_or(f32::MIN);
            let b_group = best.get(&b.kind.sort_key()).copied().unwrap_or(f32::MIN);
            b_group
                .partial_cmp(&a_group)
                .unwrap_or(Ordering::Equal)
                // Equally strong groups fall back to the fixed kind order.
                .then_with(|| a.kind.sort_key().cmp(&b.kind.sort_key()))
                .then_with(|| {
                    b_strength
                        .partial_cmp(a_strength)
                        .unwrap_or(Ordering::Equal)
                })
            // No further tiebreak: `sort_by` is stable, so equally strong
            // results keep the provider's own order rather than being
            // re-alphabetised by title.
        });
        let completion = QueryCompletion {
            complete: incomplete.is_empty(),
            incomplete,
        };
        (merged.into_iter().map(|(r, _)| r).collect(), completion)
    }

    /// [`Self::query`], arranged into contiguous per-kind groups and
    /// trimmed to a `row_budget` shared round-robin: the palette paints
    /// ~10 rows, and an outscoring kind (forty file hits) can otherwise
    /// push a correctly-ranked app off the bottom. A lone group is exempt
    /// — narrowing to one kind means "show me all of these".
    ///
    /// With nothing typed and no kind chosen, the desktop-wide palette offers
    /// [`jump_back_in`] instead of listing everything; scoped to one app it
    /// still lists that app's commands.
    pub fn query_grouped(&self, args: &QueryArgs<'_>, row_budget: usize) -> Vec<KindGroup> {
        if args.needle.trim().is_empty()
            && args.wanted_kinds.is_empty()
            && args.scope != SearchScope::ActiveApp
        {
            return jump_back_in(self, args);
        }
        system::group(self.query(args), row_budget)
    }
}

/// Rows per group the empty palette shows.
const JUMP_BACK_IN_GROUP_ROWS: usize = 4;

/// The starting points the empty palette offers after windows and apps.
const JUMP_BACK_IN_SUGGESTIONS: [&str; 3] =
    ["room.activity.open", "peer.invite", "shortcut-help.toggle"];

const JUMP_BACK_IN_ACTIONS_PROVIDER: &str = "actions";

/// The palette before anything is typed: windows to go back to, apps, and a
/// few ways in. It is never the full inventory, and never offers a row that
/// changes something: close, move and revoke need their verb typed. Agents
/// still enumerate everything through [`SearchRegistry::query_exhaustive`].
///
/// Interactive search is one bounded page, so late catalogue rows (help,
/// the tour) are not on that page. Suggestions are fetched by exact id.
fn jump_back_in(registry: &SearchRegistry, args: &QueryArgs<'_>) -> Vec<KindGroup> {
    let rows = registry.query(args);
    let windows = rows
        .iter()
        .filter(|row| matches!(row.kind, ResourceKind::Tab | ResourceKind::WindowGroup))
        .filter(|row| row.operation_kind == tilo_actions::OperationKind::View)
        .take(JUMP_BACK_IN_GROUP_ROWS)
        .cloned()
        .collect::<Vec<_>>();
    let apps = rows
        .iter()
        .filter(|row| row.kind == ResourceKind::Application)
        .take(JUMP_BACK_IN_GROUP_ROWS)
        .cloned()
        .collect::<Vec<_>>();
    let suggestions = JUMP_BACK_IN_SUGGESTIONS
        .iter()
        .filter_map(|id| registry.resolve(JUMP_BACK_IN_ACTIONS_PROVIDER, id, args))
        .collect::<Vec<_>>();
    [
        (ResourceKind::Tab, windows),
        (ResourceKind::Application, apps),
        (ResourceKind::Action, suggestions),
    ]
    .into_iter()
    .filter(|(_, results)| !results.is_empty())
    .map(|(kind, results)| KindGroup {
        kind,
        total: results.len(),
        results,
    })
    .collect()
}

fn scope_allows(
    provider: &dyn SearchProvider,
    scope: SearchScope,
    active_provider_id: Option<&str>,
) -> bool {
    match scope {
        SearchScope::All | SearchScope::Workspace => true,
        SearchScope::ActiveApp => match active_provider_id {
            Some(id) => provider.id() == id || provider.available_in_active_app_scope(),
            // The overlay explicitly degrades to `All` when there is no
            // focused tab. Reaching this branch means a focused app exists
            // but its primary provider is not ready; fail narrow rather than
            // leaking unrelated workspace results into "This app".
            None => provider.available_in_active_app_scope(),
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
        (self.id == "room.activity.open").then_some("Super+H")
    }
}

#[cfg(test)]
mod tests {
    /// Bounds a wait that ends as soon as its condition holds; only a hang reaches it.
    const HANG_BOUND: std::time::Duration = std::time::Duration::from_secs(30);

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
        let deadline = std::time::Instant::now()
            .checked_add(HANG_BOUND)
            .expect("the hang bound fits in an Instant");
        while std::time::Instant::now() < deadline {
            registry.refresh_indexes(args);
            if registry.query_revision(args).contains("ready:") {
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        panic!("retained search index did not become ready");
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
                action_scope: None,
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
                action_scope: None,
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
                action_scope: None,
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
                    mk_result(ResourceKind::Note, "notes", "n1", "note b", 10),
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
                action_scope: None,
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
                action_scope: None,
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
                action_scope: None,
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
                action_scope: None,
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
                action_scope: None,
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
                action_scope: None,
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
                    action_scope: None,
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
                    action_scope: None,
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
                    action_scope: None,
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
                action_scope: None,
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

        // Phase `active_app_scope_keeps_shell_actions_available`.
        {
            use crate::shell::search::providers::actions::ActionsProvider;

            assert!(scope_allows(
                &ActionsProvider::new(),
                SearchScope::ActiveApp,
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
                action_scope: None,
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
                action_scope: None,
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
                action_scope: None,
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
                    action_scope: None,
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
                action_scope: None,
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
                action_scope: None,
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
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
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
                action_scope: None,
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
                    action_scope: None,
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
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
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
                action_scope: None,
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
                    action_scope: None,
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
                action_scope: None,
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
                    action_scope: None,
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

    /// Write the on-disk shape `launcher::gui_apps` reads.
    fn install_fake_app(root: &std::path::Path, package_id: &str, display_name: &str) {
        let payload = root.join("payloads").join(package_id);
        std::fs::create_dir_all(payload.join("bin")).expect("payload dir");
        let manifest_path = payload.join("manifest.toml");
        std::fs::write(
            &manifest_path,
            format!(
                "[package]\nid = \"{package_id}\"\nname = \"{display_name}\"\n\
                 version = \"1.2.3\"\ncategory = \"Utility\"\n\
                 [entrypoint]\nbin = \"bin/app\"\ntype = \"gui\"\n"
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
