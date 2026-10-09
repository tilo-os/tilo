//! Pure data model for Tube. Everything here is plain serde-able data —
//! no toolkit types, no colours, no IO. The model is device-local presentation
//! state. Explicit collaboration boundaries live in `actions`: subscriptions
//! and settings are structured Automerge actions, while fetched catalogues,
//! playback and navigation stay local. Watch history is a private, per-user
//! CRDT projection: it syncs between that user's devices but is not a shared
//! collaborator feed.
//!
//! Colours are never stored: thumbnails derive a deterministic gradient
//! from a seed string at draw time (`util::seed_gradient`) so the model
//! stays theme-agnostic and the OS keeps control of the palette.

use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

pub type VideoId = String;
pub type ChannelId = String;

/// Normalize a channel display name for cross-source matching. yt-dlp and
/// rustypipe can key the same creator by slightly different ids/names, so cards
/// reconcile avatars by this normalized name as well as by id.
pub fn norm_channel_name(name: &str) -> String {
    name.trim().to_lowercase()
}

/// `SponsorBlock` segment categories. These map 1:1 onto the official
/// `SponsorBlock` API category strings (`api_key`), so the same enum is
/// used both for the on-screen seek-bar markers and for the live query.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, PartialOrd, Ord)]
pub enum Category {
    Sponsor,
    SelfPromo,
    Interaction,
    Intro,
    Outro,
    Preview,
    MusicOfftopic,
    Filler,
}

impl Category {
    /// Every category, in the order they appear on the settings page.
    pub const ALL: [Category; 8] = [
        Category::Sponsor,
        Category::SelfPromo,
        Category::Interaction,
        Category::Intro,
        Category::Outro,
        Category::Preview,
        Category::MusicOfftopic,
        Category::Filler,
    ];

    /// The `SponsorBlock` API key (`category` field in the JSON response).
    pub fn api_key(self) -> &'static str {
        match self {
            Category::Sponsor => "sponsor",
            Category::SelfPromo => "selfpromo",
            Category::Interaction => "interaction",
            Category::Intro => "intro",
            Category::Outro => "outro",
            Category::Preview => "preview",
            Category::MusicOfftopic => "music_offtopic",
            Category::Filler => "filler",
        }
    }

    pub fn from_api_key(s: &str) -> Option<Category> {
        Category::ALL.into_iter().find(|c| c.api_key() == s)
    }

    /// Short human label for the seek bar legend + settings rows.
    pub fn label(self) -> &'static str {
        match self {
            Category::Sponsor => "Sponsor",
            Category::SelfPromo => "Self-promo",
            Category::Interaction => "Interaction reminder",
            Category::Intro => "Intermission / intro",
            Category::Outro => "Endcards / outro",
            Category::Preview => "Preview / recap",
            Category::MusicOfftopic => "Non-music section",
            Category::Filler => "Filler tangent",
        }
    }

    pub fn blurb(self) -> &'static str {
        match self {
            Category::Sponsor => "Paid promotion, paid referrals and direct advertisements.",
            Category::SelfPromo => "Unpaid/self promotion, merch, donations, charity.",
            Category::Interaction => "\"Like and subscribe\" reminders without other content.",
            Category::Intro => "An interval without actual content — pause, animation, intro.",
            Category::Outro => "Credits or when the video is over — endcards and outros.",
            Category::Preview => "Quick recap of previous episodes or a preview of what's coming.",
            Category::MusicOfftopic => "Non-music sections in a music video — for music listening.",
            Category::Filler => "Tangential scenes added only to lengthen the video.",
        }
    }

    /// The official `SponsorBlock` colour identity (RGB), kept here so the
    /// seek-bar legend matches what users already recognise. These are the
    /// brand colours of the *data source*, not OS chrome — chrome (panels,
    /// text, surfaces) still comes from the [`ui::theme::Theme`].
    pub fn rgb(self) -> (u8, u8, u8) {
        match self {
            Category::Sponsor => (0x00, 0xd4, 0x00),
            Category::SelfPromo => (0xff, 0xff, 0x00),
            Category::Interaction => (0xcc, 0x00, 0xff),
            Category::Intro => (0x00, 0xff, 0xff),
            Category::Outro => (0x02, 0x02, 0xed),
            Category::Preview => (0x00, 0x8f, 0xd6),
            Category::MusicOfftopic => (0xff, 0x94, 0x00),
            Category::Filler => (0x7a, 0x00, 0xff),
        }
    }
}

/// How Tube treats a `SponsorBlock` category, per-category. This is the
/// Grayjay-style three-way mode (not a plain on/off): each category can be
/// auto-skipped, merely marked with a manual skip button, or ignored entirely.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum SkipMode {
    /// Seek past the segment automatically as the playhead enters it.
    Skip,
    /// Mark it on the timeline and offer a manual "Skip" button, but never
    /// skip on its own.
    Show,
    /// Ignore the category — not marked on the timeline, never skipped.
    Disabled,
}

impl SkipMode {
    /// Short label for the settings mode selector.
    pub fn label(self) -> &'static str {
        match self {
            SkipMode::Skip => "Auto-skip",
            SkipMode::Show => "Show",
            SkipMode::Disabled => "Off",
        }
    }

    /// The stable name an effect names this mode by.
    pub fn key(self) -> &'static str {
        match self {
            SkipMode::Skip => "skip",
            SkipMode::Show => "show",
            SkipMode::Disabled => "disabled",
        }
    }

    pub fn from_key(key: &str) -> Option<Self> {
        [SkipMode::Skip, SkipMode::Show, SkipMode::Disabled]
            .into_iter()
            .find(|mode| mode.key() == key)
    }

    /// Cycle order for the settings selector: Auto-skip → Show → Off → …
    #[must_use]
    pub fn next(self) -> Self {
        match self {
            SkipMode::Skip => SkipMode::Show,
            SkipMode::Show => SkipMode::Disabled,
            SkipMode::Disabled => SkipMode::Skip,
        }
    }

    /// Whether the category is considered at all (marked on the timeline /
    /// queried). `Disabled` is the only mode that is not.
    pub fn is_enabled(self) -> bool {
        !matches!(self, SkipMode::Disabled)
    }
}

