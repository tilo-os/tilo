//! The desk widgets and the Super surface, as the overlay host presents
//! them.
//!
//! The decisions (which form each widget is in) come from
//! [`crate::shell::desk_widgets::DeskBoard`]; this module feeds it what the
//! host knows (typing into the focused window, the pointer, the caret, which
//! surfaces are open), lays the widgets out at their edges beside any live
//! call or navigation card, and paints them.
//!
//! The Super surface is the command palette opened field-only
//! ([`ui::CommandPalette::set_field_only_while_empty`]) over a light scrim,
//! with every widget open around it and the time and status under the field. Typing
//! searches exactly as the palette always has; Escape and a click outside
//! close it through the palette's own paths.
//!
//! Nothing here paints a blurred backdrop: glance forms and cards are the
//! opaque overlay surface, the scrim one translucent fill, so an idle desk
//! with widgets showing costs no frames and a shown one no blur pass.

#![allow(
    clippy::arithmetic_side_effects,
    reason = "widget geometry is finite logical-pixel kurbo arithmetic, and deadlines add sub-minute durations to a monotonic instant"
)]

use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use automerge::{AutoCommit, ObjId};
use ui::TextRole;
use ui::presence::PresenceCtx;
use ui::text_render::{TextRenderer, TextWeight};
use ui::theme::Theme;
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect, RoundedRect, Size};
use vello::peniko::{BlendMode, Fill};
use winit::event::{ElementState, MouseButton, WindowEvent};

use super::ShellOverlayHost;
use super::layers::LayerId;
use crate::shell::desk_widgets::calendar::{self, Month};
use crate::shell::desk_widgets::layout::{self, Item};
use crate::shell::desk_widgets::system_monitor::{self, Reading};
use crate::shell::desk_widgets::team::{self, TeamMember};
use crate::shell::desk_widgets::{
    Context, DeskBoard, DeskWidgetPrefs, DockEdge, Form, ShowMode, Slot, WidgetId,
};
use crate::shell::input_policy::SuperGesture;
use crate::shell::now_playing::{NowPlayingView, PlaybackStatus, PlayerSnapshot};
use crate::shell::quick_settings::now_playing::{NowPlayingCard, paint_artwork};

/// How strongly the Super surface dims what is behind it, as a share of
/// the theme's modal scrim: the background stays mostly visible.
pub(crate) const SUPER_SCRIM: f32 = 0.35;

/// The music widget's glance pill.
const MUSIC_GLANCE: Size = Size::new(240.0, 40.0);
/// Space between the Super surface's field and the time card under it.
const SUPER_HEADER_GAP: f64 = ui::tokens::Spacing::BASE.s4 as f64;
/// Width of every open card.
const CARD_WIDTH: f64 = ui::tokens::Metrics::BASE.panel_width_utility as f64;
/// The band above an open card holding its pin.
const PIN_BAND: f64 = ui::tokens::Spacing::BASE.s8 as f64;
const PIN: f64 = ui::tokens::Layout::BASE.pin as f64;
/// Team avatars in the glance form.
const AVATAR: f64 = ui::tokens::Metrics::BASE.bar_control_height as f64;
const AVATAR_STEP: f64 = ui::tokens::Layout::BASE.avatar_step as f64;
const GLANCE_AVATARS: usize = 5;
const TEAM_HEADER: f64 = ui::tokens::Layout::BASE.row_header as f64;
const TEAM_ROW: f64 = ui::tokens::Layout::BASE.team_row as f64;
const TEAM_MORE: f64 = ui::tokens::Spacing::BASE.s6 as f64;
/// The calendar's glance form: the weekday over the day of the month.
const CALENDAR_GLANCE: Size = Size::new(52.0, 56.0);
const CALENDAR_HEADER: f64 = ui::tokens::Metrics::BASE.bar_height as f64;
const CALENDAR_WEEKDAYS: f64 = ui::tokens::Spacing::BASE.s6 as f64;
const CALENDAR_ROW: f64 = ui::tokens::Spacing::BASE.s8 as f64;
/// The system monitor's glance pill.
const MONITOR_GLANCE: Size = Size::new(176.0, 40.0);
const MONITOR_HEADER: f64 = ui::tokens::Layout::BASE.row_header as f64;
const MONITOR_ROW: f64 = ui::tokens::Metrics::BASE.bar_height as f64;
/// A press that moves this far becomes a drag to another edge.
const DRAG_SLOP: f64 = ui::tokens::Metrics::BASE.icon_label_gap as f64;

/// How the Super surface was opened.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SuperSurface {
    /// Super held, not yet grown: the quiet first stage. The widgets' glance
    /// forms and the time; no scrim, no field, nothing over the windows. A
    /// chord takes it away at once.
    Glimpse,
    /// Super held: closes when it comes up, unless the person used it.
    Peek,
    /// Super tapped (or a peek taken up): stays until Escape or a click
    /// outside.
    Pinned,
}

/// A press on a widget, and the drag it may become.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Press {
    id: WidgetId,
    at: Point,
    dragging: bool,
}

/// What the host keeps for the desk between frames.
pub(crate) struct DeskState {
    board: DeskBoard,
    prefs: DeskWidgetPrefs,
    /// Write choices made on the desk (a pin, a drag to another edge) to
    /// the settings file. Only the live session does; fixtures never touch
    /// the person's settings.
    persist_prefs: bool,
    slots: Vec<Slot>,
    edges: BTreeMap<WidgetId, DockEdge>,
    /// How much of each widget shows, and how open its card is
    /// (`ui::motion::Ambient`).
    shown: BTreeMap<WidgetId, ui::motion::Presence>,
    opened: BTreeMap<WidgetId, ui::motion::Presence>,
    super_surface: Option<SuperSurface>,
    /// Where the time card was last painted.
    super_header_rect: std::cell::Cell<Option<Rect>>,
    /// The time card's own arrival, so it shows in the quiet stage before
    /// the search field does.
    super_header: ui::motion::Presence,
    system_activities: Vec<Rect>,
    music_card: NowPlayingCard,
    press: Option<Press>,
    team_count: usize,
    now_playing_revision: u64,
    /// The motion the person allows, as last painted.
    motion: ui::tokens::EffectiveMotion,
    /// The system monitor's readings, sent only while it shows.
    monitor: system_monitor::Monitor,
    monitor_source: Box<dyn system_monitor::Source>,
    /// Wakes the loop when a reading arrives.
    monitor_wake: Option<crate::session_runtime::Wake>,
}

impl Default for DeskState {
    fn default() -> Self {
        Self {
            board: DeskBoard::default(),
            prefs: DeskWidgetPrefs::default(),
            persist_prefs: false,
            slots: Vec::new(),
            edges: BTreeMap::new(),
            shown: BTreeMap::new(),
            opened: BTreeMap::new(),
            super_surface: None,
            super_header_rect: std::cell::Cell::new(None),
            super_header: ui::motion::Presence::closed(),
            system_activities: Vec::new(),
            music_card: NowPlayingCard::new(),
            press: None,
            team_count: 0,
            now_playing_revision: 0,
            motion: ui::tokens::EffectiveMotion::new(ui::tokens::Motion::default(), false),
            monitor: system_monitor::Monitor::default(),
            monitor_source: Box::new(system_monitor::Service::default()),
            monitor_wake: None,
        }
    }
}

impl DeskState {
    /// Follow the person's settings file with desk choices: the live
    /// session's host.
    pub(crate) fn persisting() -> Self {
        Self {
            persist_prefs: true,
            ..Self::default()
        }
    }
}

