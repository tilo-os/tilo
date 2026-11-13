//! Native and browser system-search policy. Activation payloads remain host-owned.
use crate::CommandEntry;
use std::cmp::Ordering;
use std::collections::HashMap;
use tilo_sort::CompactSort as _;
pub mod filters;
#[cfg(test)]
mod tests;
pub const PALETTE_ROW_BUDGET: usize = 8;
use crate::search::Scope;

/// One well-known category a result can belong to — the domain vocabulary the chip strip
/// and grouping use.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum ResourceKind {
    Action,
    /// An app the person has: a Tilo app, a Flatpak, a downloaded program or one of the
    /// system's.
    Application,
    Note,
    Record,
    File,
    Folder,
    Creator,
    Video,
    Playlist,
    Tab,
    WindowGroup,
    Custom(String),
}

impl ResourceKind {
    /// Stable English name, independent of the desktop's language: the source of the chip
    /// strip's scriptable `palette:filter:*` address and the control socket's `kind`
    /// field.
    pub fn id(&self) -> &str {
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

    /// The kind's own label, as [`Self::id`], for scripted addresses and comparisons.
    pub fn label(&self) -> &str {
        self.id()
    }

    /// What the group header and the chip strip say this kind is, in the desktop's
    /// language.
    #[must_use]
    pub fn display_label(&self) -> String {
        let key = match self {
            ResourceKind::Action => "search-kind-actions",
            ResourceKind::Application => "search-kind-apps",
            ResourceKind::Note => "search-kind-notes",
            ResourceKind::Record => "search-kind-records",
            ResourceKind::File => "search-kind-files",
            ResourceKind::Folder => "search-kind-folders",
            ResourceKind::Creator => "search-kind-creators",
            ResourceKind::Video => "search-kind-videos",
            ResourceKind::Playlist => "search-kind-playlists",
            ResourceKind::Tab => "search-kind-tabs",
            ResourceKind::WindowGroup => "search-kind-window-groups",
            ResourceKind::Custom(s) => return s.clone(),
        };
        crate::i18n::widget_format(key, &[])
    }

    /// Stable sort key pinning the well-known kinds to a fixed palette order.
    /// Applications rank above content kinds because Enter activates the top row — a
    /// typed app name must launch the app, not surface a source file that merely contains
    /// the word.
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

/// How far a hit is from the person: what they have, an app Tilo opens at once, or an app
/// they would first have to install.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum Availability {
    #[default]
    OnDevice,
    TiloCatalogue,
    ThirdPartyCatalogue,
}

impl Availability {
    /// The band the merge orders the hit in, before match quality: every hit the person
    /// can open now leads every app they would have to install first.
    #[must_use]
    pub const fn band(self) -> u8 {
        match self {
            Self::OnDevice => 0,
            Self::TiloCatalogue => 1,
            Self::ThirdPartyCatalogue => 2,
        }
    }
}

/// Hard ceilings for one provider page. A provider may stop sooner, but must never exceed
/// any non-zero ceiling.
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

/// An opaque provider-owned continuation. The revision is carried alongside the
/// provider-local payload, so a result page can never silently advance across a changed
/// ordering.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct SearchCursor {
    pub revision: String,
    pub token: String,
}

