//! Live window previews for the Alt+Tab switcher and the overview.
//!
//! A preview is a picture of a window as it looks now, drawn the way its
//! tile draws it:
//!
//! - **A client window** is drawn straight from its current buffer in the
//!   frame store, scaled into its card in the same scene. Nothing is
//!   captured, read back or copied, so the preview is there in the very
//!   first frame that shows the card and moves with every commit. A window
//!   off screen (another strip or workspace) shows the last buffer it
//!   presented: the store keeps a reference to it, not a copy. One that has
//!   never presented shows its app mark at the card's final size.
//! - **A window the shell draws itself** is painted live into the overview
//!   through the same path a screenshot of it takes ([`LiveThumbnails`]).
//!   The switcher, which draws through its own layer, still receives those
//!   few as small offscreen renders (`crate::compositor::window_previews`),
//!   kept here:
//!   - **Only while needed.** They are asked for only while the switcher is
//!     up, and dropped the moment it is not, so a closed switcher holds no
//!     pixels.
//!   - **Throttled.** A refresh round runs at most every
//!     [`REFRESH_INTERVAL`] and renders at most [`RENDERS_PER_REFRESH`]
//!     windows.
//!   - **Bounded.** At most [`MAX_PREVIEWS`] of at most
//!     [`MAX_PREVIEW_WIDTH`] × [`MAX_PREVIEW_HEIGHT`] pixels exist at once.
//! - **Private.** A capture-protected window
//!   ([`crate::shell::capture_protection`]) never gets one: the card shows
//!   its app mark and title instead. Previews live only in this device's
//!   chrome; nothing here is shared with collaborators, streamed, or dumped
//!   as pixels.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use vello::kurbo::Rect;

use ui::theme::Theme;
use ui::widget::{Image, ImageFit};

use tilo_desktop::window_switcher::{
    PreviewRequest, PreviewSource, SwitcherWindow, WindowSource, WindowThumbnails,
};
use vello::Scene;
use vello::kurbo::Affine;
use vello::peniko::{Extend, Fill, ImageBrush, ImageQuality};

use crate::shell::state::{GroupId, TileContent};

/// Shortest time between two refresh rounds: ten a second is live enough
/// for a glance and costs little.
pub const REFRESH_INTERVAL: Duration = Duration::from_millis(100);
/// Most windows rendered in one refresh round.
pub const RENDERS_PER_REFRESH: usize = 4;
/// Most previews held at once; windows past this show their app mark.
pub const MAX_PREVIEWS: usize = 16;
/// Largest preview, in pixels. Twice a switcher card's picture at the
/// default density, so previews stay sharp on a 2× display.
pub const MAX_PREVIEW_WIDTH: u32 = 352;
pub const MAX_PREVIEW_HEIGHT: u32 = 220;

/// The shell's windows as the switcher and the overview see them: the
/// icon theme's artwork, and live pictures except for capture-protected
/// windows.
pub struct ShellWindows;

impl WindowSource for ShellWindows {
    fn app_kind(&self, tile: &TileContent) -> ui::widget::AppKind {
        crate::shell::tile_app_kind(tile)
    }

    fn app_icon(&self, tile: &TileContent) -> Option<ui::icons::AppImage> {
        crate::app_icons::for_tile(tile)
    }

    fn preview_source(
        &self,
        tile: &TileContent,
        group: GroupId,
        tab: u64,
        shown: bool,
    ) -> PreviewSource {
        match tile {
            TileContent::Foreign { app_id, surface_id } => {
                if crate::shell::foreign::store().is_capture_protected(*surface_id, app_id) {
                    PreviewSource::Withheld
                } else {
                    PreviewSource::Client {
                        surface: *surface_id,
                    }
                }
            }
            TileContent::LocalApp { app_id, .. }
            | TileContent::PeerWindow { app_id, .. }
            | TileContent::HostedWindow { app_id, .. } => {
                if crate::shell::capture_protection::holds_secrets(app_id) {
                    PreviewSource::Withheld
                } else {
                    PreviewSource::Unavailable
                }
            }
            TileContent::AppPlaceholder { .. } => PreviewSource::Unavailable,
            TileContent::Stub { .. } | TileContent::WorkspaceActivity => {
                if shown {
                    PreviewSource::Painted {
                        capture_id: format!("group:{group}:tab:{tab}"),
                    }
                } else {
                    PreviewSource::Unavailable
                }
            }
        }
    }
}

