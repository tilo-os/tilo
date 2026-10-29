//! Where everything in the overview stands: the panel of workspaces, the
//! picture of the workspace you are in, and what a point lands on.
//!
//! Paint, hit-testing and the semantic tree all read these, so nothing is
//! drawn in one place and pressed in another.

use std::collections::HashMap;

use tilo_desktop_model::{StripId, TabId, WorkspaceId};
use ui::TextRole;
use ui::num::{f32_of_f64, f64_of_usize};
use ui::theme::Theme;
use vello::kurbo::{Point, Rect, Size};
use vello::peniko::Color;

use super::OverviewPanel;
use crate::overview::RoomCard;
use crate::overview_picture::{GridMetrics, StripPlace, band_height, layout_strips};
use crate::window_switcher::{PreviewSource, SwitcherWindow, WindowAddress};

/// Most share rows drawn before the rest collapse into "and N more": the
/// panel says whether things leave this room, the permissions pane has the
/// full list.
pub(super) const SHARES_MAX_ROWS: usize = 4;

/// Width to height of a window's tab that is not in front, beside it.
const BEHIND_TAB_ASPECT: f64 = 0.75;

/// How far the panel rises as it opens, matching the workspace-settings
/// modal so the desktop's floating surfaces arrive the same way.
pub const PANEL_RISE_PX: f64 = 20.0;

/// The scale the panel settles from as it opens.
pub const PANEL_START_SCALE: f64 = 0.97;

/// `(alpha, dy, scale)` for the panel of workspaces at the overview's
/// `progress` (already on the zoom's spring, so the exit is the same motion
/// reversed): it rises the last 20px and settles from 0.97 as the windows
/// arrive in the picture beside it.
pub fn panel_curve(progress: f32) -> (f32, f64, f64) {
    let o = f64::from(progress.clamp(0.0, 1.0));
    let alpha = f32_of_f64(o);
    let dy = PANEL_RISE_PX * (1.0 - o);
    let scale = PANEL_START_SCALE + (1.0 - PANEL_START_SCALE) * o;
    (alpha, dy, scale)
}

/// The overview's density, resolved from the theme once per layout pass so
/// hit-testing (which has no theme in hand) reads the numbers paint does.
///
/// Every vertical extent is a line height at the size the text is drawn
/// with, plus spacing steps: larger text grows the panel instead of
/// putting its lines on top of one another.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct Metrics {
    /// Inset from the display's edge, and the smallest gap the panel keeps
    /// from its top and bottom (`s12`).
    pub margin: f64,
    /// Widest the panel gets alone: the readable width.
    pub panel_width: f64,
    /// Width of the panel beside the picture of this workspace.
    pub column_width: f64,
    /// The narrowest picture worth putting beside the panel.
    pub min_picture_width: f64,
    /// Below this the overview is a page filling the display.
    pub sheet_floor: f64,
    pub pad: f64,
    pub section_gap: f64,
    pub panel_radius: f64,
    pub row_radius: f64,
    /// Line height of the current room's name; faces centre on it.
    pub name_lh: f64,
    /// The "you are here" block: the name line, `s1`, the access line and
    /// `s6` of air.
    pub here_h: f64,
    pub caption_h: f64,
    pub caption_text_dy: f64,
    pub share_row_h: f64,
    pub room_row_h: f64,
    pub room_row_gap: f64,
    pub row_pad: f64,
    pub chip_h: f64,
    /// Gutter above the picture inside the panel (`s3`).
    pub window_gap: f64,
    /// The air closing the "you are here" block (`s6`); the picture takes
    /// its place.
    pub here_air: f64,
    pub s1: f64,
    pub s2: f64,
    pub s3: f64,
    pub s4: f64,
    /// A face in a stack of faces.
    pub face: f64,
    pub icon_md: f64,
    pub icon_lg: f64,
}

