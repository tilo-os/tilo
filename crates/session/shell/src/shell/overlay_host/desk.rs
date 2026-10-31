//! The desk widgets and the Super surface, as the overlay host presents
//! them.
//!
//! The decisions (which form each widget is in) come from
//! [`crate::shell::desk_widgets::DeskBoard`]; this module feeds it what the
//! host knows (typing into the focused window, the pointer, which surfaces
//! are open), lays each widget out where the person put it on the display
//! it lives on ([`crate::shell::desk_widgets::placement`]), takes presses on
//! it and paints it (`desk_paint`).
//!
//! # Input
//!
//! A showing widget floats above the windows, and a press on it is its own
//! on every display: the compositor asks
//! [`ShellOverlayHost::desk_captures_pointer_at`] before it hands a press to
//! the window beneath. Nothing acts on the press itself. A press that moves
//! past the slop is a drag: the widget follows the hand 1:1, lifted, and on
//! release coasts on with the hand's momentum, is drawn onto a nearby edge,
//! corner or neighbour, and springs to rest there, on whichever display it
//! was let go over; thrown off the screen it is removed, with Undo. A press
//! let go where it landed is a tap on the part it landed on ([`DeskPart`]),
//! and each part does one thing. The resize handle and the seek bar follow
//! the hand from the press instead. Hit tests read the rects the frame
//! paints, at the same instant.
//!
//! The Super surface is global search over a light scrim, with every widget
//! open around it and the time and status under the field. Search is the
//! launcher app's card ([`super::launcher_host`]). Tapped, it lists the
//! launcher's sections under the field ([`crate::shell::search::launcher`]);
//! held, it is the field alone (the answer's `field_only`), a glance at the
//! widgets. Typing searches as search always does; Escape and a click
//! outside close it through search's own paths.
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
use ui::motion::MotionValue;
use ui::presence::PresenceCtx;
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect, RoundedRect, Size, Vec2};
use vello::peniko::{BlendMode, Fill};
use winit::event::{ElementState, MouseButton, WindowEvent};

use super::ShellOverlayHost;
use super::desk_paint::{
    BAND, CALENDAR_GLANCE, MONITOR_GLANCE, MUSIC_COMPACT_HEIGHT, MUSIC_GLANCE, badge_rect,
    calendar_card_height, card_rect, close_rect, collapse_rect, desk_widget_title, glance_radius,
    grip_edge, grip_rect, large_art, monitor_card_height, music_compact_play, music_glance_expand,
    music_glance_play, music_large_parts, paint_badge, paint_band_control, paint_calendar_card,
    paint_calendar_glance, paint_card_ground, paint_glance_ground, paint_grip, paint_lift,
    paint_monitor_card, paint_monitor_glance, paint_morph_ground, paint_music_compact,
    paint_music_glance, paint_music_large_art, paint_team_card, paint_team_glance, pin_rect,
    team_card_height, team_glance_size, team_rows, with_alpha,
};
use super::layers::LayerId;
use crate::shell::desk_widgets::calendar::Month;
use crate::shell::desk_widgets::sizing::{self, Stage};
use crate::shell::desk_widgets::system_monitor::{self, Reading};
use crate::shell::desk_widgets::team::{self, TeamMember};
use crate::shell::desk_widgets::{
    Context, DeskBoard, DeskWidgetPrefs, Form, ShowMode, Slot, WidgetId, placement,
};
use crate::shell::i18n::{t, t_args};
use crate::shell::input_policy::SuperGesture;
use crate::shell::now_playing::{NowPlayingView, PlayerSnapshot};
use crate::shell::now_playing_panel::{CardTarget, NowPlayingCard};

/// How strongly the Super surface dims what is behind it, as a share of
/// the theme's modal scrim: the background stays mostly visible.
pub(crate) const SUPER_SCRIM: f32 = 0.35;

/// A desk widget's target prefix: `desk.widget:<key>` opens or folds it,
/// `.pin` keeps it on this strip and `.remove` takes it off the desk.
pub(super) const DESK_TARGET: &str = "desk.widget:";

/// A press that moves this far is a drag, never a tap.
pub(crate) const DRAG_SLOP: f64 = ui::tokens::Metrics::BASE.icon_label_gap as f64;

/// How much larger a widget is drawn while lifted under the hand.
const LIFT_SCALE: f64 = 0.03;

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

/// What a press on a widget landed on: each part does one thing when
/// tapped, and a press anywhere but the seek bar or the resize handle can
/// still become a drag that moves the widget instead (a drag never also
/// taps).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum DeskPart {
    /// The widget where it has no control of its own: on a glance, a tap
    /// opens its card in place; on a card, a tap opens its app (Calendar,
    /// System Monitor).
    Body,
    /// The song (its artwork and its title): a tap brings the player's
    /// window forward, wherever it is.
    Song,
    /// Play/pause on the music glance or the compact music card.
    PlayPause,
    /// The music glance's expand control (its art and title open the
    /// player instead).
    Expand,
    /// A control of the open music card.
    Card(CardTarget),
    /// The open music card's seek bar, which follows the hand from the
    /// press.
    Seek,
    /// A person on the team card: a tap opens who is here, at them.
    Person(String),
    /// The open card's collapse control.
    Collapse,
    /// The open card's pin.
    Pin,
    /// Remove the widget from the desk: the open card's control, or a
    /// hovered glance's badge.
    Remove,
    /// The open card's resize handle, which follows the hand from the
    /// press.
    Resize,
}

/// A press held on a widget.
#[derive(Clone, Debug)]
struct Grab {
    id: WidgetId,
    part: DeskPart,
    /// The form it was pressed in.
    form: Form,
    at: Point,
    started: Instant,
    /// How far the widget was drawn from its laid-out place at the press:
    /// a throw still settling is caught where it is.
    offset: Vec2,
    /// The card's width at the press, for a resize.
    width: f64,
    dragging: bool,
    /// Recent pointer samples: seconds since the press, and where.
    samples: Vec<(f64, Point)>,
}

/// One widget as [`ShellOverlayHost::relayout_desk`] places it.
struct Laid {
    id: WidgetId,
    display: usize,
    glance: Rect,
    open: Size,
    has_content: bool,
}

/// A widget just removed, fading where it was let go.
#[derive(Clone, Copy, Debug)]
struct Leaving {
    id: WidgetId,
    form: Form,
    slot: Slot,
    since: Instant,
}

/// What the host keeps for the desk between frames.
pub(crate) struct DeskState {
    board: DeskBoard,
    prefs: DeskWidgetPrefs,
    /// Write choices made on the desk (a pin, a drop, a resize, a removal)
    /// to the settings file. Only the live session does; fixtures never
    /// touch the person's settings.
    persist_prefs: bool,
    slots: Vec<Slot>,
    /// The display each widget lives on now: its identity and work area.
    homes: BTreeMap<WidgetId, (String, Rect)>,
    /// How much of each widget shows, and how far its card has expanded
    /// from its glance (`ui::motion::Ambient`, `ui::motion::Zoom`).
    shown: BTreeMap<WidgetId, ui::motion::Presence>,
    opened: BTreeMap<WidgetId, ui::motion::Presence>,
    /// How far each widget is drawn from its laid-out place: nothing at
    /// rest, the rest of a throw while it settles.
    offsets: BTreeMap<WidgetId, MotionValue<Vec2>>,
    /// How lifted each widget is: 1 while the hand carries it.
    lifts: BTreeMap<WidgetId, MotionValue<f64>>,
    /// Whether another surface owned the screen at the last update: the
    /// widgets coming back when it goes follow what closed it.
    covered: bool,
    super_surface: Option<SuperSurface>,
    system_activities: Vec<Rect>,
    music_card: NowPlayingCard,
    grab: Option<Grab>,
    /// The width a resize under way gives its card.
    resizing: Option<(WidgetId, f64)>,
    leaving: Vec<Leaving>,
    /// The last pointer sample the board was given: the compositor reports
    /// motion over a window and the shell's own dispatch may report it
    /// again, and the same sample twice must not read as a pause.
    pointer: Option<Point>,
    team_count: usize,
    /// The people on the team card as last painted, in row order: what a
    /// press on a row opens.
    team_ids: Vec<String>,
    now_playing_revision: u64,
    /// The motion the person allows, as last painted.
    motion: ui::tokens::EffectiveMotion,
    /// The system monitor's readings, sent only while it shows.
    monitor: system_monitor::Monitor,
    monitor_source: Box<dyn system_monitor::Source>,
    /// Wakes the loop when a reading arrives.
    monitor_wake: Option<crate::session_runtime::Wake>,
    /// The widgets and their controls as Tab and assistive technology reach
    /// them.
    targets: ui::widget::PaintedTargets,
    /// The desk itself holds keyboard focus (Tab reached it).
    focused: bool,
}

