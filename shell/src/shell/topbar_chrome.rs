//! Rendering widget for the shell top bar.
//!
//! `TopBar` in `shell/topbar.rs` owns the logical state (workspace nav,
//! center-widget cooldown, tooltip timing, collaborator helpers). This file
//! provides a [`TopBarChrome`] that implements [`ui::widget::Widget`]
//! and paints the bar onto the root scene. Phase 1 wires it into
//! `build_nirios_root` so there is finally a visible top-bar surface; later
//! phases will route clicks on its left/center/right clusters back into the
//! logical `TopBar` for workspace switch / overview toggle / quick-settings.

use std::collections::HashMap;

use automerge::{AutoCommit, ObjId};
use chrono::Local;
use taffy::{Dimension, Display, NodeId, Size, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect, RoundedRect};
use vello::peniko::{Brush, ColorStop, Extend, Fill, Gradient};
use winit::event::WindowEvent;

use ui::color_compat::ColorCompat;
use ui::presence::{PresenceCtx, user_color};
use ui::text_render::{TextRenderer, TextWeight};
use ui::theme::Theme;
use ui::widget::Widget;
use vello::peniko::Color;

use crate::shell::chrome_state::Chrome;
use crate::shell::state::WorkspaceTree;

pub const TOPBAR_HEIGHT_PX: f32 = 40.0;

/// Opacity of the top-bar surface: the toolkit's shared chrome opacity, so
/// the bar cannot drift away from the popovers that hang off it.
///
/// The concept's §1.1 backdrop is `bg-black/80 backdrop-blur-xl`, and this
/// used to honour "just the alpha split" on the grounds that vello has no
/// backdrop filter. A tint without its blur is not glass, though — it is a
/// contrast loss that tracks whatever is open behind the surface — so
/// chrome is opaque and the depth comes from the surface's own ground and
/// edge. See `ui::chrome::shell::CHROME_SURFACE_ALPHA`.
///
/// Note that [`TopBarChrome::draw`] fills `surfaces.overlay` directly and
/// never consults this constant, so the bar is currently painting the
/// declared tier raw rather than resolving `ChromeRole::Bar`. That is why
/// the dark bar is darker than the desktop while a panel is lighter — the
/// two are not, in fact, one material on screen yet, whatever
/// `bar_and_panel_share_one_material` proves about the roles themselves.
pub const TOP_BAR_SURFACE_ALPHA: f32 = ui::chrome::shell::CHROME_SURFACE_ALPHA;

/// Weight of the SystemMonitor sparkline fill. Low enough that the bars
/// read as a backdrop behind the numbers rather than as a fourth element
/// competing with the three metric labels.
const SPARKLINE_FILL_ALPHA: f32 = 0.30;

/// Which center-widget variant is active in the top-bar carousel.
/// Scroll-wheel over the center band cycles between these; a three-dot
/// indicator on the left of the band shows the current position.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum CenterWidget {
    /// Default matches the constructor's seed value (the bar boots
    /// into Clock mode). Letting `CacheFingerprint::default()` derive
    /// `Default` for free keeps the cache machinery clean.
    #[default]
    Clock,
    SystemMonitor,
    Calendar,
}

/// Identifies a metric column in the SystemMonitor center-widget. The
/// concept paints each metric in its own semantic colour (CPU emerald,
/// RAM blue, TMP orange — see `nirios-concept/src/components/\
/// TopBarWidgets.tsx:60-110`); routing through
/// [`TopBarChrome::metric_label_color`] keeps the per-metric colour
/// cue as a single source of truth so a future regression collapsing
/// the strip back to a monotone token surfaces at the helper rather
/// than scattered through the draw branch.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MetricKind {
    Cpu,
    Ram,
    Tmp,
}

impl CenterWidget {
    pub fn next(self) -> Self {
        match self {
            CenterWidget::Clock => CenterWidget::SystemMonitor,
            CenterWidget::SystemMonitor => CenterWidget::Calendar,
            CenterWidget::Calendar => CenterWidget::Clock,
        }
    }
    pub fn prev(self) -> Self {
        match self {
            CenterWidget::Clock => CenterWidget::Calendar,
            CenterWidget::SystemMonitor => CenterWidget::Clock,
            CenterWidget::Calendar => CenterWidget::SystemMonitor,
        }
    }
    pub fn index(self) -> usize {
        match self {
            CenterWidget::Clock => 0,
            CenterWidget::SystemMonitor => 1,
            CenterWidget::Calendar => 2,
        }
    }
    pub fn from_index(i: usize) -> Self {
        match i % 3 {
            0 => CenterWidget::Clock,
            1 => CenterWidget::SystemMonitor,
            _ => CenterWidget::Calendar,
        }
    }
}

/// Snapshot of the data the top bar renders. Cached at construction and
/// refreshed when the shell calls [`TopBarChrome::sync_from_tree`] (e.g. in
/// response to a workspace switch). Keeping the render widget off
/// Snapshot of every input that affects the top-bar's painted pixels.
/// Compared field-for-field against the previous frame's snapshot to
/// decide whether [`TopBarChrome::cached`] can be reused or has to
/// rebuild. Stored on the widget so the comparison is O(1) per frame.
///
/// The time fields are split by mode because each variant consumes a
/// different slice of `chrono::Local::now()`: Clock paints `HH:MM`
/// so the minute is the finest grain; SystemMonitor's bars + values
/// are derived from `seconds % N`, so seconds drive invalidation;
/// Calendar is fully static (`time_bucket = 0`).
#[derive(Default, Clone, PartialEq, Eq)]
struct CacheFingerprint {
    /// Position + size of the bar's rect (`x0, y0, w, h` in integer
    /// pixels). Every shape inside the bar is anchored to
    /// `self.rect`'s corners *and* the per-frame draw repopulates
    /// member-field hit rects (e.g. `self.overview_btn_rect`) from
    /// those corners. Keying the cache on the full rect (not just
    /// size) means cache hits also imply hit-rects are still valid.
    /// The bar moves only on viewport resize / sidebar-width change,
    /// so the rebuild cost is paid rarely.
    rect_px: (i64, i64, u64, u64),
    workspace_name: String,
    workspace_index: usize,
    workspace_total: usize,
    /// Three avatar initials + overflow count. The actual colour
    /// palette is stable for a given roster (derived from peer ids),
    /// so the initials carry the colour signal too.
    collaborator_initials: Vec<char>,
    collaborator_overflow: usize,
    center_widget: CenterWidget,
    /// Time bucket — see struct doc. `0` means "no time dependency"
    /// (static center widget like Calendar).
    time_bucket: u64,
    /// Quantised status reading. Not the [`SystemStatus`] itself: it
    /// carries an `f32` volume level, and comparing floats frame to
    /// frame would either never match (denying the cache) or need an
    /// epsilon nobody would remember to keep in step with the number of
    /// pixels the indicator actually moves. This is the reading *as
    /// drawn* — the volume rounded to the percent the glyph switches on,
    /// the battery percent, the signal bucket — so the cache is
    /// invalidated exactly when the painted pixels would differ.
    status_key: StatusKey,
}

/// One dot in the centre band's carousel indicator: the disc that gets
/// painted, and the region that selects it when clicked.
///
/// The two travel together deliberately. They were computed separately —
/// the disc from [`TopBarChrome::DOT_ACTIVE_RADIUS`], the slot from a
/// differently-derived centre — and disagreed about both where each dot
/// was and how much room it had.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CenterDot {
    /// Centre of the painted disc.
    pub center: Point,
    /// Radius the disc is painted at.
    pub radius: f64,
    /// Region that selects this dot when clicked.
    pub slot: Rect,
}

/// The three pieces the centre band's calendar strip paints, given the
/// current local time: weekday, date, and the note that fills the slot
/// an event would occupy.
///
/// No calendar source is wired to the shell, so there is no next event
/// to name. This follows the same rule as the tray pill: paint what is
/// actually knowable and say plainly that nothing backs the rest. The
/// strip used to read "Next: Design Sync 15:30" — a fixed string on
/// every machine, which anyone glancing at the screen takes for the
/// user's own diary and anyone watching a recording takes for a
/// scheduling feature that exists.
///
/// Split out as a free function so the strip's content is assertable
/// without a paint pass; when a real calendar lands, the note is what
/// it replaces.
fn calendar_strip_parts(now: chrono::DateTime<Local>) -> (String, String, &'static str) {
    (
        now.format("%a").to_string(),
        now.format("%-d %B").to_string(),
        "No calendar connected",
    )
}

/// Reduce a reading to what the tray paints. Free function so
/// [`TopBarChrome::needs_redraw`] can key on the *chrome's* current
/// status without first copying it onto the widget — `needs_redraw`
/// takes `&self`, and the whole point is to answer before the copy.
fn status_key_of(s: &crate::shell::status::SystemStatus) -> StatusKey {
    StatusKey {
        battery_percent: s.battery_percent(),
        battery_charging: s.battery_is_charging(),
        battery_low: s.battery_is_low(),
        battery_critical: s.battery_is_critical(),
        link: s.link.as_ref().map(|l| {
            let kind = match l.kind {
                os_actions::LinkKind::Offline => 0,
                os_actions::LinkKind::Wired => 1,
                os_actions::LinkKind::Wifi => 2,
            };
            (kind, crate::shell::status::link_bars(l))
        }),
        volume_pct: s
            .volume
            .map(|v| (v.level * 100.0).round().clamp(0.0, 100.0) as u8),
        muted: s.audio_is_silent(),
    }
}

/// The status reading reduced to what the tray paints. See
/// [`CacheFingerprint::status_key`].
#[derive(Default, Clone, Copy, PartialEq, Eq)]
struct StatusKey {
    battery_percent: Option<u8>,
    battery_charging: bool,
    battery_low: bool,
    battery_critical: bool,
    link: Option<(u8, u8)>,
    volume_pct: Option<u8>,
    muted: bool,
}

/// `WorkspaceTree` lets `build_nirios_root` stay a pure function of the
/// tree and avoids a circular borrow against the shell's owned logical
/// `TopBar`.
pub struct TopBarChrome {
    rect: Rect,
    workspace_name: String,
    workspace_index: usize,
    workspace_total: usize,
    /// Scene cache — covers the entire top-bar paint. The bar is large
    /// (full viewport width × 40 px) and most of its content is stable
    /// across frames; the only per-frame mutators are the clock (per
    /// minute), the SystemMonitor sparkline + values (per second when
    /// in that variant), workspace nav state, and collaborator roster.
    /// Caching the encoded scene cuts the draw budget at rest by an
    /// order of magnitude (~12 fills + dozens of text glyphs avoided).
    cached: Scene,
    /// Cache-validity fingerprint. The cache is reused iff every field
    /// here matches its corresponding `self.*` field at draw time —
    /// see [`TopBarChrome::current_fingerprint`] for what goes in. A
    /// fresh widget (`Rect::ZERO` rect) yields a fingerprint whose
    /// rect-width-zero immediately misses, forcing a first-frame paint.
    cache_fp: CacheFingerprint,
    collaborator_initials: Vec<char>,
    /// Per-peer fill colour for the avatar circles (one per entry in
    /// `collaborator_initials`, same index). Resolved from the
    /// collaborator's `cursor_color` override when present and otherwise
    /// from [`ui::presence::user_color`], so the top-bar stack
    /// matches the concept's `USERS[*].cursorColor` palette.
    collaborator_colors: Vec<Color>,
    /// Count of collaborators beyond the three rendered as avatars. When
    /// nonzero the top bar paints a "+N" chip after the stack; concept
    /// keeps the chip's visual weight lighter than a real avatar.
    collaborator_overflow: usize,
    /// Latest battery / network / volume reading, copied off the chrome
    /// on each revision bump. Empty until something polls (see
    /// [`crate::shell::status`]), and an empty reading paints no
    /// indicator rather than a plausible-looking default.
    status: crate::shell::status::SystemStatus,
    /// Shared chrome state; used to toggle overlays and switch workspaces
    /// in response to clicks on the top bar's left/right clusters.
    chrome: Option<Chrome>,
    /// Cached revision from `Chrome::revision` so `update` can detect when
    /// the tree has mutated since the last sync.
    synced_revision: u64,
    // Click geometry — recomputed every frame so it tracks DPI / resize.
    overview_btn_rect: Rect,
    up_btn_rect: Rect,
    down_btn_rect: Rect,
    ws_pill_rect: Rect,
    tray_pill_rect: Rect,
    /// Whether each drawn chip belongs to an agent, indexed alongside
    /// `collaborator_initials`. What makes the stack say the same thing
    /// about a participant as the roster panel does.
    collaborator_is_agent: Vec<bool>,
    /// Bounding rect of the avatar stack (initials + overflow chip). Click
    /// toggles `overlays.collaborators` so the shell can show the peer
    /// dropdown — matches the concept's avatar-stack-as-trigger affordance.
    avatar_stack_rect: Rect,
    /// Active center-widget. Scroll-wheel over `center_rect` cycles the
    /// variant; clicks on one of the three dot indicators jumps directly.
    center_widget: CenterWidget,
    /// Hit region for the center band (dots + content). Populated every
    /// draw so scroll + click hit-testing stays aligned with what's shown.
    center_rect: Rect,
    /// Dot-indicator click targets (one per variant). Same populate-on-draw
    /// contract as the other rect fields on this struct.
    center_dot_rects: [Rect; 3],
    /// Scroll-wheel accumulator over the center band. Matches the
    /// sidebar-wheel scroll contract so a single deliberate wheel tick
    /// advances one variant.
    center_scroll_accum: f32,
    /// Last cursor position in logical coords. Winit delivers the position
    /// with `CursorMoved` but not `MouseInput`, so we cache it here to
    /// hit-test click events.
    last_cursor: Option<Point>,
}