/// One bounded answer from a [`Provider`]. `stale_cursor` is a protocol outcome rather
/// than an empty page: callers restart explicitly, preserving the distinction between
/// "there are no more hits" and "the source changed while you were paging".
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

    /// The last page when `next_token` is `None`; otherwise a page its budget cut short,
    /// continuing at that token.
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
/// One contiguous run of same-kind results, as [`group_by_kind`] arranges them.
#[derive(Clone, Debug)]
pub struct KindGroup<Row> {
    pub kind: ResourceKind,
    /// The rows to render — at most the cap the caller asked for.
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
/// Whether a search of `scope` asks the provider `provider_id`: only a search of one app
/// narrows, to that app's own providers and the ones that keep to the app in front.
pub fn scope_allows(
    scope: &Scope,
    provider_id: &str,
    active_provider_id: Option<&str>,
    narrows_to_active_app: bool,
) -> bool {
    match scope {
        Scope::InApp(_) => {
            active_provider_id.is_none_or(|id| id == provider_id || narrows_to_active_app)
        }
        Scope::WindowGroup(_) | Scope::Workspace | Scope::Everywhere => true,
    }
}
/// The package id in full names exactly one app, so an exact id is as strong as the name;
/// a shared id prefix names a publisher and stays below an exact name.
pub const PACKAGE_ID_WEIGHT: super::FieldWeight = super::FieldWeight::new(88);

/// An app's searchable fields: its name, the words it is known by and what kind of app it
/// is. The package id in full, or its last segment, identifies exactly one app and weighs
/// as its name — `hex-editor` typed for `so.tilo.hex-editor` is the app's identity and
/// must outrank an incidental substring.
#[must_use]
pub fn application_fields<'a>(
    name: &'a str,
    package_id: &'a str,
    category: Option<&'a str>,
) -> [(&'a str, super::FieldWeight); 4] {
    use super::FieldWeight;
    let last_segment = package_id.rsplit('.').next().unwrap_or(package_id);
    [
        (name, FieldWeight::TITLE),
        (last_segment, FieldWeight::TITLE),
        (package_id, PACKAGE_ID_WEIGHT),
        (category.unwrap_or_default(), FieldWeight::METADATA),
    ]
}

/// What an app-published action is found by, as every search ranks it: its label names
/// it, the other words its declaration gives name it as people say it ("mkdir" for New
/// folder), and the resource it acts on narrows it.
#[must_use]
pub fn action_search_fields<'a>(
    action_label: &'a str,
    keywords: &'a [String],
    resource_label: &'a str,
) -> Vec<RankField<'a>> {
    use super::FieldWeight;
    let mut fields = vec![
        (action_label, FieldWeight::TITLE),
        (resource_label, FieldWeight::SUBTITLE),
    ];
    fields.extend(
        keywords
            .iter()
            .map(|word| (word.as_str(), FieldWeight::KEYWORD)),
    );
    fields
}

/// Words a query is never required to match: possessives and filler ("my mails", "all the
/// notes", "videos of <channel>").
pub const FILLER_WORDS: &[&str] = &["my", "mine", "our", "the", "all", "of"];

/// `needle` without its [`FILLER_WORDS`]: what every source is asked.
#[must_use]
pub fn without_filler(needle: &str) -> std::borrow::Cow<'_, str> {
    let filler = |word: &str| {
        let folded = super::fold_search_text(word);
        FILLER_WORDS.contains(&folded.as_str())
    };
    let words: Vec<&str> = needle.split_whitespace().collect();
    if !words.iter().any(|word| filler(word)) || words.iter().all(|word| filler(word)) {
        return std::borrow::Cow::Borrowed(needle);
    }
    std::borrow::Cow::Owned(
        words
            .into_iter()
            .filter(|word| !filler(word))
            .collect::<Vec<_>>()
            .join(" "),
    )
}

/// What a query naming a resource's type rather than its title scores on the provider
/// scale, by how it named it and whether the resource is its app's root.
const TYPE_MATCH_SCORE: i32 = 20;

const TYPE_NOUN_BONUS: i32 = 1;

const ROOT_BONUS: i32 = 3;

const TYPE_AND_TEXT_BONUS: i32 = 5;

fn named_at(words: &[String], name: &str, out: &mut Vec<usize>) -> bool {
    let name: Vec<String> = super::fold_search_text(name)
        .split_whitespace()
        .map(str::to_owned)
        .collect();
    if name.is_empty() || name.len() > words.len() {
        return false;
    }
    let mut found = false;
    for start in 0..=words.len().saturating_sub(name.len()) {
        if words.get(start..start.saturating_add(name.len())) == Some(&name[..]) {
            out.extend(start..start.saturating_add(name.len()));
            found = true;
        }
    }
    found
}