/// One `SponsorBlock` segment within a video, in seconds.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Segment {
    pub category: Category,
    pub start: f64,
    pub end: f64,
    /// Community votes (informational only here).
    #[serde(default)]
    pub votes: i64,
}

impl Segment {
    pub fn contains(&self, t: f64) -> bool {
        t >= self.start && t < self.end
    }
}

/// A single comment on the watch page.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Comment {
    pub author: String,
    pub initials: String,
    pub text: String,
    pub likes: u64,
    pub time_ago: String,
    #[serde(default)]
    pub pinned: bool,
    #[serde(default)]
    pub hearted: bool,
    #[serde(default)]
    pub replies: u32,
}

pub type PlaylistId = String;

/// A playlist result / page: an ordered collection of videos with its own
/// title, creator and thumbnail. Populated from the live backend; empty on the
/// hermetic sample.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Playlist {
    pub id: PlaylistId,
    pub title: String,
    pub channel_id: ChannelId,
    pub channel_name: String,
    /// Real thumbnail URL (the playlist's cover, usually its first video's
    /// still), fetched through the [`ui::ImageStore`]. Empty ⇒ seed gradient.
    #[serde(default)]
    pub thumbnail_url: String,
    /// Deterministic gradient seed for the thumbnail fallback.
    pub seed: String,
    /// Total number of videos in the playlist (may exceed the ids we hold).
    pub video_count: u64,
    /// Ordered ids of the playlist's videos (as many as the backend returned).
    #[serde(default)]
    pub video_ids: Vec<VideoId>,
}

/// The three result kinds Tube's search can surface, used both as the type
/// filter and to tag a search result row.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum SearchType {
    Video,
    Playlist,
    Creator,
}

impl SearchType {
    pub const ALL: [SearchType; 3] = [SearchType::Video, SearchType::Playlist, SearchType::Creator];

    pub fn label(self) -> &'static str {
        match self {
            SearchType::Video => "Video",
            SearchType::Playlist => "Playlist",
            SearchType::Creator => "Creator",
        }
    }
}

/// Multi-select type filter for the search results. All three types are shown
/// by default; the *first* click on any chip narrows to just that type
/// (deselecting the others), and subsequent clicks toggle individual chips.
/// Toggling everything off is treated as "show all" so the results never dead-end.
#[allow(
    clippy::struct_excessive_bools,
    reason = "each bool is an independent, orthogonal filter chip toggle (video/playlist/ \
              creator), not a state machine"
)]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SearchTypeFilter {
    pub video: bool,
    pub playlist: bool,
    pub creator: bool,
    /// True until the user has interacted; the first interaction is the
    /// "narrow to just this type" click.
    pub pristine: bool,
}

impl Default for SearchTypeFilter {
    fn default() -> Self {
        Self {
            video: true,
            playlist: true,
            creator: true,
            pristine: true,
        }
    }
}

impl SearchTypeFilter {
    /// Whether type `t` is currently selected (and thus its results shown + its
    /// chip highlighted).
    pub fn shows(&self, t: SearchType) -> bool {
        match t {
            SearchType::Video => self.video,
            SearchType::Playlist => self.playlist,
            SearchType::Creator => self.creator,
        }
    }

    /// Apply a click on the chip for type `t`. The first click narrows to just
    /// `t`; later clicks toggle `t`. If a toggle would leave nothing selected,
    /// revert to the all-selected (pristine) default.
    pub fn click(&mut self, t: SearchType) {
        if self.pristine {
            self.video = t == SearchType::Video;
            self.playlist = t == SearchType::Playlist;
            self.creator = t == SearchType::Creator;
            self.pristine = false;
        } else {
            match t {
                SearchType::Video => self.video = !self.video,
                SearchType::Playlist => self.playlist = !self.playlist,
                SearchType::Creator => self.creator = !self.creator,
            }
            if !self.video && !self.playlist && !self.creator {
                *self = SearchTypeFilter::default();
            }
        }
    }
}

/// A channel / uploader.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Channel {
    pub id: ChannelId,
    pub name: String,
    pub handle: String,
    pub subscribers: u64,
    pub verified: bool,
    pub description: String,
    /// Seed for the deterministic avatar/banner gradient (fallback when no
    /// real image URL is available or before it loads).
    pub seed: String,
    /// Real channel avatar image URL (yt3.googleusercontent.com), fetched
    /// through the [`ui::ImageStore`] and painted in place of the initials
    /// avatar once decoded. Empty on the offline sample.
    #[serde(default)]
    pub avatar_url: String,
    /// Real channel banner image URL, painted across the channel header once
    /// decoded. Empty on the offline sample (the seed gradient shows instead).
    #[serde(default)]
    pub banner_url: String,
}

/// A video and everything the watch page needs about it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Video {
    pub id: VideoId,
    pub title: String,
    /// `DeArrow` community-sourced, non-clickbait title. When `DeArrow` is
    /// enabled this replaces `title`; an indicator badge marks it.
    #[serde(default)]
    pub dearrow_title: Option<String>,
    pub channel_id: ChannelId,
    pub channel_name: String,
    pub channel_seed: String,
    /// Real uploader avatar URL carried on a feed/search list item (`YouTube`
    /// returns it on the channel tag). Used to populate the channel record's
    /// [`Channel::avatar_url`] so Home/Search cards show the real creator icon
    /// without a separate channel-page fetch. Empty on the offline sample.
    #[serde(default)]
    pub channel_avatar_url: String,
    pub views: u64,
    pub published: String,
    pub duration: u32,
    pub description: String,
    pub likes: u64,
    pub seed: String,
    /// Real video thumbnail image URL (i.ytimg.com), fetched through the
    /// [`ui::ImageStore`] and painted cover-fit on the card / poster once
    /// decoded. Empty on the offline sample (the seed gradient shows instead).
    #[serde(default)]
    pub thumbnail_url: String,
    pub category: String,
    /// Marks whether `DeArrow` has a community thumbnail for this video
    /// (we render a "neutral frame" treatment instead of the clickbait
    /// frame when [`Settings::dearrow_thumbnails`] is on).
    #[serde(default)]
    pub dearrow_thumb: bool,
    #[serde(default)]
    pub sponsor: Vec<Segment>,
    #[serde(default)]
    pub comments: Vec<Comment>,
    #[serde(default)]
    pub live: bool,
}