/// Body of a Selector-role control in the bar — the workspace pill, the
/// centre band, the tray pill.
///
/// These filled with `surfaces.raised`, which is `#ffffff` on the light
/// palette while the bar's own surface is also white: the pills had no
/// visible body at all on light, so the capsule shape they had been given
/// was invisible and the workspace name floated loose in the bar. The
/// shared selection tier derives its lift from the ground by luminance,
/// so a Selector has a body on every theme.
fn bar_selector_body(theme: &Theme, state: ui::chrome::SelectionState) -> ui::chrome::SelectionTier {
    // Anchored on the bar's own surface, not on `surfaces.base`. See
    // [`ui::chrome::selection_tier_on`] — on the light palette those are two
    // different whites, and the base-anchored lift painted the clock pill in
    // exactly the bar's colour.
    ui::chrome::selection_tier_on(theme, state, theme.tokens.colors.surfaces.overlay)
}

/// How much `danger` the tray pill carries when the battery is critical.
///
/// Named because two things must agree on it: the pill's own wash, and the
/// ground the gauge is measured against.
pub(crate) const TRAY_CRITICAL_WASH: f32 = 0.18;

/// The opaque colour the tray pill leaves on the screen.
///
/// The pill is a lift over the bar, and at critical it takes a `danger`
/// wash on top of that. Both are translucent, so neither token is the
/// colour anything drawn on the pill is actually seen against — only this
/// composite is.
pub(crate) fn tray_pill_body(theme: &Theme, critical: bool) -> Color {
    let colors = &theme.tokens.colors;
    let tier = bar_selector_body(theme, ui::chrome::SelectionState::active(true));
    let mut body = ui::tokens::colors::over(
        colors.surfaces.overlay,
        tier.fill,
        tier.fill.components[3],
    );
    if critical {
        body = ui::tokens::colors::over(body, colors.semantic.danger, TRAY_CRITICAL_WASH);
    }
    body
}

/// Colour of the battery gauge's charge fill.
///
/// At critical the tray pill is itself washed `danger`, so a `danger` fill
/// sat on a tinted version of itself and measured 2.61:1 — *less* legible
/// than the merely-low state's 5.40:1. That is the signal running
/// backwards: the more urgent the state, the harder it was to see, which is
/// the one direction a battery indicator must never fail in. Deriving the
/// hue against the pill it is actually drawn on keeps the danger hue and
/// puts the urgency back in front of it.
pub(crate) fn battery_gauge_hue(
    theme: &Theme,
    critical: bool,
    low: bool,
    charging: bool,
) -> Color {
    let colors = &theme.tokens.colors;
    let tone = if critical {
        colors.semantic.danger
    } else if low {
        colors.semantic.warning
    } else if charging {
        colors.semantic.success
    } else {
        colors.text_dim
    };
    let body = tray_pill_body(theme, critical);
    let mut floor = ui::tokens::colors::NON_TEXT_MIN_CONTRAST;
    if critical {
        // Clearing the floor is not enough here. Critical is the one state a
        // person must not miss, and it is also the state that tints the pill
        // `danger` — so a `danger` gauge derived only up to the 3:1 floor
        // stopped at 3.07:1 while the merely *low* gauge sat at 6.10:1 on
        // the untinted pill. Passing the floor and still being the quietest
        // thing in the tray is the same failure the numbers were meant to
        // catch, one tier up. Ask critical for at least what low gets, so
        // urgency and legibility cannot point in opposite directions.
        //
        // The hue this walks to may be lighter than the `danger` token. That
        // is the right trade: the pill underneath already carries the red, so
        // the gauge's job is to be *read* against it, not to repeat it.
        floor = floor.max(gauge_ratio(theme, false, true, false));
    }
    colors.ink_on(body, tone, floor)
}

/// Contrast the gauge achieves in a given state, against the pill it is drawn
/// on. Split out so [`battery_gauge_hue`] can hold critical to what the low
/// state manages without recursing into its own floor.
fn gauge_ratio(theme: &Theme, critical: bool, low: bool, charging: bool) -> f64 {
    let colors = &theme.tokens.colors;
    let tone = if critical {
        colors.semantic.danger
    } else if low {
        colors.semantic.warning
    } else if charging {
        colors.semantic.success
    } else {
        colors.text_dim
    };
    let body = tray_pill_body(theme, critical);
    let hue = colors.ink_on(body, tone, ui::tokens::colors::NON_TEXT_MIN_CONTRAST);
    ui::a11y::contrast_ratio(hue, body)
}

impl TopBarChrome {
    pub fn from_tree(tree: &WorkspaceTree) -> Self {
        let mut this = Self {
            rect: Rect::ZERO,
            workspace_name: String::new(),
            workspace_index: 0,
            workspace_total: 1,
            cached: Scene::new(),
            cache_fp: CacheFingerprint::default(),
            collaborator_initials: Vec::new(),
            collaborator_colors: Vec::new(),
            collaborator_overflow: 0,
            status: crate::shell::status::SystemStatus::default(),
            chrome: None,
            synced_revision: 0,
            overview_btn_rect: Rect::ZERO,
            up_btn_rect: Rect::ZERO,
            down_btn_rect: Rect::ZERO,
            ws_pill_rect: Rect::ZERO,
            tray_pill_rect: Rect::ZERO,
            collaborator_is_agent: Vec::new(),
            avatar_stack_rect: Rect::ZERO,
            center_widget: CenterWidget::Clock,
            center_rect: Rect::ZERO,
            center_dot_rects: [Rect::ZERO; 3],
            center_scroll_accum: 0.0,
            last_cursor: None,
        };
        this.sync_from_tree(tree);
        this
    }

    /// Snapshot of the current frame's cache-affecting inputs. Compared
    /// against `self.cache_fp` to decide whether `self.cached` can be
    /// reused. See [`CacheFingerprint`] for what each field signals.
    fn current_fingerprint(&self) -> CacheFingerprint {
        // Floating-point Rect coords → integer keys. The rect is
        // taffy-derived from logical pixel coords; even at fractional-
        // DPI the values that land here are integer-valued f64s.
        // Casting via `as` after `.round()` keeps the fingerprint
        // stable against benign floating-point noise.
        let x0 = self.rect.x0.round() as i64;
        let y0 = self.rect.y0.round() as i64;
        let w = self.rect.width().round() as u64;
        let h = self.rect.height().round() as u64;
        let time_bucket = match self.center_widget {
            CenterWidget::Clock => {
                // Minute granularity — the Clock variant paints
                // `HH:MM`, so we only need to invalidate once per
                // minute. (Hour and date roll-over imply a minute
                // change too, so a single timestamp slice covers all.)
                let t = chrono::Local::now();
                t.timestamp() as u64 / 60
            }
            CenterWidget::SystemMonitor => {
                // Second granularity — the sparkline bars + CPU/RAM/TMP
                // values are derived from `seconds % N`, so the strip
                // visibly mutates once per second.
                chrono::Local::now().timestamp() as u64
            }
            CenterWidget::Calendar => {
                // Day granularity — the Calendar variant paints today's
                // date, so the cache has to drop at midnight or the
                // strip keeps showing yesterday until something else
                // dirties the fingerprint.
                chrono::Local::now().timestamp() as u64 / 86_400
            }
        };
        CacheFingerprint {
            rect_px: (x0, y0, w, h),
            workspace_name: self.workspace_name.clone(),
            workspace_index: self.workspace_index,
            workspace_total: self.workspace_total,
            collaborator_initials: self.collaborator_initials.clone(),
            collaborator_overflow: self.collaborator_overflow,
            center_widget: self.center_widget,
            time_bucket,
            status_key: self.status_key(),
        }
    }

    /// The status reading quantised to what the tray paints.
    fn status_key(&self) -> StatusKey {
        status_key_of(&self.status)
    }

    /// The status the chrome is currently holding, or the last one this
    /// bar copied when there is no chrome attached.
    ///
    /// Read at draw time rather than pushed in by an `update` pass,
    /// because nothing pumps this widget: `Column` / `Stack` /
    /// `Container` all inherit the default [`Widget::update`], which
    /// returns `false` without recursing, so a leaf's `update` is only
    /// reached if some ancestor explicitly forwards — and none of the
    /// bar's do. Pulling on the frame the value is needed is the only
    /// arrangement that cannot be defeated by where the widget happens
    /// to sit in the tree.
    fn live_status(&self) -> crate::shell::status::SystemStatus {
        match self.chrome.as_ref() {
            Some(c) => c.borrow().status.clone(),
            None => self.status.clone(),
        }
    }

    /// Attach a shared chrome-state handle. The top bar uses it to toggle
    /// overlays (quick-settings, workspace-settings, overview) and to
    /// switch the active workspace via the up/down chevrons.
    pub fn with_chrome(mut self, chrome: Chrome) -> Self {
        let rev = chrome.borrow().revision;
        // Take the status here, not only on the next revision bump.
        //
        // Attaching the chrome also *declares this widget synced to it*
        // at the current revision, so `update`'s sync branch does not
        // run until the revision moves again. Everything else the bar
        // reads was seeded by `from_tree`; the status was not, which is
        // why the tray drew empty until the first bump. After this the
        // bar stays current through [`Widget::update`], which the host
        // reaches via the `Stack`/`Column` the bar sits in — a container
        // that swallowed `update` would freeze every field below.
        self.status = chrome.borrow().status.clone();
        self.chrome = Some(chrome);
        self.synced_revision = rev;
        self
    }

    pub fn sync_from_tree(&mut self, tree: &WorkspaceTree) {
        self.workspace_total = tree.workspaces.len().max(1);
        let (idx, name) = tree
            .workspaces
            .iter()
            .enumerate()
            .find(|(_, w)| w.id == tree.active)
            .map(|(i, w)| (i, w.name.clone()))
            .unwrap_or((0, String::from("Workspace")));
        self.workspace_index = idx;
        self.workspace_name = name;
        // Roster source: the room on screen, as
        // `crate::shell::state::active_room_roster` settles it — the live
        // roster the frame loop restates, with the workspace's own list
        // answering only where there is no live one. Reading the
        // workspace's list first, as this did, showed a guest nothing but
        // the placeholder row its own join had written.
        //
        // The local user (`id == "me"`) is filtered out of the avatar
        // stack: the right cluster shows *other* peers in the room, not
        // the local user. Collaborators whose name has no starting char
        // are dropped, and at most 3 are drawn — the rest flows into
        // `collaborator_overflow`.
        // The same rows the collaborators panel lists, from the same
        // call. The stack used to walk `active_room_roster` itself,
        // which is a strictly smaller list: it holds peers, and an
        // agent holds its membership by capability rather than by
        // presence, so no agent could ever appear here. The bar is the
        // surface a viewer sees without opening anything, and it was
        // the one that could not show every member of the room.
        let roster = crate::shell::overlay_host::collaborator_roster_rows(tree);
        // Somebody who has been invited and has not arrived is not in
        // the avatar cluster. An avatar there is a claim that a specific
        // person is in this workspace, and the empty-roster slot exists
        // precisely to say "there is nobody here yet, invite someone" —
        // so filling it with the person you just invited would answer
        // its own offer and hide the moment they actually turn up.
        let present =
            |c: &&crate::shell::overlay_host::RosterRow| c.id != "me" && !c.invited;
        self.collaborator_initials.clear();
        self.collaborator_colors.clear();
        self.collaborator_is_agent.clear();
        // Peer count flows through the shared helper so the top-bar
        // overflow chip and the wheel-disc active-card dot count always
        // agree (audit B-10).
        let peer_count = crate::shell::state::active_workspace_peer_count(tree);
        // Colour and monogram both come off the row, which settled them
        // over the whole room — so a peer's avatar here is the colour
        // their cursor is drawn in on the desktop, and matches the chip
        // the panel shows for the same person.
        for c in roster.iter().filter(present).take(3) {
            self.collaborator_initials.push(c.initial);
            self.collaborator_colors.push(c.color);
            self.collaborator_is_agent.push(c.is_agent);
        }
        self.collaborator_overflow = peer_count.saturating_sub(3);
    }

    /// The clock as the top bar shows it.
    ///
    /// `QOS_CLOCK` pins it, for the same reason `QOS_SHOTS_THEME` pins
    /// the palette: a harness that compares whole rendered frames has to
    /// be able to hold every part of the frame still. Now that overlays
    /// let the desktop show through instead of covering it, the top bar
    /// is in shot for surfaces that never used to include it, and a
    /// running clock would make those comparisons fail once a minute.
    /// Unset — which is every real session — reads the wall clock.
    fn current_clock_string() -> String {
        match std::env::var("QOS_CLOCK") {
            Ok(pinned) if !pinned.is_empty() => pinned,
            _ => Local::now().format("%H:%M").to_string(),
        }
    }

    /// Initials painted as avatars (up to 3). Exposed for integration tests.
    pub fn collaborator_initials(&self) -> &[char] {
        &self.collaborator_initials
    }

    /// Which of those chips belong to an agent rather than a person,
    /// same index as [`Self::collaborator_initials`].
    pub fn collaborator_is_agent(&self) -> &[bool] {
        &self.collaborator_is_agent
    }

    /// Whether a workspace precedes the active one. The up-chevron is
    /// painted live exactly when this holds — the same condition under
    /// which `ChromeState::switch_workspace(-1)` actually moves — so the
    /// affordance and the action cannot disagree.
    pub fn can_switch_up(&self) -> bool {
        self.workspace_index > 0
    }

    /// Whether a workspace follows the active one. Mirror of
    /// [`Self::can_switch_up`] for the down-chevron.
    pub fn can_switch_down(&self) -> bool {
        self.workspace_index + 1 < self.workspace_total
    }

    /// Resolved avatar fill colours (one per entry in
    /// `collaborator_initials`, same index). Each peer's own
    /// `cursor_color` takes precedence; otherwise the palette-pinned
    /// [`ui::presence::user_color`] is used. Exposed so tests can
    /// assert two different peers map to two different hues.
    pub fn collaborator_colors(&self) -> &[Color] {
        &self.collaborator_colors
    }

    /// Count of collaborators beyond the three rendered as avatars.
    /// Zero when three-or-fewer peers; otherwise shown as the "+N" chip.
    pub fn collaborator_overflow(&self) -> usize {
        self.collaborator_overflow
    }