impl ShellOverlayHost {
    /// Super alone, tapped or held.
    ///
    /// A hold grows in: first the quiet stage ([`SuperSurface::Glimpse`]),
    /// then the whole surface once Super has been held longer or the
    /// pointer moves. A chord at either stage takes it away without a fade.
    pub fn super_gesture(&mut self, gesture: SuperGesture, doc: &AutoCommit, root: &ObjId) {
        match (gesture, self.desk.super_surface) {
            // A second tap closes the surface, and closes plain search as it
            // always has.
            (SuperGesture::Tap, surface) if surface.is_some() || self.palette_visible => {
                self.close_super_surface();
            }
            (SuperGesture::Tap, None) => {
                self.open_super_surface(SuperSurface::Pinned, doc, root);
            }
            (SuperGesture::Peek, None) if !self.palette_visible => {
                self.open_super_glimpse();
            }
            (SuperGesture::PeekFull, Some(SuperSurface::Glimpse)) => {
                self.open_super_surface(SuperSurface::Peek, doc, root);
            }
            (SuperGesture::PeekTaken, Some(SuperSurface::Glimpse)) => {
                self.open_super_surface(SuperSurface::Pinned, doc, root);
            }
            (
                SuperGesture::PeekReleased | SuperGesture::PeekCancelled,
                Some(SuperSurface::Glimpse),
            )
            | (SuperGesture::PeekCancelled, Some(SuperSurface::Peek)) => {
                self.cancel_super_surface();
            }
            (SuperGesture::PeekReleased, Some(SuperSurface::Peek)) => {
                if self.palette.query().is_empty() {
                    self.close_super_surface();
                } else {
                    self.desk.super_surface = Some(SuperSurface::Pinned);
                }
            }
            (SuperGesture::PeekTaken, Some(SuperSurface::Peek)) => {
                self.desk.super_surface = Some(SuperSurface::Pinned);
            }
            _ => return,
        }
        self.bump_chrome();
    }

    /// The quiet first stage of a held Super: the widgets come to their
    /// glance forms and the time card shows where it stands under the
    /// field, so growing into the whole surface moves neither.
    fn open_super_glimpse(&mut self) {
        // Lay the palette out field-only, unseen, so the time card has the
        // field's place to stand under before the field shows.
        self.palette.set_field_only_while_empty(true);
        self.palette_layout_dirty = true;
        self.desk.super_surface = Some(SuperSurface::Glimpse);
        self.open_super_header();
    }

    /// Open the Super surface: search with an empty field at the top, every
    /// widget open around it, the time and status.
    pub fn open_super_surface(&mut self, mode: SuperSurface, doc: &AutoCommit, root: &ObjId) {
        if !self.palette_visible {
            self.open_palette_with_scope(crate::shell::chrome_state::SearchScope::All);
        }
        self.palette.set_field_only_while_empty(true);
        self.palette_layout_dirty = true;
        self.desk.super_surface = Some(mode);
        self.open_super_header();
        self.refresh_search_results(doc, root);
    }

    fn open_super_header(&mut self) {
        self.desk.super_header.show(
            true,
            ui::motion::Ambient::ASKED,
            self.desk.motion,
            ui::tokens::MotionSource::Pointer,
            ui::time::now(),
        );
    }

    /// Close the Super surface (and the search it holds).
    pub fn close_super_surface(&mut self) {
        self.palette_visible = false;
        self.palette.close();
        self.end_super_surface();
    }

    /// The hold was a chord after all, or Super came up during the quiet
    /// stage: everything the hold brought is gone at once, with no fade
    /// for the eye to follow while the chord runs.
    fn cancel_super_surface(&mut self) {
        self.close_super_surface();
        self.palette_was_visible = false;
        self.palette_transition = ui::motion::Presence::closed();
    }

    fn end_super_surface(&mut self) {
        self.desk.super_surface = None;
        self.desk.super_header = ui::motion::Presence::closed();
        self.palette.set_field_only_while_empty(false);
        self.palette_layout_dirty = true;
    }

    /// How the Super surface is open, if it is.
    pub fn super_surface(&self) -> Option<SuperSurface> {
        self.desk.super_surface
    }

    /// Show and drive `source`'s players instead of the session's, for a
    /// scenario capture.
    #[cfg(feature = "hosted-test-control")]
    pub fn set_now_playing_source_for_shots(
        &mut self,
        source: std::rc::Rc<dyn crate::shell::now_playing::NowPlayingSource>,
    ) {
        self.now_playing_source = source;
        self.bump_chrome();
    }

    /// The person's desk choices, as Settings and the desk itself wrote
    /// them.
    pub fn set_desk_widget_prefs(&mut self, prefs: DeskWidgetPrefs) {
        if self.desk.prefs != prefs {
            self.desk.prefs = prefs;
            self.bump_chrome();
        }
    }

    pub fn desk_widget_prefs(&self) -> &DeskWidgetPrefs {
        &self.desk.prefs
    }

    /// The form `id` is in now.
    pub fn desk_form(&self, id: WidgetId) -> Form {
        self.desk.board.form(id)
    }

    /// Where `id` is laid out this frame.
    pub fn desk_slot(&self, id: WidgetId) -> Option<Slot> {
        self.desk.slots.iter().find(|slot| slot.id == id).copied()
    }

    /// The live calls and navigation cards floating on the desktop: widgets
    /// step past them at the edges.
    pub fn set_system_activity_rects(&mut self, rects: Vec<Rect>) {
        if self.desk.system_activities != rects {
            self.desk.system_activities = rects;
            self.bump_chrome();
        }
    }

    /// The last key the person typed into the focused window, read once a
    /// tick. A newer one sends the widgets away and holds back banners not
    /// addressed to them until they pause.
    pub fn note_client_typing(&mut self, last: Option<Instant>, now: Instant) {
        let Some(at) = last else {
            return;
        };
        if self.desk.board.last_typed().is_some_and(|seen| seen >= at) {
            return;
        }
        self.desk.board.note_typed(at, &self.desk.slots);
        if let Some(chrome) = self.chrome.as_ref() {
            chrome
                .borrow_mut()
                .set_typing_until(self.desk.board.typing_until());
        }
        if self.update_desk(now) {
            self.bump_chrome();
        }
    }

    /// Whether another surface owns the screen, so widgets stay away.
    fn desk_covered(&self) -> bool {
        if self.onboarding.is_some() {
            return true;
        }
        if self.palette_visible && self.desk.super_surface.is_none() {
            return true;
        }
        let flags = self.overlay_flags();
        let fullscreen = self
            .chrome
            .as_ref()
            .is_some_and(|chrome| chrome.borrow().fullscreen_group().is_some());
        fullscreen
            || flags.capture
            || LayerId::ALL.into_iter().any(|id| {
                !matches!(
                    id,
                    LayerId::CommandPalette | LayerId::RoomActivity | LayerId::NowPlaying
                ) && self.overlay_is_open(id, flags)
            })
    }

    fn music_players(&self) -> (Vec<PlayerSnapshot>, Option<usize>) {
        let players = self.now_playing_source.players();
        let active = crate::shell::now_playing::active_index(
            &players,
            self.now_playing_preferred.as_deref(),
        );
        (players, active)
    }

    fn music_view(&self, now: Instant) -> Option<NowPlayingView> {
        let (players, _) = self.music_players();
        NowPlayingView::new(
            players,
            self.now_playing_preferred.as_deref(),
            Vec::new(),
            now,
        )
    }

