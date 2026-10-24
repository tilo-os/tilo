//! Music's views as a stack, and its search as a view of its own.
//!
//! The library, its songs, albums, artists and playlists, an album's or an
//! artist's page are Music's designed views; the window keeps them as a
//! stack ([`ViewStack`]), so Back and Forward always work and returning to a
//! view puts it back as it was left, scrolled to the same place with the same
//! songs selected.
//!
//! Typing into the window bar's search field turns the window's view into a
//! query view of the library ([`View::Search`]): the songs, albums, artists
//! and playlists that match, each drawn as everywhere else in Music (a song
//! is the same row, liked, playing, draggable, with its menu), the chips the
//! types' filterable fields offer under the field, the streaming catalogue's
//! answer in its own section below, and the controls of Music that match in
//! a trailing "In this app" group. Escape returns to the view before. A
//! query view can be kept: it then sits in the sidebar beside the designed
//! views ([`View::Kept`]) and survives a restart.

use std::rc::Rc;
use std::sync::Arc;

use ui::background::Pumped;
use ui::framework::views::{
    FacetOffer, Item, KeptViews, QueryEngine, QuerySpec, RemoteError, RemoteSource, ResultGroup,
    ViewPlace, ViewStack, facet_bar,
};
use ui::icons::Icon;
use ui::motion::Presence;
use ui::paint::PaintCtx;
use ui::time::{Duration, Instant};
use ui::widget::{Button, ButtonRole, ControlSize, Row, Toggle};
use vello::kurbo::{Point, Rect};

use super::{AlbumDisp, ArtistDisp, Look, PlaylistDisp, TrackTable};
use crate::app::{Action, MusicApp, View};
use crate::search_type::{ALBUM_TYPE_ID, ARTIST_TYPE_ID, PLAYLIST_TYPE_ID, SONG_TYPE_ID};
use crate::views::Fetched;

/// How many matching controls the "In this app" group shows.
const CONTROL_HITS: usize = 4;

/// How long a control the person was taken to stays highlighted.
const HIGHLIGHT_FOR: Duration = Duration::from_millis(1600);

/// The window's search and what it found.
pub(in crate::app) struct SearchView {
    pub(in crate::app) engine: QueryEngine,
    /// What the window bar's field holds.
    pub(in crate::app) input: String,
    /// The chips picked under the field.
    pub(in crate::app) picked: Vec<ui::framework::views::Chip>,
    /// The bar's search is open on this window.
    pub(in crate::app) open: bool,
    /// A capture's executor: the catalogue answers when a frame pumps it.
    pumped: Option<Pumped>,
    /// A capture holds the catalogue's answer back, to show it on its way.
    pub(in crate::app) withhold: bool,
    catalogue: Option<Arc<crate::views::CatalogueSource>>,
    /// The library revision the local items were taken from.
    local_revision: Option<u64>,
    /// The remote sections as drawn this frame.
    pub(in crate::app) remote_rect: Rect,
    /// The keyboard's row among the results' songs, once Enter or Down in
    /// the field handed it the keyboard.
    pub(in crate::app) cursor: Option<usize>,
    /// Which answer's catalogue songs are folded into the library already.
    absorbed: (u64, usize),
    /// A control the person was taken to, highlighted where it lives: the
    /// ring closes in on it, holds, then fades.
    pub(in crate::app) highlight: Option<(Action, Presence, Instant)>,
    /// Songs being dragged onto a playlist.
    pub(in crate::app) drag: Option<TrackDrag>,
    /// The chips and matching controls drawn last, and what they were made
    /// of: made again only when the results or a control's state change.
    drawn: Option<Drawn>,
    /// The remote sections' placeholder steps as painted last: a deadline
    /// repaints when one moves on.
    pub(in crate::app) painted_steps: Vec<Option<ui::arriving::Step>>,
}

/// What a frame of the query view derives from its results.
struct Drawn {
    key: (u64, usize, String),
    offers: Vec<FacetOffer>,
    hits: Vec<ui::framework::views::ActionHit>,
}

/// Songs a person drags from a list onto a playlist in the sidebar.
#[derive(Clone, Debug, PartialEq)]
pub(in crate::app) struct TrackDrag {
    pub(in crate::app) tracks: Vec<String>,
    /// Where the pointer is.
    pub(in crate::app) at: Point,
}

/// What a query view offers about keeping it.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Keep {
    /// Nothing asked yet: nothing to keep.
    Nothing,
    /// Keep it beside the designed views.
    Offer,
    /// It is kept already.
    Kept,
    /// It is the kept view `id`: stop keeping it.
    Forget(String),
}

/// Where in Music a control lives.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Home {
    /// The now-playing bar.
    Player,
    /// The sidebar.
    Sidebar,
}

impl Home {
    fn id(self) -> &'static str {
        match self {
            Self::Player => "player",
            Self::Sidebar => "sidebar",
        }
    }

    /// What the button that takes the person there says.
    fn show_label(id: &str) -> &'static str {
        if id == "sidebar" {
            "Show in sidebar"
        } else {
            "Show in player"
        }
    }
}

/// One of Music's controls as the action it runs: the action or effect, what
/// its own control does, its icon, where it lives, its name there, and the
/// state of a switch.
struct MusicControl {
    spec: tilo_actions::ActionSpec,
    action: Action,
    icon: Icon,
    home: Home,
    label: &'static str,
    on: Option<bool>,
}

#[cfg_attr(
    test,
    expect(
        dead_code,
        reason = "a test never builds a window that reaches the network"
    )
)]
fn catalogue_backend() -> Box<dyn crate::backend::MusicBackend> {
    crate::backend::preferred_backend(|note| tracing::info!(target: "music::search", "{note}"))
}