impl Video {
    /// The title to display given the `DeArrow` toggle.
    pub fn display_title(&self, dearrow: bool) -> &str {
        if dearrow && let Some(t) = &self.dearrow_title {
            return t;
        }
        &self.title
    }

    /// Whether a `DeArrow` alternative exists *and* is active.
    pub fn dearrow_active(&self, dearrow: bool) -> bool {
        dearrow && self.dearrow_title.is_some()
    }
}

/// How this person likes Tube to play: `SponsorBlock`, `DeArrow`, dislike
/// counts, autoplay, comments and the preferred quality. A personal setting
/// that follows them to every device and that collaborators on a shared
/// library never see.
#[allow(
    clippy::struct_excessive_bools,
    reason = "each bool is an independent, user-facing setting toggle, not a state machine"
)]
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Settings {
    pub sponsorblock_enabled: bool,
    /// Per-category [`SkipMode`] (auto-skip / show / off). Absent key defaults
    /// to auto-skip (matches the upstream extension default of "skip everything
    /// but filler").
    pub categories: BTreeMap<String, SkipMode>,
    /// `DeArrow` master switch. When off, neither titles nor thumbnails are
    /// replaced regardless of the two sub-toggles below.
    pub dearrow_enabled: bool,
    /// Replace clickbait titles with the community-sourced factual one.
    #[serde(default = "default_true")]
    pub dearrow_titles: bool,
    pub dearrow_thumbnails: bool,
    /// Show the crowdsourced dislike count (Return `YouTube` Dislike) on the watch
    /// page alongside likes.
    #[serde(default = "default_true")]
    pub return_dislike_enabled: bool,
    pub autoplay: bool,
    pub show_comments: bool,
    pub preferred_quality: String,
}

/// serde default helper (a `#[serde(default)]` bool defaults to `false`; several
/// of our flags want to default *on* when absent from older persisted state).
fn default_true() -> bool {
    true
}

impl ui::storage::SettingsSchema for Settings {
    const SCOPE: ui::storage::Scope = ui::storage::Scope::Personal;
}

impl Default for Settings {
    fn default() -> Self {
        let mut categories = BTreeMap::new();
        for c in Category::ALL {
            // Filler ignored by default, everything else auto-skipped — the
            // SponsorBlock community default.
            let mode = if c == Category::Filler {
                SkipMode::Disabled
            } else {
                SkipMode::Skip
            };
            categories.insert(c.api_key().to_string(), mode);
        }
        Self {
            sponsorblock_enabled: true,
            categories,
            dearrow_enabled: true,
            dearrow_titles: true,
            dearrow_thumbnails: true,
            return_dislike_enabled: true,
            autoplay: true,
            show_comments: true,
            preferred_quality: "1080p".to_string(),
        }
    }
}

impl Settings {
    /// The effective [`SkipMode`] for a category: the master switch forces
    /// `Disabled` when `SponsorBlock` is off; otherwise the stored per-category
    /// mode (defaulting to auto-skip for an absent key).
    pub fn mode_of(&self, c: Category) -> SkipMode {
        if !self.sponsorblock_enabled {
            return SkipMode::Disabled;
        }
        self.categories
            .get(c.api_key())
            .copied()
            .unwrap_or(SkipMode::Skip)
    }

    /// Whether the category is marked/considered at all (Show or Auto-skip).
    pub fn category_enabled(&self, c: Category) -> bool {
        self.mode_of(c).is_enabled()
    }

    /// Whether the playhead entering this category should auto-skip it.
    pub fn should_auto_skip(&self, c: Category) -> bool {
        matches!(self.mode_of(c), SkipMode::Skip)
    }

    /// Whether this category should offer a manual "Skip" button (Show mode).
    pub fn should_show_skip(&self, c: Category) -> bool {
        matches!(self.mode_of(c), SkipMode::Show)
    }

    /// Cycle a category's mode: Auto-skip → Show → Off → Auto-skip.
    pub fn cycle_category(&mut self, c: Category) {
        let cur = self
            .categories
            .get(c.api_key())
            .copied()
            .unwrap_or(SkipMode::Skip);
        self.categories.insert(c.api_key().to_string(), cur.next());
    }

    /// Whether `DeArrow` title replacement is active (master + titles sub-toggle).
    pub fn dearrow_titles_on(&self) -> bool {
        self.dearrow_enabled && self.dearrow_titles
    }

    /// Whether `DeArrow` thumbnail replacement is active (master + thumbs toggle).
    pub fn dearrow_thumbs_on(&self) -> bool {
        self.dearrow_enabled && self.dearrow_thumbnails
    }
}

/// Where the catalogue currently on screen came from. Drives the top-bar
/// status pill ("Live" vs "Sample") so the user always knows whether they are
/// looking at real `YouTube` data or the bundled offline sample.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct DataSource {
    /// True once a live backend has returned data this session.
    pub live: bool,
    /// Short human label for the pill (e.g. "offline — sample data",
    /// "live (rustypipe)", "connecting…").
    pub label: String,
}

impl Default for DataSource {
    fn default() -> Self {
        Self {
            live: false,
            label: "sample data".into(),
        }
    }
}

/// Which screen the app is showing.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum Route {
    Home,
    Trending,
    Subscriptions,
    /// A collaboratively shared set of videos. Membership is synchronised,
    /// while playback order deliberately stays local to each viewer.
    Queue,
    Watch(VideoId),
    Channel(ChannelId),
    Playlist(PlaylistId),
    Search(String),
    History,
    Settings,
}

impl Route {
    /// The nav-rail section this route belongs to (for highlighting).
    pub fn nav_section(&self) -> &'static str {
        match self {
            Route::Home => "home",
            Route::Trending => "trending",
            Route::Subscriptions => "subs",
            Route::Queue => "queue",
            Route::History => "history",
            Route::Settings => "settings",
            _ => "",
        }
    }
}

