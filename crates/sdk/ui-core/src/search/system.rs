//! Native and browser system-search policy. Activation payloads remain host-owned.
use crate::CommandEntry;
use std::cmp::Ordering;
use std::collections::HashMap;
pub mod filters;
#[cfg(test)]
mod tests;
pub const PALETTE_ROW_BUDGET: usize = 8;
/// Seeded by which chord opened the palette: `Ctrl+K` →
/// [`SearchScope::ActiveApp`]; `Super+K` / `Super+Space` / `Alt+Space` →
/// [`SearchScope::All`] (`Alt+Space` because a nested Wayland client can't
/// claim Super from the host DE). Degrades rather than empties: no
/// provider claiming the focused tab widens `ActiveApp` to every
/// provider; nothing focused widens the palette to `All`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SearchScope {
    ActiveApp,
    #[default]
    Workspace,
    All,
}

impl SearchScope {
    /// `ActiveApp` renders the tab's title instead of this static
    /// string. Must not share a word with the kind-filter chips —
    /// those name result sorts, this names places.
    pub fn label(self) -> &'static str {
        match self {
            SearchScope::ActiveApp => "This app",
            SearchScope::Workspace => "Workspace",
            SearchScope::All => "Everywhere",
        }
    }

    /// The pill renders a narrowing scope with an explicit `Only …`
    /// prefix so an empty result list is attributable to the scope,
    /// not the query.
    pub fn narrows_results(self) -> bool {
        matches!(self, SearchScope::ActiveApp)
    }
}

/// One well-known category a result can belong to — the domain vocabulary
/// the chip strip and grouping use. Providers pick from it or, via
/// [`ResourceKind::Custom`], mint a new one.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum ResourceKind {
    /// A palette-registered chrome action (e.g. "Open workspace settings").
    Action,
    /// An installed Tilo application. Enter launches it, sandboxed, into
    /// the workspace you are looking at.
    Application,
    /// An app Tilo did not package (Flatpak, distro binary). Its own kind
    /// because a Tilo app runs sandboxed in a room; a host app runs unconfined.
    HostApplication,
    /// A note document (title + body). Enter opens/focuses the note tab.
    Note,
    /// A record an app made, collected or imported. Enter brings it into
    /// the current room (a reference, not a copy) and opens it if openable.
    Record,
    File,
    Folder,
    /// A creator/channel-style entity — pre-declared so a future provider
    /// plugs into the same registry without teaching the palette a new kind.
    Creator,
    Video,
    Playlist,
    /// An open tab on the desktop, as a target for window actions.
    Tab,
    /// Tabs sharing one window frame.
    WindowGroup,
    /// Escape hatch for provider-specific kinds, shown verbatim in the chip strip.
    Custom(String),
}

impl ResourceKind {
    /// Stable English name, independent of the desktop's language: the
    /// source of the chip strip's scriptable `palette:filter:*` address and
    /// the control socket's `kind` field. `Custom` variants carry their own.
    pub fn id(&self) -> &str {
        self.label()
    }

    /// Human-readable label used in the filter chip strip and group
    /// headers. `Custom` variants surface their own string verbatim.
    pub fn label(&self) -> &str {
        match self {
            ResourceKind::Action => "Actions",
            ResourceKind::Application => "Applications",
            ResourceKind::HostApplication => "Host Apps",
            ResourceKind::Note => "Notes",
            ResourceKind::Record => "Records",
            ResourceKind::File => "Files",
            ResourceKind::Folder => "Folders",
            ResourceKind::Creator => "Creators",
            ResourceKind::Video => "Videos",
            ResourceKind::Playlist => "Playlists",
            ResourceKind::Tab => "Tabs",
            ResourceKind::WindowGroup => "Window groups",
            ResourceKind::Custom(s) => s.as_str(),
        }
    }

    /// Stable sort key pinning the well-known kinds to a fixed palette
    /// order. Applications rank above content kinds because Enter
    /// activates the top row — a typed app name must launch the app, not
    /// surface a source file that merely contains the word.
    pub fn sort_key(&self) -> (u8, String) {
        match self {
            ResourceKind::Action => (0, String::new()),
            ResourceKind::Application => (1, String::new()),
            ResourceKind::HostApplication => (2, String::new()),
            ResourceKind::Note => (3, String::new()),
            ResourceKind::Record => (4, String::new()),
            ResourceKind::File => (5, String::new()),
            ResourceKind::Folder => (6, String::new()),
            ResourceKind::Creator => (7, String::new()),
            ResourceKind::Video => (8, String::new()),
            ResourceKind::Playlist => (9, String::new()),
            ResourceKind::Tab => (10, String::new()),
            ResourceKind::WindowGroup => (11, String::new()),
            ResourceKind::Custom(s) => (12, s.clone()),
        }
    }
}