impl SearchView {
    fn with_engine(engine: QueryEngine) -> Self {
        Self {
            engine,
            input: String::new(),
            picked: Vec::new(),
            open: false,
            pumped: None,
            withhold: false,
            catalogue: None,
            local_revision: None,
            remote_rect: Rect::ZERO,
            cursor: None,
            absorbed: (0, 0),
            highlight: None,
            drag: None,
            drawn: None,
            painted_steps: Vec::new(),
        }
    }

    /// The search of a window someone uses: the library, and the connected
    /// catalogue when this build can reach one.
    #[cfg_attr(
        test,
        expect(
            dead_code,
            reason = "a test never builds a window that reaches the network"
        )
    )]
    pub(in crate::app) fn live() -> Self {
        let engine = QueryEngine::new(crate::views::kinds());
        if !super::super::ONLINE_CATALOGUE {
            return Self::with_engine(engine);
        }
        let catalogue = Arc::new(crate::views::CatalogueSource::new(catalogue_backend));
        let source: Arc<dyn RemoteSource> = Arc::clone(&catalogue) as Arc<dyn RemoteSource>;
        let mut search = Self::with_engine(engine.with_remote(source));
        search.catalogue = Some(catalogue);
        search
    }

    /// The search of a capture or a past version: the library, and the
    /// sample catalogue, answering only when a frame pumps it, so a capture
    /// comes out the same on every machine.
    pub(in crate::app) fn hermetic() -> Self {
        let pumped = Pumped::new();
        let engine = QueryEngine::new(crate::views::kinds())
            .with_background(pumped.background())
            .with_remote(Arc::new(crate::views::SampleCatalogue));
        let mut search = Self::with_engine(engine);
        search.pumped = Some(pumped);
        search
    }

    /// A test's search: the library alone.
    #[cfg(test)]
    pub(in crate::app) fn local_only() -> Self {
        Self::with_engine(QueryEngine::new(crate::views::kinds()))
    }

    /// When a remote section's placeholder next moves on a step.
    pub(in crate::app) fn next_placeholder_step(&self, now: Instant) -> Option<Instant> {
        if !self.engine.is_open() {
            return None;
        }
        self.engine.results().next_placeholder_step(now)
    }

    /// Whether a frame should come to take in what is due: a capture's
    /// catalogue answers only when a frame pumps it.
    pub(in crate::app) fn needs_frame(&self) -> bool {
        self.pumped.is_some() && !self.withhold && self.engine.results().pending()
    }

    /// The person switched catalogues.
    pub(in crate::app) fn source_changed(&self) {
        if let Some(catalogue) = &self.catalogue {
            catalogue.source_changed();
        }
    }
}

impl MusicApp {
    // ---- the view stack ------------------------------------------------------

    /// Where the person is in the view shown: how far it is scrolled and
    /// which songs are selected.
    pub(in crate::app) fn current_place(&self) -> ViewPlace {
        ViewPlace {
            scroll: self.scroll_y(),
            selection: self
                .track_tables
                .first()
                .map(|slot| slot.table.selected_ids())
                .unwrap_or_default(),
            focus: None,
        }
    }

    /// Show `view`, leaving the one shown where it is for Back to return to.
    pub(in crate::app) fn open_view(&mut self, view: View) {
        let place = self.current_place();
        // What the window shows is the stack's current view, however it got
        // there.
        self.nav.replace(self.view.clone());
        if self.nav.open(view.clone(), place) {
            self.view = view;
            self.set_scroll_y(0.0);
            self.restore_selection = Some(Vec::new());
            self.view_shown();
        }
    }

    /// Show `view` in place of everything held: a sidebar destination
    /// starts a fresh trail.
    pub(in crate::app) fn reset_view(&mut self, view: View) {
        self.nav = ViewStack::new(view.clone());
        self.view = view;
        self.view_shown();
    }

    /// Show the sidebar destination `view`, from its top, as a fresh trail.
    pub(in crate::app) fn open_section(&mut self, view: View) {
        self.reset_view(view);
        self.set_scroll_y(0.0);
        self.restore_selection = Some(Vec::new());
    }

    /// Go back to the view before, as it was left.
    pub(in crate::app) fn go_back(&mut self) {
        let place = self.current_place();
        self.nav.replace(self.view.clone());
        if let Some(entry) = self.nav.back(place) {
            let (view, place) = (entry.view.clone(), entry.place.clone());
            self.arrive(view, &place);
        }
    }

    /// Go forward again, as it was left.
    pub(in crate::app) fn go_forward(&mut self) {
        let place = self.current_place();
        self.nav.replace(self.view.clone());
        if let Some(entry) = self.nav.forward(place) {
            let (view, place) = (entry.view.clone(), entry.place.clone());
            self.arrive(view, &place);
        }
    }

    /// Show `view` again at `place`.
    fn arrive(&mut self, view: View, place: &ViewPlace) {
        self.view = view;
        self.set_scroll_y(place.scroll);
        self.restore_selection = Some(place.selection.clone());
        self.view_shown();
        self.dirty = true;
    }

    /// The view shown changed: a query view shows its query's results.
    fn view_shown(&mut self) {
        match self.view.clone() {
            View::Search => {
                let query = self.search_query_spec();
                self.run_query(query);
            }
            View::Kept(id) => {
                if let Some(kept) = self.kept_views().get(&id).cloned() {
                    self.run_query(kept.query);
                }
            }
            _ => {
                let held = |view: &View| matches!(view, View::Search | View::Kept(_));
                if !self.search.open && !held(self.nav.current()) {
                    self.search.engine.close();
                }
            }
        }
    }

    // ---- the query -----------------------------------------------------------

    /// What the field and the picked chips ask.
    pub(in crate::app) fn search_query_spec(&self) -> QuerySpec {
        QuerySpec::parse(
            &self.search.input,
            &self.search.picked,
            self.search.engine.kinds(),
        )
    }

    /// Ask `query`, with the library's items current.
    fn run_query(&mut self, query: QuerySpec) {
        self.sync_query_local();
        if self.search.engine.set_query(query) {
            self.search.cursor = None;
            self.dirty = true;
        }
    }

