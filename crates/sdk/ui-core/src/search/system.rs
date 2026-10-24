//! Native and browser system-search policy. Activation payloads remain host-owned.
use crate::CommandEntry;
use std::cmp::Ordering;
use std::collections::HashMap;
use tilo_sort::CompactSort as _;
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
    /// An app the person has or can get: a Tilo app, a Flatpak, a
    /// downloaded program or one of the system's. Where it comes from is a
    /// fact on its row, not a kind of its own. Enter starts it in the
    /// workspace you are looking at.
    Application,
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
            ResourceKind::Application => "Apps",
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

/// How far a hit is from the person: what they have, an app Tilo opens at
/// once, or an app they would first have to install. A property of the hit,
/// set by the provider that knows it, and read by the merge
/// ([`order_hits`]): what the person has, and the Tilo apps that just open,
/// rank by match quality; a third-party app the person does not have (a
/// Flathub app's Store page) ranks after all of them, however well its name
/// matches, ordered among its kind by match quality and still listed.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum Availability {
    /// On this device or the person's own: an installed app, a file, a
    /// message, an action, a setting.
    #[default]
    OnDevice,
    /// A Tilo app from the catalogue, its documents and actions: not on this
    /// device, but Enter downloads and opens it, with no install step.
    TiloCatalogue,
    /// An app from a third-party catalogue (Flathub) the person does not
    /// have: Enter shows its Store page, where they decide to install it.
    ThirdPartyCatalogue,
}

