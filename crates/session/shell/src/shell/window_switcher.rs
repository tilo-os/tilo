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
//! painting and semantic tree. The overlay host routes input here and
//! focuses whatever a session commits to.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use vello::Scene;
use vello::kurbo::{Affine, Point, Rect, RoundedRect};
use vello::peniko::Fill;
use winit::event::ElementState;
use winit::keyboard::{Key, ModifiersState, NamedKey};

use ui::a11y::{AccessNode, AccessState, Role};
use ui::i18n::ArgValue;
use ui::icons::AppImage;
use ui::text_render::{TextRenderer, TextWeight};
use ui::theme::Theme;
use ui::widget::AppKind as UiAppKind;

use crate::shell::i18n::{t, t_args};
use crate::shell::state::{GroupId, StripId, TileContent, WorkspaceId, WorkspaceTree};

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
    pub fn of(tile: &TileContent) -> Self {
        let kind = crate::shell::tile_app_kind(tile);
        match tile {
            TileContent::Foreign { app_id, .. } => Self {
                key: format!("app:{app_id}"),
                kind,
                icon: crate::app_icons::for_tile(tile),
            },
            TileContent::LocalApp { app_id, .. } | TileContent::PeerWindow { app_id, .. } => Self {
                key: format!("app:{app_id}"),
                kind,
                icon: crate::app_icons::for_window_app_id(app_id),
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
}

/// Which windows a session walks.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SwitcherScope {
    /// Every window in the workspace (Alt+Tab).
    Workspace,
    /// The focused window's application only (Alt+Grave).
    FocusedApp,
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
fn workspace_windows(tree: &WorkspaceTree) -> (Vec<SwitcherWindow>, Option<usize>) {
    let desktop = &tree.desktop;
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
                    app: WindowApp::of(&tab.tile),
                });
            }
        }
    }
    (windows, current)
}