    /// Hand the library's items to the query when the library changed.
    pub(in crate::app) fn sync_query_local(&mut self) {
        if self.search.local_revision == Some(self.library_revision) {
            return;
        }
        self.search.local_revision = Some(self.library_revision);
        let items = crate::views::library_items(&self.stored_library);
        self.search.engine.sync_local(items);
    }

    /// Take in what the catalogue answered, once a frame. Returns whether
    /// the results changed.
    pub(in crate::app) fn poll_query(&mut self) -> bool {
        if !self.search.engine.is_open() {
            return false;
        }
        self.sync_query_local();
        if let Some(pumped) = &self.search.pumped
            && !self.search.withhold
        {
            pumped.run_pending();
        }
        let pointer_on_remote =
            self.search.remote_rect.contains(self.mouse) || self.search.drag.is_some();
        self.search.engine.hold(pointer_on_remote);
        let changed = self.search.engine.poll();
        if changed {
            self.absorb_query_results();
            self.dirty = true;
        }
        let steps = self
            .search
            .engine
            .results()
            .placeholder_steps(ui::motion::frame_now());
        if steps != self.search.painted_steps {
            self.dirty = true;
        }
        changed
    }

    /// Make the catalogue's songs in the results playable and likeable, and
    /// record the covers of everything it found.
    fn absorb_query_results(&mut self) {
        let results = self.search.engine.results();
        let count: usize = results
            .remote
            .iter()
            .map(|section| section.rows().count())
            .sum();
        if self.search.absorbed == (results.generation, count) {
            return;
        }
        self.search.absorbed = (results.generation, count);
        let mut tracks = Vec::new();
        let mut covers = Vec::new();
        for item in results
            .remote
            .iter()
            .flat_map(ui::framework::views::RemoteSection::rows)
        {
            match item.payload_as::<Fetched>() {
                Some(Fetched::Track(hit)) => tracks.push(hit.clone()),
                Some(Fetched::Album(hit)) => covers.push((hit.id.clone(), hit.cover_url.clone())),
                Some(Fetched::Artist(hit)) => {
                    covers.push((hit.id.clone(), hit.avatar_url.clone()));
                }
                Some(Fetched::Playlist(hit)) => {
                    covers.push((hit.id.clone(), hit.cover_url.clone()));
                }
                None => {}
            }
        }
        self.absorb_tracks(&tracks);
        for (id, url) in covers {
            self.record_cover(&id, &url);
        }
    }

    /// The person picked or took away a chip.
    pub(in crate::app) fn on_set_query(&mut self, query: QuerySpec) {
        if let View::Kept(id) = self.view.clone() {
            // A kept view is the person's own: what they narrow it to stays.
            let picked = query.clone();
            self.update_kept(|kept| {
                if let Some(view) = kept.kept_views.iter_mut().find(|view| view.id == id) {
                    view.query = picked;
                }
            });
            self.run_query(query);
            return;
        }
        self.search.picked = query
            .chips
            .iter()
            .filter(|chip| !chip.typed)
            .cloned()
            .collect();
        let query = self.search_query_spec();
        self.run_query(query);
    }

    // ---- kept views ----------------------------------------------------------

    /// The query views the person kept, oldest first.
    pub(in crate::app) fn kept_views(&self) -> KeptViews {
        self.kept
            .as_ref()
            .map_or_else(|| self.kept_here.clone(), ui::framework::Setting::get)
    }

    fn update_kept(&mut self, change: impl FnOnce(&mut KeptViews)) {
        if let Some(setting) = self.kept.clone() {
            if let Err(error) = setting.update(change) {
                self.notify(
                    ui::widget::toast::ToastKind::Error,
                    format!("Couldn\u{2019}t keep this view: {error}"),
                );
            }
        } else {
            change(&mut self.kept_here);
        }
        self.dirty = true;
    }

    /// Keep the query view shown beside the designed views.
    pub(in crate::app) fn on_keep_query(&mut self) {
        let query = self.search.engine.results().query.clone();
        if query.is_empty() {
            return;
        }
        let name = KeptViews::default_name(&query, self.search.engine.kinds());
        let mut kept_id = String::new();
        self.update_kept(|kept| kept_id = kept.keep(&query, name.clone()));
        if !kept_id.is_empty() {
            self.notify(
                ui::widget::toast::ToastKind::Success,
                format!("Kept \u{201c}{name}\u{201d}"),
            );
        }
    }

    /// Stop keeping the view `id`.
    pub(in crate::app) fn on_forget_kept(&mut self, id: &str) {
        self.update_kept(|kept| {
            kept.forget(id);
        });
        let gone = View::Kept(id.to_owned());
        self.nav.forget(|view| *view == gone);
        if self.view == gone {
            if self.nav.can_go_back() {
                self.go_back();
            } else {
                self.reset_view(View::Home);
            }
        }
    }

    /// The kept view the query shown is, if it is one.
    fn kept_id_of(&self, query: &QuerySpec) -> Option<String> {
        let key = query.key();
        self.kept_views()
            .kept_views
            .iter()
            .find(|kept| kept.query.key() == key)
            .map(|kept| kept.id.clone())
    }

    // ---- controls ------------------------------------------------------------