impl Default for DeskState {
    fn default() -> Self {
        Self {
            board: DeskBoard::default(),
            prefs: DeskWidgetPrefs::default(),
            persist_prefs: false,
            slots: Vec::new(),
            homes: BTreeMap::new(),
            shown: BTreeMap::new(),
            opened: BTreeMap::new(),
            offsets: BTreeMap::new(),
            lifts: BTreeMap::new(),
            covered: false,
            super_surface: None,
            system_activities: Vec::new(),
            music_card: NowPlayingCard::new(),
            grab: None,
            resizing: None,
            leaving: Vec::new(),
            pointer: None,
            team_count: 0,
            team_ids: Vec::new(),
            now_playing_revision: 0,
            motion: ui::tokens::EffectiveMotion::new(ui::tokens::Motion::default(), false),
            monitor: system_monitor::Monitor::default(),
            monitor_source: Box::new(system_monitor::Service::default()),
            monitor_wake: None,
            targets: ui::widget::PaintedTargets::new(),
            focused: false,
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
        // While the capture tool is up, Super is held for its chords: nothing
        // the hold brings up may show over what is being captured.
        if self.capture_studio.is_open()
            && matches!(
                gesture,
                SuperGesture::Tap | SuperGesture::Peek | SuperGesture::PeekFull
            )
        {
            return;
        }
        match (gesture, self.desk.super_surface) {
            // A second tap closes the surface, and closes plain search as it
            // always has.
            (SuperGesture::Tap, surface) if surface.is_some() || self.palette_visible => {
                self.close_super_surface();
            }
            (SuperGesture::Tap, None) | (SuperGesture::PeekTaken, Some(SuperSurface::Glimpse)) => {
                self.open_super_surface(SuperSurface::Pinned, doc, root);
            }
            (SuperGesture::Peek, None) if !self.palette_visible => {
                self.open_super_glimpse();
            }
            (SuperGesture::PeekFull, Some(SuperSurface::Glimpse)) => {
                self.open_super_surface(SuperSurface::Peek, doc, root);
            }
            (
                SuperGesture::PeekReleased | SuperGesture::PeekCancelled,
                Some(SuperSurface::Glimpse),
            )
            | (SuperGesture::PeekCancelled, Some(SuperSurface::Peek)) => {
                self.cancel_super_surface();
            }
            (SuperGesture::PeekReleased, Some(SuperSurface::Peek)) => {
                if self.search.query().is_empty() {
                    self.close_super_surface();
                } else {
                    self.desk.super_surface = Some(SuperSurface::Pinned);
                    self.search.set_field_only_while_empty(false);
                    self.palette_layout_dirty = true;
                }
            }
            (SuperGesture::PeekTaken, Some(SuperSurface::Peek)) => {
                self.desk.super_surface = Some(SuperSurface::Pinned);
                self.search.set_field_only_while_empty(false);
                self.palette_layout_dirty = true;
            }
            _ => return,
        }
        self.bump_chrome();
    }

    /// The quiet first stage of a held Super: the widgets come to their
    /// glance forms, so growing into the whole surface moves nothing.
    fn open_super_glimpse(&mut self) {
        // Lay search out field-only, unseen, so it has its place before the
        // field shows.
        self.search.set_field_only_while_empty(true);
        self.palette_layout_dirty = true;
        self.desk.super_surface = Some(SuperSurface::Glimpse);
    }

    /// Open the Super surface: search with an empty field at the top, every
    /// widget open around it, the time and status.
    pub fn open_super_surface(&mut self, mode: SuperSurface, doc: &AutoCommit, root: &ObjId) {
        if !self.global_search_open() {
            self.toggle_global_search();
        }
        // Tapped, it is global search with the launcher under the field;
        // held, a glance at the widgets with the field alone.
        self.search
            .set_field_only_while_empty(mode != SuperSurface::Pinned);
        self.palette_layout_dirty = true;
        self.desk.super_surface = Some(mode);
        self.refresh_search_results(doc, root);
    }

    /// Close the Super surface (and the search it holds).
    pub fn close_super_surface(&mut self) {
        self.palette_visible = false;
        self.search.close();
        self.end_super_surface();
    }

    /// The hold was a chord after all, or Super came up during the quiet
    /// stage: everything the hold brought is gone at once, with no fade
    /// for the eye to follow while the chord runs.
    pub(super) fn cancel_super_surface(&mut self) {
        self.close_super_surface();
        self.palette_was_visible = false;
        self.palette_transition = ui::motion::Presence::closed();
    }

    fn end_super_surface(&mut self) {
        self.desk.super_surface = None;
        self.search.set_field_only_while_empty(false);
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

    /// Whether the desk has a control Tab can reach now: a showing
    /// widget's glance or card.
    pub(super) fn desk_is_focusable(&self) -> bool {
        self.desk
            .slots
            .iter()
            .any(|slot| self.desk.board.form(slot.id) != Form::Away)
    }

    /// Tab reached the desk, or left it: the widgets' controls take or give
    /// back the keyboard.
    pub(super) fn set_desk_focus(&mut self, focused: bool) {
        if self.desk.focused != focused {
            self.desk.focused = focused;
            self.desk.targets.set_host_focus(focused);
            self.bump_chrome();
        }
    }

    /// Whether the desk shows where keyboard focus is: it holds focus and
    /// the keyboard is what moved last.
    fn desk_focus_visible(&self) -> bool {
        self.desk.focused && self.input_source == ui::tokens::MotionSource::Keyboard
    }

    pub(super) fn desk_is_focused(&self) -> bool {
        self.desk.focused
    }

    pub(super) fn desk_wants_tab(&self) -> bool {
        self.desk.targets.wants_tab_input()
    }

    /// A key for the desk while Tab has reached it: Tab between its
    /// controls, Enter or Space to open, fold or pin one.
    pub(super) fn handle_desk_keyboard(
        &mut self,
        event: &WindowEvent,
        ctx: &mut ui::widget::EventCtx<'_>,
    ) -> bool {
        if !self.desk.focused {
            return false;
        }
        let mut changed = self.desk.targets.handle_event(event, ctx);
        let activated = self.desk.targets.take_activated();
        for target in activated {
            changed |= self.desk_activate_target(&target);
        }
        if changed {
            self.bump_chrome();
        }
        *ctx.handled || changed
    }

    /// The desk's controls as assistive technology reads them, when the
    /// desk is on screen rather than under another surface.
    pub(super) fn desk_accessibility_children(&self) -> Vec<ui::a11y::AccessNode> {
        if self.desk_covered() {
            return Vec::new();
        }
        self.desk.targets.accessibility_children()
    }

    /// Carry out what assistive technology asked of a desk control: the
    /// same open, fold or pin its press and Enter perform.
    pub(super) fn desk_semantic_action(&mut self, action: &ui::a11y::Action, target: &str) -> bool {
        if !self.desk.targets.perform_semantic_action(action, target) {
            return false;
        }
        let activated = self.desk.targets.take_activated();
        let mut changed = false;
        for target in activated {
            changed |= self.desk_activate_target(&target);
        }
        if changed || matches!(action, ui::a11y::Action::Focus) {
            self.bump_chrome();
        }
        true
    }

    fn desk_activate_target(&mut self, target: &str) -> bool {
        let Some(rest) = target.strip_prefix(DESK_TARGET) else {
            return false;
        };
        let (key, control) = match rest.rsplit_once('.') {
            Some((key, control @ ("pin" | "remove"))) => (key, Some(control)),
            _ => (rest, None),
        };
        let Some(widget) = WidgetId::ALL.into_iter().find(|id| id.key() == key) else {
            return false;
        };
        let now = ui::time::now();
        match control {
            Some("pin") => self.toggle_widget_pin(widget),
            Some(_) => {
                let form = self.desk.board.form(widget);
                if let Some(slot) = self.desk_slot(widget) {
                    let drawn = self.drawn_slot(&slot, now);
                    self.remove_desk_widget(widget, form, drawn);
                }
            }
            None if self.desk.board.form(widget) == Form::Open => self.desk.board.close(),
            None => self.desk.board.open(widget),
        }
        self.update_desk(now);
        self.bump_chrome();
        true
    }

    fn desk_strip(&self) -> Option<u64> {
        self.chrome
            .as_ref()
            .and_then(|chrome| chrome.borrow().active_strip_id())
            .map(|strip| strip.0)
    }

    /// Register this frame's widgets and controls for Tab and assistive
    /// technology, where they are drawn. A widget's own target opens or
    /// folds it; its pin keeps it on this strip; its remove control takes it
    /// off the desk.
    fn register_desk_targets(&mut self, now: Instant) {
        self.desk.targets.begin_frame();
        let strip = self.desk_strip();
        for slot in self.desk.slots.clone() {
            let form = self.desk.board.form(slot.id);
            if form == Form::Away {
                continue;
            }
            let drawn = self.drawn_slot(&slot, now);
            let title = desk_widget_title(slot.id);
            let id = format!("{DESK_TARGET}{}", slot.id.key());
            let remove = t_args("desk-widget-remove", &[("widget", title.as_str().into())]);
            if form == Form::Open {
                self.desk.targets.target(
                    id.clone(),
                    drawn.open,
                    t_args("desk-widget-collapse", &[("widget", title.as_str().into())]),
                );
                let pinned = self.desk.prefs.get(slot.id).always_on(strip);
                self.desk.targets.target(
                    format!("{id}.pin"),
                    pin_rect(drawn.open),
                    t(if pinned {
                        "desk-widget-unpin"
                    } else {
                        "desk-widget-pin"
                    }),
                );
                self.desk
                    .targets
                    .target(format!("{id}.remove"), close_rect(drawn.open), remove);
            } else {
                self.desk.targets.target(
                    id.clone(),
                    drawn.glance,
                    t_args("desk-widget-open", &[("widget", title.into())]),
                );
                let badge = badge_rect(drawn.glance, self.desk_anchor(slot.id));
                self.desk
                    .targets
                    .target(format!("{id}.remove"), badge, remove);
            }
        }
        self.desk.targets.end_frame();
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
                !matches!(id, LayerId::CommandPalette | LayerId::NowPlaying)
                    && self.overlay_is_open(id, flags)
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

    /// The displays a widget can live on, primary first: each one's monitor
    /// identity (the stable key the display partition uses) and its work
    /// area, below its top bar.
    pub(crate) fn desk_displays(&self) -> Vec<(String, Rect)> {
        let outputs = super::displays::distinct_outputs(&self.outputs);
        let band = super::displays::own_top_band();
        let below_bar = |rect: Rect| Rect::new(rect.x0, rect.y0 + band, rect.x1, rect.y1);
        if outputs.len() < 2 || !self.is_partitioned() {
            let key = outputs
                .first()
                .map(|output| super::displays::monitor_key(output))
                .unwrap_or_default();
            return vec![(key, below_bar(self.overlay_area()))];
        }
        outputs
            .into_iter()
            .map(|output| {
                (
                    super::displays::monitor_key(output),
                    below_bar(output.rect()),
                )
            })
            .collect()
    }

    /// The width `id`'s card is drawn at: the resize under way, else the
    /// person's, else the widget's own.
    fn desk_card_width(&self, id: WidgetId) -> f64 {
        let sizing = sizing::sizing(id);
        let width = match self.desk.resizing {
            Some((resized, width)) if resized == id => width,
            _ => self.desk.prefs.get(id).width.unwrap_or(sizing.default),
        };
        sizing.clamp(width)
    }

    /// `id`'s glance size, resting at fractions `at`.
    fn desk_glance_size(&self, id: WidgetId, at: (f64, f64)) -> Size {
        match id {
            WidgetId::Music => MUSIC_GLANCE,
            // Against a side edge (not in a bottom corner) the faces stack
            // down it; anywhere else they line up across.
            WidgetId::Team => team_glance_size(
                (at.0 <= 0.01 || at.0 >= 0.99) && at.1 < 0.99,
                self.desk.team_count,
            ),
            WidgetId::Calendar => CALENDAR_GLANCE,
            WidgetId::SystemMonitor => MONITOR_GLANCE,
        }
    }

    /// How tall `id`'s card is at `stage` and `width`.
    fn desk_stage_height(
        &self,
        id: WidgetId,
        stage: Stage,
        width: f64,
        view: Option<&NowPlayingView>,
    ) -> f64 {
        match id {
            WidgetId::Music => {
                let regular =
                    view.map_or(120.0, |view| self.desk.music_card.preferred_height(view));
                match stage {
                    Stage::Compact => MUSIC_COMPACT_HEIGHT,
                    Stage::Regular => regular,
                    Stage::Large => large_art(width).1 + regular,
                }
            }
            WidgetId::Team => team_card_height(stage, self.desk.team_count),
            WidgetId::Calendar => calendar_card_height(stage, width),
            WidgetId::SystemMonitor => monitor_card_height(stage),
        }
    }

    /// `id`'s open size (card and band), at most `room` wide, and whether
    /// it has anything to show.
    fn desk_open_size(
        &self,
        id: WidgetId,
        view: Option<&NowPlayingView>,
        room: f64,
    ) -> (Size, bool) {
        let width = self.desk_card_width(id).min(room);
        let height = sizing::height(id, width, |stage, w| {
            self.desk_stage_height(id, stage, w, view)
        });
        let has_content = match id {
            WidgetId::Music => view.is_some(),
            WidgetId::Team => self.desk.team_count > 0,
            // The date is always there to show; the kernel always offers
            // memory.
            WidgetId::Calendar | WidgetId::SystemMonitor => true,
        };
        (Size::new(width, height + BAND), has_content)
    }

    /// Lay every widget out where the person put it, on the display it lives
    /// on: the first display it was dropped on that is connected, else the
    /// primary, at the place it had on the display it last lived on (so an
    /// unplugged monitor's widget keeps its corner on the panel), else its
    /// default. Widgets that would rest on each other, on a live call or on
    /// the strip switcher step aside. Returns whether anything moved.
    #[allow(
        clippy::too_many_lines,
        reason = "one layout pass over every widget and display"
    )]
    fn relayout_desk(&mut self, now: Instant, super_up: bool) -> bool {
        let displays = self.desk_displays();
        let connected: Vec<&str> = displays.iter().map(|(key, _)| key.as_str()).collect();
        let view = self.music_view(now);
        // The strip ribbon is where the strip switcher opens: a widget
        // resting there would take the pointer that reaches for it, and its
        // labels ("New strip" under the last) open over whatever rests
        // beside or below it. Widgets step past all of it, as past a call.
        let mut occupied = self.desk.system_activities.clone();
        let ribbon = self.strip_frame().zone();
        if ribbon.area() > 0.0 {
            let labels = self.strip_navigator.labels_extent(&self.strip_frame());
            let switcher = if labels.area() > 0.0 {
                ribbon.union(labels)
            } else {
                ribbon
            };
            occupied.push(switcher.inflate(placement::MARGIN, placement::MARGIN));
        }
        let mut laid: Vec<Laid> = WidgetId::ALL
            .into_iter()
            .map(|id| {
                let prefs = self.desk.prefs.get(id);
                let (display, at) = match prefs.place_on(&connected) {
                    Some(place) => (
                        connected
                            .iter()
                            .position(|key| *key == place.display)
                            .unwrap_or(0),
                        (place.x, place.y),
                    ),
                    None => (
                        0,
                        prefs
                            .places
                            .first()
                            .map_or_else(|| id.default_place(), |place| (place.x, place.y)),
                    ),
                };
                let area = displays
                    .get(display)
                    .map_or_else(|| self.overlay_area(), |d| d.1);
                let glance = placement::rect_at(area, self.desk_glance_size(id, at), at);
                let room = (area.width() - 2.0 * placement::MARGIN).max(1.0);
                let (open, has_content) = self.desk_open_size(id, view.as_ref(), room);
                Laid {
                    id,
                    display,
                    glance,
                    open,
                    has_content,
                }
            })
            .collect();
        // Only widgets that can show take room from each other.
        let prefs = &self.desk.prefs;
        for (index, (_, area)) in displays.iter().enumerate() {
            let mut showing: Vec<&mut Laid> = laid
                .iter_mut()
                .filter(|item| {
                    item.display == index && item.has_content && prefs.get(item.id).enabled
                })
                .collect();
            let mut rects: Vec<Rect> = showing.iter().map(|item| item.glance).collect();
            placement::resolve(*area, &mut rects, &occupied);
            for (item, rect) in showing.iter_mut().zip(rects) {
                item.glance = rect;
            }
        }
        let mut slots: Vec<Slot> = laid
            .iter()
            .map(|item| {
                let area = displays
                    .get(item.display)
                    .map_or_else(|| self.overlay_area(), |d| d.1);
                Slot {
                    id: item.id,
                    glance: item.glance,
                    open: placement::open_rect(area, item.glance, item.open),
                    reach: placement::reach_zone(area, item.glance),
                    has_content: item.has_content,
                    area,
                }
            })
            .collect();
        // With every card open (the Super surface), the cards step aside
        // from each other too.
        if super_up {
            for (index, (_, area)) in displays.iter().enumerate() {
                let mut showing: Vec<&mut Slot> = slots
                    .iter_mut()
                    .zip(laid.iter())
                    .filter(|(slot, item)| item.display == index && slot.has_content)
                    .map(|(slot, _)| slot)
                    .collect();
                let mut rects: Vec<Rect> = showing.iter().map(|slot| slot.open).collect();
                placement::resolve(*area, &mut rects, &[]);
                for (slot, rect) in showing.iter_mut().zip(rects) {
                    slot.open = rect;
                }
            }
        }
        self.desk.homes = laid
            .iter()
            .filter_map(|item| displays.get(item.display).map(|d| (item.id, d.clone())))
            .collect();
        let changed = slots != self.desk.slots;
        self.desk.slots = slots;
        changed
    }

    /// Which way `id`'s card grows from its glance: toward the middle of
    /// the display it lives on ([`placement::anchor`]).
    fn desk_anchor(&self, id: WidgetId) -> (bool, bool) {
        let area = self.desk.homes.get(&id).map(|(_, area)| *area);
        self.desk_slot(id)
            .zip(area)
            .map_or((true, true), |(slot, area)| {
                placement::anchor(placement::fractions(area, slot.glance))
            })
    }

    /// Whether another surface owns the screen now, and what the widgets
    /// showing move as: coming back as a surface over them closes, as that
    /// close did (a key lands them at once, a click fades them in);
    /// otherwise on their own.
    fn desk_cover(&mut self) -> (bool, ui::tokens::MotionSource) {
        let covered = self.desk_covered();
        let uncovered = std::mem::replace(&mut self.desk.covered, covered) && !covered;
        let source = if uncovered {
            self.input_source
        } else {
            ui::tokens::MotionSource::Pointer
        };
        (covered, source)
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
            // Closed through search's own paths (Escape, a click outside,
            // an activated result).
            self.end_super_surface();
        }
        self.desk.team_count = self.team_count();
        let mut changed = self.restore_desk_widgets();
        let super_up = self.super_up();
        changed |= self.relayout_desk(now, super_up);
        changed |= self.decide_desk(now);
        let fade = self.desk.motion.fade(self.desk.motion.fast());
        let before = self.desk.leaving.len();
        self.desk
            .leaving
            .retain(|leaving| now.saturating_duration_since(leaving.since) < fade);
        changed |= before != self.desk.leaving.len();
        changed
    }