/// The complete application state.
#[allow(
    clippy::struct_excessive_bools,
    reason = "each bool is an independent piece of application state, not a state machine"
)]
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TubeModel {
    pub videos: Vec<Video>,
    pub channels: Vec<Channel>,
    /// Playlists we've seen (from search results / opened playlist pages).
    #[serde(default)]
    pub playlists: Vec<Playlist>,
    /// Canonical local channel ids plus normalized `YouTube` channel URLs pulled
    /// before this device has fetched that channel's metadata. URL entries are
    /// intentionally retained so paired-device libraries round-trip safely.
    pub subscriptions: Vec<ChannelId>,
    /// Shared watch-queue membership projected from the document. Queue order is
    /// deliberately not a collaborative presentation concern: keyed membership
    /// makes concurrent add/remove operations converge without one client
    /// silently reordering another client's queue.
    #[serde(default)]
    pub queue: Vec<VideoId>,
    /// The person's watch history, newest first, as their own record holds
    /// it ([`crate::history::WatchHistory`]); never part of the shared library.
    /// Ids without loaded metadata stay, so a refresh cannot drop history.
    pub history: Vec<VideoId>,
    /// Shown when the watch history could not be stored.
    #[serde(default)]
    pub history_notice: Option<String>,
    pub settings: Settings,
    pub route: Route,
    /// Last submitted, device-local search query. The editable top-bar buffer
    /// is a toolkit [`ui::LocalState`] owned by `TubeApp`; it is copied here
    /// only when the user submits so this route can render independently.
    pub search_query: String,
    /// Playhead position (seconds) on the current watch page.
    pub playhead: f64,
    pub playing: bool,
    /// Videos the user has liked this session (local, mirrored into the CRDT).
    /// The watch page's like pill reads this to render its active state + the
    /// incremented count.
    #[serde(default)]
    pub liked: Vec<VideoId>,
    /// Durable comment-like membership, keyed by the stable source-content
    /// identity used by the action layer. Unlike fetched comments themselves,
    /// these are user mutations mirrored from the collaborative document.
    #[serde(default)]
    pub comment_likes: BTreeSet<String>,
    /// Transient user-facing notices — auto-skip confirmations, worker status,
    /// "link copied" acknowledgements — rendered as a toast stack on the watch
    /// page. Newest first, capped.
    #[serde(default)]
    pub notices: Vec<String>,
    /// Provenance of the on-screen catalogue (sample vs live).
    #[serde(default)]
    pub data_source: DataSource,
    /// The query whose live results are held in `search_hits`. Lets the
    /// search screen render the backend's exact ordered result set (which a
    /// local substring filter would otherwise mangle for multi-word queries)
    /// while still falling back to the local filter when there are no live
    /// hits.
    #[serde(default)]
    pub search_hits_query: String,
    /// Ordered video ids returned by the live backend for `search_hits_query`.
    #[serde(default)]
    pub search_hits: Vec<VideoId>,
    /// Ordered playlist ids returned by the live backend for `search_hits_query`.
    #[serde(default)]
    pub search_playlist_hits: Vec<PlaylistId>,
    /// Ordered creator/channel ids returned by the live backend for
    /// `search_hits_query`.
    #[serde(default)]
    pub search_channel_hits: Vec<ChannelId>,
    /// The active multi-select type filter on the search results.
    #[serde(default)]
    pub search_type_filter: SearchTypeFilter,
    /// The video whose live "up next" list is held in `related_hits`.
    #[serde(default)]
    pub related_of: VideoId,
    /// Ordered related-video ids returned by the live backend for `related_of`.
    #[serde(default)]
    pub related_hits: Vec<VideoId>,
    /// Crowdsourced dislike counts (Return `YouTube` Dislike), keyed by video id.
    /// Kept off [`Video`] so the API result folds in without a catalogue rewrite.
    #[serde(default)]
    pub dislikes: BTreeMap<VideoId, u64>,
    /// `DeArrow` community thumbnail URLs (a rendered frame at the top-voted
    /// timestamp, or a submitted image), keyed by video id. Used in place of the
    /// clickbait frame when `DeArrow` thumbnails are enabled.
    #[serde(default)]
    pub dearrow_thumb_url: BTreeMap<VideoId, String>,
    /// Whether the home/trending feed is currently being fetched from the live
    /// backend. Drives the Home skeleton so a cold, empty catalogue reads as
    /// "loading" rather than a blank page. The online app starts `true` (it fetches
    /// on launch); the hermetic `--shots`/sample path leaves it `false`.
    #[serde(default)]
    pub feed_loading: bool,
    /// A user-legible reason the newest Home request could not be queued. This
    /// is distinct from offline provenance: queue saturation is local and
    /// retryable, not evidence that the network is unavailable.
    #[serde(default)]
    pub feed_error: Option<String>,
    /// Whether a search is in flight, so the results screen shows a skeleton
    /// instead of a premature "No results".
    #[serde(default)]
    pub search_loading: bool,
    /// A user-legible reason the newest search could not be queued. Cleared on
    /// each fresh submission and on a result response.
    #[serde(default)]
    pub search_error: Option<String>,
}

impl Default for TubeModel {
    fn default() -> Self {
        Self {
            videos: Vec::new(),
            channels: Vec::new(),
            playlists: Vec::new(),
            subscriptions: Vec::new(),
            queue: Vec::new(),
            history: Vec::new(),
            history_notice: None,
            settings: Settings::default(),
            route: Route::Home,
            search_query: String::new(),
            playhead: 0.0,
            playing: false,
            liked: Vec::new(),
            comment_likes: BTreeSet::new(),
            notices: Vec::new(),
            data_source: DataSource::default(),
            search_hits_query: String::new(),
            search_hits: Vec::new(),
            search_playlist_hits: Vec::new(),
            search_channel_hits: Vec::new(),
            search_type_filter: SearchTypeFilter::default(),
            related_of: String::new(),
            related_hits: Vec::new(),
            dislikes: BTreeMap::new(),
            dearrow_thumb_url: BTreeMap::new(),
            feed_loading: false,
            feed_error: None,
            search_loading: false,
            search_error: None,
        }
    }
}

