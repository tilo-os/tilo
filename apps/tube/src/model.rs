//! Pure data model for Tube. Everything here is plain serde-able data — no toolkit types, no
//! colours, no IO.

use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

use ui::tilo_resource::{self, Model};

pub type VideoId = String;
pub type ChannelId = String;

pub fn norm_channel_name(name: &str) -> String {
    name.trim().to_lowercase()
}

#[derive(
    Model, Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize, PartialOrd, Ord,
)]
pub enum Category {
    #[default]
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

    /// The catalogue message naming this category on the seek bar legend and in the settings rows.
    pub fn message(self) -> &'static str {
        match self {
            Category::Sponsor => "tube-category-sponsor",
            Category::SelfPromo => "tube-category-self-promo",
            Category::Interaction => "tube-category-interaction",
            Category::Intro => "tube-category-intro",
            Category::Outro => "tube-category-outro",
            Category::Preview => "tube-category-preview",
            Category::MusicOfftopic => "tube-category-music-offtopic",
            Category::Filler => "tube-category-filler",
        }
    }

    /// The catalogue message describing this category, shown under its name in the settings rows.
    pub fn blurb(self) -> &'static str {
        match self {
            Category::Sponsor => "tube-category-sponsor-blurb",
            Category::SelfPromo => "tube-category-self-promo-blurb",
            Category::Interaction => "tube-category-interaction-blurb",
            Category::Intro => "tube-category-intro-blurb",
            Category::Outro => "tube-category-outro-blurb",
            Category::Preview => "tube-category-preview-blurb",
            Category::MusicOfftopic => "tube-category-music-offtopic-blurb",
            Category::Filler => "tube-category-filler-blurb",
        }
    }

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

#[derive(Model, Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum SkipMode {
    /// An absent mode reads as the one most videos want.
    /// Seek past the segment automatically as the playhead enters it.
    #[default]
    Skip,
    /// Mark it on the timeline and offer a manual "Skip" button, but never
    /// skip on its own.
    Show,
    /// Ignore the category — not marked on the timeline, never skipped.
    Disabled,
}

impl SkipMode {
    /// The catalogue message naming this mode in the settings selector.
    pub fn message(self) -> &'static str {
        match self {
            SkipMode::Skip => "tube-skip-auto",
            SkipMode::Show => "tube-skip-show",
            SkipMode::Disabled => "tube-skip-off",
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
#[derive(Model, Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
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
#[derive(Model, Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
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
#[derive(Model, Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
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

    /// The stable name [`crate::actions::TubeEffect::FilterSearch`] names this type by.
    pub fn id(self) -> &'static str {
        match self {
            SearchType::Video => "video",
            SearchType::Playlist => "playlist",
            SearchType::Creator => "creator",
        }
    }

    pub fn message(self) -> &'static str {
        match self {
            SearchType::Video => "tube-type-video",
            SearchType::Playlist => "tube-type-playlist",
            SearchType::Creator => "tube-type-creator",
        }
    }
}

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
    pub fn shows(&self, t: SearchType) -> bool {
        match t {
            SearchType::Video => self.video,
            SearchType::Playlist => self.playlist,
            SearchType::Creator => self.creator,
        }
    }

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

#[derive(Model, Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
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
///
/// A declared resource type: a video the catalogue publishes is an item
/// resource of this type, found by "youtube videos of …" with nothing Tube
/// writes for it ([`resource`](ui::tilo_resource::resource)).
#[derive(Model, Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
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
    /// Whether this video plays only in the Tilo app: a live stream comes as
    /// HLS alone, which the browser build has no player for (its media
    /// element plays one progressive file through the fetch proxy).
    pub fn plays_only_in_app(&self) -> bool {
        self.live && !media::available(media::Feature::LiveStreams)
    }

    pub fn display_title(&self, dearrow: bool) -> &str {
        if dearrow && let Some(t) = &self.dearrow_title {
            return t;
        }
        &self.title
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
    pub categories: BTreeMap<String, SkipMode>,
    pub dearrow_enabled: bool,
    #[serde(default = "default_true")]
    pub dearrow_titles: bool,
    pub dearrow_thumbnails: bool,
    #[serde(default = "default_true")]
    pub return_dislike_enabled: bool,
    pub autoplay: bool,
    pub show_comments: bool,
    pub preferred_quality: String,
    #[serde(default = "default_speed")]
    pub playback_speed: f32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub caption_language: Option<String>,
}