    fn super_up(&self) -> bool {
        self.desk
            .super_surface
            .is_some_and(|surface| surface != SuperSurface::Glimpse)
    }

    /// The widget under the hand: the one a press is held on, else the one
    /// the pointer is on. It keeps its form ([`Context::engaged`]).
    fn desk_engaged(&self, now: Instant) -> Option<WidgetId> {
        if let Some(grab) = self.desk.grab.as_ref() {
            return Some(grab.id);
        }
        let p = self.desk.pointer?;
        self.desk_hit_at(p, now).map(|(id, ..)| id)
    }

    /// Decide every widget's form at `now` from the layout already made, and
    /// set each one coming, going, expanding or folding. Returns whether
    /// anything changed.
    fn decide_desk(&mut self, now: Instant) -> bool {
        let super_up = self.super_up();
        let super_glance = self.desk.super_surface == Some(SuperSurface::Glimpse);
        let strip = self.desk_strip();
        let (covered, shown_source) = self.desk_cover();
        let engaged = self.desk_engaged(now);
        let ctx = Context {
            now,
            prefs: &self.desk.prefs,
            strip,
            super_surface: super_up,
            super_glance,
            covered,
            engaged,
            slots: &self.desk.slots,
        };
        let mut changed = self.desk.board.update(&ctx);
        let motion = self.desk.motion;
        let super_asked = self.desk.super_surface.is_some();
        for slot in &self.desk.slots {
            let form = self.desk.board.form(slot.id);
            let asked = super_asked
                || self.desk.board.reached() == Some(slot.id)
                || self.desk.board.opened() == Some(slot.id);
            self.desk
                .shown
                .entry(slot.id)
                .or_insert_with(ui::motion::Presence::closed)
                .show(
                    form != Form::Away,
                    shown_spec(slot.id, asked),
                    motion,
                    shown_source,
                    now,
                );
            // A card expands from its glance and folds back into it in
            // place: one shape between the two.
            self.desk
                .opened
                .entry(slot.id)
                .or_insert_with(ui::motion::Presence::closed)
                .show(
                    form == Form::Open,
                    ui::motion::Zoom::SPEC,
                    motion,
                    self.input_source,
                    now,
                );
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

    /// Whether a desk widget is still coming or going, for captures that
    /// show it at rest.
    #[cfg(feature = "hosted-test-control")]
    pub fn desk_settling_for_shots(&self) -> bool {
        self.desk_animating(ui::time::now())
    }

    /// Lay the desk out at `now` with every widget already where it is
    /// going, for a test that measures what follows the desk's first
    /// appearance rather than the appearance itself.
    #[cfg(test)]
    pub(crate) fn force_desk_settled(&mut self, now: Instant) {
        let _ = self.update_desk(now);
        for presence in self
            .desk
            .shown
            .values_mut()
            .chain(self.desk.opened.values_mut())
        {
            let open = presence.is_open();
            presence.snap(open);
        }
        for offset in self.desk.offsets.values_mut() {
            offset.snap(Vec2::ZERO);
        }
        for lift in self.desk.lifts.values_mut() {
            let target = lift.target();
            lift.snap(target);
        }
        self.desk.leaving.clear();
    }

    /// Take every desk widget away at once, with no fade: something that
    /// must not show them has opened (the capture tool, which holds the
    /// screen still and must not hold a glance the Super of its chord
    /// brought up).
    pub(super) fn clear_desk_at_once(&mut self) {
        for presence in self
            .desk
            .shown
            .values_mut()
            .chain(self.desk.opened.values_mut())
        {
            presence.snap(false);
        }
        self.desk.leaving.clear();
    }

    pub(crate) fn desk_animating(&self, now: Instant) -> bool {
        self.desk
            .shown
            .values()
            .chain(self.desk.opened.values())
            .any(|presence| presence.is_moving(now))
            || self.desk.offsets.values().any(|m| m.is_moving(now))
            || self.desk.lifts.values().any(|m| m.is_moving(now))
            || !self.desk.leaving.is_empty()
    }

    /// A pointer sample at `p`, in desktop coordinates: the compositor's,
    /// over a window as much as over the desk, so reaching for a widget and
    /// carrying one read the whole path.
    pub(crate) fn desk_pointer(&mut self, p: Point, now: Instant) {
        let was = self.desk.pointer.replace(p);
        if was == Some(p) {
            return;
        }
        self.desk.board.on_pointer(p, now, &self.desk.slots);
        if self.desk.grab.is_some() {
            self.desk_grab_moved(p, now);
            self.bump_chrome();
            return;
        }
        // The hovered widget shows its controls (a glance's remove badge,
        // a card's resize handle): repaint while the pointer is on one or
        // just left it.
        let over = |p: Option<Point>| p.and_then(|p| self.desk_hit_at(p, now)).map(|hit| hit.0);
        let hover_changed = over(was) != over(Some(p)) || over(Some(p)).is_some();
        if self.decide_desk(now) || hover_changed {
            self.bump_chrome();
        }
    }

    /// The pointer moved to `p` with a press held on a widget: carry it,
    /// resize it or scrub, as the press began.
    fn desk_grab_moved(&mut self, p: Point, now: Instant) {
        let Some(mut grab) = self.desk.grab.take() else {
            return;
        };
        let elapsed = now.saturating_duration_since(grab.started).as_secs_f64();
        grab.samples.push((elapsed, p));
        if grab.samples.len() > 24 {
            grab.samples.remove(0);
        }
        match grab.part {
            DeskPart::Seek => {
                if let Some(view) = self.music_view(now) {
                    let _ = self.desk.music_card.drag(&view, p);
                }
            }
            DeskPart::Resize => {
                grab.dragging = true;
                // The card grows from the corner it is anchored by: dragging
                // the handle away from that corner widens it.
                let (right, _) = self.desk_anchor(grab.id);
                let toward = if right { -1.0 } else { 1.0 };
                let room = self
                    .desk
                    .homes
                    .get(&grab.id)
                    .map_or(f64::INFINITY, |(_, area)| {
                        area.width() - 2.0 * placement::MARGIN
                    });
                let width = sizing::sizing(grab.id)
                    .clamp(grab.width + toward * (p.x - grab.at.x))
                    .min(room);
                self.desk.resizing = Some((grab.id, width));
                let super_up = self.super_up();
                self.relayout_desk(now, super_up);
            }
            _ => {
                if !grab.dragging && (p - grab.at).hypot() > DRAG_SLOP {
                    grab.dragging = true;
                    self.set_desk_lift(grab.id, true, now);
                }
            }
        }
        self.desk.grab = Some(grab);
    }

    /// Lift `id` under the hand, or set it down.
    fn set_desk_lift(&mut self, id: WidgetId, up: bool, now: Instant) {
        let timing = ui::motion::ControlChange::MOVE.timing(
            ui::motion::Direction::of(up),
            self.desk.motion,
            ui::tokens::MotionSource::Gesture,
        );
        self.desk
            .lifts
            .entry(id)
            .or_insert_with(|| MotionValue::new(0.0))
            .set(if up { 1.0 } else { 0.0 }, timing, now);
    }

    /// Whether the desk takes the pointer at `p` (the pointer's place, or
    /// where a finger lands): on a showing widget, in either form, on any
    /// display, and anywhere while a press on one is held. A widget floats
    /// above the windows, so a press on it is its own and never the
    /// window's beneath; a widget that is away holds nothing.
    pub(crate) fn desk_captures_pointer_at(&self, p: Point) -> bool {
        self.desk.grab.is_some()
            || (!self.desk_covered() && self.desk_hit_at(p, ui::time::now()).is_some())
    }

    /// Whether the desk takes `key` from the window that has the keyboard:
    /// Escape, while a widget's card is open on purpose, folds it.
    pub fn desk_owns_key(&self, key: &winit::keyboard::Key) -> bool {
        matches!(
            key,
            winit::keyboard::Key::Named(winit::keyboard::NamedKey::Escape)
        ) && self.desk.board.opened().is_some()
            && self.desk.super_surface.is_none()
            && !self.desk_covered()
    }

    /// Where each desk widget waits in its quiet form: what else at the
    /// edges (the pinned people overlay) steps past.
    pub(super) fn desk_glances(&self) -> Vec<Rect> {
        self.desk.slots.iter().map(|slot| slot.glance).collect()
    }

    pub(crate) fn desk_pointer_left(&mut self, now: Instant) {
        self.desk.pointer = None;
        self.desk.board.pointer_left();
        // A press whose release will never come (the pointer left every
        // display): what it carried springs back.
        self.cancel_desk_grab(now);
    }

    fn cancel_desk_grab(&mut self, now: Instant) {
        let Some(grab) = self.desk.grab.take() else {
            return;
        };
        self.desk.resizing = None;
        if grab.dragging {
            let at = offset_of(&grab, self.desk.pointer.unwrap_or(grab.at));
            let timing = ui::motion::Snap::SPEC.timing(
                ui::motion::Direction::Exit,
                self.desk.motion,
                ui::tokens::MotionSource::Gesture,
            );
            let offset = self
                .desk
                .offsets
                .entry(grab.id)
                .or_insert_with(|| MotionValue::new(Vec2::ZERO));
            offset.snap(at);
            offset.set(Vec2::ZERO, timing, now);
        }
        self.set_desk_lift(grab.id, false, now);
        self.bump_chrome();
    }

    /// How far `id` is drawn from its laid-out place at `now`: with the
    /// hand while carried, else the rest of a throw.
    fn desk_offset(&self, id: WidgetId, now: Instant) -> Vec2 {
        if let Some(grab) = self.desk.grab.as_ref()
            && grab.id == id
            && grab.dragging
            && !matches!(grab.part, DeskPart::Seek | DeskPart::Resize)
            && let Some(p) = self.desk.pointer
        {
            return offset_of(grab, p);
        }
        self.desk
            .offsets
            .get(&id)
            .map_or(Vec2::ZERO, |offset| offset.get(now))
    }

    /// `slot` where it is drawn at `now`.
    fn drawn_slot(&self, slot: &Slot, now: Instant) -> Slot {
        let offset = self.desk_offset(slot.id, now);
        Slot {
            glance: slot.glance + offset,
            open: slot.open + offset,
            reach: slot.reach,
            ..*slot
        }
    }

    /// The widget form under `p` where it is drawn at `now`, if one is
    /// showing there: the topmost, as painted. A glance's remove badge is
    /// part of it.
    pub(crate) fn desk_hit_at(&self, p: Point, now: Instant) -> Option<(WidgetId, Form, Rect)> {
        let grabbed = self.desk.grab.as_ref().map(|grab| grab.id);
        // Painted in slot order with the carried one last: hit the reverse.
        let mut order: Vec<&Slot> = self.desk.slots.iter().collect();
        if let Some(index) = order.iter().position(|slot| Some(slot.id) == grabbed) {
            let carried = order.remove(index);
            order.push(carried);
        }
        order.into_iter().rev().find_map(|slot| {
            let form = self.desk.board.form(slot.id);
            let drawn = self.drawn_slot(slot, now);
            match form {
                Form::Away => None,
                Form::Open => drawn
                    .open
                    .contains(p)
                    .then_some((slot.id, form, drawn.open)),
                Form::Glance => (drawn.glance.contains(p)
                    || badge_rect(drawn.glance, self.desk_anchor(slot.id)).contains(p))
                .then_some((slot.id, form, drawn.glance)),
            }
        })
    }

    /// A press anywhere, wherever it goes (the compositor reports every
    /// one, over windows too): one outside the card open on purpose folds
    /// it.
    pub(super) fn desk_note_press(&mut self, p: Point) {
        if self.desk.super_surface.is_some() {
            return;
        }
        let Some(open) = self.desk.board.opened() else {
            return;
        };
        let now = ui::time::now();
        if self.desk_hit_at(p, now).is_some_and(|(id, ..)| id == open) {
            return;
        }
        self.desk.board.close();
        self.update_desk(now);
        self.bump_chrome();
    }

    /// A press, drag or release on a showing widget, or Escape for an open
    /// card. `true` when the desk took it.
    ///
    /// Nothing acts on the press: a press that moves past the slop is a
    /// drag that carries the widget, and only one let go where it was
    /// pressed is a tap on the part it landed on ([`DeskPart`]). The seek
    /// bar and the resize handle follow the hand from the press.
    pub(crate) fn route_desk_event(&mut self, event: &WindowEvent) -> bool {
        if let WindowEvent::KeyboardInput { event: key, .. } = event {
            if !self.desk_owns_key(&key.logical_key) {
                return false;
            }
            if key.state == ElementState::Pressed {
                self.desk.board.close();
                self.update_desk(ui::time::now());
                self.bump_chrome();
            }
            return true;
        }
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
        let now = ui::time::now();
        match state {
            ElementState::Pressed => {
                let Some((id, form, rect)) = self.desk_hit_at(p, now) else {
                    // A press on anything else folds the open card (over a
                    // window the compositor already did, `desk_note_press`).
                    self.desk_note_press(p);
                    return false;
                };
                let part = self.desk_part_at(id, form, rect, p, now);
                if part == DeskPart::Seek
                    && let Some(view) = self.music_view(now)
                {
                    let _ = self.desk.music_card.click(&view, p);
                }
                let offset = self.desk_offset(id, now);
                // A throw still settling is caught where it is drawn.
                if let Some(settling) = self.desk.offsets.get_mut(&id) {
                    settling.snap(offset);
                }
                self.desk.pointer = Some(p);
                self.desk.grab = Some(Grab {
                    id,
                    part,
                    form,
                    at: p,
                    started: now,
                    offset,
                    width: self.desk_card_width(id),
                    dragging: false,
                    samples: vec![(0.0, p)],
                });
                self.decide_desk(now);
                self.bump_chrome();
                true
            }
            ElementState::Released => {
                let Some(grab) = self.desk.grab.take() else {
                    return false;
                };
                self.release_desk_grab(grab, p, now);
                self.update_desk(now);
                self.bump_chrome();
                true
            }
        }
    }

    /// The press `grab` let go at `p`: a resize or scrub ends, a drag lands
    /// the widget, anything else is a tap.
    fn release_desk_grab(&mut self, grab: Grab, p: Point, now: Instant) {
        self.set_desk_lift(grab.id, false, now);
        match grab.part {
            DeskPart::Seek => {
                if let Some(view) = self.music_view(now) {
                    let action = self.desk.music_card.release(&view);
                    self.apply_now_playing_action(action, &view);
                }
            }
            DeskPart::Resize => {
                if let Some((id, width)) = self.desk.resizing.take() {
                    let mut prefs = self.desk.prefs.get(id).clone();
                    prefs.width = Some(width.round());
                    self.store_widget_prefs(id, prefs);
                }
            }
            _ if grab.dragging => {
                let mut grab = grab;
                // The release is the last sample of the throw.
                let elapsed = now.saturating_duration_since(grab.started).as_secs_f64();
                if grab.samples.last().is_none_or(|(_, last)| *last != p) {
                    grab.samples.push((elapsed, p));
                }
                self.drop_desk_widget(&grab, p, now);
            }
            part => self.desk_tap(grab.id, grab.form, part),
        }
    }

    /// The part of widget `id`, showing in `form` drawn at `rect`, under
    /// `p`.
    pub(crate) fn desk_part_at(
        &mut self,
        id: WidgetId,
        form: Form,
        rect: Rect,
        p: Point,
        now: Instant,
    ) -> DeskPart {
        let anchor = self.desk_anchor(id);
        match form {
            Form::Away => DeskPart::Body,
            Form::Glance => {
                // The badge shows on the hovered glance, drawn over it.
                if badge_rect(rect, anchor).contains(p) {
                    return DeskPart::Remove;
                }
                match id {
                    WidgetId::Music if music_glance_expand(rect).contains(p) => DeskPart::Expand,
                    WidgetId::Music if music_glance_play(rect).contains(p) => DeskPart::PlayPause,
                    WidgetId::Music => DeskPart::Song,
                    WidgetId::Team | WidgetId::Calendar | WidgetId::SystemMonitor => DeskPart::Body,
                }
            }
            Form::Open => {
                if close_rect(rect).contains(p) {
                    return DeskPart::Remove;
                }
                if pin_rect(rect).contains(p) {
                    return DeskPart::Pin;
                }
                if collapse_rect(rect).contains(p) {
                    return DeskPart::Collapse;
                }
                let card = card_rect(rect);
                if grip_rect(card, anchor).contains(p) || grip_edge(card, anchor).contains(p) {
                    return DeskPart::Resize;
                }
                if !card.contains(p) {
                    return DeskPart::Body;
                }
                let stage = sizing::dominant(id, card.width());
                match id {
                    WidgetId::Music => self.music_part_at(card, stage, p, now),
                    WidgetId::Team => team_rows(card, stage, self.desk.team_ids.len())
                        .iter()
                        .zip(self.desk.team_ids.iter())
                        .find(|(row, _)| row.contains(p))
                        .map_or(DeskPart::Body, |(_, person)| {
                            DeskPart::Person(person.clone())
                        }),
                    WidgetId::Calendar | WidgetId::SystemMonitor => DeskPart::Body,
                }
            }
        }
    }

    /// The part of the open music card at `card`, laid out at `stage`,
    /// under `p`.
    fn music_part_at(&mut self, card: Rect, stage: Stage, p: Point, now: Instant) -> DeskPart {
        let Some(view) = self.music_view(now) else {
            return DeskPart::Body;
        };
        let regular = match stage {
            Stage::Compact => {
                return if music_compact_play(card).contains(p) {
                    DeskPart::PlayPause
                } else {
                    DeskPart::Song
                };
            }
            Stage::Regular => card,
            Stage::Large => {
                let (art, below) = music_large_parts(card);
                if art.contains(p) {
                    return DeskPart::Song;
                }
                below
            }
        };
        self.desk.music_card.layout(regular);
        match self.desk.music_card.target_at(&view, p) {
            Some(CardTarget::Seek) => DeskPart::Seek,
            Some(target) => DeskPart::Card(target),
            None if self.desk.music_card.song_rect().contains(p) => DeskPart::Song,
            None => DeskPart::Body,
        }
    }

    /// A press let go where it landed, on `part` of widget `id` showing in
    /// `form`.
    fn desk_tap(&mut self, id: WidgetId, form: Form, part: DeskPart) {
        let now = ui::time::now();
        let view = (id == WidgetId::Music)
            .then(|| self.music_view(now))
            .flatten();
        match (part, view) {
            (DeskPart::Pin, _) => self.toggle_widget_pin(id),
            (DeskPart::Remove, _) => {
                if let Some(slot) = self.desk_slot(id) {
                    let drawn = self.drawn_slot(&slot, now);
                    self.remove_desk_widget(id, form, drawn);
                }
            }
            (DeskPart::Collapse, _) => self.desk.board.close(),
            (DeskPart::Expand, _) => self.desk.board.open(id),
            (DeskPart::Person(person), _) => self.show_person(person),
            (DeskPart::Song, Some(view)) => {
                if !self.show_player_window(view.player()) {
                    // A player with no window here (a background service,
                    // a window closed while it plays): its card instead.
                    self.desk.board.open(id);
                }
            }
            (DeskPart::PlayPause, Some(view)) => {
                // The same dispatch the Quick Settings card uses: the
                // command goes to exactly that player's bus, whichever
                // workspace its app runs in; nothing switches.
                let action = crate::shell::now_playing_panel::CardAction::Command {
                    bus_name: view.player().bus_name.clone(),
                    command: crate::shell::now_playing::PlayerCommand::PlayPause,
                };
                self.apply_now_playing_action(action, &view);
            }
            (DeskPart::Card(target), Some(view)) => {
                let action = self.desk.music_card.activate(&view, target);
                self.apply_now_playing_action(action, &view);
            }
            (DeskPart::Body, _) if form == Form::Glance => self.desk.board.open(id),
            (DeskPart::Body, _) => match id {
                WidgetId::Calendar => {
                    self.launch_app("so.tilo.calendar", &t("desk-widget-calendar"));
                }
                WidgetId::SystemMonitor => {
                    self.launch_app("so.tilo.monitor", &t("desk-widget-system-monitor"));
                }
                WidgetId::Music | WidgetId::Team => {}
            },
            _ => {}
        }
    }

    /// Bring `player`'s window forward, on whichever workspace, strip and
    /// display it is: the window whose app is the player's
    /// ([`PlayerSnapshot::app`]), the one in front first. Whether one was
    /// found.
    #[allow(
        clippy::significant_drop_tightening,
        reason = "the tree is read for the whole search, which ends with the block"
    )]
    pub(crate) fn show_player_window(&mut self, player: &PlayerSnapshot) -> bool {
        let Some(app) = player.app.as_deref() else {
            return false;
        };
        let Some(chrome) = self.chrome.clone() else {
            return false;
        };
        let tab = {
            let state = chrome.borrow();
            let tree = state.tree();
            let desktop = &tree.desktop;
            let active = desktop.active;
            let ordered = desktop
                .workspaces
                .iter()
                .filter(|ws| ws.id == active)
                .chain(desktop.workspaces.iter().filter(|ws| ws.id != active));
            let mut found = None;
            'search: for workspace in ordered {
                for strip in &workspace.strips {
                    for group in &strip.window_groups {
                        // The tab in front of its group before the others.
                        let tabs = group
                            .tabs
                            .iter()
                            .filter(|tab| tab.id == group.active_tab)
                            .chain(group.tabs.iter().filter(|tab| tab.id != group.active_tab));
                        for tab in tabs {
                            if tile_is_app(&tab.tile, app) {
                                found = Some(tab.id);
                                break 'search;
                            }
                        }
                    }
                }
            }
            found
        };
        let Some(tab) = tab else {
            return false;
        };
        let shown = chrome.borrow_mut().show_tab(tab);
        if shown {
            self.observe_window_focus();
        }
        shown
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