impl TubeModel {
    pub fn video(&self, id: &str) -> Option<&Video> {
        self.videos.iter().find(|v| v.id == id)
    }

    pub fn channel(&self, id: &str) -> Option<&Channel> {
        self.channels.iter().find(|c| c.id == id)
    }

    pub fn playlist(&self, id: &str) -> Option<&Playlist> {
        self.playlists.iter().find(|p| p.id == id)
    }

    /// Insert or update a playlist by id, preserving an already-fetched video
    /// list when the incoming record (e.g. a search-result stub) carries none.
    pub fn upsert_playlist(&mut self, p: Playlist) {
        if let Some(slot) = self.playlists.iter_mut().find(|x| x.id == p.id) {
            let keep_ids = std::mem::take(&mut slot.video_ids);
            *slot = p;
            if slot.video_ids.is_empty() {
                slot.video_ids = keep_ids;
            }
        } else {
            self.playlists.push(p);
        }
    }

    /// Playlists to render for `query`: the backend's ordered playlist hits.
    pub fn search_playlists_display(&self, query: &str) -> Vec<&Playlist> {
        if self.search_hits_query != query {
            return Vec::new();
        }
        self.search_playlist_hits
            .iter()
            .filter_map(|id| self.playlist(id))
            .collect()
    }

    /// Creators (channels) to render for `query`: the backend's ordered channel
    /// hits.
    pub fn search_creators_display(&self, query: &str) -> Vec<&Channel> {
        if self.search_hits_query != query {
            return Vec::new();
        }
        self.search_channel_hits
            .iter()
            .filter_map(|id| self.channel(id))
            .collect()
    }

    /// Return the durable subscription key associated with a channel. A key
    /// can be the local catalogue id or a normalized URL imported before this
    /// device learned that id; callers must use this key when unsubscribing so
    /// an imported subscription is not accidentally left active.
    pub fn subscription_key(&self, id: &str) -> Option<&str> {
        self.subscriptions.iter().find_map(|subscription| {
            let matches = subscription == id
                || self.channel(id).is_some_and(|channel| {
                    subscription == &channel.handle
                        || subscription
                            .strip_prefix("https://youtube.com/")
                            .is_some_and(|handle| handle == channel.handle.trim_start_matches('/'))
                });
            matches.then_some(subscription.as_str())
        })
    }

    pub fn is_subscribed(&self, id: &str) -> bool {
        self.subscription_key(id).is_some()
    }

    pub fn is_liked(&self, id: &str) -> bool {
        self.liked.iter().any(|l| l == id)
    }

    /// Whether a video is a member of the shared watch queue.
    pub fn is_queued(&self, id: &str) -> bool {
        self.queue.iter().any(|queued| queued == id)
    }

    /// The like count to display for a video: the base count plus one while the
    /// user has liked it this session, so the pill's number tracks its state.
    pub fn display_likes(&self, v: &Video) -> u64 {
        v.likes + u64::from(self.is_liked(&v.id))
    }

    /// The crowdsourced dislike count for a video (Return `YouTube` Dislike), once
    /// fetched. `None` until it lands (or when disabled).
    pub fn dislikes_of(&self, id: &str) -> Option<u64> {
        self.dislikes.get(id).copied()
    }

    /// The thumbnail URL to display for `v`, honouring the DeArrow-thumbnails
    /// setting: the community thumbnail when enabled and available, otherwise the
    /// video's own thumbnail. Keeps the DeArrow/original choice in one place so
    /// every card + poster stays consistent.
    pub fn display_thumbnail_url<'a>(&'a self, v: &'a Video) -> &'a str {
        if self.settings.dearrow_thumbs_on()
            && let Some(url) = self.dearrow_thumb_url.get(&v.id)
            && !url.is_empty()
        {
            return url;
        }
        &v.thumbnail_url
    }

    /// Push a transient notice onto the front of the toast stack (newest first,
    /// capped so it stays a compact strip).
    pub fn push_notice(&mut self, msg: impl Into<String>) {
        self.notices.insert(0, msg.into());
        self.notices.truncate(8);
    }

    /// Videos uploaded by channels the user is subscribed to.
    pub fn subscription_feed(&self) -> Vec<&Video> {
        self.videos
            .iter()
            .filter(|v| self.is_subscribed(&v.channel_id))
            .collect()
    }

    pub fn channel_videos(&self, id: &str) -> Vec<&Video> {
        self.videos.iter().filter(|v| v.channel_id == id).collect()
    }

    pub fn history_videos(&self) -> Vec<&Video> {
        self.history
            .iter()
            .filter_map(|id| self.video(id))
            .collect()
    }

    /// Insert or update a video by id, preserving any `SponsorBlock` segments /
    /// comments already fetched for it (a fresh list item carries neither).
    pub fn upsert_video(&mut self, v: Video) {
        if let Some(slot) = self.videos.iter_mut().find(|x| x.id == v.id) {
            let keep_sponsor = std::mem::take(&mut slot.sponsor);
            let keep_comments = std::mem::take(&mut slot.comments);
            let keep_dearrow = slot.dearrow_title.take();
            let keep_thumb = std::mem::take(&mut slot.thumbnail_url);
            *slot = v;
            if slot.sponsor.is_empty() {
                slot.sponsor = keep_sponsor;
            }
            if slot.comments.is_empty() {
                slot.comments = keep_comments;
            }
            if slot.dearrow_title.is_none() {
                slot.dearrow_title = keep_dearrow;
            }
            // A later detail/related fetch may not carry a thumbnail URL; keep
            // the one a search/feed item already resolved so the card image
            // doesn't blank out on refresh.
            if slot.thumbnail_url.is_empty() {
                slot.thumbnail_url = keep_thumb;
            }
        } else {
            self.videos.push(v);
        }
    }

