//! What a still, a bench run or a test puts on the screen: the sample page a
//! capture shows, the route it opens on, the player it seeds with a frame, and
//! the hooks that replace the catalogue or the worker. The live window reaches
//! none of these; they exist so a review or a test sees real content without a
//! network.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use ui::widget::VideoPlayer;
use vello::kurbo::Rect;

use crate::action::{self, Action};
use crate::model::{Route, TubeModel};

use super::TubeApp;
use super::playback::pump_player;

impl TubeApp {
    fn play_demo(&mut self) {
        // The sample video that carries captions, so the still shows the whole
        // watch page: picture, transport, settings and the lines being spoken.
        let id = self
            .model
            .video(crate::sample::SAMPLE_CAPTIONED_VIDEO)
            .map(|v| v.id.clone())
            .or_else(|| self.model.videos.first().map(|v| v.id.clone()))
            .unwrap_or_else(|| crate::sample::SAMPLE_CAPTIONED_VIDEO.to_string());
        self.model.route = Route::Watch(id.clone());
        // The still shows captions: they are part of what a watch page is, and
        // a capture that leaves them out reviews a page nobody sees.
        self.model.settings.caption_language = Some("en".to_string());
        self.request_captions_for(&id);
        {
            let mut p = self.player.borrow_mut();
            // Pin the deterministic synthesized-frame backend for the shot: the
            // `media` feature (real GStreamer decode) is compiled in by default,
            // so `MediaEngine::new()` would otherwise build a real pipeline and
            // contact the placeholder `demo.local` host. The synthesized engine
            // seeds the same surface without a pipeline or network. (The real
            // decode path is exercised by `with_live_frame_for_shot` under the
            // separate `--live-shots` hook.)
            p.set_engine(media::MediaEngine::new_synthesized());
            p.open(media::Source::Remote {
                url: "https://demo.local/clip.mp4".into(),
            });
            // Seed a colourful mid-stream frame for the still, then pump the
            // engine so the frame lands in the surface. The offscreen renderer
            // syncs the app exactly once — *before* this player is mounted in the
            // tree — so without pumping here the single shot would draw an empty
            // (never-pumped) surface. A live run pumps every frame, so this is
            // shots-only.
            p.seek_to_ms(45_000);
            p.play();
            p.reveal_controls();
            pump_player(&mut p);
        }
        self.player_video = Some(id);
    }

    pub(crate) fn stage_scenario(&mut self, scenario: &str) {
        match scenario {
            "empty" => self.show_empty_home(),
            "loading" => self.show_loading_home(),
            #[cfg(feature = "grayjay-sync")]
            "device-sync" => self.show_device_sync_sample(),
            "play-demo" => self.play_demo(),
            "narrow" => {
                self.force_sidebar_open_shot = true;
                self.shell.open_sidebar();
                self.shell.snap();
            }
            // The sidebar as a person who dragged its edge out left it: a
            // list, each icon beside its label.
            "sidebar-list" => self
                .shell
                .stage_sidebar_width(f64::from(ui::tokens::Metrics::BASE.sidebar_width)),
            "trending" => self.show_route(Route::Trending),
            "subscriptions" => self.show_route(Route::Subscriptions),
            "queue" => self.show_route(Route::Queue),
            "history" => self.show_route(Route::History),
            "settings" => self.show_route(Route::Settings),
            "search" => self.show_route(Route::Search("rust".into())),
            "watch" => self.show_route(Route::Watch("vid_donut".into())),
            _ => {}
        }
        self.structure_dirty = true;
        self.shell_stale = true;
    }

    /// Test/shots hook: attach an image store so an offscreen `--shots` render
    /// can paint fetched thumbnails, avatars, and banners. The live app gets
    /// its store via [`Self::online`].
    #[must_use]
    pub fn with_image_store(mut self, store: ui::ImageStore) -> Self {
        self.images = Some(store);
        self
    }

    /// Test/shots hook: replace the model wholesale (e.g. a channel page built
    /// from real yt-dlp data) so an offscreen render shows real content.
    #[must_use]
    pub fn with_model_for_test(mut self, model: TubeModel) -> Self {
        self.stage_model_for_test(model);
        self
    }