    /// `grab`'s widget let go at `p`: thrown off the screen it is removed;
    /// otherwise it coasts on with the hand's momentum to where the throw
    /// projects, is drawn onto a nearby edge, corner or neighbour on the
    /// display under it, and springs to rest there. That display and place
    /// are kept.
    fn drop_desk_widget(&mut self, grab: &Grab, p: Point, now: Instant) {
        let id = grab.id;
        let Some(slot) = self.desk_slot(id) else {
            return;
        };
        let motion = self.desk.motion;
        let velocity = placement::release_velocity(&grab.samples);
        let travel = offset_of(grab, p);
        let open = grab.form == Form::Open;
        let drawn = if open { slot.open } else { slot.glance } + travel;
        let displays = self.desk_displays();
        let areas: Vec<Rect> = displays.iter().map(|(_, area)| *area).collect();
        if placement::thrown_off(&areas, drawn, velocity, motion) {
            let at = Slot {
                glance: slot.glance + travel,
                open: slot.open + travel,
                ..slot
            };
            self.remove_desk_widget(id, grab.form, at);
            return;
        }
        let landed = placement::landing(drawn, velocity, motion);
        let Some((key, area)) = placement::display_at(&areas, landed.center())
            .and_then(|index| displays.get(index).cloned())
        else {
            return;
        };
        let neighbours: Vec<Rect> = self
            .desk
            .slots
            .iter()
            .filter(|other| {
                other.id != id
                    && self
                        .desk
                        .homes
                        .get(&other.id)
                        .is_some_and(|(home, _)| *home == key)
            })
            .filter_map(|other| match self.desk.board.form(other.id) {
                Form::Away => None,
                Form::Glance => Some(other.glance),
                Form::Open => Some(other.open),
            })
            .collect();
        let snapped = placement::magnet(area, landed, &neighbours);
        let glance = if open {
            placement::glance_in(area, snapped, slot.glance.size())
        } else {
            snapped
        };
        let at = placement::fractions(area, glance);
        let mut prefs = self.desk.prefs.get(id).clone();
        prefs.drop_on(&key, at.0, at.1);
        self.store_widget_prefs(id, prefs);
        let super_up = self.super_up();
        self.relayout_desk(now, super_up);
        let rest = self
            .desk_slot(id)
            .map_or(snapped, |slot| if open { slot.open } else { slot.glance });
        // From where the hand let go to its new place, carrying the hand's
        // velocity: a throw coasts on and slows into its place; a set-down
        // settles the last pixels. Reduced motion: it lands.
        let timing = ui::motion::Snap::COAST.timing(
            ui::motion::Direction::Enter,
            motion,
            ui::tokens::MotionSource::Gesture,
        );
        let offset = self
            .desk
            .offsets
            .entry(id)
            .or_insert_with(|| MotionValue::new(Vec2::ZERO));
        offset.snap(drawn.origin() - rest.origin());
        offset.release(Vec2::ZERO, velocity, timing, now);
    }