impl Metrics {
    pub(super) fn for_theme(theme: &Theme) -> Self {
        let tokens = &theme.tokens;
        let spacing = &tokens.spacing;
        let lh = |role| f64::from(tokens.typography.style(role).line_height_px());
        let s1 = f64::from(spacing.s1);
        let s2 = f64::from(spacing.s2);
        let s3 = f64::from(spacing.s3);
        let s4 = f64::from(spacing.s4);
        let s6 = f64::from(spacing.s6);
        let name_lh = lh(TextRole::Display);
        Self {
            margin: f64::from(spacing.s12),
            panel_width: f64::from(tokens.metrics.readable_width),
            // The column and the picture keep the base density's widths:
            // larger text grows rows, not the split of the display.
            column_width: f64::from(ui::tokens::Metrics::BASE.overview_panel_width),
            min_picture_width: f64::from(ui::tokens::Metrics::BASE.overview_picture_min_width),
            sheet_floor: f64::from(tokens.metrics.panel_width_sheet),
            pad: s4,
            section_gap: s4,
            panel_radius: f64::from(tokens.radii.panel),
            row_radius: f64::from(tokens.radii.control),
            name_lh,
            here_h: name_lh + s1 + lh(TextRole::Body) + s6,
            caption_h: s2 + lh(TextRole::Caption) + s1,
            caption_text_dy: s2,
            share_row_h: lh(TextRole::Body) + s3,
            room_row_h: lh(TextRole::Heading) + s1 + lh(TextRole::Caption) + 2.0 * s3,
            room_row_gap: s2,
            row_pad: s4,
            chip_h: lh(TextRole::Caption) + s2,
            window_gap: s3,
            here_air: s6,
            s1,
            s2,
            s3,
            s4,
            face: f64::from(tokens.metrics.avatar_inline),
            icon_md: f64::from(tokens.metrics.icon_md),
            icon_lg: f64::from(tokens.metrics.icon_lg),
        }
    }
}

impl Default for Metrics {
    /// The default theme's density, for a panel not laid out yet.
    fn default() -> Self {
        Self::for_theme(&Theme::default())
    }
}

/// One strip of the workspace you are in, as the picture shows it.
#[derive(Clone, Debug)]
pub(super) struct PictureStrip {
    pub id: StripId,
    pub name: String,
    /// This machine names and moves it.
    pub renamable: bool,
    /// The strip with focus.
    pub focused: bool,
    /// Every tab of every window, left to right, each window's tabs
    /// together.
    pub windows: Vec<SwitcherWindow>,
    /// Each one's size on the display.
    pub sizes: Vec<Size>,
    /// Each one is a tab of the same window as the one before it.
    pub joined: Vec<bool>,
    /// Each one is the tab its window shows.
    pub front: Vec<bool>,
    /// The people in each one: their initial and colour.
    pub faces: Vec<Vec<(char, Color)>>,
}

/// The picture of the workspace you are in, laid out.
#[derive(Clone, Debug, Default)]
pub(super) struct Picture {
    pub strips: Vec<PictureStrip>,
    pub places: Vec<StripPlace>,
}

impl Picture {
    /// Every pictured window in reading order, with its strip's index and
    /// its index in the strip.
    pub fn windows(&self) -> impl Iterator<Item = (usize, usize, &SwitcherWindow)> {
        self.strips.iter().enumerate().flat_map(|(si, strip)| {
            strip
                .windows
                .iter()
                .enumerate()
                .map(move |(wi, window)| (si, wi, window))
        })
    }

    /// A pictured window's bounds: its picture over its title.
    pub fn tile(&self, si: usize, wi: usize) -> Option<Rect> {
        let place = self.places.get(si)?;
        Some(place.wells.get(wi)?.union(*place.titles.get(wi)?))
    }

    /// Strip `si`'s window whose tab is `tab`.
    pub fn find(&self, tab: TabId) -> Option<(usize, usize, &SwitcherWindow)> {
        self.windows()
            .find(|(_, _, window)| window.address.tab == tab)
    }
}

