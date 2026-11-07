//! What an app offers to search: an entry, the weights its fields rank by
//! and the prose it is read for.

use std::sync::Arc;

use crate::search_index::{IndexKind, IndexPerson};
use crate::search_preview::SearchPreview;

use super::{SearchRow, scope_contains};

/// A search index entry. `anchor` addresses what a hit opens (an item id);
/// `action`, when set, is the published action a hit invokes, so choosing it
/// goes through the same door, permissions and history as the palette.
///
/// How a hit reads is the entry's too: the result group it is listed under
/// (the resource's label unless `group` names another, such as "Timeline
/// markers"), a dimmer `subtitle` line, a named `icon` (a lucide kebab name,
/// `"star"`), and a `boost` that floats pinned or recent items over an equal
/// match. Long prose an entry is found by — a book's chapter, a long note —
/// is its `body`, which the index reads in place rather than the entry
/// copying it, and whose segments may each be a hit of their own.
// `Clone` and `PartialEq` are written out: derived, they are inlined into
// every app that copies or compares an entry.
#[derive(Debug, Default, Eq)]
pub struct SearchEntry {
    /// The containing resource scope for a flat anchor.
    pub within: Option<String>,
    /// Presentation for the platform-rendered result row.
    pub row: Option<SearchRow>,
    pub anchor: Option<String>,
    pub title: String,
    pub text: String,
    pub action: Option<tilo_actions::ActionAddress>,
    pub group: Option<String>,
    pub subtitle: Option<String>,
    pub icon: Option<String>,
    /// What kind of thing it is, where the desk's generic rows would read
    /// poorly: an agent, a conversation, a message.
    pub kind: Option<IndexKind>,
    /// Who the row shows, in the order people read them: the first is its
    /// avatar, the rest its faces.
    pub people: Vec<IndexPerson>,
    /// Who keeps it, for an agent.
    pub keeper: Option<IndexPerson>,
    /// Where it is, for a message: the conversation it sits in.
    pub place: Option<IndexPerson>,
    /// When it happened, in seconds since the Unix epoch.
    pub when: Option<i64>,
    /// What the palette shows of the hit while a person is on it; `None`
    /// shows its `text`, and an entry with neither shows no preview.
    pub preview: Option<SearchPreview>,
    pub boost: i32,
    pub body: Option<SearchBody>,
    /// More short text the entry is found by, each ranked by its own
    /// weight: a sender's address, a book's subject, a message's labels.
    pub fields: Vec<(String, SearchWeight)>,
    /// Structured filter values: token key, localized group label, value.
    pub facets: Vec<(String, String, String)>,
    /// The entry describes a secret (a vault's login): the system index
    /// keeps its metadata but finds it only while the app reports its
    /// secrets unlocked ([`SearchEntry::secret_bearing`]).
    pub secret: bool,
}

impl Clone for SearchEntry {
    fn clone(&self) -> Self {
        Self {
            within: self.within.clone(),
            row: self.row.clone(),
            anchor: self.anchor.clone(),
            title: self.title.clone(),
            text: self.text.clone(),
            action: self.action.clone(),
            group: self.group.clone(),
            subtitle: self.subtitle.clone(),
            icon: self.icon.clone(),
            kind: self.kind,
            people: self.people.clone(),
            keeper: self.keeper.clone(),
            place: self.place.clone(),
            when: self.when,
            preview: self.preview.clone(),
            boost: self.boost,
            body: self.body.clone(),
            fields: self.fields.clone(),
            facets: self.facets.clone(),
            secret: self.secret,
        }
    }
}

impl PartialEq for SearchEntry {
    fn eq(&self, other: &Self) -> bool {
        self.within == other.within
            && self.row == other.row
            && self.anchor == other.anchor
            && self.title == other.title
            && self.text == other.text
            && self.action == other.action
            && self.group == other.group
            && self.subtitle == other.subtitle
            && self.icon == other.icon
            && self.kind == other.kind
            && self.people == other.people
            && self.keeper == other.keeper
            && self.place == other.place
            && self.when == other.when
            && self.preview == other.preview
            && self.boost == other.boost
            && self.body == other.body
            && self.fields == other.fields
            && self.facets == other.facets
            && self.secret == other.secret
    }
}

