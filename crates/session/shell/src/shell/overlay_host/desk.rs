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
use ui::ColorCompat as _;
use ui::TextRole;
use ui::presence::PresenceCtx;
use ui::text_render::{TextRenderer, TextWeight};
use ui::theme::Theme;
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect, RoundedRect, Size, Stroke};
use vello::peniko::{BlendMode, Fill};
use winit::event::{ElementState, MouseButton, WindowEvent};

use super::ShellOverlayHost;
use super::layers::LayerId;
use crate::shell::desk_widgets::layout::{self, Item};
use crate::shell::desk_widgets::team::{self, TeamMember};
use crate::shell::desk_widgets::{
    Context, DeskBoard, DeskWidgetPrefs, DockEdge, FADE_IN, FADE_OUT, Form, SLIDE_PX, ShowMode,
    Slot, WidgetId,
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
const SUPER_HEADER_GAP: f64 = 16.0;
/// Width of every open card.
const CARD_WIDTH: f64 = 320.0;
/// The band above an open card holding its pin.
const PIN_BAND: f64 = 32.0;
const PIN: f64 = 26.0;
/// Team avatars in the glance form.
const AVATAR: f64 = 28.0;
const AVATAR_STEP: f64 = 22.0;
const GLANCE_AVATARS: usize = 5;
const TEAM_HEADER: f64 = 36.0;
const TEAM_ROW: f64 = 62.0;
const TEAM_MORE: f64 = 24.0;
/// A press that moves this far becomes a drag to another edge.
const DRAG_SLOP: f64 = 6.0;

/// How the Super surface was opened.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SuperSurface {
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
    shown: BTreeMap<WidgetId, ui::Transition>,
    opened: BTreeMap<WidgetId, ui::Transition>,
    super_surface: Option<SuperSurface>,
    /// Where the time card was last painted.
    super_header_rect: std::cell::Cell<Option<Rect>>,
    system_activities: Vec<Rect>,
    music_card: NowPlayingCard,
    press: Option<Press>,
    team_count: usize,
    now_playing_revision: u64,
    /// The theme's reduced-motion choice, as last painted: fades then
    /// complete at once.
    reduce_motion: bool,
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
            system_activities: Vec::new(),
            music_card: NowPlayingCard::new(),
            press: None,
            team_count: 0,
            now_playing_revision: 0,
            reduce_motion: false,
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
                self.open_super_surface(SuperSurface::Peek, doc, root);
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
            (SuperGesture::PeekCancelled, Some(SuperSurface::Peek)) => self.close_super_surface(),
            _ => return,
        }
        self.bump_chrome();
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
        self.refresh_search_results(doc, root);
    }

    /// Close the Super surface (and the search it holds).
    pub fn close_super_surface(&mut self) {
        self.palette_visible = false;
        self.palette.close();
        self.end_super_surface();
    }

    fn end_super_surface(&mut self) {
        self.desk.super_surface = None;
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
        }
    }

    /// Lay the widgets out and decide their forms at `now`. Returns whether
    /// anything on the desk changed.
    pub(crate) fn update_desk(&mut self, now: Instant) -> bool {
        if !self.palette_visible && self.desk.super_surface.is_some() {
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
        let super_up = self.desk.super_surface.is_some();
        let area = self.overlay_area();
        let slots = layout::place(area, &items, &self.desk.system_activities, super_up);
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
            covered,
            slots: &self.desk.slots,
        };
        changed |= self.desk.board.update(&ctx);
        let reduce_motion = self.desk.reduce_motion;
        for slot in &self.desk.slots {
            let form = self.desk.board.form(slot.id);
            let shown = form != Form::Away;
            let fade = if reduce_motion {
                Duration::ZERO
            } else if shown {
                FADE_IN
            } else {
                FADE_OUT
            };
            self.desk
                .shown
                .entry(slot.id)
                .or_insert_with(ui::Transition::closed)
                .set_open_at(shown, fade, ui::Curve::EaseOut, now);
            self.desk
                .opened
                .entry(slot.id)
                .or_insert_with(ui::Transition::closed)
                .set_open_at(form == Form::Open, fade, ui::Curve::EaseOut, now);
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
        self.desk.board.deadline(now).into_iter().chain(clock).min()
    }

    /// Whether a widget is fading or folding.
    pub(crate) fn desk_animating(&self, now: Instant) -> bool {
        self.desk
            .shown
            .values()
            .chain(self.desk.opened.values())
            .any(|transition| transition.is_animating(now))
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
        self.desk.reduce_motion = theme.reduce_motion;
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
        let drag = self
            .desk
            .press
            .filter(|press| press.dragging)
            .zip(self.last_cursor)
            .map(|(press, p)| (press.id, p - press.at));
        for slot in self.desk.slots.clone() {
            let shown = self.desk.shown.get(&slot.id).map_or(0.0, |t| t.value(now));
            if shown <= f32::EPSILON {
                continue;
            }
            let open = self.desk.opened.get(&slot.id).map_or(0.0, |t| t.value(now));
            let edge = self
                .desk
                .edges
                .get(&slot.id)
                .copied()
                .unwrap_or_else(|| slot.id.default_edge());
            // In from the edge as it fades in; nothing else moves.
            let slide = layout::outward(edge) * (SLIDE_PX * f64::from(1.0 - shown));
            let shift = drag
                .filter(|(id, _)| *id == slot.id)
                .map_or(slide, |(_, delta)| slide + delta);
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
                    },
                );
                painted.push(at.transform_rect_bbox(slot.glance.inflate(24.0, 32.0)));
            }
            let open_alpha = shown * open;
            if open_alpha > f32::EPSILON {
                let content = OpenContent {
                    music: music.as_ref(),
                    members: &members,
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
                        let radius = ui::chrome::ChromeRole::Panel.radius(theme, card.height());
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
                    }
                }
                WidgetId::Team => {
                    paint_card_ground(scene, at, theme, card);
                    paint_team_card(scene, at, text, theme, card, content.members);
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
        let palette_now = Instant::now();
        let palette_shown = self.palette_transition.is_visible(palette_now);
        let desktop = transform.transform_rect_bbox(self.rect);
        let super_up = self.super_surface().is_some();
        super::overlay_part(scene, "shell.palette-scrim", desktop, |scene| {
            if palette_shown {
                let openness = self.palette_transition.value(palette_now);
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
            let openness = if super_up && self.palette.shows_field_only() {
                self.palette_transition.value(palette_now)
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
        let field = ui::Widget::get_rect(&self.palette);
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

fn team_card_height(people: usize) -> f64 {
    let rows = people.min(team::MAX_ROWS);
    let more = if people > team::MAX_ROWS {
        TEAM_MORE
    } else {
        0.0
    };
    TEAM_HEADER + TEAM_ROW * ui::num::f64_of_usize(rows) + more + 8.0
}

/// The opaque floating ground every desk card stands on: overlay surface,
/// panel edge, the floating elevation. No blur.
fn paint_card_ground(scene: &mut Scene, transform: Affine, theme: &Theme, rect: Rect) {
    let radius = ui::chrome::ChromeRole::Panel.radius(theme, rect.height());
    ui::surface::shadow(
        scene,
        transform,
        rect,
        radius,
        &theme.tokens.elevation.floating,
    );
    ui::surface::card(
        scene,
        transform,
        rect,
        radius,
        theme.tokens.colors.surfaces.overlay,
    );
    scene.stroke(
        &Stroke::new(1.0),
        transform,
        ui::chrome::panel_border(theme),
        None,
        &RoundedRect::from_rect(rect, radius),
    );
}

fn paint_pill_ground(scene: &mut Scene, transform: Affine, theme: &Theme, rect: Rect) {
    let radius = ui::chrome::ChromeRole::Selector.radius(theme, rect.height());
    let mut shadow = theme.tokens.elevation.floating;
    shadow.color = shadow.color.with_alpha_factor(0.6);
    ui::surface::shadow(scene, transform, rect, radius, &shadow);
    ui::surface::card(
        scene,
        transform,
        rect,
        radius,
        theme.tokens.colors.surfaces.overlay,
    );
    scene.stroke(
        &Stroke::new(1.0),
        transform,
        ui::chrome::panel_border(theme),
        None,
        &RoundedRect::from_rect(rect, radius),
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
    scene.stroke(
        &Stroke::new(1.0),
        transform,
        ui::chrome::panel_border(theme),
        None,
        &RoundedRect::from_rect(rect, radius),
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
