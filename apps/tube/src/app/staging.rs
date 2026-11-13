//! What a screenshot, a bench or a test puts on the screen: the sample page a capture shows, the
//! route it opens on, the player it seeds with a frame, and the hooks that replace the catalogue or
//! the worker.

use std::cell::RefCell;
use std::rc::Rc;

use ui::widget::VideoPlayer;
use vello::kurbo::Rect;

use crate::action::{self, Action};
use crate::model::{Route, TubeModel};

#[cfg(feature = "samples")]
use super::ShellAsk;
use super::playback::pump_player;
use super::{Tube, TubeApp};

impl Tube {
    #[cfg(feature = "samples")]
    fn play_demo(&mut self) {
        let id = self
            .model
            .video(crate::sample::SAMPLE_CAPTIONED_VIDEO)
            .map(|v| v.id.clone())
            .or_else(|| self.model.videos.first().map(|v| v.id.clone()))
            .unwrap_or_else(|| crate::sample::SAMPLE_CAPTIONED_VIDEO.to_string());
        self.model.route = Route::Watch(id.clone());
        self.model.settings.caption_language = Some("en".to_string());
        self.request_captions_for(&id);
        {
            let mut p = self.player.borrow_mut();
            p.set_engine(media::MediaEngine::new_synthesized());
            p.open(media::Source::Remote {
                url: "https://demo.local/clip.mp4".into(),
            });
            p.seek_to_ms(45_000);
            p.play();
            p.reveal_controls();
            pump_player(&mut p);
        }
        self.player_video = Some(id);
    }

    #[cfg(feature = "samples")]
    pub(crate) fn stage_scenario(&mut self, scenario: &str) {
        match scenario {
            "empty" => self.show_empty_home(),
            "loading" => self.show_loading_home(),
            #[cfg(feature = "grayjay-sync")]
            "device-sync" => self.show_device_sync_sample(),
            "play-demo" => self.play_demo(),
            "narrow" => self.shell_asks.push(ShellAsk::OpenSidebar),
            "sidebar-list" => self.shell_asks.push(ShellAsk::SidebarWidth(f64::from(
                ui::tokens::Metrics::BASE.sidebar_width,
            ))),
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
    }

    fn set_image_store(&mut self, store: ui::ImageStore) {
        self.images = Some(store);
    }

    fn stage_model_for_test(&mut self, model: TubeModel) {
        self.search_query.set(model.search_query.clone());
        self.model = model;
        self.online = false;
        self.worker = None;
        self.structure_dirty = true;
    }

    pub(crate) fn wake_with(&mut self, wake: ui::WakeHandle) {
        self.wake = wake;
    }

    fn script_worker_for_test(&mut self) -> crate::api::Updates {
        let (worker, results) = crate::api::Worker::scripted(self.wake.clone());
        self.worker = Some(worker);
        results
    }

    fn search_on_launch(&self, query: &str) {
        self.search_query.set(query.to_owned());
        action::push(&self.bus, Action::SubmitSearch);
    }

    fn stage_search(&mut self, query: &str, video_ids: Vec<String>) {
        self.model.search_query = query.to_string();
        self.model.search_hits_query = query.to_string();
        self.model.search_hits = video_ids;
        self.model.route = Route::Search(query.to_string());
        self.nav.push(Route::Search(query.to_string()));
    }

    #[cfg(feature = "samples")]
    fn show_loading_home(&mut self) {
        self.model = TubeModel::default();
        self.model.feed_loading = true;
        self.model.route = Route::Home;
    }

    #[cfg(feature = "samples")]
    fn show_empty_home(&mut self) {
        self.model = TubeModel::default();
        self.model.data_source.label = "offline: network unavailable".into();
        self.model.route = Route::Home;
    }

    fn show_route(&mut self, route: Route) {
        if let Route::Search(q) = &route {
            q.clone_into(&mut self.model.search_query);
            self.search_query.set(q.clone());
        }
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

    #[cfg(all(feature = "samples", feature = "grayjay-sync"))]
    fn show_device_sync_sample(&mut self) {
        self.model.route = Route::Settings;
        self.device_sync = crate::sync::DeviceSync::visual_sample();
        self.scroll_cells.borrow_mut().insert(
            "settings".into(),
            Rc::new(std::cell::Cell::new((0.0, f32::MAX))),
        );
    }

    fn stage_live_frame(&mut self, video_id: &str, url: String) {
        use ui::time::Duration;
        self.model.route = Route::Watch(video_id.to_string());
        {
            let mut p = self.playing_player();
            p.open(media::Source::Remote { url });
            p.play();
            p.reveal_controls();
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
            pump(
                &mut p,
                &|p| p.latest_frame().is_some(),
                Duration::from_secs(25),
            );
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
    }

    fn stage_bench_loaded_watch(&mut self, video_id: &str) {
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
            pump_player(&mut p);
            p.pause();
        }
        self.player_video = Some(video_id.to_string());
    }

    fn play_synthesized_for_test(&mut self) {
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
}

impl TubeApp {
    #[must_use]
    pub fn with_image_store(self, store: ui::ImageStore) -> Self {
        self.tube().set_image_store(store);
        self
    }

    #[must_use]
    pub fn with_model_for_test(self, model: TubeModel) -> Self {
        self.stage_model_for_test(model);
        self
    }

    pub fn stage_model_for_test(&self, model: TubeModel) {
        self.tube().stage_model_for_test(model);
    }

    #[doc(hidden)]
    pub fn script_worker_for_test(&mut self) -> crate::api::Updates {
        self.tube().script_worker_for_test()
    }

    #[must_use]
    pub fn searching(self, query: &str) -> Self {
        self.tube().search_on_launch(query);
        self
    }

    #[must_use]
    pub fn with_search_for_test(self, query: &str, video_ids: Vec<String>) -> Self {
        self.tube().stage_search(query, video_ids);
        self
    }

    #[must_use]
    pub fn with_viewport(mut self, w: f64, h: f64) -> Self {
        self.rect = Rect::new(0.0, 0.0, w.max(0.0), h.max(0.0));
        self
    }

    #[must_use]
    pub fn with_route_shot(self, route: Route) -> Self {
        self.tube().show_route(route);
        self
    }

    #[must_use]
    pub fn with_live_frame_for_shot(self, video_id: &str, url: String) -> Self {
        self.tube().stage_live_frame(video_id, url);
        self
    }

    #[must_use]
    pub fn with_bench_loaded_watch(self, video_id: &str) -> Self {
        self.tube().stage_bench_loaded_watch(video_id);
        self
    }

    #[doc(hidden)]
    pub fn play_synthesized_for_test(&mut self) {
        self.tube().play_synthesized_for_test();
    }

    #[doc(hidden)]
    pub fn media_engine_started_for_test(&self) -> bool {
        self.tube.borrow().player.borrow().engine().is_some()
    }

    pub fn route_for_test(&self) -> Route {
        self.tube.borrow().model.route.clone()
    }

    pub fn can_go_back_for_test(&self) -> bool {
        self.tube.borrow().nav.can_go_back()
    }

    pub fn model_for_test(&self) -> std::cell::Ref<'_, TubeModel> {
        std::cell::Ref::map(self.tube.borrow(), |tube| &tube.model)
    }

    pub fn sidebar_open_for_test(&self) -> bool {
        self.shell.is_sidebar_open()
    }
}