    /// People other than this person in the active workspace, without
    /// presence (counting only).
    fn team_count(&self) -> usize {
        let Some(chrome) = self.chrome.as_ref() else {
            return 0;
        };
        let state = chrome.borrow();
        let local_user = state.active_local_user();
        let own = state.local_user_id.clone();
        tilo_desktop::collaborators::collaborator_roster_rows(&state.tree().desktop, &local_user)
            .iter()
            .filter(|row| !row.invited && row.id != own && row.id != local_user)
            .count()
    }

    fn team_members(&self, presence: &PresenceCtx) -> Vec<TeamMember> {
        let Some(chrome) = self.chrome.as_ref() else {
            return Vec::new();
        };
        let state = chrome.borrow();
        let local_user = state.active_local_user();
        let own = state.local_user_id.clone();
        team::members(
            &state.tree().desktop,
            &local_user,
            &own,
            presence,
            std::time::SystemTime::now(),
        )
    }

    /// Read the system monitor from `source` instead of the session's
    /// service.
    #[cfg(test)]
    pub(crate) fn set_monitor_source_for_tests(&mut self, source: Box<dyn system_monitor::Source>) {
        self.desk.monitor_source = source;
    }

    /// What wakes the loop when a desk widget's reading arrives from
    /// another thread.
    pub fn set_desk_wake(&mut self, wake: crate::session_runtime::Wake) {
        self.desk.monitor_wake = Some(wake);
    }

    /// The clock the desk reads the date from: the top bar's, a fixture's
    /// pinned one included.
    fn desk_clock(&self) -> tilo_desktop::chrome::ClockSnapshot {
        self.chrome
            .as_ref()
            .and_then(|chrome| chrome.borrow().hosted_inputs.fixture_clock())
            .unwrap_or_else(crate::shell::topbar_chrome::clock_snapshot)
    }

    /// The month the calendar shows: the one today is in.
    fn calendar_month(&self) -> Month {
        use chrono::TimeZone as _;
        let secs = self.desk_clock().unix_seconds;
        let today = chrono::Local
            .timestamp_opt(secs, 0)
            .single()
            .map_or_else(|| ui::clock::local_now().date_naive(), |at| at.date_naive());
        Month::around(today, ui::format::first_weekday())
    }

    fn widget_sizes(&self, id: WidgetId, now: Instant) -> (Size, Size, bool) {
        let edge = self.desk.prefs.get(id).edge;
        match id {
            WidgetId::Music => {
                let view = self.music_view(now);
                let open_h = view
                    .as_ref()
                    .map_or(120.0, |view| self.desk.music_card.preferred_height(view));
                (
                    MUSIC_GLANCE,
                    Size::new(CARD_WIDTH, open_h + PIN_BAND),
                    view.is_some(),
                )
            }
            WidgetId::Team => {
                let n = self.desk.team_count;
                (
                    team_glance_size(edge, n),
                    Size::new(CARD_WIDTH, team_card_height(n) + PIN_BAND),
                    n > 0,
                )
            }
            // The date is always there to show.
            WidgetId::Calendar => (
                CALENDAR_GLANCE,
                Size::new(CARD_WIDTH, calendar_card_height() + PIN_BAND),
                true,
            ),
            // Read once it shows; the kernel always offers memory.
            WidgetId::SystemMonitor => (
                MONITOR_GLANCE,
                Size::new(
                    CARD_WIDTH,
                    MONITOR_HEADER + MONITOR_ROW * 3.0 + 8.0 + PIN_BAND,
                ),
                true,
            ),
        }
    }

    /// Lay the widgets out and decide their forms at `now`. Returns whether
    /// anything on the desk changed.
    pub(crate) fn update_desk(&mut self, now: Instant) -> bool {
        if !self.palette_visible
            && self
                .desk
                .super_surface
                .is_some_and(|surface| surface != SuperSurface::Glimpse)
        {
            // Closed through the palette's own paths (Escape, a click
            // outside, an activated result).
            self.end_super_surface();
        }
        self.desk.team_count = self.team_count();
        let items: Vec<Item> = WidgetId::ALL
            .into_iter()
            .map(|id| {
                let (glance, open, has_content) = self.widget_sizes(id, now);
                Item {
                    id,
                    edge: self.desk.prefs.get(id).edge,
                    glance,
                    open,
                    has_content,
                }
            })
            .collect();
        let super_up = self
            .desk
            .super_surface
            .is_some_and(|surface| surface != SuperSurface::Glimpse);
        let super_glance = self.desk.super_surface == Some(SuperSurface::Glimpse);
        let area = self.overlay_area();
        // The strip ribbon on the right edge is where the strip switcher
        // opens: a widget waiting there would take the pointer that reaches
        // for it. Widgets on that edge step past it, as past a call card.
        let mut occupied = self.desk.system_activities.clone();
        let ribbon = self.strip_switcher_zone();
        if ribbon.area() > 0.0 {
            occupied.push(ribbon.inflate(layout::MARGIN, layout::MARGIN));
        }
        let slots = layout::place(area, &items, &occupied, super_up);
        let mut changed = slots != self.desk.slots;
        self.desk.slots = slots;
        self.desk.edges = items.iter().map(|item| (item.id, item.edge)).collect();
        let strip = self
            .chrome
            .as_ref()
            .and_then(|chrome| chrome.borrow().active_strip_id())
            .map(|strip| strip.0);
        let covered = self.desk_covered();
        let ctx = Context {
            now,
            prefs: &self.desk.prefs,
            strip,
            caret: self.text_caret,
            super_surface: super_up,
            super_glance,
            covered,
            slots: &self.desk.slots,
        };
        changed |= self.desk.board.update(&ctx);
        let motion = self.desk.motion;
        let super_asked = self.desk.super_surface.is_some();
        for slot in &self.desk.slots {
            let form = self.desk.board.form(slot.id);
            let shown = form != Form::Away;
            let open = form == Form::Open;
            let asked = super_asked
                || self.desk.board.reached() == Some(slot.id)
                || self.desk.board.opened() == Some(slot.id);
            let spec = ui::motion::Ambient::spec(asked);
            let source = ui::tokens::MotionSource::Pointer;
            self.desk
                .shown
                .entry(slot.id)
                .or_insert_with(ui::motion::Presence::closed)
                .show(shown, spec, motion, source, now);
            // A card only ever opens on purpose: asked for.
            self.desk
                .opened
                .entry(slot.id)
                .or_insert_with(ui::motion::Presence::closed)
                .show(open, spec, motion, source, now);
        }
        // The system monitor is sent readings only while it shows; each one
        // wakes the loop, so nothing here waits on a deadline.
        if self.desk.board.form(WidgetId::SystemMonitor) == Form::Away {
            self.desk.monitor.pause(self.desk.monitor_source.as_mut());
        } else {
            let wake = self.desk.monitor_wake.clone();
            changed |= self
                .desk
                .monitor
                .show(self.desk.monitor_source.as_mut(), wake);
        }
        // A track change wakes the loop through the observer; repaint once.
        let revision = crate::shell::mpris::now_playing_revision();
        if revision != self.desk.now_playing_revision {
            self.desk.now_playing_revision = revision;
            changed |= self.desk.board.form(WidgetId::Music) != Form::Away;
        }
        if let Some(chrome) = self.chrome.as_ref() {
            changed |= chrome.borrow_mut().release_waiting_banners(now);
        }
        changed
    }