    /// Take `id` off the desk (its remove control, or thrown off the
    /// screen), drawn at `at` in `form`: it fades where it is, its place
    /// and size are kept, and a toast offers Undo. Settings brings it back
    /// too.
    pub(crate) fn remove_desk_widget(&mut self, id: WidgetId, form: Form, at: Slot) {
        let now = ui::time::now();
        let mut prefs = self.desk.prefs.get(id).clone();
        prefs.enabled = false;
        self.store_widget_prefs(id, prefs);
        if self.desk.board.opened() == Some(id) {
            self.desk.board.close();
        }
        for presence in [self.desk.shown.get_mut(&id), self.desk.opened.get_mut(&id)]
            .into_iter()
            .flatten()
        {
            presence.snap(false);
        }
        if let Some(offset) = self.desk.offsets.get_mut(&id) {
            offset.snap(Vec2::ZERO);
        }
        if let Some(lift) = self.desk.lifts.get_mut(&id) {
            lift.snap(0.0);
        }
        self.desk.leaving.push(Leaving {
            id,
            form,
            slot: at,
            since: now,
        });
        if let Some(chrome) = self.chrome.as_ref() {
            let title = t_args(
                "desk-widget-removed",
                &[("widget", desk_widget_title(id).into())],
            );
            chrome.borrow_mut().push_desk_undo_toast(id, title);
        }
        self.update_desk(now);
        self.bump_chrome();
    }