/// A strip's icon buttons at the end of its name line.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum StripControl {
    Rename,
    MoveUp,
    MoveDown,
}

impl StripControl {
    pub fn icon(self) -> ui::icons::Icon {
        match self {
            Self::Rename => ui::icons::Icon::Pencil,
            Self::MoveUp => ui::icons::Icon::ArrowUp,
            Self::MoveDown => ui::icons::Icon::ArrowDown,
        }
    }

    pub fn name(self) -> String {
        crate::i18n::t(match self {
            Self::Rename => "overview-strip-rename",
            Self::MoveUp => "overview-strip-move-up",
            Self::MoveDown => "overview-strip-move-down",
        })
    }

    pub fn key(self) -> &'static str {
        match self {
            Self::Rename => "rename",
            Self::MoveUp => "move-up",
            Self::MoveDown => "move-down",
        }
    }
}

impl OverviewPanel {
    /// Take the theme's density, and keep the scroll and the selection
    /// valid for it.
    pub(super) fn resolve_layout(&mut self, theme: &Theme) {
        let metrics = Metrics::for_theme(theme);
        let grid = GridMetrics::for_theme(theme);
        self.reduce_motion = theme.reduce_motion;
        if metrics == self.metrics && grid == self.grid && self.rect == self.laid_out {
            return;
        }
        let had_layout = self.laid_out.area() > 0.0;
        self.metrics = metrics;
        self.grid = grid;
        self.laid_out = self.rect;
        self.scroll_y = self.scroll_y.clamp(0.0, self.max_scroll_y());
        if had_layout {
            self.scroll_selection_into_view();
        }
        self.bump();
    }

    /// The room this session is in, by its place among the cards.
    pub(super) fn here_index(&self) -> Option<usize> {
        self.desk.cards.iter().position(|card| card.is_current)
    }

    pub(super) fn here_card(&self) -> Option<&RoomCard> {
        self.desk.cards.iter().find(|card| card.is_current)
    }

    /// How many share rows the "shared out of here" list draws: none when
    /// the room shares nothing.
    pub(super) fn share_rows(card: &RoomCard) -> usize {
        let shown = card.shares.len().min(SHARES_MAX_ROWS);
        shown.saturating_add(usize::from(card.shares.len() > shown))
    }

    pub(super) fn shares_height(&self) -> f64 {
        let rows = self.here_card().map_or(0, Self::share_rows);
        if rows == 0 {
            return 0.0;
        }
        self.metrics.caption_h + f64_of_usize(rows) * self.metrics.share_row_h
    }

    /// Height of the "you are here" block with its picture and share list.
    pub(super) fn here_block_height(&self) -> f64 {
        self.metrics.here_h + self.windows_row_height() + self.shares_height()
    }

    /// Rows of the "go to" list: every room but the one you are in, then
    /// "New workspace" and "Join a workspace…".
    pub(super) fn list_slots(&self) -> usize {
        let rooms = self
            .desk
            .cards
            .len()
            .saturating_sub(usize::from(self.here_index().is_some()));
        rooms
            .saturating_add(usize::from(self.can_add()))
            .saturating_add(usize::from(self.desk.can_join))
    }

    /// Height of the list if nothing clipped it.
    pub(super) fn content_height(&self) -> f64 {
        let m = self.metrics;
        let k = f64_of_usize(self.list_slots());
        (k * (m.room_row_h + m.room_row_gap) - m.room_row_gap).max(0.0)
    }

    /// The display is too small for a floating panel: the overview is a
    /// page filling it.
    pub(super) fn uses_full_window(&self) -> bool {
        self.rect.width().min(self.rect.height()) < self.metrics.sheet_floor
    }

    /// The picture sits beside the panel of workspaces: the display is wide
    /// enough for both. Otherwise the panel holds it.
    pub(super) fn beside_picture(&self) -> bool {
        let m = self.metrics;
        !self.uses_full_window()
            && self.rect.width() >= m.column_width + m.min_picture_width + m.margin * 3.0
    }

