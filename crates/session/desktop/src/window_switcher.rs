//! Alt+Tab: the window switcher.
//!
//! Holding Alt and pressing Tab walks the current workspace's windows in the
//! order they were last used, and releasing Alt goes to the chosen one. A
//! quick Alt+Tab never shows anything and lands on the previous window; only
//! a held Alt, past [`REVEAL_DELAY`], raises the switcher with each window's
//! app mark and title. Alt+Grave (the key above Tab) walks only the focused app's windows.
//!
//! A "window" is a tab, as it is for every other window traversal in the
//! shell: groups are frames around windows, not windows themselves.
//!
//! This module owns the recency history, the open session, its geometry,
//! painting and semantic tree. The host routes input here and focuses
//! whatever a session commits to. What only the host knows (an app's own
//! artwork, which windows may be pictured, the pictures themselves) comes
//! through [`WindowSource`] and [`WindowThumbnails`].

use std::collections::HashMap;
use ui::num::f32_of_f64;

use vello::Scene;
use vello::kurbo::{Affine, Point, Rect, RoundedRect};
use vello::peniko::Fill;
use winit::event::{ElementState, MouseButton, WindowEvent};
use winit::keyboard::{Key, ModifiersState, NamedKey};

use ui::a11y::{AccessNode, AccessState, Role};
use ui::i18n::ArgValue;
use ui::icons::AppImage;
use ui::paint::PaintCtx;
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::time::{Duration, Instant};
use ui::widget::AppKind as UiAppKind;

use tilo_desktop_model::{DesktopTree, GroupId, StripId, TileContent, WorkspaceId};

use crate::i18n::{t, t_args};
use ui::TextRole;

/// Where a window's preview comes from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PreviewSource {
    /// A client's own latest buffer, from the host's frame store. Works for
    /// every tab of a client, shown or not.
    Client { surface: u64 },
    /// A window the host draws itself, painted through the same live path
    /// a screenshot of it takes. Only the tab its group shows is painted.
    Painted { capture_id: String },
    /// Never pictured: the app holds secrets.
    Withheld,
    /// Nothing worth picturing: a hidden tab of a host-drawn app, a window
    /// that is still a placeholder card, or a host that pictures nothing.
    Unavailable,
}

impl PreviewSource {
    /// Whether this source can produce pixels at all.
    pub fn renderable(&self) -> bool {
        matches!(self, Self::Client { .. } | Self::Painted { .. })
    }
}

/// One window the host should picture.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PreviewRequest {
    pub tab: u64,
    pub source: PreviewSource,
}

/// What a card can show for a window.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PreviewState {
    /// A live picture is ready.
    Live,
    /// The app holds secrets; its mark stands in.
    Withheld,
    /// No picture (yet); its mark stands in.
    None,
}

impl PreviewState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Live => "live",
            Self::Withheld => "withheld",
            Self::None => "none",
        }
    }
}

/// What only the host knows about a window: its application's artwork and
/// whether (and from where) it may be pictured. The defaults suit a host
/// with neither.
pub trait WindowSource {
    /// The glyph shown for `tile` when its application has no artwork.
    fn app_kind(&self, tile: &TileContent) -> UiAppKind {
        crate::resolver::tile_app_kind(tile)
    }

    /// The application's own artwork for `tile`.
    fn app_icon(&self, _tile: &TileContent) -> Option<AppImage> {
        None
    }

    /// Where a preview of `tile`, the tab `tab` of group `group`, comes
    /// from. `shown` is whether that tab is the one its group displays.
    fn preview_source(
        &self,
        _tile: &TileContent,
        _group: GroupId,
        _tab: u64,
        _shown: bool,
    ) -> PreviewSource {
        PreviewSource::Unavailable
    }
}

/// A host that knows nothing beyond the desktop tree.
pub struct PlainWindows;

impl WindowSource for PlainWindows {}

/// The host's live window pictures, keyed by tab.
pub trait WindowThumbnails {
    /// A picture of `tab` is on hand.
    fn has(&self, tab: u64) -> bool;

    /// Paint the picture of `tab` fitted inside `rect`, keeping its shape.
    /// Returns the rect it covers, or `None` when there is none.
    fn paint(
        &mut self,
        tab: u64,
        scene: &mut Scene,
        transform: Affine,
        rect: Rect,
        theme: &Theme,
    ) -> Option<Rect>;
}

/// A host with no pictures: every card shows its app mark.
pub struct NoThumbnails;

impl WindowThumbnails for NoThumbnails {
    fn has(&self, _tab: u64) -> bool {
        false
    }

    fn paint(
        &mut self,
        _tab: u64,
        _scene: &mut Scene,
        _transform: Affine,
        _rect: Rect,
        _theme: &Theme,
    ) -> Option<Rect> {
        None
    }
}

/// How long Alt has to stay down after Tab before the switcher appears.
/// Shorter than a deliberate look, longer than a practised Alt+Tab, so the
/// common "back to the last window" flick never flashes a panel.
pub const REVEAL_DELAY: Duration = Duration::from_millis(120);

/// Tabs remembered per workspace. Recency beyond this is noise.
const HISTORY_LIMIT: usize = 256;

/// Where a window lives, which is everything focusing it needs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WindowAddress {
    pub strip: StripId,
    pub group: GroupId,
    pub tab: u64,
}

/// Which application a window belongs to, and how to draw it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WindowApp {
    /// Two windows of one application share this key: the Wayland app id
    /// for a client, the app kind for one of Tilo's own.
    pub key: String,
    /// The glyph shown when the application has no artwork of its own.
    pub kind: UiAppKind,
    pub icon: Option<AppImage>,
}

impl WindowApp {
    pub fn of(tile: &TileContent, source: &dyn WindowSource) -> Self {
        let kind = source.app_kind(tile);
        match tile {
            TileContent::Foreign { app_id, .. }
            | TileContent::LocalApp { app_id, .. }
            | TileContent::PeerWindow { app_id, .. }
            | TileContent::HostedWindow { app_id, .. } => Self {
                key: format!("app:{app_id}"),
                kind,
                icon: source.app_icon(tile),
            },
            TileContent::AppPlaceholder { kind: app, .. } | TileContent::Stub { kind: app, .. } => {
                Self {
                    key: format!("kind:{}", app.as_str()),
                    kind,
                    icon: None,
                }
            }
            TileContent::WorkspaceActivity => Self {
                key: "workspace-activity".into(),
                kind,
                icon: None,
            },
        }
    }
}

/// One window as the switcher lists it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SwitcherWindow {
    pub address: WindowAddress,
    pub title: String,
    pub app: WindowApp,
    /// Where its live preview comes from, if it may have one.
    pub preview: PreviewSource,
}

/// Which windows a session walks.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SwitcherScope {
    /// Every window in the workspace (Alt+Tab).
    Workspace,
    /// The focused window's application only (Alt+Grave).
    FocusedApp,
}

impl SwitcherScope {
    /// The switcher chord `key` presses with `mods` held, if any: Alt+Tab
    /// walks the workspace, Alt with the key above Tab the focused app.
    pub fn for_chord(key: &Key, mods: ModifiersState) -> Option<Self> {
        if !mods.alt_key() {
            return None;
        }
        match key {
            Key::Named(NamedKey::Tab) => Some(Self::Workspace),
            key if is_grave(key) && !mods.control_key() && !mods.super_key() => {
                Some(Self::FocusedApp)
            }
            _ => None,
        }
    }
}