    /// Bring back the widgets an Undo asked for. Whether any came back.
    fn restore_desk_widgets(&mut self) -> bool {
        let Some(chrome) = self.chrome.clone() else {
            return false;
        };
        let restores = chrome.borrow_mut().take_desk_restores();
        for id in &restores {
            let mut prefs = self.desk.prefs.get(*id).clone();
            prefs.enabled = true;
            self.store_widget_prefs(*id, prefs);
        }
        !restores.is_empty()
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

    /// Where a part of `id` is drawn now, for a scenario's hand to move to:
    /// `glance`, `expand` and `badge` on its glance; `card`, `grip`,
    /// `collapse` and `remove` on its open card.
    #[cfg(feature = "hosted-test-control")]
    pub fn desk_spot_for_shots(&self, id: WidgetId, spot: &str) -> Option<Point> {
        let now = ui::time::now();
        let drawn = self.drawn_slot(&self.desk_slot(id)?, now);
        let anchor = self.desk_anchor(id);
        let card = card_rect(drawn.open);
        let rect = match spot {
            "glance" => drawn.glance,
            "expand" => music_glance_expand(drawn.glance),
            "badge" => badge_rect(drawn.glance, anchor),
            "card" => card,
            "grip" => grip_rect(card, anchor),
            "collapse" => collapse_rect(drawn.open),
            "remove" => close_rect(drawn.open),
            _ => return None,
        };
        Some(rect.center())
    }

    /// Named geometry for `tilo-control list-targets`, where each widget is
    /// drawn.
    pub(crate) fn desk_test_rects(&self) -> Vec<(String, Rect)> {
        let now = ui::time::now();
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
                let drawn = self.drawn_slot(slot, now);
                match self.desk.board.form(slot.id) {
                    Form::Away => None,
                    Form::Glance => Some((name.to_string(), drawn.glance)),
                    Form::Open => Some((format!("{name}.open"), drawn.open)),
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
        let now = ui::time::now();
        self.desk.motion = theme.motion();
        self.register_desk_targets(now);
        let content = self.desk_content(now, presence);
        // What a press on a team row opens is the row as painted.
        self.desk.team_ids = content.members.iter().map(|m| m.row.id.clone()).collect();
        // The strip switcher's labels are where the person is looking: a
        // widget glance under them steps aside while they are open.
        let switcher_card = self
            .strip_navigator
            .is_open()
            .then(|| self.strip_navigator.switcher().layout().bounds);
        let mut painted = Vec::new();
        let mut pass = DeskPass {
            scene,
            transform,
            text,
            theme,
            content: &content,
        };
        for leaving in self.desk.leaving.clone() {
            let gone = ui::motion::CROSS_FADE.progress_after(
                ui::motion::Direction::Exit,
                self.desk.motion,
                ui::tokens::MotionSource::Pointer,
                now.saturating_duration_since(leaving.since),
            );
            if gone < 1.0 {
                self.paint_leaving(&mut pass, leaving, gone, &mut painted);
            }
        }
        // The widget under the hand rides on top of the others.
        let mut order = self.desk.slots.clone();
        if let Some(grabbed) = self.desk.grab.as_ref().map(|grab| grab.id)
            && let Some(index) = order.iter().position(|slot| slot.id == grabbed)
        {
            let carried = order.remove(index);
            order.push(carried);
        }
        for slot in order {
            self.paint_slot(&mut pass, slot, now, switcher_card, &mut painted);
        }
        // The focus ring is the keyboard's: only while the keyboard drives
        // the desk, never left over from a press (a press focuses the host).
        if self.desk_focus_visible() {
            self.desk.targets.paint_focus(
                scene,
                transform,
                f64::from(theme.tokens.radii.control),
                theme,
            );
        }
        painted
    }

    /// What the widgets show this pass: who is on the team, the music the
    /// player has, the month on view and the monitor's reading.
    fn desk_content(&self, now: Instant, presence: &PresenceCtx) -> DeskContent {
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
        DeskContent {
            music: self.music_view(now),
            month: self.calendar_month(),
            reading: self.desk.monitor.reading(),
            members,
        }
    }

    /// One widget where it is drawn: its glance while folded, its card
    /// while open, one shape morphing between the two while it expands or
    /// folds; lifted while carried. Returns the bounds it painted.
    #[allow(
        clippy::too_many_lines,
        reason = "one widget's whole paint: its ground, lift, morph and both forms"
    )]
    fn paint_slot(
        &mut self,
        pass: &mut DeskPass<'_>,
        slot: Slot,
        now: Instant,
        switcher_card: Option<Rect>,
        painted: &mut Vec<Rect>,
    ) {
        let shown = self
            .desk
            .shown
            .get(&slot.id)
            .map_or(0.0, |t| shown_opacity(t.progress(now)));
        if shown <= f32::EPSILON || switcher_card.is_some_and(|card| card.overlaps(slot.glance)) {
            return;
        }
        let open = self
            .desk
            .opened
            .get(&slot.id)
            .map_or(0.0, |t| t.progress(now).clamp(0.0, 1.0));
        let drawn = self.drawn_slot(&slot, now);
        let lift = self
            .desk
            .lifts
            .get(&slot.id)
            .map_or(0.0, |lift| lift.get(now).clamp(0.0, 1.0));
        let reduced = self.desk.motion.reduced();
        let theme = pass.theme;
        let glance_r = glance_radius(slot.id, theme, drawn.glance);
        // The card's ground holds its band of controls too.
        let card_r = ui::widget::floating_surface::card_radius(drawn.open, theme);
        // Under reduced motion nothing changes shape: the two forms fade.
        let morph = if reduced { open.round() } else { open };
        let ground = lerp_rect(drawn.glance, drawn.open, morph);
        let radius = glance_r + (card_r - glance_r) * morph;
        let scale = if reduced {
            1.0
        } else {
            1.0 + LIFT_SCALE * lift
        };
        let lifted = Affine::scale_about(scale, ground.center());
        let at = pass.transform * lifted;
        let carried = self
            .desk
            .grab
            .as_ref()
            .is_some_and(|grab| grab.id == slot.id && grab.dragging);
        let reach = ground
            .union(drawn.glance)
            .union(drawn.open)
            .inflate(40.0, 48.0);
        // A widget stands on its display: its shadow at that display's edge
        // stops there rather than painting (and repainting) the edge of the
        // display beside it. Carried, it goes where the hand takes it.
        let bounds = if carried {
            reach
        } else {
            reach.intersect(lifted.inverse().transform_rect_bbox(slot.area))
        };
        let hovered = !carried
            && self
                .desk
                .pointer
                .and_then(|p| self.desk_hit_at(p, now))
                .is_some_and(|(id, ..)| id == slot.id);
        let (glance_alpha, card_alpha) = if reduced {
            #[allow(clippy::cast_possible_truncation, reason = "a unit fraction")]
            let open = open as f32;
            (1.0 - open, open)
        } else {
            #[allow(clippy::cast_possible_truncation, reason = "a unit fraction")]
            let morph = morph as f32;
            (
                (1.0 - 2.0 * morph).clamp(0.0, 1.0),
                (2.0 * morph - 1.0).clamp(0.0, 1.0),
            )
        };
        let mid = !reduced && morph > 1e-3 && morph < 1.0 - 1e-3;
        let anchor = self.desk_anchor(slot.id);
        let (scene, text, content) = (&mut *pass.scene, &mut *pass.text, pass.content);
        with_alpha(scene, at, bounds, shown, |scene| {
            paint_lift(scene, at, theme, ground, radius, lift);
            if mid {
                paint_morph_ground(scene, at, theme, ground, radius);
            }
            if glance_alpha > f32::EPSILON {
                with_alpha(scene, at, bounds, glance_alpha, |scene| {
                    self.paint_glance(
                        &mut Brush {
                            scene: &mut *scene,
                            at,
                            text: &mut *text,
                            theme,
                            content,
                        },
                        slot.id,
                        drawn.glance,
                        !mid,
                    );
                    if hovered && card_alpha <= f32::EPSILON {
                        let badge = badge_rect(drawn.glance, anchor);
                        let on_badge = self.desk.pointer.is_some_and(|p| badge.contains(p));
                        paint_badge(scene, at, theme, badge, on_badge);
                    }
                });
            }
            if card_alpha > f32::EPSILON {
                if mid {
                    // Inside the growing shape only.
                    scene.push_layer(
                        Fill::NonZero,
                        BlendMode::default(),
                        card_alpha,
                        at,
                        &RoundedRect::from_rect(ground, radius),
                    );
                    self.paint_card(
                        &mut Brush {
                            scene: &mut *scene,
                            at,
                            text: &mut *text,
                            theme,
                            content,
                        },
                        slot.id,
                        drawn.open,
                        false,
                        hovered,
                    );
                    scene.pop_layer();
                } else {
                    with_alpha(scene, at, bounds, card_alpha, |scene| {
                        self.paint_card(
                            &mut Brush {
                                scene: &mut *scene,
                                at,
                                text: &mut *text,
                                theme,
                                content,
                            },
                            slot.id,
                            drawn.open,
                            true,
                            hovered,
                        );
                    });
                }
            }
        });
        painted.push(at.transform_rect_bbox(bounds));
    }