/// The pixel size of a preview of a `width` × `height` window: as large as
/// fits inside the preview bound, never larger than the window, same shape.
#[expect(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "a positive pixel extent, rounded and clamped to the preview bound"
)]
pub fn preview_size(width: f64, height: f64) -> Option<(u32, u32)> {
    if !(width.is_finite() && height.is_finite()) || width < 1.0 || height < 1.0 {
        return None;
    }
    let scale = (f64::from(MAX_PREVIEW_WIDTH) / width)
        .min(f64::from(MAX_PREVIEW_HEIGHT) / height)
        .min(1.0);
    let w = (width * scale)
        .round()
        .clamp(1.0, f64::from(MAX_PREVIEW_WIDTH)) as u32;
    let h = (height * scale)
        .round()
        .clamp(1.0, f64::from(MAX_PREVIEW_HEIGHT)) as u32;
    Some((w, h))
}

struct WindowPreview {
    image: Image,
    rgba: Arc<[u8]>,
    width: u32,
    height: u32,
    /// The client frame this was rendered from, so an unchanged window is
    /// not rendered again. `None` for shell-painted windows.
    stamp: Option<u64>,
}

/// The previews on hand and the refresh clock.
#[derive(Default)]
pub struct WindowPreviews {
    previews: HashMap<u64, WindowPreview>,
    last_refresh: Option<Instant>,
    /// Where the next round starts in the wanted list, so every window
    /// gets its turn when there are more than one round renders.
    turn: usize,
    changed: bool,
}

impl WindowPreviews {
    /// A refresh round may run at `now`.
    pub fn refresh_due(&self, now: Instant) -> bool {
        self.last_refresh
            .is_none_or(|last| now.saturating_duration_since(last) >= REFRESH_INTERVAL)
    }

    /// When the next round may run; `None` when none has run yet.
    pub fn next_refresh_at(&self) -> Option<Instant> {
        self.last_refresh
            .and_then(|last| last.checked_add(REFRESH_INTERVAL))
    }

    /// Start a refresh round for the windows `wanted` now, in display
    /// order. Previews of windows no longer wanted are dropped. Returns the
    /// requests that need an offscreen render (shell-drawn windows; a
    /// client window is drawn live from its buffer and never rendered
    /// here), rotated so this round starts where the last one stopped;
    /// empty when the round is not due yet.
    pub fn begin_refresh(
        &mut self,
        now: Instant,
        wanted: &[PreviewRequest],
    ) -> Vec<PreviewRequest> {
        let wanted: Vec<PreviewRequest> = wanted
            .iter()
            .filter(|request| matches!(request.source, PreviewSource::Painted { .. }))
            .take(MAX_PREVIEWS)
            .cloned()
            .collect();
        let before = self.previews.len();
        self.previews
            .retain(|tab, _| wanted.iter().any(|request| request.tab == *tab));
        if self.previews.len() != before {
            self.changed = true;
        }
        if !self.refresh_due(now) || wanted.is_empty() {
            return Vec::new();
        }
        self.last_refresh = Some(now);
        let start = self.turn.checked_rem(wanted.len()).unwrap_or(0);
        let mut rotated = wanted;
        rotated.rotate_left(start);
        rotated
    }

    /// The round went through `visited` requests; the next one starts after them.
    pub fn advance(&mut self, visited: usize) {
        self.turn = self.turn.wrapping_add(visited);
    }

    /// The client frame the preview of `tab` was rendered from.
    pub fn stamp(&self, tab: u64) -> Option<u64> {
        self.previews.get(&tab).and_then(|preview| preview.stamp)
    }