/// What the host must do after the switcher saw an input.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SwitcherOutcome {
    /// Not the switcher's: route it on.
    Ignored,
    /// Taken; the session is still open.
    Consumed,
    /// The session ended on this window: focus it.
    Commit(WindowAddress),
    /// The session ended without a change.
    Cancelled,
}

/// Every window of the active workspace, in tree order, with the focused one.
fn workspace_windows(
    desktop: &DesktopTree,
    source: &dyn WindowSource,
) -> (Vec<SwitcherWindow>, Option<usize>) {
    let Some(workspace) = desktop.active_workspace() else {
        return (Vec::new(), None);
    };
    let active_strip = desktop
        .active_strip
        .get(&workspace.id)
        .copied()
        .or_else(|| workspace.strips.first().map(|strip| strip.id));
    let mut windows = Vec::new();
    let mut current = None;
    for strip in &workspace.strips {
        let active_group = desktop
            .active_group_for(strip.id)
            .filter(|id| strip.window_groups.iter().any(|group| group.id == *id))
            .or_else(|| strip.window_groups.first().map(|group| group.id));
        for group in &strip.window_groups {
            for tab in &group.tabs {
                if Some(strip.id) == active_strip
                    && Some(group.id) == active_group
                    && tab.id == group.active_tab
                {
                    current = Some(windows.len());
                }
                let title = if tab.title.trim().is_empty() {
                    t("window-switcher-untitled")
                } else {
                    tab.title.clone()
                };
                windows.push(SwitcherWindow {
                    address: WindowAddress {
                        strip: strip.id,
                        group: group.id,
                        tab: tab.id,
                    },
                    title,
                    app: WindowApp::of(&tab.tile, source),
                    preview: source.preview_source(
                        &tab.tile,
                        group.id,
                        tab.id,
                        tab.id == group.active_tab,
                    ),
                });
            }
        }
    }
    (windows, current)
}

/// The focused tab of the active workspace, cheaply: no titles or icons.
fn focused_tab(desktop: &DesktopTree) -> Option<(WorkspaceId, u64)> {
    let workspace = desktop.active_workspace()?;
    let strip = desktop
        .active_strip
        .get(&workspace.id)
        .and_then(|id| workspace.strips.iter().find(|strip| strip.id == *id))
        .or_else(|| workspace.strips.first())?;
    let group = desktop
        .active_group_for(strip.id)
        .and_then(|id| strip.window_groups.iter().find(|group| group.id == id))
        .or_else(|| strip.window_groups.first())?;
    group
        .tabs
        .iter()
        .any(|tab| tab.id == group.active_tab)
        .then_some((workspace.id, group.active_tab))
}

/// Which windows were focused most recently, per workspace. Each workspace
/// is its own room with its own windows, so its recency is its own too.
#[derive(Default)]
pub struct FocusHistory {
    recent: HashMap<WorkspaceId, Vec<u64>>,
}

impl FocusHistory {
    /// Record the focused window as the most recent in its workspace.
    pub fn observe(&mut self, tree: &DesktopTree) {
        let Some((workspace, tab)) = focused_tab(tree) else {
            return;
        };
        let recent = self.recent.entry(workspace).or_default();
        if recent.first() == Some(&tab) {
            return;
        }
        recent.retain(|id| *id != tab);
        recent.insert(0, tab);
        recent.truncate(HISTORY_LIMIT);
        if self.recent.len() > tree.workspaces.len() {
            self.recent
                .retain(|id, _| tree.workspaces.iter().any(|ws| ws.id == *id));
        }
    }

    /// The active workspace's windows (tabs) by when they were last focused,
    /// most recent first; windows never focused are not listed.
    pub fn recent_tabs(&self, tree: &DesktopTree) -> Vec<u64> {
        tree.active_workspace()
            .and_then(|ws| self.recent.get(&ws.id))
            .cloned()
            .unwrap_or_default()
    }

    /// The active workspace's windows, focused one first, then by recency.
    /// Windows never focused follow in strip order from the focused one, so
    /// on a fresh desktop Alt+Tab steps to the next window along the strip.
    pub fn ordered_windows(
        &self,
        tree: &DesktopTree,
        source: &dyn WindowSource,
    ) -> Vec<SwitcherWindow> {
        let (windows, current) = workspace_windows(tree, source);
        let count = windows.len();
        let start = current.unwrap_or(0);
        let mut rank: HashMap<u64, usize> = HashMap::new();
        if let Some(recent) = tree
            .active_workspace()
            .and_then(|ws| self.recent.get(&ws.id))
        {
            for (index, tab) in recent.iter().enumerate() {
                rank.entry(*tab).or_insert(index);
            }
        }
        let current_tab = current
            .and_then(|index| windows.get(index))
            .map(|window| window.address.tab);
        let mut keyed: Vec<(usize, usize, SwitcherWindow)> = windows
            .into_iter()
            .enumerate()
            .map(|(index, window)| {
                let tier = if Some(window.address.tab) == current_tab {
                    0
                } else if let Some(rank) = rank.get(&window.address.tab) {
                    rank.saturating_add(1)
                } else {
                    HISTORY_LIMIT.saturating_add(1)
                };
                let along_strip = index
                    .saturating_add(count)
                    .saturating_sub(start)
                    .checked_rem(count)
                    .unwrap_or(0);
                (tier, along_strip, window)
            })
            .collect();
        keyed.sort_by_key(|(tier, along, _)| (*tier, *along));
        keyed.into_iter().map(|(_, _, window)| window).collect()
    }
}

struct Session {
    workspace: WorkspaceId,
    scope: SwitcherScope,
    windows: Vec<SwitcherWindow>,
    selected: usize,
    opened: Instant,
}

/// Sizes of the panel, all read off the theme's tokens.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
struct Metrics {
    /// The app mark standing in for a window without a preview.
    icon: f64,
    /// The app mark beside a previewed window's title.
    mark: f64,
    /// The height of the 16:10 well a window's preview sits in; the
    /// card is the well plus an inset on each side.
    well_h: f64,
    well_radius: f64,
    card_w: f64,
    card_h: f64,
    inset: f64,
    gap: f64,
    pad: f64,
    margin: f64,
    radius: f64,
    card_radius: f64,
    caption_h: f64,
}

impl Metrics {
    fn from_tokens(tokens: &ui::tokens::Tokens) -> Self {
        let spacing = &tokens.spacing;
        let line = |role| f64::from(tokens.typography.style(role).line_height_px());
        let inset = f64::from(spacing.s2);
        let title_h = line(TextRole::Label);
        let well_w = f64::from(spacing.s16) * 2.5;
        let well_h = well_w * 10.0 / 16.0;
        Self {
            icon: f64::from(spacing.s12),
            mark: f64::from(tokens.metrics.icon_sm),
            well_h,
            well_radius: f64::from(tokens.radii.control),
            card_w: well_w + 2.0 * inset,
            card_h: inset + well_h + inset + title_h + inset,
            inset,
            gap: f64::from(spacing.s2),
            pad: f64::from(spacing.s4),
            margin: f64::from(spacing.s8),
            radius: f64::from(tokens.radii.panel),
            card_radius: f64::from(tokens.radii.control),
            caption_h: line(TextRole::Subheading) + f64::from(spacing.s3),
        }
    }
}

