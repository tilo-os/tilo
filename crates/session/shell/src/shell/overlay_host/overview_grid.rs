//! The overview's picture of this workspace: every strip, top to bottom,
//! with its windows left to right in the order the strip holds them, each
//! at its share of the size it has on the display.
//!
//! Each window is drawn on its own, over the panel of workspaces (whose
//! recording is kept, [`ShellOverlayHost::draw_overview_panel`]), so it can
//! fly between where it stands on its strip and its place in the picture on
//! the overview's zoom ([`ui::motion::Zoom::OVERVIEW`]): the picture of a
//! window starts exactly over the window it pictures, from the buffer the
//! compositor already holds, so the first frame after Super+Tab is the
//! windows lifting off their strips, and nothing is captured or rendered to
//! open it.
//!
//! The strips are the switcher's own: one name per [`StripId`], renamed and
//! moved through the same chrome commands ([`ChromeState::rename_strip`],
//! [`ChromeState::move_strip`]), so the overview, the strip switcher and the
//! action door always agree on which strip is which and what it is called.
//!
//! [`ChromeState::rename_strip`]: crate::shell::chrome_state::ChromeState::rename_strip
//! [`ChromeState::move_strip`]: crate::shell::chrome_state::ChromeState::move_strip

use std::collections::HashMap;

use tilo_desktop::text_entry::{self, Edit, Mods};
use tilo_desktop::window_switcher::{PreviewState, PreviewWell, SwitcherWindow, WindowSwitcher};
use ui::TextRole;
use ui::num::f64_of_usize;
use ui::presence::PresenceCtx;
use ui::text_render::{TextRenderer, TextWeight};
use ui::text_selection::Selection;
use ui::theme::Theme;
use ui::widget::{Button, ControlSize, FieldFace, IconButton, TextInput, Widget as _};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect, Size};
use vello::peniko::{BlendMode, Color, Fill};
use winit::event::{ElementState, WindowEvent};
use winit::keyboard::{Key, ModifiersState, NamedKey};

use super::ShellOverlayHost;
use crate::shell::i18n::t;
use crate::shell::search::providers::actions::{STRIP_NAME_MAX_CHARS, valid_strip_name};
use crate::shell::state::StripId;

/// Width of the panel of workspaces beside the picture.
pub(super) const COLUMN_WIDTH_PX: f64 = ui::tokens::Metrics::BASE.overview_panel_width as f64;
/// The narrowest picture worth putting beside the panel; a narrower display
/// holds the picture inside the panel instead.
const MIN_PICTURE_WIDTH_PX: f64 = ui::tokens::Metrics::BASE.overview_picture_min_width as f64;
/// A window is pictured at most at this share of its size on the display:
/// a workspace of one window still reads as a picture, not as the window.
const MAX_SCALE: f64 = 0.5;
/// A pictured window carried to another workspace is left this faint in
/// its place.
const CARRIED_ALPHA: f32 = 0.35;
/// Width to height of a window's tab that is not in front, beside it.
const BEHIND_TAB_ASPECT: f64 = 0.75;
/// Most faces drawn on one window before the picture says nothing more.
const TILE_FACES_MAX: usize = 3;

/// Spacing of the picture, from the theme.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct GridMetrics {
    /// A strip's name line, with the air below it.
    pub label_h: f64,
    /// Air between a pictured window and its title.
    pub title_gap: f64,
    /// Line height of a window's title.
    pub title_lh: f64,
    /// Air between two strips.
    pub row_gap: f64,
    /// Air between two windows of a strip.
    pub window_gap: f64,
    /// Air between two tabs of one window, so they read as one window's.
    pub tab_gap: f64,
    /// Height of a strip's windows when the picture sits inside the panel.
    pub band: f64,
    /// Side of a strip's icon buttons.
    pub control: f64,
    /// Corner radius of a pictured window at rest.
    pub radius: f64,
    /// Corner radius of a window on its strip, where a picture starts.
    pub window_radius: f64,
}

impl GridMetrics {
    pub(super) fn for_theme(theme: &Theme) -> Self {
        let tokens = &theme.tokens;
        let spacing = &tokens.spacing;
        let lh = |role| f64::from(tokens.typography.style(role).line_height_px());
        let control = ControlSize::Small.height(theme);
        Self {
            label_h: control.max(lh(TextRole::Label)) + f64::from(spacing.s2),
            title_gap: f64::from(spacing.s1),
            title_lh: lh(TextRole::Caption),
            row_gap: f64::from(spacing.s6),
            window_gap: f64::from(spacing.s4),
            tab_gap: f64::from(spacing.s1),
            band: f64::from(spacing.s16) * 1.5,
            control,
            radius: f64::from(tokens.radii.control),
            window_radius: f64::from(ui::tokens::Radii::WINDOW),
        }
    }

    fn title_h(&self) -> f64 {
        self.title_gap + self.title_lh
    }
}

/// Where one strip sits in the picture.
#[derive(Clone, Debug, Default, PartialEq)]
pub(super) struct StripPlace {
    /// The strip's name line, across the picture.
    pub label: Rect,
    /// Each window's picture, in strip order.
    pub wells: Vec<Rect>,
    /// Each window's title line, under its picture.
    pub titles: Vec<Rect>,
    /// The slot a strip with no windows shows instead.
    pub empty: Option<Rect>,
}

impl StripPlace {
    /// Everything the strip occupies: its name line and its windows.
    pub(super) fn bounds(&self) -> Rect {
        self.wells
            .iter()
            .chain(&self.titles)
            .chain(&self.empty)
            .fold(self.label, |all, rect| all.union(*rect))
    }
}

/// Height of a picture of `strips` strips whose windows are `band` tall.
pub(super) fn band_height(strips: usize, m: &GridMetrics, band: f64) -> f64 {
    if strips == 0 {
        return 0.0;
    }
    let n = f64_of_usize(strips);
    n * (m.label_h + band + m.title_h()) + (n - 1.0) * m.row_gap
}