/// The words of `needle` that name what a resource is rather than what it is called: the
/// ones a [`type_match_score_with`] over `ty` (with its kindred types' nouns, `kin`, and
/// its app's names, `keywords`) leaves out. The rest is the text the resource itself must
/// match — and what an outside origin is asked for what the query names (search.md §4,
/// §7).
#[must_use]
pub fn type_words_rest<'k>(
    needle: &str,
    ty: &tilo_actions::ResourceType,
    kin: impl IntoIterator<Item = &'k tilo_actions::Noun>,
    keywords: &[&str],
) -> Option<String> {
    let words = split_words(needle);
    let kin: Vec<&tilo_actions::Noun> = kin.into_iter().collect();
    let covered = type_coverage(&words, ty, &kin, keywords)?;
    let rest: Vec<&str> = words
        .iter()
        .enumerate()
        .filter(|(at, _)| !covered.contains(at))
        .map(|(_, word)| word.as_str())
        .collect();
    Some(rest.join(" "))
}

/// Whether `needle` names `ty` by its own noun rather than only by its app's name or a
/// kindred type's noun: "videos" names a `Video` so, while "videos of <channel>" names a
/// `Channel` only through its kindred type.
#[must_use]
pub fn named_by_own_noun(needle: &str, ty: &tilo_actions::ResourceType) -> bool {
    let words = split_words(needle);
    let mut at = Vec::new();
    named_at(&words, &ty.noun.one, &mut at) | named_at(&words, &ty.noun.many, &mut at)
}

fn split_words(needle: &str) -> Vec<String> {
    super::fold_search_text(needle)
        .split_whitespace()
        .map(str::to_owned)
        .collect()
}

fn type_coverage(
    words: &[String],
    ty: &tilo_actions::ResourceType,
    kin: &[&tilo_actions::Noun],
    keywords: &[&str],
) -> Option<Vec<usize>> {
    let type_names: Vec<String> = std::iter::once(ty.noun.clone())
        .chain(kin.iter().map(|noun| (*noun).clone()))
        .flat_map(|noun| [noun.one, noun.many])
        .map(|name| super::fold_search_text(&name))
        .collect();
    let app_names: Vec<String> = [&ty.app.one, &ty.app.many]
        .into_iter()
        .map(|name| super::fold_search_text(name))
        .chain(
            keywords
                .iter()
                .map(|keyword| super::fold_search_text(keyword))
                .filter(|keyword| !type_names.contains(keyword)),
        )
        .collect();
    let mut own = Vec::new();
    for name in [&ty.noun.one, &ty.noun.many] {
        named_at(words, name, &mut own);
    }
    for name in &app_names {
        named_at(words, name, &mut own);
    }
    let mut covered = own;
    let title: Vec<String> = [&ty.app.one, &ty.app.many]
        .into_iter()
        .map(|name| super::fold_search_text(name))
        .collect();
    for noun in kin {
        for name in [&noun.one, &noun.many] {
            if !title.contains(&super::fold_search_text(name)) {
                named_at(words, name, &mut covered);
            }
        }
    }
    (!covered.is_empty()).then_some(covered)
}

/// How well `needle` names a resource by what it is ([`tilo_actions::ResourceType`]): a
/// word naming its type's noun or plural, or its app's name, matches it whatever it is
/// called, and the rest of the query must match its text, which `text` ranks (on the
/// provider scale).
pub fn type_match_score<'k>(
    needle: &str,
    ty: &tilo_actions::ResourceType,
    kin: impl IntoIterator<Item = &'k tilo_actions::Noun>,
    text: impl FnOnce(&str) -> Option<i32>,
) -> Option<i32> {
    type_match_score_with(needle, ty, kin, &[], text)
}

