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

use std::collections::HashMap;

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
use ui::widget::{NavSplit, SplitMode, Widget};

use crate::action::{self, Action, ActionBus, SettingFlag};
use crate::api::{Request, Update, Worker};
use crate::model::{Route, TubeModel};
use crate::screens;
use crate::widgets::Flex;

const STATE_KEY: &str = "tube_state";
const SEARCH_KEY: &str = "search_query";
const QUALITIES: [&str; 4] = ["1080p", "720p", "480p", "auto"];

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
            sidebar_open: None,
            force_sidebar_open_shot: false,
            last_nav_overlay: None,
        }
    }

    /// `--shots` builder: render at the narrow breakpoint with the navigation
    /// rail summoned as an open overlay flap, so the offscreen still shows the
    /// collapsed-sidebar state. The live app never calls this.
    pub fn with_open_sidebar_shot(mut self) -> Self {
        self.force_sidebar_open_shot = true;
        self
    }

    fn build_shell_themed(&self, theme: &Theme) -> Box<dyn Widget> {
        let m = &self.model;
        let bus = &self.bus;
        let content = match &m.route {
            Route::Home => screens::home(m, theme, bus),
            Route::Trending => screens::trending(m, theme, bus),
            Route::Subscriptions => screens::subscriptions(m, theme, bus),
            Route::History => screens::history(m, theme, bus),
            Route::Settings => screens::settings(m, theme, bus),
            Route::Watch(id) => screens::watch(m, theme, bus, id),
            Route::Channel(id) => screens::channel(m, theme, bus, id),
            Route::Search(q) => screens::search(m, theme, bus, q),
        };

        let content_area = Flex::col().grow(1.0).fill_h().clip().push(content);

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

        Box::new(
            Flex::col()
                .fill_w()
                .fill_h()
                .push(screens::top_bar(m, theme, bus))
                .push(split),
        )
    }

    /// Whether the navigation rail is collapsed to an overlay flap at the
    /// current allocated width. Mirrors `SplitLayout::from_width` so the
    /// hamburger toggle and the selection-dismiss resolve the rail's default
    /// the same way the live split does.
    fn nav_is_overlay(&self) -> bool {
        let w = self.rect.width();
        // Width 0 = first build, before the first `apply_layout`. Assume the
        // desktop (docked) default so a wide window's rail isn't hidden for a
        // frame; a narrow window self-corrects on its first laid-out frame.
        w > 0.0 && w < NAV_BREAKPOINT
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

    /// Resolve the current watch video's duration, if any.
    fn current_duration(&self) -> Option<f64> {
        match &self.model.route {
            Route::Watch(id) => self.model.video(id).map(|v| v.duration as f64),
            _ => None,
        }
    }

    /// If the playhead sits inside an enabled SponsorBlock segment, jump to
    /// its end and log the skip. Mirrors the auto-skip behaviour as the
    /// user scrubs.
    fn apply_auto_skip(&mut self) {
        if !self.model.settings.auto_skip || !self.model.settings.sponsorblock_enabled {
            return;
        }
        let Route::Watch(id) = self.model.route.clone() else {
            return;
        };
        let (seg_end, label) = {
            let Some(v) = self.model.video(&id) else { return };
            let mut found = None;
            for s in &v.sponsor {
                if self.model.settings.category_enabled(s.category) && s.contains(self.model.playhead) {
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
                None => return,
            }
        };
        self.model.playhead = seg_end;
        self.model.skipped_log.insert(0, label);
        self.model.skipped_log.truncate(8);
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
                self.model.record_watch(&id);
                self.model.route = Route::Watch(id.clone());
                self.model.playhead = 0.0;
                self.model.playing = true;
                self.dismiss_overlay_rail();
                self.request(Request::Branding(id));
            }
            Action::OpenChannel(id) => {
                self.model.route = Route::Channel(id);
                self.dismiss_overlay_rail();
            }
            Action::ToggleSubscribe(id) => {
                self.model.toggle_subscribe(&id);
            }
            Action::Seek(frac) => {
                if let Some(d) = self.current_duration() {
                    self.model.playhead = (frac * d).clamp(0.0, d);
                    self.apply_auto_skip();
                }
            }
            Action::TogglePlay => {
                self.model.playing = !self.model.playing;
            }
            Action::ToggleCategory(c) => {
                self.model.settings.toggle_category(c);
            }
            Action::ToggleSetting(flag) => {
                let s = &mut self.model.settings;
                match flag {
                    SettingFlag::SponsorBlock => s.sponsorblock_enabled = !s.sponsorblock_enabled,
                    SettingFlag::AutoSkip => s.auto_skip = !s.auto_skip,
                    SettingFlag::DeArrow => s.dearrow_enabled = !s.dearrow_enabled,
                    SettingFlag::DeArrowThumbnails => s.dearrow_thumbnails = !s.dearrow_thumbnails,
                    SettingFlag::Autoplay => s.autoplay = !s.autoplay,
                    SettingFlag::ShowComments => s.show_comments = !s.show_comments,
                }
            }
            Action::SetQuality(_) => {
                let cur = self.model.settings.preferred_quality.clone();
                let idx = QUALITIES.iter().position(|q| *q == cur).unwrap_or(0);
                self.model.settings.preferred_quality = QUALITIES[(idx + 1) % QUALITIES.len()].to_string();
            }
            Action::SubmitSearch => {
                let q = self.read_search(state, root);
                self.model.search_query = q.clone();
                self.model.route = Route::Search(q.clone());
                if !q.is_empty() {
                    let instance = self.model.settings.invidious_instance.clone();
                    self.request(Request::Search { instance, query: q });
                }
            }
            Action::Like => {
                // Local-only acknowledgement for now.
                self.model.skipped_log.insert(0, "Liked (stored locally)".into());
                self.model.skipped_log.truncate(8);
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
        for u in updates {
            match u {
                Update::Sponsor(id, segs) => {
                    if let Some(v) = self.model.videos.iter_mut().find(|v| v.id == id) {
                        v.sponsor = segs;
                        changed = true;
                    }
                }
                Update::DeArrow { video, title, has_thumbnail } => {
                    if let Some(v) = self.model.videos.iter_mut().find(|v| v.id == video) {
                        if title.is_some() {
                            v.dearrow_title = title;
                        }
                        v.dearrow_thumb = has_thumbnail;
                        changed = true;
                    }
                }
                Update::SearchResults(query, vids) => {
                    if !vids.is_empty() {
                        // Merge new videos that we don't already have.
                        for nv in vids {
                            if !self.model.videos.iter().any(|v| v.id == nv.id) {
                                self.model.videos.push(nv);
                            }
                        }
                        self.model.route = Route::Search(query);
                        changed = true;
                    }
                }
                Update::Note(n) => {
                    self.model.skipped_log.insert(0, n);
                    self.model.skipped_log.truncate(8);
                    changed = true;
                }
            }
        }
        changed
    }

    fn persist(&self, state: &mut AutoCommit, root: &ObjId) {
        if let Ok(json) = serde_json::to_string(&self.model) {
            let _ = state.put(root, STATE_KEY, json);
        }
    }

    fn ensure_search_obj(&mut self, state: &mut AutoCommit, root: &ObjId) {
        if self.search_obj.is_some() {
            return;
        }
        match state.get(root, SEARCH_KEY) {
            Ok(Some((Value::Object(ObjType::Text), id))) => self.search_obj = Some(id),
            _ => {
                if let Ok(id) = state.put_object(root, SEARCH_KEY, ObjType::Text) {
                    self.search_obj = Some(id);
                }
            }
        }
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
        if let Some(c) = &mut self.root_child {
            c.sync(state, path);
        }
    }

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
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
            if self.nav_is_overlay() && self.sidebar_currently_open() {
                self.sidebar_open = Some(false);
            }
        }

        // Drain & apply queued actions.
        let actions = action::drain(&self.bus);
        let mut changed = false;
        for a in actions {
            changed |= self.apply(a, state, path);
        }
        // Fold any ready worker results.
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
        // Keep syncing while a fetch is outstanding so results fold in.
        self.worker.is_some()
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
        assert!(app.model.skipped_log.iter().any(|l| l.contains("Sponsor")));
    }

    #[test]
    fn toggling_category_flips_enabled() {
        let (mut app, mut doc) = fresh();
        let r = automerge::ROOT;
        let before = app.model.settings.category_enabled(Category::Intro);
        app.apply(Action::ToggleCategory(Category::Intro), &mut doc, &r);
        assert_ne!(before, app.model.settings.category_enabled(Category::Intro));
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
}