    /// When the desk next changes on its own.
    pub(crate) fn desk_wake_deadline(&self, now: Instant) -> Option<Instant> {
        let clock = self.desk.super_surface.is_some().then(|| {
            // The surface shows the time: wake at the next minute.
            let secs = ui::clock::local_now().timestamp().rem_euclid(60);
            now + Duration::from_secs(u64::try_from(60 - secs).unwrap_or(60))
        });
        let showing = |id| self.desk.board.form(id) != Form::Away;
        // A showing calendar turns the page at midnight.
        let day = showing(WidgetId::Calendar).then(|| {
            use chrono::Timelike as _;
            let secs = ui::clock::local_now().num_seconds_from_midnight();
            now + Duration::from_secs(u64::from(86_400_u32.saturating_sub(secs).max(1)))
        });
        self.desk
            .board
            .deadline(now)
            .into_iter()
            .chain(clock)
            .chain(day)
            .min()
    }

    /// Whether a widget is fading or folding.
    pub(crate) fn desk_animating(&self, now: Instant) -> bool {
        self.desk
            .shown
            .values()
            .chain(self.desk.opened.values())
            .chain(std::iter::once(&self.desk.super_header))
            .any(|presence| presence.is_moving(now))
    }

    /// A pointer sample at `p`, in desktop coordinates.
    pub(crate) fn desk_pointer(&mut self, p: Point, now: Instant) {
        self.desk.board.on_pointer(p, now, &self.desk.slots);
        if let Some(press) = self.desk.press.as_mut()
            && !press.dragging
            && (p - press.at).hypot() > DRAG_SLOP
        {
            press.dragging = true;
        }
        if self.update_desk(now) || self.desk.press.is_some_and(|press| press.dragging) {
            self.bump_chrome();
        }
    }

    pub(crate) fn desk_pointer_left(&mut self, now: Instant) {
        self.desk.board.pointer_left(now);
    }

    /// The widget form under `p`, if one is showing there.
    fn desk_hit(&self, p: Point) -> Option<(WidgetId, Form, Rect)> {
        self.desk
            .slots
            .iter()
            .find_map(|slot| match self.desk.board.form(slot.id) {
                Form::Open if slot.open.contains(p) => Some((slot.id, Form::Open, slot.open)),
                Form::Glance if slot.glance.contains(p) => {
                    Some((slot.id, Form::Glance, slot.glance))
                }
                _ => None,
            })
    }

    /// A press, drag or release on a showing widget. `true` when the desk
    /// took it.
    pub(crate) fn route_desk_event(&mut self, event: &WindowEvent) -> bool {
        let WindowEvent::MouseInput {
            state,
            button: MouseButton::Left,
            ..
        } = event
        else {
            return false;
        };
        let Some(p) = self.last_cursor else {
            return false;
        };
        match state {
            ElementState::Pressed => {
                let Some((id, form, rect)) = self.desk_hit(p) else {
                    return false;
                };
                if form == Form::Open && self.desk_open_click(id, rect, p) {
                    return true;
                }
                self.desk.press = Some(Press {
                    id,
                    at: p,
                    dragging: false,
                });
                true
            }
            ElementState::Released => {
                let Some(press) = self.desk.press.take() else {
                    return false;
                };
                if press.dragging {
                    let edge = layout::nearest_edge(self.overlay_area(), p);
                    self.set_widget_edge(press.id, edge);
                } else if self.desk.board.form(press.id) == Form::Glance {
                    self.desk.board.open(press.id);
                }
                self.update_desk(Instant::now());
                self.bump_chrome();
                true
            }
        }
    }

    /// A press on `id`'s open card at `p`: its pin, or (music) its
    /// transport. `true` when a control took it.
    fn desk_open_click(&mut self, id: WidgetId, open: Rect, p: Point) -> bool {
        if pin_rect(open).contains(p) {
            self.toggle_widget_pin(id);
            return true;
        }
        if id == WidgetId::Music
            && let Some(view) = self.music_view(Instant::now())
        {
            let card = card_rect(open);
            self.desk.music_card.layout(card);
            if card.contains(p) {
                let action = self.desk.music_card.click(&view, p);
                if !matches!(
                    action,
                    crate::shell::quick_settings::now_playing::CardAction::None
                ) {
                    // The same dispatch the Quick Settings card uses: the
                    // command goes to exactly that player's bus, whichever
                    // workspace its app runs in; nothing switches.
                    self.apply_now_playing_action(action, &view);
                    return true;
                }
            }
        }
        false
    }

    /// "Always show on this strip": on for the active strip, or off again.
    pub fn toggle_widget_pin(&mut self, id: WidgetId) {
        let strip = self
            .chrome
            .as_ref()
            .and_then(|chrome| chrome.borrow().active_strip_id())
            .map(|strip| strip.0);
        let mut prefs = self.desk.prefs.get(id).clone();
        if prefs.always_on(strip) {
            prefs.always_on_strips.retain(|s| Some(*s) != strip);
            if prefs.always_on_strips.is_empty() {
                prefs.show = ShowMode::Pause;
            }
        } else {
            prefs.show = ShowMode::Always;
            if let Some(strip) = strip
                && !prefs.always_on_strips.contains(&strip)
            {
                prefs.always_on_strips.push(strip);
            }
        }
        self.store_widget_prefs(id, prefs);
    }

    /// Move `id` to `edge`; it snaps there.
    pub fn set_widget_edge(&mut self, id: WidgetId, edge: DockEdge) {
        let mut prefs = self.desk.prefs.get(id).clone();
        prefs.edge = edge;
        self.store_widget_prefs(id, prefs);
    }

    fn store_widget_prefs(&mut self, id: WidgetId, prefs: crate::shell::desk_widgets::WidgetPrefs) {
        if self.desk.persist_prefs
            && let Err(error) = settings_store::desk_widgets::prefs_key(id).set(prefs.clone())
        {
            tracing::warn!(target: "tilo::desk", %error, "desk widget choice not saved");
        }
        *self.desk.prefs.get_mut(id) = prefs;
        self.bump_chrome();
    }

    /// Named geometry for `tilo-control list-targets`.
    pub(crate) fn desk_test_rects(&self) -> Vec<(String, Rect)> {
        self.desk
            .slots
            .iter()
            .filter_map(|slot| {
                let name = match slot.id {
                    WidgetId::Music => "desk.music",
                    WidgetId::Team => "desk.team",
                    WidgetId::Calendar => "desk.calendar",
                    WidgetId::SystemMonitor => "desk.system-monitor",
                };
                match self.desk.board.form(slot.id) {
                    Form::Away => None,
                    Form::Glance => Some((name.to_string(), slot.glance)),
                    Form::Open => Some((format!("{name}.open"), slot.open)),
                }
            })
            .collect()
    }