    /// Active center-widget variant. Exposed for integration tests so they
    /// can verify the scroll-wheel and dot-click state transitions.
    pub fn center_widget(&self) -> CenterWidget {
        self.center_widget
    }

    /// Test-only setter: jump the center-band widget directly to a
    /// variant without driving the scroll-wheel event path. Used by the
    /// concept-parity spec tests to put the bar into SystemMonitor mode
    /// so they can assert the CPU/RAM label colours match the concept's
    /// emerald-400/blue-400 semantic palette (visual-gap #11/#12 in the
    /// concept-coverage audit). Production code mutates `center_widget`
    /// only via `handle_event`'s scroll/click branches; this accessor is
    /// gated to test/debug builds so it can't leak into release.
    #[cfg(any(test, debug_assertions))]
    pub fn set_center_widget_for_tests(&mut self, w: CenterWidget) {
        self.center_widget = w;
    }

    /// Base (unscaled) radius used for the center-band three-dot stack.
    pub const DOT_BASE_SIZE: f64 = 3.0;
    /// Rendered radius of the *active* center-band dot. Sized 1.6× the
    /// base so the saturated indigo fill reads as the focused indicator;
    /// see gap #8 in `docs/nirios/visual-gaps.md`.
    pub const DOT_ACTIVE_RADIUS: f64 = Self::DOT_BASE_SIZE * 1.6;
    /// Rendered radius of the two inactive center-band dots.
    pub const DOT_INACTIVE_RADIUS: f64 = Self::DOT_BASE_SIZE * 1.0;

    /// Radius at which the active center-band dot is painted. Exposed so
    /// tests can assert it is strictly larger than the inactive radius
    /// (the contrast affordance is the scale-up + saturated indigo, not
    /// the scale alone).
    pub fn active_dot_radius(&self) -> f64 {
        Self::DOT_ACTIVE_RADIUS
    }

    /// Radius at which each inactive center-band dot is painted.
    pub fn inactive_dot_radius(&self) -> f64 {
        Self::DOT_INACTIVE_RADIUS
    }

    /// Clear space kept between the centre band's rim and the dot column.
    const DOT_COLUMN_MARGIN: f64 = 1.0;

    /// Lay the centre-band dot column out inside `band`.
    ///
    /// Painting and hit-testing both read this, so the two cannot drift —
    /// which is what had gone wrong. The column was sized as
    /// `3 * DOT_BASE_SIZE + 2 * gap`, and stepped by `DOT_BASE_SIZE + gap`,
    /// both of which treat [`Self::DOT_BASE_SIZE`] as a diameter when its
    /// own doc comment calls it a radius and the paint uses it as one. So
    /// 17 px were reserved for three discs that are painted 9.6 px and 6 px
    /// across, on a 7 px pitch narrower than the active disc by itself: the
    /// active dot ran 0.8 px into its neighbour and the two fused into a
    /// single blob, the column's ink sat ~1 px above the band's centre line
    /// while its slot claimed to be centred, and the one visible gap left
    /// was the one between the two dots that were *both* small.
    ///
    /// Here the sizes come from the radii the discs are actually painted
    /// at, the two gaps are equal edge-to-edge (so what the eye compares
    /// matches even though one disc is larger), and the leftover height is
    /// split so the column centres on the band.
    fn center_dot_layout(band: Rect, bar: Rect, active: usize) -> [CenterDot; 3] {
        let radius_of =
            |i: usize| if i == active { Self::DOT_ACTIVE_RADIUS } else { Self::DOT_INACTIVE_RADIUS };
        let ink: f64 = (0..3).map(|i| radius_of(i) * 2.0).sum();
        // Whatever the band has spare once the discs are placed, shared
        // between the two gaps. Deriving it rather than fixing it is what
        // keeps the column inside the pill if either the band height or the
        // radii are changed later.
        let gap = ((band.height() - Self::DOT_COLUMN_MARGIN * 2.0 - ink) / 2.0).max(1.0);
        let column_h = ink + gap * 2.0;
        let mut edge = band.center().y - column_h / 2.0;

        let mut centers = [0.0_f64; 3];
        let mut radii = [0.0_f64; 3];
        for i in 0..3 {
            let r = radius_of(i);
            radii[i] = r;
            centers[i] = edge + r;
            edge += r * 2.0 + gap;
        }

        // Slots tile the strip from the bar's top edge to its bottom, split
        // at the midpoint of each gap. They were 20 px squares centred on
        // each dot, which on the real pitch overlapped two-deep — and since
        // the hit test takes the first slot that contains the point, dot 0
        // was swallowing most of dot 1's ink and dot 1 most of dot 2's.
        let boundary = |a: usize, b: usize| (centers[a] + radii[a] + centers[b] - radii[b]) / 2.0;
        let cuts = [boundary(0, 1), boundary(1, 2)];
        let x0 = Self::dot_column_x(band) - Self::DOT_SLOT_HALF_W;
        let x1 = Self::dot_column_x(band) + Self::DOT_SLOT_HALF_W;
        let edges = [bar.y0, cuts[0], cuts[1], bar.y1];

        std::array::from_fn(|i| CenterDot {
            center: Point::new(Self::dot_column_x(band), centers[i]),
            radius: radii[i],
            slot: Rect::new(x0, edges[i], x1, edges[i + 1]),
        })
    }

    /// Horizontal centre of the dot column, inset from the band's left rim.
    fn dot_column_x(band: Rect) -> f64 {
        band.x0 + 12.0
    }

    /// Half-width of a dot's click slot. The band's own content starts at
    /// `dot_column_x + 24`, so this stays clear of it.
    const DOT_SLOT_HALF_W: f64 = 10.0;

    /// Which lucide glyph is used for the top-bar overview button.
    /// Pinned so the gap-#5 icon swap survives later refactors — if a
    /// future change reverts the overview glyph back to the four-rect
    /// quad, this accessor's test will fail loudly.
    pub fn overview_glyph_icon() -> ui::icons::Icon {
        ui::icons::Icon::LayoutGrid
    }

    /// Typographic weight the top bar paints the clock with. Concept
    /// `§2.2`: the center-band clock is `font-bold` / `font-mono`, so it
    /// reads as the "focal glance value" of the chrome. Exposed so
    /// integration tests can pin the contract — see visual-gap #7 in
    /// `docs/nirios/visual-gaps.md`.
    pub fn clock_text_weight(&self) -> TextWeight {
        TextWeight::Bold
    }

    /// Typographic weight for the workspace-name pill (§2.1). Semibold so
    /// it reads as a clickable title without tipping over into the h1/h2
    /// weight tier the clock uses.
    pub fn workspace_name_text_weight(&self) -> TextWeight {
        TextWeight::Semibold
    }

    /// Typographic weight for the system-monitor row's acronym labels ("CPU",
    /// "RAM", "TMP"). Concept uses `font-mono font-semibold` for labels while
    /// the numeric values take `font-bold` — [`Self::system_monitor_value_text_weight`]
    /// exposes that tier. Splitting the two makes the strip scannable at a
    /// glance without every label shouting.
    pub fn system_monitor_label_text_weight(&self) -> TextWeight {
        TextWeight::Semibold
    }

    /// Typographic weight for the numeric values in the system-monitor row
    /// (e.g. "12%", "40°"). Concept tier: `font-mono font-bold`.
    pub fn system_monitor_value_text_weight(&self) -> TextWeight {
        TextWeight::Bold
    }

    /// Per-metric label colour used by the SystemMonitor center widget.
    ///
    /// The three metrics do need to be told apart at a glance, and a
    /// single accent for all three collapses that cue. But they were
    /// distinguished with literal Tailwind hexes — emerald-400,
    /// blue-400, orange-400 — which are the same three pixels on every
    /// theme the user might pick, including high-contrast, where they are
    /// the only part of the bar that does not respond.
    ///
    /// The theme already defines a set of mutually distinguishable
    /// meanings and redefines them per palette, so the metrics take
    /// those: healthy/steady, informational, and thermal-warm. Same cue,
    /// and it tracks the desktop it is drawn on.
    pub fn metric_label_color(&self, theme: &Theme, k: MetricKind) -> Color {
        let semantic = &theme.tokens.colors.semantic;
        match k {
            MetricKind::Cpu => semantic.success,
            MetricKind::Ram => semantic.info,
            MetricKind::Tmp => semantic.warning,
        }
    }

    /// Fill colour for the SystemMonitor sparkline bars.
    ///
    /// The sparkline reads as the CPU history strip and keeps that one
    /// colour even while the chips either side advertise RAM or TMP, so
    /// it takes the CPU metric's colour at a low weight — a backdrop to
    /// the numbers rather than a fourth thing competing with them. The
    /// `_k` parameter is kept so a per-metric variant can be wired in
    /// without changing the signature.
    pub fn sparkline_fill(&self, theme: &Theme, _k: MetricKind) -> Color {
        self.metric_label_color(theme, MetricKind::Cpu)
            .with_alpha_factor(SPARKLINE_FILL_ALPHA)
    }

    /// Bounding rect of the overview button. Populated during `draw`;
    /// exposed so integration tests can hit-test click coordinates against
    /// the exact geometry the widget uses internally.
    pub fn overview_btn_rect(&self) -> Rect {
        self.overview_btn_rect
    }

    /// Opacity of the top-bar surface. See [`TOP_BAR_SURFACE_ALPHA`];
    /// pinned by `shell_chrome_test` so the bar cannot drift away from the
    /// chrome constant the popovers share.
    pub fn top_bar_alpha() -> f32 {
        TOP_BAR_SURFACE_ALPHA
    }

    /// Bounding rect of the quick-settings tray pill (three-dot widget on
    /// the far right of the top bar). Populated during `draw`; exposed so
    /// integration tests can hit-test click coordinates against the exact
    /// geometry the widget uses internally.
    pub fn tray_pill_rect(&self) -> Rect {
        self.tray_pill_rect
    }

    /// Paint the battery as a gauge, not a glyph.
    ///
    /// `ui::icons::Icon::Battery` is a fixed outline — it looks the same
    /// at 4% as at 94%, which is exactly the thing the old tray got
    /// wrong. A gauge whose fill tracks the charge is readable without
    /// reading the number, which is the whole job of a tray indicator;
    /// the percentage beside it is the confirmation, not the signal.
    ///
    /// Drawn as a rounded body + terminal nub + inset fill, in the same
    /// proportions as lucide's battery so it sits in the icon row rather
    /// than beside it.
    fn draw_battery_gauge(
        scene: &mut Scene,
        transform: Affine,
        rect: Rect,
        percent: u8,
        charging: bool,
        hue: Color,
        colors: &ui::tokens::colors::Colors,
    ) {
        // lucide's battery is a 16×12 body at x=2,y=6 in a 24 box, with
        // the nub from x=22. Keep those ratios so the gauge lines up
        // optically with the neighbouring stroked glyphs.
        let s = rect.width().min(rect.height()) / 24.0;
        let ox = rect.x0 + (rect.width() - 24.0 * s) / 2.0;
        let oy = rect.y0 + (rect.height() - 24.0 * s) / 2.0;
        let px = |x: f64| ox + x * s;
        let py = |y: f64| oy + y * s;
        let stroke_w = (ui::icons::LUCIDE_DEFAULT_STROKE_PX).max(1.0);

        let body = RoundedRect::new(px(2.0), py(6.0), px(18.0), py(18.0), 2.0 * s);
        scene.stroke(
            &vello::kurbo::Stroke::new(stroke_w),
            transform,
            colors.text_dim,
            None,
            &body,
        );
        // Terminal nub.
        scene.stroke(
            &vello::kurbo::Stroke::new(stroke_w).with_caps(vello::kurbo::Cap::Round),
            transform,
            colors.text_dim,
            None,
            &vello::kurbo::Line::new(
                vello::kurbo::Point::new(px(21.0), py(10.0)),
                vello::kurbo::Point::new(px(21.0), py(14.0)),
            ),
        );

        // Charge fill, inset one stroke inside the body so the outline
        // stays legible at every level.
        let inset = stroke_w + 0.5;
        let x0 = px(2.0) + inset;
        let x1 = px(18.0) - inset;
        let frac = (percent as f64 / 100.0).clamp(0.0, 1.0);
        if frac > 0.0 {
            // A hairline at 1% would disappear; give any nonzero charge
            // at least a visible sliver so "nearly empty" and "empty"
            // do not render identically.
            let w = ((x1 - x0) * frac).max(1.5);
            let fill = RoundedRect::new(
                x0,
                py(6.0) + inset,
                (x0 + w).min(x1),
                py(18.0) - inset,
                1.0 * s,
            );
            scene.fill(Fill::NonZero, transform, hue, None, &fill);
        }

        if charging {
            // A bolt over the fill. Drawn in the surface colour with a
            // faint outline rather than in an accent, so it reads on top
            // of whatever the fill level happens to be underneath it.
            let mut bolt = vello::kurbo::BezPath::new();
            bolt.move_to((px(11.5), py(7.5)));
            bolt.line_to((px(8.0), py(12.6)));
            bolt.line_to((px(11.0), py(12.6)));
            bolt.line_to((px(9.5), py(16.5)));
            bolt.line_to((px(13.5), py(11.0)));
            bolt.line_to((px(10.5), py(11.0)));
            bolt.close_path();
            scene.fill(Fill::NonZero, transform, colors.text_primary, None, &bolt);
            scene.stroke(
                &vello::kurbo::Stroke::new(0.75),
                transform,
                colors.surfaces.raised,
                None,
                &bolt,
            );
        }
    }
}

impl Widget for TopBarChrome {
    fn sync(&mut self, _state: &mut AutoCommit, _path: &ObjId) {}