/// Lay out `strips`, each a list of its windows' sizes on the display, in
/// `area` (`joined[s][i]` puts strip `s`'s window `i` against the one before
/// it, as a tab of the same window): one scale for the whole picture so windows keep their sizes
/// relative to each other, a smaller one only for a strip too wide for the
/// picture. With `band`, every strip's windows are that tall and the picture
/// grows down from the top of `area`; without, it fills `area`'s height,
/// centred.
pub(super) fn layout_strips(
    area: Rect,
    strips: &[Vec<Size>],
    joined: &[Vec<bool>],
    m: &GridMetrics,
    band: Option<f64>,
) -> Vec<StripPlace> {
    if strips.is_empty() || area.width() <= 0.0 {
        return Vec::new();
    }
    let gap = |strip: usize, window: usize| {
        if joined
            .get(strip)
            .and_then(|joined| joined.get(window))
            .copied()
            .unwrap_or(false)
        {
            m.tab_gap
        } else {
            m.window_gap
        }
    };
    let gaps = |strip: usize, count: usize| (1..count).map(|i| gap(strip, i)).sum::<f64>();
    let tallest = |sizes: &Vec<Size>| sizes.iter().map(|s| s.height).fold(0.0, f64::max);
    let reference = strips.iter().map(tallest).fold(0.0, f64::max).max(1.0);
    // A strip with no windows takes a row as tall as the others' windows.
    let heights: Vec<f64> = strips
        .iter()
        .map(|sizes| {
            Some(tallest(sizes))
                .filter(|h| *h > 0.0)
                .unwrap_or(reference)
        })
        .collect();
    let fixed = band_height(strips.len(), m, 0.0);
    let fill_scale = ((area.height() - fixed) / heights.iter().sum::<f64>()).clamp(0.0, MAX_SCALE);
    let rows: Vec<(f64, f64)> = strips
        .iter()
        .zip(&heights)
        .enumerate()
        .map(|(si, (sizes, height))| {
            let height_scale = band.map_or(fill_scale, |band| band / height);
            let widths: f64 = sizes.iter().map(|s| s.width).sum();
            let gaps = gaps(si, sizes.len());
            let width_scale = if widths > 0.0 {
                ((area.width() - gaps) / widths).max(0.0)
            } else {
                f64::INFINITY
            };
            (height_scale.min(width_scale), height * height_scale)
        })
        .collect();
    let row_width = |si: usize, sizes: &Vec<Size>, (scale, band_h): (f64, f64)| {
        if sizes.is_empty() {
            return band_h * 16.0 / 10.0;
        }
        sizes.iter().map(|s| s.width * scale).sum::<f64>() + gaps(si, sizes.len())
    };
    let widest = strips
        .iter()
        .zip(&rows)
        .enumerate()
        .map(|(si, (sizes, row))| row_width(si, sizes, *row))
        .fold(0.0, f64::max)
        .min(area.width());
    let x0 = area.x0 + ((area.width() - widest) / 2.0).max(0.0);
    let total = fixed + rows.iter().map(|(_, band_h)| band_h).sum::<f64>();
    let mut y = if band.is_some() {
        area.y0
    } else {
        area.y0 + ((area.height() - total) / 2.0).max(0.0)
    };
    let mut places = Vec::with_capacity(strips.len());
    for (si, (sizes, (scale, band_h))) in strips.iter().zip(rows).enumerate() {
        let label = Rect::new(x0, y, x0 + widest, y + m.label_h);
        y = label.y1;
        let mut place = StripPlace {
            label,
            ..StripPlace::default()
        };
        if sizes.is_empty() {
            let w = (band_h * 16.0 / 10.0).min(widest);
            place.empty = Some(Rect::new(x0, y, x0 + w, y + band_h));
        }
        let mut x = x0;
        for (wi, size) in sizes.iter().enumerate() {
            if wi > 0 {
                x += gap(si, wi);
            }
            let well = Rect::new(x, y, x + size.width * scale, y + size.height * scale);
            let title_y = well.y1 + m.title_gap;
            place.wells.push(well);
            place
                .titles
                .push(Rect::new(well.x0, title_y, well.x1, title_y + m.title_lh));
            x = well.x1;
        }
        y += band_h + m.title_h() + m.row_gap;
        places.push(place);
    }
    places
}

/// One strip of this workspace, as the overview pictures it.
#[derive(Clone, Debug)]
pub(super) struct OverviewStrip {
    pub id: StripId,
    pub name: String,
    /// This machine writes the strip's name and place.
    pub renamable: bool,
    /// The strip with focus.
    pub focused: bool,
    /// Every tab of every window group, left to right as the strip shows
    /// them, each group's tabs together.
    pub windows: Vec<SwitcherWindow>,
    /// Each window's content size on the display, in the same order.
    pub sizes: Vec<Size>,
    /// Each one is a tab of the same window as the one before it.
    pub joined: Vec<bool>,
    /// Each one is the tab its window shows, which stands where its picture
    /// starts; the other tabs fade in where they rest.
    pub front: Vec<bool>,
    /// The people in each window, in the same order: their initial and
    /// colour.
    pub faces: Vec<Vec<(char, Color)>>,
}

/// The picture: the strips and where each one sits.
#[derive(Clone, Debug, Default)]
pub(super) struct OverviewPicture {
    pub strips: Vec<OverviewStrip>,
    pub places: Vec<StripPlace>,
}

impl OverviewPicture {
    /// Every pictured window in reading order, with its strip's index and
    /// its index in the strip.
    fn windows(&self) -> impl Iterator<Item = (usize, usize, &SwitcherWindow)> {
        self.strips.iter().enumerate().flat_map(|(si, strip)| {
            strip
                .windows
                .iter()
                .enumerate()
                .map(move |(wi, window)| (si, wi, window))
        })
    }

    /// The bounds of a pictured window, its picture over its title.
    fn tile(&self, si: usize, wi: usize) -> Option<Rect> {
        let place = self.places.get(si)?;
        Some(place.wells.get(wi)?.union(*place.titles.get(wi)?))
    }
}

/// A strip's name being typed in the overview.
pub(crate) struct OverviewNaming {
    strip: StripId,
    text: String,
    selection: Selection,
    input: Option<TextInput>,
}

/// A strip's icon buttons, at the end of its name line.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum StripControl {
    Rename,
    MoveUp,
    MoveDown,
}

impl StripControl {
    fn icon(self) -> ui::icons::Icon {
        match self {
            Self::Rename => ui::icons::Icon::Pencil,
            Self::MoveUp => ui::icons::Icon::ArrowUp,
            Self::MoveDown => ui::icons::Icon::ArrowDown,
        }
    }

    fn name(self) -> String {
        t(match self {
            Self::Rename => "overview-strip-rename",
            Self::MoveUp => "overview-strip-move-up",
            Self::MoveDown => "overview-strip-move-down",
        })
    }
}

impl ShellOverlayHost {
    /// The picture sits beside the panel of workspaces: the display is wide
    /// enough for both. Otherwise the panel holds it, where the current
    /// workspace's block is.
    pub(super) fn overview_beside_picture(&self) -> bool {
        let area = self.overlay_area();
        !self.overview_uses_full_window()
            && area.width()
                >= COLUMN_WIDTH_PX + MIN_PICTURE_WIDTH_PX + self.overview_metrics.margin * 3.0
    }

    fn overview_grid_metrics(&self) -> GridMetrics {
        GridMetrics::for_theme(&self.overview_layout_theme)
    }