    /// Keep a freshly rendered preview of `tab`. Refused past
    /// [`MAX_PREVIEWS`] or when its pixels do not match its size.
    pub fn store(
        &mut self,
        tab: u64,
        rgba: Arc<[u8]>,
        width: u32,
        height: u32,
        stamp: Option<u64>,
    ) {
        let expected = rgba_len(width, height);
        if width == 0
            || height == 0
            || width > MAX_PREVIEW_WIDTH
            || height > MAX_PREVIEW_HEIGHT
            || expected != Some(rgba.len())
        {
            return;
        }
        if !self.previews.contains_key(&tab) && self.previews.len() >= MAX_PREVIEWS {
            return;
        }
        let image = Image::from_rgba(Arc::clone(&rgba), width, height).with_fit(ImageFit::Contain);
        self.previews.insert(
            tab,
            WindowPreview {
                image,
                rgba,
                width,
                height,
                stamp,
            },
        );
        self.changed = true;
    }

    /// Forget every preview and restart the clock, once no chrome shows
    /// them (so there is nothing to repaint).
    pub fn clear(&mut self) {
        self.changed = false;
        self.previews.clear();
        self.last_refresh = None;
        self.turn = 0;
    }

    pub fn size(&self, tab: u64) -> Option<(u32, u32)> {
        self.previews
            .get(&tab)
            .map(|preview| (preview.width, preview.height))
    }

    /// The RGBA pixels of the preview of `tab`, row by row.
    pub fn pixels(&self, tab: u64) -> Option<Arc<[u8]>> {
        self.previews
            .get(&tab)
            .map(|preview| Arc::clone(&preview.rgba))
    }

    pub fn len(&self) -> usize {
        self.previews.len()
    }

    pub fn is_empty(&self) -> bool {
        self.previews.is_empty()
    }

    /// Bytes of pixels held.
    pub fn memory_bytes(&self) -> usize {
        self.previews
            .values()
            .map(|preview| rgba_len(preview.width, preview.height).unwrap_or(usize::MAX))
            .fold(0, usize::saturating_add)
    }

    /// Previews changed since the last paint.
    pub fn changed(&self) -> bool {
        self.changed
    }

    pub fn mark_painted(&mut self) {
        self.changed = false;
    }
}

impl WindowThumbnails for WindowPreviews {
    fn has(&self, window: &SwitcherWindow) -> bool {
        match &window.preview {
            PreviewSource::Client { surface } => client_has_frame(*surface),
            _ => self.previews.contains_key(&window.address.tab),
        }
    }

    fn paint(
        &mut self,
        window: &SwitcherWindow,
        scene: &mut Scene,
        transform: Affine,
        rect: Rect,
        theme: &Theme,
    ) -> Option<Rect> {
        if let PreviewSource::Client { surface } = &window.preview {
            return paint_client_frame(*surface, scene, transform, rect);
        }
        let preview = self.previews.get_mut(&window.address.tab)?;
        let fitted = fit_rect(rect, preview.width, preview.height);
        preview.image.paint_at_on(scene, transform, fitted, theme);
        Some(fitted)
    }
}

/// Whether client `surface` has presented a frame a card can draw.
pub fn client_has_frame(surface: u64) -> bool {
    crate::shell::foreign::store().frame_size(surface).is_some()
}

/// The frame counter of client `surface`'s current picture, so a cached
/// scene that drew it knows when to draw it again.
pub fn client_frame_seq(surface: u64) -> u64 {
    crate::shell::foreign::store().frame_seq(surface)
}

/// Draw client `surface`'s current frame fitted inside `rect`, straight
/// from the buffer the frame store holds (the GPU texture when it stayed
/// there), as its tile draws it. Copies nothing.
pub fn paint_client_frame(
    surface: u64,
    scene: &mut Scene,
    transform: Affine,
    rect: Rect,
) -> Option<Rect> {
    let (image, _) = crate::shell::foreign::store().frame_image(surface)?;
    let (width, height) = (image.width, image.height);
    if width == 0 || height == 0 || rect.width() <= 0.0 || rect.height() <= 0.0 {
        return None;
    }
    let fitted = fit_rect(rect, width, height);
    let brush = ImageBrush::new(image)
        .with_extend(Extend::Pad)
        .with_quality(ImageQuality::Medium);
    scene.fill(
        Fill::NonZero,
        transform
            * Affine::translate((fitted.x0, fitted.y0))
            * Affine::scale_non_uniform(
                fitted.width() / f64::from(width),
                fitted.height() / f64::from(height),
            ),
        &brush,
        None,
        &Rect::new(0.0, 0.0, f64::from(width), f64::from(height)),
    );
    Some(fitted)
}