fn default_speed() -> f32 {
    1.0
}

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
            playback_speed: default_speed(),
            caption_language: None,
        }
    }
}

impl Settings {
    pub fn mode_of(&self, c: Category) -> SkipMode {
        if !self.sponsorblock_enabled {
            return SkipMode::Disabled;
        }
        self.categories
            .get(c.api_key())
            .copied()
            .unwrap_or(SkipMode::Skip)
    }

    pub fn category_enabled(&self, c: Category) -> bool {
        self.mode_of(c).is_enabled()
    }

    pub fn should_auto_skip(&self, c: Category) -> bool {
        matches!(self.mode_of(c), SkipMode::Skip)
    }

    pub fn should_show_skip(&self, c: Category) -> bool {
        matches!(self.mode_of(c), SkipMode::Show)
    }

    pub fn dearrow_titles_on(&self) -> bool {
        self.dearrow_enabled && self.dearrow_titles
    }

    pub fn dearrow_thumbs_on(&self) -> bool {
        self.dearrow_enabled && self.dearrow_thumbnails
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct DataSource {
    pub live: bool,
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

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum Route {
    Home,
    Trending,
    Subscriptions,
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

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TubeModel {
    pub videos: Vec<Video>,
    pub channels: Vec<Channel>,
    #[serde(default)]
    pub playlists: Vec<Playlist>,
    pub subscriptions: Vec<ChannelId>,
    #[serde(default)]
    pub queue: Vec<VideoId>,
    pub history: Vec<VideoId>,
    #[serde(default)]
    pub history_positions: BTreeMap<VideoId, u64>,
    #[serde(default)]
    pub caption_tracks: Vec<crate::captions::CaptionTrack>,
    #[serde(default)]
    pub captions: Vec<crate::captions::CaptionCue>,
    #[serde(default)]
    pub captions_of: VideoId,
    #[serde(default)]
    pub captions_notice: Option<String>,
    #[serde(default)]
    pub stream_quality: String,
    #[serde(default)]
    pub history_notice: Option<String>,
    pub settings: Settings,
    pub route: Route,
    pub search_query: String,
    pub playhead: f64,
    pub playing: bool,
    #[serde(skip)]
    pub unplayable: Option<Unplayable>,
    #[serde(default)]
    pub liked: Vec<VideoId>,
    #[serde(default)]
    pub comment_likes: BTreeSet<String>,
    #[serde(default)]
    pub notices: Vec<String>,
    #[serde(default)]
    pub data_source: DataSource,
    #[serde(default)]
    pub search_hits_query: String,
    #[serde(default)]
    pub search_hits: Vec<VideoId>,
    #[serde(default)]
    pub search_playlist_hits: Vec<PlaylistId>,
    #[serde(default)]
    pub search_channel_hits: Vec<ChannelId>,
    #[serde(default)]
    pub search_type_filter: SearchTypeFilter,
    #[serde(default)]
    pub related_of: VideoId,
    #[serde(default)]
    pub related_hits: Vec<VideoId>,
    #[serde(default)]
    pub dislikes: BTreeMap<VideoId, u64>,
    #[serde(default)]
    pub dearrow_thumb_url: BTreeMap<VideoId, String>,
    #[serde(default)]
    pub feed_loading: bool,
    #[serde(default)]
    pub feed_error: Option<String>,
    #[serde(skip)]
    pub sources_unreached: Option<crate::api::Unreached>,
    #[serde(default)]
    pub search_loading: bool,
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
            history_positions: BTreeMap::new(),
            caption_tracks: Vec::new(),
            captions: Vec::new(),
            captions_of: String::new(),
            captions_notice: None,
            stream_quality: String::new(),
            history_notice: None,
            settings: Settings::default(),
            route: Route::Home,
            search_query: String::new(),
            playhead: 0.0,
            playing: false,
            unplayable: None,
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
            sources_unreached: None,
            search_loading: false,
            search_error: None,
        }
    }
}

impl TubeModel {
    pub fn video(&self, id: &str) -> Option<&Video> {
        self.videos.iter().find(|v| v.id == id)
    }

    pub fn show_skip_end(&self) -> Option<f64> {
        if !self.settings.sponsorblock_enabled {
            return None;
        }
        let Route::Watch(id) = &self.route else {
            return None;
        };
        self.video(id)?
            .sponsor
            .iter()
            .find(|s| self.settings.should_show_skip(s.category) && s.contains(self.playhead))
            .map(|s| s.end)
    }

    pub fn channel(&self, id: &str) -> Option<&Channel> {
        self.channels.iter().find(|c| c.id == id)
    }

    pub fn playlist(&self, id: &str) -> Option<&Playlist> {
        self.playlists.iter().find(|p| p.id == id)
    }

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

    pub fn search_playlists_display(&self, query: &str) -> Vec<&Playlist> {
        if self.search_hits_query != query {
            return Vec::new();
        }
        self.search_playlist_hits
            .iter()
            .filter_map(|id| self.playlist(id))
            .collect()
    }

    pub fn search_creators_display(&self, query: &str) -> Vec<&Channel> {
        if self.search_hits_query != query {
            return Vec::new();
        }
        self.search_channel_hits
            .iter()
            .filter_map(|id| self.channel(id))
            .collect()
    }

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

    pub fn is_queued(&self, id: &str) -> bool {
        self.queue.iter().any(|queued| queued == id)
    }

    pub fn display_likes(&self, v: &Video) -> u64 {
        v.likes + u64::from(self.is_liked(&v.id))
    }

    pub fn dislikes_of(&self, id: &str) -> Option<u64> {
        self.dislikes.get(id).copied()
    }

    pub fn display_thumbnail_url<'a>(&'a self, v: &'a Video) -> &'a str {
        if self.settings.dearrow_thumbs_on()
            && let Some(url) = self.dearrow_thumb_url.get(&v.id)
            && !url.is_empty()
        {
            return url;
        }
        &v.thumbnail_url
    }

    #[must_use]
    pub fn unplayable(&self, video: &str) -> Option<&Unplayable> {
        self.unplayable.as_ref().filter(|u| u.video == video)
    }

    pub fn push_notice(&mut self, msg: impl Into<String>) {
        self.notices.insert(0, msg.into());
        self.notices.truncate(8);
    }

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

    #[must_use]
    pub fn resume_at_ms(&self, video: &str) -> Option<u64> {
        let position = self.history_positions.get(video).copied()?;
        let duration = self.video(video).map(|v| u64::from(v.duration) * 1000);
        duration
            .is_none_or(|duration| {
                duration == 0
                    || position.saturating_add(crate::history::RESUME_TOLERANCE_MS) < duration
            })
            .then_some(position)
            .filter(|position| *position > 0)
    }

    #[must_use]
    pub fn watched_fraction(&self, video: &str) -> Option<f64> {
        let position = self.resume_at_ms(video)?;
        let duration = self.video(video).map(|v| f64::from(v.duration))?;
        (duration > 0.0).then(|| (position as f64 / 1000.0 / duration).clamp(0.0, 1.0))
    }

    #[must_use]
    pub fn chosen_caption_track(&self) -> Option<&crate::captions::CaptionTrack> {
        let language = self.settings.caption_language.as_deref()?;
        self.caption_tracks
            .iter()
            .find(|track| track.language == language)
    }

    pub fn upsert_video(&mut self, v: Video) {
        if let Some(slot) = self.videos.iter_mut().find(|x| x.id == v.id) {
            let keep_sponsor = std::mem::take(&mut slot.sponsor);
            let keep_comments = std::mem::take(&mut slot.comments);
            let keep_dearrow = slot.dearrow_title.take();
            let keep_thumb = std::mem::take(&mut slot.thumbnail_url);
            let keep_likes = slot.likes;
            *slot = v;
            if slot.likes == 0 {
                slot.likes = keep_likes;
            }
            if slot.sponsor.is_empty() {
                slot.sponsor = keep_sponsor;
            }
            if slot.comments.is_empty() {
                slot.comments = keep_comments;
            }
            if slot.dearrow_title.is_none() {
                slot.dearrow_title = keep_dearrow;
            }
            if slot.thumbnail_url.is_empty() {
                slot.thumbnail_url = keep_thumb;
            }
        } else {
            self.videos.push(v);
        }
    }

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

    pub fn adopt_live_catalogue(&mut self, videos: Vec<Video>, label: impl Into<String>) {
        for v in &videos {
            self.ensure_channel_stub(&v.channel_id, &v.channel_name);
            self.adopt_channel_avatar(&v.channel_id, &v.channel_name, &v.channel_avatar_url);
        }
        self.videos = videos;
        self.reconcile_channel_avatars();
        self.search_hits.clear();
        self.search_hits_query.clear();
        self.related_hits.clear();
        self.related_of.clear();
        self.data_source = DataSource {
            live: true,
            label: label.into(),
        };
    }

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

    pub fn search_display(&self, query: &str) -> Vec<&Video> {
        if self.search_hits_query == query
            && let Some(hits) = self.known_videos(&self.search_hits)
        {
            return hits;
        }
        self.search(query)
    }

    pub fn related_display(&self, id: &str) -> Vec<&Video> {
        if self.related_of == id
            && let Some(hits) = self.known_videos(&self.related_hits)
        {
            return hits;
        }
        self.related(id)
    }

    fn known_videos(&self, ids: &[VideoId]) -> Option<Vec<&Video>> {
        let hits: Vec<&Video> = ids.iter().filter_map(|id| self.video(id)).collect();
        (!hits.is_empty()).then_some(hits)
    }

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
        {
            let s = Settings::default();
            assert!(s.category_enabled(Category::Sponsor));
            assert_eq!(s.mode_of(Category::Sponsor), SkipMode::Skip);
            assert!(s.should_auto_skip(Category::Sponsor));
            assert!(!s.category_enabled(Category::Filler));
            assert_eq!(s.mode_of(Category::Filler), SkipMode::Disabled);
        }
        {
            let s = Settings {
                sponsorblock_enabled: false,
                ..Settings::default()
            };
            assert!(!s.category_enabled(Category::Sponsor));
            assert_eq!(s.mode_of(Category::Sponsor), SkipMode::Disabled);
            assert!(!s.should_auto_skip(Category::Sponsor));
        }
        {
            let mut s = Settings::default();
            assert_eq!(s.mode_of(Category::Intro), SkipMode::Skip);
            let cycle = |s: &mut Settings| {
                let next = s.mode_of(Category::Intro).next();
                s.categories
                    .insert(Category::Intro.api_key().to_string(), next);
            };
            cycle(&mut s);
            assert_eq!(s.mode_of(Category::Intro), SkipMode::Show);
            assert!(s.should_show_skip(Category::Intro));
            assert!(s.category_enabled(Category::Intro), "Show is still enabled");
            cycle(&mut s);
            assert_eq!(s.mode_of(Category::Intro), SkipMode::Disabled);
            assert!(!s.category_enabled(Category::Intro));
            cycle(&mut s);
            assert_eq!(
                s.mode_of(Category::Intro),
                SkipMode::Skip,
                "cycles back to Auto-skip"
            );
        }
        {
            for c in Category::ALL {
                assert_eq!(Category::from_api_key(c.api_key()), Some(c));
            }
        }
    }

    #[test]
    fn dearrow_toggles_gate_titles_and_thumbnails() {
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
        {
            let f = SearchTypeFilter::default();
            assert!(f.shows(SearchType::Video));
            assert!(f.shows(SearchType::Playlist));
            assert!(f.shows(SearchType::Creator));
            assert!(f.pristine);
        }
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
        {
            let mut m = TubeModel::default();
            m.channels.push(chan("UC_A", "Blender Studio", ""));
            let changed = m.adopt_channel_avatar("UC_B", "Blender Studio", "https://yt3/av");
            assert!(changed, "adopt reported a change");
            assert_eq!(
                m.channel("UC_A").unwrap().avatar_url,
                "https://yt3/av",
                "matched by name"
            );

            m.channels.push(chan("UC_C", "Other Creator", ""));
            m.adopt_channel_avatar("UC_C", "irrelevant name", "https://yt3/c");
            assert_eq!(
                m.channel("UC_C").unwrap().avatar_url,
                "https://yt3/c",
                "matched by id"
            );
        }
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
        {
            let mut m = TubeModel::default();
            m.upsert_channel(chan("UC_A", "Creator", "https://yt3/good"));
            m.upsert_channel(chan("UC_A", "Creator", ""));
            assert_eq!(m.channel("UC_A").unwrap().avatar_url, "https://yt3/good");
        }
        {
            let mut m = TubeModel::default();
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
        {
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

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Unplayable {
    pub video: VideoId,
    pub said: Option<String>,
}