    /// Music's controls as the actions they run.
    fn music_controls(&self) -> Vec<MusicControl> {
        use crate::actions::MusicEffect;
        use ui::framework::Effect as _;
        let idle = self.pb.current.is_none() && self.pb.queue.is_empty();
        let control = |spec, action, icon, home, label, on| MusicControl {
            spec,
            action,
            icon,
            home,
            label,
            on,
        };
        let mut controls = vec![
            control(
                MusicEffect::ToggleShuffle.bound(),
                Action::ToggleShuffle,
                Icon::Shuffle,
                Home::Player,
                "Shuffle",
                Some(self.pb.shuffle),
            ),
            control(
                MusicEffect::CycleRepeat.bound(),
                Action::CycleRepeat,
                Icon::Repeat,
                Home::Player,
                "Repeat",
                None,
            ),
            control(
                MusicEffect::ToggleMute.bound(),
                Action::ToggleMute,
                Icon::VolumeX,
                Home::Player,
                "Mute",
                Some(self.pb.volume == 0),
            ),
        ];
        if let Some(spec) = <crate::model::Library as ui::tilo_resource::Resource>::declarations()
            .iter()
            .map(ui::tilo_resource::declaration_spec)
            .find(|spec| spec.id == crate::model::Library::CREATE_PLAYLIST.id)
        {
            controls.push(control(
                spec,
                Action::BeginNewPlaylist,
                Icon::Plus,
                Home::Sidebar,
                "New playlist",
                None,
            ));
        }
        if !idle {
            controls.extend([
                control(
                    MusicEffect::PlayPause.bound(),
                    Action::TogglePlay,
                    if self.pb.playing {
                        Icon::Pause
                    } else {
                        Icon::Play
                    },
                    Home::Player,
                    if self.pb.playing { "Pause" } else { "Play" },
                    None,
                ),
                control(
                    MusicEffect::Next.bound(),
                    Action::Next,
                    Icon::SkipForward,
                    Home::Player,
                    "Next track",
                    None,
                ),
                control(
                    MusicEffect::Previous.bound(),
                    Action::Prev,
                    Icon::SkipBack,
                    Home::Player,
                    "Previous track",
                    None,
                ),
            ]);
        }
        controls
    }

    /// The controls of Music that match `text`, as their actions' hits.
    /// Only Music's real controls are offered: an action no control of Music
    /// runs is the palette's.
    pub(in crate::app) fn control_hits(&self, text: &str) -> Vec<ui::framework::views::ActionHit> {
        use ui::framework::views::{ControlDecl, action_hits};
        let controls = self.music_controls();
        let decls: Vec<ControlDecl> = controls
            .iter()
            .map(|control| {
                let decl = match control.on {
                    Some(on) => ControlDecl::switch(control.spec.id.clone(), on),
                    None => ControlDecl::button(control.spec.id.clone()),
                };
                decl.icon(control.icon)
                    .label(control.label)
                    .home(control.home.id())
            })
            .collect();
        let specs: Vec<tilo_actions::ActionSpec> =
            controls.into_iter().map(|control| control.spec).collect();
        action_hits(&specs, &decls, text, CONTROL_HITS)
    }

    /// The Music action a control hit runs.
    pub(in crate::app) fn control_action(&self, id: &str) -> Option<Action> {
        self.music_controls()
            .into_iter()
            .find(|control| control.spec.id == id)
            .map(|control| control.action)
    }

    /// Take the person to where the control running `action` lives, and
    /// highlight it there.
    pub(in crate::app) fn on_show_control(&mut self, home: &str, action: Action) {
        if home == Home::Sidebar.id()
            && let Some(bar) = &self.bar
            && !bar.is_sidebar_open()
        {
            bar.event(&ui::window_toolbar::ToolbarEvent::ToggleSidebar);
        }
        let now = ui::motion::frame_now();
        let mut ring = Presence::closed();
        ring.show(
            true,
            ui::motion::Disclose::SPEC,
            self.frame_theme
                .as_ref()
                .map(ui::theme::Theme::motion)
                .unwrap_or_default(),
            ui::tokens::MotionSource::Pointer,
            now,
        );
        self.search.highlight = Some((action, ring, now));
        self.dirty = true;
    }

    /// Ring the highlighted control where it was drawn this frame: the ring
    /// closes in on the control as it appears, holds, then fades away.
    pub(in crate::app) fn paint_highlight(&mut self, ctx: &mut PaintCtx<'_>) {
        let now = ui::motion::frame_now();
        let motion = ctx.theme.motion();
        let Some((action, ring, since)) = self.search.highlight.as_mut() else {
            return;
        };
        if ring.is_open() && now.saturating_duration_since(*since) > HIGHLIGHT_FOR {
            ring.show(
                false,
                ui::motion::Disclose::SPEC,
                motion,
                ui::tokens::MotionSource::Pointer,
                now,
            );
        }
        if !ring.is_visible(now) {
            self.search.highlight = None;
            return;
        }
        let progress = ring.progress(now);
        let action = action.clone();
        if let Some(hit) = self.hits.iter().find(|hit| hit.action == action) {
            // Closing in from a little way out as it appears.
            let reach = 3.0 + 8.0 * (1.0 - progress);
            let radius = hit.rect.height().min(hit.rect.width()) / 2.0;
            let ink = ui::color_compat::ColorCompat::with_alpha_factor(
                ctx.theme.border_focus,
                ui::num::f32_of_f64(progress),
            );
            ctx.stroke_round(hit.rect.inflate(reach, reach), radius + reach, ink, 2.0);
        }
    }

    // ---- drawing -------------------------------------------------------------