    pub(super) fn panel_width(&self) -> f64 {
        let area = self.rect;
        let m = self.metrics;
        if self.uses_full_window() {
            area.width().max(0.0)
        } else if self.beside_picture() {
            m.column_width
        } else {
            m.panel_width.min(area.width() - m.margin * 2.0).max(0.0)
        }
    }

    /// On a constrained display the whole body scrolls: a pinned header
    /// must never take the list's last reachable row.
    pub(super) fn scrolls_whole_panel(&self) -> bool {
        let m = self.metrics;
        self.uses_full_window()
            || self.content_rect().height() - self.here_block_height() - m.section_gap - m.caption_h
                < m.room_row_h
    }

    pub(super) fn content_top(&self) -> f64 {
        self.content_rect().y0
            - if self.scrolls_whole_panel() {
                self.scroll_y
            } else {
                0.0
            }
    }

    /// The panel: centred alone, at the display's trailing edge beside the
    /// picture, sized to what it has to say and never off screen.
    pub(super) fn panel_rect(&self) -> Rect {
        let m = self.metrics;
        let area = self.rect;
        if self.uses_full_window() {
            return area;
        }
        let w = self.panel_width();
        let natural = m.pad * 2.0
            + self.here_block_height()
            + m.section_gap
            + m.caption_h
            + self.content_height();
        let h = natural.min(area.height() - m.margin * 2.0).max(0.0);
        let cy = f64::midpoint(area.y0, area.y1);
        let cx = if self.beside_picture() {
            area.x1 - m.margin - w / 2.0
        } else {
            f64::midpoint(area.x0, area.x1)
        };
        Rect::new(cx - w / 2.0, cy - h / 2.0, cx + w / 2.0, cy + h / 2.0)
    }

    /// The band inside the panel that content is laid out in.
    pub(super) fn content_rect(&self) -> Rect {
        self.panel_rect().inset(-self.metrics.pad)
    }

    /// The "you are here" block: the current room's identity, access and
    /// people, and its picture when the panel holds it.
    pub(super) fn here_rect(&self) -> Rect {
        let c = self.content_rect();
        let top = self.content_top();
        Rect::new(
            c.x0,
            top,
            c.x1,
            top + self.metrics.here_h + self.windows_row_height(),
        )
    }

    pub(super) fn shares_top(&self) -> f64 {
        self.here_rect().y1
    }

    /// Y of the "Go to" caption.
    pub(super) fn list_caption_top(&self) -> f64 {
        self.content_top() + self.here_block_height() + self.metrics.section_gap
    }

    /// Y the first list row starts at, before scrolling.
    pub(super) fn list_top(&self) -> f64 {
        self.list_caption_top() + self.metrics.caption_h
    }

    /// The band the list scrolls inside.
    pub(super) fn list_band(&self) -> Rect {
        let content = self.content_rect();
        Rect::new(
            content.x0,
            self.list_top().max(content.y0),
            content.x1,
            content.y1,
        )
    }

    pub(super) fn viewport_height(&self) -> f64 {
        if self.scrolls_whole_panel() {
            self.content_rect().height().max(0.0)
        } else {
            (self.content_rect().y1 - self.list_top()).max(0.0)
        }
    }

    /// The list's overflow below its band; zero when every row fits.
    pub(super) fn max_scroll_y(&self) -> f64 {
        let m = self.metrics;
        let header = if self.scrolls_whole_panel() {
            self.here_block_height() + m.section_gap + m.caption_h
        } else {
            0.0
        };
        (header + self.content_height() - self.viewport_height()).max(0.0)
    }

