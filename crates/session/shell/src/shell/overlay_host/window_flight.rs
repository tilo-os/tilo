//! The compositor's side of the pictures the overview app names
//! ([`tilo_desktop::window_flight`]): each named window drawn from what the
//! compositor already holds of it (a client's latest buffer, a live paint
//! of a window the shell draws), flying between where it stands on its
//! strip and where the app rests it on the overview's zoom
//! ([`ui::motion::Zoom::OVERVIEW`]). The first frame after Super+Tab is the
//! windows lifting off their strips: nothing is captured or rendered to
//! open it, and the app never draws another client's buffer.

use std::collections::HashMap;

use tilo_desktop::overview_picture::GridMetrics;
use tilo_desktop::window_flight::WindowFlight;
use tilo_desktop::window_switcher::{
    PreviewSource, PreviewState, PreviewWell, SwitcherWindow, WindowSwitcher, draw_preview_well,
};
use ui::presence::PresenceCtx;
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::widget::Widget;
use vello::Scene;
use vello::kurbo::{Affine, Rect, Vec2};
use vello::peniko::{BlendMode, Fill};

use super::ShellOverlayHost;
use crate::shell::state::TabId;

impl ShellOverlayHost {
    /// The display the overview covers: the focused one.
    pub(super) fn overview_output_rect(&self) -> Rect {
        self.focused_output()
            .or_else(|| self.primary_output())
            .map_or(self.rect, |output| output.rect())
    }

    /// Whether a window laid out at `rect` flies into the overview of
    /// `output`: it is on that display's screen. One off screen (on another
    /// strip, panned away) fades in at its place instead of streaking in
    /// from past the edge.
    pub(super) fn flies_from(rect: Rect, output: Rect) -> bool {
        rect.width() > 0.0 && rect.height() > 0.0 && rect.intersect(output).area() > 0.0
    }

    /// While the overview's zoom is open or flying, the windows on its
    /// display are drawn by the flight, one picture each: the strips leave
    /// them out meanwhile, or each would show twice. Call before the
    /// desktop is drawn.
    pub(super) fn hand_flying_windows_to_the_overview(&self, now: ui::time::Instant) {
        let flying = !self.layout_theme.reduce_motion && self.overview_zoom.progress(now) > 0.0;
        let output = self.overview_output_rect();
        for &ptr in self.group_row_ptrs.values() {
            // SAFETY: see the field doc on `group_row_ptrs`.
            let row: &mut ui::widget::GroupRow = unsafe { &mut *ptr };
            let ids = if flying {
                row.groups
                    .iter()
                    .filter(|group| Self::flies_from(group.get_rect(), output))
                    .map(|group| group.id)
                    .collect()
            } else {
                Vec::new()
            };
            row.set_shown_elsewhere(ids);
        }
    }

    /// Where window group `group` stands on its strip now: the whole window
    /// for a picture painted from the window as drawn (`whole`), its
    /// content for a client's buffer. `None` when it is not laid out.
    fn overview_window_source(&self, group: u64, whole: bool) -> Option<Rect> {
        self.group_row_ptrs
            .values()
            .find_map(|&ptr| {
                // SAFETY: see the field doc on `group_row_ptrs`; read-only.
                let row: &ui::widget::GroupRow = unsafe { &*ptr };
                let group = row.groups.iter().find(|g| g.id == group)?;
                Some(if whole {
                    group.get_rect()
                } else {
                    group
                        .active_host()
                        .map_or_else(|| group.get_rect(), Widget::get_rect)
                })
            })
            .filter(|rect| rect.width() > 0.0 && rect.height() > 0.0)
    }

    /// The windows of the workspace you are in, as the switcher lists them:
    /// the same titles, app marks and picture sources Alt+Tab shows.
    pub(super) fn overview_listed_windows(&self) -> HashMap<TabId, SwitcherWindow> {
        let Some(chrome) = self.chrome.as_ref() else {
            return HashMap::new();
        };
        let Ok(state) = chrome.try_borrow() else {
            return HashMap::new();
        };
        let tree = state.tree();
        self.layers
            .window_switcher
            .ordered_windows(&tree.desktop, &crate::shell::window_previews::ShellWindows)
            .into_iter()
            .map(|window| (window.address.tab, window))
            .collect()
    }

    /// Where each window of the workspace you are in stands on screen,
    /// which its picture flies from: none under reduced motion, and none
    /// for a window off the overview's display.
    pub(super) fn overview_window_sources(
        &self,
        windows: &HashMap<TabId, SwitcherWindow>,
    ) -> HashMap<TabId, Rect> {
        if self.layout_theme.reduce_motion {
            return HashMap::new();
        }
        let output = self.overview_output_rect();
        windows
            .iter()
            .filter_map(|(tab, window)| {
                let painted = matches!(window.preview, PreviewSource::Painted { .. });
                self.overview_window_source(window.address.group, painted)
                    .filter(|&from| Self::flies_from(from, output))
                    .map(|from| (*tab, from))
            })
            .collect()
    }