    /// The query view: its chips, the library's matches grouped by type, the
    /// catalogue's section below them, and the matching controls of Music.
    /// Returns the y below it.
    pub(super) fn draw_query_view(
        &mut self,
        ctx: &mut PaintCtx<'_>,
        look: &Look,
        body: Rect,
        y: f64,
    ) -> f64 {
        let results = self.search.engine.results().clone();
        let span = (body.x0 + look.gutter, body.x1 - look.gutter);
        let mut y = y + look.gutter;
        let rows = results
            .remote
            .iter()
            .map(|section| section.rows().count())
            .sum::<usize>()
            .saturating_add(results.local.iter().map(|group| group.items.len()).sum());
        let controls = format!(
            "{:?}",
            self.music_controls()
                .iter()
                .map(|control| (control.spec.id.clone(), control.on))
                .collect::<Vec<_>>()
        );
        let key = (results.generation, rows, controls);
        if self
            .search
            .drawn
            .as_ref()
            .is_none_or(|drawn| drawn.key != key)
        {
            let offers = self.search.engine.facet_offers();
            let hits = self.control_hits(&results.query.text);
            self.search.drawn = Some(Drawn { key, offers, hits });
        }
        let (offers, hits) = self
            .search
            .drawn
            .as_ref()
            .map(|drawn| (drawn.offers.clone(), drawn.hits.clone()))
            .unwrap_or_default();
        let keep = match &self.view {
            View::Kept(id) => Keep::Forget(id.clone()),
            _ if results.query.is_empty() => Keep::Nothing,
            _ if self.kept_id_of(&results.query).is_some() => Keep::Kept,
            _ => Keep::Offer,
        };
        y = self.query_chips(ctx, span, y, &offers, &results.query, keep);
        y += look.section;
        if results.query.is_empty() {
            return Self::note(
                ctx,
                look,
                body,
                y,
                "Type to search your library and the catalogue.",
            );
        }
        let local_rows: usize = results.local.iter().map(|group| group.items.len()).sum();
        if local_rows == 0 {
            y = Self::note(ctx, look, body, y, "Nothing in your library matches.") + look.section;
        }
        for group in &results.local {
            y = self.query_group(ctx, look, body, y, group);
        }
        let remote_top = y;
        let now = ui::motion::frame_now();
        self.search.painted_steps = results.placeholder_steps(now);
        for section in &results.remote {
            if section.state.is_none() {
                continue;
            }
            y = Self::remote_heading(ctx, look, body, y, &section.label);
            if let Some(clock) = section.placeholder() {
                y = Self::remote_placeholder(ctx, look, body, y, clock, now) + look.section;
            } else if let Some(text) = remote_note(section) {
                y = Self::note(ctx, look, body, y, &text) + look.section;
            }
            for group in &section.groups {
                y = self.query_group(ctx, look, body, y, group);
            }
        }
        self.search.remote_rect = Rect::new(body.x0, remote_top, body.x1, y);
        if !hits.is_empty() {
            y = self.query_controls(ctx, look, span, y, &hits);
        }
        y
    }

    /// Where a remote section's first row will be, while its answer is on
    /// the way: a row's height reserved, its skeleton after the grace
    /// period and where the answer comes from after a while; the rows take
    /// exactly this place.
    fn remote_placeholder(
        ctx: &mut PaintCtx<'_>,
        look: &Look,
        body: Rect,
        y: f64,
        clock: &ui::arriving::ArrivalClock,
        now: Instant,
    ) -> f64 {
        let face = ui::widget::ArrivalFace::new(ui::widget::SkeletonShape::Row, clock, now);
        let width = body.width() - 2.0 * look.gutter;
        let h = face.height(ctx.text, ctx.theme, width);
        face.paint(
            ctx,
            Rect::new(body.x0 + look.gutter, y, body.x1 - look.gutter, y + h),
        );
        y + h
    }

    /// A remote source's section heading: set apart from the library's
    /// groups by room and a larger role, its name says where its rows are
    /// from.
    fn remote_heading(ctx: &mut PaintCtx<'_>, look: &Look, body: Rect, y: f64, label: &str) -> f64 {
        let y = y + look.section;
        let style = ctx.theme.text(ui::tokens::TextRole::Title);
        let h = super::text_line(
            ctx,
            label,
            &style,
            look.text,
            Point::new(body.x0 + look.gutter, y),
            body.width() - 2.0 * look.gutter,
        );
        y + h + look.section
    }

    /// The chips under the field, one per filter, and at the row's end the
    /// action that keeps the search, or on a kept view the one that stops
    /// keeping it.
    fn query_chips(
        &mut self,
        ctx: &mut PaintCtx<'_>,
        span: (f64, f64),
        y: f64,
        offers: &[FacetOffer],
        query: &QuerySpec,
        keep: Keep,
    ) -> f64 {
        let key = format!("chips:{offers:?}:{query:?}:{keep:?}");
        let offers = offers.to_vec();
        let query = query.clone();
        self.page_part(ctx, span, y, key, move |asked| {
            let sink = Rc::clone(asked);
            let on_change: Rc<dyn Fn(QuerySpec)> =
                Rc::new(move |next| sink.borrow_mut().push(Action::SetQuery(next)));
            let sink = Rc::clone(asked);
            let open: ui::framework::views::OpenFacet =
                Rc::new(move |key| sink.borrow_mut().push(Action::OpenFacet(key.to_owned())));
            let keep_action: Option<Button> = match keep {
                Keep::Nothing => None,
                Keep::Offer => Some(
                    Button::new("music.keep-view", "Keep search")
                        .with_icon(Icon::Bookmark)
                        .role(ButtonRole::Secondary)
                        .tooltip("Keep this search beside your library")
                        .on_click(super::super::page_part::asking(asked, Action::KeepQuery)),
                ),
                Keep::Kept => Some(
                    Button::new("music.keep-view", "Kept")
                        .with_icon(Icon::Bookmark)
                        .role(ButtonRole::Secondary)
                        .toggled(true)
                        .tooltip("This search is one of your views")
                        .with_enabled(false),
                ),
                Keep::Forget(id) => Some(
                    Button::new("music.forget-view", "Remove from your views")
                        .role(ButtonRole::Secondary)
                        .on_click(super::super::page_part::asking(
                            asked,
                            Action::ForgetKept(id),
                        )),
                ),
            };
            let mut row = Row::new()
                .no_wrap()
                .with_rhythm(ui::tokens::Rhythm::Sibling)
                .with_justify(ui::taffy::JustifyContent::SpaceBetween)
                .with_align_items(ui::taffy::AlignItems::Center)
                .push(facet_bar(&offers, &query, &on_change, &open));
            if let Some(button) = keep_action {
                row = row.push(Box::new(button));
            }
            Box::new(row)
        })
    }