/// A shell-drawn window painted live for one frame of chrome: the scene
/// its live paint produced, and the window's rect inside that scene.
pub struct PaintedWindow {
    pub scene: Scene,
    pub window: Rect,
}

/// The overview's pictures: every window drawn live in the frame that
/// shows it. Client windows come from their buffers; shell-drawn ones from
/// a live paint made for this frame ([`PaintedWindow`]).
#[derive(Default)]
pub struct LiveThumbnails {
    pub painted: HashMap<u64, PaintedWindow>,
}

impl WindowThumbnails for LiveThumbnails {
    fn has(&self, window: &SwitcherWindow) -> bool {
        match &window.preview {
            PreviewSource::Client { surface } => client_has_frame(*surface),
            PreviewSource::Painted { .. } => self.painted.contains_key(&window.address.tab),
            PreviewSource::Withheld | PreviewSource::Unavailable => false,
        }
    }

    fn paint(
        &mut self,
        window: &SwitcherWindow,
        scene: &mut Scene,
        transform: Affine,
        rect: Rect,
        _theme: &Theme,
    ) -> Option<Rect> {
        match &window.preview {
            PreviewSource::Client { surface } => {
                paint_client_frame(*surface, scene, transform, rect)
            }
            PreviewSource::Painted { .. } => {
                let painted = self.painted.get(&window.address.tab)?;
                let source = painted.window;
                if source.width() <= 0.0 || source.height() <= 0.0 {
                    return None;
                }
                let fitted = fit_extent(rect, source.width(), source.height());
                let place = Affine::translate((fitted.x0, fitted.y0))
                    * Affine::scale_non_uniform(
                        fitted.width() / source.width(),
                        fitted.height() / source.height(),
                    )
                    * Affine::translate((-source.x0, -source.y0));
                let mut clipped = Scene::new();
                clipped.push_layer(
                    Fill::NonZero,
                    vello::peniko::BlendMode::default(),
                    1.0,
                    Affine::IDENTITY,
                    &source,
                );
                clipped.append(&painted.scene, None);
                clipped.pop_layer();
                scene.append(&clipped, Some(transform * place));
                Some(fitted)
            }
            PreviewSource::Withheld | PreviewSource::Unavailable => None,
        }
    }
}

/// Bytes of a `width` × `height` RGBA image.
pub fn rgba_len(width: u32, height: u32) -> Option<usize> {
    usize::try_from(width)
        .ok()?
        .checked_mul(usize::try_from(height).ok()?)?
        .checked_mul(4)
}

/// `width` × `height` scaled to fit inside `rect`, centred.
fn fit_rect(rect: Rect, width: u32, height: u32) -> Rect {
    fit_extent(rect, f64::from(width.max(1)), f64::from(height.max(1)))
}