impl Metrics {
    /// These metrics fitted to `host` with `caption_h` reserved under the
    /// cards: unchanged when one card fits inside the margins, otherwise
    /// the outer margin falls to the card inset and the preview well
    /// shrinks (keeping 16:10) until one card does, so the panel never
    /// runs off a small display.
    fn fitted(self, host: Rect, caption_h: f64) -> Self {
        let room = |m: &Self| {
            (
                host.width() - 2.0 * (m.margin + m.pad),
                host.height() - 2.0 * (m.margin + m.pad) - caption_h,
            )
        };
        let (room_w, room_h) = room(&self);
        if room_w >= self.card_w && room_h >= self.card_h {
            return self;
        }
        let mut m = self;
        m.margin = m.margin.min(m.inset);
        let (room_w, room_h) = room(&m);
        let text_part = m.card_h - m.well_h;
        let well_w = (m.card_w - 2.0 * m.inset)
            .min(room_w - 2.0 * m.inset)
            .min((room_h - text_part) * 16.0 / 10.0)
            .max(0.0);
        m.well_h = well_w * 10.0 / 16.0;
        m.card_w = well_w + 2.0 * m.inset;
        m.card_h = text_part + m.well_h;
        m.icon = m.icon.min(m.well_h * 0.6);
        m
    }
}

/// Where everything of an open switcher sits.
#[derive(Clone, Debug, Default, PartialEq)]
struct Geometry {
    panel: Rect,
    /// `(window index, card)` for the page on screen.
    cards: Vec<(usize, Rect)>,
    /// The page position line under the cards; zero when every window is
    /// on screen and there is nothing to add to the cards.
    caption: Rect,
    /// The metrics the cards were laid out at, fitted to the host.
    metrics: Metrics,
}

fn usize_to_f64(n: usize) -> f64 {
    u32::try_from(n).map_or_else(|_| f64::from(u32::MAX), f64::from)
}

#[expect(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "a non-negative count of cards that fit on a display, floored"
)]
fn fit_count(span: f64, pitch: f64) -> usize {
    if pitch <= 0.0 || !span.is_finite() {
        return 1;
    }
    ((span / pitch).floor().max(1.0)) as usize
}

/// Cards in rows, centred on the display, with the selected window's page
/// on screen when there are more windows than fit.
fn geometry(host: Rect, count: usize, selected: usize, m: Metrics) -> Geometry {
    if count == 0 {
        return Geometry::default();
    }
    // The caption only says where the page is, so it exists only when the
    // windows do not all fit.
    let whole = geometry_with(host, count, selected, m.fitted(host, 0.0), 0.0);
    if whole.cards.len() >= count {
        return whole;
    }
    let fitted = m.fitted(host, m.caption_h);
    let paged = geometry_with(host, count, selected, fitted, m.caption_h);
    if paged.cards.len() >= count {
        // Shrinking the cards to make room for the line fitted them all
        // after all: there is no page to report.
        return geometry_with(host, count, selected, fitted, 0.0);
    }
    paged
}

fn geometry_with(
    host: Rect,
    count: usize,
    selected: usize,
    m: Metrics,
    caption_h: f64,
) -> Geometry {
    let pitch_x = m.card_w + m.gap;
    let pitch_y = m.card_h + m.gap;
    let avail_w = host.width() - 2.0 * (m.margin + m.pad) + m.gap;
    let avail_h = host.height() - 2.0 * (m.margin + m.pad) - caption_h + m.gap;
    let columns = fit_count(avail_w, pitch_x).min(count);
    let rows_fit = fit_count(avail_h, pitch_y);
    let page = columns.saturating_mul(rows_fit).max(1);
    let first = selected.checked_div(page).unwrap_or(0).saturating_mul(page);
    let shown = count.saturating_sub(first).min(page);
    let rows = shown.div_ceil(columns).max(1);
    let grid_w = usize_to_f64(columns) * pitch_x - m.gap;
    let grid_h = usize_to_f64(rows) * pitch_y - m.gap;
    let panel_w = grid_w + 2.0 * m.pad;
    let panel_h = grid_h + caption_h + 2.0 * m.pad;
    let center = host.center();
    let panel = Rect::new(
        center.x - panel_w / 2.0,
        center.y - panel_h / 2.0,
        center.x + panel_w / 2.0,
        center.y + panel_h / 2.0,
    );
    let cards = (0..shown)
        .map(|slot| {
            let row = slot.checked_div(columns).unwrap_or(0);
            let column = slot.checked_rem(columns).unwrap_or(0);
            // The last row is centred when it is short.
            let in_row = if row.saturating_add(1) == rows {
                shown.saturating_sub(row.saturating_mul(columns))
            } else {
                columns
            };
            let row_w = usize_to_f64(in_row) * pitch_x - m.gap;
            let x0 = panel.x0 + m.pad + (grid_w - row_w) / 2.0 + usize_to_f64(column) * pitch_x;
            let y0 = panel.y0 + m.pad + usize_to_f64(row) * pitch_y;
            (
                first.saturating_add(slot),
                Rect::new(x0, y0, x0 + m.card_w, y0 + m.card_h),
            )
        })
        .collect();
    let caption = Rect::new(
        panel.x0 + m.pad,
        panel.y1 - m.pad - caption_h,
        panel.x1 - m.pad,
        panel.y1 - m.pad,
    );
    Geometry {
        panel,
        cards,
        caption,
        metrics: m,
    }
}

/// The Alt+Tab switcher: recency history plus at most one open session.
pub struct WindowSwitcher {
    history: FocusHistory,
    session: Option<Session>,
    host_rect: Rect,
    metrics: Metrics,
    hover: Option<usize>,
    /// Geometry changed since the last visible frame; old pixels are inert.
    pointer_dirty: bool,
}

impl Default for WindowSwitcher {
    fn default() -> Self {
        Self {
            history: FocusHistory::default(),
            session: None,
            host_rect: Rect::ZERO,
            metrics: Metrics::from_tokens(&Theme::dark().tokens),
            hover: None,
            pointer_dirty: false,
        }
    }
}

fn is_grave(key: &Key) -> bool {
    matches!(key, Key::Character(text) if matches!(text.as_str(), "`" | "~"))
}

impl WindowSwitcher {
    /// Note which window has focus now, so recency stays true whichever
    /// path moved it (click, keyboard, search, an app).
    pub fn observe(&mut self, tree: &DesktopTree) {
        self.history.observe(tree);
        self.reconcile_session(tree);
    }

