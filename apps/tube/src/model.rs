//! Pure data model for Tube. Everything here is plain serde-able data —
//! no toolkit types, no colours, no IO. The whole [`TubeModel`] is mirrored
//! into the Automerge document as JSON (see `app::TubeApp::persist`), which
//! is what gives subscriptions / settings / history undo + collaboration.
//!
//! Colours are never stored: thumbnails derive a deterministic gradient
//! from a seed string at draw time (`util::seed_gradient`) so the model
//! stays theme-agnostic and the OS keeps control of the palette.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

pub type VideoId = String;
pub type ChannelId = String;

/// SponsorBlock segment categories. These map 1:1 onto the official
/// SponsorBlock API category strings (`api_key`), so the same enum is
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

    /// The SponsorBlock API key (`category` field in the JSON response).
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

    /// The official SponsorBlock colour identity (RGB), kept here so the
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

/// One SponsorBlock segment within a video, in seconds.
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

/// A channel / uploader.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Channel {
    pub id: ChannelId,
    pub name: String,
    pub handle: String,
    pub subscribers: u64,
    pub verified: bool,
    pub description: String,
    /// Seed for the deterministic avatar/banner gradient.
    pub seed: String,
}

/// A video and everything the watch page needs about it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Video {
    pub id: VideoId,
    pub title: String,
    /// DeArrow community-sourced, non-clickbait title. When DeArrow is
    /// enabled this replaces [`title`]; an indicator badge marks it.
    #[serde(default)]
    pub dearrow_title: Option<String>,
    pub channel_id: ChannelId,
    pub channel_name: String,
    pub channel_seed: String,
    pub views: u64,
    pub published: String,
    pub duration: u32,
    pub description: String,
    pub likes: u64,
    pub seed: String,
    pub category: String,
    /// Marks whether DeArrow has a community thumbnail for this video
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
    /// The title to display given the DeArrow toggle.
    pub fn display_title<'a>(&'a self, dearrow: bool) -> &'a str {
        if dearrow {
            if let Some(t) = &self.dearrow_title {
                return t;
            }
        }
        &self.title
    }

    /// Whether a DeArrow alternative exists *and* is active.
    pub fn dearrow_active(&self, dearrow: bool) -> bool {
        dearrow && self.dearrow_title.is_some()
    }
}

/// User-controlled settings. Lives in the CRDT.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Settings {
    pub sponsorblock_enabled: bool,
    pub auto_skip: bool,
    /// Per-category enable map. Absent key = enabled (matches the
    /// upstream extension default of "skip everything but filler").
    pub categories: BTreeMap<String, bool>,
    pub dearrow_enabled: bool,
    pub dearrow_thumbnails: bool,
    pub autoplay: bool,
    pub show_comments: bool,
    pub preferred_quality: String,
    pub invidious_instance: String,
}

impl Default for Settings {
    fn default() -> Self {
        let mut categories = BTreeMap::new();
        for c in Category::ALL {
            // Filler off by default, everything else on — the SponsorBlock
            // community default.
            categories.insert(c.api_key().to_string(), c != Category::Filler);
        }
        Self {
            sponsorblock_enabled: true,
            auto_skip: true,
            categories,
            dearrow_enabled: true,
            dearrow_thumbnails: true,
            autoplay: true,
            show_comments: true,
            preferred_quality: "1080p".to_string(),
            invidious_instance: "https://invidious.fdn.fr".to_string(),
        }
    }
}

impl Settings {
    pub fn category_enabled(&self, c: Category) -> bool {
        self.sponsorblock_enabled && *self.categories.get(c.api_key()).unwrap_or(&true)
    }

    pub fn toggle_category(&mut self, c: Category) {
        let e = self.categories.entry(c.api_key().to_string()).or_insert(true);
        *e = !*e;
    }
}

/// Which screen the app is showing.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum Route {
    Home,
    Trending,
    Subscriptions,
    Watch(VideoId),
    Channel(ChannelId),
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
            Route::History => "history",
            Route::Settings => "settings",
            _ => "",
        }
    }
}