    /// Paint every showing widget. Returns the bounds painted.
    pub(crate) fn draw_desk(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) -> Vec<Rect> {
        let now = Instant::now();
        self.desk.motion = theme.motion();
        let mut painted = Vec::new();
        let team_showing = self.desk.board.form(WidgetId::Team) != Form::Away
            || self
                .desk
                .shown
                .get(&WidgetId::Team)
                .is_some_and(|t| t.is_visible(now));
        let members = if team_showing {
            self.team_members(presence)
        } else {
            Vec::new()
        };
        let music = self.music_view(now);
        let month = self.calendar_month();
        let reading = self.desk.monitor.reading();
        let drag = self
            .desk
            .press
            .filter(|press| press.dragging)
            .zip(self.last_cursor)
            .map(|(press, p)| (press.id, p - press.at));
        // The strip switcher's card is where the person is looking: a widget
        // glance under it steps aside while it is open, not over it.
        let switcher_card = self
            .strip_switcher
            .is_open()
            .then(|| self.strip_switcher.layout().card);
        for slot in self.desk.slots.clone() {
            let shown = self
                .desk
                .shown
                .get(&slot.id)
                .map_or(0.0, |t| ui::motion::unit_opacity(t.progress(now)));
            if shown <= f32::EPSILON || switcher_card.is_some_and(|card| card.overlaps(slot.glance))
            {
                continue;
            }
            let open = self
                .desk
                .opened
                .get(&slot.id)
                .map_or(0.0, |t| ui::motion::unit_opacity(t.progress(now)));
            let edge = self
                .desk
                .edges
                .get(&slot.id)
                .copied()
                .unwrap_or_else(|| slot.id.default_edge());
            // Opacity only: a widget fades in place, and only a drag moves
            // it.
            let shift = drag
                .filter(|(id, _)| *id == slot.id)
                .map_or(vello::kurbo::Vec2::ZERO, |(_, delta)| delta);
            let at = transform.pre_translate(shift);
            let glance_alpha = shown * (1.0 - open);
            if glance_alpha > f32::EPSILON {
                with_alpha(
                    scene,
                    at,
                    slot.glance.inflate(24.0, 24.0),
                    glance_alpha,
                    |scene| match slot.id {
                        WidgetId::Music => {
                            if let Some(view) = music.as_ref() {
                                paint_music_glance(
                                    scene,
                                    at,
                                    text,
                                    theme,
                                    slot.glance,
                                    view.player(),
                                );
                            }
                        }
                        WidgetId::Team => {
                            paint_team_glance(scene, at, text, theme, slot.glance, edge, &members);
                        }
                        WidgetId::Calendar => {
                            paint_calendar_glance(scene, at, text, theme, slot.glance, &month);
                        }
                        WidgetId::SystemMonitor => {
                            paint_monitor_glance(scene, at, text, theme, slot.glance, reading);
                        }
                    },
                );
                painted.push(at.transform_rect_bbox(slot.glance.inflate(24.0, 32.0)));
            }
            let open_alpha = shown * open;
            if open_alpha > f32::EPSILON {
                let content = OpenContent {
                    music: music.as_ref(),
                    members: &members,
                    month: &month,
                    reading,
                };
                self.paint_open_card(scene, (at, open_alpha), text, theme, slot, content);
                painted.push(at.transform_rect_bbox(slot.open.inflate(24.0, 32.0)));
            }
        }
        painted
    }

    /// `slot`'s open card, pin band included, at `alpha`.
    fn paint_open_card(
        &mut self,
        scene: &mut Scene,
        (at, alpha): (Affine, f32),
        text: &mut TextRenderer,
        theme: &Theme,
        slot: Slot,
        content: OpenContent<'_>,
    ) {
        let pinned = self.desk.prefs.get(slot.id).always_on(
            self.chrome
                .as_ref()
                .and_then(|chrome| chrome.borrow().active_strip_id())
                .map(|strip| strip.0),
        );
        let card = card_rect(slot.open);
        let cursor = self.last_cursor;
        with_alpha(scene, at, slot.open.inflate(24.0, 24.0), alpha, |scene| {
            paint_pin(scene, at, theme, pin_rect(slot.open), pinned);
            match slot.id {
                WidgetId::Music => {
                    if let Some(view) = content.music {
                        paint_card_ground(scene, at, theme, card);
                        self.desk.music_card.set_text_scale(theme.text_scale);
                        self.desk.music_card.layout(card);
                        let radius = ui::widget::floating_surface::card_radius(card, theme);
                        scene.push_layer(
                            Fill::NonZero,
                            BlendMode::default(),
                            1.0,
                            at,
                            &RoundedRect::from_rect(card, radius),
                        );
                        self.desk
                            .music_card
                            .draw(scene, at, text, theme, card, cursor, view);
                        scene.pop_layer();
                        // Its artwork may reach the edge: the card's edge
                        // stays on top.
                        ui::widget::floating_surface::paint_card_edge(scene, at, card, theme);
                    }
                }
                WidgetId::Team => {
                    paint_card_ground(scene, at, theme, card);
                    paint_team_card(scene, at, text, theme, card, content.members);
                }
                WidgetId::Calendar => {
                    paint_card_ground(scene, at, theme, card);
                    paint_calendar_card(scene, at, text, theme, card, content.month);
                }
                WidgetId::SystemMonitor => {
                    paint_card_ground(scene, at, theme, card);
                    paint_monitor_card(scene, at, text, theme, card, content.reading);
                }
            }
        });
    }

    /// The search palette and the desk, in their stacking order: the scrim
    /// (light over the Super surface), the widgets, the time and status,
    /// then the palette card. Each is its own damage part, so a keystroke
    /// in the field repaints the card alone and a widget's fade only its
    /// own rect.
    pub(crate) fn draw_search_and_desk(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        let palette_now = ui::time::now();
        let palette_shown = self.palette_transition.is_visible(palette_now);
        let desktop = transform.transform_rect_bbox(self.rect);
        let super_up = self.super_surface().is_some();
        super::overlay_part(scene, "shell.palette-scrim", desktop, |scene| {
            if palette_shown {
                let openness =
                    ui::motion::unit_opacity(self.palette_transition.progress(palette_now));
                // The person is looking at their desk, not away from it.
                let strength = if super_up {
                    openness * SUPER_SCRIM
                } else {
                    openness
                };
                super::draw_modal_scrim_at(scene, transform, self.rect, strength, theme);
            }
            None
        });
        super::overlay_part(scene, "shell.desk", desktop, |scene| {
            Some(self.draw_desk(scene, transform, text_renderer, theme, presence))
        });
        super::overlay_part(scene, "shell.super-header", desktop, |scene| {
            // Typing lists results where the card stands: it steps away.
            // It arrives with the quiet stage, before the field does.
            let openness = if super_up && self.palette.shows_field_only() {
                ui::motion::unit_opacity(
                    self.palette_transition
                        .progress(palette_now)
                        .max(self.desk.super_header.progress(palette_now)),
                )
            } else {
                0.0
            };
            if openness > f32::EPSILON {
                self.draw_super_header(scene, transform, text_renderer, theme, openness)
            } else {
                Some(Vec::new())
            }
        });
        super::overlay_part(scene, "shell.palette", desktop, |scene| {
            palette_shown
                .then(|| self.draw_palette_card(scene, transform, text_renderer, theme, presence))
                .flatten()
        });
    }

    /// Where the Super surface's time card was last painted.
    pub fn super_header_rect_for_tests(&self) -> Option<Rect> {
        self.desk.super_header_rect.get()
    }