/// [`type_match_score`], with the words its app is known by besides its title
/// (`keywords`: the registry's "youtube", "channel", "subscription" for Tube).
pub fn type_match_score_with<'k>(
    needle: &str,
    ty: &tilo_actions::ResourceType,
    kin: impl IntoIterator<Item = &'k tilo_actions::Noun>,
    keywords: &[&str],
    text: impl FnOnce(&str) -> Option<i32>,
) -> Option<i32> {
    let words: Vec<String> = super::fold_search_text(needle)
        .split_whitespace()
        .map(str::to_owned)
        .collect();
    let kin: Vec<&tilo_actions::Noun> = kin.into_iter().collect();
    // A word that is one of the app's types' nouns names that type, not the app: the
    // registry's "videos" for Tube is its `Video` type's plural, so "youtube videos" are
    // the client's videos, never every thing of it.
    let type_names: Vec<String> = std::iter::once(ty.noun.clone())
        .chain(kin.iter().map(|noun| (*noun).clone()))
        .flat_map(|noun| [noun.one, noun.many])
        .map(|name| super::fold_search_text(&name))
        .collect();
    let app_names: Vec<String> = [&ty.app.one, &ty.app.many]
        .into_iter()
        .map(|name| super::fold_search_text(name))
        .chain(
            keywords
                .iter()
                .map(|keyword| super::fold_search_text(keyword))
                .filter(|keyword| !type_names.contains(keyword)),
        )
        .collect();
    let mut own = Vec::new();
    let mut by_noun = false;
    for name in [&ty.noun.one, &ty.noun.many] {
        by_noun |= named_at(&words, name, &mut own);
    }
    for name in &app_names {
        named_at(&words, name, &mut own);
    }
    let mut covered = own.clone();
    let mut kin_named = false;
    let title: Vec<String> = [&ty.app.one, &ty.app.many]
        .into_iter()
        .map(|name| super::fold_search_text(name))
        .collect();
    for noun in kin {
        for name in [&noun.one, &noun.many] {
            if !title.contains(&super::fold_search_text(name)) {
                kin_named |= named_at(&words, name, &mut covered);
            }
        }
    }
    if covered.is_empty() {
        return None;
    }
    let rest: Vec<&str> = words
        .iter()
        .enumerate()
        .filter(|(at, _)| !covered.contains(at))
        .map(|(_, word)| word.as_str())
        .collect();
    if rest.is_empty() {
        if own.is_empty() || (kin_named && !by_noun) {
            return None;
        }
        return Some(
            TYPE_MATCH_SCORE
                .saturating_add(if ty.root { ROOT_BONUS } else { 0 })
                .saturating_add(if by_noun { TYPE_NOUN_BONUS } else { 0 }),
        );
    }
    let matched = text(&rest.join(" "))?;
    Some(
        matched
            .saturating_add(TYPE_AND_TEXT_BONUS)
            .min(MAX_PROVIDER_SCORE),
    )
}

/// How well `needle` names an action by its fields, on [`action_ranks`]: [`rank_fields`]
/// for an action.
#[must_use]
pub fn rank_action_fields(needle: &str, fields: &[RankField<'_>]) -> Option<i32> {
    rank_actions_fields(needle, [fields.iter().copied()])
        .into_iter()
        .next()
        .flatten()
}

/// [`rank_action_fields`] for every action of a list at once, row for row: the list is
/// indexed once, so ranking a session's hundreds of published actions costs one index,
/// not one per action.
pub fn rank_actions_fields<'a, R>(
    needle: &str,
    rows: impl IntoIterator<Item = R>,
) -> Vec<Option<i32>>
where
    R: IntoIterator<Item = RankField<'a>>,
{
    super::ListIndex::default()
        .with_ranks(action_ranks)
        .ranks(needle, rows)
        .into_iter()
        .map(|rank| rank.map(provider_score))
        .collect()
}