/// How much a match in one of a [`SearchEntry`]'s fields counts. The title
/// ranks as [`Self::Title`], the subtitle as [`Self::Subtitle`] and the text
/// as [`Self::Body`]; [`SearchEntry::field`] adds others.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum SearchWeight {
    /// The name a person looks for.
    Title,
    /// A stable identity typed verbatim: an address, an id, a path.
    Identity,
    /// A secondary line: an author, a sender, a folder.
    Subtitle,
    /// Other short fields: labels, tags, a subject.
    Metadata,
    /// Long prose.
    Body,
    /// Found, but never reorders hits.
    Unranked,
}

/// One run of a [`SearchText`] segment: styled text, say.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SearchPiece<'a> {
    pub text: &'a str,
    /// Whether a word break separates this piece from the text before it.
    pub space_before: bool,
}

/// Long text a search entry is found by, owned by the app: the index reads
/// it while indexing and never copies it. It is a sequence of segments (the
/// unit a hit can point at: a paragraph, a block), each a sequence of
/// pieces. It must not change once handed to an entry; changed prose is a
/// new body.
pub trait SearchText: Send + Sync {
    fn segment_count(&self) -> usize;

    fn piece_count(&self, segment: usize) -> usize;

    fn piece(&self, segment: usize, piece: usize) -> Option<SearchPiece<'_>>;

    /// Whether a match in `segment` is a hit of its own ([`Self::location`])
    /// rather than the entry's.
    fn locates(&self, segment: usize) -> bool {
        let _ = segment;
        false
    }

    /// The hit a match in `segment` is — a reading location, invoking the
    /// action that opens the text there — when [`Self::locates`] says so.
    fn location(&self, segment: usize) -> Option<SearchEntry> {
        let _ = segment;
        None
    }
}

/// A [`SearchText`] an entry carries. Cloning one is cheap, and two bodies
/// are the same body only when they share one text, so an app that rebuilds
/// its entries over mostly unchanged prose keeps its bodies and hands the
/// same ones back.
#[derive(Clone)]
pub struct SearchBody(Arc<dyn SearchText>);

impl SearchBody {
    #[must_use]
    pub fn new(text: Arc<dyn SearchText>) -> Self {
        Self(text)
    }

    #[must_use]
    pub fn text(&self) -> &dyn SearchText {
        self.0.as_ref()
    }

    /// The shared text, for an index that reads it later.
    #[must_use]
    pub fn shared(&self) -> Arc<dyn SearchText> {
        Arc::clone(&self.0)
    }
}

impl PartialEq for SearchBody {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
}

impl Eq for SearchBody {}

impl std::fmt::Debug for SearchBody {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("SearchBody")
            .field("segments", &self.0.segment_count())
            .finish()
    }
}

impl SearchEntry {
    /// Offer this entry under one value of a platform search facet.
    #[must_use]
    pub fn facet(
        mut self,
        key: impl Into<String>,
        label: impl Into<String>,
        value: impl Into<String>,
    ) -> Self {
        self.facets.push((key.into(), label.into(), value.into()));
        self
    }

    /// Whether the entry lies in `scope`: its anchor, or the scope it is
    /// [`within`](Self::within), is the scope or lies beneath it.
    #[must_use]
    pub fn in_scope(&self, scope: &str) -> bool {
        [self.within.as_deref(), self.anchor.as_deref()]
            .into_iter()
            .flatten()
            .any(|anchor| scope_contains(scope, anchor))
    }

    /// An entry titled `title` whose body text is `text`.
    pub fn new(title: impl Into<String>, text: impl Into<String>) -> Self {
        Self {
            title: title.into(),
            text: text.into(),
            ..Self::default()
        }
    }

    /// List the hit inside the scope `scope` (an item's anchor) though its
    /// own anchor lies elsewhere: an article in its feed's scope, related by
    /// a field rather than by nesting.
    #[must_use]
    pub fn inside(mut self, scope: impl Into<String>) -> Self {
        self.within = Some(scope.into());
        self
    }