    /// Open a capture's staged filter values over its chip, once the chips
    /// are laid out.
    pub(super) fn open_staged_facet(&mut self) {
        let Some(key) = self.staged_facet.take() else {
            return;
        };
        let id = format!("view-chip:{key}");
        if let Some(chip) = self.page_parts.iter().find_map(|part| part.find(&id)) {
            self.mouse = Point::new(chip.x0 + 12.0, chip.y1);
            self.open_facet(&key);
        }
    }

    /// Open the values of the filter `key` beside its chip: each with how
    /// many results share it, any number of them chosen.
    pub(in crate::app) fn open_facet(&mut self, key: &str) {
        let Some(offer) = self
            .search
            .engine
            .facet_offers()
            .into_iter()
            .find(|offer| offer.key == key)
        else {
            return;
        };
        let query = self.search.engine.results().query.clone();
        self.context = ui::widget::context_menu::ContextMenu::new();
        self.menu_actions.clear();
        for value in &offer.values {
            let index = i64::try_from(self.menu_actions.len()).unwrap_or(i64::MAX);
            let pending = self.pending.clone();
            let mut item =
                ui::widget::context_menu::MenuItem::action(value.value.clone(), move || {
                    pending.store(index, std::sync::atomic::Ordering::SeqCst);
                })
                .with_checked(value.chosen)
                .with_enabled(!value.typed);
            if value.count > 0 {
                item = item.with_detail(value.count.to_string());
            }
            self.context.push(item);
            self.menu_actions
                .push(Action::SetQuery(offer.toggled(&query, &value.value)));
        }
        self.open_context_menu_at_pointer();
    }

    /// One group of results under its heading, drawn as Music draws its
    /// type everywhere: songs in the track list, the rest as cards.
    fn query_group(
        &mut self,
        ctx: &mut PaintCtx<'_>,
        look: &Look,
        body: Rect,
        y: f64,
        group: &ResultGroup,
    ) -> f64 {
        let y = Self::section_heading(ctx, look, body, y, &group.label);
        let ids: Vec<String> = group.items.iter().map(|item| item.id.clone()).collect();
        let below = match group.kind {
            SONG_TYPE_ID => {
                let rows = self.gather_tracks(&ids);
                if rows.is_empty() {
                    return y;
                }
                let shown: Vec<String> = rows.iter().map(|row| row.id.clone()).collect();
                self.draw_track_table(ctx, look, body, y, &TrackTable::new(&rows, &shown))
            }
            ALBUM_TYPE_ID => {
                let albums: Vec<AlbumDisp> = group
                    .items
                    .iter()
                    .map(|item| self.album_disp(item))
                    .collect();
                self.draw_album_grid(ctx, look, body, y, &albums)
            }
            ARTIST_TYPE_ID => {
                let artists: Vec<ArtistDisp> = group
                    .items
                    .iter()
                    .map(|item| ArtistDisp {
                        id: item.id.clone(),
                        name: item
                            .value(crate::views::TITLE)
                            .unwrap_or_default()
                            .to_owned(),
                        genre: self
                            .lib
                            .artist(&item.id)
                            .map(|artist| artist.genre.clone())
                            .unwrap_or_default(),
                    })
                    .collect();
                self.draw_artist_grid(ctx, look, body, y, &artists)
            }
            PLAYLIST_TYPE_ID => {
                let playlists: Vec<PlaylistDisp> = group
                    .items
                    .iter()
                    .map(|item| PlaylistDisp {
                        id: item.id.clone(),
                        name: item
                            .value(crate::views::TITLE)
                            .unwrap_or_default()
                            .to_owned(),
                        count: self.lib.playlist(&item.id).map_or_else(
                            || match item.payload_as::<Fetched>() {
                                Some(Fetched::Playlist(hit)) => {
                                    usize::try_from(hit.track_count).unwrap_or(usize::MAX)
                                }
                                _ => 0,
                            },
                            crate::model::Playlist::len,
                        ),
                    })
                    .collect();
                self.draw_playlist_cards(ctx, look, body, y, &playlists)
            }
            _ => y,
        };
        below + look.section
    }

    /// An album of the results as its card shows it: from the library when
    /// it holds the album, else from what the catalogue sent.
    fn album_disp(&self, item: &Item) -> AlbumDisp {
        if let Some(album) = self.lib.albums.get(&item.id) {
            return AlbumDisp {
                id: album.id.clone(),
                title: album.title.clone(),
                artist: self.lib.artist_name(&album.artist_id).to_string(),
                year: album.year,
                queue: self
                    .lib
                    .album_tracks(&album.id)
                    .iter()
                    .map(|track| track.id.clone())
                    .collect(),
            };
        }
        AlbumDisp {
            id: item.id.clone(),
            title: item
                .value(crate::views::TITLE)
                .unwrap_or_default()
                .to_owned(),
            artist: item
                .value(crate::views::ARTIST)
                .unwrap_or_default()
                .to_owned(),
            year: item
                .value(crate::views::YEAR)
                .and_then(|year| year.parse().ok())
                .unwrap_or(0),
            queue: Vec::new(),
        }
    }