    /// Keep the opening order, but never arm an address that disappeared or moved.
    fn reconcile_session(&mut self, tree: &DesktopTree) {
        let Some(session) = self.session.as_mut() else {
            return;
        };
        if session.workspace != tree.active {
            self.cancel();
            return;
        }
        let Some(workspace) = tree.active_workspace() else {
            self.cancel();
            return;
        };
        let selected = session.windows.get(session.selected).map(|w| w.address.tab);
        let hovered = self
            .hover
            .and_then(|i| session.windows.get(i))
            .map(|w| w.address.tab);
        let mut live = HashMap::new();
        for strip in &workspace.strips {
            for group in &strip.window_groups {
                for tab in &group.tabs {
                    live.insert(
                        tab.id,
                        (
                            WindowAddress {
                                strip: strip.id,
                                group: group.id,
                                tab: tab.id,
                            },
                            tab,
                        ),
                    );
                }
            }
        }
        let Some(selected) = selected.filter(|id| live.contains_key(id)) else {
            self.cancel();
            return;
        };
        let before = session.windows.len();
        session.windows.retain_mut(|window| {
            let Some((address, tab)) = live.get(&window.address.tab) else {
                return false;
            };
            window.address = *address;
            if tab.title.trim().is_empty() {
                window.title = t("window-switcher-untitled");
            } else {
                window.title.clone_from(&tab.title);
            }
            true
        });
        let Some(index) = session
            .windows
            .iter()
            .position(|w| w.address.tab == selected)
        else {
            self.cancel();
            return;
        };
        session.selected = index;
        self.pointer_dirty |= session.windows.len() != before;
        self.hover =
            hovered.and_then(|id| session.windows.iter().position(|w| w.address.tab == id));
    }

    /// The active workspace's windows by when they were last focused, most
    /// recent first ([`FocusHistory::recent_tabs`]).
    pub fn recent_tabs(&self, tree: &DesktopTree) -> Vec<u64> {
        self.history.recent_tabs(tree)
    }

    /// The windows Alt+Tab would walk right now, in order.
    pub fn ordered_windows(
        &self,
        tree: &DesktopTree,
        source: &dyn WindowSource,
    ) -> Vec<SwitcherWindow> {
        self.history.ordered_windows(tree, source)
    }

    /// A session is open: Alt is held after Tab.
    pub fn is_active(&self) -> bool {
        self.session.is_some()
    }

    /// The panel is on screen.
    pub fn is_revealed(&self, now: Instant) -> bool {
        self.session
            .as_ref()
            .is_some_and(|session| now.saturating_duration_since(session.opened) >= REVEAL_DELAY)
    }

    /// A frame is needed to put the panel up once the delay has passed.
    pub fn needs_frame(&self, now: Instant) -> bool {
        self.is_active() && (!self.is_revealed(now) || self.pointer_dirty)
    }

    /// Treat Alt as having been held `by` longer than it has.
    pub fn elapse(&mut self, by: Duration) {
        if let Some(session) = self.session.as_mut() {
            session.opened = session.opened.checked_sub(by).unwrap_or(session.opened);
        }
    }

    /// The window a release would go to.
    pub fn selected(&self) -> Option<&SwitcherWindow> {
        self.session
            .as_ref()
            .and_then(|session| session.windows.get(session.selected))
    }

    /// Open a session on the first step: the previous window, or the last
    /// one when walking backwards. Returns `false` when there is nothing to
    /// switch to.
    pub fn begin(
        &mut self,
        tree: &DesktopTree,
        source: &dyn WindowSource,
        scope: SwitcherScope,
        backwards: bool,
        now: Instant,
    ) -> bool {
        self.observe(tree);
        let mut windows = self.history.ordered_windows(tree, source);
        if scope == SwitcherScope::FocusedApp {
            let Some(app) = windows.first().map(|window| window.app.key.clone()) else {
                return false;
            };
            windows.retain(|window| window.app.key == app);
        }
        if windows.len() < 2 {
            return false;
        }
        let selected = if backwards {
            windows.len().saturating_sub(1)
        } else {
            1
        };
        self.session = Some(Session {
            workspace: tree.active,
            scope,
            windows,
            selected,
            opened: now,
        });
        self.hover = None;
        self.pointer_dirty = false;
        true
    }

    fn step(&mut self, delta: isize) {
        if let Some(session) = self.session.as_mut() {
            let len = isize::try_from(session.windows.len()).unwrap_or(isize::MAX);
            if len == 0 {
                return;
            }
            let current = isize::try_from(session.selected).unwrap_or(0);
            let next = current.saturating_add(delta).rem_euclid(len);
            session.selected = usize::try_from(next).unwrap_or(0);
        }
    }

    fn select(&mut self, index: usize) -> bool {
        match self.session.as_mut() {
            Some(session) if index < session.windows.len() => {
                session.selected = index;
                true
            }
            _ => false,
        }
    }

    fn commit(&mut self) -> SwitcherOutcome {
        self.hover = None;
        self.pointer_dirty = false;
        self.session
            .take()
            .and_then(|session| session.windows.get(session.selected).map(|w| w.address))
            .map_or(SwitcherOutcome::Cancelled, SwitcherOutcome::Commit)
    }

    fn cancel(&mut self) -> SwitcherOutcome {
        self.hover = None;
        self.pointer_dirty = false;
        if self.session.take().is_some() {
            SwitcherOutcome::Cancelled
        } else {
            SwitcherOutcome::Ignored
        }
    }

    /// End the session without switching (focus moved elsewhere, the
    /// workspace changed, the screen locked).
    pub fn dismiss(&mut self) -> bool {
        self.cancel() == SwitcherOutcome::Cancelled
    }

    /// A key while a session is open. `mods` is the modifier state with
    /// this key applied, so an Alt release arrives with Alt already up.
    pub fn key(&mut self, key: &Key, state: ElementState, mods: ModifiersState) -> SwitcherOutcome {
        if self.session.is_none() {
            return SwitcherOutcome::Ignored;
        }
        if state == ElementState::Released {
            let alt_released = matches!(key, Key::Named(NamedKey::Alt)) || !mods.alt_key();
            return if alt_released {
                self.commit()
            } else {
                SwitcherOutcome::Consumed
            };
        }
        let backwards = mods.shift_key();
        match key {
            Key::Named(NamedKey::Escape) => self.cancel(),
            Key::Named(NamedKey::Enter | NamedKey::Space) => self.commit(),
            Key::Named(NamedKey::Tab) => {
                self.step(if backwards { -1 } else { 1 });
                SwitcherOutcome::Consumed
            }
            key if is_grave(key) => {
                self.step(if backwards { -1 } else { 1 });
                SwitcherOutcome::Consumed
            }
            Key::Named(NamedKey::ArrowLeft | NamedKey::ArrowUp) => {
                self.step(-1);
                SwitcherOutcome::Consumed
            }
            Key::Named(NamedKey::ArrowRight | NamedKey::ArrowDown) => {
                self.step(1);
                SwitcherOutcome::Consumed
            }
            Key::Named(NamedKey::Home) => {
                self.select(0);
                SwitcherOutcome::Consumed
            }
            Key::Named(NamedKey::End) => {
                let last = self
                    .session
                    .as_ref()
                    .map_or(0, |session| session.windows.len().saturating_sub(1));
                self.select(last);
                SwitcherOutcome::Consumed
            }
            // Alt came up without this session hearing it (another seat
            // took it, or the event was lost): a key pressed without Alt
            // settles on the selection rather than typing into a window
            // nobody chose yet.
            _ if !mods.alt_key() && !matches!(key, Key::Named(NamedKey::Alt)) => self.commit(),
            _ => SwitcherOutcome::Consumed,
        }
    }