/// The complete application state.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TubeModel {
    pub videos: Vec<Video>,
    pub channels: Vec<Channel>,
    pub subscriptions: Vec<ChannelId>,
    pub history: Vec<VideoId>,
    pub settings: Settings,
    pub route: Route,
    pub search_query: String,
    /// Playhead position (seconds) on the current watch page.
    pub playhead: f64,
    pub playing: bool,
    /// Segments auto-skipped in this session — surfaced as a toast row.
    #[serde(default)]
    pub skipped_log: Vec<String>,
    /// Whether the Ctrl+K global command palette overlay is open.
    #[serde(default)]
    pub palette_open: bool,
    /// The palette's live query (separate from the top-bar search box so
    /// opening the palette doesn't clobber an in-progress search).
    #[serde(default)]
    pub palette_query: String,
}

impl Default for TubeModel {
    fn default() -> Self {
        Self {
            videos: Vec::new(),
            channels: Vec::new(),
            subscriptions: Vec::new(),
            history: Vec::new(),
            settings: Settings::default(),
            route: Route::Home,
            search_query: String::new(),
            playhead: 0.0,
            playing: false,
            skipped_log: Vec::new(),
            palette_open: false,
            palette_query: String::new(),
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

    pub fn is_subscribed(&self, id: &str) -> bool {
        self.subscriptions.iter().any(|c| c == id)
    }

    pub fn toggle_subscribe(&mut self, id: &str) {
        if let Some(pos) = self.subscriptions.iter().position(|c| c == id) {
            self.subscriptions.remove(pos);
        } else {
            self.subscriptions.push(id.to_string());
        }
    }

    /// Push a video onto the front of the watch history (dedup, cap 50).
    pub fn record_watch(&mut self, id: &str) {
        self.history.retain(|h| h != id);
        self.history.insert(0, id.to_string());
        self.history.truncate(50);
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

    /// "Up next" suggestions for a watch page: same category first, then
    /// fill with the rest, excluding the current video.
    pub fn related(&self, id: &str) -> Vec<&Video> {
        let cat = self.video(id).map(|v| v.category.clone()).unwrap_or_default();
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
    fn default_categories_match_sponsorblock_defaults() {
        let s = Settings::default();
        assert!(s.category_enabled(Category::Sponsor));
        assert!(!s.category_enabled(Category::Filler));
    }

    #[test]
    fn sponsorblock_master_switch_overrides_categories() {
        let mut s = Settings::default();
        s.sponsorblock_enabled = false;
        assert!(!s.category_enabled(Category::Sponsor));
    }

    #[test]
    fn dearrow_title_swaps_only_when_present_and_enabled() {
        let mut v = sample_video();
        v.dearrow_title = Some("Calm title".into());
        assert_eq!(v.display_title(true), "Calm title");
        assert_eq!(v.display_title(false), "ORIGINAL");
        v.dearrow_title = None;
        assert_eq!(v.display_title(true), "ORIGINAL");
    }

    #[test]
    fn subscribe_toggles() {
        let mut m = TubeModel::default();
        m.toggle_subscribe("c1");
        assert!(m.is_subscribed("c1"));
        m.toggle_subscribe("c1");
        assert!(!m.is_subscribed("c1"));
    }

    #[test]
    fn history_dedups_and_fronts() {
        let mut m = TubeModel::default();
        m.record_watch("a");
        m.record_watch("b");
        m.record_watch("a");
        assert_eq!(m.history, vec!["a", "b"]);
    }

    #[test]
    fn category_roundtrips_api_key() {
        for c in Category::ALL {
            assert_eq!(Category::from_api_key(c.api_key()), Some(c));
        }
    }

    fn sample_video() -> Video {
        Video {
            id: "x".into(),
            title: "ORIGINAL".into(),
            dearrow_title: None,
            channel_id: "c".into(),
            channel_name: "Chan".into(),
            channel_seed: "c".into(),
            views: 1,
            published: "now".into(),
            duration: 60,
            description: "".into(),
            likes: 0,
            seed: "x".into(),
            category: "Tech".into(),
            dearrow_thumb: false,
            sponsor: vec![],
            comments: vec![],
            live: false,
        }
    }
}