    /// Insert or replace a channel by id, preserving a real avatar/banner the
    /// record already holds when the incoming one carries none — a later
    /// avatar-less list item (or a creator row whose avatar URL didn't resolve)
    /// must never blank a photo an earlier item already resolved.
    pub fn upsert_channel(&mut self, c: Channel) {
        if let Some(slot) = self.channels.iter_mut().find(|x| x.id == c.id) {
            let keep_avatar = std::mem::take(&mut slot.avatar_url);
            let keep_banner = std::mem::take(&mut slot.banner_url);
            *slot = c;
            if slot.avatar_url.is_empty() {
                slot.avatar_url = keep_avatar;
            }
            if slot.banner_url.is_empty() {
                slot.banner_url = keep_banner;
            }
        } else {
            self.channels.push(c);
        }
    }

    /// Make sure a minimal channel entry exists so avatars, subscribe and the
    /// channel page have at least a name to show for a video's uploader (live
    /// feed items reference channels we haven't fetched a full page for yet).
    pub fn ensure_channel_stub(&mut self, id: &str, name: &str) {
        if id.is_empty() || self.channels.iter().any(|c| c.id == id) {
            return;
        }
        self.channels.push(Channel {
            id: id.to_string(),
            name: name.to_string(),
            handle: id.to_string(),
            subscribers: 0,
            verified: false,
            description: String::new(),
            seed: id.to_string(),
            avatar_url: String::new(),
            banner_url: String::new(),
        });
    }

    /// Record a channel's real avatar URL (carried on a feed/search list item)
    /// when we don't already hold one, so Home/Search cards paint the real
    /// creator icon without waiting on a separate channel-page fetch. A
    /// no-op for empty ids/urls or when a real avatar is already known.
    pub fn note_channel_avatar(&mut self, id: &str, url: &str) {
        if id.is_empty() || url.is_empty() {
            return;
        }
        if let Some(ch) = self.channels.iter_mut().find(|c| c.id == id)
            && ch.avatar_url.is_empty()
        {
            ch.avatar_url = url.to_string();
        }
    }

    /// Stamp `url` as the avatar for every avatar-less channel record that
    /// matches `id` OR the normalized display `name`. This reconciles an avatar
    /// discovered under one data source's channel identity (e.g. rustypipe, or a
    /// channel-page fetch whose returned id differs from the search item's) onto
    /// the exact record the video cards read. Returns true if anything changed.
    pub fn adopt_channel_avatar(&mut self, id: &str, name: &str, url: &str) -> bool {
        if url.is_empty() {
            return false;
        }
        let norm_name = norm_channel_name(name);
        let mut changed = false;
        for ch in &mut self.channels {
            let matches = (!id.is_empty() && ch.id == id)
                || (!norm_name.is_empty() && norm_channel_name(&ch.name) == norm_name);
            if matches && ch.avatar_url.is_empty() {
                ch.avatar_url = url.to_string();
                changed = true;
            }
        }
        changed
    }

    /// Fill any avatar-less channel record from another record describing the
    /// same creator (same normalized name) that already carries an avatar. Two
    /// sources (yt-dlp / rustypipe) can produce two records for one creator, only
    /// one of which has the photo; this bridges them so every card resolves.
    pub fn reconcile_channel_avatars(&mut self) {
        let mut by_name: BTreeMap<String, String> = BTreeMap::new();
        for ch in &self.channels {
            if !ch.avatar_url.is_empty() {
                by_name
                    .entry(norm_channel_name(&ch.name))
                    .or_insert_with(|| ch.avatar_url.clone());
            }
        }
        if by_name.is_empty() {
            return;
        }
        for ch in &mut self.channels {
            if ch.avatar_url.is_empty()
                && let Some(url) = by_name.get(&norm_channel_name(&ch.name))
            {
                url.clone_into(&mut ch.avatar_url);
            }
        }
    }

    /// Replace the whole catalogue with a freshly-fetched live set, marking the
    /// data source live and synthesising channel stubs for its uploaders.
    pub fn adopt_live_catalogue(&mut self, videos: Vec<Video>, label: impl Into<String>) {
        for v in &videos {
            self.ensure_channel_stub(&v.channel_id, &v.channel_name);
            self.adopt_channel_avatar(&v.channel_id, &v.channel_name, &v.channel_avatar_url);
        }
        self.videos = videos;
        self.reconcile_channel_avatars();
        // Stale local ordering hints no longer apply to the new catalogue.
        self.search_hits.clear();
        self.search_hits_query.clear();
        self.related_hits.clear();
        self.related_of.clear();
        self.data_source = DataSource {
            live: true,
            label: label.into(),
        };
    }

    /// Simple case-insensitive title/channel search.
    pub fn search(&self, q: &str) -> Vec<&Video> {
        let q = q.trim().to_lowercase();
        if q.is_empty() {
            return self.videos.iter().collect();
        }
        self.videos
            .iter()
            .filter(|v| {
                v.title.to_lowercase().contains(&q)
                    || v.channel_name.to_lowercase().contains(&q)
                    || v.category.to_lowercase().contains(&q)
            })
            .collect()
    }

    /// Results to render for `query`: the backend's exact ordered live hits
    /// when we have them for this query, otherwise the local substring filter
    /// (offline / before a live response lands).
    pub fn search_display(&self, query: &str) -> Vec<&Video> {
        if self.search_hits_query == query && !self.search_hits.is_empty() {
            let hits: Vec<&Video> = self
                .search_hits
                .iter()
                .filter_map(|id| self.video(id))
                .collect();
            if !hits.is_empty() {
                return hits;
            }
        }
        self.search(query)
    }

    /// "Up next" list to render for a watch page: the backend's real
    /// recommendations when we have them for this video, otherwise the local
    /// same-category heuristic.
    pub fn related_display(&self, id: &str) -> Vec<&Video> {
        if self.related_of == id && !self.related_hits.is_empty() {
            let hits: Vec<&Video> = self
                .related_hits
                .iter()
                .filter_map(|rid| self.video(rid))
                .collect();
            if !hits.is_empty() {
                return hits;
            }
        }
        self.related(id)
    }