    /// Input while a session is open. Keys all belong to it; the pointer
    /// (at `cursor`, in the host's coordinates) does once the panel is on
    /// screen. A modifier change is reported but belongs to the host too.
    pub fn route_event(
        &mut self,
        event: &WindowEvent,
        mods: ModifiersState,
        cursor: Option<Point>,
        now: Instant,
    ) -> SwitcherOutcome {
        if self.session.is_none() {
            return SwitcherOutcome::Ignored;
        }
        match event {
            WindowEvent::KeyboardInput { event: key, .. } => {
                self.key(&key.logical_key, key.state, mods)
            }
            WindowEvent::ModifiersChanged(changed) => self.modifiers_changed(changed.state()),
            WindowEvent::CursorMoved { .. } => cursor.map_or(SwitcherOutcome::Ignored, |point| {
                self.pointer_moved(point, now)
            }),
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            } => cursor.map_or(SwitcherOutcome::Ignored, |point| {
                self.pointer_pressed(point, now)
            }),
            WindowEvent::MouseInput { .. }
            | WindowEvent::MouseWheel { .. }
            | WindowEvent::Touch(_)
                if self.is_revealed(now) =>
            {
                SwitcherOutcome::Consumed
            }
            _ => SwitcherOutcome::Ignored,
        }
    }

    /// The modifier state changed with no key to go with it.
    pub fn modifiers_changed(&mut self, mods: ModifiersState) -> SwitcherOutcome {
        if self.session.is_some() && !mods.alt_key() {
            self.commit()
        } else {
            SwitcherOutcome::Ignored
        }
    }

    fn geometry(&self) -> Geometry {
        match self.session.as_ref() {
            Some(session) => geometry(
                self.host_rect,
                session.windows.len(),
                session.selected,
                self.metrics,
            ),
            None => Geometry::default(),
        }
    }

    /// Lay out against the host's rect before the first paint, so pointer
    /// input and the semantic tree agree with what will be drawn.
    pub fn set_host_rect(&mut self, host: Rect) {
        if self.is_active() && self.host_rect != host {
            self.pointer_dirty = true;
        }
        self.host_rect = host;
    }

    /// The window card under `point`, if the panel is up.
    pub fn card_at(&self, point: Point, now: Instant) -> Option<usize> {
        if !self.is_revealed(now) {
            return None;
        }
        self.geometry()
            .cards
            .iter()
            .find(|(_, rect)| rect.contains(point))
            .map(|(index, _)| *index)
    }

    /// Pointer motion: hovering a card selects it, as it does in every
    /// desktop's switcher, so a release commits what the pointer rests on.
    pub fn pointer_moved(&mut self, point: Point, now: Instant) -> SwitcherOutcome {
        if !self.is_revealed(now) {
            return SwitcherOutcome::Ignored;
        }
        if self.pointer_dirty {
            return SwitcherOutcome::Consumed;
        }
        let card = self.card_at(point, now);
        if card != self.hover {
            self.hover = card;
            if let Some(index) = card {
                self.select(index);
            }
        }
        SwitcherOutcome::Consumed
    }

    /// A primary press (a click, or a tap, which arrives as one): on a card
    /// it goes there, off the panel it cancels.
    pub fn pointer_pressed(&mut self, point: Point, now: Instant) -> SwitcherOutcome {
        if !self.is_revealed(now) {
            return SwitcherOutcome::Ignored;
        }
        if self.pointer_dirty {
            return SwitcherOutcome::Consumed;
        }
        match self.card_at(point, now) {
            Some(index) => {
                self.select(index);
                self.commit()
            }
            None if self.geometry().panel.contains(point) => SwitcherOutcome::Consumed,
            None => self.cancel(),
        }
    }

    /// Select the window at `index` without committing: what an assistive
    /// technology's focus does.
    pub fn focus_index(&mut self, index: usize) -> bool {
        self.select(index)
    }

    /// Bounds of the card for window `index`, when on screen.
    pub fn card_rect(&self, index: usize) -> Option<Rect> {
        self.geometry()
            .cards
            .iter()
            .find(|(candidate, _)| *candidate == index)
            .map(|(_, rect)| *rect)
    }

    pub fn panel_rect(&self) -> Rect {
        self.geometry().panel
    }

    /// The open panel's semantic tree: a dialog holding the window list,
    /// the selected window both selected and focused.
    pub fn accessibility(&self) -> Option<AccessNode> {
        let session = self.session.as_ref()?;
        let geometry = self.geometry();
        let total = session.windows.len();
        let items = session
            .windows
            .iter()
            .enumerate()
            .map(|(index, window)| {
                let selected = index == session.selected;
                let mut node = AccessNode::new(Role::ListItem)
                    .with_name(window.title.clone())
                    .with_semantic_key(format!("window-switcher-window:{}", window.address.tab))
                    .with_position(index.saturating_add(1), total)
                    .with_state(
                        AccessState::new()
                            .focusable(true)
                            .focused(selected)
                            .selected(selected),
                    );
                if let Some((_, rect)) = geometry.cards.iter().find(|(i, _)| *i == index) {
                    node = node.with_bounds(*rect);
                }
                node
            })
            .collect();
        let (name, description) = match session.scope {
            SwitcherScope::Workspace => {
                (t("window-switcher-title"), t("window-switcher-description"))
            }
            SwitcherScope::FocusedApp => (
                t("window-switcher-app-title"),
                t("window-switcher-app-description"),
            ),
        };
        Some(
            AccessNode::new(Role::Dialog)
                .with_name(name.clone())
                .with_description(description)
                .with_bounds(geometry.panel)
                .with_state(AccessState::new().modal(true))
                .with_children(vec![
                    AccessNode::new(Role::List)
                        .with_name(name)
                        .with_bounds(geometry.panel)
                        .with_children(items),
                ]),
        )
    }

    /// Paint the panel once it is revealed.
    pub fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        host: Rect,
        now: Instant,
        previews: &mut dyn WindowThumbnails,
    ) {
        self.host_rect = host;
        self.metrics = Metrics::from_tokens(&theme.tokens);
        if !self.is_revealed(now) {
            return;
        }
        // Theme type tokens already carry the user's text scale (HIG §7a).
        let renderer_scale = text_renderer.text_scale();
        text_renderer.set_text_scale(1.0);
        self.draw_panel(scene, transform, text_renderer, theme, previews);
        self.pointer_dirty = false;
        text_renderer.set_text_scale(renderer_scale);
    }

    /// The windows of the open session that may have a live preview, the
    /// page on screen first.
    pub fn preview_requests(&self) -> Vec<PreviewRequest> {
        let Some(session) = self.session.as_ref() else {
            return Vec::new();
        };
        let on_screen: Vec<usize> = self
            .geometry()
            .cards
            .iter()
            .map(|(index, _)| *index)
            .collect();
        let rest = (0..session.windows.len()).filter(|index| !on_screen.contains(index));
        on_screen
            .iter()
            .copied()
            .chain(rest)
            .filter_map(|index| session.windows.get(index))
            .filter(|window| window.preview.renderable())
            .map(|window| PreviewRequest {
                tab: window.address.tab,
                source: window.preview.clone(),
            })
            .collect()
    }

    /// What a card can show for `window` right now.
    pub fn preview_state(window: &SwitcherWindow, previews: &dyn WindowThumbnails) -> PreviewState {
        match window.preview {
            PreviewSource::Withheld => PreviewState::Withheld,
            _ if previews.has(window.address.tab) => PreviewState::Live,
            _ => PreviewState::None,
        }
    }

    /// The open session as data: each window, whether it is selected, and
    /// whether its card shows a live preview. Never the pixels themselves.
    pub fn state_json(&self, previews: &dyn WindowThumbnails, now: Instant) -> serde_json::Value {
        let Some(session) = self.session.as_ref() else {
            return serde_json::json!({ "open": false });
        };
        let windows: Vec<serde_json::Value> = session
            .windows
            .iter()
            .enumerate()
            .map(|(index, window)| {
                serde_json::json!({
                    "tab": window.address.tab,
                    "title": window.title,
                    "selected": index == session.selected,
                    "preview": Self::preview_state(window, previews).as_str(),
                })
            })
            .collect();
        serde_json::json!({
            "open": true,
            "revealed": self.is_revealed(now),
            "scope": match session.scope {
                SwitcherScope::Workspace => "workspace",
                SwitcherScope::FocusedApp => "app",
            },
            "windows": windows,
        })
    }

    fn draw_panel(
        &self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        previews: &mut dyn WindowThumbnails,
    ) {
        let Some(session) = self.session.as_ref() else {
            return;
        };
        let tokens = &theme.tokens;
        let colors = &tokens.colors;
        let geometry = self.geometry();
        let m = geometry.metrics;
        let panel = geometry.panel;

        // A Panel (HIG §9.1): the shell's floating-surface ground, edge and
        // elevation, like the overview and the palette.
        ui::surface::shadow(
            scene,
            transform,
            panel,
            m.radius,
            &tokens.elevation.floating,
        );
        scene.fill(
            Fill::NonZero,
            transform,
            ui::chrome::shell::panel_ground(theme),
            None,
            &RoundedRect::from_rect(panel, m.radius),
        );
        scene.stroke(
            &vello::kurbo::Stroke::new(1.0),
            transform,
            ui::chrome::shell::panel_border(theme),
            None,
            &ui::surface::edge_outline(panel, m.radius),
        );

        for (index, card) in &geometry.cards {
            if let Some(window) = session.windows.get(*index) {
                let state = ui::chrome::SelectionState::new(
                    *index == session.selected,
                    self.hover == Some(*index) && *index != session.selected,
                    false,
                );
                Self::draw_card(
                    &mut PaintCtx::new(scene, transform, text_renderer, theme),
                    (*card, m),
                    window,
                    state,
                    previews,
                );
            }
        }

        // Where this page sits among the windows, when they do not all fit.
        // The selected window's title is on its card already; repeating it
        // here only said the same thing twice.
        if geometry.caption.height() > 0.0 {
            let caption = t_args(
                "window-switcher-caption-paged",
                &[
                    (
                        "position",
                        ArgValue::Number(usize_to_f64(session.selected.saturating_add(1))),
                    ),
                    (
                        "count",
                        ArgValue::Number(usize_to_f64(session.windows.len())),
                    ),
                ],
            );
            let text = text_renderer.elide_to_width_style(
                &caption,
                f32_of_f64(geometry.caption.width()),
                &theme.text(TextRole::Subheading),
            );
            let (w, h) =
                text_renderer.measure_style(&text, None, &theme.text(TextRole::Subheading));
            text_renderer.draw_style(
                scene,
                &text,
                colors.text_dim,
                transform.pre_translate(
                    (
                        geometry.caption.center().x - f64::from(w) / 2.0,
                        geometry.caption.center().y - f64::from(h) / 2.0,
                    )
                        .into(),
                ),
                None,
                &theme.text(TextRole::Subheading),
            );
        }
    }

    /// One window's card: its live preview (or app mark) over its elided
    /// title, lifted by the shared selection tier when selected or hovered.
    fn draw_card(
        ctx: &mut PaintCtx<'_>,
        (card, m): (Rect, Metrics),
        window: &SwitcherWindow,
        state: ui::chrome::SelectionState,
        previews: &mut dyn WindowThumbnails,
    ) {
        let (scene, transform, text_renderer, theme) =
            (&mut *ctx.scene, ctx.transform, &mut *ctx.text, ctx.theme);
        let tokens = &theme.tokens;
        let colors = &tokens.colors;
        ui::chrome::selection_tier(theme, state).paint(scene, transform, card, m.card_radius);
        let well = Rect::new(
            card.x0 + m.inset,
            card.y0 + m.inset,
            card.x1 - m.inset,
            card.y0 + m.inset + m.well_h,
        );
        let live = draw_preview_well(
            scene,
            transform,
            theme,
            PreviewWell {
                rect: well,
                radius: m.well_radius,
                icon: m.icon,
            },
            window,
            previews,
        );

        // The title line, led by the app mark when the well shows the
        // window instead of the mark.
        let gap = f64::from(tokens.spacing.s1);
        let lead = if live { m.mark + gap } else { 0.0 };
        let title_w = (card.width() - 2.0 * m.inset - lead).max(0.0);
        let title = text_renderer.elide_to_width_style(
            &window.title,
            f32_of_f64(title_w),
            &theme.text(TextRole::Label),
        );
        let (w, h) = text_renderer.measure_style(&title, None, &theme.text(TextRole::Label));
        let line_top = well.y1 + m.inset;
        let line_w = lead + f64::from(w);
        let x0 = card.center().x - line_w * 0.5;
        if live {
            let mark_top = line_top + (f64::from(h) - m.mark) / 2.0;
            ui::widget::draw_app_mark(
                scene,
                transform,
                Rect::new(x0, mark_top, x0 + m.mark, mark_top + m.mark),
                window.app.icon.as_ref(),
                window.app.kind,
                colors.text_primary,
            );
        }
        text_renderer.draw_style(
            scene,
            &title,
            if state.active {
                colors.text_primary
            } else {
                colors.text_dim
            },
            transform.pre_translate((x0 + lead, line_top).into()),
            None,
            &theme.text(TextRole::Label),
        );
    }
}