    /// The strips of this workspace and their windows, as the overview
    /// pictures them now.
    fn overview_strips(&self) -> Vec<OverviewStrip> {
        // Before the tree is read: the display's size stands in for a window
        // that is not laid out.
        let display = self.overview_output_rect();
        let fallback = Size::new(display.width() / 2.0, display.height() * 0.9);
        let Some(chrome) = self.chrome.as_ref() else {
            return Vec::new();
        };
        let Ok(state) = chrome.try_borrow() else {
            return Vec::new();
        };
        let local_user = state.active_local_user();
        let own = state.local_user_id.clone();
        let tree = state.tree();
        let Some(workspace) = tree.desktop.active_workspace() else {
            return Vec::new();
        };
        let focused = tree
            .desktop
            .active_strip
            .get(&workspace.id)
            .copied()
            .filter(|id| workspace.strips.iter().any(|s| s.id == *id))
            .or_else(|| workspace.strips.first().map(|s| s.id));
        // The switcher's own list of windows: the same titles, app marks
        // and picture sources Alt+Tab shows.
        let listed: HashMap<u64, SwitcherWindow> = self
            .layers
            .window_switcher
            .ordered_windows(&tree.desktop, &crate::shell::window_previews::ShellWindows)
            .into_iter()
            .map(|window| (window.address.tab, window))
            .collect();
        // Where each other person is: the tab their presence names.
        let people: Vec<(u64, char, Color)> =
            tilo_desktop::collaborators::collaborator_roster_rows(&tree.desktop, &local_user)
                .into_iter()
                .filter(|row| !row.invited && row.id != own && row.id != local_user)
                .filter_map(|row| {
                    let tab = workspace
                        .collaborators
                        .iter()
                        .chain(&tree.desktop.collaborators)
                        .find(|participant| participant.id == row.id)?
                        .active_tab?;
                    Some((tab, row.initial, row.color))
                })
                .collect();
        workspace
            .strips
            .iter()
            .map(|strip| {
                let row = self.group_row_ptrs.get(&strip.id).map(|&ptr| {
                    // SAFETY: see the field doc on `group_row_ptrs`; this is
                    // a read-only query under `&self`.
                    let row: &ui::widget::GroupRow = unsafe { &*ptr };
                    row
                });
                let mut windows = Vec::new();
                let mut sizes = Vec::new();
                let mut faces = Vec::new();
                let mut joined = Vec::new();
                let mut front = Vec::new();
                for group in
                    tilo_desktop::composition::display_ordered(&tree.desktop.outputs, strip)
                {
                    // The whole window, its bar included, as its picture
                    // shows it. Its other tabs stand beside it, narrower: they
                    // are the same window, not windows of their own.
                    let laid_out = row
                        .and_then(|row| row.groups.iter().find(|g| g.id == group.id))
                        .map(|g| g.get_rect())
                        .filter(|rect| rect.width() > 0.0 && rect.height() > 0.0);
                    let size = laid_out.map_or(fallback, |rect| rect.size());
                    let behind = Size::new(
                        (size.height * BEHIND_TAB_ASPECT).min(size.width),
                        size.height,
                    );
                    let mut first = true;
                    for tab in &group.tabs {
                        let Some(window) = listed.get(&tab.id).cloned() else {
                            continue;
                        };
                        sizes.push(if tab.id == group.active_tab {
                            size
                        } else {
                            behind
                        });
                        faces.push(
                            people
                                .iter()
                                .filter(|(on, _, _)| *on == tab.id)
                                .map(|(_, initial, color)| (*initial, *color))
                                .collect(),
                        );
                        joined.push(!first);
                        front.push(tab.id == group.active_tab);
                        windows.push(window);
                        first = false;
                    }
                }
                OverviewStrip {
                    id: strip.id,
                    name: strip.name.clone(),
                    renamable: tree.may_rename_strip(strip.id),
                    focused: Some(strip.id) == focused,
                    windows,
                    sizes,
                    joined,
                    front,
                    faces,
                }
            })
            .collect()
    }

    /// Where the picture lies: beside the panel, or in the current
    /// workspace's block inside it (panned with the panel's scroll).
    fn overview_grid_area(&self, strips: usize) -> Rect {
        let m = self.overview_metrics;
        if self.overview_beside_picture() {
            let area = self.overlay_area();
            let panel = self.overview_panel_rect();
            return Rect::new(
                area.x0 + m.margin,
                area.y0 + m.margin,
                (panel.x0 - m.margin).max(area.x0 + m.margin),
                area.y1 - m.margin,
            );
        }
        let content = self.overview_content_rect();
        let top = self.overview_here_rect().y0 + m.here_h - m.here_air + m.window_gap;
        let g = self.overview_grid_metrics();
        Rect::new(
            content.x0,
            top,
            content.x1,
            top + band_height(strips, &g, g.band),
        )
    }

    /// The picture, laid out.
    pub(super) fn overview_picture(&self) -> OverviewPicture {
        if !self.overview_shows_windows() {
            return OverviewPicture::default();
        }
        let strips = self.overview_strips();
        let g = self.overview_grid_metrics();
        let area = self.overview_grid_area(strips.len());
        let sizes: Vec<Vec<Size>> = strips.iter().map(|s| s.sizes.clone()).collect();
        let joined: Vec<Vec<bool>> = strips.iter().map(|s| s.joined.clone()).collect();
        let band = (!self.overview_beside_picture()).then_some(g.band);
        let places = layout_strips(area, &sizes, &joined, &g, band);
        OverviewPicture { strips, places }
    }

    /// Whether the overview pictures this workspace: it has strips, and the
    /// window switcher is not up over it.
    pub(super) fn overview_shows_windows(&self) -> bool {
        if self.layers.window_switcher.is_active() {
            return false;
        }
        self.chrome.as_ref().is_some_and(|chrome| {
            chrome.try_borrow().is_ok_and(|state| {
                state
                    .tree()
                    .desktop
                    .active_workspace()
                    .is_some_and(|workspace| !workspace.strips.is_empty())
            })
        })
    }

    /// Height the picture adds to the current workspace's block when the
    /// panel holds it; nothing when it sits beside the panel.
    pub(super) fn overview_windows_row_height(&self) -> f64 {
        if self.overview_beside_picture() || !self.overview_shows_windows() {
            return 0.0;
        }
        let strips = self.chrome.as_ref().map_or(0, |chrome| {
            chrome.try_borrow().map_or(0, |state| {
                state
                    .tree()
                    .desktop
                    .active_workspace()
                    .map_or(0, |workspace| workspace.strips.len())
            })
        });
        let g = self.overview_grid_metrics();
        self.overview_metrics.window_gap + band_height(strips, &g, g.band)
    }

    /// Where pictured windows can be seen and pressed: the display beside
    /// the panel, or the panel's visible band when it holds them.
    pub(super) fn overview_grid_clip(&self) -> Rect {
        if self.overview_beside_picture() {
            self.overview_output_rect()
        } else {
            self.overview_content_rect()
        }
    }

    /// Everything the overview occupies: the panel and the picture.
    pub(super) fn overview_bounds(&self) -> Rect {
        let panel = self.overview_panel_rect();
        if !self.overview_beside_picture() {
            return panel;
        }
        self.overview_picture()
            .places
            .iter()
            .fold(panel, |all, place| all.union(place.bounds()))
    }

    /// The windows the picture shows, in reading order: strip by strip, top
    /// to bottom, each strip's left to right.
    pub(super) fn overview_row_windows(&self) -> Vec<SwitcherWindow> {
        self.overview_picture()
            .windows()
            .map(|(_, _, window)| window.clone())
            .collect()
    }

    /// Bounds of the `index`th pictured window in reading order: its
    /// picture over its title. Paint, hit-testing and the semantic tree all
    /// read it.
    pub(super) fn overview_window_tile_rect(&self, index: usize) -> Rect {
        let picture = self.overview_picture();
        picture
            .windows()
            .nth(index)
            .and_then(|(si, wi, _)| picture.tile(si, wi))
            .unwrap_or(Rect::ZERO)
    }