    /// The trailing "In this app" group: each matching control of Music as
    /// that control, running what it runs, with Show taking the person to
    /// where it lives.
    fn query_controls(
        &mut self,
        ctx: &mut PaintCtx<'_>,
        look: &Look,
        span: (f64, f64),
        y: f64,
        hits: &[ui::framework::views::ActionHit],
    ) -> f64 {
        use ui::framework::views::ControlKind;
        let rows: Vec<(String, String, ui::framework::views::ControlDecl, Action)> = hits
            .iter()
            .filter_map(|hit| {
                let action = self.control_action(&hit.spec.id)?;
                Some((
                    hit.spec.id.clone(),
                    hit.label().to_owned(),
                    hit.control.clone(),
                    action,
                ))
            })
            .collect();
        if rows.is_empty() {
            return y;
        }
        let key = format!("controls:{rows:?}");
        let gap = ui::num::f32_of_f64(look.related);
        self.page_part(ctx, span, y, key, move |asked| {
            let mut column = ui::widget::Column::new()
                .with_no_padding()
                .with_gap(gap)
                .push(Box::new(
                    ui::widget::Text::new("In this app").with_role(ui::tokens::TextRole::Heading),
                ));
            for (id, label, decl, action) in rows {
                let (icon, home) = (decl.icon, decl.home);
                let control: ui::widget::Element = match decl.kind {
                    ControlKind::Button => {
                        let mut button = Button::new(&format!("music.control.{id}"), &label)
                            .role(ButtonRole::Secondary)
                            .control_size(ControlSize::Small);
                        if let Some(icon) = icon {
                            button = button.with_icon(icon);
                        }
                        Box::new(
                            button.on_click(super::super::page_part::asking(asked, action.clone())),
                        )
                    }
                    ControlKind::Switch { on } => {
                        let sink = Rc::clone(asked);
                        let flip = action.clone();
                        let mut toggle = Toggle::new(&format!("music.control.{id}"), "")
                            .with_accessible_name(&label)
                            .on_change(move |_| sink.borrow_mut().push(flip.clone()));
                        toggle.set_checked(on);
                        Box::new(
                            Row::new()
                                .with_gap(gap)
                                .with_align_items(ui::taffy::AlignItems::Center)
                                .push(Box::new(toggle))
                                .push(Box::new(ui::widget::Text::new(&label))),
                        )
                    }
                };
                let mut row = Row::new()
                    .with_gap(gap)
                    .with_align_items(ui::taffy::AlignItems::Center)
                    .push(control);
                if let Some(home) = home {
                    row = row.push(Box::new(
                        Button::new(&format!("music.control-show.{id}"), Home::show_label(&home))
                            .role(ButtonRole::Plain)
                            .control_size(ControlSize::Small)
                            .with_accessible_name(&format!("{}: {label}", Home::show_label(&home)))
                            .on_click(super::super::page_part::asking(
                                asked,
                                Action::ShowControl {
                                    home,
                                    action: Box::new(action),
                                },
                            )),
                    ));
                }
                column = column.push(Box::new(row));
            }
            Box::new(column)
        }) + look.section
    }

    // ---- dragging songs onto a playlist ------------------------------------------

    /// A drag began on row `row` of the track list `table`: the selected
    /// songs go with it when the row is one of them, else the row's song.
    pub(in crate::app) fn begin_track_drag(&mut self, table: usize, row: usize) {
        let Some(slot) = self.track_tables.get(table) else {
            return;
        };
        let selected = slot.table.selected_rows();
        let rows = if selected.contains(&row) {
            selected
        } else {
            vec![row]
        };
        let tracks: Vec<String> = rows
            .into_iter()
            .filter_map(|index| slot.table.rows().get(index).map(|cell| cell.id.clone()))
            .filter(|id| !id.is_empty())
            .collect();
        if !tracks.is_empty() {
            self.search.drag = Some(TrackDrag {
                tracks,
                at: self.mouse,
            });
            self.dirty = true;
        }
    }

    /// The playlist of the sidebar under `point`, while songs are dragged.
    pub(in crate::app) fn drop_target_at(&self, point: Point) -> Option<(String, Rect)> {
        let (place, row) = self.places.place_at(point)?;
        match crate::app::shell::place_view(&place)? {
            View::Playlist(id) if self.lib.playlist(&id).is_some() => Some((id, row)),
            _ => None,
        }
    }

    /// The dragged songs were let go: onto a playlist they join it, as one
    /// step to undo.
    pub(in crate::app) fn drop_tracks(&mut self, state: &mut ui::widget::Edits<'_>) {
        let Some(drag) = self.search.drag.take() else {
            return;
        };
        self.dirty = true;
        let Some((playlist, _)) = self.drop_target_at(drag.at) else {
            return;
        };
        let _gesture = self
            .dispatcher
            .as_ref()
            .map(ui::framework::Dispatcher::begin_group);
        let mut added = 0usize;
        for track in &drag.tracks {
            self.import_live_track(track, state);
            if self.lib.playlist(&playlist).is_some_and(|list| {
                list.tracks
                    .contains(&ui::tilo_resource::Id::from_raw(track.as_str()))
            }) {
                continue;
            }
            if let Some(edit) = self.lib.add_to_playlist(&playlist, track) {
                self.perform_action(state, edit);
                self.reload_library(state);
                added += 1;
            }
        }
        let name = self
            .lib
            .playlist(&playlist)
            .map(|list| list.name.clone())
            .unwrap_or_default();
        if added > 0 {
            let what = if added == 1 {
                "1 song".to_owned()
            } else {
                format!("{added} songs")
            };
            self.notify(
                ui::widget::toast::ToastKind::Success,
                format!("Added {what} to {name}"),
            );
        }
    }

    /// The songs being dragged, as a chip that follows the pointer, and the
    /// playlist they would join lifted.
    pub(in crate::app) fn paint_track_drag(&self, ctx: &mut PaintCtx<'_>) {
        let Some(drag) = self.search.drag.clone() else {
            return;
        };
        if let Some((_, row)) = self.drop_target_at(drag.at) {
            // A playlist row's corner, concentric with the ring just outside it.
            let radius = f64::from(ctx.theme.tokens.radii.item) + 1.0;
            let ink = ctx.theme.border_focus;
            ctx.stroke_round(row.inset(-1.0), radius, ink, 2.0);
        }
        let label = match drag.tracks.as_slice() {
            [one] => self.track_title(one),
            many => format!("{} songs", many.len()),
        };
        let theme = ctx.theme;
        let lead = ui::widget::ChipLead::Glyph(Icon::Music);
        let height = ui::widget::ChipFace::height(theme);
        let width = ui::widget::ChipFace::intrinsic_width(&label, lead, false, ctx.text, theme);
        let origin = Point::new(drag.at.x + 12.0, drag.at.y + 12.0);
        let rect = Rect::new(origin.x, origin.y, origin.x + width, origin.y + height);
        ui::widget::ChipFace::new(rect, &label)
            .lead(lead)
            .selected(true)
            .paint(ctx.scene, ctx.transform, ctx.text, theme);
    }