    /// List slot `slot`, panned by the scroll.
    pub(super) fn list_slot_rect(&self, slot: usize) -> Rect {
        let c = self.content_rect();
        let m = self.metrics;
        let scroll = if self.scrolls_whole_panel() {
            0.0
        } else {
            self.scroll_y
        };
        let y0 = self.list_top() - scroll + f64_of_usize(slot) * (m.room_row_h + m.room_row_gap);
        Rect::new(c.x0, y0, c.x1, y0 + m.room_row_h)
    }

    /// The list slot room `idx` occupies: the room you are in has none (it
    /// is the block at the top), every other keeps its order.
    pub(super) fn slot_of(&self, idx: usize) -> Option<usize> {
        match self.here_index() {
            Some(here) if here == idx => None,
            Some(here) if idx > here => Some(idx.saturating_sub(1)),
            _ => Some(idx),
        }
    }

    /// Room `idx`: the "you are here" block for the room you are in, its
    /// list row for any other.
    pub fn row_rect(&self, idx: usize) -> Rect {
        match self.slot_of(idx) {
            None => self.here_rect(),
            Some(slot) => self.list_slot_rect(slot),
        }
    }

    fn rooms_in_list(&self) -> usize {
        self.desk
            .cards
            .len()
            .saturating_sub(usize::from(self.here_index().is_some()))
    }

    /// The "New workspace" row, after the last room.
    pub(super) fn add_rect(&self) -> Option<Rect> {
        self.can_add()
            .then(|| self.list_slot_rect(self.rooms_in_list()))
    }

    /// The "Join a workspace…" row, after "New workspace".
    pub(super) fn join_rect(&self) -> Option<Rect> {
        self.desk.can_join.then(|| {
            self.list_slot_rect(
                self.rooms_in_list()
                    .saturating_add(usize::from(self.can_add())),
            )
        })
    }

    /// `rect` where it can be seen and pressed in the list's band.
    fn in_band(&self, rect: Rect) -> Option<Rect> {
        let band = self.list_band();
        if band.y0 >= band.y1 {
            return None;
        }
        let shown = rect.intersect(band);
        (shown.width() > 0.0 && shown.height() > 0.0).then_some(shown)
    }

    /// The "New workspace" row where it can be pressed.
    pub fn add_hit_rect(&self) -> Option<Rect> {
        self.add_rect().and_then(|rect| self.in_band(rect))
    }

    /// The "Join a workspace…" row where it can be pressed.
    pub fn join_hit_rect(&self) -> Option<Rect> {
        self.join_rect().and_then(|rect| self.in_band(rect))
    }

    /// The Leave control on room `idx` when it is a site's workspace, at
    /// its row's trailing edge.
    pub(super) fn leave_rect(&self, idx: usize) -> Option<Rect> {
        let card = self.desk.cards.get(idx)?;
        if !self.desk.leavable.contains(&card.id) {
            return None;
        }
        let row = self.row_rect(idx);
        // Wide enough for "Leave" in every language the desktop speaks.
        let w = 112.0;
        let h = 28.0_f64;
        if self.slot_of(idx).is_none() {
            let x1 = self.content_rect().x1;
            return Some(Rect::new(x1 - w, row.y1 - h, x1, row.y1));
        }
        let pad = self.metrics.row_pad;
        let h = h.min(row.height() - 8.0).max(0.0);
        let cy = row.center().y;
        Some(Rect::new(
            row.x1 - pad - w,
            cy - h / 2.0,
            row.x1 - pad,
            cy + h / 2.0,
        ))
    }

    /// Pan the list so the keyboard's selection is on screen.
    pub(super) fn scroll_selection_into_view(&mut self) {
        let Some(idx) = self.selection else {
            return;
        };
        let m = self.metrics;
        let whole = self.scrolls_whole_panel();
        let header = if whole {
            self.here_block_height() + m.section_gap + m.caption_h
        } else {
            0.0
        };
        let count = self.desk.cards.len();
        let slot = if idx >= count {
            // New workspace, then Join.
            let past = idx.saturating_sub(count);
            Some(self.rooms_in_list().saturating_add(past))
        } else {
            self.slot_of(idx)
        };
        let (top, height) = match slot {
            Some(slot) => (
                header + f64_of_usize(slot) * (m.room_row_h + m.room_row_gap),
                m.room_row_h,
            ),
            None if whole => (0.0, m.here_h + self.windows_row_height()),
            None => return,
        };
        let bottom = top + height;
        let view = self.viewport_height();
        let mut y = self.scroll_y;
        if top < y {
            y = top;
        } else if bottom > y + view {
            y = bottom - view;
        }
        self.scroll_y = y.clamp(0.0, self.max_scroll_y());
    }