/// The actions of `actions` a search for `query` finds, best first, on
/// [`action_search_fields`]: what an app's test asserts to pin the words people use for
/// its actions ("mkdir") to the action they mean.
#[must_use]
pub fn actions_found_by<'a>(
    query: &str,
    actions: &'a [tilo_actions::ActionSpec],
) -> Vec<&'a tilo_actions::ActionSpec> {
    let mut found: Vec<(i32, &tilo_actions::ActionSpec)> = actions
        .iter()
        .filter_map(|action| {
            let score = rank_action_fields(
                query,
                &action_search_fields(&action.label, &action.keywords, ""),
            )?;
            Some((score, action))
        })
        .collect();
    found.sort_by_key(|(score, _)| std::cmp::Reverse(*score));
    found.into_iter().map(|(_, action)| action).collect()
}

/// An app's [`application_fields`] plus what else people call it: each of its `keywords`
/// (aliases, synonyms, the products it stands in for, in every language its manifest
/// gives), its names in other languages as the name itself, and the initials of its name
/// (`wp` for `Word Processor`).
#[must_use]
pub fn application_search_fields<'a>(
    name: &'a str,
    package_id: &'a str,
    category: Option<&'a str>,
    other_names: impl IntoIterator<Item = &'a str>,
    keywords: impl IntoIterator<Item = &'a str>,
) -> Vec<(String, super::FieldWeight)> {
    use super::FieldWeight;
    let mut fields: Vec<(String, FieldWeight)> = application_fields(name, package_id, category)
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

/// What a tier earns in an app's fields: an app is found by the words that name it begun,
/// never by a word that merely holds them.
#[must_use]
pub fn application_ranks(tier: super::MatchTier, weight: super::FieldWeight) -> Option<i32> {
    use super::MatchTier;
    if matches!(
        tier,
        MatchTier::Typo | MatchTier::Spanning | MatchTier::Contains
    ) {
        return None;
    }
    if weight == super::FieldWeight::KEYWORD {
        match tier {
            MatchTier::Exact | MatchTier::PrefixWord => {
                return MatchTier::Prefix.rank_in(super::FieldWeight::TITLE);
            }
            MatchTier::Prefix => return MatchTier::Word.rank_in(super::FieldWeight::TITLE),
            _ => {}
        }
    }
    tier.rank_in(weight)
}

/// What a tier earns in an action's fields: the usual ranks, except that an action's
/// keywords are the other words people use for it ("power off" for Shut down, "mkdir" for
/// New folder), so a keyword typed whole ranks as the start of the action's title, as an
/// app's other names rank as its name ([`application_ranks`]).
#[must_use]
pub fn action_ranks(tier: super::MatchTier, weight: super::FieldWeight) -> Option<i32> {
    // An action is found by a word of it begun, never by letters a longer word hides it
    // in: "shut" is not the incomplete items a command's sentence happens to hold.
    if tier == super::MatchTier::Contains {
        return None;
    }
    if weight == super::FieldWeight::KEYWORD && tier == super::MatchTier::Exact {
        return super::MatchTier::Prefix.rank_in(super::FieldWeight::TITLE);
    }
    tier.rank_in(weight)
}

/// How much more an application hit is worth at the merge than its field rank says,
/// because a person who types what an app is called, or a word it is known by, wants to
/// open it: a perfect name match is launch intent; an exact alias or keyword ("youtube"
/// for a video app) is the same intent reached by another word and must still lead the
/// notes and files that merely contain it; a keyword or name that only begins or contains
/// the query is weaker and ranks by its score alone.
#[must_use]
pub fn application_intent_boost(score: i32, top: i32) -> f32 {
    let top = top.max(1);
    if score >= top {
        4.5
    } else if score.saturating_mul(100) >= top.saturating_mul(90) {
        3.0
    } else if score.saturating_mul(100) >= top.saturating_mul(70) {
        1.5
    } else {
        0.0
    }
}

/// Product-owned ordering for the empty Applications palette.
#[must_use]
pub fn starting_point_rank(package_id: &str) -> i32 {
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

/// A field rank on the provider scale.
#[must_use]
pub fn provider_score(rank: i32) -> i32 {
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

/// Provider-independent lexical fit used at the global merge boundary. A provider still
/// contributes domain knowledge through its normalized score, but an exact visible title
/// must beat a weak metadata/fuzzy match from a different source.
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
    let title = if matches!(kind, ResourceKind::Application) {
        title.strip_prefix("open ").unwrap_or(&title)
    } else {
        &title
    };
    match match_tier(&needle, title) {
        Some(MatchTier::Exact) => 4.0,
        Some(MatchTier::Prefix | MatchTier::PrefixWord) => TITLE_BEGUN_FIT,
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

/// A deliberately small contextual preference: proximity should settle close matches,
/// never make a weak metadata hit beat an exact visible title.
pub const ACTIVE_APP_PROXIMITY_BOOST: f32 = 0.25;

/// One hit's strength against every other provider's: its lexical fit, its provider's
/// normalized score, launch intent for an application match
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
    let application_intent =
        if !needle.trim().is_empty() && matches!(kind, ResourceKind::Application) {
            application_intent_boost(score, top)
        } else {
            0.0
        };
    let lexical = lexical_strength(needle, kind, title, subtitle);
    // An action's provider gives its top score only to its title begun or to a word it is
    // known by typed whole ([`action_ranks`]): "power off" names Shut down as well as
    // "shut" does, though its title says neither.
    let lexical =
        if score >= top && !needle.trim().is_empty() && matches!(kind, ResourceKind::Action) {
            lexical.max(TITLE_BEGUN_FIT)
        } else {
            lexical
        };
    lexical + score as f32 / top as f32 + application_intent + proximity
}

const TITLE_BEGUN_FIT: f32 = 2.5;

/// Order merged hits: groups by how well their best member matched, then the fixed kind
/// order, then strength within a group. Enter activates the top row, so an app's name
/// must not open an action that merely mentions the word.
pub fn order_by_kind<T>(merged: &mut [(T, f32)], kind: impl Fn(&T) -> &ResourceKind) {
    order_hits(merged, |hit| (kind(hit), Availability::OnDevice));
}

/// [`order_by_kind`] within each [`Availability::band`]: every hit the person can open
/// now, ordered as [`order_by_kind`] orders them, then the apps they would have to
/// install first, ordered the same way among themselves.
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
        a_key
            .0
            .cmp(&b_key.0)
            .then_with(|| b_group.partial_cmp(&a_group).unwrap_or(Ordering::Equal))
            .then_with(|| a_key.1.cmp(&b_key.1))
            .then_with(|| {
                b_strength
                    .partial_cmp(a_strength)
                    .unwrap_or(Ordering::Equal)
            })
    });
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
    order_hits(&mut merged, |row| (row.kind(), row.availability()));
    merged.into_iter().map(|(r, _)| r).collect()
}