    // ---- the keyboard in the results -----------------------------------------------

    /// The songs of the results, in the order they are shown.
    fn result_songs(&self) -> Vec<String> {
        let results = self.search.engine.results();
        results
            .local
            .iter()
            .chain(results.remote.iter().flat_map(|section| &section.groups))
            .filter(|group| group.kind == SONG_TYPE_ID)
            .flat_map(|group| group.items.iter().map(|item| item.id.clone()))
            .collect()
    }

    /// Up, Down or Enter while the results have the keyboard. Returns
    /// whether the key was theirs.
    pub(in crate::app) fn result_key(
        &mut self,
        key: &winit::keyboard::Key,
        state: &mut ui::widget::Edits<'_>,
    ) -> bool {
        use winit::keyboard::{Key, NamedKey};
        if !matches!(self.view, View::Search | View::Kept(_)) {
            return false;
        }
        let Some(cursor) = self.search.cursor else {
            return false;
        };
        let songs = self.result_songs();
        match key {
            Key::Named(NamedKey::ArrowDown) => {
                self.search.cursor = Some((cursor + 1).min(songs.len().saturating_sub(1)));
            }
            Key::Named(NamedKey::ArrowUp) => {
                self.search.cursor = Some(cursor.saturating_sub(1));
            }
            Key::Named(NamedKey::Enter) => {
                if songs.get(cursor).is_some() {
                    self.dispatch(
                        Action::Play {
                            queue: songs.into(),
                            index: cursor,
                        },
                        state,
                    );
                }
            }
            _ => return false,
        }
        self.dirty = true;
        true
    }

    /// The song the keyboard is on in the results, if any.
    pub(in crate::app) fn result_cursor_song(&self) -> Option<String> {
        let cursor = self.search.cursor?;
        self.result_songs().into_iter().nth(cursor)
    }
}

impl MusicApp {
    /// Keep the person's views in `kept`, a setting that follows them, and
    /// wake the window through `wake` when the catalogue answers.
    pub(crate) fn attach_views(
        &mut self,
        kept: ui::framework::Setting<KeptViews>,
        wake: ui::WakeHandle,
    ) {
        self.kept = Some(kept);
        self.search.engine.set_waker(wake);
    }

    /// Stage a query view for a capture: `text` typed into the window bar's
    /// field with `picked` chips, from the Songs library, and the sample
    /// catalogue's answer in. With `withhold`, the answer to `text` is held
    /// back after `earlier`'s came in: the earlier rows that still match
    /// show while it is on its way.
    pub(crate) fn stage_query(
        &mut self,
        text: &str,
        picked: &[ui::framework::views::Chip],
        earlier: Option<&str>,
    ) {
        use ui::widget::Queryable as _;
        self.view = View::Songs;
        self.query_opened();
        self.search.picked = picked.to_vec();
        if let Some(earlier) = earlier {
            self.set_query_text(earlier);
            self.settle_query();
            self.search.withhold = true;
        }
        self.set_query_text(text);
        if earlier.is_none() {
            self.settle_query();
        }
    }

    /// Run what the query has due and take in the answers, now: a capture
    /// shows them in its first frame.
    pub(crate) fn settle_query(&mut self) {
        self.search.engine.poll();
        if let Some(pumped) = &self.search.pumped
            && !self.search.withhold
        {
            pumped.run_pending();
        }
        self.search.engine.poll();
        self.absorb_query_results();
    }

    /// Stage a query view for a capture whose catalogue answer is still on
    /// its way.
    pub(crate) fn stage_query_waiting(&mut self, text: &str) {
        self.search.withhold = true;
        self.stage_query(text, &[], None);
    }

    /// Keep the query view shown, and show it as the kept view it now is.
    pub(crate) fn stage_kept_view(&mut self, text: &str) {
        self.stage_query(text, &[], None);
        self.on_keep_query();
        let query = self.search.engine.results().query.clone();
        if let Some(id) = self.kept_id_of(&query) {
            self.open_section(View::Kept(id));
            self.settle_query();
        }
        self.toast = ui::widget::toast::Notice::default();
    }
}

impl ui::widget::Queryable for MusicApp {
    fn query_opened(&mut self) {
        self.search.open = true;
        self.search.engine.warm();
    }

    fn set_query_text(&mut self, text: &str) {
        text.clone_into(&mut self.search.input);
        self.search.cursor = None;
        let query = self.search_query_spec();
        if self.view != View::Search {
            // The field holding the keyboard is not yet a search: typing
            // is.
            if query.is_empty() {
                return;
            }
            self.open_view(View::Search);
        }
        self.run_query(query);
        self.dirty = true;
    }

    fn enter_results(&mut self) {
        if !self.result_songs().is_empty() {
            self.search.cursor = Some(0);
            self.dirty = true;
        }
    }

    fn query_closed(&mut self) {
        self.search.open = false;
        self.search.input.clear();
        self.search.picked.clear();
        self.search.cursor = None;
        if self.view == View::Search {
            self.go_back();
        } else {
            self.nav.forget(|view| *view == View::Search);
            self.view_shown();
        }
        self.dirty = true;
    }
}

/// What a remote section says in place of rows, once its answer is in and
/// gave none: that it cannot be reached, or that it found nothing. While the
/// answer is on the way its placeholder stands there instead.
fn remote_note(section: &ui::framework::views::RemoteSection) -> Option<String> {
    if let Some(RemoteError(text)) = section.failure() {
        return Some(if text.is_empty() {
            format!("{} can\u{2019}t be reached right now.", section.label)
        } else {
            text.clone()
        });
    }
    (section.is_answered() && section.groups.is_empty())
        .then(|| format!("No matches in {}.", section.label))
}