    pub(super) fn overview_window_list_rect(&self) -> Option<Rect> {
        let picture = self.overview_picture();
        picture
            .windows()
            .filter_map(|(si, wi, _)| picture.tile(si, wi))
            .reduce(|all, tile| all.union(tile))
    }

    /// The pictured window under `p`, by its place in reading order.
    pub(super) fn hit_test_overview_window(&self, p: Point) -> Option<usize> {
        if !self.overview_grid_clip().contains(p) {
            return None;
        }
        let picture = self.overview_picture();
        picture
            .windows()
            .position(|(si, wi, _)| picture.tile(si, wi).is_some_and(|tile| tile.contains(p)))
    }

    /// The room a carried window would land in: any room but the one you
    /// are in, under the pointer or finger.
    pub(super) fn overview_drop_target(&self) -> Option<usize> {
        let press = self
            .overview_press
            .filter(tilo_desktop::overview::OverviewPress::is_dragging)?;
        self.hit_test_overview(press.at)
            .filter(|room| Some(*room) != self.overview_here_index())
    }

    /// Where the window group `group` stands on its strip now, where its
    /// picture starts and ends its flight: the whole window for a picture
    /// painted from the window as drawn (`whole`), its content for a client's
    /// buffer. `None` when it is not laid out.
    fn overview_window_source(&self, group: u64, whole: bool) -> Option<Rect> {
        self.group_row_ptrs
            .values()
            .find_map(|&ptr| {
                // SAFETY: see the field doc on `group_row_ptrs`; read-only.
                let row: &ui::widget::GroupRow = unsafe { &*ptr };
                let group = row.groups.iter().find(|g| g.id == group)?;
                let rect = if whole {
                    group.get_rect()
                } else {
                    group
                        .active_host()
                        .map_or_else(|| group.get_rect(), |host| host.get_rect())
                };
                Some(rect)
            })
            .filter(|rect| rect.width() > 0.0 && rect.height() > 0.0)
    }

    /// The pictures of shell-drawn windows for `windows`, painted once and
    /// kept while what they show is unchanged.
    fn take_overview_thumbnails(
        &mut self,
        windows: &[SwitcherWindow],
        text_renderer: &mut TextRenderer,
        theme: &Theme,
    ) -> (u64, crate::shell::window_previews::LiveThumbnails) {
        let revision = self
            .chrome
            .as_ref()
            .map_or(0, |chrome| chrome.borrow().revision);
        // Each client window's current frame, so a commit is drawn at once.
        let key = windows.iter().fold(revision, |key, window| {
            let seq = match &window.preview {
                tilo_desktop::window_switcher::PreviewSource::Client { surface } => {
                    crate::shell::window_previews::client_frame_seq(*surface)
                }
                _ => 0,
            };
            key.rotate_left(7) ^ window.address.tab.wrapping_mul(0x9E37_79B9_7F4A_7C15) ^ seq
        });
        match self.overview_thumbnails.take() {
            Some((cached, thumbnails)) if cached == key => (key, thumbnails),
            _ => (
                key,
                self.overview_live_thumbnails(windows, text_renderer, theme),
            ),
        }
    }

    /// The strip the keyboard is in: the one of the selected window, else
    /// the strip with focus.
    fn overview_keyboard_strip(&self, picture: &OverviewPicture) -> Option<usize> {
        self.overview_window_selection
            .and_then(|tab| {
                picture
                    .windows()
                    .find(|(_, _, window)| window.address.tab == tab)
                    .map(|(si, _, _)| si)
            })
            .or_else(|| picture.strips.iter().position(|strip| strip.focused))
    }