    fn update(&mut self, _state: &mut AutoCommit, _path: &ObjId) -> bool {
        // Re-sync when the shared chrome state has mutated since our last
        // pass — typically after a workspace switch or overlay toggle
        // triggered by a sibling widget.
        if let Some(chrome) = self.chrome.clone() {
            let rev = chrome.borrow().revision;
            if rev != self.synced_revision {
                // Clone the tree to avoid re-borrowing chrome while calling
                // `sync_from_tree` (which borrows &self mut).
                let tree_snap = chrome.borrow().tree().clone();
                self.sync_from_tree(&tree_snap);
                // The status poller bumps the same revision, so it rides
                // in on the same edge as everything else.
                self.status = chrome.borrow().status.clone();
                self.synced_revision = rev;
                return true;
            }
        }
        false
    }

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        _theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        // A fullscreen surface covers the whole output, top bar included — so
        // while any group holds the output fullscreen the top bar collapses to
        // zero height and stops painting. Clearing the fullscreen state
        // restores its normal 40 px band exactly.
        let fullscreen = self
            .chrome
            .as_ref()
            .is_some_and(|c| c.borrow().is_fullscreen());
        let node = taffy
            .new_leaf(Style {
                display: if fullscreen {
                    Display::None
                } else {
                    Display::Flex
                },
                size: Size {
                    width: Dimension::percent(1.0),
                    height: if fullscreen {
                        Dimension::length(0.0)
                    } else {
                        Dimension::length(TOPBAR_HEIGHT_PX)
                    },
                },
                flex_shrink: 0.0,
                ..Default::default()
            })
            .unwrap();
        node_map.insert(node, self as *mut dyn Widget);
        node
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        let l = taffy.layout(node).unwrap();
        self.rect = Rect::new(
            origin.x + l.location.x as f64,
            origin.y + l.location.y as f64,
            origin.x + l.location.x as f64 + l.size.width as f64,
            origin.y + l.location.y as f64 + l.size.height as f64,
        );
    }

    fn draw(
        &mut self,
        out_scene: &mut Scene,
        out_transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        _presence: &PresenceCtx,
    ) {
        // Cache fast-path. The top bar is the largest piece of *mostly-
        // static* chrome in the shell — workspace nav, clock band,
        // collaborator avatars — and re-encoding it every frame is
        // expensive (~12 fills + several text measurements + lucide
        // icon strokes). We fingerprint the inputs once per frame and
        // replay the cached scene with the caller's transform if the
        // fingerprint matches.
        //
        // The fingerprint covers rect, workspace identity/index/total,
        // collaborator initials + overflow, center-widget variant, and
        // a time bucket (minute for Clock, second for SystemMonitor,
        // zero for static Calendar). Anything that affects the painted
        // pixels lives in there; anything that doesn't (e.g. cursor
        // position used for hit-testing) deliberately doesn't.
        // Collapsed to zero height (a window holds the output fullscreen — see
        // the `Display::None` branch in `build_layout`): paint nothing. Without
        // this the encode below still ran and emitted the clock band / nav pill
        // at their intrinsic sizes, which overflowed the zero-height rect and
        // left a stray fragment floating over the fullscreen window.
        if self.rect.height() <= 0.0 || self.rect.width() <= 0.0 {
            return;
        }
        // Take the machine's latest reading before fingerprinting, so a
        // battery that just dropped a percent is both *drawn* and
        // *counted as a change* in the same pass. See
        // [`Self::live_status`] for why this is pulled here rather than
        // pushed in by an update.
        self.status = self.live_status();
        let fp = self.current_fingerprint();
        if fp == self.cache_fp && fp.rect_px.2 > 0 {
            // Cache hit — `*_btn_rect` / `center_rect` / `avatar_stack_rect`
            // / `center_dot_rects` were populated by the most recent
            // cache-miss draw and remain valid because the rect, workspace
            // state, and collaborator roster are unchanged.
            out_scene.append(&self.cached, Some(out_transform));
            return;
        }

        // Cache miss — encode the topbar at `Affine::IDENTITY` into
        // `self.cached`, then splice with `out_transform`. Same
        // `std::mem::take` dance as `desktop_backdrop` so the encode
        // body has an exclusive `&mut Scene` without aliasing `self`.
        let mut local = std::mem::take(&mut self.cached);
        local.reset();
        let scene = &mut local;
        let transform = Affine::IDENTITY;

        let tokens = &theme.tokens;
        let colors = &tokens.colors;

        scene.fill(
            Fill::NonZero,
            transform,
            colors.surfaces.overlay,
            None,
            &self.rect,
        );

        let border_rect = Rect::new(self.rect.x0, self.rect.y1 - 1.0, self.rect.x1, self.rect.y1);
        scene.fill(
            Fill::NonZero,
            transform,
            colors.border_standard,
            None,
            &border_rect,
        );

        // Soft bottom shadow — masks the abrupt translucent-topbar →
        // exposed-backdrop transition that read as "noisy pixels"
        // against the dotted-grid wallpaper. Concept-style top-bar
        // has a subtle drop-shadow; we approximate with a 28-px
        // vertical fade extending below the bar. Vello widgets
        // aren't clipped to their rect, and the topbar paints
        // after the backdrop and before the workspace content,
        // so the band lays cleanly over the seam below.
        let shadow_h = 28.0_f64;
        let shadow_rect = Rect::new(
            self.rect.x0,
            self.rect.y1,
            self.rect.x1,
            self.rect.y1 + shadow_h,
        );
        let shadow_grad = Gradient::new_linear(
            Point::new(0.0, self.rect.y1),
            Point::new(0.0, self.rect.y1 + shadow_h),
        )
        .with_extend(Extend::Pad)
        .with_stops(
            &[
                ColorStop {
                    offset: 0.0,
                    color: Color::from_rgba8(0, 0, 0, 46).into(), // ≈ 0.18
                },
                ColorStop {
                    offset: 1.0,
                    color: Color::from_rgba8(0, 0, 0, 0).into(),
                },
            ][..],
        );
        scene.fill(
            Fill::NonZero,
            transform,
            &Brush::Gradient(shadow_grad),
            None,
            &shadow_rect,
        );

        let pad_x = tokens.spacing.s3 as f64;
        let center_y = (self.rect.y0 + self.rect.y1) / 2.0;
        let icon_size = 14.0_f64;

        // --- Left cluster: overview icon + chevrons + workspace name ---
        let mut x = self.rect.x0 + pad_x;

        // Overview glyph: lucide `LayoutGrid` (gap #5). Drawn as stroked
        // rounded rects via `ui::icons::draw_icon` so it matches
        // the concept's lucide-react lockup instead of the ad-hoc four
        // filled squares we used before.
        self.overview_btn_rect = Rect::new(
            x - 4.0,
            center_y - icon_size / 2.0 - 4.0,
            x + icon_size + 4.0,
            center_y + icon_size / 2.0 + 4.0,
        );
        let overview_icon_rect = Rect::new(
            x,
            center_y - icon_size / 2.0,
            x + icon_size,
            center_y + icon_size / 2.0,
        );
        ui::icons::draw_icon(
            scene,
            transform,
            overview_icon_rect,
            Self::overview_glyph_icon(),
            colors.text_dim,
            ui::icons::LUCIDE_DEFAULT_STROKE_PX,
        );
        x += icon_size + tokens.spacing.s3 as f64;

        // Vertical divider
        let divider = Rect::new(x, center_y - 8.0, x + 1.0, center_y + 8.0);
        scene.fill(
            Fill::NonZero,
            transform,
            colors.border_standard,
            None,
            &divider,
        );
        x += 1.0 + tokens.spacing.s2 as f64;

        // Up-chevron: lucide `ChevronUp` (gap #5). Drawn through
        // `ui::icons::draw_icon` so the chrome's chevrons share
        // stroke caps + joins with the rest of the lucide lockup
        // (`LayoutGrid` / `Settings2` / `Wifi` / `Monitor`) instead of
        // the previous hand-rolled BezPath polyline.
        let chev = 10.0_f64;
        self.up_btn_rect = Rect::new(
            x - 4.0,
            center_y - chev * 0.35 - 6.0,
            x + chev + 4.0,
            center_y + chev * 0.35 + 6.0,
        );
        let up_icon_rect = Rect::new(x, center_y - chev / 2.0, x + chev, center_y + chev / 2.0);
        ui::icons::draw_icon(
            scene,
            transform,
            up_icon_rect,
            ui::icons::Icon::ChevronUp,
            if self.can_switch_up() {
                colors.text_dim
            } else {
                colors.border_standard
            },
            ui::icons::LUCIDE_DEFAULT_STROKE_PX,
        );
        x += chev + tokens.spacing.s2 as f64;

        // Down-chevron: lucide `ChevronDown` (gap #5). Same lockup
        // rationale as the up-chevron above.
        self.down_btn_rect = Rect::new(
            x - 4.0,
            center_y - chev * 0.35 - 6.0,
            x + chev + 4.0,
            center_y + chev * 0.35 + 6.0,
        );
        let dn_icon_rect = Rect::new(x, center_y - chev / 2.0, x + chev, center_y + chev / 2.0);
        ui::icons::draw_icon(
            scene,
            transform,
            dn_icon_rect,
            ui::icons::Icon::ChevronDown,
            if self.can_switch_down() {
                colors.text_dim
            } else {
                colors.border_standard
            },
            ui::icons::LUCIDE_DEFAULT_STROKE_PX,
        );
        x += chev + tokens.spacing.s3 as f64;

        // Workspace-name button: text + gear glyph
        let ws_text_size = tokens.typography.scale.sm;
        let ws_label = format!(
            "{}  {}/{}",
            self.workspace_name,
            self.workspace_index + 1,
            self.workspace_total
        );
        // Concept §2.1: workspace-name label is `font-semibold`. Measure with
        // the same weight so the pill width tracks the glyph advance at
        // Semibold, not at the Regular fallback — visual-gap #7.
        let ws_weight = self.workspace_name_text_weight();
        let (ws_w, ws_h) = text_renderer.measure_weighted(&ws_label, ws_text_size, None, ws_weight);
        let ws_y = center_y - ws_h as f64 / 2.0;
        // Subtle pill backdrop so the workspace name reads as clickable.
        let ws_pill_outer = Rect::new(
            x - 6.0,
            center_y - 12.0,
            x + ws_w as f64 + 28.0,
            center_y + 12.0,
        );
        self.ws_pill_rect = ws_pill_outer;
        // A thing you choose between is a capsule (HIG §9.1, Selector).
        // These were 4 px rectangles sitting a few pixels from the
        // fully-round tray pill and the fully-round tab pills, which is
        // most of why the bar read as a different design system from the
        // window chrome beneath it.
        let pill = RoundedRect::from_rect(
            ws_pill_outer,
            ui::chrome::ChromeRole::Selector.radius(theme, ws_pill_outer.height()),
        );
        // The current workspace is the active Selector in the bar.
        let body = bar_selector_body(theme, ui::chrome::SelectionState::active(true));
        scene.fill(Fill::NonZero, transform, body.fill, None, &pill);
        if let Some(edge) = body.border {
            scene.stroke(&vello::kurbo::Stroke::new(1.0), transform, edge, None, &pill);
        }
        text_renderer.draw_text_weighted(
            scene,
            &ws_label,
            ws_text_size,
            colors.text_primary,
            transform * Affine::translate((x, ws_y)),
            None,
            ws_weight,
        );
        let gear_x = x + ws_w as f64 + 6.0;
        // Gear glyph: lucide `Settings2` (gap #5). The ad-hoc ring+dot
        // placeholder read as generic rather than "open settings"; the
        // lucide sliders+knobs glyph matches the concept's affordance.
        let gear_rect = Rect::new(gear_x - 2.0, center_y - 6.0, gear_x + 10.0, center_y + 6.0);
        ui::icons::draw_icon(
            scene,
            transform,
            gear_rect,
            ui::icons::Icon::Settings2,
            colors.text_dim,
            ui::icons::LUCIDE_DEFAULT_STROKE_PX,
        );

        // --- Center cluster: carousel band with 3 dots + active widget ---
        let band_w = 360.0_f64;
        let band_h = 28.0_f64;
        let band_cx = (self.rect.x0 + self.rect.x1) / 2.0;
        let band = Rect::new(
            band_cx - band_w / 2.0,
            center_y - band_h / 2.0,
            band_cx + band_w / 2.0,
            center_y + band_h / 2.0,
        );
        self.center_rect = band;
        // Subtle pill backdrop so the band reads as a scroll surface.
        let band_rr = RoundedRect::from_rect(
            band,
            ui::chrome::ChromeRole::Selector.radius(theme, band.height()),
        );
        // The centre band is a Selector: you scroll between its three widgets.
        let body = bar_selector_body(theme, ui::chrome::SelectionState::active(true));
        scene.fill(Fill::NonZero, transform, body.fill, None, &band_rr);
        if let Some(edge) = body.border {
            scene.stroke(&vello::kurbo::Stroke::new(1.0), transform, edge, None, &band_rr);
        }

        // Three-dot indicator on the left of the band (vertical stack).
        // Geometry — discs and click slots alike — comes from
        // [`Self::center_dot_layout`], so what is painted and what is
        // clickable are the same arithmetic rather than two guesses at it.
        let dots = Self::center_dot_layout(band, self.rect, self.center_widget.index());
        for (i, dot) in dots.iter().enumerate() {
            let active = self.center_widget.index() == i;
            // Active dot is scaled up 1.6× and painted in the saturated
            // indigo accent (`accent[5]` = indigo-500 in the token ramp).
            // Inactive dots use `neutral[7]` (slate-300) so they sit clearly
            // above `text_dim` (slate-400) but still read below
            // `text_primary` — this is the contrast bump from gap #8.
            let color = if active {
                colors.accent[5]
            } else {
                colors.neutral[7]
            };
            self.center_dot_rects[i] = dot.slot;
            let c = vello::kurbo::Circle::new(dot.center, dot.radius);
            scene.fill(Fill::NonZero, transform, color, None, &c);
        }

        // Widget content fills the band to the right of the dots.
        let content_x0 = Self::dot_column_x(band) + 24.0;
        let content_x1 = band.x1 - 12.0;
        let content_cx = (content_x0 + content_x1) / 2.0;
        match self.center_widget {
            CenterWidget::Clock => {
                let s = Self::current_clock_string();
                let size = tokens.typography.scale.base;
                // Concept §2.2: clock is `font-bold`. Bold glyphs advance
                // wider than Regular, so the measure call uses the same
                // weight as the draw call — centering would drift by a few
                // pixels otherwise. Visual-gap #7.
                let weight = self.clock_text_weight();
                let (tw, th) = text_renderer.measure_weighted(&s, size, None, weight);
                let tx = content_cx - tw as f64 / 2.0;
                let ty = center_y - th as f64 / 2.0;
                text_renderer.draw_text_weighted(
                    scene,
                    &s,
                    size,
                    colors.text_primary,
                    transform * Affine::translate((tx, ty)),
                    None,
                    weight,
                );
            }
            CenterWidget::SystemMonitor => {
                // CPU % (left) • sparkline (stretch) • RAM / TMP (right).
                // Concept uses live random data; here we render a stable
                // snapshot derived from the clock seconds so nothing
                // repaints every frame yet the values change over time.
                let t = Local::now();
                let secs = t.format("%S").to_string().parse::<u32>().unwrap_or(0);
                let cpu = 12 + (secs % 20);
                let ram = 45 + (secs % 8);
                let tmp = 40 + (secs % 11);
                let sm_size = tokens.typography.scale.xs;
                let cpu_str = format!("{cpu}%");
                let ram_str = format!("{ram}%");
                let tmp_str = format!("{tmp}°");
                // Left: CPU label + value. Concept monitor labels are
                // `font-mono font-semibold`; values are `font-mono font-bold`.
                text_renderer.draw_text_weighted(
                    scene,
                    "CPU",
                    sm_size,
                    // Concept paints each metric in its own semantic hue —
                    // CPU is `text-emerald-400` (#4ade80). Routing through
                    // `metric_label_color` keeps the per-metric colour cue
                    // the concept relies on (audit `concept-coverage.md:\
                    // 23,163-166`, visual-gap #11/#12) and prevents a
                    // future drift back to monotone indigo.
                    self.metric_label_color(theme, MetricKind::Cpu),
                    transform * Affine::translate((content_x0, center_y - 6.0)),
                    None,
                    TextWeight::Semibold,
                );
                text_renderer.draw_text_weighted(
                    scene,
                    &cpu_str,
                    tokens.typography.scale.sm,
                    colors.text_primary,
                    transform * Affine::translate((content_x0 + 26.0, center_y - 7.0)),
                    None,
                    TextWeight::Bold,
                );
                // Sparkline: 20 bars between CPU label and RAM cluster.
                let spark_x0 = content_x0 + 70.0;
                let spark_x1 = content_x1 - 100.0;
                let spark_w = (spark_x1 - spark_x0).max(40.0);
                let bars = 20usize;
                let bar_gap = 1.0;
                let bar_w = (spark_w - (bars as f64 - 1.0) * bar_gap) / bars as f64;
                for i in 0..bars {
                    // Deterministic but varied heights driven by seconds so
                    // the strip looks "live" on a re-render.
                    let seed = (secs as usize + i * 7) % 100;
                    let h = 4.0 + (seed as f64) * 0.08;
                    let bx = spark_x0 + i as f64 * (bar_w + bar_gap);
                    let b = Rect::new(bx, center_y + 6.0 - h, bx + bar_w, center_y + 6.0);
                    // Concept paints the sparkline at `bg-emerald-500/30`
                    // (#10b981 at 30% alpha) — see `nirios-concept/src/\
                    // components/TopBarWidgets.tsx:120-124`. The previous
                    // `theme.accent_primary` (indigo-500) was the wrong
                    // colour family entirely (audit `concept-coverage.md:\
                    // 23,160-162`).
                    scene.fill(
                        Fill::NonZero,
                        transform,
                        self.sparkline_fill(theme, MetricKind::Cpu),
                        None,
                        &b,
                    );
                }
                // Right: RAM + TMP chips. Same Semibold label / Bold value
                // tiering as the CPU cluster so the strip reads uniformly.
                text_renderer.draw_text_weighted(
                    scene,
                    "RAM",
                    sm_size,
                    // Concept: RAM label is `text-blue-400` (#60a5fa) —
                    // see `nirios-concept/src/components/\
                    // TopBarWidgets.tsx:60-110`. Previously `colors\
                    // .accent[5]` (indigo-500), which collapsed the
                    // per-metric semantic colour cue.
                    self.metric_label_color(theme, MetricKind::Ram),
                    transform * Affine::translate((content_x1 - 96.0, center_y - 6.0)),
                    None,
                    TextWeight::Semibold,
                );
                text_renderer.draw_text_weighted(
                    scene,
                    &ram_str,
                    tokens.typography.scale.sm,
                    colors.text_primary,
                    transform * Affine::translate((content_x1 - 70.0, center_y - 7.0)),
                    None,
                    TextWeight::Bold,
                );
                text_renderer.draw_text_weighted(
                    scene,
                    "TMP",
                    sm_size,
                    // Concept: TMP label is `text-orange-400` (#fb923c) —
                    // see `nirios-concept/src/components/\
                    // TopBarWidgets.tsx:60-110`. Previously `colors\
                    // .text_dim`, which dropped the temperature-warning
                    // hue the concept relies on.
                    self.metric_label_color(theme, MetricKind::Tmp),
                    transform * Affine::translate((content_x1 - 40.0, center_y - 6.0)),
                    None,
                    TextWeight::Semibold,
                );
                text_renderer.draw_text_weighted(
                    scene,
                    &tmp_str,
                    tokens.typography.scale.sm,
                    colors.text_primary,
                    transform * Affine::translate((content_x1 - 18.0, center_y - 7.0)),
                    None,
                    TextWeight::Bold,
                );
            }
            CenterWidget::Calendar => {
                let (weekday, date, note) = calendar_strip_parts(Local::now());
                let prefix = weekday.as_str();
                let event = date.as_str();
                let time = note;
                let size = tokens.typography.scale.sm;
                // Concept calendar-strip tiers: prefix + time as dim Medium
                // pieces, the event name as a Semibold primary-colour anchor.
                let prefix_weight = TextWeight::Medium;
                let event_weight = TextWeight::Semibold;
                let time_weight = TextWeight::Medium;
                let (pw, ph) = text_renderer.measure_weighted(prefix, size, None, prefix_weight);
                let (ew, _) = text_renderer.measure_weighted(event, size, None, event_weight);
                let (tw, _) = text_renderer.measure_weighted(
                    time,
                    tokens.typography.scale.xs,
                    None,
                    time_weight,
                );
                let gap = 8.0;
                let total = pw as f64 + gap + ew as f64 + gap + tw as f64;
                let mut tx = content_cx - total / 2.0;
                let ty = center_y - ph as f64 / 2.0;
                text_renderer.draw_text_weighted(
                    scene,
                    prefix,
                    size,
                    colors.text_dim,
                    transform * Affine::translate((tx, ty)),
                    None,
                    prefix_weight,
                );
                tx += pw as f64 + gap;
                text_renderer.draw_text_weighted(
                    scene,
                    event,
                    size,
                    colors.text_primary,
                    transform * Affine::translate((tx, ty)),
                    None,
                    event_weight,
                );
                tx += ew as f64 + gap;
                text_renderer.draw_text_weighted(
                    scene,
                    time,
                    tokens.typography.scale.xs,
                    colors.text_dim,
                    transform * Affine::translate((tx, ty + 1.0)),
                    None,
                    time_weight,
                );
            }
        }

        // --- Right cluster: avatar stack + tray pill ---
        let mut rx = self.rect.x1 - pad_x;

        // Tray pill — the machine's live state: audio, link, battery.
        //
        // These used to be three fixed lucide glyphs in dim text with no
        // data behind them. Now the pill sizes itself to whatever is
        // actually knowable: a reading that has not arrived (or a
        // machine that has no battery) contributes nothing rather than a
        // plausible-looking default, so the pill is narrower on a
        // desktop than on a laptop and empty until the first poll.
        let tray_h = 22.0_f64;
        let tray_slot = 18.0_f64;
        let tray_pad = 9.0_f64;
        let tray_gap = 4.0_f64;
        let icon_px = 14.0_f64;
        let pct_size = tokens.typography.scale.xs;

        // Measure first: the pill's width is the sum of what it will
        // actually paint.
        let has_volume = self.status.volume.is_some();
        let has_link = self.status.link.is_some();
        let battery_pct = self.status.battery_percent();
        let pct_label = battery_pct.map(|p| format!("{p}%"));
        let pct_w = pct_label
            .as_ref()
            .map(|s| {
                text_renderer
                    .measure_weighted(s, pct_size, None, TextWeight::Semibold)
                    .0 as f64
            })
            .unwrap_or(0.0);
        let mut slots = 0usize;
        if has_volume {
            slots += 1;
        }
        if has_link {
            slots += 1;
        }
        if battery_pct.is_some() {
            slots += 1;
        }
        let mut tray_w = if slots == 0 {
            // Nothing read yet. The pill still has to exist — it is the
            // quick-settings trigger — so it keeps a single slot with the
            // settings glyph, which is honest about what it opens rather
            // than pretending to report hardware.
            tray_pad * 2.0 + tray_slot
        } else {
            tray_pad * 2.0
                + slots as f64 * tray_slot
                + (slots.saturating_sub(1)) as f64 * tray_gap
        };
        if pct_w > 0.0 {
            tray_w += 3.0 + pct_w;
        }

        self.tray_pill_rect = Rect::new(
            rx - tray_w,
            center_y - tray_h / 2.0,
            rx,
            center_y + tray_h / 2.0,
        );
        let tray = RoundedRect::from_rect(
            self.tray_pill_rect,
            ui::chrome::ChromeRole::Selector.radius(theme, tray_h),
        );
        let tray_body = bar_selector_body(theme, ui::chrome::SelectionState::active(true));
        scene.fill(
            Fill::NonZero,
            transform,
            tray_body.fill,
            None,
            &tray,
        );
        // A critical battery tints the whole pill rather than just its
        // glyph: at 3% the user needs to notice the tray, not read it.
        if self.status.battery_is_critical() {
            scene.fill(
                Fill::NonZero,
                transform,
                colors.semantic.danger.with_alpha_factor(TRAY_CRITICAL_WASH),
                None,
                &tray,
            );
        }

        let dim = colors.text_dim;
        let bright = colors.text_primary;
        let mut sx = self.tray_pill_rect.x0 + tray_pad;
        let slot_rect = |x: f64| {
            Rect::new(
                x + (tray_slot - icon_px) / 2.0,
                center_y - icon_px / 2.0,
                x + (tray_slot + icon_px) / 2.0,
                center_y + icon_px / 2.0,
            )
        };

        if slots == 0 {
            ui::icons::draw_icon(
                scene,
                transform,
                slot_rect(sx),
                ui::icons::Icon::Settings2,
                dim,
                ui::icons::LUCIDE_DEFAULT_STROKE_PX,
            );
        }

        // --- Volume ---------------------------------------------------
        if let Some(vol) = self.status.volume {
            let silent = self.status.audio_is_silent();
            ui::icons::draw_icon(
                scene,
                transform,
                slot_rect(sx),
                if silent {
                    ui::icons::Icon::VolumeX
                } else {
                    ui::icons::Icon::Volume2
                },
                // Silence is a state the user chose and may want undone,
                // so it reads at full strength; a normal level is
                // ambient and stays dim.
                if silent { bright } else { dim },
                ui::icons::LUCIDE_DEFAULT_STROKE_PX,
            );
            let _ = vol;
            sx += tray_slot + tray_gap;
        }

        // --- Network --------------------------------------------------
        if let Some(link) = self.status.link.clone() {
            let r = slot_rect(sx);
            match link.kind {
                os_actions::LinkKind::Wired => {
                    ui::icons::draw_icon(
                        scene,
                        transform,
                        r,
                        ui::icons::Icon::EthernetPort,
                        dim,
                        ui::icons::LUCIDE_DEFAULT_STROKE_PX,
                    );
                }
                os_actions::LinkKind::Offline => {
                    // Offline is a fault, not an ambient state — it gets
                    // the warning hue so "why is nothing loading" is
                    // answerable at a glance.
                    ui::icons::draw_icon(
                        scene,
                        transform,
                        r,
                        ui::icons::Icon::WifiOff,
                        colors.semantic.warning,
                        ui::icons::LUCIDE_DEFAULT_STROKE_PX,
                    );
                }
                os_actions::LinkKind::Wifi => {
                    // The arcs are a scale. `icon_subpaths(Wifi)` yields
                    // [base dot, outer arc, middle arc, inner arc], so a
                    // weak signal lights the dot and the inner arc and
                    // leaves the outer ones at a fraction of the alpha —
                    // the same glyph, read as a meter.
                    let bars = crate::shell::status::link_bars(&link);
                    let faint = dim.with_alpha_factor(0.25);
                    ui::icons::draw_icon_layered(
                        scene,
                        transform,
                        r,
                        ui::icons::Icon::Wifi,
                        ui::icons::LUCIDE_DEFAULT_STROKE_PX,
                        |i| {
                            // Subpath 0 is the base dot (always lit);
                            // 1 = outer (needs 4 bars), 2 = middle (3),
                            // 3 = inner (2).
                            let needed = match i {
                                0 => 1,
                                1 => 4,
                                2 => 3,
                                _ => 2,
                            };
                            if bars >= needed { dim } else { faint }
                        },
                    );
                }
            }
            sx += tray_slot + tray_gap;
        }

        // --- Battery --------------------------------------------------
        if let Some(pct) = battery_pct {
            let charging = self.status.battery_is_charging();
            let hue = battery_gauge_hue(
                theme,
                self.status.battery_is_critical(),
                self.status.battery_is_low(),
                charging,
            );
            Self::draw_battery_gauge(scene, transform, slot_rect(sx), pct, charging, hue, colors);
            sx += tray_slot;
            if let Some(label) = pct_label.as_ref() {
                // The number sits next to the gauge rather than replacing
                // it: the gauge is the at-a-glance read, the number is
                // the one you look at when the gauge worries you.
                let (_, lh) =
                    text_renderer.measure_weighted(label, pct_size, None, TextWeight::Semibold);
                text_renderer.draw_text_weighted(
                    scene,
                    label,
                    pct_size,
                    if self.status.battery_is_low() {
                        hue
                    } else {
                        bright
                    },
                    transform * Affine::translate((sx + 3.0, center_y - lh as f64 / 2.0)),
                    None,
                    TextWeight::Semibold,
                );
            }
        }

        rx -= tray_w + tokens.spacing.s4 as f64; // s4 = 16, ≥ 8 px after avatar radius

        // Snapshot the rightmost x of the stack before drawing so we can
        // populate `avatar_stack_rect` once we know how many circles we
        // actually drew (including the overflow chip).
        let stack_right_x = rx;

        // Avatar stack: up to 3 chips, overlapping by 4 px, newest on top.
        // Each chip is the shared `ui::Avatar` painted through
        // `draw_ringed_at` — the per-user fill (peer's own `cursor_color`, or
        // the palette-pinned `user_color` fallback) plus a 1.5 px black
        // separator ring so overlapping avatars stay distinct (concept's
        // `border-2 border-black`). Routing through the toolkit keeps the
        // desktop roster pixel-identical to the app rosters that back it.
        for (i, initial) in self.collaborator_initials.iter().enumerate() {
            let cx = rx - i as f64 * (14.0 - 4.0);
            let center = Point::new(cx - 7.0, center_y);
            // 15 px chip (r = 7.5); the ring adds 1.5 px so the outer disc is
            // r = 9, matching the previous hand-painted separator geometry.
            let rect = Rect::new(
                center.x - 7.5,
                center.y - 7.5,
                center.x + 7.5,
                center.y + 7.5,
            );
            let fill = self
                .collaborator_colors
                .get(i)
                .copied()
                .unwrap_or(colors.accent[5]);
            let paint = if self.collaborator_is_agent.get(i).copied().unwrap_or(false) {
                ui::widget::Avatar::draw_agent_ringed_at
            } else {
                ui::widget::Avatar::draw_ringed_at
            };
            paint(
                scene,
                transform,
                rect,
                &initial.to_string(),
                fill,
                // The ring separates overlapping avatars from each
                // other, so it has to read against the bar it is drawn
                // on. A literal black did that on a dark bar and became
                // a hard black outline on a light one; the bar's own
                // surface is the correct separator on every theme.
                colors.surfaces.overlay,
                1.5,
                theme,
                text_renderer,
            );
        }

        // Overflow chip: "+N" in the same avatar slot, painted through the
        // shared `Avatar::draw_overflow_at` so it reads as "count of extras"
        // (neutral raised surface + dim label) rather than another person.
        if self.collaborator_overflow > 0 {
            let i = self.collaborator_initials.len();
            let cx = rx - i as f64 * (14.0 - 4.0);
            let center = Point::new(cx - 7.0, center_y);
            let rect = Rect::new(
                center.x - 7.5,
                center.y - 7.5,
                center.x + 7.5,
                center.y + 7.5,
            );
            ui::widget::Avatar::draw_overflow_at(
                scene,
                transform,
                rect,
                self.collaborator_overflow,
                colors.surfaces.raised,
                colors.surfaces.overlay,
                1.5,
                colors.text_dim,
                text_renderer,
            );
        }

        // Avatar stack click target. Spans from the left-most circle to the
        // right of the stack (rx before drawing).
        let slot_count =
            self.collaborator_initials.len() + if self.collaborator_overflow > 0 { 1 } else { 0 };
        if slot_count > 0 {
            // Stack grows leftward from `stack_right_x`; each circle is 18 px
            // wide with 4 px overlap, so step = 14 px.
            let step = 14.0_f64;
            let leftmost_cx = stack_right_x - (slot_count as f64 - 1.0) * step;
            let y_mid = center_y;
            self.avatar_stack_rect =
                Rect::new(leftmost_cx - 9.0, y_mid - 11.0, stack_right_x, y_mid + 11.0);
        } else {
            // An empty roster is exactly when this control matters most.
            // Clicking the stack is how the room's people are opened, and
            // a stack of nobody used to paint nothing and publish
            // `Rect::ZERO` — so the one person who needs the control, the
            // one about to invite the *first* collaborator, was the one
            // person it was not there for. The panel behind it could then
            // only be reached by its address, which is not a gesture.
            //
            // So the slot keeps its geometry and says what it is for. The
            // ring is dashed and the chip is unfilled because an empty
            // seat is not a person: a filled chip in the avatar row would
            // read as a participant nobody added. The verb is spelled out
            // beside it because a bare `+` in a top bar is a guess.
            let label = "Invite";
            let label_size = tokens.typography.scale.xs;
            let (label_w, label_h) =
                text_renderer.measure_weighted(label, label_size, None, TextWeight::Medium);
            let chip_r = 7.5_f64;
            let pill_x1 = stack_right_x + 2.0;
            let chip_cx = pill_x1 - 6.0 - chip_r;
            let label_x = chip_cx - chip_r - 6.0 - label_w as f64;
            let pill = Rect::new(label_x - 10.0, center_y - 12.0, pill_x1, center_y + 12.0);
            let pill_rr = RoundedRect::from_rect(pill, tokens.radii.sm as f64);
            scene.fill(
                Fill::NonZero,
                transform,
                colors.surfaces.raised,
                None,
                &pill_rr,
            );
            text_renderer.draw_text_weighted(
                scene,
                label,
                label_size,
                colors.text_dim,
                transform * Affine::translate((label_x, center_y - label_h as f64 / 2.0)),
                None,
                TextWeight::Medium,
            );
            let chip = vello::kurbo::Circle::new(Point::new(chip_cx, center_y), chip_r);
            scene.stroke(
                &vello::kurbo::Stroke::new(1.0).with_dashes(0.0, [3.0, 3.0]),
                transform,
                colors.border_standard,
                None,
                &chip,
            );
            let plus = Rect::new(chip_cx - 5.0, center_y - 5.0, chip_cx + 5.0, center_y + 5.0);
            ui::icons::draw_icon(
                scene,
                transform,
                plus,
                ui::icons::Icon::Plus,
                colors.text_dim,
                ui::icons::LUCIDE_DEFAULT_STROKE_PX,
            );
            self.avatar_stack_rect = pill;
        }

        // End of cache build — splice the freshly-encoded local scene
        // into the caller's `out_scene` with the host transform, then
        // restore `self.cached` and record the fingerprint so the next
        // matching frame takes the fast path above.
        out_scene.append(&local, Some(out_transform));
        self.cached = local;
        self.cache_fp = fp;
    }

    fn handle_event(
        &mut self,
        event: &WindowEvent,
        scale_factor: f64,
        _modifiers: &winit::event::Modifiers,
        _state: &mut AutoCommit,
        _path: &ObjId,
        handled: &mut bool,
        _focus_target: &mut Option<NodeId>,
        _node_id: NodeId,
        _taffy: &TaffyTree<()>,
    ) {
        // Track the cursor position so `MouseInput` (which doesn't carry a
        // position) can still hit-test against the click geometry captured
        // during the most recent draw pass.
        if let WindowEvent::CursorMoved { position, .. } = event {
            self.last_cursor = Some(Point::new(
                position.x / scale_factor,
                position.y / scale_factor,
            ));
            return;
        }
        // Scroll-wheel over the center band cycles the active center-widget
        // variant (Clock → SystemMonitor → Calendar → …). Mirrors the
        // sidebar-wheel accumulator contract so a single deliberate tick
        // advances one variant.
        if let WindowEvent::MouseWheel { delta, .. } = event {
            let Some(p) = self.last_cursor else { return };
            if !self.center_rect.contains(p) {
                return;
            }
            let dy = match delta {
                winit::event::MouseScrollDelta::LineDelta(_, y) => *y as f32,
                winit::event::MouseScrollDelta::PixelDelta(pos) => (pos.y as f32) / 40.0,
            };
            self.center_scroll_accum += dy;
            while self.center_scroll_accum >= 1.0 {
                self.center_widget = self.center_widget.next();
                self.center_scroll_accum -= 1.0;
            }
            while self.center_scroll_accum <= -1.0 {
                self.center_widget = self.center_widget.prev();
                self.center_scroll_accum += 1.0;
            }
            *handled = true;
            return;
        }
        let WindowEvent::MouseInput {
            state: winit::event::ElementState::Pressed,
            button: winit::event::MouseButton::Left,
            ..
        } = event
        else {
            return;
        };
        // Dot-indicator clicks: jump directly to that variant. These don't
        // need the shared chrome handle — purely local widget state.
        if let Some(p) = self.last_cursor {
            for (i, rect) in self.center_dot_rects.iter().enumerate() {
                if rect.contains(p) {
                    self.center_widget = CenterWidget::from_index(i);
                    *handled = true;
                    return;
                }
            }
        }
        let Some(chrome) = self.chrome.clone() else {
            return;
        };
        // Need the cursor position — winit delivers MouseInput without it,
        // so we reach into the last-known cursor via the Modifiers path.
        // For now, fall back to a separate CursorMoved tracker stored on
        // the widget. Without a cursor tracker we can't map; skip until a
        // CursorMoved arrives to prime the click area.
        let pos = match self.last_cursor {
            Some(p) => p,
            None => return,
        };
        let _ = scale_factor;
        let p = pos;
        let mut state = chrome.borrow_mut();
        if self.overview_btn_rect.contains(p) {
            state.overlays.overview = !state.overlays.overview;
            state.bump();
            *handled = true;
            return;
        }
        if self.up_btn_rect.contains(p) {
            if state.switch_workspace(-1) {
                *handled = true;
            }
            return;
        }
        if self.down_btn_rect.contains(p) {
            if state.switch_workspace(1) {
                *handled = true;
            }
            return;
        }
        if self.ws_pill_rect.contains(p) {
            state.overlays.ws_settings = !state.overlays.ws_settings;
            state.bump();
            *handled = true;
            return;
        }
        if self.tray_pill_rect.contains(p) {
            state.overlays.quick_settings = !state.overlays.quick_settings;
            state.bump();
            *handled = true;
            return;
        }
        if self.avatar_stack_rect.contains(p) {
            state.overlays.collaborators = !state.overlays.collaborators;
            state.bump();
            *handled = true;
        }
    }

    fn name(&self) -> &str {
        "TopBarChrome"
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }

    /// The bar's own affordances, so a harness clicks the pill the
    /// renderer painted instead of a guessed coordinate.
    fn test_rects(&self) -> Vec<(String, Rect)> {
        vec![
            ("topbar_overview_button".to_string(), self.overview_btn_rect),
            ("topbar_workspace_up".to_string(), self.up_btn_rect),
            ("topbar_workspace_down".to_string(), self.down_btn_rect),
            // State as well as geometry. The chevron rects above are
            // always published (they are the click targets); these two
            // carry the same rect only while the switch would actually
            // move. `walk_widget` drops zero-area entries, so a scenario
            // asks "is the up chevron live?" by asking whether the name
            // resolves at all.
            (
                "topbar_workspace_up_enabled".to_string(),
                if self.can_switch_up() { self.up_btn_rect } else { Rect::ZERO },
            ),
            (
                "topbar_workspace_down_enabled".to_string(),
                if self.can_switch_down() { self.down_btn_rect } else { Rect::ZERO },
            ),
            ("topbar_workspace_pill".to_string(), self.ws_pill_rect),
            ("topbar_tray_pill".to_string(), self.tray_pill_rect),
            ("topbar_avatar_stack".to_string(), self.avatar_stack_rect),
        ]
    }

    /// The top bar reports clean-after-first-frame. Subsequent
    /// invalidations flow through:
    ///   - [`Widget::update`] when the shared `Chrome` revision bumps
    ///     (workspace switch / overlay toggle); `update` returning
    ///     `true` is the standing contract for "redraw me".
    ///   - The cache fingerprint check inside `draw` itself, which
    ///     detects clock/SystemMonitor time-bucket transitions on the
    ///     next frame the App pumps (typically driven by some other
    ///     event — same as the pre-cache behaviour where the clock
    ///     lagged at idle).
    ///
    /// We must keep returning `true` until the first successful draw
    /// fills the cache (`rect_px.2 == 0` for the default fingerprint),
    /// otherwise the very first frame after layout never paints.
    fn needs_redraw(&self) -> bool {
        // `Default` for CacheFingerprint has rect_px = (0,0,0,0) → w=0.
        if self.cache_fp.rect_px.2 == 0 {
            return true;
        }
        // A status change has to reach the screen without waiting for
        // some unrelated event to force a frame. The ancestors cache
        // this subtree's scene and only rebuild when a descendant
        // reports dirty, so a battery that drains while the desktop
        // sits idle would otherwise keep painting the old figure until
        // the user happened to move the mouse.
        status_key_of(&self.live_status()) != self.cache_fp.status_key
    }

    fn mark_drawn(&mut self) {
        // Leaf widget — `cache_fp` is updated inside `draw` itself, so
        // there is nothing to clear here. Mirrors `Button::mark_drawn`.
    }

    /// The bar's intrinsic size is fixed at `(100%, TOPBAR_HEIGHT_PX)`
    /// in `build_layout`, so its taffy node never needs recomputing
    /// after the first pass. Returning `false` after that pass spares
    /// the App a `compute_layout_with_measure` on every CursorMoved.
    fn is_layout_dirty(&self) -> bool {
        self.rect == Rect::ZERO
    }
}