    /// The time, the date and the machine's status under the Super
    /// surface's search field.
    pub(crate) fn draw_super_header(
        &self,
        scene: &mut Scene,
        transform: Affine,
        text: &mut TextRenderer,
        theme: &Theme,
        openness: f32,
    ) -> Option<Vec<Rect>> {
        if self.desk.super_surface.is_none() && openness <= f32::EPSILON {
            return None;
        }
        // The same clock the top bar shows, a fixture's pinned one included.
        let clock = self
            .chrome
            .as_ref()
            .and_then(|chrome| chrome.borrow().hosted_inputs.fixture_clock())
            .unwrap_or_else(crate::shell::topbar_chrome::clock_snapshot);
        let status = self
            .chrome
            .as_ref()
            .and_then(|chrome| chrome.borrow().status.reading());
        let colors = &theme.tokens.colors;
        let time_style = theme.text(TextRole::LargeTitle);
        let date_style = theme.text(TextRole::Body);
        let status_style = theme.text(TextRole::Caption);
        let date = format!("{} {}", clock.weekday, clock.date);
        let (time_w, time_h) = text.measure_style(&clock.clock, None, &time_style);
        let (date_w, date_h) = text.measure_style(&date, None, &date_style);
        // No reading, no line: the card never holds a stand-in.
        let (status_w, status_h) = status.as_deref().map_or((0.0, 0.0), |status| {
            text.measure_style(status, None, &status_style)
        });
        let width = f64::from(time_w.max(date_w).max(status_w)) + 32.0;
        let height = f64::from(time_h + date_h + status_h) + 28.0;
        // Under the field, on its left edge: the window's tab bar and
        // toolbar end above the field's row, so the card never covers
        // window chrome, and it never covers the field.
        // Where the field stands, or will stand once the quiet stage grows:
        // the card never moves between the two.
        let frame = self.palette_frame(theme.text_scale);
        let field = Rect::new(
            frame.x0,
            frame.y0,
            frame.x1,
            frame.y0 + self.palette.desired_size().1,
        );
        let card = Rect::new(
            field.x0,
            field.y1 + SUPER_HEADER_GAP,
            field.x0 + width,
            field.y1 + SUPER_HEADER_GAP + height,
        );
        self.desk.super_header_rect.set(Some(card));
        with_alpha(
            scene,
            transform,
            card.inflate(24.0, 24.0),
            openness,
            |scene| {
                paint_card_ground(scene, transform, theme, card);
                let mut y = card.y0 + 14.0;
                let status_line = status
                    .iter()
                    .map(|status| (status, &status_style, colors.text_dim, status_h));
                for (line, style, color, h) in [
                    (&clock.clock, &time_style, colors.text_primary, time_h),
                    (&date, &date_style, colors.text_primary, date_h),
                ]
                .into_iter()
                .chain(status_line)
                {
                    text.draw_style(
                        scene,
                        line,
                        color,
                        transform.pre_translate((card.x0 + 16.0, y).into()),
                        None,
                        style,
                    );
                    y += f64::from(h);
                }
            },
        );
        Some(vec![
            transform.transform_rect_bbox(card.inflate(24.0, 32.0)),
        ])
    }
}

/// What an open card shows, read once per frame.
#[derive(Clone, Copy)]
struct OpenContent<'a> {
    music: Option<&'a NowPlayingView>,
    members: &'a [TeamMember],
    month: &'a Month,
    reading: Reading,
}

/// Run `paint` at `alpha`, in a layer only while it is fading.
fn with_alpha(
    scene: &mut Scene,
    transform: Affine,
    clip: Rect,
    alpha: f32,
    paint: impl FnOnce(&mut Scene),
) {
    let fading = alpha < 1.0 - f32::EPSILON;
    if fading {
        scene.push_layer(Fill::NonZero, BlendMode::default(), alpha, transform, &clip);
    }
    paint(scene);
    if fading {
        scene.pop_layer();
    }
}

/// The card part of an open rect, under its pin band.
fn card_rect(open: Rect) -> Rect {
    Rect::new(open.x0, open.y0 + PIN_BAND, open.x1, open.y1)
}

/// The pin's target in an open rect's band.
fn pin_rect(open: Rect) -> Rect {
    Rect::new(open.x1 - PIN, open.y0, open.x1, open.y0 + PIN)
}

fn team_glance_size(edge: DockEdge, people: usize) -> Size {
    let shown = people.clamp(1, GLANCE_AVATARS);
    let long = 12.0 + AVATAR + AVATAR_STEP * ui::num::f64_of_usize(shown - 1);
    match edge {
        DockEdge::Left | DockEdge::Right => Size::new(AVATAR + 12.0, long),
        DockEdge::Bottom | DockEdge::BottomLeft | DockEdge::BottomRight => {
            Size::new(long, AVATAR + 12.0)
        }
    }
}

fn calendar_card_height() -> f64 {
    CALENDAR_HEADER
        + CALENDAR_WEEKDAYS
        + CALENDAR_ROW * ui::num::f64_of_usize(calendar::WEEKS)
        + 8.0
}

fn team_card_height(people: usize) -> f64 {
    let rows = people.min(team::MAX_ROWS);
    let more = if people > team::MAX_ROWS {
        TEAM_MORE
    } else {
        0.0
    };
    TEAM_HEADER + TEAM_ROW * ui::num::f64_of_usize(rows) + more + 8.0
}

/// The ground every desk card stands on: the one floating card
/// ([`ui::widget::floating_surface::paint_card`]), as banners and the
/// palette.
fn paint_card_ground(scene: &mut Scene, transform: Affine, theme: &Theme, rect: Rect) {
    ui::widget::floating_surface::paint_card(scene, transform, rect, theme);
}

fn paint_pill_ground(scene: &mut Scene, transform: Affine, theme: &Theme, rect: Rect) {
    let radius = ui::chrome::ChromeRole::Selector.radius(theme, rect.height());
    let shadow = theme.tokens.elevation.floating.with_alpha_scaled(0.6);
    ui::surface::shadow(scene, transform, rect, radius, &shadow);
    ui::surface::card(
        scene,
        transform,
        rect,
        radius,
        theme.tokens.colors.surfaces.overlay,
    );
    ui::paint::stroke_round(
        scene,
        transform,
        rect,
        radius,
        ui::chrome::panel_border(theme),
        1.0,
    );
}

#[allow(
    clippy::cast_possible_truncation,
    reason = "widths are bounded by the display; f32 is the shaping precision"
)]
fn paint_music_glance(
    scene: &mut Scene,
    transform: Affine,
    text: &mut TextRenderer,
    theme: &Theme,
    rect: Rect,
    player: &PlayerSnapshot,
) {
    paint_pill_ground(scene, transform, theme, rect);
    let art = Rect::from_center_size(
        Point::new(rect.x0 + 6.0 + 14.0, rect.center().y),
        (28.0, 28.0),
    );
    paint_artwork(scene, transform, text, theme, art, player, None);
    let glyph = Rect::from_center_size(Point::new(rect.x1 - 20.0, rect.center().y), (14.0, 14.0));
    let icon = if player.status == PlaybackStatus::Playing {
        ui::icons::Icon::Pause
    } else {
        ui::icons::Icon::Play
    };
    ui::icons::draw_icon(
        scene,
        transform,
        glyph,
        icon,
        theme.tokens.colors.text_dim,
        1.5,
    );
    let style = theme
        .text(TextRole::Caption)
        .with_weight(TextWeight::Semibold);
    let line = match player.subline() {
        Some(sub) => format!("{} \u{b7} {sub}", player.headline()),
        None => player.headline().to_string(),
    };
    let x = art.x1 + 8.0;
    let width = (glyph.x0 - 8.0 - x).max(1.0) as f32;
    let line = text.elide_to_width_style(&line, width, &style);
    let (_, h) = text.measure_style(&line, None, &style);
    text.draw_style(
        scene,
        &line,
        theme.tokens.colors.text_primary,
        transform.pre_translate((x, rect.center().y - f64::from(h) / 2.0).into()),
        None,
        &style,
    );
}