    /// The icon buttons at the end of strip `si`'s name line: renaming and
    /// moving are for strips this machine writes.
    fn overview_strip_controls(
        &self,
        picture: &OverviewPicture,
        si: usize,
    ) -> Vec<(StripControl, Rect)> {
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
        let g = self.overview_grid_metrics();
        let gap = f64::from(self.overview_layout_theme.tokens.spacing.s1);
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

    /// Whether strip `si`'s controls show: the pointer is over the strip,
    /// or the keyboard is in it.
    fn overview_strip_controls_shown(&self, picture: &OverviewPicture, si: usize) -> bool {
        let hovered = self.last_cursor.is_some_and(|p| {
            picture
                .places
                .get(si)
                .is_some_and(|place| place.bounds().contains(p))
        });
        let keyboard = self.overview_window_selection.is_some()
            && self.overview_keyboard_strip(picture) == Some(si);
        let naming = self.overview_naming.as_ref().is_some_and(|naming| {
            picture
                .strips
                .get(si)
                .is_some_and(|strip| strip.id == naming.strip)
        });
        (hovered || keyboard) && !naming
    }

    /// Where strip `si`'s name field is while its name is typed: its name
    /// line, short of the controls.
    fn overview_naming_field(&self, picture: &OverviewPicture, si: usize) -> Option<Rect> {
        let place = picture.places.get(si)?;
        let g = self.overview_grid_metrics();
        let reserve = (g.control + f64::from(self.overview_layout_theme.tokens.spacing.s1)) * 3.0;
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

    // -- drawing ---------------------------------------------------------

    /// Every pictured window at the zoom's `progress`: from its place on its
    /// strip (0) to its place in the picture (1), with each strip's name,
    /// its controls and the window being carried, if any.
    pub(super) fn draw_overview_tiles(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        progress: f64,
    ) {
        self.overview_row_drawn.clear();
        let picture = self.overview_picture();
        if picture.strips.is_empty() {
            return;
        }
        let p = progress.clamp(0.0, 1.0);
        let windows: Vec<SwitcherWindow> = picture.windows().map(|(_, _, w)| w.clone()).collect();
        let (key, mut thumbnails) = self.take_overview_thumbnails(&windows, text_renderer, theme);
        // In flight the windows cross the whole display; at rest they keep
        // to where the picture can be seen.
        let clip = if p < 1.0 {
            self.overview_output_rect()
        } else {
            self.overview_grid_clip()
        };
        scene.push_clip_layer(Fill::NonZero, transform, &clip);
        // Names, titles and controls arrive once the windows mostly have.
        let chrome = if theme.reduce_motion {
            p
        } else {
            ui::motion::stage(p, 0.5, 1.0)
        };
        for si in 0..picture.strips.len() {
            self.draw_overview_strip_label(
                &mut ui::paint::PaintCtx::new(scene, transform, text_renderer, theme),
                &picture,
                si,
                ui::motion::unit_opacity(chrome),
            );
            let (Some(strip), Some(place)) = (picture.strips.get(si), picture.places.get(si))
            else {
                continue;
            };
            if let Some(empty) = place.empty {
                Self::draw_overview_empty_strip(
                    &mut ui::paint::PaintCtx::new(scene, transform, text_renderer, theme),
                    empty,
                    ui::motion::unit_opacity(chrome),
                );
            }
            for (wi, window) in strip.windows.iter().enumerate() {
                let (Some(well), Some(title)) = (place.wells.get(wi), place.titles.get(wi)) else {
                    continue;
                };
                let tile = Tile {
                    window,
                    front: strip.front.get(wi).copied().unwrap_or(true),
                    well: *well,
                    title: *title,
                    faces: strip.faces.get(wi).map_or(&[][..], Vec::as_slice),
                };
                let live = self.draw_overview_tile(
                    &mut ui::paint::PaintCtx::new(scene, transform, text_renderer, theme),
                    &tile,
                    (p, chrome),
                    &mut thumbnails,
                );
                self.overview_row_drawn.push((
                    window.address.tab,
                    if live {
                        PreviewState::Live
                    } else {
                        WindowSwitcher::preview_state(window, &thumbnails)
                    },
                ));
            }
        }
        self.draw_overview_carried_window(scene, transform, theme, &picture, &mut thumbnails);
        scene.pop_layer();
        self.overview_thumbnails = Some((key, thumbnails));
    }

    /// One pictured window at `(progress, chrome)`: its picture flying from
    /// its strip, its title and faces arriving with the picture's chrome.
    /// Returns whether a live picture was drawn.
    fn draw_overview_tile(
        &self,
        ctx: &mut ui::paint::PaintCtx<'_>,
        tile: &Tile<'_>,
        (progress, chrome): (f64, f64),
        thumbnails: &mut crate::shell::window_previews::LiveThumbnails,
    ) -> bool {
        let (scene, transform, theme) = (&mut *ctx.scene, ctx.transform, ctx.theme);
        let g = GridMetrics::for_theme(theme);
        let tab = tile.window.address.tab;
        let source = (!theme.reduce_motion && tile.front)
            .then(|| {
                let painted = matches!(
                    tile.window.preview,
                    tilo_desktop::window_switcher::PreviewSource::Painted { .. }
                );
                self.overview_window_source(tile.window.address.group, painted)
            })
            .flatten();
        // A window that stands on its strip flies from there, its picture
        // exactly over it at the start; one that is not laid out (or under
        // reduced motion) fades in where it rests.
        let (rect, radius, alpha) = match source {
            Some(from) => (
                ui::motion::Zoom::rect_at(from, tile.well, progress),
                ui::motion::Zoom::radius_at(g.window_radius, g.radius, progress),
                1.0,
            ),
            None => (tile.well, g.radius, ui::motion::unit_opacity(progress)),
        };
        let carried = self
            .overview_press
            .filter(tilo_desktop::overview::OverviewPress::is_dragging)
            .and_then(|press| press.tab)
            == Some(tab);
        let alpha = if carried {
            alpha * CARRIED_ALPHA
        } else {
            alpha
        };
        if alpha <= 0.0 {
            return false;
        }
        let s1 = f64::from(theme.tokens.spacing.s1);
        let bounds = tile.well.union(tile.title);
        let layered = alpha < 1.0;
        if layered {
            let reach = rect.union(bounds).inflate(s1 * 2.0, s1 * 2.0);
            scene.push_layer(
                Fill::NonZero,
                BlendMode::default(),
                alpha,
                transform,
                &reach,
            );
        }
        let state = ui::chrome::SelectionState::new(
            self.overview_window_selection == Some(tab),
            self.last_cursor.is_some_and(|c| bounds.contains(c)),
            self.overview_press.and_then(|press| press.tab) == Some(tab),
        );
        // Selection and focus belong to the window at rest.
        if progress >= 1.0 && !carried && (state.active || state.hover || state.pressed) {
            ui::chrome::selection_tier(theme, state).paint(
                scene,
                transform,
                bounds.inflate(s1, s1),
                g.radius + s1,
            );
            if state.active {
                ui::widget::paint_focus_ring(
                    scene,
                    transform,
                    bounds.inflate(s1, s1),
                    g.radius + s1,
                    theme,
                );
            }
        }
        let live = tilo_desktop::window_switcher::draw_preview_well(
            scene,
            transform,
            theme,
            PreviewWell {
                rect,
                radius,
                icon: f64::from(theme.tokens.metrics.icon_lg).min(rect.height() * 0.6),
            },
            tile.window,
            thumbnails,
        );
        if chrome > 0.0 {
            self.draw_overview_tile_title(
                &mut ui::paint::PaintCtx::new(scene, transform, &mut *ctx.text, theme),
                tile,
                rect,
                (ui::motion::unit_opacity(chrome), state.active),
            );
        }
        if layered {
            scene.pop_layer();
        }
        live
    }

    /// A pictured window's title under its picture at `rect`, and the faces
    /// of the people in it at the title's end.
    fn draw_overview_tile_title(
        &self,
        ctx: &mut ui::paint::PaintCtx<'_>,
        tile: &Tile<'_>,
        rect: Rect,
        (alpha, active): (f32, bool),
    ) {
        let (scene, transform, theme) = (&mut *ctx.scene, ctx.transform, ctx.theme);
        let g = GridMetrics::for_theme(theme);
        let colors = &theme.tokens.colors;
        let style = theme.tokens.typography.style(TextRole::Caption);
        let line = Rect::new(
            rect.x0,
            rect.y1 + g.title_gap,
            rect.x1,
            rect.y1 + g.title_gap + g.title_lh,
        );
        scene.push_layer(
            Fill::NonZero,
            BlendMode::default(),
            alpha,
            transform,
            &line.inflate(g.title_lh, g.title_lh),
        );
        let face = g.title_lh;
        let shown = tile.faces.len().min(TILE_FACES_MAX);
        let faces_w = if shown == 0 {
            0.0
        } else {
            face * (1.0 + 0.6 * f64_of_usize(shown.saturating_sub(1)))
        };
        let text_w = (line.width() - faces_w - f64::from(theme.tokens.spacing.s1)).max(0.0);
        let title =
            ctx.text
                .elide_to_width_style(&tile.window.title, ui::num::f32_of_f64(text_w), &style);
        ctx.text.draw_style(
            scene,
            &title,
            if active {
                colors.text_primary
            } else {
                colors.text_dim
            },
            transform.pre_translate((line.x0, line.y0).into()),
            None,
            &style,
        );
        let ground = colors.surfaces.base;
        let mut x1 = line.x1;
        for (initial, color) in tile.faces.iter().take(shown).rev() {
            let disc = Rect::new(x1 - face, line.y0, x1, line.y0 + face);
            ui::widget::AvatarFace::new(&initial.to_string(), "")
                .presence(*color)
                .ringed_on(ground)
                .paint(
                    &mut ui::paint::PaintCtx::new(scene, transform, &mut *ctx.text, theme),
                    disc,
                );
            x1 -= face * 0.6;
        }
        scene.pop_layer();
    }

    /// Strip `si`'s name line: its name (or the field while it is typed) and
    /// its controls.
    fn draw_overview_strip_label(
        &mut self,
        ctx: &mut ui::paint::PaintCtx<'_>,
        picture: &OverviewPicture,
        si: usize,
        alpha: f32,
    ) {
        let (Some(strip), Some(place)) = (picture.strips.get(si), picture.places.get(si)) else {
            return;
        };
        if alpha <= 0.0 {
            return;
        }
        let (scene, transform, theme) = (&mut *ctx.scene, ctx.transform, ctx.theme);
        scene.push_layer(
            Fill::NonZero,
            BlendMode::default(),
            alpha,
            transform,
            &place.label,
        );
        let naming = self
            .overview_naming
            .as_ref()
            .is_some_and(|naming| naming.strip == strip.id);
        if naming {
            if let Some(field) = self.overview_naming_field(picture, si) {
                self.draw_overview_naming_field(
                    &mut ui::paint::PaintCtx::new(scene, transform, &mut *ctx.text, theme),
                    field,
                );
            }
        } else {
            let controls = self.overview_strip_controls(picture, si);
            let shown = self.overview_strip_controls_shown(picture, si);
            let end = controls
                .first()
                .filter(|_| shown)
                .map_or(place.label.x1, |(_, rect)| {
                    rect.x0 - f64::from(theme.tokens.spacing.s2)
                });
            let mut style = theme.tokens.typography.style(TextRole::Label);
            if strip.focused {
                style.weight = TextWeight::Semibold;
            }
            let name = ctx.text.elide_to_width_style(
                &strip.name,
                ui::num::f32_of_f64((end - place.label.x0).max(0.0)),
                &style,
            );
            let (_, name_h) = ctx.text.measure_style(&name, None, &style);
            let colors = &theme.tokens.colors;
            let top = place.label.y0
                + (place.label.height() - f64::from(theme.tokens.spacing.s2) - f64::from(name_h))
                    / 2.0;
            ctx.text.draw_style(
                scene,
                &name,
                if strip.focused {
                    colors.text_primary
                } else {
                    colors.text_dim
                },
                transform.pre_translate((place.label.x0, top).into()),
                None,
                &style,
            );
            if shown {
                for (control, rect) in controls {
                    let name = control.name();
                    Button::paint(
                        scene,
                        transform,
                        &mut *ctx.text,
                        theme,
                        &IconButton::face(rect, control.icon(), &name)
                            .control_size(ControlSize::Small)
                            .hovered(self.last_cursor.is_some_and(|p| rect.contains(p))),
                    );
                }
            }
        }
        scene.pop_layer();
    }

    /// The slot of a strip with no windows.
    fn draw_overview_empty_strip(ctx: &mut ui::paint::PaintCtx<'_>, slot: Rect, alpha: f32) {
        if alpha <= 0.0 {
            return;
        }
        let (scene, transform, theme) = (&mut *ctx.scene, ctx.transform, ctx.theme);
        scene.push_layer(Fill::NonZero, BlendMode::default(), alpha, transform, &slot);
        let on = ui::widget::paint_surface(
            scene,
            transform,
            slot,
            ui::widget::SurfaceLevel::Region,
            theme,
        );
        let style = on.tokens.typography.style(TextRole::Caption);
        let label = t("overview-strip-empty");
        let (w, h) = ctx.text.measure_style(&label, None, &style);
        ctx.text.draw_style(
            scene,
            &label,
            on.tokens.colors.text_dim,
            transform.pre_translate(
                (
                    slot.center().x - f64::from(w) / 2.0,
                    slot.center().y - f64::from(h) / 2.0,
                )
                    .into(),
            ),
            None,
            &style,
        );
        scene.pop_layer();
    }

    /// The name field over a strip's name line.
    fn draw_overview_naming_field(&mut self, ctx: &mut ui::paint::PaintCtx<'_>, field: Rect) {
        let Some(naming) = self.overview_naming.as_mut() else {
            return;
        };
        let (scene, transform, theme) = (&mut *ctx.scene, ctx.transform, ctx.theme);
        let placeholder = t("strip-switcher-name-placeholder");
        let face = FieldFace::new(field, &naming.text)
            .placeholder(&placeholder)
            .text_role(TextRole::Label)
            .focused(true);
        if naming.text.is_empty() {
            face.paint(scene, transform, &mut *ctx.text, theme);
        } else {
            face.paint_chrome(scene, transform, theme);
        }
        let input = naming.input.get_or_insert_with(|| {
            TextInput::new(Vec::new())
                .with_plain_chrome()
                .with_spellcheck(false)
                .with_autocorrect(false)
                .with_text_role(TextRole::Label)
        });
        input.set_text(&naming.text);
        input.set_selection(naming.selection.anchor, naming.selection.cursor);
        input.set_focus(true);
        input.apply_layout_external(face.text_rect(theme));
        let input_theme = theme
            .clone()
            .with_text_scale(theme.text_scale * theme.ui_scale);
        input.draw(
            scene,
            transform,
            &mut *ctx.text,
            &input_theme,
            &PresenceCtx::default(),
        );
    }

    /// The window being carried to another workspace, under the pointer or
    /// finger, over everything else.
    fn draw_overview_carried_window(
        &self,
        scene: &mut Scene,
        transform: Affine,
        theme: &Theme,
        picture: &OverviewPicture,
        thumbnails: &mut crate::shell::window_previews::LiveThumbnails,
    ) {
        let Some(press) = self
            .overview_press
            .filter(tilo_desktop::overview::OverviewPress::is_dragging)
        else {
            return;
        };
        let Some((si, wi, window)) = picture
            .windows()
            .find(|(_, _, window)| Some(window.address.tab) == press.tab)
        else {
            return;
        };
        let Some(well) = picture.places.get(si).and_then(|place| place.wells.get(wi)) else {
            return;
        };
        let g = GridMetrics::for_theme(theme);
        let (w, h) = (well.width(), well.height());
        let rect = Rect::new(
            press.at.x - w / 2.0,
            press.at.y - h / 2.0,
            press.at.x + w / 2.0,
            press.at.y + h / 2.0,
        );
        ui::surface::shadow(
            scene,
            transform,
            rect,
            g.radius,
            &theme.tokens.elevation.floating,
        );
        let _ = tilo_desktop::window_switcher::draw_preview_well(
            scene,
            transform,
            theme,
            PreviewWell {
                rect,
                radius: g.radius,
                icon: f64::from(theme.tokens.metrics.icon_lg),
            },
            window,
            thumbnails,
        );
    }

    // -- the keyboard ----------------------------------------------------

    /// Left and Right walk the pictured windows in reading order, a ring
    /// like the room list; Up and Down go to the nearest window of the strip
    /// above or below; Enter on a selected window goes there. Returns
    /// whether the key was taken.
    pub(super) fn handle_overview_window_keys(&mut self, logical_key: &Key) -> bool {
        let picture = self.overview_picture();
        let windows: Vec<(usize, usize, u64)> = picture
            .windows()
            .map(|(si, wi, window)| (si, wi, window.address.tab))
            .collect();
        let count = windows.len();
        if count == 0 {
            self.overview_window_selection = None;
            self.overview_window_interrupted = false;
            return false;
        }
        let current = self
            .overview_window_selection
            .and_then(|tab| windows.iter().position(|(_, _, t)| *t == tab));
        match logical_key {
            Key::Named(NamedKey::Enter | NamedKey::Space) => {
                let Some(&(_, _, tab)) = current.and_then(|index| windows.get(index)) else {
                    return false;
                };
                self.activate_overview_window(tab);
                self.latch_keyboard_overview();
                true
            }
            Key::Named(NamedKey::ArrowRight) => {
                let next = current.map_or(0, |index| {
                    index.saturating_add(1).checked_rem(count).unwrap_or(0)
                });
                self.select_overview_window(next);
                true
            }
            Key::Named(NamedKey::ArrowLeft) => {
                let last = count.saturating_sub(1);
                let next = current.map_or(last, |index| index.checked_sub(1).unwrap_or(last));
                self.select_overview_window(next);
                true
            }
            Key::Named(key @ (NamedKey::ArrowUp | NamedKey::ArrowDown)) => {
                let Some(index) = current else {
                    return false;
                };
                let up = *key == NamedKey::ArrowUp;
                match Self::window_in_next_strip(&picture, &windows, index, up) {
                    Some(next) => {
                        self.select_overview_window(next);
                        true
                    }
                    None => false,
                }
            }
            _ => false,
        }
    }

    /// The window of the nearest strip above (`up`) or below the one of
    /// window `index` that has windows, whose centre is closest to its
    /// centre.
    fn window_in_next_strip(
        picture: &OverviewPicture,
        windows: &[(usize, usize, u64)],
        index: usize,
        up: bool,
    ) -> Option<usize> {
        let &(si, wi, _) = windows.get(index)?;
        let x = picture.tile(si, wi)?.center().x;
        let strips: Vec<usize> = if up {
            (0..si).rev().collect()
        } else {
            (si.saturating_add(1)..picture.strips.len()).collect()
        };
        let target = strips.into_iter().find(|s| {
            picture
                .strips
                .get(*s)
                .is_some_and(|strip| !strip.windows.is_empty())
        })?;
        windows
            .iter()
            .enumerate()
            .filter(|(_, (s, _, _))| *s == target)
            .min_by(|(_, (s1, w1, _)), (_, (s2, w2, _))| {
                let d = |s: usize, w: usize| {
                    picture
                        .tile(s, w)
                        .map_or(f64::INFINITY, |tile| (tile.center().x - x).abs())
                };
                d(*s1, *w1).total_cmp(&d(*s2, *w2))
            })
            .map(|(i, _)| i)
    }

    /// Put the keyboard on pictured window `index`; the picture is this
    /// workspace's, so the room you are in is selected with it.
    pub(super) fn select_overview_window(&mut self, index: usize) {
        let Some(tab) = self
            .overview_row_windows()
            .get(index)
            .map(|window| window.address.tab)
        else {
            return;
        };
        self.overview_window_selection = Some(tab);
        self.overview_window_interrupted = false;
        self.overview_selection = self.overview_here_index();
        self.scroll_overview_selection_into_view();
        self.bump_chrome();
    }

    /// F2 names the strip the keyboard is in; Alt with Up or Down moves it.
    /// Returns whether the key was taken.
    pub(super) fn handle_overview_strip_keys(
        &mut self,
        logical_key: &Key,
        mods: ModifiersState,
        handled: &mut bool,
    ) -> bool {
        if !self.overlay_flags().overview || self.palette_visible {
            return false;
        }
        let picture = self.overview_picture();
        let Some(si) = self.overview_keyboard_strip(&picture) else {
            return false;
        };
        let taken = match logical_key {
            Key::Named(NamedKey::F2) => {
                self.begin_overview_naming(&picture, si);
                true
            }
            Key::Named(NamedKey::ArrowUp) if mods.alt_key() => {
                self.apply_overview_strip_control(&picture, si, StripControl::MoveUp);
                true
            }
            Key::Named(NamedKey::ArrowDown) if mods.alt_key() => {
                self.apply_overview_strip_control(&picture, si, StripControl::MoveDown);
                true
            }
            _ => false,
        };
        if taken {
            *handled = true;
            self.bump_chrome();
        }
        taken
    }

    // -- naming and moving strips ----------------------------------------

    fn begin_overview_naming(&mut self, picture: &OverviewPicture, si: usize) {
        let Some(strip) = picture.strips.get(si).filter(|strip| strip.renamable) else {
            return;
        };
        let text = strip.name.clone();
        let end = text.len();
        self.overview_naming = Some(OverviewNaming {
            strip: strip.id,
            text,
            selection: Selection {
                anchor: 0,
                cursor: end,
            },
            input: None,
        });
    }

    /// Do what strip `si`'s `control` says.
    fn apply_overview_strip_control(
        &mut self,
        picture: &OverviewPicture,
        si: usize,
        control: StripControl,
    ) {
        let Some(strip) = picture.strips.get(si).filter(|strip| strip.renamable) else {
            return;
        };
        let before = match control {
            StripControl::Rename => {
                self.begin_overview_naming(picture, si);
                return;
            }
            StripControl::MoveUp => match si.checked_sub(1) {
                Some(above) => picture.strips.get(above).map(|s| s.id),
                None => return,
            },
            StripControl::MoveDown => {
                if si.saturating_add(1) >= picture.strips.len() {
                    return;
                }
                picture.strips.get(si.saturating_add(2)).map(|s| s.id)
            }
        };
        let Some(chrome) = self.chrome.clone() else {
            return;
        };
        let mut state = chrome.borrow_mut();
        if state.move_strip(strip.id, before).is_err() {
            state.push_warning_notification(t("overview-strip-move-failed"), String::new());
        }
    }

    /// A key while a strip's name is typed: Enter keeps it, Escape keeps the
    /// old one.
    pub(super) fn overview_naming_key(&mut self, key: &Key, mods: ModifiersState) {
        let Some(naming) = self.overview_naming.as_mut() else {
            return;
        };
        match text_entry::apply(
            key,
            Mods::from_state(mods),
            &mut naming.text,
            &mut naming.selection,
        ) {
            Edit::Submit => self.commit_overview_naming(),
            Edit::Cancel => self.overview_naming = None,
            Edit::Changed => Self::cap_strip_name(naming),
            Edit::Ignored | Edit::ConsumedQuiet => {}
        }
        self.bump_chrome();
    }

    /// Text an input method committed into a strip's name field.
    pub(super) fn overview_naming_commit_text(&mut self, text: &str) -> bool {
        let Some(naming) = self.overview_naming.as_mut() else {
            return false;
        };
        text_entry::commit(text, &mut naming.text, &mut naming.selection);
        Self::cap_strip_name(naming);
        true
    }

    fn cap_strip_name(naming: &mut OverviewNaming) {
        if naming.text.chars().count() > STRIP_NAME_MAX_CHARS {
            naming.text = naming.text.chars().take(STRIP_NAME_MAX_CHARS).collect();
            naming.selection = Selection::collapsed(naming.text.len());
        }
    }

    /// Give the strip the typed name, when it is a name and a new one.
    fn commit_overview_naming(&mut self) {
        let Some(naming) = self.overview_naming.take() else {
            return;
        };
        let Some(name) = valid_strip_name(&naming.text).map(str::to_owned) else {
            return;
        };
        let Some(chrome) = self.chrome.clone() else {
            return;
        };
        let mut state = chrome.borrow_mut();
        let unchanged = state
            .tree()
            .desktop
            .active_workspace()
            .and_then(|workspace| workspace.strips.iter().find(|s| s.id == naming.strip))
            .is_some_and(|strip| strip.name == name);
        if !unchanged && state.rename_strip(naming.strip, &name).is_err() {
            state.push_warning_notification(t("overview-strip-rename-failed"), String::new());
        }
    }

    /// A press while a strip's name is typed: on the field it stays with the
    /// field, anywhere else it keeps the name and is taken, so it does not
    /// also act on what is under it.
    pub(super) fn overview_naming_takes_press(&mut self, event: &WindowEvent) -> bool {
        if self.overview_naming.is_none() {
            return false;
        }
        let WindowEvent::MouseInput {
            state: ElementState::Pressed,
            ..
        } = event
        else {
            return false;
        };
        let picture = self.overview_picture();
        let on_field = self
            .overview_naming
            .as_ref()
            .and_then(|naming| picture.strips.iter().position(|s| s.id == naming.strip))
            .and_then(|si| self.overview_naming_field(&picture, si))
            .zip(self.last_cursor)
            .is_some_and(|(field, p)| field.contains(p));
        if !on_field {
            self.commit_overview_naming();
        }
        self.bump_chrome();
        true
    }

    /// A click or tap on the picture's strip names and controls. Returns
    /// whether it landed on one.
    pub(super) fn overview_strip_click(&mut self, p: Point) -> bool {
        let picture = self.overview_picture();
        let Some(si) = picture
            .places
            .iter()
            .position(|place| place.label.contains(p))
        else {
            return false;
        };
        let control = self
            .overview_strip_controls(&picture, si)
            .into_iter()
            .find(|(_, rect)| rect.contains(p))
            .map(|(control, _)| control);
        match control {
            Some(control) => self.apply_overview_strip_control(&picture, si, control),
            // The name itself: a strip this machine names is renamed in
            // place.
            None => self.begin_overview_naming(&picture, si),
        }
        self.bump_chrome();
        true
    }

    // -- accessibility ---------------------------------------------------

    /// The strips of this workspace for assistive technology, top to
    /// bottom: each one's name and whether it has focus. Renaming and moving
    /// them is the `strip.rename` and `strip.move` actions.
    pub(super) fn overview_strips_accessibility(&self) -> Option<ui::a11y::AccessNode> {
        use ui::a11y::{AccessNode, AccessState, Role};
        let picture = self.overview_picture();
        if picture.strips.is_empty() {
            return None;
        }
        let total = picture.strips.len();
        let items = picture
            .strips
            .iter()
            .zip(&picture.places)
            .enumerate()
            .map(|(index, (strip, place))| {
                AccessNode::new(Role::ListItem)
                    .with_semantic_key(format!("overview-strip:{}", strip.id.raw()))
                    .with_name(strip.name.clone())
                    .with_position(index.saturating_add(1), total)
                    .with_bounds(place.label)
                    .with_state(AccessState::new().selected(strip.focused))
            })
            .collect();
        Some(
            AccessNode::new(Role::List)
                .with_name(t("overview-strips-title"))
                .with_bounds(
                    picture
                        .places
                        .iter()
                        .map(StripPlace::bounds)
                        .reduce(|all, rect| all.union(rect))
                        .unwrap_or(Rect::ZERO),
                )
                .with_children(items),
        )
    }
}

/// A pictured window, as drawn.
struct Tile<'a> {
    window: &'a SwitcherWindow,
    front: bool,
    well: Rect,
    title: Rect,
    faces: &'a [(char, Color)],
}

#[cfg(test)]
mod tests {
    use super::*;