/// Where a window's preview well sits and how its stand-in mark is sized.
#[derive(Clone, Copy, Debug)]
pub struct PreviewWell {
    pub rect: Rect,
    pub radius: f64,
    /// Edge of the app mark shown when there is no live preview.
    pub icon: f64,
}

/// A 16:10 well on the window-card surface holding `window`'s live
/// preview, or its app mark when it has none (not rendered yet, or an app
/// that holds secrets). Shared by the switcher and the overview so a
/// window looks the same in both. Returns whether a preview was painted.
pub fn draw_preview_well(
    scene: &mut Scene,
    transform: Affine,
    theme: &Theme,
    well: PreviewWell,
    window: &SwitcherWindow,
    previews: &mut dyn WindowThumbnails,
) -> bool {
    let colors = &theme.tokens.colors;
    let well_rr = RoundedRect::from_rect(well.rect, well.radius);
    scene.fill(
        Fill::NonZero,
        transform,
        colors.surfaces.content,
        None,
        &well_rr,
    );
    let live = WindowSwitcher::preview_state(window, previews) == PreviewState::Live && {
        scene.push_layer(
            Fill::NonZero,
            vello::peniko::BlendMode::default(),
            1.0,
            transform,
            &well_rr,
        );
        let painted = previews
            .paint(window.address.tab, scene, transform, well.rect, theme)
            .is_some();
        scene.pop_layer();
        painted
    };
    scene.stroke(
        &vello::kurbo::Stroke::new(1.0),
        transform,
        colors.surfaces.content_edge,
        None,
        &well_rr,
    );
    if !live {
        let center = well.rect.center();
        let half = well.icon / 2.0;
        ui::widget::draw_app_mark(
            scene,
            transform,
            Rect::new(
                center.x - half,
                center.y - half,
                center.x + half,
                center.y + half,
            ),
            window.app.icon.as_ref(),
            window.app.kind,
            colors.on_content().ink,
        );
    }
    live
}