fn paint_avatar(
    scene: &mut Scene,
    transform: Affine,
    text: &mut TextRenderer,
    theme: &Theme,
    rect: Rect,
    member: &TeamMember,
) {
    ui::widget::AvatarFace::new(&member.row.label, &member.row.id)
        .presence(member.row.color)
        .ringed_on(theme.tokens.colors.surfaces.overlay)
        .paint(
            &mut ui::paint::PaintCtx::new(scene, transform, text, theme),
            rect,
        );
}

fn paint_team_glance(
    scene: &mut Scene,
    transform: Affine,
    text: &mut TextRenderer,
    theme: &Theme,
    rect: Rect,
    edge: DockEdge,
    members: &[TeamMember],
) {
    if members.is_empty() {
        return;
    }
    paint_pill_ground(scene, transform, theme, rect);
    let vertical = matches!(edge, DockEdge::Left | DockEdge::Right);
    for (k, member) in members.iter().take(GLANCE_AVATARS).enumerate() {
        let offset = 6.0 + AVATAR_STEP * ui::num::f64_of_usize(k);
        let origin = if vertical {
            Point::new(rect.x0 + 6.0, rect.y0 + offset)
        } else {
            Point::new(rect.x0 + offset, rect.y0 + 6.0)
        };
        paint_avatar(
            scene,
            transform,
            text,
            theme,
            Rect::from_origin_size(origin, (AVATAR, AVATAR)),
            member,
        );
    }
}

#[allow(
    clippy::cast_possible_truncation,
    reason = "widths are bounded by the display; f32 is the shaping precision"
)]
fn paint_team_card(
    scene: &mut Scene,
    transform: Affine,
    text: &mut TextRenderer,
    theme: &Theme,
    card: Rect,
    members: &[TeamMember],
) {
    let colors = &theme.tokens.colors;
    let caption = theme.text(TextRole::Caption);
    let name_style = theme.text(TextRole::Body).with_weight(TextWeight::Semibold);
    let title = crate::shell::i18n::t("desk-team-title");
    let (_, title_h) = text.measure_style(&title, None, &caption);
    text.draw_style(
        scene,
        &title,
        colors.text_dim,
        transform.pre_translate(
            (
                card.x0 + 14.0,
                card.y0 + (TEAM_HEADER - f64::from(title_h)) / 2.0,
            )
                .into(),
        ),
        None,
        &caption,
    );
    let text_x = card.x0 + 14.0 + 32.0 + 10.0;
    let width = (card.x1 - 14.0 - text_x).max(1.0) as f32;
    let mut y = card.y0 + TEAM_HEADER;
    for member in members.iter().take(team::MAX_ROWS) {
        paint_avatar(
            scene,
            transform,
            text,
            theme,
            Rect::new(card.x0 + 14.0, y + 6.0, card.x0 + 46.0, y + 38.0),
            member,
        );
        let mut line_y = y + 4.0;
        let name = text.elide_to_width_style(&member.row.label, width, &name_style);
        let (_, name_h) = text.measure_style(&name, None, &name_style);
        text.draw_style(
            scene,
            &name,
            colors.text_primary,
            transform.pre_translate((text_x, line_y).into()),
            None,
            &name_style,
        );
        line_y += f64::from(name_h);
        if let Some(status) = member.row.status.as_ref() {
            // Who wrote the line: a pencil for their own words, sparkles
            // for a line their machine generated.
            let icon = match status.source {
                tilo_desktop::collaborators::LineSource::Written => ui::icons::Icon::Pencil,
                tilo_desktop::collaborators::LineSource::Generated
                | tilo_desktop::collaborators::LineSource::Observed => ui::icons::Icon::Sparkles,
            };
            let (_, line_h) = text.measure_style("Ag", None, &caption);
            let mark = Rect::from_origin_size(
                Point::new(text_x, line_y + (f64::from(line_h) - 11.0) / 2.0),
                (11.0, 11.0),
            );
            ui::icons::draw_icon(scene, transform, mark, icon, colors.text_dim, 1.2);
            let line = text.elide_to_width_style(&status.text, (width - 16.0).max(1.0), &caption);
            text.draw_style(
                scene,
                &line,
                colors.text_primary,
                transform.pre_translate((text_x + 16.0, line_y).into()),
                None,
                &caption,
            );
            line_y += f64::from(line_h);
        }
        if let Some(place) = member.place.as_ref() {
            let line = text.elide_to_width_style(place, width, &caption);
            text.draw_style(
                scene,
                &line,
                colors.text_dim,
                transform.pre_translate((text_x, line_y).into()),
                None,
                &caption,
            );
        }
        y += TEAM_ROW;
    }
    if members.len() > team::MAX_ROWS {
        let more = crate::shell::i18n::t_args(
            "desk-team-more",
            &[("count", (members.len() - team::MAX_ROWS).into())],
        );
        text.draw_style(
            scene,
            &more,
            colors.text_dim,
            transform.pre_translate((text_x, y + 2.0).into()),
            None,
            &caption,
        );
    }
}

/// Draw `line` centred on `center` in `style` and `color`.
fn draw_centered(
    text: &mut TextRenderer,
    scene: &mut Scene,
    transform: Affine,
    (line, center): (&str, Point),
    style: &ui::text_render::TextStyle,
    color: vello::peniko::Color,
) {
    let (w, h) = text.measure_style(line, None, style);
    text.draw_style(
        scene,
        line,
        color,
        transform
            .pre_translate((center.x - f64::from(w) / 2.0, center.y - f64::from(h) / 2.0).into()),
        None,
        style,
    );
}

/// The calendar's glance: the weekday over the day of the month, on a tile.
fn paint_calendar_glance(
    scene: &mut Scene,
    transform: Affine,
    text: &mut TextRenderer,
    theme: &Theme,
    rect: Rect,
    month: &Month,
) {
    use chrono::Datelike as _;
    let radius = ui::chrome::ChromeRole::Panel.radius(theme, rect.height());
    let shadow = theme.tokens.elevation.floating.with_alpha_scaled(0.6);
    ui::surface::shadow(scene, transform, rect, radius, &shadow);
    ui::surface::card(
        scene,
        transform,
        rect,
        radius,
        theme.tokens.colors.surfaces.overlay,
    );
    ui::paint::stroke_round(
        scene,
        transform,
        rect,
        radius,
        ui::chrome::panel_border(theme),
        1.0,
    );
    let colors = &theme.tokens.colors;
    let weekday = ui::format::weekday_short(month.today.weekday());
    draw_centered(
        text,
        scene,
        transform,
        (&weekday, Point::new(rect.center().x, rect.y0 + 15.0)),
        &theme
            .text(TextRole::Caption)
            .with_weight(TextWeight::Semibold),
        colors.text_dim,
    );
    draw_centered(
        text,
        scene,
        transform,
        (
            &month.today.day().to_string(),
            Point::new(rect.center().x, rect.y1 - 20.0),
        ),
        &theme
            .text(TextRole::Title)
            .with_weight(TextWeight::Semibold),
        colors.text_primary,
    );
}