/// The focused tab of the active workspace, cheaply: no titles or icons.
fn focused_tab(tree: &WorkspaceTree) -> Option<(WorkspaceId, u64)> {
    let desktop = &tree.desktop;
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
    pub fn observe(&mut self, tree: &WorkspaceTree) {
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
        if self.recent.len() > tree.desktop.workspaces.len() {
            self.recent
                .retain(|id, _| tree.desktop.workspaces.iter().any(|ws| ws.id == *id));
        }
    }

    /// The active workspace's windows, focused one first, then by recency.
    /// Windows never focused follow in strip order from the focused one, so
    /// on a fresh desktop Alt+Tab steps to the next window along the strip.
    pub fn ordered_windows(&self, tree: &WorkspaceTree) -> Vec<SwitcherWindow> {
        let (windows, current) = workspace_windows(tree);
        let count = windows.len();
        let start = current.unwrap_or(0);
        let mut rank: HashMap<u64, usize> = HashMap::new();
        if let Some(recent) = tree
            .desktop
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
    scope: SwitcherScope,
    windows: Vec<SwitcherWindow>,
    selected: usize,
    opened: Instant,
}

/// Sizes of the panel, all read off the theme's tokens.
#[derive(Clone, Copy, Debug)]
struct Metrics {
    icon: f64,
    card_w: f64,
    card_h: f64,
    gap: f64,
    pad: f64,
    margin: f64,
    radius: f64,
    card_radius: f64,
    title_size: f32,
    caption_size: f32,
    caption_h: f64,
}

impl Metrics {
    fn from_tokens(tokens: &ui::tokens::Tokens) -> Self {
        let spacing = &tokens.spacing;
        let scale = &tokens.typography.scale;
        let icon = f64::from(spacing.s12);
        let title_size = scale.sm;
        let caption_size = scale.base;
        let pad_in = f64::from(spacing.s3);
        let title_h = f64::from(title_size) * 1.4;
        Self {
            icon,
            card_w: icon + f64::from(spacing.s16) + f64::from(spacing.s4),
            card_h: pad_in + icon + f64::from(spacing.s2) + title_h + pad_in,
            gap: f64::from(spacing.s2),
            pad: f64::from(spacing.s4),
            margin: f64::from(spacing.s8),
            radius: f64::from(tokens.radii.xl),
            card_radius: f64::from(tokens.radii.lg),
            title_size,
            caption_size,
            caption_h: f64::from(caption_size) * 1.4 + f64::from(spacing.s3),
        }
    }
}

/// Where everything of an open switcher sits.
#[derive(Clone, Debug, Default, PartialEq)]
struct Geometry {
    panel: Rect,
    /// `(window index, card)` for the page on screen.
    cards: Vec<(usize, Rect)>,
    caption: Rect,
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

#[expect(clippy::cast_possible_truncation, reason = "screen pixels fit f32")]
fn f32_of(value: f64) -> f32 {
    value as f32
}

/// Cards in rows, centred on the display, with the selected window's page
/// on screen when there are more windows than fit.
fn geometry(host: Rect, count: usize, selected: usize, m: Metrics) -> Geometry {
    if count == 0 {
        return Geometry::default();
    }
    let pitch_x = m.card_w + m.gap;
    let pitch_y = m.card_h + m.gap;
    let avail_w = (host.width() - 2.0 * (m.margin + m.pad) + m.gap).max(pitch_x);
    let avail_h = (host.height() - 2.0 * (m.margin + m.pad) - m.caption_h + m.gap).max(pitch_y);
    let columns = fit_count(avail_w, pitch_x).min(count);
    let rows_fit = fit_count(avail_h, pitch_y);
    let page = columns.saturating_mul(rows_fit).max(1);
    let first = selected.checked_div(page).unwrap_or(0).saturating_mul(page);
    let shown = count.saturating_sub(first).min(page);
    let rows = shown.div_ceil(columns).max(1);
    let grid_w = usize_to_f64(columns) * pitch_x - m.gap;
    let grid_h = usize_to_f64(rows) * pitch_y - m.gap;
    let panel_w = grid_w + 2.0 * m.pad;
    let panel_h = grid_h + m.caption_h + 2.0 * m.pad;
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
        panel.y1 - m.pad - m.caption_h,
        panel.x1 - m.pad,
        panel.y1 - m.pad,
    );
    Geometry {
        panel,
        cards,
        caption,
    }
}

/// The Alt+Tab switcher: recency history plus at most one open session.
pub struct WindowSwitcher {
    history: FocusHistory,
    session: Option<Session>,
    host_rect: Rect,
    metrics: Metrics,
    hover: Option<usize>,
}

impl Default for WindowSwitcher {
    fn default() -> Self {
        Self {
            history: FocusHistory::default(),
            session: None,
            host_rect: Rect::ZERO,
            metrics: Metrics::from_tokens(&Theme::dark().tokens),
            hover: None,
        }
    }
}

fn is_grave(key: &Key) -> bool {
    matches!(key, Key::Character(text) if matches!(text.as_str(), "`" | "~"))
}

impl WindowSwitcher {
    /// Note which window has focus now, so recency stays true whichever
    /// path moved it (click, keyboard, search, an app).
    pub fn observe(&mut self, tree: &WorkspaceTree) {
        self.history.observe(tree);
    }

    /// The windows Alt+Tab would walk right now, in order.
    pub fn ordered_windows(&self, tree: &WorkspaceTree) -> Vec<SwitcherWindow> {
        self.history.ordered_windows(tree)
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
        self.is_active() && !self.is_revealed(now)
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
        tree: &WorkspaceTree,
        scope: SwitcherScope,
        backwards: bool,
        now: Instant,
    ) -> bool {
        self.observe(tree);
        let mut windows = self.history.ordered_windows(tree);
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
            scope,
            windows,
            selected,
            opened: now,
        });
        self.hover = None;
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
        self.session
            .take()
            .and_then(|session| session.windows.get(session.selected).map(|w| w.address))
            .map_or(SwitcherOutcome::Cancelled, SwitcherOutcome::Commit)
    }

    fn cancel(&mut self) -> SwitcherOutcome {
        self.hover = None;
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

    /// The panel covers `point`.
    pub fn panel_contains(&self, point: Point, now: Instant) -> bool {
        self.is_revealed(now) && self.geometry().panel.contains(point)
    }

    /// Pointer motion: hovering a card selects it, as it does in every
    /// desktop's switcher, so a release commits what the pointer rests on.
    pub fn pointer_moved(&mut self, point: Point, now: Instant) -> SwitcherOutcome {
        if !self.is_revealed(now) {
            return SwitcherOutcome::Ignored;
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
    ) {
        self.host_rect = host;
        self.metrics = Metrics::from_tokens(&theme.tokens);
        if !self.is_revealed(now) {
            return;
        }
        // Theme type tokens already carry the user's text scale (HIG §7a).
        let renderer_scale = text_renderer.text_scale();
        text_renderer.set_text_scale(1.0);
        self.draw_panel(scene, transform, text_renderer, theme);
        text_renderer.set_text_scale(renderer_scale);
    }

    fn draw_panel(
        &self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
    ) {
        let Some(session) = self.session.as_ref() else {
            return;
        };
        let tokens = &theme.tokens;
        let colors = &tokens.colors;
        let m = self.metrics;
        let geometry = self.geometry();
        let panel = geometry.panel;

        // A Panel (HIG §9.1): the shell's floating-surface ground, edge and
        // elevation, like the overview and the palette.
        ui::surface::shadow(scene, transform, panel, m.radius, &tokens.elevation.e4);
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
                self.draw_card(scene, transform, text_renderer, theme, *card, window, state);
            }
        }

        // The chosen window's whole title, which a card can only elide.
        if let Some(window) = session.windows.get(session.selected) {
            let caption = if session.windows.len() > geometry.cards.len() {
                t_args(
                    "window-switcher-caption-paged",
                    &[
                        ("title", ArgValue::Text(window.title.clone())),
                        (
                            "position",
                            ArgValue::Number(usize_to_f64(session.selected.saturating_add(1))),
                        ),
                        (
                            "count",
                            ArgValue::Number(usize_to_f64(session.windows.len())),
                        ),
                    ],
                )
            } else {
                window.title.clone()
            };
            let text = text_renderer.elide_to_width_weighted(
                &caption,
                m.caption_size,
                f32_of(geometry.caption.width()),
                TextWeight::Semibold,
            );
            let (w, h) =
                text_renderer.measure_weighted(&text, m.caption_size, None, TextWeight::Semibold);
            text_renderer.draw_text_weighted(
                scene,
                &text,
                m.caption_size,
                colors.text_primary,
                transform.pre_translate(
                    (
                        geometry.caption.center().x - f64::from(w) / 2.0,
                        geometry.caption.center().y - f64::from(h) / 2.0,
                    )
                        .into(),
                ),
                None,
                TextWeight::Semibold,
            );
        }
    }

    /// One window's card: its app mark over its elided title, lifted by
    /// the shared selection tier when selected or hovered.
    #[expect(
        clippy::too_many_arguments,
        reason = "a paint step taking the caller's scene, theme and laid-out card"
    )]
    fn draw_card(
        &self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        card: Rect,
        window: &SwitcherWindow,
        state: ui::chrome::SelectionState,
    ) {
        let tokens = &theme.tokens;
        let colors = &tokens.colors;
        let m = self.metrics;
        let tier = ui::chrome::selection_tier(theme, state);
        let rr = RoundedRect::from_rect(card, m.card_radius);
        if tier.fill.components[3] > 0.0 {
            scene.fill(Fill::NonZero, transform, tier.fill, None, &rr);
        }
        if let Some(border) = tier.border {
            scene.stroke(
                &vello::kurbo::Stroke::new(1.0),
                transform,
                border,
                None,
                &rr,
            );
        }
        if let Some(glow) = tier.inner_glow {
            scene.stroke(
                &vello::kurbo::Stroke::new(1.0),
                transform,
                glow,
                None,
                &RoundedRect::from_rect(card.inset(-1.0), (m.card_radius - 1.0).max(0.0)),
            );
        }
        let inner_top = card.y0 + f64::from(tokens.spacing.s3);
        let icon = Rect::new(
            card.center().x - m.icon / 2.0,
            inner_top,
            card.center().x + m.icon / 2.0,
            inner_top + m.icon,
        );
        ui::widget::draw_app_mark(
            scene,
            transform,
            icon,
            window.app.icon.as_ref(),
            window.app.kind,
            colors.text_primary,
        );
        let title_w = card.width() - 2.0 * f64::from(tokens.spacing.s2);
        let title = text_renderer.elide_to_width(&window.title, m.title_size, f32_of(title_w));
        let (w, _) = text_renderer.measure(&title, m.title_size, None);
        text_renderer.draw_text(
            scene,
            &title,
            m.title_size,
            if state.active {
                colors.text_primary
            } else {
                colors.text_dim
            },
            transform.pre_translate(
                (
                    card.center().x - f64::from(w) / 2.0,
                    icon.y1 + f64::from(tokens.spacing.s2),
                )
                    .into(),
            ),
            None,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shell::state::{FreshId as _, Strip, Tab, WindowGroup, WindowWidth};

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
        }
    }

    /// One strip of four single-tab groups (tabs 1..=4), group 10 focused.
    fn tree() -> WorkspaceTree {
        let mut tree = WorkspaceTree::bootstrap_default();
        let strip = StripId::new();
        let workspace = tree.desktop.workspaces[0].id;
        tree.desktop.workspaces[0].strips = vec![Strip {
            id: strip,
            name: String::new(),
            window_groups: vec![
                group(10, vec![tab(1, "a")]),
                group(20, vec![tab(2, "b")]),
                group(30, vec![tab(3, "a")]),
                group(40, vec![tab(4, "c")]),
            ],
        }];
        tree.desktop.active_strip.insert(workspace, strip);
        tree.desktop.set_active_group(strip, 10);
        tree
    }

    fn focus(tree: &mut WorkspaceTree, group: u64) {
        let strip = tree.desktop.workspaces[0].strips[0].id;
        tree.desktop.set_active_group(strip, group);
    }

    fn order(switcher: &WindowSwitcher, tree: &WorkspaceTree) -> Vec<u64> {
        switcher
            .ordered_windows(tree)
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
        let home = tree.desktop.active;
        let away = WorkspaceId::new();
        let mut other = tree.desktop.workspaces[0].clone();
        other.id = away;
        other.strips[0].id = StripId::new();
        tree.desktop.workspaces.push(other);
        tree.desktop.active = away;
        switcher.observe(&tree);
        tree.desktop.active = home;
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
        assert!(switcher.begin(&tree, SwitcherScope::Workspace, false, now));
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
        assert!(switcher.begin(&tree, SwitcherScope::FocusedApp, false, Instant::now()));
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
            !switcher.begin(&lone, SwitcherScope::FocusedApp, false, Instant::now()),
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
}