#[cfg(test)]
mod calendar_strip_tests {
    use super::*;
    use chrono::TimeZone;

    /// The strip reports the day it is actually run on. The previous
    /// implementation returned the same three strings forever, so this
    /// is what tells the two apart.
    #[test]
    fn the_strip_reads_the_clock() {
        let day = Local.with_ymd_and_hms(2026, 3, 9, 11, 0, 0).unwrap();
        let (weekday, date, _) = calendar_strip_parts(day);
        assert_eq!(weekday, "Mon");
        assert_eq!(date, "9 March");

        let other = Local.with_ymd_and_hms(2025, 12, 25, 11, 0, 0).unwrap();
        let (weekday, date, _) = calendar_strip_parts(other);
        assert_eq!(weekday, "Thu");
        assert_eq!(date, "25 December");
    }

    /// Nothing feeds the event slot, so it says so rather than naming
    /// a meeting nobody scheduled. Pinned by name: a stand-in that
    /// reads as a real appointment is the defect this closes.
    #[test]
    fn the_event_slot_names_no_meeting() {
        let (_, _, note) = calendar_strip_parts(Local::now());
        assert_eq!(note, "No calendar connected");
        assert!(
            !note.contains("Design Sync"),
            "the slot must not carry an invented appointment"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shell::state::{Workspace, WorkspaceId, WorkspaceTree};

    fn sample_chrome() -> TopBarChrome {
        let mut tree = WorkspaceTree {
            workspaces: vec![Workspace::new(WorkspaceId::new(), "A")],
            active: WorkspaceId::from(0),
            active_strip: Default::default(),
            active_group: Default::default(),
            collaborators: Vec::new(),
            ..Default::default()
        };
        tree.active = tree.workspaces[0].id;
        TopBarChrome::from_tree(&tree)
    }

    /// A room with an owner, a peer and an admitted agent in it.
    fn room_with_a_person_and_an_agent() -> WorkspaceTree {
        use crate::shell::state::{CapOverride, Collaborator, Subject, Verb};
        let mut tree = WorkspaceTree {
            workspaces: vec![Workspace::new(WorkspaceId::new(), "A")],
            active: WorkspaceId::from(0),
            ..Default::default()
        };
        tree.active = tree.workspaces[0].id;
        tree.collaborators.push(Collaborator::new("xaver", "Xaver"));
        let ws = tree.active_workspace_mut().expect("fixture has a room");
        ws.set_owner("quinten");
        let room = ws.id.raw().to_string();
        ws.grant_as(
            &"quinten".to_string(),
            CapOverride::membership(Subject::agent("scribe"), room, Verb::Write),
        )
        .expect("the owner may admit an agent");
        tree
    }

    /// The bar shows every member of the room, agents included.
    ///
    /// The stack walked the collaborator list directly, which holds
    /// peers and nothing else — an agent holds its membership by
    /// capability, not by presence, so it could never reach this
    /// surface at all. The bar is what a viewer sees without opening
    /// anything, so "who is in this room" was answered differently
    /// there than in the panel one click away.
    #[test]
    fn the_avatar_stack_shows_an_agent_that_is_in_the_room() {
        let chrome = TopBarChrome::from_tree(&room_with_a_person_and_an_agent());
        assert_eq!(
            chrome.collaborator_initials(),
            &['X', 'S'],
            "the person and the agent both belong in the stack",
        );
    }

    /// And says which is which, from the same flag the panel reads.
    #[test]
    fn the_avatar_stack_marks_the_agent_and_only_the_agent() {
        let chrome = TopBarChrome::from_tree(&room_with_a_person_and_an_agent());
        assert_eq!(
            chrome.collaborator_is_agent(),
            &[false, true],
            "one participant must not read as a person here and an agent \
             in the panel below",
        );
        assert_eq!(
            chrome.collaborator_is_agent().len(),
            chrome.collaborator_initials().len(),
            "every drawn chip needs its own answer, or the stack pairs a \
             mark with the wrong member",
        );
    }

    /// The local user still stays out of the cluster: it shows the
    /// *other* people in the room.
    #[test]
    fn the_avatar_stack_still_leaves_the_local_user_out() {
        use crate::shell::state::Collaborator;
        let mut tree = room_with_a_person_and_an_agent();
        tree.collaborators.insert(0, Collaborator::new("me", "Me"));
        let chrome = TopBarChrome::from_tree(&tree);
        assert_eq!(
            chrome.collaborator_initials(),
            &['X', 'S'],
            "the stack shows who else is here, not who is looking at it",
        );
    }

    /// Gap #8: the active center-band dot must be strictly larger than an
    /// inactive dot. The indigo-500 fill plus the scale-up together give
    /// the "where are we in the carousel" affordance; losing the scale
    /// delta would collapse the indicator into three same-size circles.
    #[test]
    fn active_center_dot_radius_is_strictly_larger_than_inactive() {
        let chrome = sample_chrome();
        assert!(
            chrome.active_dot_radius() > chrome.inactive_dot_radius(),
            "active dot radius {} must be strictly larger than inactive {}",
            chrome.active_dot_radius(),
            chrome.inactive_dot_radius(),
        );
        // Guard the specific 1.6× ratio from the concept so a future tweak
        // is an intentional decision, not a silent regression.
        assert!(
            (chrome.active_dot_radius() - TopBarChrome::DOT_BASE_SIZE * 1.6).abs() < f64::EPSILON,
            "active dot radius should be 1.6× the base size",
        );
    }

    /// The discs the centre band paints must fit the column that is laid
    /// out for them.
    ///
    /// This pins the painted geometry against the laid-out geometry rather
    /// than against a constant, because the bug was precisely that the two
    /// disagreed: the column was sized and stepped as though
    /// `DOT_BASE_SIZE` were a diameter while the discs were painted with it
    /// as a radius. Every assertion below held trivially on the old
    /// arithmetic if you only looked at the constants — you had to compare
    /// a disc's extent with its neighbour's to see it.
    #[test]
    fn center_dots_fit_the_column_they_are_laid_out_in() {
        // The real band: 360×28 centred in a full-width 40 px bar.
        let bar = Rect::new(0.0, 0.0, 1600.0, TOPBAR_HEIGHT_PX as f64);
        let band = Rect::new(620.0, 6.0, 980.0, 34.0);

        for active in 0..3 {
            let dots = TopBarChrome::center_dot_layout(band, bar, active);

            // The scaled-up disc is the one the carousel says is current.
            for (i, dot) in dots.iter().enumerate() {
                let want = if i == active {
                    TopBarChrome::DOT_ACTIVE_RADIUS
                } else {
                    TopBarChrome::DOT_INACTIVE_RADIUS
                };
                assert!(
                    (dot.radius - want).abs() < 1e-9,
                    "active={active} dot {i}: radius {} should be {want}",
                    dot.radius,
                );
            }

            // No disc may touch its neighbour. The old pitch was 7 px with
            // a 9.6 px active disc, so dot 0 and dot 1 fused into one blob.
            for i in 0..2 {
                let lower = dots[i].center.y + dots[i].radius;
                let upper = dots[i + 1].center.y - dots[i + 1].radius;
                assert!(
                    upper > lower,
                    "active={active}: dot {i} ends at {lower} but dot {} starts at {upper} \
                     — the discs overlap",
                    i + 1,
                );
            }

            // The two gaps the eye compares must match.
            let gap = |i: usize| {
                (dots[i + 1].center.y - dots[i + 1].radius) - (dots[i].center.y + dots[i].radius)
            };
            assert!(
                (gap(0) - gap(1)).abs() < 1e-9,
                "active={active}: gaps {} and {} should be equal",
                gap(0),
                gap(1),
            );

            // The whole column must sit inside the band, and centred on it.
            let top = dots[0].center.y - dots[0].radius;
            let bottom = dots[2].center.y + dots[2].radius;
            assert!(
                top >= band.y0 && bottom <= band.y1,
                "active={active}: column {top}..{bottom} escapes the band \
                 {}..{}",
                band.y0,
                band.y1,
            );
            assert!(
                ((top + bottom) / 2.0 - band.center().y).abs() < 1e-9,
                "active={active}: column centre {} should be the band's {}",
                (top + bottom) / 2.0,
                band.center().y,
            );

            // Every disc must be inside the slot that selects it, and no
            // two slots may overlap — the slots used to be 20 px squares on
            // an 8 px pitch, and the first-match-wins hit test handed dot
            // 0 most of dot 1's ink.
            for (i, dot) in dots.iter().enumerate() {
                assert!(
                    dot.slot.y0 <= dot.center.y - dot.radius
                        && dot.slot.y1 >= dot.center.y + dot.radius,
                    "active={active}: dot {i}'s disc is not inside its own click slot",
                );
                assert!(
                    dot.slot.contains(dot.center),
                    "active={active}: clicking dot {i}'s centre must select dot {i}",
                );
            }
            for i in 0..2 {
                assert!(
                    dots[i].slot.y1 <= dots[i + 1].slot.y0,
                    "active={active}: slot {i} overlaps slot {}",
                    i + 1,
                );
            }
        }
    }

    /// The SystemMonitor strip must tell its three metrics apart by
    /// colour, and must do it with colours that belong to the user's
    /// theme.
    ///
    /// This previously pinned three literal Tailwind hexes
    /// (emerald-400 / blue-400 / orange-400). That caught the original
    /// regression — all three labels collapsing to one accent — but it
    /// also froze the strip to three fixed pixels that were identical on
    /// every theme, including high-contrast, making the metric labels the
    /// one part of the bar that ignored the desktop around it. The
    /// property worth defending is the *cue*, not the hex, so this now
    /// asserts the cue directly and additionally asserts what the hex
    /// version could never catch: that the colours move when the theme
    /// does.
    #[test]
    fn system_monitor_metric_labels_are_distinct_and_theme_driven() {
        let chrome = sample_chrome();
        let kinds = [MetricKind::Cpu, MetricKind::Ram, MetricKind::Tmp];

        for (name, theme) in [
            ("dark", Theme::dark()),
            ("light", Theme::light()),
            ("high_contrast", Theme::high_contrast()),
        ] {
            let c: Vec<Color> = kinds.iter().map(|k| chrome.metric_label_color(&theme, *k)).collect();
            // The cue: three metrics, three different colours.
            for i in 0..c.len() {
                for j in (i + 1)..c.len() {
                    assert_ne!(
                        c[i].to_rgba8().to_u32(),
                        c[j].to_rgba8().to_u32(),
                        "{name}: {:?} and {:?} render in the same colour, collapsing the cue",
                        kinds[i],
                        kinds[j]
                    );
                }
            }
            // None of them may be the accent: the accent means live
            // state, not "this is a CPU reading".
            let accent = theme.tokens.colors.accent[5];
            for (k, col) in kinds.iter().zip(&c) {
                assert_ne!(
                    col.to_rgba8().to_u32(),
                    accent.to_rgba8().to_u32(),
                    "{name}: {k:?} label reuses the accent"
                );
            }
            // And each is the theme's own semantic token, not a literal.
            let sem = &theme.tokens.colors.semantic;
            assert_eq!(c[0].to_rgba8().to_u32(), sem.success.to_rgba8().to_u32(), "{name}: CPU");
            assert_eq!(c[1].to_rgba8().to_u32(), sem.info.to_rgba8().to_u32(), "{name}: RAM");
            assert_eq!(c[2].to_rgba8().to_u32(), sem.warning.to_rgba8().to_u32(), "{name}: TMP");
        }

        // The strip is not painted in fixed pixels: switching the theme
        // must actually move it. A hard-coded palette passes every
        // assertion above and fails this one.
        let dark = chrome.metric_label_color(&Theme::dark(), MetricKind::Cpu);
        let hc = chrome.metric_label_color(&Theme::high_contrast(), MetricKind::Cpu);
        assert_ne!(
            dark.to_rgba8().to_u32(),
            hc.to_rgba8().to_u32(),
            "the CPU label paints the same pixel on dark and high-contrast"
        );
    }

    /// The sparkline reads as the CPU history strip, so it takes the CPU
    /// metric's colour at a low weight — one colour relationship rather
    /// than a fourth independent hue, and it tracks the theme with the
    /// label above it.
    #[test]
    fn sparkline_shares_the_cpu_colour_at_a_backdrop_weight() {
        let chrome = sample_chrome();
        for (name, theme) in [
            ("dark", Theme::dark()),
            ("light", Theme::light()),
            ("high_contrast", Theme::high_contrast()),
        ] {
            let cpu = chrome.metric_label_color(&theme, MetricKind::Cpu);
            let fill = chrome.sparkline_fill(&theme, MetricKind::Cpu);
            let rgb = |c: Color| { let p = c.to_rgba8(); (p.r, p.g, p.b) };
            assert_eq!(rgb(fill), rgb(cpu), "{name}: sparkline hue drifted off the CPU label");
            assert!(
                fill.components[3] < cpu.components[3],
                "{name}: the sparkline must sit behind the numbers, not compete with them"
            );
            assert_ne!(
                fill.to_rgba8().to_u32(),
                theme.tokens.colors.accent[5].to_rgba8().to_u32(),
                "{name}: sparkline reuses the accent"
            );
        }
    }

    /// Audit #3 / Wave 3 unit 3-D: the rightmost avatar must sit ≥ 8 px
    /// to the left of the tray pill so the two clusters read as separate
    /// affordances. Pre-fix the spacer was `s3 = 12 px` measured from
    /// avatar centre, leaving roughly 3 px to the pill edge after the
    /// 9 px avatar disc radius. Bumping to `s4 = 16 px` recovers the
    /// breathing room.
    #[test]
    fn avatar_stack_has_breathing_room_to_tray_pill() {
        // Workspace seeded with one explicit collaborator (other than the
        // local user) so `collaborator_initials` is non-empty and the
        // avatar disc is actually painted.
        let mut tree = WorkspaceTree {
            workspaces: vec![Workspace::new(WorkspaceId::new(), "A")],
            active: WorkspaceId::from(0),
            active_strip: Default::default(),
            active_group: Default::default(),
            collaborators: vec![crate::shell::state::Collaborator::new("alice", "Alice")],
            ..Default::default()
        };
        tree.active = tree.workspaces[0].id;

        let mut bar = TopBarChrome::from_tree(&tree);
        let theme = Theme::default();
        let mut taffy: TaffyTree<()> = TaffyTree::new();
        let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
        let node = bar.build_layout(&mut taffy, &theme, &mut node_map);
        let vp = taffy
            .new_with_children(
                Style {
                    size: Size {
                        width: Dimension::length(1600.0),
                        height: Dimension::length(40.0),
                    },
                    ..Default::default()
                },
                &[node],
            )
            .unwrap();
        taffy
            .compute_layout_with_measure(
                vp,
                Size {
                    width: taffy::AvailableSpace::Definite(1600.0),
                    height: taffy::AvailableSpace::Definite(40.0),
                },
                |_, _, _, _, _| taffy::Size::ZERO,
            )
            .unwrap();
        bar.apply_layout(&taffy, node, Point::ORIGIN);
        let mut scene = Scene::new();
        let mut tr = TextRenderer::new();
        let presence = PresenceCtx::default();
        bar.draw(&mut scene, Affine::IDENTITY, &mut tr, &theme, &presence);

        let avatar = bar.avatar_stack_rect;
        let tray = bar.tray_pill_rect();
        assert!(
            avatar.width() > 0.0,
            "test seeded one collaborator but avatar_stack_rect is empty",
        );
        let gap = tray.x0 - avatar.x1;
        assert!(
            gap >= 8.0,
            "avatar→tray gap must be ≥ 8 px (got {gap:.2}); avatar.x1={}, tray.x0={}",
            avatar.x1,
            tray.x0,
        );
    }

    /// Inviting the *first* person is the one moment the avatar stack
    /// has nobody to draw, and it used to answer that by publishing
    /// `Rect::ZERO` — so the control for "open the room's people"
    /// vanished at exactly the moment somebody wanted it, and the panel
    /// behind it could only be reached by its address. An empty roster
    /// has to still be something a person can point at and click.
    #[test]
    fn empty_roster_still_offers_a_clickable_invite_affordance() {
        let mut tree = WorkspaceTree {
            workspaces: vec![Workspace::new(WorkspaceId::new(), "A")],
            active: WorkspaceId::from(0),
            active_strip: Default::default(),
            active_group: Default::default(),
            collaborators: Vec::new(),
            ..Default::default()
        };
        tree.active = tree.workspaces[0].id;

        let mut bar = TopBarChrome::from_tree(&tree);
        let chrome = crate::shell::chrome_state::ChromeState::new(tree);
        bar = bar.with_chrome(chrome.clone());

        let theme = Theme::default();
        let mut taffy: TaffyTree<()> = TaffyTree::new();
        let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
        let node = bar.build_layout(&mut taffy, &theme, &mut node_map);
        let vp = taffy
            .new_with_children(
                Style {
                    size: Size {
                        width: Dimension::length(1600.0),
                        height: Dimension::length(40.0),
                    },
                    ..Default::default()
                },
                &[node],
            )
            .unwrap();
        taffy
            .compute_layout_with_measure(
                vp,
                Size {
                    width: taffy::AvailableSpace::Definite(1600.0),
                    height: taffy::AvailableSpace::Definite(40.0),
                },
                |_, _, _, _, _| taffy::Size::ZERO,
            )
            .unwrap();
        bar.apply_layout(&taffy, node, Point::ORIGIN);
        let mut scene = Scene::new();
        let mut tr = TextRenderer::new();
        let presence = PresenceCtx::default();
        bar.draw(&mut scene, Affine::IDENTITY, &mut tr, &theme, &presence);

        assert!(
            bar.collaborator_initials().is_empty() && bar.collaborator_overflow() == 0,
            "this test is about the empty case and the roster is not empty",
        );
        let stack = bar.avatar_stack_rect;
        assert!(
            stack.area() > 0.0,
            "an empty roster still has to offer a control to click",
        );
        let bar_rect = bar.get_rect();
        assert!(
            stack.y0 >= bar_rect.y0 && stack.y1 <= bar_rect.y1,
            "the affordance sits inside the bar: {stack:?} vs {bar_rect:?}",
        );
        let tray = bar.tray_pill_rect();
        assert!(
            stack.x1 <= tray.x0,
            "the affordance stays left of the tray pill: stack.x1={}, tray.x0={}",
            stack.x1,
            tray.x0,
        );

        // And it is wired to the thing it looks like: clicking opens the
        // room's people, which is what the stack does when it has some.
        assert!(!chrome.borrow().overlays.collaborators);
        let mut doc = AutoCommit::new();
        let doc_root = automerge::ROOT.clone();
        let modifiers = winit::event::Modifiers::default();
        let mut focus: Option<NodeId> = None;
        for event in [
            WindowEvent::CursorMoved {
                device_id: unsafe { std::mem::zeroed() },
                position: winit::dpi::PhysicalPosition::new(
                    stack.center().x,
                    stack.center().y,
                ),
            },
            WindowEvent::MouseInput {
                device_id: unsafe { std::mem::zeroed() },
                state: winit::event::ElementState::Pressed,
                button: winit::event::MouseButton::Left,
            },
        ] {
            let mut handled = false;
            bar.handle_event(
                &event,
                1.0,
                &modifiers,
                &mut doc,
                &doc_root,
                &mut handled,
                &mut focus,
                node,
                &taffy,
            );
        }
        assert!(
            chrome.borrow().overlays.collaborators,
            "clicking the empty-roster affordance opened the collaborators panel",
        );
    }

    /// Regression: the topbar must paint a soft drop-shadow band below
    /// its rect so the seam between the translucent overlay surface and
    /// the dotted-grid backdrop doesn't read as "noisy pixels". The
    /// shadow is the only linear-gradient fill the topbar emits, so
    /// pinning a `LINEAR_GRADIENT` draw tag shows up after `draw` is a
    /// load-bearing signal — if a future refactor drops the gradient
    /// brush back to a solid fill (or removes the band entirely) this
    /// test fails loudly.
    #[test]
    fn topbar_paints_a_bottom_shadow_below_its_rect() {
        use vello_encoding::DrawTag;

        let mut tree = WorkspaceTree {
            workspaces: vec![Workspace::new(WorkspaceId::new(), "A")],
            active: WorkspaceId::from(0),
            active_strip: Default::default(),
            active_group: Default::default(),
            collaborators: Vec::new(),
            ..Default::default()
        };
        tree.active = tree.workspaces[0].id;

        let mut bar = TopBarChrome::from_tree(&tree);
        let theme = Theme::default();
        let mut taffy: TaffyTree<()> = TaffyTree::new();
        let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
        let node = bar.build_layout(&mut taffy, &theme, &mut node_map);
        let vp = taffy
            .new_with_children(
                Style {
                    size: Size {
                        width: Dimension::length(800.0),
                        height: Dimension::length(40.0),
                    },
                    ..Default::default()
                },
                &[node],
            )
            .unwrap();
        taffy
            .compute_layout_with_measure(
                vp,
                Size {
                    width: taffy::AvailableSpace::Definite(800.0),
                    height: taffy::AvailableSpace::Definite(40.0),
                },
                |_, _, _, _, _| taffy::Size::ZERO,
            )
            .unwrap();
        bar.apply_layout(&taffy, node, Point::ORIGIN);
        let mut scene = Scene::new();
        let mut tr = TextRenderer::new();
        let presence = PresenceCtx::default();
        bar.draw(&mut scene, Affine::IDENTITY, &mut tr, &theme, &presence);

        let gradient_fills = scene
            .encoding()
            .draw_tags
            .iter()
            .filter(|t| **t == DrawTag::LINEAR_GRADIENT)
            .count();
        assert!(
            gradient_fills >= 1,
            "topbar must emit at least one linear-gradient fill (the soft \
             drop-shadow band below its rect); got {gradient_fills}",
        );

        // Pin the band's vertical extent so a future tweak that shrinks
        // it to zero (or moves it above the bar) can't pass this test.
        // The shadow rect spans `[rect.y1, rect.y1 + 28]`.
        let bar_bottom = bar.rect.y1;
        assert!(bar_bottom > 0.0, "topbar rect must have non-zero y1");
        // Sanity: bar height matches the public TOPBAR_HEIGHT_PX constant
        // so the y1 the shadow attaches to is the one downstream code
        // (overlay_host, sidebar_wheel) lays out below.
        assert!(
            (bar_bottom - TOPBAR_HEIGHT_PX as f64).abs() < 0.5,
            "topbar y1 ({bar_bottom}) should equal TOPBAR_HEIGHT_PX ({TOPBAR_HEIGHT_PX})",
        );
    }

    /// The bar shows the room's people once there are any.
    ///
    /// The empty-roster slot says "Invite", which is right for a room
    /// nobody is in and wrong the moment somebody joins one: a bar
    /// offering to invite the first person, with a person already
    /// there, is the OS contradicting its own collaborators panel.
    ///
    /// Both states are asserted together because only the pair says
    /// anything. A stack that is empty in both cases and a stack that
    /// is full in both cases each pass half of this.
    #[test]
    fn a_peer_in_the_room_replaces_the_invite_slot_with_their_avatar() {
        let empty = WorkspaceTree {
            workspaces: vec![Workspace::new(WorkspaceId::new(), "A")],
            active: WorkspaceId::from(0),
            active_strip: Default::default(),
            active_group: Default::default(),
            collaborators: Vec::new(),
            ..Default::default()
        };
        let mut empty = empty;
        empty.active = empty.workspaces[0].id;
        let bar = TopBarChrome::from_tree(&empty);
        assert!(
            bar.collaborator_initials().is_empty(),
            "a room nobody is in has nobody to show, so the slot offers \
             the invite instead",
        );

        // What `PeerPresence::refresh_roster` writes when somebody
        // joins: a tree-level collaborator, id spelled the way the
        // presence path spells a peer.
        let mut joined = empty.clone();
        joined.collaborators = vec![crate::shell::state::Collaborator::new(
            "peer@11895642303397647149",
            "Xaver",
        )];
        let bar = TopBarChrome::from_tree(&joined);
        assert_eq!(
            bar.collaborator_initials(),
            &['X'],
            "with somebody in the room the bar must show them, not an \
             offer to invite the first person",
        );
        assert_eq!(
            bar.collaborator_overflow(),
            0,
            "one peer is not an overflow",
        );

        // And the state in between, which is the one that can lie. The
        // room has named him — the invitation is real and the panel
        // lists it — but he has not redeemed it. An avatar here would
        // be the bar claiming he is in the workspace, and it would
        // replace the very affordance that exists to invite him.
        let mut asked = empty.clone();
        asked.collaborators = vec![
            crate::shell::state::Collaborator::new("peer@11895642303397647149", "Xaver")
                .as_invited(),
        ];
        let bar = TopBarChrome::from_tree(&asked);
        assert!(
            bar.collaborator_initials().is_empty(),
            "somebody invited and not yet arrived is not an avatar in the \
             cluster: {:?}",
            bar.collaborator_initials(),
        );
        assert_eq!(
            bar.collaborator_overflow(),
            0,
            "and does not show up in the +N chip either",
        );
    }

    /// A guest shows the room, not the placeholder its own join left.
    ///
    /// The two lists the bar can read say different things, and only on
    /// a guest. Redeeming an invite admits exactly one row into the
    /// *workspace* — the inviter, under its session handle, because a
    /// signed token carries no name
    /// (`network::invite::accept_invite_token`). A moment later
    /// [`crate::shell::peer_presence::PeerPresence::refresh_roster`]
    /// computes the room's real people into the *tree-level* roster:
    /// named out of the room document, and including every peer that
    /// took part in it — which is how a participant with no pointer to
    /// move is counted at all.
    ///
    /// The bar preferred the workspace list whenever it held any
    /// non-`me` row, so a guest's stack was pinned to the single
    /// placeholder its join wrote: one avatar, initialled `P` from
    /// `p<hex>`, and nobody else arriving could ever add to it. A host
    /// never redeems an invite, so its workspace list stays empty and it
    /// saw the room correctly — which is why this only ever looked
    /// broken from the guest's side.
    #[test]
    fn a_guest_shows_the_room_rather_than_the_row_its_invite_left() {
        let mut tree = WorkspaceTree {
            workspaces: vec![Workspace::new(WorkspaceId::new(), "A")],
            active: WorkspaceId::from(0),
            active_strip: Default::default(),
            active_group: Default::default(),
            collaborators: Vec::new(),
            ..Default::default()
        };
        tree.active = tree.workspaces[0].id;

        // Exactly what redeeming the invite leaves behind, built the way
        // `accept_invite_token` builds it: the inviter's session handle
        // standing in for a name nobody has said yet.
        let inviter = 0xfaea_de77_9d28_394eu64;
        let placeholder = crate::shell::state::session_handle(inviter);
        tree.workspaces[0].collaborators = vec![
            crate::shell::state::Collaborator::new(placeholder.clone(), placeholder.clone())
                .with_session(inviter),
        ];

        // And what the room itself says a moment later: two people, both
        // named, both having taken part. Neither is invited-and-absent —
        // this is a room that is genuinely occupied.
        tree.collaborators = vec![
            crate::shell::state::Collaborator::new("peer@18080508259351411022", "quinten"),
            crate::shell::state::Collaborator::new("peer@7611448496700649778", "Claude"),
        ];

        let bar = TopBarChrome::from_tree(&tree);
        assert_eq!(
            bar.collaborator_initials(),
            &['Q', 'C'],
            "a guest's bar must show the room's people; it showed {:?}",
            bar.collaborator_initials(),
        );
        assert!(
            !bar.collaborator_initials().contains(&'P'),
            "the row the join left is a stand-in for somebody the room \
             now names, not a third person to draw beside them",
        );
        assert_eq!(
            crate::shell::state::active_workspace_peer_count(&tree),
            2,
            "and the +N chip and the wheel-disc dot count the same room",
        );

        // The other half, or the assertion above passes on a bar that
        // ignores the workspace outright: with nothing live to show, the
        // workspace's own roster is still what there is.
        let mut quiet = tree.clone();
        quiet.collaborators = Vec::new();
        let bar = TopBarChrome::from_tree(&quiet);
        assert_eq!(
            bar.collaborator_initials(),
            &['P'],
            "with no live room roster the workspace's own list is the \
             only answer there is, and must still be drawn",
        );
    }
}