    /// Supply a deterministic catalogue in a hosted test without changing its
    /// dispatcher, effect admission, or shared library. Live launch stays empty.
    ///
    /// A live window has already spawned its fetch worker on its first frame.
    /// Retire it with the live catalogue: its startup notes, its status and
    /// the feed it fetched would otherwise land whenever the network answers,
    /// rebuild the staged page and drop a click made in between.
    pub fn stage_model_for_test(&mut self, model: TubeModel) {
        self.search_query.set(model.search_query.clone());
        self.model = model;
        self.online = false;
        self.worker = None;
        self.shell_stale = true;
    }

    /// Wake this window through `wake` when the fetch worker posts a result.
    pub(crate) fn wake_with(&mut self, wake: ui::WakeHandle) {
        self.wake = wake;
    }

    /// Test hook: fetch nothing, and fold the results the returned sender
    /// sends as the network's, whenever the test lands one.
    #[doc(hidden)]
    pub fn script_worker_for_test(&mut self) -> crate::api::Updates {
        let (worker, results) = crate::api::Worker::scripted(self.wake.clone());
        self.worker = Some(worker);
        results
    }

    /// Open on the results for `query`, as typing it into the search field
    /// and pressing Enter does: a launch from desktop search (`Search
    /// YouTube for cats`). The search runs on the first frame, once the
    /// window can be woken by its results.
    #[must_use]
    pub fn searching(self, query: &str) -> Self {
        self.search_query.set(query.to_owned());
        action::push(&self.bus, Action::SubmitSearch);
        self
    }

    #[must_use]
    pub fn with_search_for_test(mut self, query: &str, video_ids: Vec<String>) -> Self {
        self.model.search_query = query.to_string();
        self.model.search_hits_query = query.to_string();
        self.model.search_hits = video_ids;
        self.model.route = Route::Search(query.to_string());
        self.nav.push(Route::Search(query.to_string()));
        self
    }

    fn show_loading_home(&mut self) {
        self.model = TubeModel::default();
        self.model.feed_loading = true;
        self.model.route = Route::Home;
    }

    fn show_empty_home(&mut self) {
        self.model = TubeModel::default();
        self.model.data_source.label = "offline: network unavailable".into();
        self.model.route = Route::Home;
    }

    /// `--shots` builder: record the render viewport up front so the first
    /// frame sizes the width-driven watch player from the true width. The
    /// live app learns its width from `apply_layout` each frame.
    #[must_use]
    pub fn with_viewport(mut self, w: f64, h: f64) -> Self {
        self.rect = Rect::new(0.0, 0.0, w.max(0.0), h.max(0.0));
        self
    }

    /// `--shots` builder: start on a specific [`Route`] so each screen can be
    /// captured for self-inspection. Pure presentation override — the live app
    /// starts from the sample/persisted route instead.
    #[must_use]
    pub fn with_route_shot(mut self, route: Route) -> Self {
        self.show_route(route);
        self
    }

    fn show_route(&mut self, route: Route) {
        if let Route::Search(q) = &route {
            q.clone_into(&mut self.model.search_query);
            self.search_query.set(q.clone());
        }
        // The shell uses `NavStack` as the navigation authority (Back state,
        // keyboard handling and the top bar all consult it), while the model
        // mirrors its current route.  A deterministic screenshot must seed
        // both sides together; changing only the presentation model created a
        // state that a user or an agent could never actually navigate to.
        // A page reached by browsing (a video, a creator, a playlist) is
        // opened from Home, so Back has somewhere to return to.
        if matches!(
            route,
            Route::Watch(_) | Route::Channel(_) | Route::Playlist(_)
        ) {
            self.nav = ui::NavStack::new(Route::Home);
            self.nav.push(route.clone());
        } else {
            self.nav = ui::NavStack::new(route.clone());
        }
        self.model.route = route;
    }

    #[cfg(feature = "grayjay-sync")]
    fn show_device_sync_sample(&mut self) {
        self.model.route = Route::Settings;
        self.device_sync = crate::sync::DeviceSync::visual_sample();
        // Scroll to the end of the form: the device section is the last one.
        self.scroll_cells
            .borrow_mut()
            .insert("settings".into(), Rc::new(Cell::new((0.0, f32::MAX))));
    }