    /// Every window in `windows` drawn live for this frame: a client from
    /// its current buffer, a shell-drawn window from a live paint of it
    /// made now (the same path a screenshot of it takes).
    fn overview_live_thumbnails(
        &mut self,
        windows: &[SwitcherWindow],
        text_renderer: &mut TextRenderer,
        theme: &Theme,
    ) -> crate::shell::window_previews::LiveThumbnails {
        let mut live = crate::shell::window_previews::LiveThumbnails::default();
        let locked = self
            .chrome
            .as_ref()
            .and_then(|chrome| chrome.try_borrow().ok().map(|state| state.session_locked()))
            .unwrap_or(true);
        if locked {
            return live;
        }
        for window in windows {
            let PreviewSource::Painted { capture_id } = &window.preview else {
                continue;
            };
            let mut painted = Scene::new();
            let rects = {
                let _capture = crate::shell::capture_protection::CapturePaint::begin();
                self.draw_capture_targets(
                    &ui::widget::LiveCapture {
                        target_ids: std::slice::from_ref(capture_id),
                        complete_semantic_content: false,
                        content_only: false,
                    },
                    &mut ui::paint::PaintCtx::new(
                        &mut painted,
                        Affine::IDENTITY,
                        text_renderer,
                        theme,
                    ),
                    &PresenceCtx::default(),
                )
            };
            if let Some(rect) = rects
                .first()
                .map(|(_, rect)| *rect)
                .filter(|rect| rect.area() > 0.0)
            {
                live.painted.insert(
                    window.address.tab,
                    crate::shell::window_previews::PaintedWindow {
                        scene: painted,
                        window: rect,
                    },
                );
            }
        }
        live
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
                PreviewSource::Client { surface } => {
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

    /// Every picture `flight` names (in the coordinates of the app placed
    /// at `origin`), drawn at the zoom's `progress` from the window it
    /// pictures: flying from where it stands to where it rests, faded in
    /// where it rests when it has nowhere to fly from, the carried one over
    /// everything with its shadow.
    pub(super) fn draw_window_flight(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        (flight, progress, origin): (&WindowFlight, f64, Vec2),
    ) {
        self.overview_row_drawn.clear();
        let pictures = flight.pictures();
        if pictures.is_empty() {
            return;
        }
        let listed = self.overview_listed_windows();
        let sources = self.overview_window_sources(&listed);
        let windows: Vec<SwitcherWindow> = pictures
            .iter()
            .filter(|picture| !picture.lifted)
            .filter_map(|picture| listed.get(&picture.tab).cloned())
            .collect();
        let (key, mut thumbnails) = self.take_overview_thumbnails(&windows, text_renderer, theme);
        let p = progress.clamp(0.0, 1.0);
        let output = self.overview_output_rect();
        // In flight the pictures cross the whole display; at rest they keep
        // to where the overview shows them.
        let clip = if p < 1.0 {
            output
        } else {
            flight.clip().map_or(output, |clip| clip + origin)
        };
        scene.push_clip_layer(Fill::NonZero, transform, &clip);
        let g = GridMetrics::for_theme(theme);
        let icon = f64::from(theme.tokens.metrics.icon_lg);
        for picture in &pictures {
            let Some(window) = listed.get(&picture.tab) else {
                continue;
            };
            let rest = picture.rect + origin;
            if picture.lifted {
                ui::surface::shadow(
                    scene,
                    transform,
                    rest,
                    picture.radius,
                    &theme.tokens.elevation.floating,
                );
                let _ = draw_preview_well(
                    scene,
                    transform,
                    theme,
                    PreviewWell {
                        rect: rest,
                        radius: picture.radius,
                        icon,
                    },
                    window,
                    &mut thumbnails,
                );
                continue;
            }
            let source = picture
                .flies
                .then(|| sources.get(&picture.tab).copied())
                .flatten();
            let (rect, radius, alpha) = match source {
                Some(from) => (
                    ui::motion::Zoom::rect_at(from, rest, p),
                    ui::motion::Zoom::radius_at(g.window_radius, picture.radius, p),
                    picture.alpha,
                ),
                None => (
                    rest,
                    picture.radius,
                    picture.alpha * ui::motion::unit_opacity(p),
                ),
            };
            if alpha <= 0.0 {
                continue;
            }
            let layered = alpha < 1.0;
            if layered {
                scene.push_layer(Fill::NonZero, BlendMode::default(), alpha, transform, &rect);
            }
            let live = draw_preview_well(
                scene,
                transform,
                theme,
                PreviewWell {
                    rect,
                    radius,
                    icon: icon.min(rect.height() * 0.6),
                },
                window,
                &mut thumbnails,
            );
            if layered {
                scene.pop_layer();
            }
            self.overview_row_drawn.push((
                picture.tab,
                if live {
                    PreviewState::Live
                } else {
                    WindowSwitcher::preview_state(window, &thumbnails)
                },
            ));
        }
        scene.pop_layer();
        self.overview_thumbnails = Some((key, thumbnails));
    }
}