    /// A widget just removed, `gone` of the way faded, shrinking a little
    /// where it was let go.
    fn paint_leaving(
        &mut self,
        pass: &mut DeskPass<'_>,
        leaving: Leaving,
        gone: f64,
        painted: &mut Vec<Rect>,
    ) {
        let rect = if leaving.form == Form::Open {
            leaving.slot.open
        } else {
            leaving.slot.glance
        };
        let scale = if self.desk.motion.reduced() {
            1.0
        } else {
            1.0 - 0.08 * gone
        };
        let at = pass.transform * Affine::scale_about(scale, rect.center());
        #[allow(clippy::cast_possible_truncation, reason = "a unit fraction")]
        let alpha = (1.0 - gone).clamp(0.0, 1.0) as f32;
        let bounds = rect.inflate(40.0, 48.0);
        let (scene, text, theme, content) =
            (&mut *pass.scene, &mut *pass.text, pass.theme, pass.content);
        with_alpha(scene, at, bounds, alpha, |scene| {
            if leaving.form == Form::Open {
                self.paint_card(
                    &mut Brush {
                        scene: &mut *scene,
                        at,
                        text: &mut *text,
                        theme,
                        content,
                    },
                    leaving.id,
                    leaving.slot.open,
                    true,
                    false,
                );
            } else {
                self.paint_glance(
                    &mut Brush {
                        scene: &mut *scene,
                        at,
                        text: &mut *text,
                        theme,
                        content,
                    },
                    leaving.id,
                    leaving.slot.glance,
                    true,
                );
            }
        });
        painted.push(at.transform_rect_bbox(bounds));
    }