#[cfg(test)]
mod tests {
    use super::*;
    use tilo_desktop_model::{Strip, Tab, WindowGroup, WindowWidth, Workspace};

    fn tab(id: u64, app: &str) -> Tab {
        Tab {
            id,
            title: format!("{app} {id}"),
            tile: TileContent::Foreign {
                app_id: app.into(),
                surface_id: id,
            },
        }
    }

    fn group(id: u64, tabs: Vec<Tab>) -> WindowGroup {
        WindowGroup {
            id,
            active_tab: tabs.first().map_or(0, |tab| tab.id),
            tabs,
            width: WindowWidth::Normal,
            maximized: false,
            focus_history: Vec::new(),
        }
    }

    /// One strip of four single-tab groups (tabs 1..=4), group 10 focused.
    fn tree() -> DesktopTree {
        let workspace = WorkspaceId(1);
        let mut tree = DesktopTree {
            active: workspace,
            workspaces: vec![Workspace::new(workspace, "Home")],
            ..Default::default()
        };
        let strip = StripId(100);
        tree.workspaces[0].strips = vec![Strip {
            id: strip,
            name: String::new(),
            window_groups: vec![
                group(10, vec![tab(1, "a")]),
                group(20, vec![tab(2, "b")]),
                group(30, vec![tab(3, "a")]),
                group(40, vec![tab(4, "c")]),
            ],
        }];
        tree.active_strip.insert(workspace, strip);
        tree.set_active_group(strip, 10);
        tree
    }

    fn focus(tree: &mut DesktopTree, group: u64) {
        let strip = tree.workspaces[0].strips[0].id;
        tree.set_active_group(strip, group);
    }

    fn order(switcher: &WindowSwitcher, tree: &DesktopTree) -> Vec<u64> {
        switcher
            .ordered_windows(tree, &PlainWindows)
            .iter()
            .map(|window| window.address.tab)
            .collect()
    }

    #[test]
    fn unvisited_windows_follow_the_strip_from_the_focused_one() {
        let mut tree = tree();
        focus(&mut tree, 30);
        let switcher = WindowSwitcher::default();
        assert_eq!(order(&switcher, &tree), [3, 4, 1, 2]);
    }

    #[test]
    fn recency_orders_the_windows_that_were_focused() {
        let mut tree = tree();
        let mut switcher = WindowSwitcher::default();
        for group in [20, 40, 10] {
            focus(&mut tree, group);
            switcher.observe(&tree);
        }
        assert_eq!(order(&switcher, &tree), [1, 4, 2, 3]);
    }

    #[test]
    fn each_workspace_keeps_its_own_recency() {
        let mut tree = tree();
        let mut switcher = WindowSwitcher::default();
        focus(&mut tree, 40);
        switcher.observe(&tree);
        let home = tree.active;
        let away = WorkspaceId(2);
        let mut other = tree.workspaces[0].clone();
        other.id = away;
        other.strips[0].id = StripId(200);
        tree.workspaces.push(other);
        tree.active = away;
        switcher.observe(&tree);
        tree.active = home;
        assert_eq!(order(&switcher, &tree).first(), Some(&4));
    }

    #[test]
    fn a_quick_alt_tab_goes_back_and_holding_reveals_after_the_delay() {
        let mut tree = tree();
        let mut switcher = WindowSwitcher::default();
        focus(&mut tree, 20);
        switcher.observe(&tree);
        focus(&mut tree, 10);
        let now = Instant::now();
        assert!(switcher.begin(&tree, &PlainWindows, SwitcherScope::Workspace, false, now));
        assert!(!switcher.is_revealed(now));
        assert!(switcher.is_revealed(now + REVEAL_DELAY));
        let outcome = switcher.key(
            &Key::Named(NamedKey::Alt),
            ElementState::Released,
            ModifiersState::empty(),
        );
        assert!(matches!(outcome, SwitcherOutcome::Commit(address) if address.tab == 2));
        assert!(!switcher.is_active());
    }

    #[test]
    fn the_app_scope_walks_only_the_focused_apps_windows() {
        let tree = tree();
        let mut switcher = WindowSwitcher::default();
        assert!(switcher.begin(
            &tree,
            &PlainWindows,
            SwitcherScope::FocusedApp,
            false,
            Instant::now()
        ));
        assert_eq!(switcher.selected().map(|w| w.address.tab), Some(3));
        switcher.key(
            &Key::Character("`".into()),
            ElementState::Pressed,
            ModifiersState::ALT,
        );
        assert_eq!(switcher.selected().map(|w| w.address.tab), Some(1));
        let mut lone = tree;
        focus(&mut lone, 40);
        assert!(
            !switcher.begin(
                &lone,
                &PlainWindows,
                SwitcherScope::FocusedApp,
                false,
                Instant::now()
            ),
            "an app with one window has nothing to switch to"
        );
    }