    /// The room at `p`, by its place among the cards. A row panned out of
    /// the list's band cannot be hit: a click never lands on a room nobody
    /// can see.
    pub fn hit_room(&self, p: Point) -> Option<usize> {
        if !self.content_rect().contains(p) {
            return None;
        }
        let band = self.list_band();
        (0..self.desk.cards.len()).find(|&i| {
            let r = self.row_rect(i);
            let visible =
                self.slot_of(i).is_none() || (r.y0 >= band.y0 - 0.5 && r.y1 <= band.y1 + 0.5);
            visible && r.contains(p)
        })
    }

    // ---- The picture of the workspace you are in ----

    /// Whether the overview pictures this workspace: it has strips and the
    /// host shows its windows.
    pub(super) fn shows_windows(&self) -> bool {
        !self.desk.hide_windows && self.here_card().is_some_and(|card| !card.strips.is_empty())
    }

    /// Height the picture adds to the "here" block when the panel holds
    /// it; nothing beside it.
    pub(super) fn windows_row_height(&self) -> f64 {
        if self.beside_picture() || !self.shows_windows() {
            return 0.0;
        }
        let strips = self.here_card().map_or(0, |card| card.strips.len());
        self.metrics.window_gap + band_height(strips, &self.grid, self.grid.band)
    }

    /// Where the picture lies: beside the panel, or in the "here" block
    /// inside it (panned with the panel's scroll).
    fn grid_area(&self, strips: usize) -> Rect {
        let m = self.metrics;
        if self.beside_picture() {
            let area = self.rect;
            let panel = self.panel_rect();
            return Rect::new(
                area.x0 + m.margin,
                area.y0 + m.margin,
                (panel.x0 - m.margin).max(area.x0 + m.margin),
                area.y1 - m.margin,
            );
        }
        let content = self.content_rect();
        let top = self.here_rect().y0 + m.here_h - m.here_air + m.window_gap;
        Rect::new(
            content.x0,
            top,
            content.x1,
            top + band_height(strips, &self.grid, self.grid.band),
        )
    }

    /// Each strip of the workspace you are in, as the picture shows it.
    fn picture_strips(&self) -> Vec<PictureStrip> {
        let Some(card) = self.here_card() else {
            return Vec::new();
        };
        let focused = card
            .strips
            .iter()
            .find(|strip| strip.is_current)
            .or_else(|| card.strips.first())
            .map(|strip| strip.id);
        // A window not laid out on the host stands at its share of the
        // display.
        let display = self.rect.size();
        card.strips
            .iter()
            .map(|strip| {
                let mut picture = PictureStrip {
                    id: strip.id,
                    name: strip.name.clone(),
                    renamable: self.desk.renamable.contains(&strip.id),
                    focused: Some(strip.id) == focused,
                    windows: Vec::new(),
                    sizes: Vec::new(),
                    joined: Vec::new(),
                    front: Vec::new(),
                    faces: Vec::new(),
                };
                for group in &strip.groups {
                    let size = self
                        .desk
                        .group_sizes
                        .get(&group.id)
                        .copied()
                        .unwrap_or_else(|| {
                            let share = if group.width_permille == 0 {
                                0.5
                            } else {
                                f64::from(group.width_permille) / 1000.0
                            };
                            Size::new(display.width * share, display.height * 0.9)
                        });
                    // A window's other tabs stand beside it, narrower: they
                    // are the same window, not windows of their own.
                    let behind = Size::new(
                        (size.height * BEHIND_TAB_ASPECT).min(size.width),
                        size.height,
                    );
                    let only = group.tabs.len() == 1;
                    for (index, tab) in group.tabs.iter().enumerate() {
                        let front = tab.is_active || only;
                        picture.windows.push(SwitcherWindow {
                            address: WindowAddress {
                                strip: strip.id,
                                group: group.id,
                                tab: tab.id,
                            },
                            title: tab.title.clone(),
                            app: tab.app.clone(),
                            preview: PreviewSource::Unavailable,
                        });
                        picture.sizes.push(if front { size } else { behind });
                        picture.joined.push(index > 0);
                        picture.front.push(front);
                        picture.faces.push(
                            tab.people
                                .iter()
                                .map(|person| {
                                    let initial = person
                                        .name
                                        .chars()
                                        .next()
                                        .unwrap_or('?')
                                        .to_ascii_uppercase();
                                    (initial, self.person_color(&person.id))
                                })
                                .collect(),
                        );
                    }
                }
                picture
            })
            .collect()
    }