    /// The item a hit opens.
    #[must_use]
    pub fn anchored(mut self, anchor: impl Into<String>) -> Self {
        self.anchor = Some(anchor.into());
        self
    }

    /// The published action a hit invokes.
    #[must_use]
    pub fn invoking(mut self, action: tilo_actions::ActionAddress) -> Self {
        self.action = Some(action);
        self
    }

    /// Anchored at the live row `row`, opened by running `effect`, the
    /// view effect that row offers (go to a bookmark, show a find match):
    /// for a hit whose row is the window's own state rather than an item
    /// of the record.
    #[must_use]
    pub fn opening<E: crate::Effect>(mut self, row: impl Into<String>, effect: &E) -> Self {
        let row = row.into();
        self.action = Some(tilo_actions::ActionAddress::new(row.clone(), effect.id()));
        self.anchor = Some(row);
        self
    }

    /// List hits under `group` rather than the resource's label.
    #[must_use]
    pub fn grouped(mut self, group: impl Into<String>) -> Self {
        self.group = Some(group.into());
        self
    }

    /// A secondary line under the title: where the item is, what it is.
    #[must_use]
    pub fn subtitled(mut self, subtitle: impl Into<String>) -> Self {
        self.subtitle = Some(subtitle.into());
        self
    }

    /// The glyph a hit leads with, by its lucide kebab name (`"star"`).
    #[must_use]
    pub fn with_icon(mut self, icon: impl Into<String>) -> Self {
        self.icon = Some(icon.into());
        self
    }

    /// What kind of thing it is, where the desk's generic rows would read
    /// poorly: an agent, a conversation, a message.
    #[must_use]
    pub fn of_kind(mut self, kind: IndexKind) -> Self {
        self.kind = Some(kind);
        self
    }

    /// Who the row shows, in the order people read them: the first is its
    /// avatar, the rest its faces.
    #[must_use]
    pub fn showing(mut self, people: Vec<IndexPerson>) -> Self {
        self.people = people;
        self
    }

    /// Who keeps it, for an agent: the row says so, and "you" when it is the
    /// person searching.
    #[must_use]
    pub fn kept_by(mut self, keeper: IndexPerson) -> Self {
        self.keeper = Some(keeper);
        self
    }

    /// Where it is, for a message: the conversation it sits in.
    #[must_use]
    pub fn in_place(mut self, place: IndexPerson) -> Self {
        self.place = Some(place);
        self
    }

    /// When it happened, in seconds since the Unix epoch.
    #[must_use]
    pub fn at(mut self, when: i64) -> Self {
        self.when = Some(when);
        self
    }

    /// What the palette shows of the hit while a person is on it, in place
    /// of its text.
    #[must_use]
    pub fn previewed(mut self, preview: SearchPreview) -> Self {
        self.preview = Some(preview);
        self
    }

    /// Rank the hit `boost` above an equal match: a nudge, never an
    /// override of how well it matches.
    #[must_use]
    pub fn boosted(mut self, boost: i32) -> Self {
        self.boost = boost;
        self
    }

    /// Find the entry by `text` too, a match in it ranked as `weight`.
    #[must_use]
    pub fn field(mut self, text: impl Into<String>, weight: SearchWeight) -> Self {
        let text = text.into();
        if !text.trim().is_empty() {
            self.fields.push((text, weight));
        }
        self
    }

    /// Find the entry by `body` too, read in place.
    #[must_use]
    pub fn with_body(mut self, body: SearchBody) -> Self {
        self.body = Some(body);
        self
    }

    /// Mark the entry as describing a secret. The app publishes metadata
    /// only (a name, a username, a site, never the secret), and the
    /// system's search finds the entry only while the app reports its
    /// secrets unlocked; while they are locked a match offers to unlock
    /// the app instead.
    #[must_use]
    pub fn secret_bearing(mut self) -> Self {
        self.secret = true;
        self
    }
}