    #[test]
    fn cards_stay_inside_the_display_and_page_to_the_selection() {
        let m = Metrics::from_tokens(&Theme::dark().tokens);
        let host = Rect::new(0.0, 0.0, 800.0, 600.0);
        let few = geometry(host, 3, 0, m);
        assert_eq!(few.cards.len(), 3);
        assert!(
            few.cards
                .iter()
                .all(|(_, card)| few.panel.contains(card.center()))
        );
        let many = geometry(host, 200, 150, m);
        assert!(many.cards.len() < 200, "not every window fits");
        assert!(many.cards.iter().any(|(index, _)| *index == 150));
        assert!(host.contains(many.panel.origin()));
        assert!(many.panel.x1 <= host.x1 && many.panel.y1 <= host.y1);
    }
    /// The panel and every card on it stay inside the display at every
    /// size, from a phone-class window to the desktop, whatever the
    /// window count: a host too small for one full card gets a smaller
    /// card, never a panel that runs off the screen.
    #[test]
    fn the_switcher_panel_fits_every_host() {
        for theme in [Theme::dark(), Theme::dark().with_text_scale(1.5)] {
            let m = Metrics::from_tokens(&theme.tokens);
            for (w, h) in [
                (240.0, 160.0),
                (320.0, 480.0),
                (480.0, 300.0),
                (640.0, 400.0),
                (800.0, 600.0),
                (1280.0, 800.0),
                (1472.0, 920.0),
            ] {
                let host = Rect::new(0.0, 0.0, w, h);
                for count in [1, 3, 12] {
                    for selected in [0, count - 1] {
                        let g = geometry(host, count, selected, m);
                        let inside = |r: Rect| {
                            r.x0 >= host.x0 - 1e-9
                                && r.y0 >= host.y0 - 1e-9
                                && r.x1 <= host.x1 + 1e-9
                                && r.y1 <= host.y1 + 1e-9
                        };
                        assert!(
                            inside(g.panel),
                            "panel {:?} leaves {host:?} with {count} windows",
                            g.panel
                        );
                        assert!(!g.cards.is_empty(), "a card is always on screen");
                        assert!(
                            g.cards.iter().any(|(index, _)| *index == selected),
                            "the selected window is on the page"
                        );
                        for (index, card) in &g.cards {
                            assert!(
                                g.panel.contains(card.origin())
                                    && card.x1 <= g.panel.x1 + 1e-9
                                    && card.y1 <= g.panel.y1 + 1e-9,
                                "card {index} {card:?} leaves the panel {:?} on {host:?}",
                                g.panel
                            );
                        }
                        assert_eq!(
                            g.caption.height() > 0.0,
                            g.cards.len() < count,
                            "a position line only when the windows are paged"
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn closing_an_unselected_candidate_preserves_the_selected_window_and_removes_its_card() {
        let mut tree = tree();
        let mut switcher = WindowSwitcher::default();
        assert!(switcher.begin(
            &tree,
            &PlainWindows,
            SwitcherScope::Workspace,
            false,
            Instant::now()
        ));
        assert_eq!(switcher.selected().map(|w| w.address.tab), Some(2));
        tree.workspaces[0].strips[0].window_groups.remove(0);
        focus(&mut tree, 30);
        switcher.observe(&tree);
        assert_eq!(switcher.selected().map(|w| w.address.tab), Some(2));
        assert!(
            !switcher
                .session
                .as_ref()
                .expect("remaining candidates")
                .windows
                .iter()
                .any(|window| window.address.tab == 1),
            "the closed candidate must not remain painted or accessible"
        );
        assert!(
            matches!(switcher.modifiers_changed(ModifiersState::empty()),
            SwitcherOutcome::Commit(address) if address.tab == 2)
        );
    }

    #[test]
    fn closing_the_selected_candidate_cancels_before_alt_release() {
        let mut tree = tree();
        let mut switcher = WindowSwitcher::default();
        assert!(switcher.begin(
            &tree,
            &PlainWindows,
            SwitcherScope::Workspace,
            false,
            Instant::now()
        ));
        tree.workspaces[0].strips[0].window_groups.remove(1);
        switcher.observe(&tree);
        assert!(
            !switcher.is_active(),
            "a closed target must not remain armed"
        );
        assert_eq!(
            switcher.modifiers_changed(ModifiersState::empty()),
            SwitcherOutcome::Ignored
        );
        assert_eq!(
            switcher.key(
                &Key::Named(NamedKey::Enter),
                ElementState::Pressed,
                ModifiersState::ALT
            ),
            SwitcherOutcome::Ignored
        );
    }

    #[test]
    fn moving_the_selected_candidate_updates_its_focus_address() {
        let mut tree = tree();
        let mut switcher = WindowSwitcher::default();
        assert!(switcher.begin(
            &tree,
            &PlainWindows,
            SwitcherScope::Workspace,
            false,
            Instant::now()
        ));
        let moved = tree.workspaces[0].strips[0]
            .window_groups
            .remove(1)
            .tabs
            .remove(0);
        tree.workspaces[0].strips[0].window_groups[1]
            .tabs
            .push(moved);
        switcher.observe(&tree);
        assert!(
            matches!(switcher.modifiers_changed(ModifiersState::empty()),
            SwitcherOutcome::Commit(address) if address.tab == 2 && address.group == 30)
        );
    }
    #[test]
    fn switcher_live_candidate_capture_matrix_is_written() {
        let out = std::env::var_os("TILO_SWITCHER_LIVE_SHOTS").map_or_else(
            || std::path::PathBuf::from("target/switcher-live-candidates"),
            std::path::PathBuf::from,
        );
        std::fs::create_dir_all(&out).expect("capture output");
        for (name, theme, width, height) in [
            ("dark", Theme::dark(), 960_u32, 540_u32),
            ("light", Theme::light(), 960, 540),
            ("high-contrast", Theme::high_contrast(), 960, 540),
            ("large-text", Theme::dark().with_text_scale(2.0), 800, 600),
            (
                "ui-text-scaled",
                Theme::dark().with_ui_scale(1.5).with_text_scale(1.3),
                960,
                640,
            ),
            ("narrow", Theme::dark().with_text_scale(2.0), 320, 480),
        ] {
            let mut tree = tree();
            for (group, title) in tree.workspaces[0].strips[0]
                .window_groups
                .iter_mut()
                .zip(["Terminal", "Files", "Browser", "Reader"])
            {
                group.tabs[0].title = title.to_owned();
            }
            let mut switcher = WindowSwitcher::default();
            assert!(switcher.begin(
                &tree,
                &PlainWindows,
                SwitcherScope::Workspace,
                false,
                Instant::now()
            ));
            switcher.elapse(REVEAL_DELAY + Duration::from_millis(20));
            tree.workspaces[0].strips[0].window_groups.remove(2);
            switcher.observe(&tree);
            let viewport = Rect::new(0.0, 0.0, f64::from(width), f64::from(height));
            let mut scene = Scene::new();
            switcher.draw(
                &mut scene,
                Affine::IDENTITY,
                &mut TextRenderer::new(),
                &theme,
                viewport,
                Instant::now(),
                &mut NoThumbnails,
            );
            ui::offscreen::OffscreenRenderer::new(width, height).render_png(
                &scene,
                theme.tokens.colors.surfaces.base,
                out.join(format!("{name}-other-candidate-closed.png")),
            );
        }
    }
    #[test]
    fn a_press_on_a_removed_card_waits_for_the_replacement_frame() {
        let mut tree = tree();
        let mut switcher = WindowSwitcher::default();
        assert!(switcher.begin(
            &tree,
            &PlainWindows,
            SwitcherScope::Workspace,
            false,
            Instant::now()
        ));
        switcher.elapse(REVEAL_DELAY + Duration::from_millis(20));
        let viewport = Rect::new(0.0, 0.0, 960.0, 600.0);
        switcher.draw(
            &mut Scene::new(),
            Affine::IDENTITY,
            &mut TextRenderer::new(),
            &Theme::dark(),
            viewport,
            Instant::now(),
            &mut NoThumbnails,
        );
        let old_card = switcher.card_rect(2).expect("painted candidate");
        let old_point = Point::new(old_card.x0 + old_card.width() / 4.0, old_card.center().y);
        tree.workspaces[0].strips[0].window_groups.remove(2);
        switcher.observe(&tree);
        assert_eq!(
            switcher.pointer_pressed(old_point, Instant::now()),
            SwitcherOutcome::Consumed,
            "the old Browser pixels must not activate another window"
        );
        assert!(switcher.is_active());
        assert_eq!(switcher.selected().map(|w| w.address.tab), Some(2));
        switcher.draw(
            &mut Scene::new(),
            Affine::IDENTITY,
            &mut TextRenderer::new(),
            &Theme::dark(),
            viewport,
            Instant::now(),
            &mut NoThumbnails,
        );
        let selected = switcher.card_rect(1).expect("repainted selection").center();
        assert!(matches!(switcher.pointer_pressed(selected, Instant::now()),
            SwitcherOutcome::Commit(address) if address.tab == 2));
    }
}