    /// The colour person `id` wears: their cursor's.
    pub(super) fn person_color(&self, id: &str) -> Color {
        super::paint::face_colour(&self.desk.colors, id)
    }

    /// Each person's colour, as a face stack reads them.
    pub(super) fn colors(&self) -> &HashMap<String, (u8, u8, u8)> {
        &self.desk.colors
    }

    /// The picture, laid out.
    pub(super) fn picture(&self) -> Picture {
        if !self.shows_windows() {
            return Picture::default();
        }
        let strips = self.picture_strips();
        let area = self.grid_area(strips.len());
        let sizes: Vec<Vec<Size>> = strips.iter().map(|s| s.sizes.clone()).collect();
        let joined: Vec<Vec<bool>> = strips.iter().map(|s| s.joined.clone()).collect();
        let band = (!self.beside_picture()).then_some(self.grid.band);
        let places = layout_strips(area, &sizes, &joined, &self.grid, band);
        Picture { strips, places }
    }

    /// Where pictured windows can be seen and pressed: the display beside
    /// the panel, the panel's band when it holds them.
    pub(super) fn grid_clip(&self) -> Rect {
        if self.beside_picture() {
            self.rect
        } else {
            self.content_rect()
        }
    }

    /// Everything the overview occupies: the panel and the picture.
    pub(super) fn bounds(&self) -> Rect {
        let panel = self.panel_rect();
        if !self.beside_picture() {
            return panel;
        }
        self.picture()
            .places
            .iter()
            .fold(panel, |all, place| all.union(place.bounds()))
    }

    /// The pictured windows in reading order.
    pub(super) fn row_windows(&self) -> Vec<SwitcherWindow> {
        self.picture()
            .windows()
            .map(|(_, _, window)| window.clone())
            .collect()
    }

    /// Bounds of the `index`th pictured window in reading order.
    #[cfg(test)]
    pub(super) fn window_tile_rect(&self, index: usize) -> Rect {
        let picture = self.picture();
        picture
            .windows()
            .nth(index)
            .and_then(|(si, wi, _)| picture.tile(si, wi))
            .unwrap_or(Rect::ZERO)
    }

    pub(super) fn window_list_rect(&self) -> Option<Rect> {
        let picture = self.picture();
        picture
            .windows()
            .filter_map(|(si, wi, _)| picture.tile(si, wi))
            .reduce(|all, tile| all.union(tile))
    }

    /// The pictured window under `p`.
    pub fn window_at(&self, p: Point) -> Option<TabId> {
        if !self.grid_clip().contains(p) {
            return None;
        }
        let picture = self.picture();
        picture
            .windows()
            .find(|(si, wi, _)| picture.tile(*si, *wi).is_some_and(|tile| tile.contains(p)))
            .map(|(_, _, window)| window.address.tab)
    }