    fn metrics() -> GridMetrics {
        GridMetrics::for_theme(&Theme::dark())
    }

    #[test]
    fn strips_stack_top_to_bottom_with_their_windows_in_strip_order_and_relative_sizes() {
        let m = metrics();
        let area = Rect::new(0.0, 0.0, 1200.0, 800.0);
        let strips = vec![
            vec![Size::new(1000.0, 900.0), Size::new(500.0, 900.0)],
            vec![],
            vec![Size::new(800.0, 900.0)],
        ];
        let places = layout_strips(area, &strips, &[], &m, None);
        assert_eq!(places.len(), 3);
        let first = &places[0];
        assert_eq!(first.wells.len(), 2);
        assert!(first.wells[0].x1 < first.wells[1].x0, "left to right");
        let ratio = first.wells[0].width() / first.wells[1].width();
        assert!((ratio - 2.0).abs() < 1e-6, "sizes stay relative: {ratio}");
        assert!(places[1].empty.is_some(), "an empty strip keeps its row");
        assert!(places[0].label.y1 <= first.wells[0].y0);
        assert!(first.titles[0].y0 > first.wells[0].y1);
        for pair in places.windows(2) {
            assert!(pair[0].bounds().y1 <= pair[1].label.y0, "top to bottom");
        }
        for place in &places {
            let b = place.bounds();
            assert!(b.x0 >= area.x0 && b.x1 <= area.x1 + 1e-6, "{b:?}");
            assert!(b.y0 >= area.y0 && b.y1 <= area.y1 + 1e-6, "{b:?}");
        }
        // Never larger than half the window it pictures.
        assert!(places[2].wells[0].height() <= 900.0 * MAX_SCALE + 1e-6);
    }

