//! `TubeApp` — the root widget.
//!
//! Tube keeps a single in-memory [`TubeModel`] as the source of truth for
//! rendering and mirrors it into the Automerge document (key `tube_state`)
//! so subscriptions / settings / history get undo + collaboration for
//! free. Navigation rebuilds the whole widget tree from the model in
//! `build_layout` (the `RebuildHarness` pattern), so the screen functions
//! in `screens.rs` can stay pure.
//!
//! Interaction flows through the [`ActionBus`]: leaf widgets push an
//! [`Action`]; `handle_event` drains the queue, mutates the model, persists
//! and flags a structural rebuild.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use automerge::transaction::Transactable;
use automerge::{AutoCommit, ObjId, ObjType, ReadDoc, ScalarValue, Value};
use taffy::{NodeId, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use winit::event::{ElementState, KeyEvent, WindowEvent};
use winit::keyboard::{Key, NamedKey};

use ui::permissions::PermissionContext;
use ui::presence::PresenceCtx;
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::widget::{NavSplit, SplitMode, VideoPlayer, Widget};

use crate::action::{self, Action, ActionBus, SettingFlag};
use crate::api::{Request, Update, Worker};
use crate::model::{Route, TubeModel};
use crate::screens;
use crate::widgets::{Flex, SharedPlayer};

const STATE_KEY: &str = "tube_state";
const SEARCH_KEY: &str = "search_query";
const PALETTE_KEY: &str = "palette_query";
const QUALITIES: [&str; 4] = ["1080p", "720p", "480p", "auto"];
/// Known public Invidious instances the "Source" setting cycles through. The
/// selected one is what [`Action::SubmitSearch`] actually queries.
const INSTANCES: [&str; 4] = [
    "https://invidious.fdn.fr",
    "https://yewtu.be",
    "https://vid.puffyan.us",
    "https://invidious.snopyta.org",
];

/// Below this allocated width (logical px) the navigation rail stops sitting
/// beside the content and collapses into an overlay flap summoned by the
/// hamburger. Chosen so a readable column of 300px video cards plus the rail
/// stop fitting comfortably side by side.
const NAV_BREAKPOINT: f64 = 760.0;

pub struct TubeApp {
    model: TubeModel,
    bus: ActionBus,
    worker: Option<Worker>,

    /// The freshly-built shell tree (top bar + nav rail + screen). Rebuilt
    /// in `build_layout`; raw pointers in the App's `node_map` point into
    /// it and stay valid until the next rebuild.
    root_child: Option<Box<dyn Widget>>,
    child_node: Option<NodeId>,
    rect: Rect,

    structure_dirty: bool,
    seeded: bool,
    search_obj: Option<ObjId>,
    /// CRDT Text object backing the Ctrl+K palette query, read live each
    /// `sync` so the results filter as the user types.
    palette_obj: Option<ObjId>,

    /// The user's explicit sidebar preference for this session, carried
    /// across the structural rebuilds that recreate the widget tree. `None`
    /// means "follow the layout default" (docked ⇒ open, overlay ⇒ closed);
    /// the hamburger / scrim / Esc set it to `Some(_)`. The live [`NavSplit`]
    /// owns the canonical state per frame; this mirrors it so a navigation
    /// rebuild doesn't snap the flap back to its default.
    sidebar_open: Option<bool>,
    /// `--shots` only: force the overlay flap open in the rendered still so a
    /// narrow screenshot captures the summoned rail + scrim rather than an
    /// empty closed flap. Never set by the live app.
    force_sidebar_open_shot: bool,
    /// Last resolved docked/overlay state, to detect a breakpoint crossing on
    /// resize and trigger a rebuild that re-seeds the split + drops a stale
    /// user preference.
    last_nav_overlay: Option<bool>,
    /// The docked/overlay resolution the *current* widget tree was seeded
    /// with. The first `build_layout` runs before any width is known (rect
    /// ZERO ⇒ overlay assumed), so once the true width lands in
    /// `apply_layout` we compare against this and rebuild if the seed was
    /// wrong — a wide window re-seeds its rail open on the next frame instead
    /// of being stuck on the unknown-width default.
    built_overlay: Option<bool>,

    /// Whether this instance should reach out to the live data backend. Set by
    /// [`TubeApp::online`] — the live binary turns it on; the `--shots` /
    /// offscreen path leaves it off so every screenshot stays hermetic on the
    /// bundled sample (no worker thread, no network).
    online: bool,
    /// Guards the one-shot startup fetch (home/trending) so we kick it off
    /// exactly once, on the first `sync`, rather than every frame.
    kicked_off: bool,

    /// The watch id we have already asked the worker to resolve a stream for, so
    /// a [`Request::PlayVideo`] is sent exactly once per watch-open (not every
    /// frame). Cleared implicitly by opening a *different* video.
    playing_id: Option<String>,
    /// The shared, persistent video player for the watch page — a real
    /// [`ui::widget::VideoPlayer`] backed by a [`media::MediaEngine`] (GStreamer
    /// decode + audio out with `--features media`, a synthesized frame otherwise).
    /// It lives here, behind an `Rc<RefCell<_>>`, so it survives the whole-tree
    /// rebuilds Tube does on every structural change — each rebuilt watch screen
    /// mounts it via a cheap [`crate::widgets::PlayerHost`] rather than recreating
    /// it (which would restart the video). The engine is opened on a resolved
    /// stream URL when [`Update::StreamResolved`] lands.
    player: SharedPlayer,
    /// The video id whose stream URL is currently loaded into the player engine,
    /// so we open a source exactly once per resolution and can mirror the engine
    /// playhead back onto the model only while it's the open video.
    player_video: Option<String>,

    /// Async image loader/cache for real thumbnails / avatars / banners, keyed
    /// by URL. `Some` when online (or seeded for a real-image `--shots` proof);
    /// `None` on the hermetic offline path, where every image lookup returns
    /// `None` and the procedural gradient/initials fallback shows.
    images: Option<ui::ImageStore>,

    /// End (seconds) of the show-mode SponsorBlock segment the playhead is
    /// currently inside, if any. The on-video "Skip" overlay is shown for it.
    /// Tracked here so a *transition* (playhead entering/leaving the segment)
    /// dirties the tree — the advancing playhead alone doesn't rebuild it.
    show_skip_end: Option<f64>,
}

impl Default for TubeApp {
    fn default() -> Self {
        Self::new()
    }
}

impl TubeApp {
    pub fn new() -> Self {
        Self {
            model: crate::sample::model(),
            bus: action::new_bus(),
            worker: None,
            root_child: None,
            child_node: None,
            rect: Rect::ZERO,
            structure_dirty: false,
            seeded: false,
            search_obj: None,
            palette_obj: None,
            sidebar_open: None,
            force_sidebar_open_shot: false,
            last_nav_overlay: None,
            built_overlay: None,
            online: false,
            kicked_off: false,
            playing_id: None,
            player: Rc::new(RefCell::new(
                VideoPlayer::new().with_engine(media::MediaEngine::new()),
            )),
            player_video: None,
            images: None,
            show_skip_end: None,
        }
    }

    /// Opt this instance into live data. The live binary calls this; the
    /// `--shots`/offscreen renderer and the interaction tests do not, so they
    /// stay fully offline and deterministic. When online, the first `sync`
    /// spawns the fetch worker and requests the home/trending feed; if the
    /// network is unreachable (or the `live` feature is off) the worker reports
    /// "offline — sample data" and the bundled catalogue stays on screen.
    pub fn online(mut self) -> Self {
        self.online = true;
        // No fabricated content: the real app starts with an EMPTY catalogue and
        // populates only from live results (yt-dlp / rustypipe / trending). The
        // bundled `sample` catalogue exists ONLY for the hermetic `--shots`/demo
        // review path — never presented as real. Home shows a loading skeleton
        // until the first live feed lands.
        self.model = TubeModel::default();
        self.model.feed_loading = true;
        // Real thumbnails / avatars / banners: an async image store backed by a
        // network fetcher. Kept off the offline/`--shots` path (which never
        // calls `online`) so screenshots stay hermetic.
        self.images = Some(crate::images::image_store());
        self
    }

    /// Test/shots hook: attach a real image store so an offscreen `--shots`
    /// render can paint fetched thumbnails/avatar/banner (the creator-page
    /// proof). The live app gets its store via [`Self::online`]; the default
    /// offscreen path leaves it `None` and stays hermetic.
    pub fn with_image_store(mut self, store: ui::ImageStore) -> Self {
        self.images = Some(store);
        self
    }

    /// Test/shots hook: replace the model wholesale (e.g. a channel page built
    /// from real yt-dlp data) so an offscreen render shows real content.
    pub fn with_model_for_test(mut self, model: TubeModel) -> Self {
        self.model = model;
        self
    }

    /// `--shots` builder: start on an EMPTY Home in its loading state, so a still
    /// captures the skeleton the real (online) app shows on a cold start before
    /// live results land — proving the no-fake-content default reads as
    /// "loading", not a blank page.
    pub fn with_loading_home_shot(mut self) -> Self {
        self.model = TubeModel::default();
        self.model.feed_loading = true;
        self.model.route = Route::Home;
        self
    }

    /// `--shots` builder: render at the narrow breakpoint with the navigation
    /// rail summoned as an open overlay flap, so the offscreen still shows the
    /// collapsed-sidebar state. The live app never calls this.
    pub fn with_open_sidebar_shot(mut self) -> Self {
        self.force_sidebar_open_shot = true;
        self
    }

    /// `--shots` builder: record the render viewport up front so the single
    /// offscreen frame resolves the docked-vs-overlay layout — and thus the
    /// rail's default open/closed state and the width-driven watch player —
    /// from the true width instead of the unknown-width fallback. Without this
    /// the one-and-only build would seed the split from a ZERO width. The live
    /// app never calls it; it learns its width from `apply_layout` each frame.
    pub fn with_viewport(mut self, w: f64, h: f64) -> Self {
        self.rect = Rect::new(0.0, 0.0, w.max(0.0), h.max(0.0));
        self.last_nav_overlay = Some(self.nav_is_overlay());
        self
    }

    /// `--shots` builder: start on a specific [`Route`] so each screen can be
    /// captured for self-inspection (e.g. the selected-nav-item / selected-chip
    /// contrast). Pure presentation override — the live app starts from the
    /// sample/persisted route instead. If the route is a search, the query is
    /// mirrored so the search header + active chip resolve.
    pub fn with_route_shot(mut self, route: Route) -> Self {
        if let Route::Search(q) = &route {
            self.model.search_query = q.clone();
        }
        self.model.route = route;
        self
    }

    /// Test/shots hook: start with the Ctrl+K command palette already open, and
    /// optionally pre-seed its query. Used by headless click-tests (which can't
    /// synthesise the Ctrl+K key event — winit's `KeyEvent` is not constructible
    /// outside the crate) and by the palette `--shots`.
    pub fn with_palette_open_for_test(mut self, query: &str) -> Self {
        self.model.palette_open = true;
        self.model.palette_query = query.to_string();
        self
    }

    /// Test/shots hook: open a watch page with the shared [`VideoPlayer`] already
    /// playing a demo source, so a headless `--shots` still captures the real
    /// player surface (a frame + the transport controls) without a network or a
    /// live decoder. Uses the media engine's synthesized-frame backend, which
    /// seeds a frame on `open` — the exact surface the live decode path paints.
    pub fn with_playing_demo_for_test(mut self) -> Self {
        let id = self
            .model
            .videos
            .first()
            .map(|v| v.id.clone())
            .unwrap_or_else(|| "vid_donut".to_string());
        self.model.route = Route::Watch(id.clone());
        {
            let mut p = self.player.borrow_mut();
            p.open(media::Source::Remote { url: "https://demo.local/clip.mp4".into() });
            // Seed a colourful mid-stream frame for the still, then pump the
            // engine so the frame lands in the surface. The offscreen renderer
            // syncs the app exactly once — *before* this player is mounted in the
            // tree — so without pumping here the single shot would draw an empty
            // (never-pumped) surface. A live run pumps every frame, so this is
            // shots-only.
            p.seek_to_ms(45_000);
            p.play();
            p.reveal_controls();
            let mut doc = automerge::AutoCommit::new();
            let root = automerge::ROOT;
            p.sync(&mut doc, &root);
        }
        self.player_video = Some(id);
        self
    }

    /// `--live-shots` hook: open the shared player on a real, directly-playable
    /// stream `url` and pump the media engine until a real frame decodes
    /// (bounded), so an offscreen still captures the ACTUAL decoded picture — the
    /// same `VideoPlayer` + `media::MediaEngine` the live app streams through.
    /// Needs the `media` decode backend to produce a real frame; if none arrives
    /// within the budget the surface stays as-is (the poster/backdrop shows).
    pub fn with_live_frame_for_shot(mut self, video_id: &str, url: String) -> Self {
        use std::time::{Duration, Instant};
        self.model.route = Route::Watch(video_id.to_string());
        {
            let mut p = self.player.borrow_mut();
            p.open(media::Source::Remote { url });
            p.play();
            p.reveal_controls();
            let mut doc = automerge::AutoCommit::new();
            let root = automerge::ROOT;
            // The offscreen renderer syncs the app only once — before this player
            // is mounted in the tree — so the frame must already be in hand before
            // the shot draws. Poll at the live app's ~16ms cadence: the engine's
            // appsink pull is non-blocking and the sink paces/drops frames to the
            // clock, so a slow poll misses them.
            let pump = |p: &mut ui::widget::VideoPlayer,
                        doc: &mut automerge::AutoCommit,
                        until: &dyn Fn(&ui::widget::VideoPlayer) -> bool,
                        budget: Duration| {
                let deadline = Instant::now() + budget;
                while Instant::now() < deadline {
                    p.sync(doc, &root);
                    if until(p) {
                        return true;
                    }
                    std::thread::sleep(Duration::from_millis(15));
                }
                false
            };
            // Phase 1: preroll — wait for the first decoded frame (proves the
            // pipeline is live and surfaces the real duration).
            pump(
                &mut p,
                &mut doc,
                &|p| p.latest_frame().is_some(),
                Duration::from_secs(25),
            );
            // Phase 2: seek past a typical fade-from-black intro to a colourful
            // scene, then pump until a post-seek frame at/after ~30s is decoded, so
            // the still shows real picture rather than the opening black frame.
            if p.duration_ms() > 40_000 {
                p.seek_to_ms(32_000);
                pump(
                    &mut p,
                    &mut doc,
                    &|p| p.engine().map(|e| e.position_ms()).unwrap_or(0) >= 30_000,
                    Duration::from_secs(15),
                );
            }
        }
        self.player_video = Some(video_id.to_string());
        self
    }

    /// Test accessor: the current route (read-only).
    pub fn route_for_test(&self) -> &Route {
        &self.model.route
    }

    /// Test accessor: whether the command palette is open.
    pub fn palette_open_for_test(&self) -> bool {
        self.model.palette_open
    }

    /// Test accessor: read-only view of the model, so integration tests can
    /// assert on real state (likes, notices, settings, playhead) after driving
    /// the widget tree.
    pub fn model_for_test(&self) -> &TubeModel {
        &self.model
    }

    /// Test accessor: whether the navigation rail is currently shown (resolves
    /// the session preference against the layout default).
    pub fn sidebar_open_for_test(&self) -> bool {
        self.sidebar_currently_open()
    }

    fn build_shell_themed(&self, theme: &Theme) -> Box<dyn Widget> {
        let m = &self.model;
        let bus = &self.bus;
        let imgs = crate::images::Imgs(self.images.as_ref());
        let content = match &m.route {
            Route::Home => screens::home(m, theme, bus, imgs),
            Route::Trending => screens::trending(m, theme, bus, imgs),
            Route::Subscriptions => screens::subscriptions(m, theme, bus, imgs),
            Route::History => screens::history(m, theme, bus, imgs),
            Route::Settings => screens::settings(m, theme, bus),
            Route::Watch(id) => screens::watch(
                m,
                theme,
                bus,
                imgs,
                id,
                self.content_width(),
                self.player.clone(),
            ),
            Route::Channel(id) => screens::channel(m, theme, bus, imgs, id),
            Route::Search(q) => screens::search(m, theme, bus, imgs, q),
        };

        // `fill_w` pins the content pane to a *definite* width (the split lays
        // each pane out in its own tree, so without this the column would size
        // to its max-content and the card grid inside would never learn a line
        // width to wrap against — cards would overflow and clip). With a
        // definite width the wrapping grid reflows to fewer columns as the
        // window narrows.
        let content_area = Flex::col().grow(1.0).fill_w().fill_h().clip().push(content);

        // The navigation rail and the content share the body via a responsive
        // split: docked beside the content on a wide window, collapsed into an
        // overlay flap (behind a dimming scrim, summoned by the top-bar
        // hamburger) on a narrow one. The split self-routes the scrim-click /
        // Esc dismissal and resolves docked-vs-overlay from its own allocated
        // width against `NAV_BREAKPOINT`.
        let mut split = NavSplit::new(
            Box::new(screens::nav_rail(m, theme, bus)),
            Box::new(content_area),
        )
        .with_mode(SplitMode::Adaptive)
        .with_breakpoint(NAV_BREAKPOINT)
        .with_sidebar_width(screens::NAV_RAIL_W);

        // Seed the freshly-built split's open state deterministically. The
        // split is recreated each time the tree is rebuilt (navigation), and
        // the live `ui::App` never drives its eased `update()` tick, so we snap
        // it (no ease) to the state resolved from the current width + the
        // session preference. Without this the recreated split would render
        // from a stale reveal — e.g. the flap drawn open on a fresh narrow
        // window — because nothing advances the animation toward the default.
        let open = if self.force_sidebar_open_shot {
            true
        } else {
            self.sidebar_currently_open()
        };
        split.snap_open(open);

        let mut root = Flex::col()
            .fill_w()
            .fill_h()
            .push(screens::top_bar(m, theme, bus))
            .push(split);

        // The Ctrl+K command palette overlays the whole shell when open.
        if let Some(palette) = screens::command_palette(m, theme, bus) {
            root = root.push(palette.overlay());
        }

        Box::new(root)
    }

    /// Whether the navigation rail is collapsed to an overlay flap at the
    /// current allocated width. Mirrors `SplitLayout::from_width` so the
    /// hamburger toggle and the selection-dismiss resolve the rail's default
    /// the same way the live split does.
    fn nav_is_overlay(&self) -> bool {
        let w = self.rect.width();
        // Width 0 = first build, before the first `apply_layout`. Default to
        // the overlay (narrow) resolution so the rail starts CLOSED when the
        // width is not yet known — never assume the wide layout and flash the
        // flap open over the content. A real width self-corrects on its first
        // laid-out frame: a wide window rebuilds once the true width lands (see
        // `built_overlay` in `apply_layout`).
        w <= 0.0 || w < NAV_BREAKPOINT
    }

    /// Logical width available to the screen content, i.e. the split's content
    /// pane. In overlay mode the flap draws over the content, so the content
    /// spans the full width; docked, an open rail insets the content by
    /// [`screens::NAV_RAIL_W`]. Used to size the width-driven watch player and
    /// pick the watch page's one-vs-two-column arrangement.
    fn content_width(&self) -> f64 {
        let w = self.rect.width();
        if self.nav_is_overlay() {
            w
        } else if self.sidebar_currently_open() {
            (w - screens::NAV_RAIL_W).max(0.0)
        } else {
            w
        }
    }

    /// Resolve whether the rail is currently shown, given the session
    /// preference and the layout default (overlay ⇒ closed, docked ⇒ open).
    fn sidebar_currently_open(&self) -> bool {
        self.sidebar_open
            .unwrap_or(!self.nav_is_overlay())
    }

    /// Close the rail when it is an overlay flap (narrow window) — selecting a
    /// destination should dismiss the summoned menu, the standard flap
    /// behaviour. A no-op while the rail is docked beside the content.
    fn dismiss_overlay_rail(&mut self) {
        if self.nav_is_overlay() {
            self.sidebar_open = Some(false);
        }
    }

    /// Whether any real image (thumbnail / avatar / banner) is still loading, so
    /// the app keeps redrawing/idle-syncing until each decoded image lands.
    fn images_pending(&self) -> bool {
        self.images.as_ref().map(|s| s.pending() > 0).unwrap_or(false)
    }

    /// If the current route is a watch page we haven't started playing yet, ask
    /// the worker to resolve + decode a real stream for it — exactly once per
    /// watch-open. Online-only, so the offline `--shots`/test paths never spawn
    /// the worker or reach the network. Opening a *different* video re-arms this
    /// and drops the previous video's frame so the poster shows until the new
    /// stream decodes.
    fn ensure_playback(&mut self) {
        if !self.online {
            return;
        }
        match self.model.route.clone() {
            Route::Watch(id) => {
                if self.playing_id.as_deref() == Some(id.as_str()) {
                    return;
                }
                self.playing_id = Some(id.clone());
                // A fresh resolution is pending; drop the previously-loaded
                // source so a new video starts clean once its URL lands.
                self.player_video = None;
                self.player.borrow_mut().pause();
                self.request_live(Request::PlayVideo(id));
            }
            // Left the watch page: pause the engine so audio stops and it isn't
            // still streaming a video the user is no longer watching.
            _ => {
                if self.playing_id.take().is_some() {
                    self.player_video = None;
                    self.player.borrow_mut().pause();
                }
            }
        }
    }

    /// Per-frame playback bookkeeping: mirror the engine's playhead onto the
    /// model (so SponsorBlock auto-skip + autoplay-next key off the real
    /// position), auto-skip enabled segments by seeking the engine, and advance
    /// to the next recommendation when the stream ends. Returns `true` only when
    /// something *visible* changed (a skip notice or an autoplay navigation) so
    /// it never dirties the tree merely because the playhead advanced. A no-op
    /// off the watch route or before a stream has loaded.
    fn tick_playback(&mut self) -> bool {
        let Route::Watch(id) = self.model.route.clone() else {
            return false;
        };
        if self.player_video.as_deref() != Some(id.as_str()) {
            return false;
        }
        // Short borrow: read the engine's position/duration/playing, then drop
        // the borrow before any seek (auto-skip re-borrows).
        let (pos_ms, dur_ms, playing) = {
            let p = self.player.borrow();
            (p.position_ms(), p.duration_ms(), p.is_playing())
        };
        self.model.playhead = pos_ms as f64 / 1000.0;
        self.model.playing = playing;

        let mut changed = false;
        // Auto-skip runs against the real playhead and seeks the engine.
        if self.apply_auto_skip() {
            changed = true;
        }
        // On-video "Skip" overlay visibility: recompute which (if any) show-mode
        // segment the playhead sits in. A transition (enter/leave) must rebuild
        // the tree so the overlay appears/dismisses — the advancing playhead by
        // itself never dirties it.
        let show_end = self.current_show_skip_end();
        if show_end != self.show_skip_end {
            self.show_skip_end = show_end;
            changed = true;
        }
        // Reaching the end advances to the next "up next" when autoplay is on.
        // `duration_ms` is the media duration once known; treat within 250ms of
        // the end (or an explicitly non-playing engine at the end) as finished.
        let known_dur = dur_ms > 0;
        let at_end = known_dur && pos_ms + 250 >= dur_ms;
        if at_end {
            if self.maybe_autoplay_next() {
                changed = true;
            }
        }
        changed
    }

    /// Load a resolved, directly-playable stream URL into the shared player
    /// engine and start it — the real path both the worker's
    /// [`Update::StreamResolved`] and the transport tests take. A resolution for
    /// a video that is no longer the open watch page is dropped (returns `false`)
    /// so audio never bleeds across pages.
    fn load_resolved_stream(
        &mut self,
        video: String,
        url: String,
        has_audio: bool,
        quality: String,
    ) -> bool {
        if !matches!(&self.model.route, Route::Watch(id) if *id == video) {
            return false;
        }
        {
            let mut p = self.player.borrow_mut();
            p.open(media::Source::Remote { url });
            p.play();
        }
        self.player_video = Some(video);
        self.model.playing = true;
        // No debug text under the player — the picture playing IS the feedback.
        // Quality/audio provenance is a developer diagnostic, so it goes to the
        // log, not an on-screen toast.
        tracing::debug!(%quality, has_audio, "stream loaded into player");
        true
    }

    /// Resolve the current watch video's duration, if any.
    fn current_duration(&self) -> Option<f64> {
        match &self.model.route {
            Route::Watch(id) => self.model.video(id).map(|v| v.duration as f64),
            _ => None,
        }
    }

    /// The end (seconds) of the show-mode SponsorBlock segment the playhead is
    /// currently inside, if any — what the on-video "Skip" overlay offers.
    /// `None` off the watch route, when SponsorBlock is off, or when no show-mode
    /// segment contains the playhead.
    fn current_show_skip_end(&self) -> Option<f64> {
        if !self.model.settings.sponsorblock_enabled {
            return None;
        }
        let Route::Watch(id) = &self.model.route else {
            return None;
        };
        let v = self.model.video(id)?;
        v.sponsor
            .iter()
            .find(|s| self.model.settings.should_show_skip(s.category) && s.contains(self.model.playhead))
            .map(|s| s.end)
    }

    /// If the playhead sits inside an enabled SponsorBlock segment, jump the
    /// engine (and the model) to its end and log the skip. Returns `true` when a
    /// skip actually happened. Runs both as the user scrubs and each frame from
    /// [`Self::tick_playback`] against the real engine playhead.
    fn apply_auto_skip(&mut self) -> bool {
        if !self.model.settings.sponsorblock_enabled {
            return false;
        }
        let Route::Watch(id) = self.model.route.clone() else {
            return false;
        };
        let (seg_end, label) = {
            let Some(v) = self.model.video(&id) else { return false };
            let mut found = None;
            for s in &v.sponsor {
                // Only Auto-skip-mode categories skip on their own; Show-mode
                // segments wait for the manual skip button.
                if self.model.settings.should_auto_skip(s.category) && s.contains(self.model.playhead) {
                    found = Some((
                        s.end,
                        format!(
                            "Skipped {} ({}–{})",
                            s.category.label(),
                            crate::util::clock(s.start),
                            crate::util::clock(s.end)
                        ),
                    ));
                    break;
                }
            }
            match found {
                Some(f) => f,
                None => return false,
            }
        };
        self.model.playhead = seg_end;
        // Seek the real stream past the segment (no-op when no source is loaded).
        self.player.borrow_mut().seek_to_ms((seg_end * 1000.0) as u64);
        self.model.push_notice(label);
        true
    }

    /// Open a video's watch page: record history, route to it, reset the
    /// playhead and start playing, and kick off metadata/branding fetches.
    /// Shared by the card/palette open paths and by autoplay-next so they stay
    /// in lockstep.
    fn open_video(&mut self, id: String) {
        self.model.record_watch(&id);
        self.model.route = Route::Watch(id.clone());
        self.model.playhead = 0.0;
        self.model.playing = true;
        self.request(Request::Branding(id.clone()));
        // Fetch the uploader's real avatar + subscriber count when we only hold a
        // name stub (feed/search items don't carry the channel's avatar or subs),
        // so the watch-page channel row shows a real photo + count rather than an
        // initials chip and "0 subscribers".
        if let Some(cid) = self.model.video(&id).map(|v| v.channel_id.clone()) {
            self.request_channel_if_stub(&cid);
        }
        self.request_live(Request::VideoMeta(id));
    }

    /// Ask the worker for a channel page (avatar + subscriber count + uploads)
    /// when the store only holds a bare name stub for it. Online-only.
    fn request_channel_if_stub(&mut self, channel_id: &str) {
        if channel_id.is_empty() {
            return;
        }
        let stub = self
            .model
            .channel(channel_id)
            .map(|c| c.avatar_url.is_empty() || c.subscribers == 0)
            .unwrap_or(true);
        if stub {
            self.request_live(Request::Channel(channel_id.to_string()));
        }
    }

    /// If autoplay is on and the playhead has reached the end of the current
    /// video, advance to the first "up next" recommendation — the real code
    /// path that consumes the [`Settings::autoplay`] flag. Reaching the end
    /// happens when the engine's stream runs out (mirrored each frame by
    /// [`Self::tick_playback`]) or when a test seeks the playhead to the finish.
    /// Returns `true` when it navigated to a new video.
    fn maybe_autoplay_next(&mut self) -> bool {
        if !self.model.settings.autoplay {
            return false;
        }
        let Route::Watch(id) = self.model.route.clone() else {
            return false;
        };
        let Some(dur) = self.model.video(&id).map(|v| v.duration as f64) else {
            return false;
        };
        if self.model.playhead < dur - 0.001 {
            return false;
        }
        let next = self
            .model
            .related_display(&id)
            .first()
            .map(|v| v.id.clone());
        if let Some(next) = next {
            if next != id {
                self.open_video(next);
                return true;
            }
        }
        false
    }

    /// Apply one drained action. Returns true if the model changed enough
    /// to warrant a structural rebuild + persist.
    fn apply(&mut self, action: Action, state: &mut AutoCommit, root: &ObjId) -> bool {
        match action {
            Action::ToggleSidebar => {
                // Flip the rail relative to its current resolved openness, so
                // the hamburger always does the visually-expected thing at the
                // current width.
                let now = self.sidebar_currently_open();
                self.sidebar_open = Some(!now);
            }
            Action::Navigate(route) => {
                if let Route::Search(q) = &route {
                    self.model.search_query = q.clone();
                }
                self.model.route = route;
                self.dismiss_overlay_rail();
            }
            Action::OpenVideo(id) => {
                self.open_video(id);
                self.dismiss_overlay_rail();
            }
            Action::OpenChannel(id) => {
                self.model.route = Route::Channel(id.clone());
                self.dismiss_overlay_rail();
                self.request_live(Request::Channel(id));
            }
            Action::ToggleSubscribe(id) => {
                self.model.toggle_subscribe(&id);
            }
            Action::Seek(frac) => {
                if let Some(d) = self.current_duration() {
                    self.model.playhead = (frac * d).clamp(0.0, d);
                    // Seek the real stream (no-op when no source is loaded yet).
                    self.player
                        .borrow_mut()
                        .seek_to_ms((self.model.playhead * 1000.0) as u64);
                    self.apply_auto_skip();
                    self.maybe_autoplay_next();
                }
            }
            Action::TogglePlay => {
                // Drive the real media engine, then mirror its resulting state
                // onto the model so the rest of the UI stays truthful.
                self.player.borrow_mut().toggle_play();
                self.model.playing = self.player.borrow().is_playing();
            }
            Action::ToggleCategory(c) => {
                self.model.settings.cycle_category(c);
            }
            Action::SkipSegment(to) => {
                // Manual skip button for a Show-mode segment: jump the model +
                // engine playhead to the segment end and confirm with a toast.
                if let Some(d) = self.current_duration() {
                    let to = to.clamp(0.0, d);
                    self.model.playhead = to;
                    self.player.borrow_mut().seek_to_ms((to * 1000.0) as u64);
                    self.model.push_notice(format!("Skipped to {}", crate::util::clock(to)));
                }
            }
            Action::ToggleSetting(flag) => {
                let s = &mut self.model.settings;
                match flag {
                    SettingFlag::SponsorBlock => s.sponsorblock_enabled = !s.sponsorblock_enabled,
                    SettingFlag::DeArrow => s.dearrow_enabled = !s.dearrow_enabled,
                    SettingFlag::DeArrowTitles => s.dearrow_titles = !s.dearrow_titles,
                    SettingFlag::DeArrowThumbnails => s.dearrow_thumbnails = !s.dearrow_thumbnails,
                    SettingFlag::ReturnDislike => {
                        s.return_dislike_enabled = !s.return_dislike_enabled
                    }
                    SettingFlag::Autoplay => s.autoplay = !s.autoplay,
                    SettingFlag::ShowComments => s.show_comments = !s.show_comments,
                }
            }
            Action::SetQuality(_) => {
                let cur = self.model.settings.preferred_quality.clone();
                let idx = QUALITIES.iter().position(|q| *q == cur).unwrap_or(0);
                self.model.settings.preferred_quality = QUALITIES[(idx + 1) % QUALITIES.len()].to_string();
            }
            Action::CycleInstance => {
                let cur = self.model.settings.invidious_instance.clone();
                let idx = INSTANCES.iter().position(|i| *i == cur).unwrap_or(0);
                self.model.settings.invidious_instance =
                    INSTANCES[(idx + 1) % INSTANCES.len()].to_string();
            }
            Action::SubmitSearch => {
                let q = self.read_search(state, root);
                self.model.search_query = q.clone();
                self.model.route = Route::Search(q.clone());
                if !q.is_empty() {
                    let instance = self.model.settings.invidious_instance.clone();
                    // Show the results skeleton while the live fetch is in flight
                    // (online only — the hermetic path resolves locally at once).
                    if self.online {
                        self.model.search_loading = true;
                    }
                    self.request(Request::Search { instance, query: q });
                }
            }
            Action::ToggleLike(id) => {
                // A real, visible toggle: the watch page's like pill reads
                // `is_liked` for its active fill and `display_likes` for the
                // count, so this flips both.
                self.model.toggle_like(&id);
            }
            Action::Share(id) => {
                // Copy the canonical shareable link to the system clipboard —
                // the real action — and drop a toast so the (otherwise
                // invisible) clipboard write has visible confirmation.
                let url = format!("https://youtu.be/{id}");
                ui::clipboard_copy(&url);
                self.model.push_notice(format!("Link copied: {url}"));
            }
            Action::OpenPalette => {
                self.model.palette_open = true;
            }
            Action::ClosePalette => {
                self.model.palette_open = false;
            }
            Action::ActivateHit(link) => {
                use crate::search_type::SearchHit;
                if let Some(id) = SearchHit::parse_watch_link(&link) {
                    self.open_video(id);
                } else if let Some(id) = SearchHit::parse_channel_link(&link) {
                    self.model.route = Route::Channel(id.clone());
                    self.request_live(Request::Channel(id));
                }
                self.model.palette_open = false;
                self.dismiss_overlay_rail();
            }
        }
        true
    }

    fn read_search(&self, state: &AutoCommit, root: &ObjId) -> String {
        if let Some(obj) = &self.search_obj {
            if let Ok(t) = state.text(obj) {
                return t;
            }
        }
        if let Ok(Some((Value::Scalar(s), _))) = state.get(root, SEARCH_KEY) {
            if let ScalarValue::Str(t) = s.as_ref() {
                return t.to_string();
            }
        }
        String::new()
    }

    fn request(&mut self, req: Request) {
        if self.worker.is_none() {
            self.worker = Some(Worker::spawn());
        }
        if let Some(w) = &self.worker {
            w.request(req);
        }
    }

    /// Request live InnerTube data (feed / video detail / channel), but only
    /// when this instance is online. The offline `--shots` and test paths call
    /// through the same navigation code, so gating here keeps them from ever
    /// spawning the worker or reaching the network.
    fn request_live(&mut self, req: Request) {
        if self.online {
            self.request(req);
        }
    }

    /// Fold any worker results into the model. Returns true if anything
    /// changed (→ rebuild).
    fn pump(&mut self) -> bool {
        let Some(worker) = &self.worker else {
            return false;
        };
        let updates = worker.poll();
        if updates.is_empty() {
            return false;
        }
        let mut changed = false;
        // Channels whose real avatar + subscriber count we still need to fetch
        // (a video-detail response tells us the uploader id but not its photo).
        // Collected here and fired after the borrow of `self.worker` ends.
        let mut fetch_channels: Vec<String> = Vec::new();
        for u in updates {
            match u {
                Update::Sponsor(id, segs) => {
                    if let Some(v) = self.model.videos.iter_mut().find(|v| v.id == id) {
                        v.sponsor = segs;
                        changed = true;
                    }
                }
                Update::DeArrow { video, title, thumbnail_url } => {
                    if let Some(v) = self.model.videos.iter_mut().find(|v| v.id == video) {
                        if title.is_some() {
                            v.dearrow_title = title;
                        }
                        v.dearrow_thumb = thumbnail_url.is_some();
                        changed = true;
                    }
                    if let Some(url) = thumbnail_url {
                        if !url.is_empty() {
                            self.model.dearrow_thumb_url.insert(video, url);
                            changed = true;
                        }
                    }
                }
                Update::Dislikes { video, count } => {
                    self.model.dislikes.insert(video, count);
                    changed = true;
                }
                Update::Comments { video, comments } => {
                    if let Some(v) = self.model.videos.iter_mut().find(|v| v.id == video) {
                        v.comments = comments;
                        changed = true;
                    }
                }
                Update::SearchResults(query, vids) => {
                    // A response landed (possibly empty) — drop the skeleton.
                    self.model.search_loading = false;
                    // Record the backend's exact ordered result set for this
                    // query (so the search screen renders it verbatim), and
                    // fold the videos + uploader stubs into the catalogue.
                    let ids: Vec<String> = vids.iter().map(|v| v.id.clone()).collect();
                    for nv in vids {
                        self.model.ensure_channel_stub(&nv.channel_id, &nv.channel_name);
                        self.model.upsert_video(nv);
                    }
                    self.model.search_hits_query = query.clone();
                    self.model.search_hits = ids;
                    if matches!(&self.model.route, Route::Search(_)) {
                        self.model.route = Route::Search(query);
                    }
                    changed = true;
                }
                Update::Feed(vids) => {
                    // Real home/trending catalogue — replace whatever was there.
                    self.model.adopt_live_catalogue(vids, "live (rustypipe)");
                    self.model.feed_loading = false;
                    changed = true;
                }
                Update::VideoMeta { video, related } => {
                    let id = video.id.clone();
                    let channel_id = video.channel_id.clone();
                    self.model.ensure_channel_stub(&video.channel_id, &video.channel_name);
                    self.model.upsert_video(*video);
                    // Now that we know the uploader, pull its avatar + subs if we
                    // only have a name stub (drives the watch-page channel row).
                    if !channel_id.is_empty()
                        && self
                            .model
                            .channel(&channel_id)
                            .map(|c| c.avatar_url.is_empty() || c.subscribers == 0)
                            .unwrap_or(true)
                    {
                        fetch_channels.push(channel_id);
                    }
                    let ids: Vec<String> = related.iter().map(|v| v.id.clone()).collect();
                    for rv in related {
                        self.model.ensure_channel_stub(&rv.channel_id, &rv.channel_name);
                        self.model.upsert_video(rv);
                    }
                    if !ids.is_empty() {
                        self.model.related_of = id;
                        self.model.related_hits = ids;
                    }
                    changed = true;
                }
                Update::ChannelPage { channel, uploads } => {
                    self.model.upsert_channel(*channel);
                    for uv in uploads {
                        self.model.ensure_channel_stub(&uv.channel_id, &uv.channel_name);
                        self.model.upsert_video(uv);
                    }
                    changed = true;
                }
                Update::Status { live, label } => {
                    // A terminal offline/failure status means no live feed is
                    // coming — stop showing the Home skeleton and let the empty
                    // state explain it. "connecting…" keeps the skeleton up.
                    if !live && label.to_lowercase().contains("offline") {
                        self.model.feed_loading = false;
                        self.model.search_loading = false;
                    }
                    if self.model.data_source.live != live || self.model.data_source.label != label {
                        self.model.data_source = crate::model::DataSource { live, label };
                        changed = true;
                    }
                }
                Update::StreamResolved { video, url, has_audio, quality } => {
                    if self.load_resolved_stream(video, url, has_audio, quality) {
                        changed = true;
                    }
                }
                Update::Note(n) => {
                    // Worker status notes (backend ready, stream host, seed
                    // failures, …) are developer diagnostics — log them, don't
                    // clutter the watch page with debug toasts. Genuine
                    // user-facing feedback (auto-skip, link copied) is pushed
                    // as a notice directly from its action, not via `Note`.
                    tracing::debug!(note = %n, "tube worker note");
                }
            }
        }
        // Fire the deferred channel fetches now the `self.worker` borrow is done.
        for cid in fetch_channels {
            self.request_channel_if_stub(&cid);
        }
        changed
    }

    fn persist(&self, state: &mut AutoCommit, root: &ObjId) {
        if let Ok(json) = serde_json::to_string(&self.model) {
            let _ = state.put(root, STATE_KEY, json);
        }
    }

    fn ensure_search_obj(&mut self, state: &mut AutoCommit, root: &ObjId) {
        if self.search_obj.is_none() {
            match state.get(root, SEARCH_KEY) {
                Ok(Some((Value::Object(ObjType::Text), id))) => self.search_obj = Some(id),
                _ => {
                    if let Ok(id) = state.put_object(root, SEARCH_KEY, ObjType::Text) {
                        self.search_obj = Some(id);
                    }
                }
            }
        }
        if self.palette_obj.is_none() {
            match state.get(root, PALETTE_KEY) {
                Ok(Some((Value::Object(ObjType::Text), id))) => self.palette_obj = Some(id),
                _ => {
                    if let Ok(id) = state.put_object(root, PALETTE_KEY, ObjType::Text) {
                        self.palette_obj = Some(id);
                    }
                }
            }
        }
    }

    /// Read the live palette-query text out of its bound CRDT object.
    fn read_palette(&self, state: &AutoCommit) -> String {
        if let Some(obj) = &self.palette_obj {
            if let Ok(t) = state.text(obj) {
                return t;
            }
        }
        String::new()
    }

    /// First-sync: adopt persisted state if present, else seed ours.
    fn adopt_or_seed(&mut self, state: &mut AutoCommit, root: &ObjId) {
        if self.seeded {
            return;
        }
        self.seeded = true;
        if let Ok(Some((Value::Scalar(s), _))) = state.get(root, STATE_KEY) {
            if let ScalarValue::Str(json) = s.as_ref() {
                if let Ok(m) = serde_json::from_str::<TubeModel>(json) {
                    self.model = m;
                    return;
                }
            }
        }
        self.persist(state, root);
    }
}

impl Widget for TubeApp {
    fn sync(&mut self, state: &mut AutoCommit, path: &ObjId) {
        self.ensure_search_obj(state, path);
        self.adopt_or_seed(state, path);
        // First live frame: spawn the fetch worker and pull the home/trending
        // feed. One-shot (guarded) and online-only, so the offscreen path never
        // reaches here with `online` set.
        if self.online && !self.kicked_off {
            self.kicked_off = true;
            self.request_live(Request::Feed);
        }
        // A persisted/boot route that is already a watch page starts playing on
        // the first live sync (guarded so it fires once).
        self.ensure_playback();
        // Reflect the live palette-query text into the model so the overlay
        // filters as the user types. Only adopt the CRDT text once the user has
        // actually typed (non-empty), so a pre-seeded query (tests / a future
        // "search this" deep-link) isn't clobbered by the empty text object on
        // the first frame before the bound input has written anything.
        if self.model.palette_open {
            let live = self.read_palette(state);
            if !live.is_empty() && live != self.model.palette_query {
                self.model.palette_query = live;
                self.structure_dirty = true;
            }
        }
        if let Some(c) = &mut self.root_child {
            c.sync(state, path);
        }
        // The watch page's player pumped its engine in the sync above; now mirror
        // the engine playhead onto the model and run SponsorBlock auto-skip /
        // autoplay-next against the real position. Only flags a rebuild when a
        // skip notice or an autoplay navigation actually happens (not every
        // advancing frame), so a playing video repaints without churning the tree.
        if self.tick_playback() {
            self.structure_dirty = true;
        }
    }

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        // Remember which layout resolution this tree was seeded with, so a
        // later `apply_layout` at the true width can detect a wrong seed (the
        // first build runs before any width is known) and rebuild once.
        self.built_overlay = Some(self.nav_is_overlay());
        let shell = self.build_shell_themed(theme);
        self.root_child = Some(shell);
        let node = self
            .root_child
            .as_mut()
            .unwrap()
            .build_layout(taffy, theme, node_map);
        self.child_node = Some(node);
        node
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        let layout = taffy.layout(node).unwrap();
        self.rect = Rect::new(
            origin.x + layout.location.x as f64,
            origin.y + layout.location.y as f64,
            origin.x + layout.location.x as f64 + layout.size.width as f64,
            origin.y + layout.location.y as f64 + layout.size.height as f64,
        );
        if let Some(c) = &mut self.root_child {
            c.apply_layout(taffy, node, origin);
        }

        // Detect a breakpoint crossing on resize (which does not otherwise
        // dirty the tree). On a crossing, drop a stale per-session preference
        // so the rail adopts the new layout's default, and request a rebuild
        // so the split is re-seeded at the new width.
        if self.rect.width() > 0.0 {
            let overlay = self.nav_is_overlay();
            if self.last_nav_overlay.is_some() && self.last_nav_overlay != Some(overlay) {
                self.sidebar_open = None;
                self.structure_dirty = true;
            }
            self.last_nav_overlay = Some(overlay);
            // The first build ran before the width was known and seeded the
            // tree from the unknown-width (overlay) default. Now that the true
            // width has landed, rebuild if the resolved layout differs from
            // what the current tree was built with — this re-seeds a wide
            // window's rail open. It converges in one frame (the rebuilt tree
            // is seeded from the now-known width) and never loops.
            if self.built_overlay != Some(overlay) {
                self.structure_dirty = true;
            }
        }
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        scene.fill(
            vello::peniko::Fill::NonZero,
            transform,
            theme.tokens.colors.surfaces.base,
            None,
            &self.rect,
        );
        if let Some(c) = &mut self.root_child {
            c.draw(scene, transform, text_renderer, theme, presence);
        }
    }

    fn draw_with_presence(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
        permissions: &PermissionContext,
    ) {
        scene.fill(
            vello::peniko::Fill::NonZero,
            transform,
            theme.tokens.colors.surfaces.base,
            None,
            &self.rect,
        );
        if let Some(c) = &mut self.root_child {
            c.draw_with_presence(scene, transform, text_renderer, theme, presence, permissions);
        }
    }

    fn handle_event(
        &mut self,
        event: &WindowEvent,
        scale_factor: f64,
        modifiers: &winit::event::Modifiers,
        state: &mut AutoCommit,
        path: &ObjId,
        handled: &mut bool,
        focus_target: &mut Option<NodeId>,
        node_id: NodeId,
        taffy: &TaffyTree<()>,
    ) {
        // Ctrl+K toggles the global command palette, regardless of focus. This
        // is intercepted before children so a focused text field can't swallow
        // it. `Ctrl+K` opens; a second `Ctrl+K` (or Esc, below) closes.
        if let WindowEvent::KeyboardInput {
            event:
                KeyEvent {
                    logical_key: Key::Character(c),
                    state: ElementState::Pressed,
                    ..
                },
            ..
        } = event
        {
            if modifiers.state().control_key() && c.eq_ignore_ascii_case("k") {
                action::push(
                    &self.bus,
                    if self.model.palette_open {
                        Action::ClosePalette
                    } else {
                        Action::OpenPalette
                    },
                );
                *handled = true;
            }
        }

        // Enter submits a search regardless of which field has focus.
        if let WindowEvent::KeyboardInput {
            event:
                KeyEvent {
                    logical_key: Key::Named(NamedKey::Enter),
                    state: ElementState::Pressed,
                    ..
                },
            ..
        } = event
        {
            if !self.model.search_query.is_empty() || self.search_obj.is_some() {
                action::push(&self.bus, Action::SubmitSearch);
            }
        }

        if let Some(c) = &mut self.root_child {
            c.handle_event(event, scale_factor, modifiers, state, path, handled, focus_target, node_id, taffy);
        }

        // Esc dismisses an open overlay flap. The live split closes itself on
        // Esc / scrim-click; mirror that into our session preference so the
        // next structural rebuild re-seeds the rail closed rather than
        // snapping it back open.
        if let WindowEvent::KeyboardInput {
            event:
                KeyEvent {
                    logical_key: Key::Named(NamedKey::Escape),
                    state: ElementState::Pressed,
                    ..
                },
            ..
        } = event
        {
            // Esc closes the command palette first (it's the topmost overlay).
            if self.model.palette_open {
                action::push(&self.bus, Action::ClosePalette);
                *handled = true;
            } else if self.nav_is_overlay() && self.sidebar_currently_open() {
                self.sidebar_open = Some(false);
            }
        }

        // Drain & apply queued actions.
        let actions = action::drain(&self.bus);
        let mut changed = false;
        for a in actions {
            changed |= self.apply(a, state, path);
        }
        // If a just-applied action opened a watch page, kick off real playback
        // (resolve + decode) for it — once. No-op off the watch route / offline.
        self.ensure_playback();
        // Fold any ready worker results (live data + decoded frames).
        changed |= self.pump();

        if changed {
            self.persist(state, path);
            self.structure_dirty = true;
        }
    }

    fn on_external_event(&mut self, _payload: &str, _state: &mut AutoCommit, _root: &ObjId) {}

    fn take_structure_dirty(&mut self) -> bool {
        let mut dirty = std::mem::take(&mut self.structure_dirty);
        // Fold any results that arrived between events so live data still
        // lands when the user is idle.
        if self.pump() {
            dirty = true;
        }
        if let Some(c) = &mut self.root_child {
            dirty |= c.take_structure_dirty();
        }
        dirty
    }

    fn needs_redraw(&self) -> bool {
        self.structure_dirty
            || self.images_pending()
            || self
                .root_child
                .as_ref()
                .map(|c| c.needs_redraw())
                .unwrap_or(true)
    }

    fn mark_drawn(&mut self) {
        if let Some(c) = &mut self.root_child {
            c.mark_drawn();
        }
    }

    fn is_layout_dirty(&self) -> bool {
        self.root_child
            .as_ref()
            .map(|c| c.is_layout_dirty())
            .unwrap_or(true)
    }

    fn needs_idle_sync(&self) -> bool {
        // Keep syncing while a fetch is outstanding so results fold in, and
        // while images are still loading so freshly-decoded thumbnails/avatars
        // repaint the moment they land.
        self.worker.is_some() || self.images_pending()
    }

    fn needs_animation_frame(&self) -> bool {
        // Keep the event loop pumping redraws (and thus `sync`, which pumps the
        // media engine) across two windows: while a watch stream is resolving
        // (so the worker's `StreamResolved` folds in) and while the player is
        // decoding (so the picture MOVES). winit's default `Wait` only wakes on
        // input; the player's own `needs_animation_frame` covers the playing
        // case via `root_child`, and this `streaming` gate covers the resolve
        // gap before a source is loaded. Redraws are vsync-throttled by the
        // renderer, so this paces to the display, not a hot spin.
        let streaming = self.online
            && matches!(&self.model.route, Route::Watch(id) if self.playing_id.as_deref() == Some(id.as_str()));
        streaming
            || self.images_pending()
            || self
                .root_child
                .as_ref()
                .map(|c| c.needs_animation_frame())
                .unwrap_or(false)
    }

    fn children(&self) -> Vec<&dyn Widget> {
        self.root_child.as_ref().map(|c| c.as_ref() as &dyn Widget).into_iter().collect()
    }

    fn children_mut(&mut self) -> Vec<&mut (dyn Widget + '_)> {
        self.root_child
            .as_mut()
            .map(|c| c.as_mut() as &mut (dyn Widget + '_))
            .into_iter()
            .collect()
    }

    fn name(&self) -> &str {
        "TubeApp"
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Category;

    fn fresh() -> (TubeApp, AutoCommit) {
        (TubeApp::new(), AutoCommit::new())
    }

    /// A fresh app whose player uses the deterministic **synthesized** media
    /// engine, so the transport-wiring tests behave identically whether or not
    /// the `media` (real GStreamer) backend is compiled in — a real pipeline
    /// can't preroll/seek the fake test URLs, so its position would stay 0.
    fn fresh_synth() -> (TubeApp, AutoCommit) {
        let (app, doc) = fresh();
        app.player
            .borrow_mut()
            .set_engine(media::MediaEngine::new_synthesized());
        (app, doc)
    }

    #[test]
    fn open_video_navigates_and_records_history() {
        let (mut app, mut doc) = fresh();
        let r = automerge::ROOT;
        app.apply(Action::OpenVideo("vid_donut".into()), &mut doc, &r);
        assert_eq!(app.model.route, Route::Watch("vid_donut".into()));
        assert_eq!(app.model.history.first().map(|s| s.as_str()), Some("vid_donut"));
        assert_eq!(app.model.playhead, 0.0);
    }

    #[test]
    fn subscribe_toggles_via_action() {
        let (mut app, mut doc) = fresh();
        let r = automerge::ROOT;
        assert!(!app.model.is_subscribed("ch_synth"));
        app.apply(Action::ToggleSubscribe("ch_synth".into()), &mut doc, &r);
        assert!(app.model.is_subscribed("ch_synth"));
    }

    // --- media-engine transport wiring (the real playback path) ---

    #[test]
    fn resolved_stream_opens_and_plays_the_engine() {
        // The worker's `StreamResolved` path (factored into `load_resolved_stream`)
        // must open the rangeable URL into the shared player's media engine and
        // start it — the exact code path the live app runs.
        let (mut app, _doc) = fresh_synth();
        app.model.route = Route::Watch("vid_donut".into());
        let ok = app.load_resolved_stream(
            "vid_donut".into(),
            "https://inv.example/latest_version?id=vid_donut&itag=18&local=true".into(),
            true,
            "360p".into(),
        );
        assert!(ok, "the resolved stream loaded for the open watch video");
        assert!(app.player.borrow().is_playing(), "the engine is playing the stream");
        let src = app.player.borrow().engine().and_then(|e| e.source().cloned());
        match src {
            Some(media::Source::Remote { url }) => {
                assert!(url.contains("local=true"), "opened the rangeable proxy URL: {url}")
            }
            other => panic!("expected a remote source on the engine, got {other:?}"),
        }
    }

    #[test]
    fn resolved_stream_for_a_different_video_is_dropped() {
        // A late resolution for a since-closed video must not hijack the engine.
        let (mut app, _doc) = fresh_synth();
        app.model.route = Route::Watch("vid_donut".into());
        let ok = app.load_resolved_stream(
            "some_other_id".into(),
            "https://inv.example/x?local=true".into(),
            true,
            "360p".into(),
        );
        assert!(!ok, "a stream for a non-open video is dropped");
        assert!(app.player.borrow().engine().and_then(|e| e.source()).is_none());
    }

    #[test]
    fn toggle_play_drives_the_engine() {
        let (mut app, mut doc) = fresh_synth();
        let r = automerge::ROOT;
        app.model.route = Route::Watch("vid_donut".into());
        app.load_resolved_stream(
            "vid_donut".into(),
            "https://inv.example/x?local=true".into(),
            true,
            "360p".into(),
        );
        assert!(app.player.borrow().is_playing());
        app.apply(Action::TogglePlay, &mut doc, &r);
        assert!(!app.player.borrow().is_playing(), "TogglePlay paused the engine");
        assert!(!app.model.playing, "the model mirrors the paused engine");
        app.apply(Action::TogglePlay, &mut doc, &r);
        assert!(app.player.borrow().is_playing(), "TogglePlay resumed the engine");
        assert!(app.model.playing, "the model mirrors the playing engine");
    }

    #[test]
    fn auto_skip_seeks_the_engine_past_a_sponsor_segment() {
        // Auto-skip runs against the real playhead and must seek the engine (not
        // just the model). vid_rust_async has a Sponsor segment 1440..1570.
        let (mut app, _doc) = fresh_synth();
        app.model.route = Route::Watch("vid_rust_async".into());
        app.load_resolved_stream(
            "vid_rust_async".into(),
            "https://inv.example/x?local=true".into(),
            true,
            "360p".into(),
        );
        // Playhead inside the sponsor band.
        app.model.playhead = 1500.0;
        let skipped = app.apply_auto_skip();
        assert!(skipped, "a skip happened");
        assert!(app.model.playhead >= 1570.0, "model playhead jumped past the segment");
        // The seek reached the engine (its position advanced off the start). The
        // synthesized test engine caps its duration, so the exact target clamps —
        // proving it moved is the honest real-path assertion.
        let engine_pos = app.player.borrow().engine().map(|e| e.position_ms()).unwrap_or(0);
        assert!(engine_pos > 0, "the auto-skip seeked the engine (pos={engine_pos})");
        assert!(app.model.notices.iter().any(|n| n.contains("Sponsor")));
    }

    #[test]
    fn seek_into_sponsor_auto_skips_to_segment_end() {
        let (mut app, mut doc) = fresh();
        let r = automerge::ROOT;
        // vid_rust_async has a Sponsor segment 1440..1570.
        app.apply(Action::OpenVideo("vid_rust_async".into()), &mut doc, &r);
        let dur = app.model.video("vid_rust_async").unwrap().duration as f64;
        // Seek to ~1500s (inside the sponsor segment).
        app.apply(Action::Seek(1500.0 / dur), &mut doc, &r);
        // Auto-skip should jump the playhead to the segment end (1570).
        assert!(app.model.playhead >= 1570.0, "playhead was {}", app.model.playhead);
        assert!(app.model.notices.iter().any(|l| l.contains("Sponsor")));
    }

    #[test]
    fn autoplay_advances_to_next_when_the_video_ends() {
        // Autoplay is on by default; seeking to the very end must advance to
        // the first "up next" recommendation — the real code path that
        // consumes the autoplay flag.
        let (mut app, mut doc) = fresh();
        let r = automerge::ROOT;
        assert!(app.model.settings.autoplay, "autoplay defaults on");
        app.apply(Action::OpenVideo("vid_rust_async".into()), &mut doc, &r);
        app.apply(Action::Seek(1.0), &mut doc, &r);
        match app.route_for_test() {
            Route::Watch(id) => assert_ne!(
                id, "vid_rust_async",
                "autoplay advanced to a different video"
            ),
            other => panic!("expected a watch route, got {other:?}"),
        }
    }

    #[test]
    fn autoplay_off_parks_at_the_end_without_advancing() {
        let (mut app, mut doc) = fresh();
        let r = automerge::ROOT;
        app.apply(Action::ToggleSetting(SettingFlag::Autoplay), &mut doc, &r);
        assert!(!app.model.settings.autoplay);
        app.apply(Action::OpenVideo("vid_rust_async".into()), &mut doc, &r);
        app.apply(Action::Seek(1.0), &mut doc, &r);
        assert_eq!(
            app.route_for_test(),
            &Route::Watch("vid_rust_async".into()),
            "with autoplay off, reaching the end stays on the same video"
        );
    }

    #[test]
    fn like_toggles_and_share_copies_a_link() {
        let (mut app, mut doc) = fresh();
        let r = automerge::ROOT;
        assert!(!app.model.is_liked("vid_donut"));
        app.apply(Action::ToggleLike("vid_donut".into()), &mut doc, &r);
        assert!(app.model.is_liked("vid_donut"), "ToggleLike likes the video");
        app.apply(Action::ToggleLike("vid_donut".into()), &mut doc, &r);
        assert!(!app.model.is_liked("vid_donut"), "ToggleLike again unlikes it");

        app.apply(Action::Share("vid_donut".into()), &mut doc, &r);
        assert!(
            !app.model.is_liked("vid_donut"),
            "Share must not like the video"
        );
        assert!(
            app.model.notices.iter().any(|n| n.contains("vid_donut")),
            "Share records the copied link as a notice"
        );
    }

    #[test]
    fn cycle_instance_moves_to_a_different_source() {
        let (mut app, mut doc) = fresh();
        let r = automerge::ROOT;
        let before = app.model.settings.invidious_instance.clone();
        app.apply(Action::CycleInstance, &mut doc, &r);
        assert_ne!(before, app.model.settings.invidious_instance);
    }

    #[test]
    fn cycling_category_walks_the_three_modes() {
        use crate::model::SkipMode;
        let (mut app, mut doc) = fresh();
        let r = automerge::ROOT;
        assert_eq!(app.model.settings.mode_of(Category::Intro), SkipMode::Skip);
        app.apply(Action::ToggleCategory(Category::Intro), &mut doc, &r);
        assert_eq!(app.model.settings.mode_of(Category::Intro), SkipMode::Show);
        app.apply(Action::ToggleCategory(Category::Intro), &mut doc, &r);
        assert_eq!(app.model.settings.mode_of(Category::Intro), SkipMode::Disabled);
        app.apply(Action::ToggleCategory(Category::Intro), &mut doc, &r);
        assert_eq!(app.model.settings.mode_of(Category::Intro), SkipMode::Skip);
    }

    #[test]
    fn show_mode_segment_does_not_auto_skip_but_manual_skip_seeks() {
        use crate::model::SkipMode;
        let (mut app, _doc) = fresh_synth();
        app.model.route = Route::Watch("vid_rust_async".into());
        app.load_resolved_stream(
            "vid_rust_async".into(),
            "https://inv.example/x?local=true".into(),
            true,
            "360p".into(),
        );
        // Put the Sponsor category into Show mode, then park inside its segment.
        app.model
            .settings
            .categories
            .insert(Category::Sponsor.api_key().to_string(), SkipMode::Show);
        app.model.playhead = 1500.0;
        assert!(!app.apply_auto_skip(), "Show mode must NOT auto-skip");
        assert_eq!(app.model.playhead, 1500.0, "playhead untouched by auto-skip");
        // The manual skip button jumps to the segment end.
        let mut doc = AutoCommit::new();
        let r = automerge::ROOT;
        app.apply(Action::SkipSegment(1570.0), &mut doc, &r);
        assert!(app.model.playhead >= 1570.0, "manual skip advanced the playhead");
    }

    #[test]
    fn settings_persist_round_trips_through_crdt() {
        let (mut app, mut doc) = fresh();
        let r = automerge::ROOT;
        app.apply(Action::ToggleSetting(SettingFlag::DeArrow), &mut doc, &r);
        let dearrow_after = app.model.settings.dearrow_enabled;
        app.persist(&mut doc, &r);

        // A fresh app adopts the persisted state.
        let mut app2 = TubeApp::new();
        app2.adopt_or_seed(&mut doc, &r);
        assert_eq!(app2.model.settings.dearrow_enabled, dearrow_after);
    }

    #[test]
    fn quality_cycles() {
        let (mut app, mut doc) = fresh();
        let r = automerge::ROOT;
        let q0 = app.model.settings.preferred_quality.clone();
        app.apply(Action::SetQuality(q0.clone()), &mut doc, &r);
        assert_ne!(q0, app.model.settings.preferred_quality);
    }

    // --- responsive navigation rail ---

    /// Pretend the app was allocated `w` logical px wide (as `apply_layout`
    /// would record), so width-driven layout decisions can be exercised.
    fn at_width(w: f64) -> TubeApp {
        let mut app = TubeApp::new();
        app.rect = Rect::new(0.0, 0.0, w, 800.0);
        app
    }

    #[test]
    fn rail_is_docked_wide_and_overlay_narrow() {
        assert!(!at_width(1280.0).nav_is_overlay(), "wide window docks the rail");
        assert!(at_width(NAV_BREAKPOINT).nav_is_overlay() == false, "breakpoint is inclusive → docked");
        assert!(at_width(NAV_BREAKPOINT - 1.0).nav_is_overlay(), "below breakpoint overlays the rail");
    }

    #[test]
    fn fresh_unknown_width_defaults_closed() {
        // Before the first laid-out frame the width is unknown (rect ZERO).
        // The rail must NOT assume the wide/docked layout and flash open over
        // the content — it defaults to the overlay resolution (closed).
        let app = TubeApp::new();
        assert_eq!(app.rect.width(), 0.0, "a fresh app has no measured width yet");
        assert!(app.nav_is_overlay(), "unknown width resolves to overlay, not docked");
        assert!(
            !app.sidebar_currently_open(),
            "with the width unknown the rail defaults closed (never assume wide)"
        );
    }

    #[test]
    fn shot_viewport_seeds_layout_before_first_frame() {
        // The `--shots` viewport hint lets the single offscreen frame resolve
        // the layout from the true width up front: wide docks + opens, narrow
        // overlays + closes.
        assert!(
            TubeApp::new().with_viewport(1280.0, 720.0).sidebar_currently_open(),
            "a wide shot viewport docks and opens the rail"
        );
        assert!(
            !TubeApp::new().with_viewport(560.0, 720.0).sidebar_currently_open(),
            "a narrow shot viewport overlays and closes the rail"
        );
    }

    #[test]
    fn rail_default_open_when_docked_closed_when_overlay() {
        assert!(at_width(1280.0).sidebar_currently_open(), "docked rail shows by default");
        assert!(!at_width(560.0).sidebar_currently_open(), "overlay rail hides by default");
    }

    #[test]
    fn toggle_summons_overlay_rail() {
        let (mut app, mut doc) = (at_width(560.0), AutoCommit::new());
        let r = automerge::ROOT;
        assert!(!app.sidebar_currently_open(), "narrow rail starts closed");
        app.apply(Action::ToggleSidebar, &mut doc, &r);
        assert!(app.sidebar_currently_open(), "hamburger summons the overlay rail");
        app.apply(Action::ToggleSidebar, &mut doc, &r);
        assert!(!app.sidebar_currently_open(), "a second toggle dismisses it");
    }

    #[test]
    fn selecting_a_destination_dismisses_overlay_rail() {
        let (mut app, mut doc) = (at_width(560.0), AutoCommit::new());
        let r = automerge::ROOT;
        app.apply(Action::ToggleSidebar, &mut doc, &r); // open it
        assert!(app.sidebar_currently_open());
        app.apply(Action::Navigate(Route::Trending), &mut doc, &r);
        assert!(!app.sidebar_currently_open(), "navigating closes the overlay flap");
    }

    #[test]
    fn docked_rail_is_not_dismissed_on_navigation() {
        let (mut app, mut doc) = (at_width(1280.0), AutoCommit::new());
        let r = automerge::ROOT;
        app.apply(Action::Navigate(Route::History), &mut doc, &r);
        assert!(app.sidebar_currently_open(), "the docked rail stays put when navigating");
    }

    // --- no fabricated content + loading states ---

    #[test]
    fn online_starts_empty_with_no_fabricated_sample() {
        // The real (online) app must NOT present the bundled sample catalogue as
        // if it were real data — it starts empty and shows a loading skeleton.
        let app = TubeApp::new().online();
        assert!(app.model.videos.is_empty(), "online start must seed no sample videos");
        assert!(app.model.channels.is_empty(), "online start must seed no sample channels");
        assert!(app.model.subscriptions.is_empty());
        assert!(app.model.feed_loading, "cold Home shows a loading skeleton, not a blank page");
    }

    #[test]
    fn submitting_a_search_enters_the_loading_state() {
        use automerge::transaction::Transactable;
        // A live search in flight shows a skeleton (search_loading) rather than a
        // premature "No results" — the bug that shipped.
        let mut app = TubeApp::new();
        app.online = true; // online without spawning the image store's threads
        let mut doc = AutoCommit::new();
        let r = automerge::ROOT;
        doc.put(&r, "search_query", "rust").unwrap();
        assert!(!app.model.search_loading);
        app.apply(Action::SubmitSearch, &mut doc, &r);
        assert!(app.model.search_loading, "an in-flight search must show the skeleton");
        assert!(matches!(&app.model.route, Route::Search(q) if q == "rust"));
    }

    #[test]
    fn show_mode_segment_drives_the_in_video_skip_overlay() {
        use crate::model::{Category, SkipMode};
        // The on-video "Skip" overlay is settings-driven: it appears only for a
        // Show-mode segment the playhead is inside, never for Auto-skip (which
        // jumps on its own) or Off, and never when SponsorBlock is disabled.
        let (mut app, _doc) = fresh();
        app.model.route = Route::Watch("vid_rust_async".into()); // Sponsor 1440..1570
        let key = Category::Sponsor.api_key().to_string();

        app.model.settings.categories.insert(key.clone(), SkipMode::Skip);
        app.model.playhead = 1500.0;
        assert_eq!(app.current_show_skip_end(), None, "auto-skip shows no manual overlay");

        app.model.settings.categories.insert(key.clone(), SkipMode::Show);
        assert_eq!(app.current_show_skip_end(), Some(1570.0), "show-mode offers a Skip to the end");

        app.model.playhead = 100.0;
        assert_eq!(app.current_show_skip_end(), None, "no overlay outside the segment");

        app.model.playhead = 1500.0;
        app.model.settings.sponsorblock_enabled = false;
        assert_eq!(app.current_show_skip_end(), None, "master off suppresses the overlay");
    }

    #[test]
    fn channel_stub_qualifies_for_an_uploader_fetch() {
        // The watch page's creator avatar + subscriber count come from a channel
        // fetch kicked off when we only hold a name stub. Prove the gate: a bare
        // `ensure_channel_stub` record has no avatar and 0 subscribers (so it
        // qualifies), while a fetched channel would not.
        let mut m = crate::model::TubeModel::default();
        m.ensure_channel_stub("c1", "Chan");
        let stub = m.channel("c1").expect("stub inserted");
        assert!(
            stub.avatar_url.is_empty() && stub.subscribers == 0,
            "a name stub must be recognised as needing an avatar/subs fetch",
        );
    }
}