impl Availability {
    /// The band the merge orders the hit in, before match quality: every
    /// hit the person can open now leads every app they would have to
    /// install first.
    #[must_use]
    pub const fn band(self) -> u8 {
        match self {
            Self::OnDevice | Self::TiloCatalogue => 0,
            Self::ThirdPartyCatalogue => 1,
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

    /// The last page when `next_token` is `None`; otherwise a page its budget
    /// cut short, continuing at that token.
    pub fn continuing(
        revision: impl Into<String>,
        results: Vec<Row>,
        next_token: Option<String>,
    ) -> Self {
        let revision = revision.into();
        let exhausted = next_token.is_none();
        Self {
            next_cursor: next_token.map(|token| SearchCursor {
                revision: revision.clone(),
                token,
            }),
            revision,
            results,
            exhausted,
            stale_cursor: false,
            budget_exhausted: !exhausted,
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
    /// How far the hit is from the person; see [`Availability`].
    fn availability(&self) -> Availability {
        Availability::OnDevice
    }
}
/// One contiguous run of same-kind results, as [`group_by_kind`] arranges
/// them.
#[derive(Debug)]
pub struct KindGroup<Row> {
    pub kind: ResourceKind,
    /// The rows to render — at most the cap the caller asked for.
    pub results: Vec<Row>,
    /// How many results this kind actually had, before the cap.
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
    narrows_to_active_app: bool,
) -> bool {
    match scope {
        SearchScope::All | SearchScope::Workspace => true,
        SearchScope::ActiveApp => {
            active_provider_id.is_none_or(|id| id == provider_id || narrows_to_active_app)
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

/// An app's [`application_fields`] plus what else people call it: each of
/// its `keywords` (aliases, synonyms, the products it stands in for, in
/// every language its manifest gives), its names in other languages as the
/// name itself, and the initials of its name (`wp` for `Word Processor`).
#[must_use]
pub fn application_search_fields<'a>(
    name: &'a str,
    package_id: &'a str,
    category: Option<&'a str>,
    description: Option<&'a str>,
    other_names: impl IntoIterator<Item = &'a str>,
    keywords: impl IntoIterator<Item = &'a str>,
) -> Vec<(String, super::FieldWeight)> {
    use super::FieldWeight;
    let mut fields: Vec<(String, FieldWeight)> =
        application_fields(name, package_id, category, description)
            .into_iter()
            .map(|(text, weight)| (text.to_owned(), weight))
            .collect();
    for other in other_names {
        fields.push((other.to_owned(), FieldWeight::TITLE));
        fields.extend(super::initials(other).map(|initials| (initials, FieldWeight::KEYWORD)));
    }
    fields.extend(super::initials(name).map(|initials| (initials, FieldWeight::KEYWORD)));
    fields.extend(
        keywords
            .into_iter()
            .filter(|keyword| !keyword.trim().is_empty())
            .map(|keyword| (keyword.to_owned(), FieldWeight::KEYWORD)),
    );
    fields
}

/// What a tier earns in an app's fields: the usual ranks, except that an
/// app's keywords are its other names (Finder and Explorer are Files, Task
/// Manager is System Monitor), so a keyword typed whole or begun ranks like
/// the start of its name. "explo" is already Files, ahead of every file
/// that merely starts with it; "apps" is the Store's, whatever other app has
/// the word in its name.
#[must_use]
pub fn application_ranks(tier: super::MatchTier, weight: super::FieldWeight) -> Option<i32> {
    use super::MatchTier;
    if weight == super::FieldWeight::KEYWORD {
        match tier {
            // Typed whole: the app's other name, as its name's start.
            MatchTier::Exact | MatchTier::PrefixWord => {
                return MatchTier::Prefix.rank_in(super::FieldWeight::TITLE);
            }
            // Begun: a step below, so "memo", Notes' whole word, leads
            // Monitor's "memory" begun.
            MatchTier::Prefix => return MatchTier::Word.rank_in(super::FieldWeight::TITLE),
            _ => {}
        }
    }
    tier.rank_in(weight)
}

/// How much more an application hit is worth at the merge than its field
/// rank says, because a person who types what an app is called, or a word it
/// is known by, wants to open it: a perfect name match is launch intent; an
/// exact alias or keyword ("youtube" for a video app) is the same intent
/// reached by another word and must still lead the notes and files that
/// merely contain it; a keyword or name that only begins or contains the
/// query is weaker and ranks by its score alone.
#[must_use]
pub fn application_intent_boost(score: i32, top: i32) -> f32 {
    let top = top.max(1);
    if score >= top {
        // A name, or a word the app is known by, typed whole.
        4.5
    } else if score.saturating_mul(100) >= top.saturating_mul(90) {
        3.0
    } else if score.saturating_mul(100) >= top.saturating_mul(70) {
        1.5
    } else if score > 0 && score <= provider_score(super::MatchTier::Spanning.percent()) {
        // Only a typo or loosely spread words matched. Still a person
        // reaching for an app by a half-remembered name, so a launch row
        // leads an action that is just as loosely matched.
        0.4
    } else {
        0.0
    }
}

/// Product-owned ordering for the empty Applications palette. Search queries
/// continue to rank purely by match quality; this only prevents a fresh system
/// from presenting forty alphabetised tools as though they were equally likely
/// starting points. The first group is deliberately useful to curious
/// tinkerers without assuming they are developers. Higher first.
#[must_use]
pub fn starting_point_rank(package_id: &str) -> i32 {
    // A local build publishes the same apps under `dev.tilo.`.
    let canonical = package_id
        .strip_prefix("dev.tilo.")
        .map(|name| format!("so.tilo.{name}"));
    match canonical.as_deref().unwrap_or(package_id) {
        "so.tilo.browser" => 90,
        "so.tilo.file-explorer" => 89,
        "so.tilo.notes" => 88,
        "so.tilo.terminal" => 87,
        "so.tilo.settings" => 86,
        "so.tilo.store" => 85,

        "so.tilo.calendar" => 74,
        "so.tilo.mail" => 73,
        "so.tilo.contacts" => 72,
        "so.tilo.chat" => 71,
        "so.tilo.calculator" => 70,

        "so.tilo.word-processor" => 59,
        "so.tilo.sheets" => 58,
        "so.tilo.slides" => 57,
        "so.tilo.pdf-viewer" => 56,
        "so.tilo.typst-editor" => 55,

        "so.tilo.music" => 49,
        "so.tilo.tube" => 48,
        "so.tilo.video-player" => 47,
        "so.tilo.photos" => 46,
        "so.tilo.camera" => 45,
        "so.tilo.weather" => 44,
        "so.tilo.maps" => 43,
        "so.tilo.feeds" => 42,
        "so.tilo.ebook" => 41,
        "so.tilo.totp" => 40,

        "so.tilo.monitor" | "so.tilo.disks" | "so.tilo.logs" | "so.tilo.firewall"
        | "so.tilo.remote" | "so.tilo.backups" | "so.tilo.imager" | "so.tilo.themes" => 25,
        "so.tilo.api-tester"
        | "so.tilo.snippets"
        | "so.tilo.diffmerge"
        | "so.tilo.db-client"
        | "so.tilo.containers"
        | "so.tilo.hex-editor"
        | "so.tilo.git-client"
        | "so.tilo.code-editor" => 5,
        _ => 10,
    }
}

/// Top of every provider's ranker-derived score.
pub const MAX_PROVIDER_SCORE: i32 = 100;

/// One searchable field and its weight, as the shared ranker takes it.
pub type RankField<'a> = (&'a str, super::FieldWeight);

/// A field rank on the provider scale. A title prefix is already a perfect
/// hit for a provider — it is what a person types to launch or open
/// something — so the scale tops out there; [`lexical_strength`] still
/// separates an exact title from a prefix across providers.
#[must_use]
pub fn provider_score(rank: i32) -> i32 {
    // A full-weight field ranks a tier at its whole percentage.
    let perfect = super::MatchTier::Prefix.percent();
    rank.clamp(0, perfect)
        .saturating_mul(MAX_PROVIDER_SCORE)
        .checked_div(perfect)
        .unwrap_or(MAX_PROVIDER_SCORE)
}

/// The shared ranker's rank of `fields` for `needle`, on the provider scale.
pub fn rank_fields(needle: &str, fields: &[RankField<'_>]) -> Option<i32> {
    super::ListIndex::default()
        .ranks(needle, [fields.iter().copied()])
        .into_iter()
        .next()
        .flatten()
        .map(provider_score)
}

/// Provider-independent lexical fit used at the global merge boundary. A
/// provider still contributes domain knowledge through its normalized score,
/// but an exact visible title must beat a weak metadata/fuzzy match from a
/// different source.
pub fn lexical_strength(
    needle: &str,
    kind: &ResourceKind,
    title: &str,
    subtitle: Option<&str>,
) -> f32 {
    use super::{MatchTier, fold_search_text, rank::match_tier};
    let needle = fold_search_text(needle.trim());
    if needle.is_empty() {
        return 0.0;
    }
    let title = fold_search_text(title);
    // Application providers decorate the app name with the launch verb.
    // Compare its name on the same basis as a file or note title.
    let title = if matches!(kind, ResourceKind::Application) {
        title.strip_prefix("open ").unwrap_or(&title)
    } else {
        &title
    };
    match match_tier(&needle, title) {
        Some(MatchTier::Exact) => 4.0,
        Some(MatchTier::Prefix | MatchTier::PrefixWord) => 2.5,
        Some(MatchTier::WordPrefix | MatchTier::Word) => 1.5,
        Some(MatchTier::Contains) => 1.0,
        Some(MatchTier::Spanning | MatchTier::Typo) => 0.5,
        None => subtitle.map_or(0.0, |subtitle| {
            match match_tier(&needle, &fold_search_text(subtitle)) {
                Some(MatchTier::Exact) => 0.7,
                Some(_) => 0.4,
                None => 0.0,
            }
        }),
    }
}

/// A deliberately small contextual preference: proximity should settle close
/// matches, never make a weak metadata hit beat an exact visible title.
pub const ACTIVE_APP_PROXIMITY_BOOST: f32 = 0.25;

/// One hit's strength against every other provider's: its lexical fit, its
/// provider's normalized score, launch intent for an application match
/// ([`application_intent_boost`]), and the caller's contextual `proximity` boost.
#[allow(
    clippy::cast_precision_loss,
    reason = "provider score scales are small integers; exact precision isn't needed for a 0..1 strength ratio"
)]
pub fn merged_strength(
    needle: &str,
    kind: &ResourceKind,
    title: &str,
    subtitle: Option<&str>,
    score: i32,
    top: i32,
    proximity: f32,
) -> f32 {
    let top = top.max(1);
    // Launch intent is a reading of what was typed; an empty query has none.
    let application_intent =
        if !needle.trim().is_empty() && matches!(kind, ResourceKind::Application) {
            application_intent_boost(score, top)
        } else {
            0.0
        };
    lexical_strength(needle, kind, title, subtitle)
        + score as f32 / top as f32
        + application_intent
        + proximity
}

/// Order merged hits: groups by how well their best member matched, then
/// the fixed kind order, then strength within a group. Enter activates the
/// top row, so an app's name must not open an action that merely mentions
/// the word. The sort is stable: equally strong hits keep their provider's
/// own order rather than being re-alphabetised by title.
pub fn order_by_kind<T>(merged: &mut [(T, f32)], kind: impl Fn(&T) -> &ResourceKind) {
    order_hits(merged, |hit| (kind(hit), Availability::OnDevice));
}

/// [`order_by_kind`] within each [`Availability::band`]: every hit the
/// person can open now, ordered as [`order_by_kind`] orders them, then the
/// apps they would have to install first, ordered the same way among
/// themselves. Hits of one kind in different bands are separate groups.
pub fn order_hits<T>(merged: &mut [(T, f32)], class: impl Fn(&T) -> (&ResourceKind, Availability)) {
    let key = |hit: &T| {
        let (kind, availability) = class(hit);
        (availability.band(), kind.sort_key())
    };
    let mut best: HashMap<(u8, (u8, String)), f32> = HashMap::new();
    for (hit, strength) in merged.iter() {
        let slot = best.entry(key(hit)).or_insert(f32::MIN);
        *slot = slot.max(*strength);
    }
    merged.compact_sort_by(|(a, a_strength), (b, b_strength)| {
        let (a_key, b_key) = (key(a), key(b));
        let a_group = best.get(&a_key).copied().unwrap_or(f32::MIN);
        let b_group = best.get(&b_key).copied().unwrap_or(f32::MIN);
        // What the person has leads what they would have to install.
        a_key
            .0
            .cmp(&b_key.0)
            .then_with(|| b_group.partial_cmp(&a_group).unwrap_or(Ordering::Equal))
            // Equally strong groups fall back to the fixed kind order.
            .then_with(|| a_key.1.cmp(&b_key.1))
            .then_with(|| {
                b_strength
                    .partial_cmp(a_strength)
                    .unwrap_or(Ordering::Equal)
            })
    });
}

/// Rows from providers with their declared top score, ordered by their
/// normalized score on [`order_by_kind`].
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
    order_hits(&mut merged, |row| (row.kind(), row.availability()));
    merged.into_iter().map(|(r, _)| r).collect()
}

pub fn group<Row: SearchRow>(ranked: Vec<Row>, row_budget: usize) -> Vec<KindGroup<Row>> {
    group_by_kind(ranked, SearchRow::kind, row_budget)
}

/// Ranked hits arranged into contiguous per-kind groups and trimmed to a
/// `row_budget` shared round-robin ([`trim_groups`]): a palette paints ~10
/// rows, and an outscoring kind (forty file hits) can otherwise push a
/// correctly-ranked app off the bottom. A lone group is exempt — narrowing
/// to one kind means "show me all of these". A zero budget keeps everything.
pub fn group_by_kind<T>(
    ranked: Vec<T>,
    kind: impl Fn(&T) -> &ResourceKind,
    row_budget: usize,
) -> Vec<KindGroup<T>> {
    let mut groups: Vec<KindGroup<T>> = Vec::new();
    for hit in ranked {
        match groups.last_mut() {
            Some(group) if group.kind == *kind(&hit) => {
                group.total = group.total.saturating_add(1);
                group.results.push(hit);
            }
            _ => groups.push(KindGroup {
                kind: kind(&hit).clone(),
                total: 1,
                results: vec![hit],
            }),
        }
    }
    trim_groups(groups, row_budget, &[])
}

/// Keep each group to a fair share of `row_budget` rows, front to back,
/// except the kinds in `expanded`, which keep every row. A group that lost
/// rows keeps its `total`, so its header and its [`show_all_entry`] can say
/// how many there are. A lone group, or a zero budget, keeps everything.
pub fn trim_groups<T>(
    mut groups: Vec<KindGroup<T>>,
    row_budget: usize,
    expanded: &[ResourceKind],
) -> Vec<KindGroup<T>> {
    let rows: usize = groups.iter().map(|g| g.results.len()).sum();
    if row_budget == 0 || groups.len() < 2 || rows <= row_budget {
        return groups;
    }
    // Fair share, front to back: each group takes an equal cut of whatever
    // budget remains, and what it can't fill rolls forward (never backward,
    // or later headers push further down the card).
    let n = groups.len();
    let mut remaining = row_budget;
    for (i, group) in groups.iter_mut().enumerate() {
        if expanded.contains(&group.kind) {
            remaining = remaining.saturating_sub(group.results.len());
            continue;
        }
        let groups_left = n.saturating_sub(i).max(1);
        let share = remaining.checked_div(groups_left).unwrap_or(1).max(1);
        let keep = share.min(group.results.len());
        group.results.truncate(keep);
        remaining = remaining.saturating_sub(keep);
    }
    groups
}

impl<Row> KindGroup<Row> {
    /// Whether the budget left out some of this group's rows, which a
    /// [`show_all_entry`] under it then offers.
    #[must_use]
    pub fn is_capped(&self) -> bool {
        self.results.len() < self.total
    }
}

/// The row under a group [`trim_groups`] cut short that lists the rest of
/// it in place: "Show all 59". Its host handles choosing it by `id`, by
/// expanding the group's kind. Without it a palette says "Applications 59"
/// and offers four, and the rest are only reachable by already knowing
/// their names.
#[must_use]
pub fn show_all_entry(id: impl Into<String>, group: &str, total: usize) -> CommandEntry {
    CommandEntry::new(
        id,
        crate::i18n::widget_format("search-show-all", &[("count", total.into())]),
    )
    .icon(crate::icons::Icon::ChevronDown)
    .group(group)
    .group_total(total)
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
        K::Application => Icon::LayoutGrid,
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
        K::Tab | K::WindowGroup | K::Custom(_) => Icon::Search,
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
        MAX_PROVIDER_SCORE
    }
    /// Whether this provider keeps to the focused app under
    /// [`SearchScope::ActiveApp`], answering only with that app's own rows.
    /// A search scoped to an app shows that app and nothing else: system
    /// actions and other apps come back only when the scope is cleared.
    fn narrows_to_active_app(&self) -> bool {
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
        kinds.compact_sort_by_key(ResourceKind::sort_key);
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
                provider.narrows_to_active_app(),
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
            provider.narrows_to_active_app(),
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