    /// The strip of the picture whose place holds `p`.
    pub(super) fn strip_at(&self, picture: &Picture, p: Point) -> Option<StripId> {
        if !self.grid_clip().contains(p) {
            return None;
        }
        picture
            .places
            .iter()
            .position(|place| place.bounds().contains(p))
            .and_then(|si| picture.strips.get(si))
            .map(|strip| strip.id)
    }

    /// The room a carried window would land in: any room but the one you
    /// are in, under the pointer or finger.
    pub(super) fn drop_target(&self) -> Option<usize> {
        let press = self
            .press
            .filter(crate::overview::OverviewPress::is_dragging)?;
        self.hit_room(press.at)
            .filter(|room| Some(*room) != self.here_index())
    }

    /// The strip the keyboard is in: the selected window's, else the strip
    /// with focus.
    pub(super) fn keyboard_strip(&self, picture: &Picture) -> Option<usize> {
        self.window_selection
            .and_then(|tab| picture.find(tab).map(|(si, _, _)| si))
            .or_else(|| picture.strips.iter().position(|strip| strip.focused))
    }

    /// The icon buttons at the end of strip `si`'s name line: renaming and
    /// moving are for strips this machine names.
    pub(super) fn strip_controls(&self, picture: &Picture, si: usize) -> Vec<(StripControl, Rect)> {
        let (Some(strip), Some(place)) = (picture.strips.get(si), picture.places.get(si)) else {
            return Vec::new();
        };
        if !strip.renamable {
            return Vec::new();
        }
        let mut controls = vec![StripControl::Rename];
        if si > 0 {
            controls.push(StripControl::MoveUp);
        }
        if si.saturating_add(1) < picture.strips.len() {
            controls.push(StripControl::MoveDown);
        }
        let g = self.grid;
        let gap = self.metrics.s1;
        let y0 = place.label.y0 + (place.label.height() - g.control - gap) / 2.0;
        let mut x1 = place.label.x1;
        let mut placed: Vec<(StripControl, Rect)> = controls
            .into_iter()
            .rev()
            .map(|control| {
                let rect = Rect::new(x1 - g.control, y0, x1, y0 + g.control);
                x1 = rect.x0 - gap;
                (control, rect)
            })
            .collect();
        placed.reverse();
        placed
    }

    /// Whether strip `si`'s controls show: the pointer is over the strip
    /// or the keyboard is in it, and its name is not being typed.
    pub(super) fn strip_controls_shown(&self, picture: &Picture, si: usize) -> bool {
        let hovered = self.cursor.is_some_and(|p| {
            picture
                .places
                .get(si)
                .is_some_and(|place| place.bounds().contains(p))
        });
        let keyboard = self.window_selection.is_some() && self.keyboard_strip(picture) == Some(si);
        let naming = self.naming.as_ref().is_some_and(|naming| {
            picture
                .strips
                .get(si)
                .is_some_and(|strip| strip.id == naming.strip)
        });
        (hovered || keyboard) && !naming
    }

    /// Where strip `si`'s name field is while its name is typed: its name
    /// line, short of the controls.
    pub(super) fn naming_field(&self, picture: &Picture, si: usize) -> Option<Rect> {
        let place = picture.places.get(si)?;
        let g = self.grid;
        let reserve = (g.control + self.metrics.s1) * 3.0;
        let x1 = (place.label.x1 - reserve).max(place.label.x0 + g.control * 4.0);
        let h = g.control;
        let y0 = place.label.y0 + (place.label.height() - h) / 2.0;
        Some(Rect::new(
            place.label.x0,
            y0,
            x1.min(place.label.x1),
            y0 + h,
        ))
    }

    /// The workspace `id` names, by its place among the cards.
    pub(super) fn card_index(&self, id: WorkspaceId) -> Option<usize> {
        self.desk.cards.iter().position(|card| card.id == id)
    }
}