    /// "Up next" suggestions for a watch page: same category first, then
    /// fill with the rest, excluding the current video.
    pub fn related(&self, id: &str) -> Vec<&Video> {
        let cat = self
            .video(id)
            .map(|v| v.category.clone())
            .unwrap_or_default();
        let mut same: Vec<&Video> = self
            .videos
            .iter()
            .filter(|v| v.id != id && v.category == cat)
            .collect();
        let mut others: Vec<&Video> = self
            .videos
            .iter()
            .filter(|v| v.id != id && v.category != cat)
            .collect();
        same.append(&mut others);
        same
    }

    /// Distinct categories present, for the home "chips" filter bar.
    pub fn categories(&self) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        for v in &self.videos {
            if !out.contains(&v.category) {
                out.push(v.category.clone());
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sponsorblock_categories_default_cycle_and_round_trip() {
        // Case `default_categories_match_sponsorblock_defaults`.
        {
            let s = Settings::default();
            // Sponsor auto-skips by default; Filler is off.
            assert!(s.category_enabled(Category::Sponsor));
            assert_eq!(s.mode_of(Category::Sponsor), SkipMode::Skip);
            assert!(s.should_auto_skip(Category::Sponsor));
            assert!(!s.category_enabled(Category::Filler));
            assert_eq!(s.mode_of(Category::Filler), SkipMode::Disabled);
        }
        // Case `sponsorblock_master_switch_overrides_categories`.
        {
            let s = Settings {
                sponsorblock_enabled: false,
                ..Settings::default()
            };
            assert!(!s.category_enabled(Category::Sponsor));
            assert_eq!(s.mode_of(Category::Sponsor), SkipMode::Disabled);
            assert!(!s.should_auto_skip(Category::Sponsor));
        }
        // Case `cycle_category_walks_skip_show_off`.
        {
            let mut s = Settings::default();
            assert_eq!(s.mode_of(Category::Intro), SkipMode::Skip);
            s.cycle_category(Category::Intro);
            assert_eq!(s.mode_of(Category::Intro), SkipMode::Show);
            assert!(s.should_show_skip(Category::Intro));
            assert!(s.category_enabled(Category::Intro), "Show is still enabled");
            s.cycle_category(Category::Intro);
            assert_eq!(s.mode_of(Category::Intro), SkipMode::Disabled);
            assert!(!s.category_enabled(Category::Intro));
            s.cycle_category(Category::Intro);
            assert_eq!(
                s.mode_of(Category::Intro),
                SkipMode::Skip,
                "cycles back to Auto-skip"
            );
        }
        // Case `category_roundtrips_api_key`.
        {
            for c in Category::ALL {
                assert_eq!(Category::from_api_key(c.api_key()), Some(c));
            }
        }
    }

    #[test]
    fn dearrow_toggles_gate_titles_and_thumbnails() {
        // Case `dearrow_sub_toggles_gate_titles_and_thumbnails`.
        {
            let mut s = Settings::default();
            assert!(s.dearrow_titles_on() && s.dearrow_thumbs_on());
            s.dearrow_titles = false;
            assert!(!s.dearrow_titles_on() && s.dearrow_thumbs_on());
            s.dearrow_enabled = false;
            assert!(
                !s.dearrow_titles_on() && !s.dearrow_thumbs_on(),
                "master gates both"
            );
        }
        // Case `dearrow_title_swaps_only_when_present_and_enabled`.
        {
            let mut v = sample_video();
            v.dearrow_title = Some("Calm title".into());
            assert_eq!(v.display_title(true), "Calm title");
            assert_eq!(v.display_title(false), "ORIGINAL");
            v.dearrow_title = None;
            assert_eq!(v.display_title(true), "ORIGINAL");
        }
    }

    #[test]
    fn search_type_filter_narrows_toggles_and_reverts() {
        // Case `search_type_filter_defaults_all_selected`.
        {
            let f = SearchTypeFilter::default();
            assert!(f.shows(SearchType::Video));
            assert!(f.shows(SearchType::Playlist));
            assert!(f.shows(SearchType::Creator));
            assert!(f.pristine);
        }
        // Case `search_type_filter_first_click_narrows_to_that_type`.
        {
            let mut f = SearchTypeFilter::default();
            f.click(SearchType::Playlist);
            assert!(
                !f.shows(SearchType::Video),
                "first click deselects the others"
            );
            assert!(
                f.shows(SearchType::Playlist),
                "first click selects the clicked type"
            );
            assert!(!f.shows(SearchType::Creator));
            assert!(!f.pristine);
        }
        // Case `search_type_filter_further_clicks_toggle_individually`.
        {
            let mut f = SearchTypeFilter::default();
            f.click(SearchType::Video); // narrow to Video only
            f.click(SearchType::Creator); // now Video + Creator
            assert!(f.shows(SearchType::Video));
            assert!(f.shows(SearchType::Creator));
            assert!(!f.shows(SearchType::Playlist));
            f.click(SearchType::Video); // toggle Video off → Creator only
            assert!(!f.shows(SearchType::Video));
            assert!(f.shows(SearchType::Creator));
        }
        // Case `search_type_filter_toggling_all_off_reverts_to_all`.
        {
            let mut f = SearchTypeFilter::default();
            f.click(SearchType::Video); // Video only
            f.click(SearchType::Video); // would be empty → revert to all
            assert!(f.shows(SearchType::Video));
            assert!(f.shows(SearchType::Playlist));
            assert!(f.shows(SearchType::Creator));
        }
    }

    fn chan(id: &str, name: &str, avatar: &str) -> Channel {
        Channel {
            id: id.into(),
            name: name.into(),
            handle: id.into(),
            subscribers: 0,
            verified: false,
            description: String::new(),
            seed: id.into(),
            avatar_url: avatar.into(),
            banner_url: String::new(),
        }
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "merged scenario: each former test is a labelled case block with its own fixture"
    )]
    fn channel_records_reconcile_avatars_without_losing_data() {
        // Case `adopt_channel_avatar_matches_by_id_and_by_normalized_name`.
        {
            let mut m = TubeModel::default();
            // Two records for the same creator: the card reads the id-A stub (empty).
            m.channels.push(chan("UC_A", "Blender Studio", ""));
            // An avatar arrives keyed by a DIFFERENT id but the same name — must land
            // on the id-A record so its cards resolve.
            let changed = m.adopt_channel_avatar("UC_B", "Blender Studio", "https://yt3/av");
            assert!(changed, "adopt reported a change");
            assert_eq!(
                m.channel("UC_A").unwrap().avatar_url,
                "https://yt3/av",
                "matched by name"
            );

            // A fresh stub matched by exact id.
            m.channels.push(chan("UC_C", "Other Creator", ""));
            m.adopt_channel_avatar("UC_C", "irrelevant name", "https://yt3/c");
            assert_eq!(
                m.channel("UC_C").unwrap().avatar_url,
                "https://yt3/c",
                "matched by id"
            );
        }
        // Case `adopt_channel_avatar_never_overwrites_an_existing_avatar`.
        {
            let mut m = TubeModel::default();
            m.channels
                .push(chan("UC_A", "Chan", "https://yt3/original"));
            let changed = m.adopt_channel_avatar("UC_A", "Chan", "https://yt3/other");
            assert!(!changed);
            assert_eq!(
                m.channel("UC_A").unwrap().avatar_url,
                "https://yt3/original"
            );
        }
        // Case `upsert_channel_keeps_a_real_avatar_when_the_new_record_has_none`.
        {
            let mut m = TubeModel::default();
            m.upsert_channel(chan("UC_A", "Creator", "https://yt3/good"));
            // A later avatar-less record for the same channel (e.g. a creator row
            // whose URL didn't resolve) must not blank the working photo.
            m.upsert_channel(chan("UC_A", "Creator", ""));
            assert_eq!(m.channel("UC_A").unwrap().avatar_url, "https://yt3/good");
        }
        // Case `reconcile_bridges_two_records_of_the_same_creator`.
        {
            let mut m = TubeModel::default();
            // The video card's uploader record (id A) has no avatar; a creator-row
            // record (id B) for the same creator has one. Reconcile must fill A.
            m.channels.push(chan("UC_A", "Blender Studio", ""));
            m.channels
                .push(chan("UC_B", "Blender Studio", "https://yt3/bs"));
            m.reconcile_channel_avatars();
            assert_eq!(
                m.channel("UC_A").unwrap().avatar_url,
                "https://yt3/bs",
                "avatar-less record filled from the same-name record that had one",
            );
        }
        // Case `upsert_playlist_preserves_video_ids`.
        {
            let mut m = TubeModel::default();
            m.upsert_playlist(Playlist {
                id: "PL1".into(),
                title: "Mix".into(),
                channel_id: "c".into(),
                channel_name: "C".into(),
                thumbnail_url: String::new(),
                seed: "PL1".into(),
                video_count: 3,
                video_ids: vec!["a".into(), "b".into(), "c".into()],
            });
            // A later stub (from a fresh search) carries no ids — the fetched list stays.
            m.upsert_playlist(Playlist {
                id: "PL1".into(),
                title: "Mix".into(),
                channel_id: "c".into(),
                channel_name: "C".into(),
                thumbnail_url: "https://x/t.jpg".into(),
                seed: "PL1".into(),
                video_count: 3,
                video_ids: vec![],
            });
            let p = m.playlist("PL1").unwrap();
            assert_eq!(
                p.video_ids.len(),
                3,
                "stub upsert keeps the fetched video list"
            );
            assert_eq!(
                p.thumbnail_url, "https://x/t.jpg",
                "but refreshes other fields"
            );
        }
        // Case `image_url_fields_round_trip_and_default`.
        {
            // A video/channel carrying real image URLs survives a serde round-trip
            // (the model is mirrored into the CRDT as JSON), and sparse JSON without
            // the optional fields deserializes with empty defaults.
            let mut v = sample_video();
            v.thumbnail_url = "https://i.ytimg.com/vi/x/hqdefault.jpg".into();
            let json = serde_json::to_string(&v).unwrap();
            let back: Video = serde_json::from_str(&json).unwrap();
            assert_eq!(back.thumbnail_url, v.thumbnail_url);

            let sparse = r#"{"id":"y","title":"T","channel_id":"c","channel_name":"C",
            "channel_seed":"c","views":0,"published":"","duration":0,"description":"",
            "likes":0,"seed":"y","category":"x"}"#;
            let v2: Video = serde_json::from_str(sparse).unwrap();
            assert_eq!(v2.thumbnail_url, "", "missing thumbnail_url defaults empty");

            let ch = Channel {
                id: "c".into(),
                name: "C".into(),
                handle: "@c".into(),
                subscribers: 1,
                verified: false,
                description: String::new(),
                seed: "c".into(),
                avatar_url: "https://yt3/av".into(),
                banner_url: "https://yt3/ba".into(),
            };
            let cj = serde_json::to_string(&ch).unwrap();
            let cback: Channel = serde_json::from_str(&cj).unwrap();
            assert_eq!(cback.avatar_url, "https://yt3/av");
            assert_eq!(cback.banner_url, "https://yt3/ba");
        }
    }

    #[test]
    fn an_imported_url_subscription_is_the_creators_subscription() {
        let mut m = TubeModel::default();
        m.channels.push(chan("UC_A", "Creator", ""));
        m.subscriptions.push("https://youtube.com/UC_A".into());
        assert!(m.is_subscribed("UC_A"));
        assert_eq!(m.subscription_key("UC_A"), Some("https://youtube.com/UC_A"));
        assert!(!m.is_subscribed("UC_B"));
    }

    fn sample_video() -> Video {
        Video {
            id: "x".into(),
            title: "ORIGINAL".into(),
            dearrow_title: None,
            channel_id: "c".into(),
            channel_name: "Chan".into(),
            channel_seed: "c".into(),
            channel_avatar_url: String::new(),
            views: 1,
            published: "now".into(),
            duration: 60,
            description: String::new(),
            likes: 0,
            seed: "x".into(),
            thumbnail_url: String::new(),
            category: "Tech".into(),
            dearrow_thumb: false,
            sponsor: vec![],
            comments: vec![],
            live: false,
        }
    }
}