fn fit_extent(rect: Rect, w: f64, h: f64) -> Rect {
    let scale = (rect.width() / w).min(rect.height() / h);
    let (fw, fh) = (w * scale, h * scale);
    let center = rect.center();
    Rect::new(
        center.x - fw / 2.0,
        center.y - fh / 2.0,
        center.x + fw / 2.0,
        center.y + fh / 2.0,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shell::state::AppKind;

    fn pixels(width: u32, height: u32) -> Arc<[u8]> {
        Arc::from(vec![0x80_u8; rgba_len(width, height).unwrap_or(0)])
    }

    fn painted(tab: u64) -> PreviewRequest {
        PreviewRequest {
            tab,
            source: PreviewSource::Painted {
                capture_id: format!("group:1:tab:{tab}"),
            },
        }
    }

    #[test]
    fn previews_keep_the_window_shape_inside_the_bound() {
        assert_eq!(preview_size(1920.0, 1080.0), Some((352, 198)));
        assert_eq!(preview_size(800.0, 1600.0), Some((110, 220)));
        assert_eq!(preview_size(200.0, 100.0), Some((200, 100)));
        assert_eq!(preview_size(0.0, 100.0), None);
        assert_eq!(preview_size(f64::NAN, 100.0), None);
    }

    #[test]
    fn a_round_runs_at_most_every_interval() {
        let mut previews = WindowPreviews::default();
        let start = Instant::now();
        let wanted = [painted(1), painted(2)];
        assert_eq!(previews.begin_refresh(start, &wanted).len(), 2);
        let soon = start + REFRESH_INTERVAL / 2;
        assert!(previews.begin_refresh(soon, &wanted).is_empty());
        assert_eq!(previews.next_refresh_at(), Some(start + REFRESH_INTERVAL));
        assert_eq!(
            previews
                .begin_refresh(start + REFRESH_INTERVAL, &wanted)
                .len(),
            2
        );
    }

    #[test]
    fn rounds_take_turns_through_the_windows() {
        let mut previews = WindowPreviews::default();
        let start = Instant::now();
        let wanted = [painted(1), painted(2), painted(3)];
        let first = previews.begin_refresh(start, &wanted);
        assert_eq!(first.first().map(|r| r.tab), Some(1));
        previews.advance(2);
        let second = previews.begin_refresh(start + REFRESH_INTERVAL, &wanted);
        assert_eq!(second.first().map(|r| r.tab), Some(3));
    }

    #[test]
    fn memory_stays_bounded_and_drops_unwanted_windows() {
        let mut previews = WindowPreviews::default();
        let count = u64::try_from(MAX_PREVIEWS).unwrap_or(0) + 4;
        for tab in 0..count {
            previews.store(
                tab,
                pixels(MAX_PREVIEW_WIDTH, MAX_PREVIEW_HEIGHT),
                MAX_PREVIEW_WIDTH,
                MAX_PREVIEW_HEIGHT,
                None,
            );
        }
        assert_eq!(previews.len(), MAX_PREVIEWS);
        let bound = rgba_len(MAX_PREVIEW_WIDTH, MAX_PREVIEW_HEIGHT)
            .unwrap_or(0)
            .saturating_mul(MAX_PREVIEWS);
        assert_eq!(previews.memory_bytes(), bound);
        // Oversized or mis-sized pixels are refused outright.
        previews.clear();
        previews.store(1, pixels(400, 10), 400, 10, None);
        previews.store(2, pixels(2, 2), 3, 3, None);
        assert!(previews.is_empty());
        // A round for other windows forgets the ones no longer wanted.
        previews.store(1, pixels(4, 4), 4, 4, None);
        previews.store(2, pixels(4, 4), 4, 4, None);
        let _ = previews.begin_refresh(Instant::now(), &[painted(2)]);
        assert!(previews.pixels(1).is_none());
        assert!(previews.pixels(2).is_some());
    }

    #[test]
    fn secret_holding_apps_are_never_pictured() {
        let vault = TileContent::Foreign {
            app_id: "org.keepassxc.KeePassXC".into(),
            surface_id: 7,
        };
        assert_eq!(
            ShellWindows.preview_source(&vault, 1, 2, true),
            PreviewSource::Withheld
        );
        let passwords = TileContent::LocalApp {
            app_id: "so.tilo.passwords".into(),
            label: String::new(),
            note: String::new(),
        };
        assert_eq!(
            ShellWindows.preview_source(&passwords, 1, 2, true),
            PreviewSource::Withheld
        );
        let editor = TileContent::Foreign {
            app_id: "org.gnome.TextEditor".into(),
            surface_id: 9,
        };
        assert_eq!(
            ShellWindows.preview_source(&editor, 1, 2, false),
            PreviewSource::Client { surface: 9 }
        );
        let mut previews = WindowPreviews::default();
        let withheld = PreviewRequest {
            tab: 5,
            source: PreviewSource::Withheld,
        };
        assert!(
            previews
                .begin_refresh(Instant::now(), &[withheld])
                .is_empty()
        );
    }

    #[test]
    fn only_the_shown_tab_of_a_shell_drawn_app_is_painted() {
        let terminal = TileContent::Stub {
            kind: AppKind::Terminal,
            label: String::new(),
        };
        assert_eq!(
            ShellWindows.preview_source(&terminal, 10, 3, true),
            PreviewSource::Painted {
                capture_id: "group:10:tab:3".into()
            }
        );
        assert_eq!(
            ShellWindows.preview_source(&terminal, 10, 3, false),
            PreviewSource::Unavailable
        );
    }
}