    #[test]
    fn the_tabs_of_one_window_sit_together_and_windows_apart() {
        let m = metrics();
        let area = Rect::new(0.0, 0.0, 1600.0, 800.0);
        let strips = vec![vec![Size::new(400.0, 900.0); 3]];
        // The first two are tabs of one window, the third its own window.
        let places = layout_strips(area, &strips, &[vec![false, true, false]], &m, None);
        let wells = &places[0].wells;
        assert!((wells[1].x0 - wells[0].x1 - m.tab_gap).abs() < 1e-6);
        assert!((wells[2].x0 - wells[1].x1 - m.window_gap).abs() < 1e-6);
    }

    #[test]
    fn a_strip_too_wide_for_the_picture_shrinks_alone() {
        let m = metrics();
        let area = Rect::new(0.0, 0.0, 600.0, 800.0);
        let strips = vec![
            vec![Size::new(1000.0, 900.0); 6],
            vec![Size::new(400.0, 900.0)],
        ];
        let places = layout_strips(area, &strips, &[], &m, None);
        let wide = places[0].wells.last().expect("windows");
        assert!(wide.x1 <= area.x1 + 1e-6, "{wide:?}");
        assert!(
            places[1].wells[0].height() > places[0].wells[0].height(),
            "the other strip keeps the picture's scale"
        );
    }

    #[test]
    fn inside_the_panel_every_strip_is_one_band_tall_and_the_picture_grows_down() {
        let m = metrics();
        let area = Rect::new(10.0, 100.0, 610.0, 100.0);
        let strips = vec![vec![Size::new(800.0, 600.0)], vec![Size::new(400.0, 600.0)]];
        let places = layout_strips(area, &strips, &[], &m, Some(m.band));
        assert_eq!(places[0].label.y0, area.y0);
        assert!((places[0].wells[0].height() - m.band).abs() < 1e-6);
        let bottom = places.last().map(StripPlace::bounds).expect("strips").y1;
        assert!(
            (bottom - (area.y0 + band_height(2, &m, m.band))).abs() < 1e-6,
            "{bottom}"
        );
    }
}