/// Hard ceilings for one provider page.  These are deliberately expressed in
/// the protocol rather than as palette constants: the same limits protect the
/// visual palette, headless agents, and a future remote directory source.
/// A provider may stop sooner, but must never exceed any non-zero ceiling.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SearchBudget {
    pub max_results: usize,
    pub max_bytes: usize,
    pub max_work: usize,
}

impl SearchBudget {
    pub const PALETTE: Self = Self {
        max_results: 48,
        max_bytes: 48 * 1024,
        max_work: 4_096,
    };
}

/// An opaque provider-owned continuation.  The revision is carried alongside
/// the provider-local payload, so a result page can never silently advance
/// across a changed ordering.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct SearchCursor {
    pub revision: String,
    pub token: String,
}

/// One bounded answer from a [`Provider`].  `stale_cursor` is a
/// protocol outcome rather than an empty page: callers restart explicitly,
/// preserving the distinction between "there are no more hits" and "the
/// source changed while you were paging".
#[derive(Clone)]
pub struct SearchPage<Row> {
    pub revision: String,
    pub results: Vec<Row>,
    pub next_cursor: Option<SearchCursor>,
    pub exhausted: bool,
    pub stale_cursor: bool,
    pub budget_exhausted: bool,
}

impl<Row: std::fmt::Debug> std::fmt::Debug for SearchPage<Row> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SearchPage")
            .field("revision", &self.revision)
            .field("results", &self.results)
            .field("next_cursor", &self.next_cursor)
            .field("exhausted", &self.exhausted)
            .field("stale_cursor", &self.stale_cursor)
            .field("budget_exhausted", &self.budget_exhausted)
            .finish()
    }
}

impl<Row> SearchPage<Row> {
    pub fn exhausted(revision: impl Into<String>, results: Vec<Row>) -> Self {
        Self {
            revision: revision.into(),
            results,
            next_cursor: None,
            exhausted: true,
            stale_cursor: false,
            budget_exhausted: false,
        }
    }

    pub fn stale(revision: impl Into<String>) -> Self {
        Self {
            revision: revision.into(),
            results: Vec::new(),
            next_cursor: None,
            exhausted: false,
            stale_cursor: true,
            budget_exhausted: false,
        }
    }
}

/// Portable display metadata; implement on a host row without moving its callbacks.
pub trait SearchRow {
    fn kind(&self) -> &ResourceKind;
    fn score(&self) -> i32;
    fn title(&self) -> &str;
    fn subtitle(&self) -> Option<&str>;
    fn shortcut(&self) -> Option<&str> {
        None
    }
}
#[derive(Debug)]
pub struct KindGroup<Row> {
    pub kind: ResourceKind,
    pub results: Vec<Row>,
    pub total: usize,
}
pub fn query_input_is_bounded(needle: &str) -> bool {
    needle
        .chars()
        .take(super::MAX_SEARCH_QUERY_SCALARS.saturating_add(1))
        .count()
        <= super::MAX_SEARCH_QUERY_SCALARS
}
pub fn scope_allows(
    scope: SearchScope,
    provider_id: &str,
    active_provider_id: Option<&str>,
    available_in_active_app: bool,
) -> bool {
    match scope {
        SearchScope::All | SearchScope::Workspace => true,
        SearchScope::ActiveApp => {
            active_provider_id.is_none_or(|id| id == provider_id || available_in_active_app)
        }
    }
}
/// The package id in full names exactly one app, so an exact id is as
/// strong as the name; a shared id prefix names a publisher and stays below
/// an exact name.
pub const PACKAGE_ID_WEIGHT: super::FieldWeight = super::FieldWeight::new(88);