    /// `id`'s glance at `rect`, on its own ground when `ground`.
    fn paint_glance(&self, brush: &mut Brush<'_>, id: WidgetId, rect: Rect, ground: bool) {
        let (scene, at, text, theme, content) = brush.parts();
        let cursor = self.desk.pointer;
        let has = match id {
            WidgetId::Music => content.music.is_some(),
            WidgetId::Team => !content.members.is_empty(),
            WidgetId::Calendar => true,
            WidgetId::SystemMonitor => !content.reading.is_empty(),
        };
        if !has {
            return;
        }
        if ground {
            paint_glance_ground(scene, at, theme, id, rect);
        }
        match id {
            WidgetId::Music => {
                if let Some(view) = content.music.as_ref() {
                    paint_music_glance(scene, at, text, theme, rect, view.player(), cursor);
                }
            }
            WidgetId::Team => {
                let vertical = rect.height() > rect.width();
                paint_team_glance(scene, at, text, theme, rect, vertical, &content.members);
            }
            WidgetId::Calendar => {
                paint_calendar_glance(scene, at, text, theme, rect, &content.month);
            }
            WidgetId::SystemMonitor => {
                paint_monitor_glance(scene, at, text, theme, rect, content.reading);
            }
        }
    }

    /// `id`'s open card at `open` (band and card): on its own ground when
    /// `ground`, its content cross-fading between the stages its width sits
    /// between, the resize handle while `hovered`.
    #[allow(
        clippy::too_many_lines,
        reason = "the band, the stages' cross-fade and the handle of one card"
    )]
    fn paint_card(
        &mut self,
        brush: &mut Brush<'_>,
        id: WidgetId,
        open: Rect,
        ground: bool,
        hovered: bool,
    ) {
        let (scene, at, text, theme, content) = brush.parts();
        let card = card_rect(open);
        let cursor = self.desk.pointer;
        let carried = self
            .desk
            .grab
            .as_ref()
            .is_some_and(|grab| grab.id == id && grab.dragging);
        let cursor_on = |rect: Rect| !carried && cursor.is_some_and(|p| rect.contains(p));
        if ground {
            paint_card_ground(scene, at, theme, open);
        }
        // The band: collapse at the left, pin and remove at the right.
        let target = format!("{DESK_TARGET}{}", id.key());
        let title = desk_widget_title(id);
        let pinned = self.desk.prefs.get(id).always_on(self.desk_strip());
        let collapse_label = t_args("desk-widget-collapse", &[("widget", title.as_str().into())]);
        let remove_label = t_args("desk-widget-remove", &[("widget", title.as_str().into())]);
        let pin_label = t(if pinned {
            "desk-widget-unpin"
        } else {
            "desk-widget-pin"
        });
        let focus_visible = self.desk_focus_visible();
        let focus = |suffix: &str| {
            focus_visible
                && self
                    .desk
                    .targets
                    .interaction(&format!("{target}{suffix}"))
                    .focused
        };
        for (rect, icon, label, toggled, suffix) in [
            (
                collapse_rect(open),
                ui::icons::Icon::ChevronDown,
                &collapse_label,
                false,
                "",
            ),
            (
                pin_rect(open),
                ui::icons::Icon::Pin,
                &pin_label,
                pinned,
                ".pin",
            ),
            (
                close_rect(open),
                ui::icons::Icon::X,
                &remove_label,
                false,
                ".remove",
            ),
        ] {
            paint_band_control(
                scene,
                at,
                text,
                theme,
                rect,
                (icon, label),
                (toggled, cursor_on(rect), focus(suffix)),
            );
        }
        let width = card.width();
        let (lo, hi, k) = sizing::stages(id, width);
        let stages: Vec<(Stage, f32)> = if lo == hi {
            vec![(lo, 1.0)]
        } else {
            #[allow(clippy::cast_possible_truncation, reason = "a unit fraction")]
            let k = k as f32;
            vec![(lo, 1.0 - k), (hi, k)]
        };
        let radius = ui::widget::floating_surface::card_radius(card, theme);
        for (stage, alpha) in stages {
            if alpha <= f32::EPSILON {
                continue;
            }
            with_alpha(scene, at, card.inflate(2.0, 2.0), alpha, |scene| match id {
                WidgetId::Music => {
                    let Some(view) = content.music.as_ref() else {
                        return;
                    };
                    scene.push_layer(
                        Fill::NonZero,
                        BlendMode::default(),
                        1.0,
                        at,
                        &RoundedRect::from_rect(card, radius),
                    );
                    match stage {
                        Stage::Compact => paint_music_compact(
                            scene,
                            at,
                            text,
                            theme,
                            card,
                            view.player(),
                            cursor.filter(|_| !carried),
                        ),
                        Stage::Regular | Stage::Large => {
                            let regular = if stage == Stage::Large {
                                let (art, below) = music_large_parts(card);
                                paint_music_large_art(scene, at, text, theme, art, view.player());
                                below
                            } else {
                                card
                            };
                            self.desk.music_card.set_text_scale(theme.text_scale);
                            self.desk.music_card.layout(regular);
                            self.desk.music_card.draw(
                                scene,
                                at,
                                text,
                                theme,
                                regular,
                                cursor.filter(|_| !carried),
                                view,
                            );
                        }
                    }
                    scene.pop_layer();
                    // Its artwork may reach the edge: the card's edge stays
                    // on top.
                    if ground {
                        ui::widget::floating_surface::paint_card_edge(scene, at, open, theme);
                    }
                }
                WidgetId::Team => paint_team_card(
                    scene,
                    at,
                    text,
                    theme,
                    card,
                    &content.members,
                    stage,
                    cursor.filter(|_| !carried),
                ),
                WidgetId::Calendar => {
                    paint_calendar_card(scene, at, text, theme, card, &content.month, stage);
                }
                WidgetId::SystemMonitor => {
                    paint_monitor_card(scene, at, text, theme, card, content.reading, stage);
                }
            });
        }
        let resizing = self.desk.resizing.is_some_and(|(resized, _)| resized == id);
        if hovered || resizing {
            let anchor = self.desk_anchor(id);
            let grip = grip_rect(card, anchor);
            paint_grip(scene, at, theme, grip, anchor, resizing || cursor_on(grip));
        }
    }

    /// Search and the desk, in their stacking order: the scrim (light over
    /// the Super surface), the widgets, the time and status, then the
    /// launcher's card. Each is its own damage part, so a keystroke
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
        super::overlay_part(scene, "shell.palette", desktop, |scene| {
            palette_shown
                .then(|| self.draw_palette_card(scene, transform, text_renderer, theme, presence))
                .flatten()
        });
    }
}

/// One painting pass of the desk: where it draws (`scene`, `transform`) and
/// what it draws with (`text`, `theme`) and from (`content`).
struct DeskPass<'a> {
    scene: &'a mut Scene,
    transform: Affine,
    text: &'a mut TextRenderer,
    theme: &'a Theme,
    content: &'a DeskContent,
}

/// Where one widget is painted (`scene`, at `at`) and with what (`text`,
/// `theme`) and from (`content`).
struct Brush<'a> {
    scene: &'a mut Scene,
    at: Affine,
    text: &'a mut TextRenderer,
    theme: &'a Theme,
    content: &'a DeskContent,
}

impl Brush<'_> {
    fn parts(&mut self) -> (&mut Scene, Affine, &mut TextRenderer, &Theme, &DeskContent) {
        (
            &mut *self.scene,
            self.at,
            &mut *self.text,
            self.theme,
            self.content,
        )
    }
}

/// What every widget of a pass shows, gathered once so a repeated paint of
/// the same widget reads the same.
struct DeskContent {
    music: Option<NowPlayingView>,
    month: Month,
    reading: Reading,
    members: Vec<TeamMember>,
}

/// Whether `tile` is a window of `app`: a package id or a desktop entry.
fn tile_is_app(tile: &crate::shell::state::TileContent, app: &str) -> bool {
    use crate::shell::state::TileContent;
    let (TileContent::Foreign { app_id, .. }
    | TileContent::LocalApp { app_id, .. }
    | TileContent::HostedWindow { app_id, .. }) = tile
    else {
        return false;
    };
    app_id.eq_ignore_ascii_case(app) || crate::shell::launcher::app_id_matches_package(app_id, app)
}

/// How far a widget carried by `grab` is drawn from its place, the pointer
/// at `p`.
fn offset_of(grab: &Grab, p: Point) -> Vec2 {
    grab.offset + (p - grab.at)
}

/// `a` toward `b` by `t`.
fn lerp_rect(a: Rect, b: Rect, t: f64) -> Rect {
    let t = t.clamp(0.0, 1.0);
    Rect::new(
        a.x0 + (b.x0 - a.x0) * t,
        a.y0 + (b.y0 - a.y0) * t,
        a.x1 + (b.x1 - a.x1) * t,
        a.y1 + (b.y1 - a.y1) * t,
    )
}

/// How widget `id` comes and goes: as ambient chrome does, by opacity in
/// place, going at once. The Team widget's faces are people, and a person
/// shown faintly reads as a ghost, so it always arrives as if asked for, at
/// `fast`, whole as its motion ends and otherwise hidden, never on the slow
/// unasked fade. Its motion is exactly its fade: a frame it asks for is a
/// frame that changes.
fn shown_spec(id: WidgetId, asked: bool) -> ui::motion::MotionSpec {
    ui::motion::Ambient::spec(asked || id == WidgetId::Team)
}

/// How much of a widget shows at `progress` of its coming and going
/// ([`shown_spec`]).
fn shown_opacity(progress: f64) -> f32 {
    ui::motion::unit_opacity(progress)
}

#[cfg(test)]
#[path = "desk_tests.rs"]
mod tests;