pub fn group<Row: SearchRow>(ranked: Vec<Row>, row_budget: usize) -> Vec<KindGroup<Row>> {
    group_by_kind(ranked, SearchRow::kind, row_budget)
}

/// Ranked hits arranged into contiguous per-kind groups and trimmed to a `row_budget`
/// shared round-robin ([`trim_groups`]): a palette paints ~10 rows, and an outscoring
/// kind (forty file hits) can otherwise push a correctly-ranked app off the bottom.
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

/// Keep each group to a fair share of `row_budget` rows, round-robin from the front,
/// except the kinds in `expanded`, which keep every row.
pub fn trim_groups<T>(
    groups: Vec<KindGroup<T>>,
    row_budget: usize,
    expanded: &[ResourceKind],
) -> Vec<KindGroup<T>> {
    let rows: usize = groups.iter().map(|group| group.results.len()).sum();
    if row_budget == 0 || groups.len() < 2 || rows <= row_budget {
        return groups;
    }
    let mut budget = row_budget;
    let mut trimmed = groups;
    loop {
        trimmed = keep_each_group(trimmed, budget, expanded);
        let offered = trimmed.iter().filter(|group| group.is_capped()).count();
        let fits = row_budget.saturating_sub(offered).max(1);
        if fits >= budget || !trimmed.iter().any(KindGroup::is_capped) {
            return trimmed;
        }
        budget = fits;
    }
}

