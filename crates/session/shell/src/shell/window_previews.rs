//! Live window previews for the Alt+Tab switcher and the overview.
//!
//! A preview is a small picture of a window as it looks now. The compositor
//! renders it (`crate::compositor::window_previews`); this module decides
//! which windows get one and keeps the results:
//!
//! - **Only while needed.** Previews are asked for only while the switcher
//!   or the overview is up, and all of them are dropped the moment neither
//!   is, so a closed switcher holds no pixels.
//! - **Throttled.** A refresh round runs at most every [`REFRESH_INTERVAL`],
//!   renders at most [`RENDERS_PER_REFRESH`] windows, and takes turns
//!   through the rest, so a desktop with many windows never stalls a frame.
//! - **Bounded.** At most [`MAX_PREVIEWS`] previews of at most
//!   [`MAX_PREVIEW_WIDTH`] × [`MAX_PREVIEW_HEIGHT`] pixels exist at once.
//! - **Private.** A capture-protected window
//!   ([`crate::shell::capture_protection`]) never gets one: the card shows its app
//!   mark and title instead. Previews live only in this device's chrome;
//!   nothing here is shared with collaborators, streamed, or dumped as
//!   pixels.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use vello::kurbo::Rect;

use ui::theme::Theme;
use ui::widget::{Image, ImageFit};

use tilo_desktop::window_switcher::{
    PreviewRequest, PreviewSource, WindowSource, WindowThumbnails,
};

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
            TileContent::LocalApp { app_id, .. } | TileContent::PeerWindow { app_id, .. } => {
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
    /// renderable requests, rotated so this round starts where the last one
    /// stopped; empty when the round is not due yet.
    pub fn begin_refresh(
        &mut self,
        now: Instant,
        wanted: &[PreviewRequest],
    ) -> Vec<PreviewRequest> {
        let wanted: Vec<PreviewRequest> = wanted
            .iter()
            .filter(|request| request.source.renderable())
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

    /// Order-independent identity of the stored pixels, so a panel cache
    /// rebuilds when a preview arrives and stays put while it does not.
    pub fn content_fingerprint(&self) -> u64 {
        let mut fingerprint = u64::try_from(self.previews.len()).unwrap_or(u64::MAX);
        for (tab, preview) in &self.previews {
            fingerprint ^= tab.wrapping_mul(0x9E37_79B9_7F4A_7C15)
                ^ preview.stamp.unwrap_or(0)
                ^ (u64::from(preview.width) << 32)
                ^ u64::from(preview.height);
        }
        fingerprint
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
    fn has(&self, tab: u64) -> bool {
        self.previews.contains_key(&tab)
    }

    fn paint(
        &mut self,
        tab: u64,
        scene: &mut vello::Scene,
        transform: vello::kurbo::Affine,
        rect: Rect,
        theme: &Theme,
    ) -> Option<Rect> {
        let preview = self.previews.get_mut(&tab)?;
        let fitted = fit_rect(rect, preview.width, preview.height);
        preview.image.paint_at_on(scene, transform, fitted, theme);
        Some(fitted)
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
    let (w, h) = (f64::from(width.max(1)), f64::from(height.max(1)));
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

    fn client(tab: u64) -> PreviewRequest {
        PreviewRequest {
            tab,
            source: PreviewSource::Client { surface: tab },
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
        let wanted = [client(1), client(2)];
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
        let wanted = [client(1), client(2), client(3)];
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
        let _ = previews.begin_refresh(Instant::now(), &[client(2)]);
        assert!(!previews.has(1));
        assert!(previews.has(2));
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