    /// `--live-shots` hook: open the shared player on a real, directly-playable
    /// stream `url` and pump the media engine until a real frame decodes
    /// (bounded), so an offscreen still captures the ACTUAL decoded picture — the
    /// same `VideoPlayer` + `media::MediaEngine` the live app streams through.
    /// Needs the `media` decode backend to produce a real frame; if none arrives
    /// within the budget the surface stays as-is (the poster/backdrop shows).
    #[must_use]
    pub fn with_live_frame_for_shot(mut self, video_id: &str, url: String) -> Self {
        use ui::time::Duration;
        self.model.route = Route::Watch(video_id.to_string());
        {
            let mut p = self.playing_player();
            p.open(media::Source::Remote { url });
            p.play();
            p.reveal_controls();
            // The offscreen renderer syncs the app only once — before this player
            // is mounted in the tree — so the frame must already be in hand before
            // the shot draws. Poll at the live app's ~16ms cadence: the engine's
            // appsink pull is non-blocking and the sink paces/drops frames to the
            // clock, so a slow poll misses them.
            let pump = |p: &mut ui::widget::VideoPlayer,
                        until: &dyn Fn(&ui::widget::VideoPlayer) -> bool,
                        budget: Duration| {
                let deadline = ui::time::now() + budget;
                while ui::time::now() < deadline {
                    pump_player(p);
                    if until(p) {
                        return true;
                    }
                    let _ = ui::rt::sleep_thread(Duration::from_millis(15));
                }
                false
            };
            // Wait for the first decoded frame before capturing the player.
            pump(
                &mut p,
                &|p| p.latest_frame().is_some(),
                Duration::from_secs(25),
            );
            // Seek past the typical fade-from-black intro for a useful capture.
            if p.duration_ms() > 40_000 {
                p.seek_to_ms(32_000);
                pump(
                    &mut p,
                    &|p| p.engine().map_or(0, media::MediaEngine::position_ms) >= 30_000,
                    Duration::from_secs(15),
                );
            }
        }
        self.player_video = Some(video_id.to_string());
        self
    }

    /// Bench hook: put the app on a watch page with a **synthesized, paused-but-
    /// loaded** player — a real decoded frame sitting in the surface, playback
    /// paused. This reproduces the scroll-perf scenario (`--bench-scroll --watch
    /// --loaded`): the scroll loop re-syncs each frame, pumping the engine, so the
    /// bench measures the true per-frame cost of a loaded-but-paused watch page.
    #[must_use]
    pub fn with_bench_loaded_watch(mut self, video_id: &str) -> Self {
        self.model.route = Route::Watch(video_id.to_string());
        self.player = Rc::new(RefCell::new(
            VideoPlayer::new().with_engine(media::MediaEngine::new_synthesized()),
        ));
        {
            let mut p = self.player.borrow_mut();
            p.open(media::Source::Remote {
                url: "https://demo.local/clip.mp4".into(),
            });
            p.seek_to_ms(30_000);
            // Pull the first frame into the surface, then pause so it is
            // "loaded-but-paused" (the reported jank condition).
            pump_player(&mut p);
            p.pause();
        }
        self.player_video = Some(video_id.to_string());
        self
    }

    /// Test hook: the watch page's player plays a synthesized source (no
    /// decode backend), so a test can count the work each video frame does.
    #[doc(hidden)]
    pub fn play_synthesized_for_test(&mut self) {
        let Route::Watch(id) = self.model.route.clone() else {
            return;
        };
        {
            let mut p = self.player.borrow_mut();
            p.set_engine(media::MediaEngine::new_synthesized());
            p.open(media::Source::Remote {
                url: "https://demo.local/clip.mp4".into(),
            });
            p.play();
            pump_player(&mut p);
        }
        self.player_video = Some(id);
    }

    /// Whether the shared player has built its media engine yet.
    #[doc(hidden)]
    pub fn media_engine_started_for_test(&self) -> bool {
        self.player.borrow().engine().is_some()
    }

    /// Test accessor: the current route (read-only).
    pub fn route_for_test(&self) -> &Route {
        &self.model.route
    }

    /// Test accessor: whether the navigation stack has somewhere to go back to.
    pub fn can_go_back_for_test(&self) -> bool {
        self.nav.can_go_back()
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
        self.shell.is_sidebar_open()
    }
}