/// One row each, round-robin from the front, until `budget` rows are kept: every group is
/// seen before any group is seen twice, so a group cannot reach the top of the list and
/// starve the ones behind it.
fn keep_each_group<T>(
    mut groups: Vec<KindGroup<T>>,
    budget: usize,
    expanded: &[ResourceKind],
) -> Vec<KindGroup<T>> {
    let mut kept: Vec<usize> = groups.iter().map(|_| 0).collect();
    let mut left = budget;
    for (group, kept) in groups.iter().zip(&mut kept) {
        if expanded.contains(&group.kind) {
            *kept = group.results.len();
            left = left.saturating_sub(*kept);
        }
    }
    let rest = groups
        .iter()
        .filter(|group| !expanded.contains(&group.kind))
        .count();
    left = left.max(rest);
    while left > 0 {
        let mut took = false;
        for (index, group) in groups.iter().enumerate() {
            if left == 0 {
                break;
            }
            if expanded.contains(&group.kind) || kept[index] >= group.results.len() {
                continue;
            }
            kept[index] += 1;
            left -= 1;
            took = true;
        }
        if !took {
            break;
        }
    }
    for (group, keep) in groups.iter_mut().zip(kept) {
        group.results.truncate(keep);
    }
    groups
}

impl<Row> KindGroup<Row> {
    /// Whether the budget left out some of this group's rows, which a [`show_all_entry`]
    /// under it then offers.
    #[must_use]
    pub fn is_capped(&self) -> bool {
        self.results.len() < self.total
    }
}

/// The row under a group [`trim_groups`] cut short that lists the rest of it in place:
/// "Show all 59".
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
                .group(group.kind.display_label())
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

/// Whether a person may take `spec` from a palette row now: offered to people, available
/// in the window's state, and asking no more than a row can collect
/// ([`tilo_actions::ActionSpec::takes_a_row_answer`]). What a row cannot collect (a
/// record, a person, several arguments) is picked where it lives, by the surface that
/// owns that pick.
#[must_use]
pub fn a_row_a_person_can_take(spec: &tilo_actions::ActionSpec) -> bool {
    spec.reaches_people() && spec.contract.availability.is_available() && spec.takes_a_row_answer()
}

pub fn icon_for_kind(kind: &ResourceKind) -> crate::icons::Icon {
    use crate::icons::Icon;
    use ResourceKind as K;
    match kind {
        K::Action => Icon::Settings2,
        K::Application => Icon::LayoutGrid,
        K::Note => Icon::FileText,
        // A record is one document several workspaces can reach — the stacked plates say
        // "this is the same thing in more than one place", which is exactly what a
        // reference is.
        K::Record => Icon::Layers,
        K::File => Icon::File,
        K::Folder => Icon::Folder,
        K::Creator => Icon::Star,
        K::Video => Icon::Film,
        K::Playlist => Icon::List,
        K::Tab | K::WindowGroup | K::Custom(_) => Icon::Search,
    }
}

#[derive(Clone, Debug)]
pub struct Query<'a> {
    pub needle: &'a str,
    pub scope: &'a Scope,
    pub active_provider_id: Option<&'a str>,
    pub wanted_kinds: &'a [ResourceKind],
}
pub trait Provider<Host, Row: SearchRow> {
    fn id(&self) -> &str;
    fn resource_kinds(&self) -> &[ResourceKind];
    fn max_score(&self) -> i32 {
        MAX_PROVIDER_SCORE
    }
    /// Whether this provider keeps to the focused app under a [`Scope::InApp`] search,
    /// answering only with that app's own rows.
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
/// Registration order survives equal-strength ties.
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
                ..query.clone()
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
                    ..query.clone()
                },
            )
            .filter(|r| wanted.contains(r.kind()))
    }
}