/// The calendar's open card: the month from the person's first weekday,
/// today marked.
fn paint_calendar_card(
    scene: &mut Scene,
    transform: Affine,
    text: &mut TextRenderer,
    theme: &Theme,
    card: Rect,
    month: &Month,
) {
    let colors = &theme.tokens.colors;
    let heading = theme.text(TextRole::Body).with_weight(TextWeight::Semibold);
    let title = ui::format::month_heading(month.first);
    let (_, title_h) = text.measure_style(&title, None, &heading);
    text.draw_style(
        scene,
        &title,
        colors.text_primary,
        transform.pre_translate(
            (
                card.x0 + 14.0,
                card.y0 + (CALENDAR_HEADER - f64::from(title_h)) / 2.0,
            )
                .into(),
        ),
        None,
        &heading,
    );
    let grid_x0 = card.x0 + 10.0;
    let cell_w = (card.width() - 20.0) / 7.0;
    let center_x = |col: usize| grid_x0 + cell_w * (ui::num::f64_of_usize(col) + 0.5);
    let caption = theme.text(TextRole::Caption);
    let weekdays_y = card.y0 + CALENDAR_HEADER + CALENDAR_WEEKDAYS / 2.0;
    for (col, weekday) in month.columns.iter().enumerate() {
        let label = ui::format::weekday_short(*weekday);
        draw_centered(
            text,
            scene,
            transform,
            (&label, Point::new(center_x(col), weekdays_y)),
            &caption,
            colors.text_dim,
        );
    }
    let today = month.today_cell();
    let body = theme.text(TextRole::Body);
    let rows_y0 = card.y0 + CALENDAR_HEADER + CALENDAR_WEEKDAYS;
    for (row, week) in month.weeks.iter().enumerate() {
        let cy = rows_y0 + CALENDAR_ROW * (ui::num::f64_of_usize(row) + 0.5);
        for (col, day) in week.iter().enumerate() {
            let Some(day) = day else {
                continue;
            };
            let center = Point::new(center_x(col), cy);
            // Today: an inverted disc, the way Tilo marks the one choice.
            let ink = if today == Some((row, col)) {
                let disc = Rect::from_center_size(center, (26.0, 26.0));
                ui::surface::card(scene, transform, disc, 13.0, colors.text_primary);
                colors.surfaces.overlay
            } else {
                colors.text_primary
            };
            draw_centered(
                text,
                scene,
                transform,
                (&day.to_string(), center),
                &body,
                ink,
            );
        }
    }
}

/// The system monitor's glance: load and memory, labels in quiet ink.
fn paint_monitor_glance(
    scene: &mut Scene,
    transform: Affine,
    text: &mut TextRenderer,
    theme: &Theme,
    rect: Rect,
    reading: Reading,
) {
    if reading.is_empty() {
        return;
    }
    paint_pill_ground(scene, transform, theme, rect);
    let colors = &theme.tokens.colors;
    let label_style = theme
        .text(TextRole::Caption)
        .with_weight(TextWeight::Semibold);
    let value_style = theme.text(TextRole::Caption).with_weight(TextWeight::Bold);
    let pairs = [
        ("desk-monitor-cpu-short", reading.cpu_percent),
        ("desk-monitor-memory-short", reading.memory_percent),
    ];
    let mut x = rect.x0 + 14.0;
    for (key, value) in pairs {
        let Some(value) = value else {
            continue;
        };
        let label = crate::shell::i18n::t(key);
        let value = format!("{value}%");
        for (run, style, color) in [
            (&label, &label_style, colors.text_dim),
            (&value, &value_style, colors.text_primary),
        ] {
            let (w, h) = text.measure_style(run, None, style);
            text.draw_style(
                scene,
                run,
                color,
                transform.pre_translate((x, rect.center().y - f64::from(h) / 2.0).into()),
                None,
                style,
            );
            x += f64::from(w) + 4.0;
        }
        x += 10.0;
    }
}

/// The system monitor's open card: one row per reading the machine offers,
/// each with a quiet meter.
fn paint_monitor_card(
    scene: &mut Scene,
    transform: Affine,
    text: &mut TextRenderer,
    theme: &Theme,
    card: Rect,
    reading: Reading,
) {
    let colors = &theme.tokens.colors;
    let caption = theme.text(TextRole::Caption);
    let title = crate::shell::i18n::t("desk-monitor-title");
    let (_, title_h) = text.measure_style(&title, None, &caption);
    text.draw_style(
        scene,
        &title,
        colors.text_dim,
        transform.pre_translate(
            (
                card.x0 + 14.0,
                card.y0 + (MONITOR_HEADER - f64::from(title_h)) / 2.0,
            )
                .into(),
        ),
        None,
        &caption,
    );
    let rows = [
        (
            "desk-monitor-cpu",
            reading
                .cpu_percent
                .map(|v| (format!("{v}%"), f64::from(v) / 100.0)),
        ),
        (
            "desk-monitor-memory",
            reading
                .memory_percent
                .map(|v| (format!("{v}%"), f64::from(v) / 100.0)),
        ),
        (
            "desk-monitor-temperature",
            reading
                .temperature_celsius
                .map(|c| (format!("{c}\u{b0}"), (f64::from(c) / 100.0).clamp(0.0, 1.0))),
        ),
    ];
    let label_style = theme.text(TextRole::Body);
    let value_style = theme.text(TextRole::Body).with_weight(TextWeight::Semibold);
    let mut y = card.y0 + MONITOR_HEADER;
    for (key, value) in rows {
        let Some((value, share)) = value else {
            continue;
        };
        let label = crate::shell::i18n::t(key);
        let (_, h) = text.measure_style(&label, None, &label_style);
        let line_y = y + 4.0;
        text.draw_style(
            scene,
            &label,
            colors.text_primary,
            transform.pre_translate((card.x0 + 14.0, line_y).into()),
            None,
            &label_style,
        );
        let (vw, _) = text.measure_style(&value, None, &value_style);
        text.draw_style(
            scene,
            &value,
            colors.text_primary,
            transform.pre_translate((card.x1 - 14.0 - f64::from(vw), line_y).into()),
            None,
            &value_style,
        );
        // Secondary ink, never a status hue: a resting readout is not news.
        let track_y = line_y + f64::from(h) + 6.0;
        let track = Rect::new(card.x0 + 14.0, track_y, card.x1 - 14.0, track_y + 4.0);
        ui::surface::card(
            scene,
            transform,
            track,
            2.0,
            ui::paint::with_alpha_scaled(colors.text_dim, 0.25),
        );
        let fill = Rect::new(
            track.x0,
            track.y0,
            track.x0 + track.width() * share.clamp(0.0, 1.0),
            track.y1,
        );
        if fill.width() > 0.0 {
            ui::surface::card(scene, transform, fill, 2.0, colors.text_dim);
        }
        y += MONITOR_ROW;
    }
}

fn paint_pin(scene: &mut Scene, transform: Affine, theme: &Theme, rect: Rect, pinned: bool) {
    let colors = &theme.tokens.colors;
    // On: an inverted control, the way Tilo marks a primary choice.
    let ground = if pinned {
        colors.text_primary
    } else {
        colors.surfaces.overlay
    };
    let radius = rect.height() / 2.0;
    ui::surface::card(scene, transform, rect, radius, ground);
    ui::paint::stroke_round(
        scene,
        transform,
        rect,
        radius,
        ui::chrome::panel_border(theme),
        1.0,
    );
    let ink = if pinned {
        colors.surfaces.overlay
    } else {
        colors.text_dim
    };
    ui::icons::draw_icon(
        scene,
        transform,
        rect.inflate(-6.0, -6.0),
        ui::icons::Icon::Pin,
        ink,
        1.4,
    );
}

#[cfg(test)]
#[path = "desk_tests.rs"]
mod tests;