/// An app's searchable fields. The package id in full, or its last
/// segment, identifies exactly one app and weighs as its name —
/// `hex-editor` typed for `so.tilo.hex-editor` is the app's identity and
/// must outrank an incidental substring. Category ranks below the id
/// because it's a bucket, not an identity.
#[must_use]
pub fn application_fields<'a>(
    name: &'a str,
    package_id: &'a str,
    category: Option<&'a str>,
    description: Option<&'a str>,
) -> [(&'a str, super::FieldWeight); 5] {
    use super::FieldWeight;
    let last_segment = package_id.rsplit('.').next().unwrap_or(package_id);
    [
        (name, FieldWeight::TITLE),
        (last_segment, FieldWeight::TITLE),
        (package_id, PACKAGE_ID_WEIGHT),
        (category.unwrap_or_default(), FieldWeight::METADATA),
        (description.unwrap_or_default(), FieldWeight::BODY),
    ]
}

/// A field rank on the 0–100 provider scale. A title prefix is already a
/// perfect hit for a provider — it is what a person types to launch or
/// open something — so the scale tops out there; merging still separates
/// an exact title from a prefix.
#[must_use]
pub fn provider_score(rank: i32) -> i32 {
    // A full-weight field ranks a tier at its whole percentage.
    let perfect = super::MatchTier::Prefix.percent();
    rank.clamp(0, perfect)
        .saturating_mul(100)
        .checked_div(perfect)
        .unwrap_or(100)
}

#[allow(
    clippy::cast_precision_loss,
    reason = "Ranking normalizes integer priorities into approximate floating-point scores, not durable identifiers."
)]
pub fn rank<Row: SearchRow>(rows: Vec<(Row, i32)>) -> Vec<Row> {
    let mut merged: Vec<(Row, f32)> = rows
        .into_iter()
        .map(|(row, top)| {
            let top = top.max(1);
            debug_assert!(
                row.score() <= top,
                "provider exceeded its declared maximum score"
            );
            let strength = row.score() as f32 / top as f32;
            (row, strength)
        })
        .collect();
    // Order groups by how well their best member matched, then the
    // fixed kind order: Enter activates the top row, so an app's name
    // must not open an action that merely mentions the word.
    let mut best: HashMap<(u8, String), f32> = HashMap::new();
    for (r, strength) in &merged {
        let slot = best.entry(r.kind().sort_key()).or_insert(f32::MIN);
        *slot = slot.max(*strength);
    }
    merged.sort_by(|(a, a_strength), (b, b_strength)| {
        // Every key looked up here was inserted by the loop above
        // (it walks the same `merged`), but `.get()` + a matching
        // fallback avoids an avoidable panic in a sort comparator
        // rather than relying on that invariant holding forever.
        let a_group = best.get(&a.kind().sort_key()).copied().unwrap_or(f32::MIN);
        let b_group = best.get(&b.kind().sort_key()).copied().unwrap_or(f32::MIN);
        b_group
            .partial_cmp(&a_group)
            .unwrap_or(Ordering::Equal)
            // Equally strong groups fall back to the fixed kind order.
            .then_with(|| a.kind().sort_key().cmp(&b.kind().sort_key()))
            .then_with(|| {
                b_strength
                    .partial_cmp(a_strength)
                    .unwrap_or(Ordering::Equal)
            })
        // No further tiebreak: `sort_by` is stable, so equally strong
        // results keep the provider's own order rather than being
        // re-alphabetised by title.
    });
    merged.into_iter().map(|(r, _)| r).collect()
}
pub fn group<Row: SearchRow>(ranked: Vec<Row>, row_budget: usize) -> Vec<KindGroup<Row>> {
    let mut groups: Vec<KindGroup<Row>> = Vec::new();
    for r in ranked {
        match groups.last_mut() {
            Some(g) if g.kind == *r.kind() => {
                g.total = g.total.saturating_add(1);
                g.results.push(r);
            }
            _ => groups.push(KindGroup {
                kind: r.kind().clone(),
                total: 1,
                results: vec![r],
            }),
        }
    }
    if row_budget == 0
        || groups.len() < 2
        || groups.iter().map(|g| g.total).sum::<usize>() <= row_budget
    {
        return groups;
    }
    // Fair share, front to back: each group takes an equal cut of
    // whatever budget remains, and what it can't fill rolls forward
    // (never backward, or later headers push further down the card).
    let n = groups.len();
    let mut remaining = row_budget;
    for (i, g) in groups.iter_mut().enumerate() {
        let groups_left = n.saturating_sub(i).max(1);
        let share = remaining.checked_div(groups_left).unwrap_or(1).max(1);
        let keep = share.min(g.total);
        g.results.truncate(keep);
        remaining = remaining.saturating_sub(keep);
    }
    groups
}
pub fn command_entries<Row: SearchRow>(groups: &[KindGroup<Row>]) -> Vec<CommandEntry> {
    let mut entries = Vec::new();
    for group in groups {
        for row in &group.results {
            let mut entry = CommandEntry::new(format!("result:{}", entries.len()), row.title())
                .group(group.kind.label())
                .group_total(group.total)
                .icon(icon_for_kind(row.kind()));
            if let Some(subtitle) = row.subtitle() {
                entry = entry.description(subtitle);
            }
            if let Some(shortcut) = row.shortcut() {
                entry = entry.keybinding(shortcut);
            }
            entries.push(entry);
        }
    }
    entries
}
pub fn icon_for_kind(kind: &ResourceKind) -> crate::icons::Icon {
    use crate::icons::Icon;
    use ResourceKind as K;
    match kind {
        K::Action => Icon::Settings2,
        // The app-less fallback; a row that knows its app leads with
        // [`crate::icons::app_glyph`] or the app's artwork instead. A window
        // is an app on screen, so it shares it; the magnifier names the
        // search box, not a result.
        K::Application | K::Tab | K::WindowGroup => Icon::AppWindow,
        // A host app is not one of Tilo's — it runs unconfined — so when it
        // has no artwork of its own the fallback looks different at a glance.
        K::HostApplication => Icon::ExternalLink,
        K::Note => Icon::FileText,
        // A record is one document several rooms can reach — the stacked
        // plates say "this is the same thing in more than one place",
        // which is exactly what a reference is.
        K::Record => Icon::Layers,
        K::File => Icon::File,
        K::Folder => Icon::Folder,
        K::Creator => Icon::Star,
        K::Video => Icon::Film,
        K::Playlist => Icon::List,
        K::Custom(_) => Icon::Box,
    }
}

#[derive(Clone, Copy, Debug)]
pub struct Query<'a> {
    pub needle: &'a str,
    pub scope: SearchScope,
    pub active_provider_id: Option<&'a str>,
    pub wanted_kinds: &'a [ResourceKind],
}
/// Typed fanout boundary. The host supplies retained providers and activation payloads.
pub trait Provider<Host, Row: SearchRow> {
    fn id(&self) -> &str;
    fn resource_kinds(&self) -> &[ResourceKind];
    fn max_score(&self) -> i32 {
        100
    }
    fn available_in_active_app_scope(&self) -> bool {
        false
    }
    fn revision(&self, _host: &Host, _query: &Query<'_>) -> String {
        "static".into()
    }
    fn query_page(
        &self,
        host: &Host,
        query: &Query<'_>,
        cursor: Option<&SearchCursor>,
        budget: SearchBudget,
    ) -> SearchPage<Row>;
    fn resolve(&self, host: &Host, id: &str, query: &Query<'_>) -> Option<Row>;
}
/// Registration order survives equal-strength ties. Borrowed registrations also support
/// query-scoped native adapters without transferring retained provider ownership.
pub struct Registry<'a, Host, Row: SearchRow> {
    providers: Vec<&'a dyn Provider<Host, Row>>,
}
impl<Host, Row: SearchRow> Default for Registry<'_, Host, Row> {
    fn default() -> Self {
        Self {
            providers: Vec::new(),
        }
    }
}
impl<'a, Host, Row: SearchRow> Registry<'a, Host, Row> {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn register(&mut self, provider: &'a dyn Provider<Host, Row>) {
        self.providers.push(provider);
    }
    pub fn registered_kinds(&self) -> Vec<ResourceKind> {
        let mut kinds = Vec::new();
        for provider in &self.providers {
            for kind in provider.resource_kinds() {
                if !kinds.contains(kind) {
                    kinds.push(kind.clone());
                }
            }
        }
        kinds.sort_by_key(ResourceKind::sort_key);
        kinds
    }
    pub fn query(&self, host: &Host, query: &Query<'_>) -> Vec<Row> {
        if !query_input_is_bounded(query.needle) {
            return Vec::new();
        }
        let mut rows = Vec::new();
        for provider in &self.providers {
            if !scope_allows(
                query.scope,
                provider.id(),
                query.active_provider_id,
                provider.available_in_active_app_scope(),
            ) {
                continue;
            }
            let wanted: Vec<_> = provider
                .resource_kinds()
                .iter()
                .filter(|k| query.wanted_kinds.is_empty() || query.wanted_kinds.contains(k))
                .cloned()
                .collect();
            if wanted.is_empty() {
                continue;
            }
            let routed = Query {
                wanted_kinds: &wanted,
                ..*query
            };
            let page = provider.query_page(host, &routed, None, SearchBudget::PALETTE);
            if page.stale_cursor {
                continue;
            }
            rows.extend(
                page.results
                    .into_iter()
                    .map(|row| (row, provider.max_score())),
            );
        }
        rank(rows)
    }
    pub fn query_grouped(
        &self,
        host: &Host,
        query: &Query<'_>,
        row_budget: usize,
    ) -> Vec<KindGroup<Row>> {
        group(self.query(host, query), row_budget)
    }
    pub fn resolve(
        &self,
        host: &Host,
        provider_id: &str,
        id: &str,
        query: &Query<'_>,
    ) -> Option<Row> {
        let provider = self.providers.iter().find(|p| p.id() == provider_id)?;
        if !scope_allows(
            query.scope,
            provider_id,
            query.active_provider_id,
            provider.available_in_active_app_scope(),
        ) {
            return None;
        }
        let wanted: Vec<_> = provider
            .resource_kinds()
            .iter()
            .filter(|k| query.wanted_kinds.is_empty() || query.wanted_kinds.contains(k))
            .cloned()
            .collect();
        if wanted.is_empty() {
            return None;
        }
        provider
            .resolve(
                host,
                id,
                &Query {
                    needle: "",
                    wanted_kinds: &wanted,
                    ..*query
                },
            )
            .filter(|r| wanted.contains(r.kind()))
    }
}
