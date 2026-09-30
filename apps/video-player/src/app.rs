//! The local video-player app: an edge-to-edge reusable
//! [`ui::widget::VideoPlayer`] driven by a shared [`media::MediaEngine`].
//!
//! `PlayerApp` is the root widget. The video fills the whole surface — there is
//! **no persistent app chrome bar** (the OS owns window chrome; HIG §1e wants
//! video immersive). The media title and the Open action float in a top overlay
//! that appears with the player's controls and auto-hides during playback, so
//! immersive video is truly edge-to-edge. It opens files through the XDG
//! `FileChooser` portal (or `--open PATH`), and — when a collab session is
//! attached — keeps playback in sync across peers via the shared [`media`]
//! transport.

use std::cell::{Cell, RefCell};
use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use ui::widget::{AppModel, ModelPath};

use automerge::{AutoCommit, ObjId};
use taffy::style_helpers::percent;
use taffy::{NodeId, Size, Style, TaffyTree};
use tilo_clock::unix_now_ms;
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect, Stroke};
use vello::peniko::{BlendMode, Color, Fill};
use winit::event::{ElementState, MouseButton, WindowEvent};

use ui::collaboration::{CollabEvent, CollabRequest, CollaborationManager};
use ui::icons::Icon;
use ui::paint::fill_rect;
use ui::portal::{
    ContentsChannel, ContentsOutcome, FileFilter, PickRequest, PickedContents, PortalChannel,
    PortalOutcome,
};
use ui::presence::PresenceCtx;
use ui::surface;
use ui::text_render::{TextRenderer, TextWeight};
use ui::theme::Theme;
use ui::tokens::Spacing;
use ui::widget::video_player::{PlayerSetting, PlayerSettingEvent};
use ui::widget::{
    ActionEmitter, Button, ButtonFace, FloatingSurfaceProvider, FullscreenProvider, VideoPlayer,
    Widget,
};

use crate::actions::PlayerOperation;
use crate::collab::{CollabPlayback, Role};
use crate::preferences::{PlaybackPreferences, RecentMedia};

/// A `Spacing::BASE` step as the `f64` this painter's geometry uses.
#[allow(
    clippy::cast_lossless,
    reason = "const context: `f64::from` is not const; f32 -> f64 is exact"
)]
const fn step(px: f32) -> f64 {
    px as f64
}

/// Inset of the floating top overlay (media title + Open) from the video
/// edges: the section-padding step (HIG §7b `s4`).
const OVERLAY_MARGIN: f64 = step(Spacing::BASE.s4);
/// Height (logical px) of the floating Open pill at 1× text.
const OPEN_BTN_H: f64 = 32.0;
/// Width (logical px) of the floating Open pill at 1× text.
const OPEN_BTN_W: f64 = 84.0;
/// Width (logical px) of the floating Library button at 1× text.
const LIBRARY_BTN_W: f64 = 88.0;
/// Horizontal room the shared `Button` face keeps around a label
/// (`Button::measure`, 15 px a side). Only the label part of a capsule's
/// authored width scales with text; this padding does not.
const BUTTON_PAD_X: f64 = 30.0;
/// Vertical room either side of a floating button's label once the label
/// outgrows [`OPEN_BTN_H`] (accessibility text scale): `s1`.
const BUTTON_PAD_Y: f64 = step(Spacing::BASE.s1);
/// Title ↔ Library capsule, and a row's text ↔ its controls: siblings, `s3`.
const STRIP_TITLE_GAP: f64 = step(Spacing::BASE.s3);
/// Label size the floating buttons ask the shared `Button` face for, and the
/// media title's size. Both are renderer-relative: the renderer applies the
/// text scale and the default `TextStyle` line height.
const BUTTON_LABEL_SIZE: f32 = 14.0;
const TITLE_SIZE: f32 = 15.0;

/// Line height the renderer will give `size` at `text_scale` — the default
/// `TextStyle` leading on the scaled size, not a local copy of that factor.
fn line_h(size: f32, text_scale: f64) -> f64 {
    #[allow(
        clippy::cast_possible_truncation,
        reason = "text scale is clamped to 0.5..=4.0 by Theme"
    )]
    let scale = text_scale as f32;
    ui::text_metrics::line_height_px(size, scale)
}

/// A floating capsule's width: the shared `Button`'s side padding around the
/// label's **measured** width when the renderer has measured it (`draw`
/// caches the two labels per text scale — see `StripLabels`), else the
/// authored 1× width with only its label part scaled, which is what the
/// first frame at a new scale uses before any measurement exists.
fn capsule_w(authored_1x: f64, text_scale: f64, measured_label_w: Option<f64>) -> f64 {
    match measured_label_w {
        Some(label_w) => (BUTTON_PAD_X + label_w).max(BUTTON_PAD_X),
        None => BUTTON_PAD_X + (authored_1x - BUTTON_PAD_X) * text_scale,
    }
}

/// The two floating capsule labels measured with the frame's renderer, keyed
/// by the text scale they were measured at. `apply_layout` has a theme but no
/// renderer, so `draw` measures and caches these and re-places the strip when
/// the cache changes; the next `apply_layout` then agrees with paint.
#[derive(Clone, Copy, Debug, PartialEq)]
struct StripLabels {
    text_scale: f64,
    library_w: f64,
    open_w: f64,
}
/// One gutter for everything in the library panel — title, subtitle, tabs, row
/// text on the left; Close/Clear and each row's Play on the right. The
/// section-padding step (HIG §7b `s4`).
const PANEL_GUTTER: f64 = step(Spacing::BASE.s4);
/// Rows extend `PANEL_ROW_INSET` short of the panel edge and inset their
/// content by the same amount, so row text and row controls land on
/// [`PANEL_GUTTER`] while the row card still reads as a row (`s2`).
const PANEL_ROW_INSET: f64 = step(Spacing::BASE.s2);

/// The floating top strip: one description consumed by `apply_layout` (the
/// Library/Open rects, which hit-testing and the accessibility export read)
/// and `paint_overlay` (scrim + title).
///
/// Every control shares `center_y`; the scrim always contains the buttons.
/// Heights are the authored 1× values until the measured label line no longer
/// fits, then grow just enough to contain it.
#[derive(Clone, Copy, Debug, PartialEq)]
struct TopStrip {
    center_y: f64,
    button_h: f64,
    scrim_h: f64,
}

impl TopStrip {
    fn at(y0: f64, text_scale: f64) -> Self {
        let label_line = line_h(BUTTON_LABEL_SIZE, text_scale);
        let button_h = OPEN_BTN_H.max(label_line + BUTTON_PAD_Y * 2.0);
        Self {
            center_y: y0 + OVERLAY_MARGIN + button_h / 2.0,
            button_h,
            scrim_h: button_h + OVERLAY_MARGIN * 2.0,
        }
    }
}

/// Library panel type sizes. Renderer-relative like every legacy
/// `measure`/`draw_text` size (the renderer applies the text scale), used by
/// both the geometry (to size boxes from the measured line) and the paint.
const PANEL_TITLE_SIZE: f32 = 18.0;
const PANEL_SUBTITLE_SIZE: f32 = 12.0;
const PANEL_HEADER_BUTTON_SIZE: f32 = 12.0;
const PANEL_TAB_SIZE: f32 = 13.0;
const PANEL_ROW_TITLE_SIZE: f32 = 14.0;
const PANEL_ROW_SUBTITLE_SIZE: f32 = 12.0;
const PANEL_ROW_CONTROL_SIZE: f32 = 11.0;
/// Authored 1× panel chrome. Each height is a floor: it holds until the
/// measured label line (plus its padding) needs more, so nothing moves at 1×
/// and 200% text stops painting through the next line (title over subtitle,
/// subtitle over tabs, "Remove" over "Play", 1× row pitch under 2× glyphs).
const PANEL_WIDTH: f64 = 360.0;
/// One rhythm for the panel (HIG §7b): the header sits a sibling step
/// (`s3`) under the panel edge, sections are `s3` apart, controls beside
/// each other are `s2` apart, and list rows — siblings of equal weight —
/// keep `s3` between their cards so no two rows touch.
const PANEL_HEADER_TOP: f64 = step(Spacing::BASE.s3);
const PANEL_HEADER_BUTTON_H: f64 = 28.0;
const PANEL_CLOSE_W: f64 = 56.0;
const PANEL_CLEAR_W: f64 = 50.0;
const PANEL_BUTTON_GAP: f64 = step(Spacing::BASE.s2);
const PANEL_SECTION_GAP: f64 = step(Spacing::BASE.s3);
const PANEL_TAB_H: f64 = 30.0;
const PANEL_QUEUE_TAB_W: f64 = 86.0;
const PANEL_RECENT_TAB_W: f64 = 92.0;
const PANEL_ROW_H: f64 = 58.0;
const PANEL_ROW_GAP: f64 = step(Spacing::BASE.s3);
const PANEL_ROW_PAD_Y: f64 = step(Spacing::BASE.s2);
const PANEL_ROW_TITLE_BOX_H: f64 = 21.0;
const PANEL_CONTROL_H: f64 = 44.0;
const PANEL_PLAY_W: f64 = 48.0;
const PANEL_REMOVE_W: f64 = 50.0;
/// Vertical room either side of a label inside a panel button / tab.
const PANEL_LABEL_PAD_Y: f64 = step(Spacing::BASE.s1);
/// The panel subtitle — measured for its wrapped height in the panel gutter.
const PANEL_SUBTITLE: &str = "Local to this device · paths never shared";
/// The floating strip's scrim opacity over the media (the authored 90/255).
/// The scrim itself is the theme's over-media `surfaces.scrim` — the same
/// token the shared transport bar uses — so both edges of the window agree.
const STRIP_SCRIM_ALPHA: f32 = 90.0 / 255.0;
/// Horizontal room a compact panel control keeps around its measured label
/// (6 px a side) before its authored width has to grow.
const PANEL_LABEL_PAD_X: f64 = step(Spacing::BASE.s3);

/// The library panel's labels measured with the frame's renderer at one text
/// scale and panel width. `apply_layout` has a theme but no renderer, so
/// `paint_library_panel` measures these, caches them and re-places the panel
/// when they change; until the first paint at a scale the authored widths,
/// scaled by the text factor, stand in.
#[derive(Clone, Copy, Debug, PartialEq)]
struct PanelLabels {
    text_scale: f64,
    panel_w: f64,
    close_w: f64,
    clear_w: f64,
    queue_w: f64,
    recent_w: f64,
    shared_w: f64,
    remove_w: f64,
    play_w: f64,
    /// The subtitle's wrapped height inside the panel gutter.
    subtitle_h: f64,
}

/// The library panel's chrome, derived once from the panel rect (and the
/// text scale / measured labels) so the hit rects set in `apply_layout` and
/// the faces painted in `paint_library_panel` cannot drift apart.
#[derive(Clone, Copy, Debug, PartialEq)]
struct LibraryPanelGeometry {
    title: Rect,
    subtitle: Rect,
    close: Rect,
    clear: Rect,
    queue_tab: Rect,
    recent_tab: Rect,
    shared_tab: Rect,
    rows_top: f64,
    /// One row's surface height (authored 58) — the pitch adds `PANEL_ROW_GAP`.
    row_h: f64,
    /// The row title's box under `PANEL_ROW_PAD_Y`; the subtitle box follows it.
    row_title_box_h: f64,
    /// The Play / Remove capsules, centred in the row.
    control_h: f64,
    play_w: f64,
    remove_w: f64,
}

impl LibraryPanelGeometry {
    fn for_panel(panel: Rect, text_scale: f64, labels: Option<PanelLabels>) -> Self {
        let labels = labels.filter(|l| {
            (l.text_scale - text_scale).abs() < f64::EPSILON
                && (l.panel_w - panel.width()).abs() < 0.5
        });
        // Authored width, grown to hold a measured label with `pad` around it;
        // before any measurement the authored width scales with the text.
        let grown = |authored: f64, measured: Option<f64>, pad: f64| match measured {
            Some(label_w) => authored.max(label_w + pad),
            None => authored * text_scale,
        };
        let left = panel.x0 + PANEL_GUTTER;
        let right = panel.x1 - PANEL_GUTTER;
        let header_y = panel.y0 + PANEL_HEADER_TOP;
        let header_button_h = PANEL_HEADER_BUTTON_H
            .max(line_h(PANEL_HEADER_BUTTON_SIZE, text_scale) + PANEL_LABEL_PAD_Y * 2.0);
        let title_h = PANEL_HEADER_BUTTON_H.max(line_h(PANEL_TITLE_SIZE, text_scale));
        let header_h = header_button_h.max(title_h);
        let close_w = grown(PANEL_CLOSE_W, labels.map(|l| l.close_w), PANEL_LABEL_PAD_X);
        let clear_w = grown(PANEL_CLEAR_W, labels.map(|l| l.clear_w), PANEL_LABEL_PAD_X);
        let button_y0 = header_y + (header_h - header_button_h) / 2.0;
        let close = Rect::new(
            right - close_w,
            button_y0,
            right,
            button_y0 + header_button_h,
        );
        let clear = Rect::new(
            close.x0 - PANEL_BUTTON_GAP - clear_w,
            button_y0,
            close.x0 - PANEL_BUTTON_GAP,
            button_y0 + header_button_h,
        );
        // 1×: 38..54 — the measured single line plus 1 px; a wrapped 200%
        // subtitle pushes the tabs down instead of painting through them.
        let subtitle_line =
            labels.map_or_else(|| line_h(PANEL_SUBTITLE_SIZE, text_scale), |l| l.subtitle_h);
        let subtitle_y0 = header_y + header_h;
        let subtitle = Rect::new(left, subtitle_y0, right, subtitle_y0 + subtitle_line + 1.0);
        let tab_h = PANEL_TAB_H.max(line_h(PANEL_TAB_SIZE, text_scale) + PANEL_LABEL_PAD_Y * 2.0);
        let tabs_y0 = subtitle.y1 + PANEL_SECTION_GAP;
        let queue_w = grown(PANEL_QUEUE_TAB_W, labels.map(|l| l.queue_w), BUTTON_PAD_X);
        let recent_w = grown(PANEL_RECENT_TAB_W, labels.map(|l| l.recent_w), BUTTON_PAD_X);
        let shared_w = grown(PANEL_RECENT_TAB_W, labels.map(|l| l.shared_w), BUTTON_PAD_X);
        let mut next_x = left;
        let mut next_y = tabs_y0;
        let mut tab = |width: f64| {
            let width = width.min((right - left).max(0.0));
            if next_x > left && next_x + width > right {
                next_x = left;
                next_y += tab_h + PANEL_BUTTON_GAP;
            }
            let rect = Rect::new(next_x, next_y, next_x + width, next_y + tab_h);
            next_x = rect.x1 + PANEL_BUTTON_GAP;
            rect
        };
        let queue_tab = tab(queue_w);
        let recent_tab = tab(recent_w);
        let shared_tab = tab(shared_w);
        // Rows: 8 px, the title box (21 at 1×), the subtitle line, 8 px.
        let row_title_box_h =
            PANEL_ROW_TITLE_BOX_H.max(line_h(PANEL_ROW_TITLE_SIZE, text_scale) + 3.5);
        let row_h = PANEL_ROW_H.max(
            PANEL_ROW_PAD_Y * 2.0 + row_title_box_h + line_h(PANEL_ROW_SUBTITLE_SIZE, text_scale),
        );
        let control_h =
            PANEL_CONTROL_H.max(line_h(PANEL_ROW_CONTROL_SIZE, text_scale) + PANEL_ROW_PAD_Y * 2.0);
        Self {
            // The title ends before the header's leftmost button slot (Clear,
            // shown on the Recent tab) so it can never run under either button.
            title: Rect::new(
                left,
                header_y,
                clear.x0 - PANEL_BUTTON_GAP,
                header_y + header_h,
            ),
            subtitle,
            close,
            clear,
            queue_tab,
            recent_tab,
            shared_tab,
            rows_top: shared_tab.y1 + PANEL_SECTION_GAP,
            row_h,
            row_title_box_h,
            control_h,
            play_w: grown(PANEL_PLAY_W, labels.map(|l| l.play_w), PANEL_LABEL_PAD_X),
            remove_w: grown(
                PANEL_REMOVE_W,
                labels.map(|l| l.remove_w),
                PANEL_LABEL_PAD_X,
            ),
        }
    }
}
/// A decoder open is asynchronous. Do not turn portal/direct admission into a
/// false successful history entry until a first picture has actually arrived.
const OPEN_FRAME_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(12);
/// One file-picker request at a time.  A portal response is asynchronous and
/// cannot be cancelled at the protocol level, so the app owns the receiver and
/// drops it when a newer direct open supersedes the request.  `generation` is
/// deliberately separate from the media generation: it identifies *this
/// chooser* rather than the media that may eventually be selected.
struct PendingPortalOpen {
    channel: PickerChannel,
    generation: u64,
    invocation: Option<tilo_actions::InvocationId>,
}

/// A host with paths answers with the chosen path; a host without them (a
/// browser page) hands over the file's bytes.
enum PickerChannel {
    Path(PortalChannel),
    Contents(ContentsChannel),
}

/// What the picker chose, in the form the host could give it.
enum Picked {
    Path(PathBuf),
    Contents(PickedContents),
}

/// A terminal response paired with the request identity and optional action
/// caller that owns it. Keeping this named makes `drain_portal`'s stale-result
/// guard readable without a deeply nested tuple type.
type PortalOpenOutcome = (
    u64,
    Option<tilo_actions::InvocationId>,
    Result<Picked, Option<String>>,
);

/// A canonical open invocation awaiting its decoder's first picture. This is
/// separate from the file-picker request: the picker can succeed while decode
/// fails, stalls, or is superseded before a frame appears.
struct PendingMediaOpen {
    invocation: tilo_actions::InvocationId,
    source_id: String,
    deadline: ui::time::Instant,
}

/// A file-backed local record. The source is deliberately kept only in this
/// process; public resources expose the opaque `id` and friendly basename.
#[derive(Clone)]
struct LocalMediaEntry {
    id: String,
    title: String,
    source: media::Source,
}

#[derive(Clone)]
struct LibraryRowHit {
    id: String,
    row: Rect,
    play: Rect,
    remove: Option<Rect>,
}

#[derive(Clone)]
struct SharedRowHit {
    id: String,
    jump: Option<Rect>,
    remove: Rect,
    annotation: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum LibraryTab {
    Queue,
    Recent,
    Shared,
}

fn configured_player(autoplay: bool, loop_enabled: bool) -> VideoPlayer {
    let mut player = VideoPlayer::new()
        .with_engine(media::MediaEngine::new())
        .with_test_id("video_surface");
    player.enable_mpris("tilo_video_player", "Video Player");
    configure_player_settings(&mut player, autoplay, loop_enabled);
    player
}

fn configure_player_settings(player: &mut VideoPlayer, autoplay: bool, loop_enabled: bool) {
    const RATES: [&str; 8] = ["0.25×", "0.5×", "0.75×", "1×", "1.25×", "1.5×", "2×", "4×"];
    // These settings are implemented by this app, so the shared settings
    // popover is never decorative.
    player.set_setting(PlayerSetting::toggle(
        "autoplay",
        Icon::Play,
        "Autoplay",
        0,
        autoplay,
    ));
    player.set_setting(PlayerSetting::toggle(
        "loop",
        Icon::Refresh,
        "Repeat current video",
        0,
        loop_enabled,
    ));
    let rate = player.engine().map_or(1.0, media::MediaEngine::rate);
    let selected = [0.25_f32, 0.5, 0.75, 1.0, 1.25, 1.5, 2.0, 4.0]
        .iter()
        .position(|candidate| (*candidate - rate).abs() < f32::EPSILON)
        .unwrap_or(3);
    player.set_setting(PlayerSetting::choice(
        "playback-rate",
        Icon::Refresh,
        "Playback speed",
        0,
        RATES.into_iter().map(str::to_string).collect(),
        selected,
    ));
    player.set_setting(PlayerSetting::button(
        "picture-in-picture",
        Icon::PictureInPicture2,
        "Picture in Picture",
        0,
    ));
}

/// Build a taffy node from `children` already built in the same tree.
/// `new_with_children` only fails when a child id does not resolve in this
/// tree, which cannot happen for children this same call just built.
#[allow(
    clippy::unwrap_used,
    reason = "new_with_children cannot fail for children built in this same tree; see doc comment"
)]
fn parent_node(taffy: &mut TaffyTree<()>, style: Style, children: &[NodeId]) -> NodeId {
    taffy.new_with_children(style, children).unwrap()
}

/// Shared title/description/tab rhythm for the custom library panel. Text
/// metrics are expressed in logical pixels and expanded exactly once by the
/// active accessibility scale, so paint and hit geometry cannot drift.
fn library_vertical_metrics(scale: f64, description_lines: usize) -> (f64, f64, f64) {
    let title_top = 10.0;
    let title_height = 18.0 * scale;
    let privacy_top = title_top + title_height + 8.0 * scale;
    let privacy_line_height = 16.0 * scale;
    let privacy_bottom = privacy_top + privacy_line_height * description_lines as f64;
    let tabs_top = privacy_bottom + 12.0 * scale;
    // Tabs remain generous at the default scale too. These are not merely
    // decorative filters: Queue, Recent and Shared partition different data
    // ownership models, so they need the same comfortable target as the row
    // actions below them.
    let tab_height = if scale > 1.25 { 50.0 } else { 36.0 };
    let rows_top = tabs_top + tab_height + if scale > 1.25 { 14.0 } else { 12.0 };
    (privacy_top, tabs_top, rows_top)
}

/// Paint one line of panel text vertically centred in `rect`, at the weight
/// the caller asked for.
///
/// The centre comes from the line height the renderer reports for this exact
/// string, size and weight — the same call that will shape the glyphs — so the
/// text sits where the rect says at every accessibility scale. (The previous
/// helper offset the top by `size` as a hand-rolled baseline and discarded the
/// weight, which is why the panel title floated 17 px below its rect and
/// painted Regular.)
fn panel_text(
    scene: &mut Scene,
    text_renderer: &mut TextRenderer,
    transform: Affine,
    rect: Rect,
    text: &str,
    size: f32,
    color: Color,
    weight: TextWeight,
) {
    // Bound to the rect's width so a long line wraps inside the panel instead
    // of running past its edge (the 360 px panel's empty-state copy did).
    #[allow(
        clippy::cast_possible_truncation,
        reason = "panel widths are ordinary logical-pixel values"
    )]
    let max_width = Some(rect.width().max(0.0) as f32);
    let (_, text_h) = text_renderer.measure_weighted(text, size, max_width, weight);
    let y = rect.y0 + (rect.height() - f64::from(text_h)) / 2.0;
    text_renderer.draw_text_weighted(
        scene,
        text,
        size,
        color,
        transform * Affine::translate((rect.x0, y)),
        max_width,
        weight,
    );
}

fn find_resource<'a>(
    resources: &'a [tilo_actions::Resource],
    id: &str,
) -> Option<&'a tilo_actions::Resource> {
    resources.iter().find_map(|resource| {
        if resource.id.0 == id {
            Some(resource)
        } else {
            find_resource(&resource.children, id)
        }
    })
}

fn video_open_request() -> PickRequest {
    PickRequest::open("Open a video", "Play")
        .thread_name("video-player-portal")
        .filter(
            FileFilter::new("Video")
                .mimetype("video/mp4")
                .mimetype("video/webm")
                .mimetype("video/x-matroska")
                .mimetype("video/quicktime")
                .glob("*.mp4")
                .glob("*.webm")
                .glob("*.mkv")
                .glob("*.mov")
                .glob("*.avi"),
        )
        .all_files()
}

/// Build a [`media::Source`] for a filesystem path: a path under the FUSE
/// workspace mount becomes a [`media::Source::Workspace`] (read-through,
/// hydrate-on-demand); anything else is a [`media::Source::LocalFile`].
pub fn source_for_path(path: &Path) -> media::Source {
    let root = media::Source::workspace_root();
    if path.starts_with(&root) {
        media::Source::Workspace(path.to_path_buf())
    } else {
        media::Source::LocalFile(path.to_path_buf())
    }
}

/// Opaque, deterministic per-process identity for a local source. Resource
/// IDs and action payload guards need a stable token, but publishing a URI
/// would expose an absolute path to search, history, or a collaborator.
fn local_source_token(source: &media::Source, generation: u64) -> String {
    // FNV-1a is used only as an opaque UI identifier, not as a security
    // boundary. The monotonic generation prevents a same-path reopen from
    // accepting stale dynamic stream/chapter actions.
    let mut hasher = fnv::FnvHasher::default();
    std::hash::Hasher::write(&mut hasher, media::session::session_key(source).as_bytes());
    let hash = std::hash::Hasher::finish(&hasher);
    format!("local-media-{hash:016x}-open-{generation}")
}

#[allow(
    clippy::struct_excessive_bools,
    reason = "overlay, local library, portal, and frame-dirty flags are independent widget state"
)]
pub struct PlayerApp {
    player: VideoPlayer,
    title: String,
    rect: Rect,
    /// Accessibility text scale captured from the live theme. Only geometry
    /// containing text expands; the media surface itself remains unscaled.
    text_scale: f64,
    /// The floating Open pill's hit rect (top-right of the video). Only live
    /// while the overlay is shown.
    open_button_rect: Rect,
    library_button_rect: Rect,
    /// Measured "Library" / "Open" label widths at the current text scale
    /// (see `StripLabels`); `None` until the first paint at that scale.
    strip_labels: Option<StripLabels>,
    /// Measured library-panel labels (see `PanelLabels`); same lifecycle.
    panel_labels: Option<PanelLabels>,
    library_panel_rect: Rect,
    library_queue_tab_rect: Rect,
    library_recent_tab_rect: Rect,
    library_shared_tab_rect: Rect,
    library_close_rect: Rect,
    library_row_hits: Vec<LibraryRowHit>,
    library_clear_rect: Rect,
    shared_watch_later_rect: Rect,
    shared_bookmark_rect: Rect,
    shared_row_hits: Vec<SharedRowHit>,
    library_visible: bool,
    library_tab: LibraryTab,
    queue: Vec<LocalMediaEntry>,
    recent: VecDeque<LocalMediaEntry>,
    current_queue_id: Option<String>,
    last_cursor: Point,
    /// A file-open portal request in flight, drained each `sync`. Requests are
    /// never replaced: doing so would let an old chooser response unexpectedly
    /// open a file after the user chose a newer source.
    portal_open: Option<PendingPortalOpen>,
    media_open: Option<PendingMediaOpen>,
    portal_generation: u64,
    resource_results: VecDeque<tilo_actions::InvocationUpdate>,
    open_action: Option<ActionEmitter<()>>,
    library_control_invocations: VecDeque<tilo_actions::ControlInvocation>,
    open_focused: bool,
    library_focused: bool,
    library_focus_id: Option<String>,
    /// Collaborative playback state (host/follower). `None` for a local-only
    /// session or the headless `--shots` path.
    collab: Option<CollabPlayback>,
    /// The collab socket. `None` unless a collab session is attached.
    manager: Option<CollaborationManager>,
    /// App-owned options whose state is surfaced through the settings resource.
    preferences: PlaybackPreferences,
    /// Deterministic decoder metadata used only by the explicit headless
    /// `metadata` screenshot fixture. Live runs always read the active engine.
    shot_metadata: Option<DecoderMetadata>,
    /// Deterministically holds the production no-frame/loading presentation
    /// for visual regression capture while the synthesized engine stays
    /// hermetic. Live runs always derive loading from decoder frame state.
    shot_loading: bool,
    pending_pip: Option<ui::floating_surface::FloatingSurfaceRequest>,
    actions: crate::resource::ActionSink,
    /// Framework-hosted surfaces leave the palette tree to
    /// `tilo_resource::publish`; standalone keeps the contextual builder.
    framework_hosted: bool,
    shared_library: crate::shared::SharedLibrary,
    shot_shared: bool,
    /// Opaque source identity for durable shared playlists and markers. It
    /// survives reopening the same local media, unlike the per-open source ID.
    current_media_ref: Option<String>,
    current_source_id: Option<String>,
    source_generation: u64,
    search_cache: RefCell<Option<SearchCache>>,
    search_catalog_builds: Cell<u64>,
    metadata_revision: u64,
    search_metadata: DecoderMetadata,
    /// A human-readable failure from the latest open request. It is only
    /// painted in the no-frame state, preserving a successfully playing video
    /// when a later picker request fails.
    open_error: Option<String>,
    /// Test-only observation of the authoritative retained portal failure.
    /// Production builds carry no observer or additional settlement path.
    #[cfg(test)]
    history_test_open_error_observer: Option<std::rc::Rc<std::cell::RefCell<Option<String>>>>,
    /// Where files have no path: the kept copy (`crate::kept`) of each
    /// queue or recent entry opened by contents, by entry id.
    kept: HashMap<String, String>,
    /// Kept copies still on disk, newest first.
    kept_retained: Vec<String>,
    /// A kept copy being read back, and the entry it reopens.
    kept_recall: Option<KeptRecall>,
    kept_wake: ui::wake::WakeHandle,
    dirty: bool,
}

struct KeptRecall {
    bytes: std::sync::mpsc::Receiver<Option<Vec<u8>>>,
    token: String,
    title: String,
    queue_id: Option<String>,
}

#[derive(Clone, Default, PartialEq)]
struct DecoderMetadata {
    audio_tracks: Vec<crate::actions::TrackState>,
    subtitle_tracks: Vec<crate::actions::TrackState>,
    chapters: Vec<crate::actions::ChapterState>,
}

struct SearchCache {
    source_id: Option<String>,
    revision: u64,
    provider: Arc<crate::search::VideoPlayerSearchProvider>,
}

impl PlayerApp {
    fn action_context<'a>(
        &'a self,
        audio_tracks: &'a [crate::actions::TrackState],
        subtitle_tracks: &'a [crate::actions::TrackState],
        chapters: &'a [crate::actions::ChapterState],
    ) -> crate::actions::PlayerResourceCtx<'a> {
        crate::actions::PlayerResourceCtx {
            title: &self.title,
            media: crate::actions::MediaAvailability {
                has_media: self.player.has_media(),
                source_id: self.current_source_id.clone(),
                media_ref: self.current_media_ref.clone(),
                can_control_transport: self
                    .collab
                    .as_ref()
                    .is_none_or(|collab| collab.role() == Role::Host),
            },
            settings: crate::actions::PlaybackSettingsState {
                autoplay: self.preferences.autoplay,
                loop_enabled: self.preferences.loop_enabled,
                rate: self.player.engine().map_or(1.0, media::MediaEngine::rate),
            },
            audio_tracks,
            subtitle_tracks,
            chapters,
            library: crate::actions::LocalLibraryState {
                queue: self.library_items(self.queue.iter()),
                recent: self.library_items(self.recent.iter()),
            },
        }
    }

    fn library_items<'a>(
        &self,
        entries: impl Iterator<Item = &'a LocalMediaEntry>,
    ) -> Vec<crate::actions::LocalMediaItemState> {
        entries
            .map(|entry| crate::actions::LocalMediaItemState {
                id: entry.id.clone(),
                title: entry.title.clone(),
                current: self.current_queue_id.as_deref() == Some(entry.id.as_str()),
            })
            .collect()
    }

    fn decoder_metadata(
        &self,
    ) -> (
        Vec<crate::actions::TrackState>,
        Vec<crate::actions::TrackState>,
        Vec<crate::actions::ChapterState>,
    ) {
        if let Some(metadata) = &self.shot_metadata {
            return (
                metadata.audio_tracks.clone(),
                metadata.subtitle_tracks.clone(),
                metadata.chapters.clone(),
            );
        }
        let audio_tracks = self
            .player
            .engine()
            .map(media::MediaEngine::audio_tracks)
            .unwrap_or_default()
            .into_iter()
            .map(|track| crate::actions::TrackState {
                stream_id: track.id,
                index: track.index,
                label: track.label,
                selected: track.selected,
            })
            .collect();
        let subtitle_tracks = self
            .player
            .engine()
            .map(media::MediaEngine::subtitle_tracks)
            .unwrap_or_default()
            .into_iter()
            .map(|track| crate::actions::TrackState {
                stream_id: track.id,
                index: track.index,
                label: track.label,
                selected: track.selected,
            })
            .collect();
        let chapters = self
            .player
            .engine()
            .map(media::MediaEngine::chapters)
            .unwrap_or_default()
            .into_iter()
            .map(|chapter| crate::actions::ChapterState {
                id: chapter.id,
                label: chapter.label,
                seconds: chapter.start_ms as f64 / 1000.0,
            })
            .collect();
        (audio_tracks, subtitle_tracks, chapters)
    }

    fn sync_decoder_settings(&mut self) {
        let (audio_tracks, subtitle_tracks, chapters) = self.decoder_metadata();
        let metadata = DecoderMetadata {
            audio_tracks: audio_tracks.clone(),
            subtitle_tracks: subtitle_tracks.clone(),
            chapters: chapters.clone(),
        };
        if self.search_metadata == metadata {
            return;
        }
        self.search_metadata = metadata;
        self.invalidate_search_cache();
        let audio_limit = audio_tracks
            .len()
            .min(crate::actions::CONTEXTUAL_CHILD_LIMIT);
        let subtitle_limit = subtitle_tracks
            .len()
            .min(crate::actions::CONTEXTUAL_CHILD_LIMIT);
        let chapter_limit = chapters.len().min(crate::actions::CONTEXTUAL_CHILD_LIMIT);
        let audio_tracks = audio_tracks.get(..audio_limit).unwrap_or(&audio_tracks);
        let subtitle_tracks = subtitle_tracks
            .get(..subtitle_limit)
            .unwrap_or(&subtitle_tracks);
        let chapters = chapters.get(..chapter_limit).unwrap_or(&chapters);
        if audio_tracks.is_empty() {
            self.player.remove_setting("audio-track");
        } else {
            let selected = audio_tracks
                .iter()
                .position(|track| track.selected)
                .unwrap_or(0);
            self.player.set_setting(PlayerSetting::choice(
                "audio-track",
                Icon::Settings,
                "Audio track",
                0,
                audio_tracks
                    .iter()
                    .map(|track| track.label.clone())
                    .collect(),
                selected,
            ));
        }
        if subtitle_tracks.is_empty() {
            self.player.remove_setting("subtitle-track");
        } else {
            let selected = subtitle_tracks
                .iter()
                .position(|track| track.selected)
                .map_or(0, |index| index.saturating_add(1));
            let mut options = vec!["Off".to_string()];
            options.extend(subtitle_tracks.iter().map(|track| track.label.clone()));
            self.player.set_setting(PlayerSetting::choice(
                "subtitle-track",
                Icon::Settings,
                "Subtitles",
                0,
                options,
                selected,
            ));
        }
        if chapters.is_empty() {
            self.player.remove_setting("chapters");
        } else {
            self.player.set_setting(PlayerSetting::choice(
                "chapters",
                Icon::ListVideo,
                "Chapters",
                0,
                chapters
                    .iter()
                    .map(|chapter| chapter.label.clone())
                    .collect(),
                0,
            ));
        }
    }

    /// A player with a fresh engine and no media loaded.
    pub fn new() -> Self {
        Self::new_with_preferences(PlaybackPreferences::load())
    }

    fn new_with_preferences(preferences: PlaybackPreferences) -> Self {
        // Tests exercise the real request and completion paths; a recording
        // host answers every picker so no desktop file chooser ever opens.
        #[cfg(test)]
        ui::host::install_default(ui::host::HostServices::recording().0);
        let player = configured_player(preferences.autoplay, preferences.loop_enabled);
        let recent: VecDeque<LocalMediaEntry> = crate::preferences::load_recent_history()
            .into_iter()
            .map(|entry| LocalMediaEntry {
                id: entry.id,
                title: entry.title,
                source: entry.source,
            })
            .collect();
        Self::from_prepared(player, preferences, recent)
    }

    fn from_prepared(
        player: VideoPlayer,
        preferences: PlaybackPreferences,
        recent: VecDeque<LocalMediaEntry>,
    ) -> Self {
        let kept_retained = recent
            .iter()
            .filter_map(|entry| crate::kept::token_of(&entry.source).map(str::to_owned))
            .collect();
        let action_ctx = crate::actions::PlayerResourceCtx {
            title: "No media",
            media: crate::actions::MediaAvailability {
                has_media: false,
                source_id: None,
                media_ref: None,
                can_control_transport: true,
            },
            settings: crate::actions::PlaybackSettingsState {
                autoplay: preferences.autoplay,
                loop_enabled: preferences.loop_enabled,
                rate: player.engine().map_or(1.0, media::MediaEngine::rate),
            },
            audio_tracks: &[],
            subtitle_tracks: &[],
            chapters: &[],
            library: crate::actions::LocalLibraryState::default(),
        };
        let action_surface = crate::actions::resources(&action_ctx, &AutoCommit::new());
        let open_action = action_surface
            .first()
            .and_then(|resource| tilo_actions::ActionBinding::unit_for(resource, "open").ok())
            .map(ActionEmitter::new);
        Self {
            player,
            title: "No media".to_string(),
            rect: Rect::ZERO,
            text_scale: 1.0,
            open_button_rect: Rect::ZERO,
            library_button_rect: Rect::ZERO,
            strip_labels: None,
            panel_labels: None,
            library_panel_rect: Rect::ZERO,
            library_queue_tab_rect: Rect::ZERO,
            library_recent_tab_rect: Rect::ZERO,
            library_shared_tab_rect: Rect::ZERO,
            library_close_rect: Rect::ZERO,
            library_row_hits: Vec::new(),
            library_clear_rect: Rect::ZERO,
            shared_watch_later_rect: Rect::ZERO,
            shared_bookmark_rect: Rect::ZERO,
            shared_row_hits: Vec::new(),
            library_visible: false,
            library_tab: LibraryTab::Queue,
            queue: Vec::new(),
            recent,
            current_queue_id: None,
            last_cursor: Point::ZERO,
            portal_open: None,
            media_open: None,
            portal_generation: 0,
            resource_results: VecDeque::new(),
            open_action,
            library_control_invocations: VecDeque::new(),
            open_focused: false,
            library_focused: false,
            library_focus_id: None,
            collab: None,
            manager: None,
            preferences,
            shot_metadata: None,
            shot_loading: false,
            pending_pip: None,
            actions: crate::resource::ActionSink::default(),
            framework_hosted: false,
            shared_library: crate::shared::SharedLibrary::default(),
            shot_shared: false,
            current_media_ref: None,
            current_source_id: None,
            source_generation: 0,
            search_cache: RefCell::new(None),
            search_catalog_builds: Cell::new(0),
            metadata_revision: 0,
            search_metadata: DecoderMetadata::default(),
            open_error: None,
            #[cfg(test)]
            history_test_open_error_observer: None,
            kept: HashMap::new(),
            kept_retained,
            kept_recall: None,
            kept_wake: ui::wake::WakeHandle::new(),
            dirty: true,
        }
    }

    /// Construct the real root type from inert prepared inputs, so the
    /// binary's launch-composition tests can exercise the production widget
    /// without reading settings, registering MPRIS, opening a
    /// decoder/provider, or attaching collaboration. Not feature-gated: the
    /// binary's tests cannot switch on a library feature without a self
    /// dev-dependency, which would claim `--features video-player/thin`.
    #[doc(hidden)]
    pub fn history_test_root() -> Self {
        let preferences = PlaybackPreferences {
            autoplay: false,
            loop_enabled: true,
        };
        let mut player = VideoPlayer::new()
            .with_engine(media::MediaEngine::new_synthesized())
            .with_test_id("video_surface");
        configure_player_settings(&mut player, preferences.autoplay, preferences.loop_enabled);
        let current = LocalMediaEntry {
            id: "controlled-queue-current".into(),
            title: "Controlled history clip".into(),
            source: media::Source::Remote {
                url: "https://fixture.invalid/current.mp4".into(),
            },
        };
        let recent = VecDeque::from([
            LocalMediaEntry {
                id: "controlled-recent-autumn".into(),
                title: "Autumn reel".into(),
                source: media::Source::Remote {
                    url: "https://fixture.invalid/autumn.mp4".into(),
                },
            },
            LocalMediaEntry {
                id: "controlled-recent-city".into(),
                title: "City study".into(),
                source: media::Source::Remote {
                    url: "https://fixture.invalid/city.mp4".into(),
                },
            },
        ]);
        let mut app = Self::from_prepared(player, preferences, recent);
        app.queue = vec![
            current.clone(),
            LocalMediaEntry {
                id: "controlled-queue-next".into(),
                title: "Next controlled clip".into(),
                source: media::Source::Remote {
                    url: "https://fixture.invalid/next.mp4".into(),
                },
            },
        ];
        app.title = current.title;
        app.current_queue_id = Some(current.id);
        app.current_source_id = Some("source-7".into());
        app.source_generation = 7;
        app.library_visible = true;
        app.library_tab = LibraryTab::Recent;
        app.library_focus_id = Some("controlled-recent-city".into());
        app.shot_metadata = Some(DecoderMetadata {
            audio_tracks: vec![crate::actions::TrackState {
                stream_id: "audio-1".into(),
                index: 0,
                label: "Controlled stereo".into(),
                selected: true,
            }],
            ..DecoderMetadata::default()
        });
        app
    }

    #[cfg(test)]
    pub(crate) fn history_test_with_pending_portal() -> (
        Self,
        std::sync::mpsc::Sender<PortalOutcome>,
        std::rc::Rc<std::cell::RefCell<Option<String>>>,
    ) {
        let mut app = Self::history_test_root();
        let (sender, channel) = std::sync::mpsc::channel();
        let observed_error = std::rc::Rc::new(std::cell::RefCell::new(None));
        app.history_test_open_error_observer = Some(observed_error.clone());
        app.portal_generation = 11;
        app.portal_open = Some(PendingPortalOpen {
            channel: PickerChannel::Path(channel),
            generation: 11,
            invocation: None,
        });
        (app, sender, observed_error)
    }

    /// Open a filesystem `path` immediately (used by `--open PATH` and the
    /// portal outcome). Autoplays.
    #[must_use]
    pub fn with_open_path(mut self, path: &Path) -> Self {
        let _ = self.open_path(path);
        self
    }

    /// Open a broker-validated handoff source before the first frame.
    #[must_use]
    pub fn with_open_source(mut self, source: media::Source, title: String) -> Self {
        let _ = self.open_local_source(source, title, None);
        self
    }

    /// Land on the acknowledged source playhead and preserve whether the
    /// source was playing. This runs after open so local autoplay preferences
    /// cannot override the handoff transaction.
    #[must_use]
    pub fn with_handoff_transport(mut self, position_ms: u64, resume_playing: bool) -> Self {
        self.player.seek_to_ms(position_ms);
        if resume_playing {
            self.player.play();
        } else {
            self.player.pause();
        }
        self
    }

    /// Build a player staged for a headless `--shots` capture: a source opened
    /// with its real engine state and the controls revealed. Opens
    /// `open` when given, else a synthesized demo clip. Never attaches a collab
    /// socket, so no thread/portal is created.
    ///
    /// `scenario` selects an optional demo overlay for the capture:
    ///   * `"empty"` — the first-run state before a source is chosen.
    ///   * `"error"` — a decoder/open failure before a picture arrives.
    ///   * `"loading"` — a source admitted while its first frame is pending.
    ///   * `"native-error"` — use `--open` with a bad source to capture the
    ///     decoder's actual error event rather than fixture copy.
    ///   * `"playing"` / `"paused"` — explicit transport states (the
    ///     default remains paused for byte-stable review).
    ///   * `"settings"` — open the real autoplay/repeat settings menu.
    ///   * `"fullscreen"` — toggle fullscreen so the control bar shows its
    ///     immersive (exit-fullscreen) state.
    ///   * `"metadata"` — open the controls menu with deterministic embedded
    ///     audio/subtitle/chapter metadata.
    ///   * `"library"` / `"recent"` — populated device-local queue/history.
    ///   * `"shared"` — shared playlist and timestamp-annotation management.
    ///   * `"library-focus"` — the same queue with a keyboard-visible Play
    ///     focus ring, for accessibility visual review.
    ///   * anything else / `None` — a plain player.
    pub fn for_shot(open: Option<PathBuf>, scenario: Option<&str>) -> Self {
        let mut app = Self::new();
        app.stage_shot_source(open, scenario);
        app.stage_shot_scenario(scenario);
        if !matches!(scenario, Some("empty" | "error")) {
            app.player.reveal_controls();
        }
        app
    }

    fn stage_shot_source(&mut self, open: Option<PathBuf>, scenario: Option<&str>) {
        if !matches!(scenario, Some("empty" | "error")) {
            if let Some(path) = open {
                let _ = self.open_path(&path);
            } else {
                // The demo shot must stay hermetic: pin the deterministic
                // synthesized-frame engine so no real GStreamer pipeline is
                // built and the placeholder `demo.local` host is never
                // contacted — even though `decode` compiles the real backend in
                // by default. `--shots --open <file>` is the real-decode path.
                self.player
                    .set_engine(media::MediaEngine::new_synthesized());
                let source = media::Source::Remote {
                    url: "https://demo.local/clip.mp4".into(),
                };
                let queue_id = self.ensure_queued(source.clone(), "Demo clip".into());
                let _ = self.set_current_source(&source);
                self.player.open(source);
                self.title = "Demo clip".to_string();
                self.current_queue_id = Some(queue_id);
                // Park at a fixed position for a stable reference frame. The
                // explicit `playing` fixture starts the real engine below;
                // default/`paused` deliberately stay deterministic.
                self.player.seek_to_ms(8_000);
                self.player.pause();
            }
        }
    }

    fn stage_shot_scenario(&mut self, scenario: Option<&str>) {
        match scenario {
            Some("error") => {
                self.open_error = Some(
                    "The local decoder could not read this video. Check the file and try again."
                        .into(),
                );
            }
            Some("loading") => self.shot_loading = true,
            Some("playing") => self.player.play(),
            Some("paused") => self.player.pause(),
            Some("settings") => self.player.open_settings_menu(None),
            Some("metadata") => {
                self.shot_metadata = Some(DecoderMetadata {
                    audio_tracks: vec![
                        crate::actions::TrackState {
                            stream_id: "fixture-audio-en".into(),
                            index: 0,
                            label: "English stereo".into(),
                            selected: true,
                        },
                        crate::actions::TrackState {
                            stream_id: "fixture-audio-de".into(),
                            index: 1,
                            label: "Deutsch 5.1".into(),
                            selected: false,
                        },
                    ],
                    subtitle_tracks: vec![crate::actions::TrackState {
                        stream_id: "fixture-subtitles-en".into(),
                        index: 0,
                        label: "English captions".into(),
                        selected: true,
                    }],
                    chapters: vec![
                        crate::actions::ChapterState {
                            id: "intro".into(),
                            label: "Introduction · 0:00".into(),
                            seconds: 0.0,
                        },
                        crate::actions::ChapterState {
                            id: "scene".into(),
                            label: "Second act · 0:45".into(),
                            seconds: 45.0,
                        },
                    ],
                });
                self.sync_decoder_settings();
                self.player.open_settings_menu(None);
            }
            Some("library" | "recent" | "library-focus") => {
                for (url, title) in [
                    ("https://demo.local/next.mp4", "Second act"),
                    ("https://demo.local/finale.mp4", "Closing scene"),
                ] {
                    self.ensure_queued(media::Source::Remote { url: url.into() }, title.into());
                }
                self.recent = self.queue.iter().rev().cloned().collect();
                self.library_visible = true;
                self.library_tab = if scenario == Some("recent") {
                    LibraryTab::Recent
                } else {
                    LibraryTab::Queue
                };
                if scenario == Some("library-focus") {
                    self.library_focus_id = self.queue.get(1).map(|entry| entry.id.clone());
                }
            }
            Some("shared") => {
                self.library_visible = true;
                self.library_tab = LibraryTab::Shared;
                self.shared_library = crate::shared::SharedLibrary {
                    playlists: vec![crate::shared::Playlist {
                        id: "watch-later".into(),
                        name: "Watch later".into(),
                    }],
                    items: Vec::new(),
                    annotations: vec![
                        crate::shared::TimestampAnnotation {
                            id: "opening".into(),
                            media_ref: self.current_media_ref.clone().unwrap_or_default(),
                            time_ms: 8_000,
                            text: "Opening detail".into(),
                        },
                        crate::shared::TimestampAnnotation {
                            id: "bookmark".into(),
                            media_ref: self.current_media_ref.clone().unwrap_or_default(),
                            time_ms: 45_000,
                            text: String::new(),
                        },
                    ],
                };
                self.shot_shared = true;
            }
            Some("fullscreen") => self.player.toggle_fullscreen(),
            _ => {}
        }
    }

    /// Attach collaborative playback: a live collab socket plus the host/
    /// follower role from the environment. Only the live run path calls this —
    /// `--shots` and tests stay local so no thread/socket is created.
    #[must_use]
    pub fn with_collab(mut self) -> Self {
        let role = Role::from_env();
        self.player.set_transport_enabled(role == Role::Host);
        if role == Role::Follower {
            // Repeat changes shared transport and therefore belongs to the
            // host. The resource tree retains an unavailable explanation.
            self.player.remove_setting("loop");
        }
        self.collab = Some(CollabPlayback::new(role));
        self.manager = Some(CollaborationManager::new());
        self
    }

    /// Mark this player as a framework-hosted surface so
    /// [`Widget::resources`] defers to `tilo_resource::publish`.
    pub(crate) fn set_framework_hosted(&mut self, hosted: bool) {
        self.framework_hosted = hosted;
    }

    /// Whether [`PublishesResources`](ui::resources::PublishesResources) is
    /// publishing only live track extras beside the framework door.
    pub(crate) fn is_framework_hosted(&self) -> bool {
        self.framework_hosted
    }

    /// Test-only: drop staged decoder metadata so hosted extras go empty.
    #[cfg(test)]
    pub(crate) fn clear_shot_metadata_for_test(&mut self) {
        self.shot_metadata = Some(DecoderMetadata::default());
    }

    /// Test-only: seed a playlist that has not been published through the
    /// framework resource yet.
    #[cfg(test)]
    pub(crate) fn seed_unpublished_playlist_for_test(&mut self, id: &str, name: &str) {
        self.shared_library.playlists.push(crate::shared::Playlist {
            id: id.into(),
            name: name.into(),
        });
    }

    /// Test-only: shared-library playlist ids currently held on the surface.
    #[cfg(test)]
    pub(crate) fn shared_playlist_ids_for_test(&self) -> Vec<String> {
        self.shared_library
            .playlists
            .iter()
            .map(|playlist| playlist.id.clone())
            .collect()
    }

    /// Show the playlists and annotations the framework resource holds.
    /// An empty projection does not wipe a library that has not been published yet.
    pub(crate) fn adopt_shared_library(&mut self, state: &crate::shared::SharedLibrary) {
        let incoming_empty =
            state.playlists.is_empty() && state.items.is_empty() && state.annotations.is_empty();
        let local_has_rows = !self.shared_library.playlists.is_empty()
            || !self.shared_library.items.is_empty()
            || !self.shared_library.annotations.is_empty();
        if incoming_empty && (local_has_rows || self.shot_shared) {
            return;
        }
        if state != &self.shared_library {
            self.shared_library = state.clone();
            self.invalidate_search_cache();
            self.dirty = true;
        }
    }

    /// Attach the framework dispatcher when hosted as a [`ResourceSurface`].
    pub(crate) fn attach_framework(
        &mut self,
        dispatcher: ui::framework::Dispatcher<crate::resource::VideoPlayer>,
    ) {
        self.actions.attach(dispatcher);
    }

    /// Shared read access to the inner video surface (for the headless shot
    /// warm-up and integration tests that assert a decoded frame landed).
    pub fn player(&self) -> &VideoPlayer {
        &self.player
    }

    /// Whether a decoded picture is currently on the surface. The `--shots`
    /// path pumps `sync` until this is true so a real (asynchronously prerolled)
    /// decode is captured, not a blank frame.
    pub fn has_media_frame(&self) -> bool {
        self.player.latest_frame().is_some()
    }

    fn open_path(&mut self, path: &Path) -> Result<(), String> {
        let source = source_for_path(path);
        let title = path
            .file_name()
            .and_then(|name| name.to_str())
            .filter(|name| !name.is_empty())
            .unwrap_or("selected video")
            .to_string();
        let queue_id = self.ensure_queued(source.clone(), title.clone());
        self.open_local_source(source, title, Some(queue_id))
    }

    /// Open a video the host handed over as bytes (a browser page has no
    /// paths to give).
    fn open_contents(&mut self, file: &PickedContents) -> Result<(), String> {
        let source = media::Source::from_contents(&file.name, &file.bytes)
            .map_err(|e| format!("could not stage {}: {e}", file.name))?;
        let title = if file.name.is_empty() {
            "selected video".to_string()
        } else {
            file.name.clone()
        };
        let queue_id = self.ensure_queued(source.clone(), title.clone());
        self.open_local_source(source, title, Some(queue_id.clone()))?;
        if !crate::kept::native_paths() {
            self.keep_contents(&queue_id, file);
        }
        Ok(())
    }

    /// Keep a video opened by contents for Recent, or say why it is not.
    fn keep_contents(&mut self, queue_id: &str, file: &PickedContents) {
        if file.bytes.len() > crate::kept::KEEP_LIMIT {
            if let Some(entry) = self.queue.iter_mut().find(|entry| entry.id == queue_id) {
                entry.title = format!(
                    "{} — too large to reopen after a reload (over {} MB)",
                    entry.title,
                    crate::kept::KEEP_LIMIT / (1024 * 1024)
                );
            }
            self.invalidate_search_cache();
            return;
        }
        let token = crate::kept::token(&file.name, file.bytes.len());
        self.kept_retained.insert(0, token.clone());
        self.kept_retained.truncate(crate::kept::KEPT_VIDEOS);
        self.kept.insert(queue_id.to_owned(), token.clone());
        drop(crate::kept::keep(
            token,
            file.bytes.clone(),
            self.kept_retained.clone(),
        ));
    }

    /// Start reading a kept copy back; the entry opens once it is here.
    fn recall_kept(&mut self, token: &str, title: String, queue_id: Option<String>) {
        let wake = self.kept_wake.clone();
        self.kept_recall = Some(KeptRecall {
            bytes: crate::kept::recall(token.to_owned(), move || wake.wake()),
            token: token.to_owned(),
            title,
            queue_id,
        });
        self.open_error = None;
        self.dirty = true;
    }

    fn poll_kept_recall(&mut self) {
        let Some(recall) = self.kept_recall.as_ref() else {
            return;
        };
        let bytes = match recall.bytes.try_recv() {
            Ok(bytes) => bytes,
            Err(std::sync::mpsc::TryRecvError::Empty) => return,
            Err(std::sync::mpsc::TryRecvError::Disconnected) => None,
        };
        let Some(recall) = self.kept_recall.take() else {
            return;
        };
        let opened = bytes
            .ok_or_else(|| {
                format!(
                    "the kept copy of {} is gone; open the file again",
                    recall.title
                )
            })
            .and_then(|bytes| {
                media::Source::from_contents(&recall.title, &bytes)
                    .map_err(|e| format!("could not stage {}: {e}", recall.title))
            });
        match opened {
            Ok(source) => {
                if let Some(queue_id) = &recall.queue_id {
                    self.kept.insert(queue_id.clone(), recall.token);
                }
                let _ = self.open_local_source(source, recall.title, recall.queue_id);
            }
            Err(error) => {
                self.open_error = Some(error);
                self.dirty = true;
            }
        }
    }

    fn ensure_queued(&mut self, source: media::Source, title: String) -> String {
        if let Some(entry) = self.queue.iter().find(|entry| {
            media::session::session_key(&entry.source) == media::session::session_key(&source)
        }) {
            return entry.id.clone();
        }
        let id = format!("local-queue-{}", local_source_token(&source, 0));
        self.queue.push(LocalMediaEntry {
            id: id.clone(),
            title,
            source,
        });
        self.invalidate_search_cache();
        id
    }

    fn open_local_entry(&mut self, item_id: &str) -> Result<(), String> {
        let entry = self
            .queue
            .iter()
            .chain(self.recent.iter())
            .find(|entry| entry.id == item_id)
            .cloned()
            .ok_or_else(|| "that local queue item is no longer available".to_string())?;
        self.open_local_source(entry.source, entry.title, Some(entry.id))
    }

    fn open_local_source(
        &mut self,
        source: media::Source,
        title: String,
        queue_id: Option<String>,
    ) -> Result<(), String> {
        if let Some(token) = crate::kept::token_of(&source) {
            let token = token.to_owned();
            self.recall_kept(&token, title, queue_id);
            return Ok(());
        }
        if let Err(error) = self.set_current_source(&source) {
            self.open_error = Some(error.clone());
            self.dirty = true;
            return Err(error);
        }
        self.cancel_pending_portal("a direct video open replaced the file picker");
        if let Some(pending) = self.media_open.take() {
            self.queue_resource_result(tilo_actions::InvocationUpdate::Cancelled {
                invocation: pending.invocation,
                message: Some("a newer video open replaced this decoder request".into()),
            });
        }
        self.open_error = None;
        self.invalidate_search_cache();
        self.title = title;
        self.current_queue_id = queue_id;
        self.player.open(source);
        if self.preferences.autoplay {
            self.player.play();
        } else {
            self.player.pause();
        }
        self.player.reveal_controls();
        self.player
            .set_now_playing(ui::widget::video_player::NowPlaying {
                // MPRIS/session metadata must not disclose a portal-granted
                // absolute path. The title above is just the user-facing file
                // name; this stable opaque ID is the machine identity.
                track_id: self.current_source_id.clone().unwrap_or_default(),
                title: self.title.clone(),
                artist: String::new(),
                art_url: String::new(),
            });
        self.dirty = true;
        Ok(())
    }

    fn record_current_history(&mut self) {
        let Some(queue_id) = self.current_queue_id.as_deref() else {
            return;
        };
        let Some(entry) = self
            .queue
            .iter()
            .find(|entry| entry.id == queue_id)
            .cloned()
        else {
            return;
        };
        self.recent.retain(|item| item.id != entry.id);
        self.recent.push_front(entry);
        self.recent
            .truncate(crate::preferences::RECENT_HISTORY_LIMIT);
        if let Err(error) = self.persist_recent_history() {
            tracing::warn!(%error, "video player: could not persist recent history");
        }
        self.invalidate_search_cache();
        self.dirty = true;
    }

    fn persist_recent_history(&self) -> std::io::Result<()> {
        // A page's `blob:` source dies with the page: an entry is kept by its
        // kept copy, while that copy is retained, or not at all.
        let entries: Vec<RecentMedia> = self
            .recent
            .iter()
            .filter_map(|entry| {
                let source = match self.kept.get(&entry.id) {
                    Some(token) => crate::kept::source(token),
                    None => entry.source.clone(),
                };
                let retained = crate::kept::token_of(&source)
                    .is_none_or(|token| self.kept_retained.iter().any(|kept| kept == token));
                (retained && !crate::kept::page_local(&source)).then(|| RecentMedia {
                    id: entry.id.clone(),
                    title: entry.title.clone(),
                    source,
                })
            })
            .collect();
        crate::preferences::save_recent_history(&entries)
    }

    fn clear_recent_history(&mut self) -> Result<(), String> {
        let previous = self.recent.clone();
        self.recent.clear();
        if let Err(error) = self.persist_recent_history() {
            self.recent = previous;
            return Err(format!("could not clear local recent history: {error}"));
        }
        if !self.kept_retained.is_empty() {
            self.kept.clear();
            self.kept_retained.clear();
            crate::kept::forget_all();
        }
        self.invalidate_search_cache();
        self.dirty = true;
        Ok(())
    }

    fn remove_queue_entry(&mut self, item_id: &str) -> Result<(), String> {
        let Some(index) = self.queue.iter().position(|entry| entry.id == item_id) else {
            return Err("that local queue item is no longer available".into());
        };
        self.queue.remove(index);
        if self.current_queue_id.as_deref() == Some(item_id) {
            self.current_queue_id = None;
        }
        self.invalidate_search_cache();
        self.dirty = true;
        Ok(())
    }

    fn advance_after_engine_end(&mut self) {
        if self.preferences.loop_enabled {
            return;
        }
        let Some(current) = self.current_queue_id.as_deref() else {
            return;
        };
        let Some(index) = self.queue.iter().position(|entry| entry.id == current) else {
            return;
        };
        let Some(next) = self.queue.get(index.saturating_add(1)) else {
            return;
        };
        let next_id = next.id.clone();
        if let Err(error) = self.open_local_entry(&next_id) {
            self.open_error = Some(error);
            self.dirty = true;
        }
    }

    /// Assign the next never-reused source identity. Once the u64 namespace is
    /// exhausted the app refuses a new source rather than making an action
    /// address from the start of the process current again.
    fn set_current_source(&mut self, source: &media::Source) -> Result<(), String> {
        let generation = self.source_generation.checked_add(1).ok_or_else(|| {
            "video source identity generation exhausted; restart Video Player".to_string()
        })?;
        self.source_generation = generation;
        self.current_source_id = Some(local_source_token(source, generation));
        self.current_media_ref = Some(local_source_token(source, 0));
        Ok(())
    }

    /// Invalidate the bounded search catalog before advancing its revision.
    /// Resetting the revision at the numeric boundary is safe only because the
    /// old provider is explicitly discarded first, so its `0` can never be
    /// mistaken for the fresh post-boundary catalog's `0`.
    fn invalidate_search_cache(&mut self) {
        self.search_cache.get_mut().take();
        self.metadata_revision = self.metadata_revision.checked_add(1).unwrap_or_default();
    }

    fn close_media(&mut self) {
        if let Some(pending) = self.media_open.take() {
            self.queue_resource_result(tilo_actions::InvocationUpdate::Cancelled {
                invocation: pending.invocation,
                message: Some("video was closed before decoding a first frame".into()),
            });
        }
        let mut player =
            configured_player(self.preferences.autoplay, self.preferences.loop_enabled);
        player.set_transport_enabled(
            self.collab
                .as_ref()
                .is_none_or(|collab| collab.role() == Role::Host),
        );
        if self
            .collab
            .as_ref()
            .is_some_and(|collab| collab.role() == Role::Follower)
        {
            player.remove_setting("loop");
        }
        self.player = player;
        self.title = "No media".into();
        self.current_source_id = None;
        self.current_media_ref = None;
        self.current_queue_id = None;
        self.invalidate_search_cache();
        self.dirty = true;
    }

    fn request_picture_in_picture(&mut self) {
        if !self.player.has_media() || self.player.get_rect().width() < 1.0 {
            return;
        }
        let region = self.player.get_rect();
        let aspect = (region.width() / region.height().max(1.0)).clamp(0.4, 2.8);
        self.pending_pip = Some(ui::floating_surface::FloatingSurfaceRequest {
            id: "video-player.current-video".into(),
            title: self.title.clone(),
            role: ui::floating_surface::FloatingRole::PictureInPicture,
            source: ui::floating_surface::FloatingSource::AppElement {
                app_id: "so.tilo.video-player".into(),
                element_id: "video_surface".into(),
                region,
            },
            preferred_size: vello::kurbo::Size::new(360.0, 360.0 / aspect),
            aspect_ratio: Some(aspect),
            initial_rect: None,
            presentation_rect: None,
            tracking_source: None,
        });
        self.dirty = true;
    }

    fn request_open_action(&mut self) {
        if let Some(action) = &mut self.open_action {
            let _ = action.emit(());
        }
    }

    fn request_library_play(&mut self, item_id: &str) {
        let resource_id = if self.queue.iter().any(|entry| entry.id == item_id) {
            crate::actions::queue_item_resource_id(item_id)
        } else {
            crate::actions::recent_item_resource_id(item_id)
        };
        self.request_library_action(&resource_id, "play");
    }

    fn request_library_remove(&mut self, item_id: &str) {
        self.request_library_action(&crate::actions::queue_item_resource_id(item_id), "remove");
    }

    fn request_library_clear(&mut self) {
        self.request_library_action("video-player.history.local", "clear");
    }

    fn request_library_hide(&mut self) {
        self.request_library_action("video-player.application", "hide-library");
    }

    fn request_queue_library(&mut self) {
        self.request_library_action("video-player.queue.local", "show");
    }

    fn request_recent_library(&mut self) {
        self.request_library_action("video-player.history.local", "show");
    }

    fn request_shared_library(&mut self) {
        self.request_library_action("video-player.playlists.shared", "show");
    }

    /// The Library button and `L` shortcut choose a concrete, replay-safe
    /// action from the current presentation. We never publish a toggle: a
    /// replay or delayed agent invocation must not invert a newer user choice.
    fn request_library_visibility_change(&mut self) {
        if self.library_visible {
            self.request_library_hide();
        } else {
            match self.library_tab {
                LibraryTab::Queue => self.request_queue_library(),
                LibraryTab::Recent => self.request_recent_library(),
                LibraryTab::Shared => self.request_shared_library(),
            }
        }
    }

    /// Route marker navigation through the same canonical resource action used
    /// by global search and agents. Shared marker resources live in the
    /// document, so this resolves them against the active document rather than
    /// manufacturing a local playback closure.
    fn request_annotation_jump(&mut self, doc: &AutoCommit, annotation_id: &str) {
        let (audio, subtitles, chapters) = self.decoder_metadata();
        let context = self.action_context(&audio, &subtitles, &chapters);
        let resource_id = crate::actions::annotation_resource_id(annotation_id);
        let Some(resource) = crate::actions::resource(&context, doc, &resource_id) else {
            return;
        };
        let Ok(binding) = tilo_actions::ActionBinding::unit_for(&resource, "jump") else {
            return;
        };
        if let Ok(invocation) = binding.invoke(()) {
            self.library_control_invocations.push_back(invocation);
        }
    }

    fn dispatch_shared(&mut self, doc: &mut AutoCommit, action: &crate::shared::SharedAction) {
        let context = crate::shared::history_context(action);
        self.actions
            .dispatch_with_context(doc, crate::shared::ACTION_TYPE, action, context);
        self.shared_library = crate::shared::read(doc);
        self.invalidate_search_cache();
        self.dirty = true;
    }

    fn add_current_to_watch_later(&mut self, doc: &mut AutoCommit) {
        let Some(media_ref) = self.current_media_ref.clone() else {
            return;
        };
        let playlist_id = "watch-later".to_string();
        if !self
            .shared_library
            .playlists
            .iter()
            .any(|playlist| playlist.id == playlist_id)
        {
            self.dispatch_shared(
                doc,
                &crate::shared::SharedAction::CreatePlaylist {
                    id: playlist_id.clone(),
                    name: "Watch later".into(),
                },
            );
        }
        if self
            .shared_library
            .items
            .iter()
            .any(|item| item.playlist_id == playlist_id && item.media_ref == media_ref)
        {
            return;
        }
        let position = self
            .shared_library
            .items
            .iter()
            .filter(|item| item.playlist_id == playlist_id)
            .map(|item| item.position)
            .max()
            .unwrap_or(0)
            .saturating_add(1024);
        let id = format!(
            "item-{}-{}",
            crate::actions::identity_token_for_action(&media_ref),
            unix_now_ms()
        );
        self.dispatch_shared(
            doc,
            &crate::shared::SharedAction::AddPlaylistItem {
                id,
                playlist_id,
                media_ref,
                title: self.title.clone(),
                position,
            },
        );
    }

    fn bookmark_current_time(&mut self, doc: &mut AutoCommit) {
        let Some(media_ref) = self.current_media_ref.clone() else {
            return;
        };
        let time_ms = self.player.position_ms();
        let id = format!(
            "annotation-{}-{}-{time_ms}",
            crate::actions::identity_token_for_action(&media_ref),
            unix_now_ms()
        );
        self.dispatch_shared(
            doc,
            &crate::shared::SharedAction::CreateAnnotation {
                id,
                media_ref,
                time_ms,
                text: String::new(),
            },
        );
    }

    fn request_library_action(&mut self, resource_id: &str, action_id: &str) {
        let (audio, subtitles, chapters) = self.decoder_metadata();
        let resources = crate::actions::resources(
            &self.action_context(&audio, &subtitles, &chapters),
            &AutoCommit::new(),
        );
        let Some(resource) = find_resource(&resources, resource_id) else {
            return;
        };
        let Ok(binding) = tilo_actions::ActionBinding::unit_for(resource, action_id) else {
            return;
        };
        if let Ok(invocation) = binding.invoke(()) {
            self.library_control_invocations.push_back(invocation);
        }
    }

    /// Kick off a portal file-open (or the immediate "no portal" outcome in the
    /// hermetic build). A second request is rejected instead of replacing the
    /// receiver for the first request, which would make the result race-prone.
    fn trigger_open(&mut self) -> Result<(), String> {
        if self.portal_open.is_some() {
            return Err("a video picker is already open".into());
        }
        // Do not accept a new background operation while a prior accepted
        // operation still has a completion for the host. Keeping the result is
        // more important than accepting unbounded work: dropping it would
        // strand that invocation forever.
        if !self.resource_results.is_empty() {
            return Err(
                "a previous video-picker completion is awaiting host acknowledgement".into(),
            );
        }
        let generation = self.portal_generation.checked_add(1).ok_or_else(|| {
            "video-picker request generation exhausted; restart Video Player".to_string()
        })?;
        self.portal_generation = generation;
        self.open_error = None;
        self.portal_open = Some(PendingPortalOpen {
            channel: if tilo_paths::availability(tilo_paths::Feature::NativePaths)
                == tilo_paths::Availability::Available
            {
                PickerChannel::Path(video_open_request().send())
            } else {
                PickerChannel::Contents(video_open_request().send_contents())
            },
            generation,
            invocation: None,
        });
        self.dirty = true;
        Ok(())
    }

    /// Drop a portal receiver that no longer represents the user's active
    /// intent. The portal may still complete in the desktop service, but its
    /// response has no receiver and therefore cannot mutate this player.
    fn cancel_pending_portal(&mut self, message: &str) {
        let Some(pending) = self.portal_open.take() else {
            return;
        };
        if let Some(invocation) = pending.invocation {
            self.queue_resource_result(tilo_actions::InvocationUpdate::Cancelled {
                invocation,
                message: Some(message.into()),
            });
        }
    }

    fn await_open_frame(&mut self, invocation: tilo_actions::InvocationId) -> Result<(), String> {
        if self.media_open.is_some() {
            return Err("another video open is still waiting for decoder output".into());
        }
        let source_id = self
            .current_source_id
            .clone()
            .ok_or_else(|| "video source did not receive an identity".to_string())?;
        self.media_open = Some(PendingMediaOpen {
            invocation,
            source_id,
            deadline: ui::time::now() + OPEN_FRAME_TIMEOUT,
        });
        Ok(())
    }

    fn poll_open_frame(
        &mut self,
        invocation: tilo_actions::InvocationId,
    ) -> Option<tilo_actions::InvocationUpdate> {
        let pending = self.media_open.as_ref()?;
        if pending.invocation != invocation {
            return None;
        }
        if self.current_source_id.as_deref() != Some(pending.source_id.as_str()) {
            let pending = self.media_open.take()?;
            return Some(tilo_actions::InvocationUpdate::Failed {
                invocation: pending.invocation,
                error: tilo_actions::ActionFailure {
                    code: "video_player.open_superseded".into(),
                    message: "The requested video was superseded before decoding a frame.".into(),
                    details: None,
                    retryable: true,
                },
            });
        }
        if self.player.latest_frame().is_some() {
            let pending = self.media_open.take()?;
            self.record_current_history();
            return Some(tilo_actions::InvocationUpdate::Succeeded {
                invocation: pending.invocation,
                output: tilo_actions::ActionOutput::default(),
            });
        }
        if ui::time::now() >= pending.deadline {
            let pending = self.media_open.take()?;
            let message = "The decoder did not produce a first frame in time.".to_string();
            self.open_error = Some(message.clone());
            self.dirty = true;
            return Some(tilo_actions::InvocationUpdate::Failed {
                invocation: pending.invocation,
                error: tilo_actions::ActionFailure {
                    code: "video_player.open_timed_out".into(),
                    message,
                    details: Some(serde_json::json!({
                        "timeout_seconds": OPEN_FRAME_TIMEOUT.as_secs(),
                    })),
                    retryable: true,
                },
            });
        }
        None
    }

    /// Queue an action completion without letting a newer action overwrite an
    /// older invocation that the host has not polled yet. `trigger_open`
    /// applies backpressure before accepting another asynchronous request, so
    /// every accepted invocation keeps its result until it is collected.
    fn queue_resource_result(&mut self, update: tilo_actions::InvocationUpdate) {
        self.resource_results.push_back(update);
    }

    #[allow(
        clippy::too_many_lines,
        reason = "all operation variants remain together so their local semantics cannot drift"
    )]
    fn perform_resource_operation(&mut self, operation: crate::actions::PlayerOperation) {
        use crate::actions::PlayerOperation;
        match operation {
            PlayerOperation::Open { path: None } => {
                // `answer_resource_operation` starts portal opens so it can
                // report a refusal to agents; this branch is retained for the
                // in-app path and intentionally ignores an already-reported
                // failure rather than panicking.
                let _ = self.trigger_open();
            }
            PlayerOperation::Open { path: Some(path) } => {
                if let Err(error) = self.open_path(Path::new(&path)) {
                    tracing::warn!(%error, "video player: could not open direct path");
                }
            }
            PlayerOperation::OpenQueueItem { item_id } => {
                if let Err(error) = self.open_local_entry(&item_id) {
                    tracing::warn!(%error, "video player: could not open queued item");
                }
            }
            PlayerOperation::RemoveQueueItem { item_id } => {
                if let Err(error) = self.remove_queue_entry(&item_id) {
                    tracing::warn!(%error, "video player: could not remove queued item");
                }
            }
            PlayerOperation::ClearRecentHistory => {
                if let Err(error) = self.clear_recent_history() {
                    tracing::warn!(%error, "video player: could not clear recent history");
                }
            }
            PlayerOperation::RevealLibrary => {
                self.library_visible = true;
                self.library_tab = LibraryTab::Queue;
                self.player.reveal_controls();
            }
            PlayerOperation::RevealRecentLibrary => {
                self.library_visible = true;
                self.library_tab = LibraryTab::Recent;
                self.player.reveal_controls();
            }
            PlayerOperation::RevealSharedLibrary => {
                self.library_visible = true;
                self.library_tab = LibraryTab::Shared;
                self.player.reveal_controls();
            }
            PlayerOperation::HideLibrary => {
                self.library_visible = false;
                self.library_focus_id = None;
            }
            PlayerOperation::Close => self.close_media(),
            PlayerOperation::TogglePlayback => self.player.toggle_play(),
            PlayerOperation::ToggleMute => self.player.toggle_mute(),
            PlayerOperation::Reveal => self.player.reveal_controls(),
            PlayerOperation::SeekSeconds(seconds)
            | PlayerOperation::JumpToChapter { seconds, .. } => {
                self.player.seek_to_ms((seconds.max(0.0) * 1000.0) as u64);
            }
            PlayerOperation::JumpToAnnotation { time_ms, .. } => {
                self.player.seek_to_ms(time_ms);
                self.player.reveal_controls();
            }
            PlayerOperation::SeekBySeconds(seconds) => {
                let current = self.player.position_ms() as f64;
                let duration = self.player.duration_ms() as f64;
                let target =
                    (current + seconds.clamp(-3600.0, 3600.0) * 1000.0).clamp(0.0, duration) as u64;
                self.player.seek_to_ms(target);
            }
            PlayerOperation::Restart => self.player.seek_to_ms(0),
            PlayerOperation::SeekToEnd => self.player.seek_to_ms(self.player.duration_ms()),
            PlayerOperation::SetVolume(volume) => self.player.set_volume(volume as f32),
            PlayerOperation::SetAutoplay(enabled) => {
                if self.preferences.autoplay != enabled {
                    self.preferences.autoplay = enabled;
                    configure_player_settings(
                        &mut self.player,
                        self.preferences.autoplay,
                        self.preferences.loop_enabled,
                    );
                    self.persist_preferences();
                }
            }
            PlayerOperation::SetLoop(enabled) => {
                if self.preferences.loop_enabled != enabled {
                    self.preferences.loop_enabled = enabled;
                    configure_player_settings(
                        &mut self.player,
                        self.preferences.autoplay,
                        self.preferences.loop_enabled,
                    );
                    self.persist_preferences();
                }
            }
            PlayerOperation::SetPlaybackRate(rate) => {
                if let Some(engine) = self.player.engine_mut() {
                    engine.set_rate(rate as f32);
                }
                let selected = [0.25_f64, 0.5, 0.75, 1.0, 1.25, 1.5, 2.0, 4.0]
                    .iter()
                    .position(|candidate| (*candidate - rate).abs() < f64::EPSILON);
                if let Some(selected) = selected {
                    self.player.set_choice_selection("playback-rate", selected);
                }
            }
            PlayerOperation::OpenPictureInPicture => self.request_picture_in_picture(),
            PlayerOperation::SelectAudioTrack { index, .. } => {
                if !self
                    .player
                    .engine_mut()
                    .is_some_and(|engine| engine.select_audio_track(index))
                {
                    tracing::warn!(index, "video player: refused stale audio-track selection");
                }
            }
            PlayerOperation::SelectSubtitleTrack { target_index, .. } => {
                if !self
                    .player
                    .engine_mut()
                    .is_some_and(|engine| engine.select_subtitle_track(target_index))
                {
                    tracing::warn!(
                        ?target_index,
                        "video player: refused stale subtitle-track selection"
                    );
                }
            }
            PlayerOperation::ToggleFullscreen => self.player.toggle_fullscreen(),
        }
        self.dirty = true;
    }

    fn validate_dynamic_target(&self, operation: &PlayerOperation) -> Result<(), String> {
        let (audio, subtitles, chapters) = self.decoder_metadata();
        match operation {
            PlayerOperation::OpenQueueItem { item_id } => {
                if !self.queue.iter().any(|entry| entry.id == *item_id)
                    && !self.recent.iter().any(|entry| entry.id == *item_id)
                {
                    return Err("that local queue item changed; refresh and try again".into());
                }
            }
            PlayerOperation::RemoveQueueItem { item_id } => {
                if !self.queue.iter().any(|entry| entry.id == *item_id) {
                    return Err("that local queue item changed; refresh and try again".into());
                }
            }
            PlayerOperation::SelectAudioTrack {
                source_id,
                stream_id,
                index,
            } => {
                if self.current_source_id.as_deref() != Some(source_id)
                    || !audio
                        .iter()
                        .any(|track| track.index == *index && track.stream_id == *stream_id)
                {
                    return Err("the audio track changed; refresh search and try again".into());
                }
            }
            PlayerOperation::SelectSubtitleTrack {
                source_id,
                target_stream_id,
                target_index,
                expected_stream_id,
                expected_index,
            } => {
                if target_index.is_some() != target_stream_id.is_some()
                    || expected_index.is_some() != expected_stream_id.is_some()
                {
                    return Err(
                        "subtitle target was malformed; refresh search and try again".into(),
                    );
                }
                let desired = target_index.zip(target_stream_id.as_deref());
                let expected = expected_index.zip(expected_stream_id.as_deref());
                let actual = subtitles
                    .iter()
                    .find(|track| track.selected)
                    .map(|track| (track.index, track.stream_id.as_str()));
                if self.current_source_id.as_deref() != Some(source_id)
                    || desired.is_some_and(|(index, stream_id)| {
                        !subtitles
                            .iter()
                            .any(|track| track.index == index && track.stream_id == stream_id)
                    })
                    || actual != expected
                {
                    return Err("the subtitle track changed; refresh search and try again".into());
                }
            }
            PlayerOperation::JumpToChapter {
                source_id,
                id,
                seconds,
            } if self.current_source_id.as_deref() != Some(source_id)
                || !chapters.iter().any(|chapter| {
                    chapter.id == *id && (chapter.seconds - *seconds).abs() < f64::EPSILON
                }) =>
            {
                return Err("the chapter changed; refresh search and try again".into());
            }
            PlayerOperation::JumpToAnnotation { media_ref, .. }
                if self.current_media_ref.as_deref() != Some(media_ref) =>
            {
                return Err(
                    "this review marker belongs to a different video on this device".into(),
                );
            }
            _ => {}
        }
        Ok(())
    }

    /// Drain a pending portal outcome, opening the chosen file.
    #[allow(
        clippy::too_many_lines,
        reason = "each answer a picker can give, handled in one place"
    )]
    fn drain_portal(&mut self) {
        let pending = self.portal_open.take();
        let mut outcome: Option<PortalOpenOutcome> = None;
        if let Some(request) = pending.as_ref() {
            let received = match &request.channel {
                PickerChannel::Path(channel) => channel.try_recv().map(|answer| match answer {
                    PortalOutcome::Opened(p) => Ok(Picked::Path(p)),
                    PortalOutcome::Cancelled => Err(None),
                    PortalOutcome::Failed(error) => Err(Some(error)),
                }),
                PickerChannel::Contents(channel) => channel.try_recv().map(|answer| match answer {
                    ContentsOutcome::Picked(files) => files
                        .into_iter()
                        .next()
                        .map(Picked::Contents)
                        .ok_or_else(|| Some("the picker returned no file".into())),
                    ContentsOutcome::Cancelled => Err(None),
                    ContentsOutcome::Failed(error) => Err(Some(error)),
                }),
            };
            match received {
                Ok(result) => {
                    outcome = Some((request.generation, request.invocation, result));
                }
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    outcome = Some((
                        request.generation,
                        request.invocation,
                        Err(Some("video picker disconnected".into())),
                    ));
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => {}
            }
        }
        if outcome.is_none() {
            self.portal_open = pending;
            return;
        }
        // The receiver is intentionally dropped before opening the selected
        // path. `open_path` then cannot cancel the just-completed invocation.
        if let Some((generation, invocation, outcome)) = outcome {
            if generation != self.portal_generation {
                tracing::warn!(
                    generation,
                    current_generation = self.portal_generation,
                    "video player: ignored a stale portal outcome"
                );
                if let Some(invocation) = invocation {
                    self.queue_resource_result(tilo_actions::InvocationUpdate::Failed {
                        invocation,
                        error: tilo_actions::ActionFailure {
                            code: "video_player.stale_open_response".into(),
                            message: "ignored a stale video picker response; open again".into(),
                            details: None,
                            retryable: true,
                        },
                    });
                }
                return;
            }
            let result = match outcome {
                Ok(Picked::Path(path)) => self.open_path(&path).map(|()| true),
                Ok(Picked::Contents(file)) => self.open_contents(&file).map(|()| true),
                Err(None) => Ok(false),
                Err(Some(e)) => {
                    self.open_error = Some(e.clone());
                    self.dirty = true;
                    Err(e)
                }
            };
            if let Some(invocation) = invocation {
                match result {
                    Ok(true) => {
                        // Picker acceptance is not decoder success. Retain the
                        // invocation until `poll_open_frame` observes a first
                        // decoded picture (or a truthful timeout/failure).
                        if let Err(message) = self.await_open_frame(invocation) {
                            self.queue_resource_result(tilo_actions::InvocationUpdate::Failed {
                                invocation,
                                error: tilo_actions::ActionFailure {
                                    code: "video_player.open_lifecycle_failed".into(),
                                    message,
                                    details: None,
                                    retryable: true,
                                },
                            });
                        }
                    }
                    Ok(false) => {
                        self.queue_resource_result(tilo_actions::InvocationUpdate::Cancelled {
                            invocation,
                            message: Some("video selection was cancelled".into()),
                        });
                    }
                    Err(message) => {
                        self.queue_resource_result(tilo_actions::InvocationUpdate::Failed {
                            invocation,
                            error: tilo_actions::ActionFailure {
                                code: "video_player.open_failed".into(),
                                message,
                                details: None,
                                retryable: false,
                            },
                        });
                    }
                }
            }
        }
    }

    /// Pump the collab transport: apply inbound host state to a follower, or
    /// broadcast the host's changing playback.
    fn tick_collab(&mut self) {
        let (Some(mgr), Some(collab)) = (self.manager.as_mut(), self.collab.as_mut()) else {
            return;
        };
        for ev in mgr.poll() {
            if let CollabEvent::Document { .. } = &ev {
                match ev.document_mutation() {
                    Some(Ok(collab_types::DocumentMutation::Incremental(data))) => {
                        collab.ingest(&data);
                    }
                    Some(Ok(collab_types::DocumentMutation::Snapshot(data))) => {
                        collab.ingest_snapshot(&data);
                    }
                    Some(Err(error)) => {
                        tracing::warn!(%error, "video collab: invalid document mutation");
                    }
                    None => {}
                }
            }
        }
        let now = unix_now_ms();
        match collab.role() {
            Role::Host => {
                // The manager connects in the background; the host's state
                // is stepped once it can reach followers, so the first delta
                // after connecting carries everything since.
                if mgr.is_connected()
                    && let Some(engine) = self.player.engine()
                    && let Some(delta) = collab.host_step(engine, now)
                {
                    mgr.send(CollabRequest::document_incremental(delta));
                }
            }
            Role::Follower => {
                if let Some(engine) = self.player.engine_mut() {
                    collab.follow_step(engine, now);
                }
            }
        }
    }

    fn drain_player_settings(&mut self) {
        for event in self.player.take_setting_events() {
            match event {
                PlayerSettingEvent::Toggled { id, on } if id == "autoplay" => {
                    self.perform_resource_operation(PlayerOperation::SetAutoplay(on));
                }
                PlayerSettingEvent::Toggled { id, on } if id == "loop" => {
                    if self
                        .collab
                        .as_ref()
                        .is_none_or(|collab| collab.role() == Role::Host)
                    {
                        self.perform_resource_operation(PlayerOperation::SetLoop(on));
                    }
                }
                PlayerSettingEvent::Chose { id, index } if id == "playback-rate" => {
                    const RATES: [f32; 8] = [0.25, 0.5, 0.75, 1.0, 1.25, 1.5, 2.0, 4.0];
                    if self
                        .collab
                        .as_ref()
                        .is_none_or(|collab| collab.role() == Role::Host)
                        && let Some(rate) = RATES.get(index)
                    {
                        self.perform_resource_operation(PlayerOperation::SetPlaybackRate(
                            f64::from(*rate),
                        ));
                    }
                }
                PlayerSettingEvent::Chose { id, index } if id == "audio-track" => {
                    let (audio_tracks, _, _) = self.decoder_metadata();
                    if let Some(track) = audio_tracks.get(index)
                        && let Some(source_id) = self.current_source_id.clone()
                    {
                        self.perform_resource_operation(PlayerOperation::SelectAudioTrack {
                            source_id,
                            stream_id: track.stream_id.clone(),
                            index: track.index,
                        });
                    }
                }
                PlayerSettingEvent::Chose { id, index } if id == "subtitle-track" => {
                    let (_, subtitle_tracks, _) = self.decoder_metadata();
                    let target = index
                        .checked_sub(1)
                        .and_then(|index| subtitle_tracks.get(index));
                    if let Some(source_id) = self.current_source_id.clone() {
                        let selected = subtitle_tracks.iter().find(|track| track.selected);
                        self.perform_resource_operation(PlayerOperation::SelectSubtitleTrack {
                            source_id,
                            target_stream_id: target.map(|track| track.stream_id.clone()),
                            target_index: target.map(|track| track.index),
                            expected_stream_id: selected.map(|track| track.stream_id.clone()),
                            expected_index: selected.map(|track| track.index),
                        });
                    }
                }
                PlayerSettingEvent::Chose { id, index } if id == "chapters" => {
                    let (_, _, chapters) = self.decoder_metadata();
                    if let Some(chapter) = chapters.get(index) {
                        self.perform_resource_operation(PlayerOperation::JumpToChapter {
                            source_id: self.current_source_id.clone().unwrap_or_default(),
                            id: chapter.id.clone(),
                            seconds: chapter.seconds,
                        });
                    }
                }
                PlayerSettingEvent::Activated { id } if id == "picture-in-picture" => {
                    self.perform_resource_operation(PlayerOperation::OpenPictureInPicture);
                }
                PlayerSettingEvent::Toggled { .. }
                | PlayerSettingEvent::Chose { .. }
                | PlayerSettingEvent::Activated { .. } => {}
            }
        }
    }

    /// Translate decoder-owned lifecycle events into app-visible state. The
    /// shared widget remains the single engine pump; this layer owns only the
    /// file-open contract and the user-facing error copy.
    fn drain_player_media_events(&mut self) {
        for event in self.player.take_media_events() {
            match event {
                media::MediaEvent::Error(message) => {
                    self.open_error = Some(message.clone());
                    self.dirty = true;
                    if let Some(pending) = self.media_open.take() {
                        self.queue_resource_result(tilo_actions::InvocationUpdate::Failed {
                            invocation: pending.invocation,
                            error: tilo_actions::ActionFailure {
                                code: "video_player.decoder_failed".into(),
                                message,
                                details: None,
                                retryable: true,
                            },
                        });
                    }
                }
                media::MediaEvent::Ended => {
                    // Queue advancement belongs to the app, but remains gated
                    // on a real engine end event rather than the current
                    // playhead looking close to its duration.
                    self.advance_after_engine_end();
                }
                media::MediaEvent::Buffering { .. }
                | media::MediaEvent::Position { .. }
                | media::MediaEvent::DurationKnown { .. } => {}
            }
        }
    }

    fn persist_preferences(&self) {
        if let Err(error) = self.preferences.save() {
            // A device-local preference must never prevent playback. The next
            // successful setting change can retry the atomic merged write.
            tracing::warn!(%error, "video player: could not persist playback preferences");
        }
    }

    fn maybe_loop_current_media(&mut self) {
        if !self.preferences.loop_enabled
            || !self.player.has_media()
            || self
                .collab
                .as_ref()
                .is_some_and(|collab| collab.role() == Role::Follower)
        {
            return;
        }
        let duration = self.player.duration_ms();
        if duration > 0 && self.player.position_ms() >= duration {
            self.player.seek_to_ms(0);
            self.player.play();
            self.dirty = true;
        }
    }

    /// Whether the floating overlay (media title + Open pill) should show this
    /// frame. It rides the player's own controls visibility, so it auto-hides
    /// during immersive playback — and it stays up in the empty state (no frame
    /// decoded yet) so there is always a way to open a file.
    fn overlay_visible(&self) -> bool {
        self.player.controls_visible() || self.player.latest_frame().is_none()
    }

    /// The floating top strip for the current rect and text scale — the single
    /// geometry `apply_layout`, `paint_overlay` and the a11y exports agree on.
    fn top_strip(&self) -> TopStrip {
        TopStrip::at(self.rect.y0, self.text_scale)
    }

    /// Place the Library and Open capsules on the strip's centre line. The
    /// **only** writer of `library_button_rect` / `open_button_rect`: called
    /// from `apply_layout`, and again from `draw` when a fresh label
    /// measurement changes the capsule widths, so hit-testing, focus, the
    /// accessibility export and paint always read the same rects.
    fn place_top_strip(&mut self) {
        let strip = self.top_strip();
        let labels = self
            .strip_labels
            .filter(|labels| (labels.text_scale - self.text_scale).abs() < f64::EPSILON);
        let by0 = strip.center_y - strip.button_h / 2.0;
        let by1 = strip.center_y + strip.button_h / 2.0;
        let bx1 = self.rect.x1 - OVERLAY_MARGIN;
        let open_w = capsule_w(OPEN_BTN_W, self.text_scale, labels.map(|l| l.open_w));
        let bx0 = (bx1 - open_w).max(self.rect.x0);
        self.open_button_rect = Rect::new(bx0, by0, bx1, by1);
        let lx0 = self.rect.x0 + OVERLAY_MARGIN;
        // Never let the Library rect invert on a surface too narrow for both
        // capsules: it is published to hit-testing, focus and accessibility.
        let library_w = capsule_w(LIBRARY_BTN_W, self.text_scale, labels.map(|l| l.library_w));
        let lx1 = (lx0 + library_w).min(bx0 - OVERLAY_MARGIN).max(lx0);
        self.library_button_rect = Rect::new(lx0, by0, lx1, by1);
    }

    /// Measure the two capsule labels with the frame's renderer (the one
    /// `Button::paint` will draw them with) and, if that differs from what
    /// `apply_layout` placed with, re-place the strip before painting.
    fn refresh_strip_labels(&mut self, text_renderer: &mut TextRenderer) {
        let measured = StripLabels {
            text_scale: self.text_scale,
            library_w: f64::from(text_renderer.measure("Library", BUTTON_LABEL_SIZE, None).0),
            open_w: f64::from(text_renderer.measure("Open", BUTTON_LABEL_SIZE, None).0),
        };
        if self.strip_labels != Some(measured) {
            self.strip_labels = Some(measured);
            self.place_top_strip();
        }
    }

    /// Place the library panel and its header controls. The **only** writer
    /// of the panel rects: `apply_layout` calls it, and `paint_library_panel`
    /// calls it again when a fresh label measurement changes the geometry.
    /// The panel's authored 360 px scales with the text so 200% rows keep
    /// their two-line shape; below 640 px it takes the whole surface.
    fn place_library_panel(&mut self) -> LibraryPanelGeometry {
        let panel_width = if self.rect.width() < 640.0 {
            self.rect.width()
        } else {
            (PANEL_WIDTH * self.text_scale).min(self.rect.width())
        };
        self.library_panel_rect = Rect::new(
            self.rect.x1 - panel_width,
            self.rect.y0,
            self.rect.x1,
            self.rect.y1,
        );
        let panel = LibraryPanelGeometry::for_panel(
            self.library_panel_rect,
            self.text_scale,
            self.panel_labels,
        );
        self.library_close_rect = panel.close;
        self.library_clear_rect = panel.clear;
        self.library_queue_tab_rect = panel.queue_tab;
        self.library_recent_tab_rect = panel.recent_tab;
        self.library_shared_tab_rect = panel.shared_tab;
        panel
    }

    /// Measure the panel's labels with the frame's renderer and, if that
    /// differs from what `apply_layout` placed with, re-place the panel
    /// before painting so hit rects and faces agree this frame.
    fn refresh_panel_labels(&mut self, text_renderer: &mut TextRenderer) -> LibraryPanelGeometry {
        let panel = self.library_panel_rect;
        let gutter_w = (panel.width() - 2.0 * PANEL_GUTTER).max(0.0) as f32;
        let width = |tr: &mut TextRenderer, label: &str, size: f32| {
            f64::from(tr.measure(label, size, None).0)
        };
        let measured = PanelLabels {
            text_scale: self.text_scale,
            panel_w: panel.width(),
            close_w: width(text_renderer, "Close", PANEL_HEADER_BUTTON_SIZE),
            clear_w: width(text_renderer, "Clear", PANEL_HEADER_BUTTON_SIZE),
            queue_w: width(text_renderer, "Queue", PANEL_TAB_SIZE),
            recent_w: width(text_renderer, "Recent", PANEL_TAB_SIZE),
            shared_w: width(text_renderer, "Shared", PANEL_TAB_SIZE),
            remove_w: width(text_renderer, "Remove", PANEL_ROW_CONTROL_SIZE),
            play_w: width(text_renderer, "Play", PANEL_ROW_CONTROL_SIZE).max(width(
                text_renderer,
                "Replay",
                PANEL_ROW_CONTROL_SIZE,
            )),
            subtitle_h: f64::from(
                text_renderer
                    .measure_weighted(
                        if self.library_tab == LibraryTab::Shared {
                            "Shared metadata · media access stays on each device"
                        } else {
                            PANEL_SUBTITLE
                        },
                        PANEL_SUBTITLE_SIZE,
                        Some(gutter_w),
                        TextWeight::Regular,
                    )
                    .1,
            ),
        };
        // `place_library_panel` is the one geometry writer; re-running it with
        // unchanged labels writes the same rects, so no second derivation.
        self.panel_labels = Some(measured);
        self.place_library_panel()
    }

    /// Paint the floating top overlay: the media title (left) over a soft scrim
    /// and the Open pill (right). Drawn on top of the video, only when
    /// [`Self::overlay_visible`]. There is no persistent chrome bar — the video
    /// is edge-to-edge (HIG §1e).
    fn paint_overlay(
        &self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
    ) {
        let opacity = if self.player.has_media() {
            self.player.controls_opacity()
        } else {
            1.0
        };
        if opacity <= 0.0 {
            return;
        }
        scene.push_layer(
            Fill::NonZero,
            BlendMode::default(),
            opacity,
            transform,
            &self.rect,
        );
        // A soft translucent scrim behind the title so it stays legible over
        // bright frames, without a hard opaque bar. Its height comes from the
        // same strip description that placed the buttons, so it always
        // contains them (they used to overhang a fixed 44 px scrim by 2 px).
        let strip = self.top_strip();
        let scrim = Rect::new(
            self.rect.x0,
            self.rect.y0,
            self.rect.x1,
            self.rect.y0 + strip.scrim_h,
        );
        // Over-media scrim from the theme's `surfaces.scrim` token (the shared
        // transport bar's scrim, so both window edges agree) with ink chosen
        // against it — it used to be a literal black with a literal
        // near-white title.
        let scrim_color = theme.tokens.colors.surfaces.scrim;
        let on_scrim = theme.on_surface(scrim_color);
        scene.fill(
            vello::peniko::Fill::NonZero,
            transform,
            scrim_color.multiply_alpha(STRIP_SCRIM_ALPHA),
            None,
            &scrim,
        );
        // Title (left), centred on the strip's one centre line — the line the
        // Library and Open buttons are centred on — using the line height the
        // renderer reports for this size rather than the font size.
        let display_title = if self.player.has_media() {
            self.title.as_str()
        } else {
            "Video Player"
        };
        let title_x = self.library_button_rect.x1 + STRIP_TITLE_GAP;
        let title_budget = (self.open_button_rect.x0 - title_x - OVERLAY_MARGIN)
            .max(0.0)
            .min(f64::from(f32::MAX)) as f32;
        let display_title = text_renderer.elide_to_width(display_title, TITLE_SIZE, title_budget);
        let (_, title_h) = text_renderer.measure(&display_title, TITLE_SIZE, None);
        let ty = strip.center_y - f64::from(title_h) / 2.0;
        text_renderer.draw_text(
            scene,
            &display_title,
            TITLE_SIZE,
            on_scrim.ink,
            transform * Affine::translate((title_x, ty)),
            None,
        );
        // Library is the shared Button's neutral face: its own raised fill
        // carries `text_primary`, so it reads on the dark scrim in every
        // theme (the ghost face drew `text_primary` straight onto the scrim —
        // near-black on black in light themes).
        if self.library_button_rect.width() > 0.0 {
            Button::paint(
                scene,
                transform,
                text_renderer,
                theme,
                &ButtonFace::new(self.library_button_rect, "Library")
                    .radius(self.library_button_rect.height() / 2.0)
                    .text_size(BUTTON_LABEL_SIZE)
                    .focused(self.library_focused)
                    .hovered(self.library_button_rect.contains(self.last_cursor)),
            );
        }
        // Open pill (right), a floating rounded accent button — the shared
        // primary Button face, so it gains hover/press feedback over the video.
        if self.open_button_rect.width() > 0.0 {
            Button::paint(
                scene,
                transform,
                text_renderer,
                theme,
                &ButtonFace::new(self.open_button_rect, "Open")
                    .radius(self.open_button_rect.height() / 2.0)
                    .text_size(BUTTON_LABEL_SIZE)
                    .weight(TextWeight::Regular)
                    .primary()
                    .focused(self.open_focused)
                    .hovered(self.open_button_rect.contains(self.last_cursor)),
            );
        }
        scene.pop_layer();
    }

    fn paint_media_status_copy(
        &self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
    ) {
        if (!self.shot_loading && self.player.latest_frame().is_some())
            || self.rect.height() < 180.0
        {
            return;
        }
        let center = self.rect.center();
        let compact_copy = self.text_scale > 1.25 && self.rect.width() < 520.0;
        let (title, subtitle) = if let Some(error) = &self.open_error {
            if compact_copy {
                ("Couldn't open\nvideo", "Check the file, then\ntry again.")
            } else {
                ("Couldn't open video", error.as_str())
            }
        } else if self.player.has_media() || self.kept_recall.is_some() {
            ("Loading video…", "Preparing the first frame")
        } else if compact_copy {
            ("Open a video\nto start", "MP4, WebM, MKV,\nMOV and AVI")
        } else {
            ("Open a video to start", "MP4, WebM, MKV, MOV and AVI")
        };
        let scale = self.text_scale;
        let copy_width = (self.rect.width() - 32.0).max(0.0) as f32;
        let title_lines: Vec<_> = title.lines().collect();
        let subtitle_lines: Vec<_> = subtitle.lines().collect();
        let title_step = 24.0 * scale;
        let subtitle_step = 18.0 * scale;
        let block_h = title_step * title_lines.len() as f64
            + 10.0 * scale
            + subtitle_step * subtitle_lines.len() as f64;
        let mut baseline = if compact_copy && !self.shot_loading {
            center.y + 92.0
        } else {
            center.y - block_h * 0.5 + 20.0 * scale
        };
        for line in title_lines {
            let line = text_renderer.elide_to_width(line, 20.0, copy_width);
            let (width, _) = text_renderer.measure(&line, 20.0, None);
            text_renderer.draw_text(
                scene,
                &line,
                20.0,
                theme.tokens.colors.text_primary,
                transform * Affine::translate((center.x - f64::from(width) * 0.5, baseline)),
                None,
            );
            baseline += title_step;
        }
        baseline += 10.0 * scale;
        for line in subtitle_lines {
            let line = text_renderer.elide_to_width(line, 13.0, copy_width);
            let (width, _) = text_renderer.measure(&line, 13.0, None);
            text_renderer.draw_text(
                scene,
                &line,
                13.0,
                theme.tokens.colors.text_dim,
                transform * Affine::translate((center.x - f64::from(width) * 0.5, baseline)),
                None,
            );
            baseline += subtitle_step;
        }
    }

    #[allow(
        clippy::too_many_lines,
        reason = "the compact panel paints and records one matching set of interaction geometry"
    )]
    fn paint_library_panel(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
    ) {
        if !self.library_visible || self.library_panel_rect.width() <= 0.0 {
            return;
        }
        // Same description `apply_layout` used for the hit rects, refreshed
        // with this frame's label measurements (which re-places the hit rects
        // too, so both stay one geometry).
        let geometry = self.refresh_panel_labels(text_renderer);
        let colors = &theme.tokens.colors;
        let panel = self.library_panel_rect;
        let large_text = self.text_scale > 1.25;
        // A softly distinguished header stops this persistent side panel from
        // reading as a pile of ungrouped text over the video. It deliberately
        // uses theme tokens rather than hard-coded dark paint, preserving the
        // high-contrast palette.
        let (_, tabs_top, _) =
            library_vertical_metrics(self.text_scale, if large_text { 2 } else { 1 });
        fill_rect(scene, transform, panel, colors.surfaces.raised);
        // The panel's edge against the video derives from its own surface
        // (HIG §9.5); a declared `border_standard` on `raised` vanished in
        // the dark preset.
        fill_rect(
            scene,
            transform,
            Rect::new(panel.x0, panel.y0, panel.x1, panel.y0 + tabs_top - 4.0),
            colors.neutral[1],
        );
        fill_rect(
            scene,
            transform,
            Rect::new(panel.x0, panel.y0, panel.x0 + 1.0, panel.y1),
            surface::edge_on(colors.surfaces.raised),
        );
        panel_text(
            scene,
            text_renderer,
            transform,
            geometry.title,
            "Library",
            PANEL_TITLE_SIZE,
            colors.text_primary,
            TextWeight::Semibold,
        );
        panel_text(
            scene,
            text_renderer,
            transform,
            geometry.subtitle,
            if self.library_tab == LibraryTab::Shared {
                "Shared metadata · media access stays on each device"
            } else {
                PANEL_SUBTITLE
            },
            PANEL_SUBTITLE_SIZE,
            colors.text_dim,
            TextWeight::Regular,
        );
        Button::paint(
            scene,
            transform,
            text_renderer,
            theme,
            &ButtonFace::new(self.library_close_rect, "Close")
                .ghost()
                .text_size(PANEL_HEADER_BUTTON_SIZE)
                .hovered(self.library_close_rect.contains(self.last_cursor)),
        );
        if self.library_tab == LibraryTab::Recent && !self.recent.is_empty() {
            Button::paint(
                scene,
                transform,
                text_renderer,
                theme,
                &ButtonFace::new(self.library_clear_rect, "Clear")
                    .ghost()
                    .text_size(PANEL_HEADER_BUTTON_SIZE)
                    .focused(self.library_focus_id.as_deref() == Some("library-clear"))
                    .hovered(self.library_clear_rect.contains(self.last_cursor)),
            );
        }
        let queue_active = self.library_tab == LibraryTab::Queue;
        for (label, rect, active) in [
            ("Queue", self.library_queue_tab_rect, queue_active),
            (
                "Recent",
                self.library_recent_tab_rect,
                self.library_tab == LibraryTab::Recent,
            ),
            (
                "Shared",
                self.library_shared_tab_rect,
                self.library_tab == LibraryTab::Shared,
            ),
        ] {
            let mut face = ButtonFace::new(rect, label)
                .ghost()
                .text_size(PANEL_TAB_SIZE);
            if active {
                face = face.primary();
            }
            Button::paint(
                scene,
                transform,
                text_renderer,
                theme,
                &face.hovered(rect.contains(self.last_cursor)),
            );
        }
        self.library_row_hits.clear();
        if self.library_tab == LibraryTab::Shared {
            self.paint_shared_library(
                scene,
                transform,
                text_renderer,
                theme,
                (panel, large_text, geometry.rows_top),
            );
            return;
        }
        let entries: Vec<LocalMediaEntry> = match self.library_tab {
            LibraryTab::Queue => self.queue.clone(),
            LibraryTab::Recent => self.recent.iter().cloned().collect(),
            LibraryTab::Shared => Vec::new(),
        };
        let narrow_large = large_text && panel.width() < 520.0;
        let row_surface_h = geometry.row_h
            + if narrow_large {
                geometry.control_h + PANEL_ROW_PAD_Y
            } else {
                0.0
            };
        let mut y = geometry.rows_top;
        let row_h = row_surface_h + PANEL_ROW_GAP;
        if entries.is_empty() {
            panel_text(
                scene,
                text_renderer,
                transform,
                // Tall enough for the wrapped message at any text scale;
                // `panel_text` centres the wrapped block inside it.
                Rect::new(
                    panel.x0 + PANEL_GUTTER,
                    y,
                    panel.x1 - PANEL_GUTTER,
                    (y + 132.0).min(panel.y1 - PANEL_GUTTER),
                ),
                if self.library_tab == LibraryTab::Queue {
                    "Your queue is empty. Open a video to add it here."
                } else {
                    "Recently played videos will appear here after a frame is decoded."
                },
                13.0,
                colors.text_dim,
                TextWeight::Regular,
            );
            return;
        }
        for entry in entries
            .into_iter()
            .take(crate::actions::CONTEXTUAL_CHILD_LIMIT)
        {
            if y + row_surface_h > panel.y1 - PANEL_GUTTER {
                break;
            }
            // Row surface inset `PANEL_ROW_INSET`; its text and controls inset
            // the same again, landing on `PANEL_GUTTER` like the header and tabs.
            let rect = Rect::new(
                panel.x0 + PANEL_ROW_INSET,
                y,
                panel.x1 - PANEL_ROW_INSET,
                y + row_surface_h,
            );
            let content_x0 = rect.x0 + PANEL_ROW_INSET;
            let controls_x1 = rect.x1 - PANEL_ROW_INSET;
            // Play / Remove centred in the row (7 px in at 1×), widths grown
            // to the measured labels when those need more than the authored.
            let control_y0 = if narrow_large {
                rect.y1 - PANEL_ROW_PAD_Y - geometry.control_h
            } else {
                rect.y0 + (geometry.row_h - geometry.control_h) / 2.0
            };
            let control_y1 = control_y0 + geometry.control_h;
            let play = Rect::new(
                controls_x1 - geometry.play_w,
                control_y0,
                controls_x1,
                control_y1,
            );
            let remove = (self.library_tab == LibraryTab::Queue).then_some(Rect::new(
                play.x0 - PANEL_BUTTON_GAP - geometry.remove_w,
                control_y0,
                play.x0 - PANEL_BUTTON_GAP,
                control_y1,
            ));
            // Each row is a card on the raised panel: the base surface is a
            // contrast step down from the panel and carries its own derived
            // edge, so rows read as rows at rest, not only under the pointer.
            surface::card(
                scene,
                transform,
                rect,
                f64::from(theme.tokens.radii.md),
                if rect.contains(self.last_cursor) {
                    theme.bg_hover
                } else {
                    colors.surfaces.base
                },
            );
            let active = self.current_queue_id.as_deref() == Some(entry.id.as_str());
            let text_x1 = if narrow_large {
                controls_x1
            } else {
                remove.map_or(play.x0, |remove| remove.x0) - STRIP_TITLE_GAP
            };
            // Elide at the weight the label is drawn with (`panel_text` now
            // honours `Medium`); Regular advances are narrower and would let
            // a just-fitting title run into the Remove/Play faces.
            let label = text_renderer.elide_to_width_weighted(
                &entry.title,
                PANEL_ROW_TITLE_SIZE,
                (text_x1 - content_x0).max(0.0) as f32,
                TextWeight::Medium,
            );
            // Title box under the row's top padding, subtitle box beneath it
            // to the bottom padding — the same boxes `row_h` was sized from.
            let title_box = Rect::new(
                content_x0,
                rect.y0 + PANEL_ROW_PAD_Y,
                text_x1,
                rect.y0 + PANEL_ROW_PAD_Y + geometry.row_title_box_h,
            );
            panel_text(
                scene,
                text_renderer,
                transform,
                title_box,
                &label,
                PANEL_ROW_TITLE_SIZE,
                if active {
                    colors.accent[6]
                } else {
                    colors.text_primary
                },
                TextWeight::Medium,
            );
            panel_text(
                scene,
                text_renderer,
                transform,
                Rect::new(
                    content_x0,
                    title_box.y1,
                    text_x1,
                    if narrow_large {
                        control_y0 - PANEL_ROW_PAD_Y
                    } else {
                        rect.y1 - PANEL_ROW_PAD_Y
                    },
                ),
                if active {
                    "Playing on this device"
                } else {
                    "Local video"
                },
                PANEL_ROW_SUBTITLE_SIZE,
                colors.text_dim,
                TextWeight::Regular,
            );
            if let Some(remove) = remove {
                Button::paint(
                    scene,
                    transform,
                    text_renderer,
                    theme,
                    &ButtonFace::new(remove, "Remove")
                        .ghost()
                        .text_size(PANEL_ROW_CONTROL_SIZE)
                        .focused(
                            self.library_focus_id.as_deref()
                                == Some(format!("library-remove-{}", entry.id).as_str()),
                        )
                        .hovered(remove.contains(self.last_cursor)),
                );
            }
            Button::paint(
                scene,
                transform,
                text_renderer,
                theme,
                &ButtonFace::new(
                    play,
                    if self.library_tab == LibraryTab::Recent {
                        "Replay"
                    } else {
                        "Play"
                    },
                )
                .primary()
                .text_size(PANEL_ROW_CONTROL_SIZE)
                .focused(self.library_focus_id.as_deref() == Some(entry.id.as_str()))
                .hovered(play.contains(self.last_cursor)),
            );
            self.library_row_hits.push(LibraryRowHit {
                id: entry.id,
                row: rect,
                play,
                remove,
            });
            y += row_h;
        }
    }

    /// Paints the "Watch later" / "Bookmark time" buttons at the top of the
    /// shared panel and returns their height.
    fn paint_shared_actions(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        layout: (Rect, bool, f64),
    ) -> f64 {
        let (panel, large_text, y) = layout;
        let gap = 8.0;
        let action_h = if large_text { 56.0 } else { 38.0 };
        let action_w = (panel.width() - 24.0 - gap) * 0.5;
        self.shared_watch_later_rect =
            Rect::new(panel.x0 + 12.0, y, panel.x0 + 12.0 + action_w, y + action_h);
        self.shared_bookmark_rect = Rect::new(
            self.shared_watch_later_rect.x1 + gap,
            y,
            panel.x1 - 12.0,
            y + action_h,
        );
        for (rect, label) in [
            (self.shared_watch_later_rect, "Watch later"),
            (self.shared_bookmark_rect, "Bookmark time"),
        ] {
            Button::paint(
                scene,
                transform,
                text_renderer,
                theme,
                &ButtonFace::new(rect, label)
                    .text_size(11.0)
                    .primary()
                    .hovered(rect.contains(self.last_cursor)),
            );
        }
        action_h
    }

    fn paint_shared_library(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        layout: (Rect, bool, f64),
    ) {
        let (panel, large_text, y) = layout;
        let colors = &theme.tokens.colors;
        let action_h = self.paint_shared_actions(scene, transform, text_renderer, theme, layout);
        self.shared_row_hits.clear();
        let mut row_y = y + action_h + 16.0;
        let row_h = if large_text { 72.0 } else { 48.0 };
        let remove_w = if large_text { 92.0 } else { 64.0 };
        for playlist in self
            .shared_library
            .playlists
            .iter()
            .take(crate::actions::CONTEXTUAL_CHILD_LIMIT)
        {
            if row_y + row_h > panel.y1 - 12.0 {
                break;
            }
            let remove = Rect::new(
                panel.x1 - 12.0 - remove_w,
                row_y + 4.0,
                panel.x1 - 12.0,
                row_y + row_h - 4.0,
            );
            let row = Rect::new(panel.x0 + 12.0, row_y, panel.x1 - 12.0, row_y + row_h);
            // Shared entries must retain a visible grouping boundary in every
            // theme, particularly high contrast where an unfilled text row
            // reads as sparse decoration rather than an actionable item.
            fill_rect(scene, transform, row, colors.surfaces.base);
            scene.stroke(
                &Stroke::new(1.0),
                transform,
                colors.border_standard,
                None,
                &row,
            );
            panel_text(
                scene,
                text_renderer,
                transform,
                Rect::new(
                    panel.x0 + 18.0,
                    row_y + 4.0,
                    remove.x0 - 8.0,
                    row_y + row_h - 4.0,
                ),
                &playlist.name,
                13.0,
                colors.text_primary,
                TextWeight::Regular,
            );
            Button::paint(
                scene,
                transform,
                text_renderer,
                theme,
                &ButtonFace::new(remove, "Delete")
                    .ghost()
                    .text_size(11.0)
                    .hovered(remove.contains(self.last_cursor)),
            );
            self.shared_row_hits.push(SharedRowHit {
                id: playlist.id.clone(),
                jump: None,
                remove,
                annotation: false,
            });
            row_y += row_h;
        }
        self.paint_shared_annotations(
            scene,
            transform,
            text_renderer,
            theme,
            (panel, row_h, remove_w, &mut row_y),
        );
        if self.shared_library.playlists.is_empty() && self.shared_library.annotations.is_empty() {
            panel_text(
                scene,
                text_renderer,
                transform,
                Rect::new(panel.x0 + 20.0, row_y, panel.x1 - 20.0, row_y + row_h),
                "Shared playlists and timestamp notes appear here.",
                12.0,
                colors.text_dim,
                TextWeight::Regular,
            );
        }
    }

    fn paint_shared_annotations(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        layout: (Rect, f64, f64, &mut f64),
    ) {
        let (panel, row_h, remove_w, row_y) = layout;
        for annotation in self
            .shared_library
            .annotations
            .iter()
            .take(crate::actions::CONTEXTUAL_CHILD_LIMIT)
        {
            if *row_y + row_h > panel.y1 - 12.0 {
                break;
            }
            let remove = Rect::new(
                panel.x1 - 12.0 - remove_w,
                *row_y + 4.0,
                panel.x1 - 12.0,
                *row_y + row_h - 4.0,
            );
            let matching_media = self.player.has_media()
                && self.current_media_ref.as_deref() == Some(annotation.media_ref.as_str());
            let jump = matching_media.then_some(Rect::new(
                remove.x0 - 6.0 - remove_w,
                *row_y + 4.0,
                remove.x0 - 6.0,
                *row_y + row_h - 4.0,
            ));
            let text = shared_annotation_label(annotation);
            let row = Rect::new(panel.x0 + 12.0, *row_y, panel.x1 - 12.0, *row_y + row_h);
            fill_rect(scene, transform, row, theme.tokens.colors.surfaces.base);
            // Keep the grouping visible even on renderers whose hairline
            // rectangle stroke lands between device pixels at scaled output.
            fill_rect(
                scene,
                transform,
                Rect::new(row.x0, row.y0, row.x1, row.y0 + 1.0),
                theme.tokens.colors.border_standard,
            );
            fill_rect(
                scene,
                transform,
                Rect::new(row.x0, row.y1 - 1.0, row.x1, row.y1),
                theme.tokens.colors.border_standard,
            );
            scene.stroke(
                &Stroke::new(1.0),
                transform,
                theme.tokens.colors.border_standard,
                None,
                &row,
            );
            panel_text(
                scene,
                text_renderer,
                transform,
                Rect::new(
                    panel.x0 + 18.0,
                    *row_y + 4.0,
                    jump.map_or(remove.x0, |jump| jump.x0) - 8.0,
                    *row_y + row_h - 4.0,
                ),
                &text,
                12.0,
                theme.tokens.colors.text_primary,
                TextWeight::Regular,
            );
            Button::paint(
                scene,
                transform,
                text_renderer,
                theme,
                &ButtonFace::new(remove, "Delete")
                    .ghost()
                    .text_size(11.0)
                    .hovered(remove.contains(self.last_cursor)),
            );
            if let Some(jump) = jump {
                Button::paint(
                    scene,
                    transform,
                    text_renderer,
                    theme,
                    &ButtonFace::new(jump, "Jump")
                        .primary()
                        .text_size(11.0)
                        .focused(
                            self.library_focus_id.as_deref()
                                == Some(format!("shared-jump-{}", annotation.id).as_str()),
                        )
                        .hovered(jump.contains(self.last_cursor)),
                );
            }
            self.shared_row_hits.push(SharedRowHit {
                id: annotation.id.clone(),
                jump,
                remove,
                annotation: true,
            });
            *row_y += row_h;
        }
    }

    fn handle_app_pointer(&mut self, state: &mut AutoCommit) -> bool {
        if self.library_visible && self.library_panel_rect.contains(self.last_cursor) {
            if self.library_close_rect.contains(self.last_cursor) {
                self.request_library_hide();
            } else if self.library_queue_tab_rect.contains(self.last_cursor) {
                self.request_queue_library();
            } else if self.library_recent_tab_rect.contains(self.last_cursor) {
                self.request_recent_library();
            } else if self.library_shared_tab_rect.contains(self.last_cursor) {
                self.request_shared_library();
            } else if self.library_tab == LibraryTab::Shared
                && self.shared_watch_later_rect.contains(self.last_cursor)
            {
                self.add_current_to_watch_later(state);
            } else if self.library_tab == LibraryTab::Shared
                && self.shared_bookmark_rect.contains(self.last_cursor)
            {
                self.bookmark_current_time(state);
            } else if self.library_tab == LibraryTab::Shared
                && let Some(row) = self
                    .shared_row_hits
                    .iter()
                    .find(|row| row.jump.is_some_and(|jump| jump.contains(self.last_cursor)))
                    .cloned()
            {
                self.request_annotation_jump(state, &row.id);
            } else if self.library_tab == LibraryTab::Shared
                && let Some(row) = self
                    .shared_row_hits
                    .iter()
                    .find(|row| row.remove.contains(self.last_cursor))
                    .cloned()
            {
                let action = if row.annotation {
                    crate::shared::SharedAction::DeleteAnnotation { id: row.id }
                } else {
                    crate::shared::SharedAction::DeletePlaylist {
                        observed_item_ids: self
                            .shared_library
                            .items
                            .iter()
                            .filter(|item| item.playlist_id == row.id)
                            .map(|item| item.id.clone())
                            .collect(),
                        id: row.id,
                    }
                };
                self.dispatch_shared(state, &action);
            } else if self.library_tab == LibraryTab::Recent
                && self.library_clear_rect.contains(self.last_cursor)
            {
                self.request_library_clear();
            } else if let Some(row) = self
                .library_row_hits
                .iter()
                .find(|row| row.row.contains(self.last_cursor))
                .cloned()
            {
                if row
                    .remove
                    .is_some_and(|rect| rect.contains(self.last_cursor))
                {
                    self.request_library_remove(&row.id);
                } else {
                    self.request_library_play(&row.id);
                }
            }
            self.dirty = true;
            return true;
        }
        if self.overlay_visible() && self.open_button_rect.contains(self.last_cursor) {
            self.request_open_action();
            return true;
        }
        if self.overlay_visible() && self.library_button_rect.contains(self.last_cursor) {
            self.request_library_visibility_change();
            return true;
        }
        false
    }

    /// Handle the keyboard values this app owns. Factoring this from winit's
    /// event carrier keeps shortcut tests fully safe and avoids fabricating
    /// invalid platform event structs just to exercise a key binding.
    fn handle_app_shortcut_input(
        &mut self,
        logical_key: &winit::keyboard::Key,
        state: ElementState,
        modifiers: winit::event::Modifiers,
    ) -> bool {
        if state != ElementState::Pressed {
            return false;
        }
        // The library is an app-owned side panel rather than a modal supplied
        // by the shared player surface. Give it the conventional close key so
        // keyboard and switch-control users never have to hunt for the small
        // header button. Do not consume Escape while the panel is absent: the
        // shell and the embedded player may have a more relevant use for it.
        if matches!(
            logical_key,
            winit::keyboard::Key::Named(winit::keyboard::NamedKey::Escape)
        ) && self.library_visible
        {
            self.request_library_hide();
            return true;
        }
        let winit::keyboard::Key::Character(key) = logical_key else {
            return false;
        };
        if modifiers.state().control_key() && key.eq_ignore_ascii_case("o") {
            self.request_open_action();
        } else if key.eq_ignore_ascii_case("l") {
            self.request_library_visibility_change();
        } else if key.eq_ignore_ascii_case("p") && self.player.has_media() {
            self.perform_resource_operation(PlayerOperation::OpenPictureInPicture);
        } else {
            return false;
        }
        true
    }

    /// Route a logical key through the app-owned shortcuts, then the focused
    /// reusable player surface. This mirrors the live event order while
    /// remaining safe to exercise without fabricating a platform `KeyEvent`.
    fn route_key_input(
        &mut self,
        logical_key: &winit::keyboard::Key,
        state: ElementState,
        modifiers: winit::event::Modifiers,
    ) -> bool {
        self.handle_app_shortcut_input(logical_key, state, modifiers)
            || self.player.handle_key_input(logical_key, state)
    }
}

/// The row label for a shared timestamp note: its text (or "Bookmark") and
/// the position in seconds.
fn shared_annotation_label(annotation: &crate::shared::TimestampAnnotation) -> String {
    if annotation.text.is_empty() {
        format!("Bookmark · {:.1}s", annotation.time_ms as f64 / 1000.0)
    } else {
        format!(
            "{} · {:.1}s",
            annotation.text,
            annotation.time_ms as f64 / 1000.0
        )
    }
}

fn shared_accessibility_targets(app: &PlayerApp) -> Vec<(String, String, Rect)> {
    let mut targets = Vec::new();
    for row in &app.shared_row_hits {
        if row.annotation {
            if let Some(jump) = row.jump {
                targets.push((
                    format!("shared-jump-{}", row.id),
                    format!("video-player.shared.jump.annotation.{}", row.id),
                    jump,
                ));
            }
            targets.push((
                format!("shared-delete-{}", row.id),
                format!("video-player.shared.delete.annotation.{}", row.id),
                row.remove,
            ));
        } else {
            targets.push((
                format!("shared-delete-{}", row.id),
                format!("video-player.shared.delete.playlist.{}", row.id),
                row.remove,
            ));
        }
    }
    targets
}

fn shared_accessibility_nodes(app: &PlayerApp) -> Vec<ui::a11y::AccessNode> {
    use ui::a11y::{AccessNode, AccessState, Role};
    let mut nodes = vec![
        AccessNode::new(Role::Button)
            .with_name("Add current video to Watch later")
            .with_action_target("video-player.shared.watch-later")
            .with_bounds(app.shared_watch_later_rect)
            .with_state(AccessState {
                focusable: true,
                focused: app.library_focus_id.as_deref() == Some("shared-watch-later"),
                disabled: !app.player.has_media(),
                ..Default::default()
            }),
        AccessNode::new(Role::Button)
            .with_name("Bookmark current timestamp")
            .with_action_target("video-player.shared.bookmark")
            .with_bounds(app.shared_bookmark_rect)
            .with_state(AccessState {
                focusable: true,
                focused: app.library_focus_id.as_deref() == Some("shared-bookmark"),
                disabled: !app.player.has_media(),
                ..Default::default()
            }),
    ];
    nodes.extend(
        shared_accessibility_targets(app)
            .into_iter()
            .map(|(focus, target, rect)| {
                let is_jump = focus.starts_with("shared-jump-");
                AccessNode::new(Role::Button)
                    .with_name(if is_jump {
                        "Jump to timestamp annotation"
                    } else if target.contains(".annotation.") {
                        "Delete timestamp annotation"
                    } else {
                        "Delete shared playlist"
                    })
                    .with_action_target(target)
                    .with_bounds(rect)
                    .with_state(AccessState {
                        focusable: true,
                        focused: app.library_focus_id.as_deref() == Some(focus.as_str()),
                        ..Default::default()
                    })
            }),
    );
    nodes
}

fn local_library_accessibility_nodes(app: &PlayerApp) -> Vec<ui::a11y::AccessNode> {
    use ui::a11y::{AccessNode, AccessState, Role};
    let mut nodes: Vec<_> = app
        .library_row_hits
        .iter()
        .map(|row| {
            let id = &row.id;
            let title = app
                .queue
                .iter()
                .chain(app.recent.iter())
                .find(|entry| entry.id == *id)
                .map_or("Local video", |entry| entry.title.as_str());
            AccessNode::new(Role::Button)
                .with_name(format!("Play {title}"))
                .with_description("Opens this device-local video")
                .with_action_target(format!("video-player.library.play.{id}"))
                .with_bounds(row.play)
                .with_state(AccessState {
                    focusable: true,
                    focused: app.library_focus_id.as_deref() == Some(id.as_str()),
                    ..Default::default()
                })
        })
        .collect();
    if app.library_tab == LibraryTab::Recent && !app.recent.is_empty() {
        nodes.push(
            AccessNode::new(Role::Button)
                .with_name("Clear recent videos")
                .with_description("Removes recent-history entries from this device")
                .with_action_target("video-player.library.clear")
                .with_bounds(app.library_clear_rect)
                .with_state(AccessState {
                    focusable: true,
                    focused: app.library_focus_id.as_deref() == Some("library-clear"),
                    ..Default::default()
                }),
        );
    }
    nodes.extend(app.library_row_hits.iter().filter_map(|row| {
        row.remove.map(|remove| {
            AccessNode::new(Role::Button)
                .with_name("Remove from queue")
                .with_description("Removes this item from the session-only queue")
                .with_action_target(format!("video-player.library.remove.{}", row.id))
                .with_bounds(remove)
                .with_state(AccessState {
                    focusable: true,
                    focused: app.library_focus_id.as_deref()
                        == Some(format!("library-remove-{}", row.id).as_str()),
                    ..Default::default()
                })
        })
    }));
    nodes
}

impl Default for PlayerApp {
    fn default() -> Self {
        Self::new()
    }
}

impl FloatingSurfaceProvider for PlayerApp {
    fn take_floating_surface_request(
        &mut self,
    ) -> Option<ui::floating_surface::FloatingSurfaceRequest> {
        self.pending_pip.take()
    }
}

impl FullscreenProvider for PlayerApp {
    fn fullscreen_intent(&self) -> Option<bool> {
        FullscreenProvider::fullscreen_intent(&self.player)
    }

    fn fullscreen_params(&self) -> ui::fullscreen::FullscreenRequest {
        FullscreenProvider::fullscreen_params(&self.player)
    }

    fn on_fullscreen_changed(&mut self, active: bool) {
        FullscreenProvider::on_fullscreen_changed(&mut self.player, active);
    }
}

impl Widget for PlayerApp {
    ui::provides!(
        dyn ui::widget::FullscreenProvider,
        dyn ui::widget::FloatingSurfaceProvider,
        dyn ui::undo::CommittedActions,
        dyn ui::history_mode::capability::DocumentHistory,
        dyn ui::widget::controls::ActionControls,
        dyn ui::resources::PublishesResources,
        dyn ui::search::capability::SearchSource
    );

    fn local_resource_operation_handlers(&self) -> &'static [&'static str] {
        &[crate::actions::OPERATION_TYPE]
    }

    fn start_operation(
        &mut self,
        op: &ui::widget::ResourceOperation<'_>,
        doc: &mut AutoCommit,
        path: &ObjId,
    ) -> Result<Option<ui::widget::ResourceOperationStart>, String> {
        let Some(request) = op.unscoped() else {
            return ui::widget::start_in_children(self, op, doc, path);
        };
        if let Some(started) = ui::widget::start_in_children(self, op, doc, path)? {
            return Ok(Some(started));
        }
        if let Some(started) = self.admit_resource_operation(
            request.invocation,
            request.kind,
            request.type_id,
            request.payload,
            doc,
            path,
        )? {
            return Ok(Some(started));
        }
        Ok(self
            .answer_resource_operation(request.kind, request.type_id, request.payload, doc, path)?
            .then(|| {
                ui::widget::ResourceOperationStart::Completed(
                    ui::tilo_actions::ActionOutput::default(),
                )
            }))
    }

    fn poll_resource_operation(
        &mut self,
        invocation: tilo_actions::InvocationId,
        _cancellation_requested: bool,
        _doc: &AutoCommit,
        _path: &ObjId,
    ) -> Option<tilo_actions::InvocationUpdate> {
        self.drain_portal();
        if let Some(update) = self.poll_open_frame(invocation) {
            return Some(update);
        }
        self.resource_results
            .iter()
            .position(|update| update.invocation() == invocation)
            .and_then(|index| self.resource_results.remove(index))
    }

    fn sync(&mut self, state: &mut AppModel, path: &ModelPath) {
        let (state, path) = ui::doc::document(state, path);
        self.poll_kept_recall();
        let shared = crate::shared::read(state);
        if !self.shot_shared && shared != self.shared_library {
            self.shared_library = shared;
            self.invalidate_search_cache();
            self.dirty = true;
        }
        self.drain_portal();
        #[cfg(test)]
        if let Some(observer) = &self.history_test_open_error_observer {
            observer.replace(self.open_error.clone());
        }
        self.tick_collab();
        self.player.sync(state, path);
        self.drain_player_media_events();
        self.sync_decoder_settings();
        self.drain_player_settings();
        self.maybe_loop_current_media();
    }

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        self.text_scale = f64::from(theme.text_scale);
        // Edge-to-edge: the player fills the whole surface. The app keeps itself
        // in the node map (so it can intercept the floating Open pill) by owning
        // a single-child root that stretches the player to 100%×100%.
        self.text_scale = f64::from(theme.text_scale);
        let player_node = self.player.build_layout(taffy, theme, node_map);
        let root = parent_node(
            taffy,
            Style {
                display: taffy::Display::Flex,
                flex_direction: taffy::FlexDirection::Column,
                size: Size {
                    width: percent(1.0_f32),
                    height: percent(1.0_f32),
                },
                ..Default::default()
            },
            &[player_node],
        );
        node_map.insert(root, std::ptr::from_mut::<dyn Widget>(self));
        root
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        let Ok(layout) = taffy.layout(node) else {
            return;
        };
        self.rect = Rect::new(
            origin.x + f64::from(layout.location.x),
            origin.y + f64::from(layout.location.y),
            origin.x + f64::from(layout.location.x) + f64::from(layout.size.width),
            origin.y + f64::from(layout.location.y) + f64::from(layout.size.height),
        );
        let new_origin = Point::new(self.rect.x0, self.rect.y0);
        let Ok(children) = taffy.children(node) else {
            return;
        };
        // children[0] = player, filling the whole surface (edge-to-edge).
        let Some(&player_node) = children.first() else {
            return;
        };
        self.player.apply_layout(taffy, player_node, new_origin);
        // Floating top strip — Library (left), title, Open pill (right) — all
        // centred on one line and contained by one scrim (see `TopStrip`).
        self.place_top_strip();
        self.place_library_panel();
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        self.player
            .draw(scene, transform, text_renderer, theme, presence);
        if self.shot_loading {
            fill_rect(
                scene,
                transform,
                self.rect,
                theme.tokens.colors.surfaces.base,
            );
        }
        self.paint_media_status_copy(scene, transform, text_renderer, theme);
        if self.overlay_visible() {
            self.refresh_strip_labels(text_renderer);
            self.paint_overlay(scene, transform, text_renderer, theme);
        }
        self.paint_library_panel(scene, transform, text_renderer, theme);
    }

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut ui::widget::EventCtx<'_>) {
        if let WindowEvent::CursorMoved { position, .. } = event {
            self.last_cursor =
                Point::new(position.x / ctx.scale_factor, position.y / ctx.scale_factor);
        }
        if let WindowEvent::MouseInput {
            state: ElementState::Pressed,
            button: MouseButton::Left,
            ..
        } = event
            && self.handle_app_pointer(ctx.state)
        {
            *ctx.handled = true;
            return;
        }
        if let WindowEvent::KeyboardInput { event, .. } = event
            && self.route_key_input(&event.logical_key, event.state, *ctx.modifiers)
        {
            *ctx.handled = true;
            return;
        }
        // Forward everything else to the player (its controls + shortcuts).
        let Ok(children) = ctx.taffy.children(ctx.node_id) else {
            return;
        };
        if let Some(&player_node) = children.first() {
            self.player
                .handle_event(event, &mut ctx.reborrow(ctx.path, player_node));
        }
    }

    fn children(&self) -> Vec<&dyn Widget> {
        vec![&self.player as &dyn Widget]
    }

    fn children_mut(&mut self) -> Vec<&mut (dyn Widget + '_)> {
        vec![&mut self.player as &mut (dyn Widget + '_)]
    }

    fn accessibility(&self) -> Option<ui::a11y::AccessNode> {
        use ui::a11y::{AccessNode, AccessState, Role};
        let mut children = vec![
            AccessNode::new(Role::Button)
                .with_name("Open video")
                .with_description("Ctrl+O")
                .with_action_target("video-player.open")
                .with_bounds(self.open_button_rect)
                .with_state(AccessState {
                    focusable: true,
                    focused: self.open_focused,
                    disabled: self
                        .open_action
                        .as_ref()
                        .is_none_or(|action| !action.is_available()),
                    ..Default::default()
                }),
            AccessNode::new(Role::Button)
                .with_name(if self.library_visible {
                    "Hide library"
                } else {
                    "Show queue and recent videos"
                })
                .with_description("L")
                .with_action_target("video-player.library")
                .with_bounds(self.library_button_rect)
                .with_state(AccessState {
                    focusable: true,
                    focused: self.library_focused,
                    ..Default::default()
                }),
        ];
        if self.library_visible {
            children.extend([
                AccessNode::new(Role::Button)
                    .with_name("Close library")
                    .with_description("Escape")
                    .with_action_target("video-player.library.close")
                    .with_bounds(self.library_close_rect)
                    .with_state(AccessState {
                        focusable: true,
                        focused: self.library_focus_id.as_deref() == Some("library-close"),
                        ..Default::default()
                    }),
                AccessNode::new(Role::Tab)
                    .with_name("Queue")
                    .with_action_target("video-player.library.tab.queue")
                    .with_bounds(self.library_queue_tab_rect)
                    .with_state(AccessState {
                        focusable: true,
                        focused: self.library_focus_id.as_deref() == Some("library-tab-queue"),
                        selected: self.library_tab == LibraryTab::Queue,
                        ..Default::default()
                    }),
                AccessNode::new(Role::Tab)
                    .with_name("Recent")
                    .with_action_target("video-player.library.tab.recent")
                    .with_bounds(self.library_recent_tab_rect)
                    .with_state(AccessState {
                        focusable: true,
                        focused: self.library_focus_id.as_deref() == Some("library-tab-recent"),
                        selected: self.library_tab == LibraryTab::Recent,
                        ..Default::default()
                    }),
                AccessNode::new(Role::Tab)
                    .with_name("Shared")
                    .with_action_target("video-player.library.tab.shared")
                    .with_bounds(self.library_shared_tab_rect)
                    .with_state(AccessState {
                        focusable: true,
                        focused: self.library_focus_id.as_deref() == Some("library-tab-shared"),
                        selected: self.library_tab == LibraryTab::Shared,
                        ..Default::default()
                    }),
            ]);
            if self.library_tab == LibraryTab::Shared {
                children.extend(shared_accessibility_nodes(self));
            }
            children.extend(local_library_accessibility_nodes(self));
        }
        Some(
            AccessNode::new(Role::Group)
                .with_name(if self.player.has_media() {
                    self.title.clone()
                } else {
                    "Video Player".into()
                })
                .with_children(children),
        )
    }

    fn synthetic_accessibility_rect(&self, path: &[usize]) -> Option<Rect> {
        match path {
            [0] => Some(self.open_button_rect),
            [1] => Some(self.library_button_rect),
            [2] if self.library_visible => Some(self.library_close_rect),
            [3] if self.library_visible => Some(self.library_queue_tab_rect),
            [4] if self.library_visible => Some(self.library_recent_tab_rect),
            [5] if self.library_visible => Some(self.library_shared_tab_rect),
            [6] if self.library_visible && self.library_tab == LibraryTab::Shared => {
                Some(self.shared_watch_later_rect)
            }
            [7] if self.library_visible && self.library_tab == LibraryTab::Shared => {
                Some(self.shared_bookmark_rect)
            }
            [index] if self.library_visible && self.library_tab == LibraryTab::Shared => {
                shared_accessibility_targets(self)
                    .get(index.saturating_sub(8))
                    .map(|(_, _, rect)| *rect)
            }
            [index] => {
                let library_index = index.saturating_sub(6);
                let play_count = self.library_row_hits.len();
                if let Some(row) = self.library_row_hits.get(library_index) {
                    Some(row.play)
                } else if self.library_tab == LibraryTab::Recent
                    && !self.recent.is_empty()
                    && library_index == play_count
                {
                    Some(self.library_clear_rect)
                } else {
                    let remove_index = library_index.saturating_sub(
                        play_count
                            + usize::from(
                                self.library_tab == LibraryTab::Recent && !self.recent.is_empty(),
                            ),
                    );
                    self.library_row_hits
                        .iter()
                        .filter_map(|row| row.remove)
                        .nth(remove_index)
                }
            }
            _ => None,
        }
    }

    fn focus_synthetic_accessibility(&mut self, path: &[usize]) -> bool {
        match path {
            [0] => {
                self.open_focused = true;
                self.library_focused = false;
                self.player.reveal_controls();
                self.dirty = true;
                true
            }
            [1] => {
                self.library_focused = true;
                self.open_focused = false;
                self.player.reveal_controls();
                self.dirty = true;
                true
            }
            [2] if self.library_visible => {
                self.library_focus_id = Some("library-close".into());
                self.open_focused = false;
                self.library_focused = false;
                self.dirty = true;
                true
            }
            [3] if self.library_visible => {
                self.library_focus_id = Some("library-tab-queue".into());
                self.open_focused = false;
                self.library_focused = false;
                self.dirty = true;
                true
            }
            [4] if self.library_visible => {
                self.library_focus_id = Some("library-tab-recent".into());
                self.open_focused = false;
                self.library_focused = false;
                self.dirty = true;
                true
            }
            [5] if self.library_visible => {
                self.library_focus_id = Some("library-tab-shared".into());
                self.open_focused = false;
                self.library_focused = false;
                self.dirty = true;
                true
            }
            [6] if self.library_visible && self.library_tab == LibraryTab::Shared => {
                self.library_focus_id = Some("shared-watch-later".into());
                self.dirty = true;
                true
            }
            [7] if self.library_visible && self.library_tab == LibraryTab::Shared => {
                self.library_focus_id = Some("shared-bookmark".into());
                self.dirty = true;
                true
            }
            [index] if self.library_visible && self.library_tab == LibraryTab::Shared => {
                let Some((focus, _, _)) = shared_accessibility_targets(self)
                    .get(index.saturating_sub(8))
                    .cloned()
                else {
                    return false;
                };
                self.library_focus_id = Some(focus);
                self.dirty = true;
                true
            }
            [index] => {
                let library_index = index.saturating_sub(6);
                let play_count = self.library_row_hits.len();
                self.library_focus_id = if let Some(row) = self.library_row_hits.get(library_index)
                {
                    Some(row.id.clone())
                } else if self.library_tab == LibraryTab::Recent
                    && !self.recent.is_empty()
                    && library_index == play_count
                {
                    Some("library-clear".into())
                } else {
                    let remove_index = library_index.saturating_sub(
                        play_count
                            + usize::from(
                                self.library_tab == LibraryTab::Recent && !self.recent.is_empty(),
                            ),
                    );
                    self.library_row_hits
                        .iter()
                        .filter_map(|row| row.remove.as_ref().map(|_| row.id.as_str()))
                        .nth(remove_index)
                        .map(|id| format!("library-remove-{id}"))
                };
                if self.library_focus_id.is_none() {
                    return false;
                }
                self.open_focused = false;
                self.library_focused = false;
                self.dirty = true;
                true
            }
            _ => false,
        }
    }

    fn perform_semantic_action(
        &mut self,
        target: &str,
        action: &ui::a11y::Action,
        state: &mut AppModel,
        path: &ModelPath,
    ) -> bool {
        let (state, _) = ui::doc::document(state, path);
        if target == "video-player.open" && matches!(action, ui::a11y::Action::Activate) {
            self.request_open_action();
            true
        } else if target == "video-player.library" && matches!(action, ui::a11y::Action::Activate) {
            self.request_library_visibility_change();
            true
        } else if let Some(item_id) = target.strip_prefix("video-player.library.play.")
            && matches!(action, ui::a11y::Action::Activate)
        {
            self.request_library_play(item_id);
            true
        } else if target == "video-player.library.close"
            && matches!(action, ui::a11y::Action::Activate)
        {
            self.request_library_hide();
            true
        } else if target == "video-player.library.tab.queue"
            && matches!(action, ui::a11y::Action::Activate)
        {
            self.request_queue_library();
            true
        } else if target == "video-player.library.tab.recent"
            && matches!(action, ui::a11y::Action::Activate)
        {
            self.request_recent_library();
            true
        } else if target == "video-player.library.tab.shared"
            && matches!(action, ui::a11y::Action::Activate)
        {
            self.request_shared_library();
            true
        } else if target == "video-player.shared.watch-later"
            && matches!(action, ui::a11y::Action::Activate)
        {
            self.add_current_to_watch_later(state);
            true
        } else if target == "video-player.shared.bookmark"
            && matches!(action, ui::a11y::Action::Activate)
        {
            self.bookmark_current_time(state);
            true
        } else if let Some(rest) = target.strip_prefix("video-player.shared.jump.annotation.")
            && matches!(action, ui::a11y::Action::Activate)
        {
            self.request_annotation_jump(state, rest);
            true
        } else if let Some(rest) = target.strip_prefix("video-player.shared.delete.annotation.")
            && matches!(action, ui::a11y::Action::Activate)
        {
            self.dispatch_shared(
                state,
                &crate::shared::SharedAction::DeleteAnnotation { id: rest.into() },
            );
            true
        } else if let Some(rest) = target.strip_prefix("video-player.shared.delete.playlist.")
            && matches!(action, ui::a11y::Action::Activate)
        {
            self.dispatch_shared(
                state,
                &crate::shared::SharedAction::DeletePlaylist {
                    observed_item_ids: self
                        .shared_library
                        .items
                        .iter()
                        .filter(|item| item.playlist_id == rest)
                        .map(|item| item.id.clone())
                        .collect(),
                    id: rest.into(),
                },
            );
            true
        } else if target == "video-player.library.clear"
            && matches!(action, ui::a11y::Action::Activate)
        {
            self.request_library_clear();
            true
        } else if let Some(item_id) = target.strip_prefix("video-player.library.remove.")
            && matches!(action, ui::a11y::Action::Activate)
        {
            self.request_library_remove(item_id);
            true
        } else {
            false
        }
    }

    fn tooltip(&self) -> Option<&str> {
        if self.overlay_visible() && self.open_button_rect.contains(self.last_cursor) {
            Some("Open video (Ctrl+O)")
        } else if self.overlay_visible() && self.library_button_rect.contains(self.last_cursor) {
            Some(if self.library_visible {
                "Hide queue and recent videos (L)"
            } else {
                "Show queue and recent videos (L)"
            })
        } else if self.library_visible && self.library_close_rect.contains(self.last_cursor) {
            Some("Close library (Escape)")
        } else if self.library_visible && self.library_queue_tab_rect.contains(self.last_cursor) {
            Some("Show this device's queue")
        } else if self.library_visible && self.library_recent_tab_rect.contains(self.last_cursor) {
            Some("Show recently played videos on this device")
        } else if self.library_visible && self.library_shared_tab_rect.contains(self.last_cursor) {
            Some("Show shared playlists and timestamp notes")
        } else if self.library_visible
            && self.library_tab == LibraryTab::Shared
            && self.shared_watch_later_rect.contains(self.last_cursor)
        {
            Some("Add the current video to the shared Watch later playlist")
        } else if self.library_visible
            && self.library_tab == LibraryTab::Shared
            && self.shared_bookmark_rect.contains(self.last_cursor)
        {
            Some("Add a shared bookmark at the current playback time")
        } else if self.library_visible
            && self.library_tab == LibraryTab::Shared
            && let Some(row) = self
                .shared_row_hits
                .iter()
                .find(|row| row.remove.contains(self.last_cursor))
        {
            Some(if row.annotation {
                "Delete this shared timestamp note"
            } else {
                "Delete this shared playlist"
            })
        } else if self.library_visible
            && self.library_tab == LibraryTab::Recent
            && self.library_clear_rect.contains(self.last_cursor)
        {
            Some("Clear recent videos from this device")
        } else if self.library_visible
            && self.library_tab == LibraryTab::Shared
            && self
                .shared_row_hits
                .iter()
                .any(|row| row.jump.is_some_and(|jump| jump.contains(self.last_cursor)))
        {
            Some("Jump to this timestamp in the current video")
        } else if self.library_row_hits.iter().any(|row| {
            row.remove
                .is_some_and(|rect| rect.contains(self.last_cursor))
        }) {
            Some("Remove from this session queue")
        } else if self
            .library_row_hits
            .iter()
            .any(|row| row.play.contains(self.last_cursor))
        {
            Some("Play this local video")
        } else {
            None
        }
    }

    fn name(&self) -> &'static str {
        "PlayerApp"
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }

    fn needs_redraw(&self) -> bool {
        self.dirty || self.player.needs_redraw()
    }

    fn mark_drawn(&mut self) {
        self.dirty = false;
        self.player.mark_drawn();
    }

    fn is_layout_dirty(&self) -> bool {
        self.player.is_layout_dirty()
    }

    fn needs_animation_frame(&self) -> bool {
        // Collaboration is polled through `needs_idle_sync`; treating an idle,
        // paused collab session as animation would otherwise force a 60 Hz
        // redraw forever. Playback and the portal are the only frame-rate work.
        self.player.needs_animation_frame()
            || self.portal_open.is_some()
            || self.media_open.is_some()
    }

    fn needs_idle_sync(&self) -> bool {
        // A follower must keep syncing even with no local input so it applies
        // the host's incoming transport.
        self.collab.is_some()
    }
}

impl ui::search::capability::SearchSource for PlayerApp {
    fn search_providers(&self) -> Vec<Box<dyn ui::search::SearchProvider>> {
        let source_id = self.current_source_id.clone();
        let revision = self.metadata_revision;
        let mut cache = self.search_cache.borrow_mut();
        let reusable = cache
            .as_ref()
            .is_some_and(|cached| cached.source_id == source_id && cached.revision == revision);
        if !reusable {
            let provider = crate::search::VideoPlayerSearchProvider::new(
                &self.action_context(
                    &self.search_metadata.audio_tracks,
                    &self.search_metadata.subtitle_tracks,
                    &self.search_metadata.chapters,
                ),
                &self.shared_library,
            );
            *cache = Some(SearchCache {
                source_id,
                revision,
                provider: Arc::new(provider),
            });
            self.search_catalog_builds
                .set(self.search_catalog_builds.get().saturating_add(1));
        }
        if let Some(cached) = cache.as_ref() {
            vec![Box::new((*cached.provider).clone())]
        } else {
            // The cache was initialized immediately above. Returning no stale
            // provider is safer than panicking if that invariant ever changes.
            Vec::new()
        }
    }
}

impl ui::resources::PublishesResources for PlayerApp {
    fn resources(&self, doc: &AutoCommit) -> Vec<tilo_actions::Resource> {
        let (audio_tracks, subtitle_tracks, chapters) = self.decoder_metadata();
        let ctx = self.action_context(&audio_tracks, &subtitle_tracks, &chapters);
        // Framework [`tilo_resource::publish`] owns the application root,
        // transport/library Views, and shared-library document children.
        // Audio/subtitle/chapter rows are live decoder presentation only —
        // keep that same standalone slice when hosted, never a second root.
        if self.framework_hosted {
            return crate::actions::live_track_resources(&ctx, doc);
        }
        crate::actions::resources(&ctx, doc)
    }
    fn resource(&self, doc: &AutoCommit, id: &str) -> Option<tilo_actions::Resource> {
        let (audio_tracks, subtitle_tracks, chapters) = self.decoder_metadata();
        crate::actions::resource(
            &self.action_context(&audio_tracks, &subtitle_tracks, &chapters),
            doc,
            id,
        )
    }
}

impl ui::widget::controls::ActionControls for PlayerApp {
    fn take_control_invocation(&mut self) -> Option<tilo_actions::ControlInvocation> {
        self.open_action
            .as_mut()
            .and_then(ActionEmitter::take)
            .or_else(|| self.library_control_invocations.pop_front())
    }
    fn control_action_bindings(&self) -> Vec<ui::widget::ControlBindingMetadata> {
        let mut bindings = ui::widget::semantic_action_bindings(self.accessibility());
        if let Some(action) = &self.open_action
            && let Some(binding) = bindings
                .iter_mut()
                .find(|binding| binding.control_id == "video-player.open")
        {
            *binding = action.metadata("video-player.open");
        }
        bindings
    }
    fn refresh_control_action_availability(
        &mut self,
        allowed: &std::collections::HashSet<tilo_actions::ActionAddress>,
    ) {
        if let Some(action) = &mut self.open_action {
            action.refresh_availability(allowed);
        }
    }
}

impl ui::history_mode::capability::DocumentHistory for PlayerApp {
    fn document_history_definition(&self) -> Option<&'static ui::history_mode::HistoryDefinition> {
        Some(&crate::framework_history::HISTORY)
    }
}

impl ui::undo::CommittedActions for PlayerApp {
    fn take_committed_action(&mut self) -> Option<Vec<automerge::ChangeHash>> {
        self.actions.take_committed()
    }
}

impl PlayerApp {
    fn answer_resource_operation(
        &mut self,
        kind: tilo_actions::OperationKind,
        type_id: &str,
        payload: &[u8],
        doc: &AutoCommit,
        _path: &ObjId,
    ) -> Result<bool, String> {
        if kind == tilo_actions::OperationKind::Document {
            return Ok(false);
        }
        let Some(operation) = crate::actions::decode(type_id, payload)? else {
            return Ok(false);
        };
        if operation.requires_media() && !self.player.has_media() {
            return Err("open a video before using this playback control".into());
        }
        self.validate_dynamic_target(&operation)?;
        if let PlayerOperation::JumpToAnnotation {
            id,
            media_ref,
            time_ms,
        } = &operation
        {
            let annotation_is_current =
                crate::shared::read(doc)
                    .annotations
                    .iter()
                    .any(|annotation| {
                        annotation.id == *id
                            && annotation.media_ref == *media_ref
                            && annotation.time_ms == *time_ms
                    });
            if !annotation_is_current {
                return Err("the review marker changed; refresh search and try again".into());
            }
        }
        if self
            .collab
            .as_ref()
            .is_some_and(|collab| collab.role() == Role::Follower)
            && matches!(
                &operation,
                crate::actions::PlayerOperation::TogglePlayback
                    | crate::actions::PlayerOperation::SeekSeconds(_)
                    | crate::actions::PlayerOperation::SeekBySeconds(_)
                    | crate::actions::PlayerOperation::Restart
                    | crate::actions::PlayerOperation::SeekToEnd
                    | crate::actions::PlayerOperation::SetLoop(_)
                    | crate::actions::PlayerOperation::SetPlaybackRate(_)
                    | crate::actions::PlayerOperation::JumpToChapter { .. }
                    | crate::actions::PlayerOperation::JumpToAnnotation { .. }
            )
        {
            return Err("this player is following the collaborative host".into());
        }
        if let crate::actions::PlayerOperation::Open { path: Some(path) } = &operation {
            if !Path::new(path).is_file() {
                let label = Path::new(path)
                    .file_name()
                    .and_then(|name| name.to_str())
                    .filter(|name| !name.is_empty())
                    .unwrap_or("selected video");
                let message = format!("Video does not exist: {label}");
                self.open_error = Some(message.clone());
                self.dirty = true;
                return Err(message);
            }
            self.open_path(Path::new(path))?;
        } else if matches!(
            operation,
            crate::actions::PlayerOperation::Open { path: None }
        ) {
            self.trigger_open()?;
        } else if let crate::actions::PlayerOperation::OpenQueueItem { item_id } = &operation {
            self.open_local_entry(item_id)?;
        } else if matches!(
            operation,
            crate::actions::PlayerOperation::ClearRecentHistory
        ) {
            self.clear_recent_history()?;
        } else if let crate::actions::PlayerOperation::RemoveQueueItem { item_id } = &operation {
            self.remove_queue_entry(item_id)?;
        } else {
            self.perform_resource_operation(operation);
        }
        Ok(true)
    }

    fn admit_resource_operation(
        &mut self,
        invocation: tilo_actions::InvocationId,
        kind: tilo_actions::OperationKind,
        type_id: &str,
        payload: &[u8],
        doc: &AutoCommit,
        path: &ObjId,
    ) -> Result<Option<ui::widget::ResourceOperationStart>, String> {
        let operation = crate::actions::decode(type_id, payload)?;
        if !self.answer_resource_operation(kind, type_id, payload, doc, path)? {
            return Ok(None);
        }
        if matches!(
            operation,
            Some(crate::actions::PlayerOperation::Open { path: None })
        ) {
            let Some(pending) = self.portal_open.as_mut() else {
                return Err("video picker did not start".into());
            };
            pending.invocation = Some(invocation);
            Ok(Some(ui::widget::ResourceOperationStart::Accepted))
        } else if matches!(
            operation,
            Some(crate::actions::PlayerOperation::Open { path: Some(_) })
        ) {
            // A filesystem path exists before admission, but a real decoder
            // can still fail after `open`. Accepted therefore means only
            // "decoding has begun"; polling waits for an actual frame.
            self.await_open_frame(invocation)?;
            Ok(Some(ui::widget::ResourceOperationStart::Accepted))
        } else if matches!(
            operation,
            Some(crate::actions::PlayerOperation::OpenQueueItem { .. })
        ) {
            self.await_open_frame(invocation)?;
            Ok(Some(ui::widget::ResourceOperationStart::Accepted))
        } else {
            Ok(Some(ui::widget::ResourceOperationStart::Completed(
                tilo_actions::ActionOutput::default(),
            )))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ui::headless::Framed;

    #[test]
    fn opening_sources_sets_identity_without_exposing_paths() {
        // Case `source_for_path_splits_local_vs_workspace`.
        {
            // Point the mount at a scratch root so the classification is testable.
            let root = std::env::temp_dir().join(format!("tilo-vp-app-{}", std::process::id()));
            let mut env = tilo_test_env::ScopedEnv::new();
            env.set("TILO_WORKSPACE_ROOT", &root);
            let ws = source_for_path(&root.join("media/a.mp4"));
            assert!(matches!(ws, media::Source::Workspace(_)));
            let local = source_for_path(Path::new("/home/u/b.mp4"));
            assert!(matches!(local, media::Source::LocalFile(_)));
        }
        // Case `opening_a_path_sets_title_and_loads_the_engine`.
        {
            let mut app = PlayerApp::new_with_preferences(PlaybackPreferences::default());
            app.open_path(&PathBuf::from("/tmp/Holiday.mp4"))
                .expect("source identity available");
            assert_eq!(app.title, "Holiday.mp4");
            assert!(app.player.is_playing(), "autoplay on open");
            assert_eq!(
                app.player.engine().unwrap().source(),
                Some(&media::Source::LocalFile(PathBuf::from("/tmp/Holiday.mp4")))
            );
        }
        // Case `source_identity_generation_exhaustion_refuses_the_new_open`.
        {
            let mut app = synth_app();
            app.open_path(Path::new("/tmp/Original.mp4"))
                .expect("initial source identity");
            let original_id = app.current_source_id.clone();
            let original_title = app.title.clone();
            let original_source = app
                .player
                .engine()
                .and_then(|engine| engine.source())
                .cloned();
            app.source_generation = u64::MAX;

            let error = app
                .open_path(Path::new("/tmp/New.mp4"))
                .expect_err("source generation must not wrap into an old resource identity");
            assert!(error.contains("identity generation exhausted"));
            assert_eq!(app.source_generation, u64::MAX);
            assert_eq!(app.current_source_id, original_id);
            assert_eq!(app.title, original_title);
            assert_eq!(
                app.player.engine().and_then(|engine| engine.source()),
                original_source.as_ref()
            );
        }
        // Case `local_source_identity_and_now_playing_never_expose_an_absolute_path`.
        {
            let mut app = synth_app();
            let path = Path::new("/home/alice/Private videos/launch-cut.mp4");
            app.open_path(path)
                .expect("local source opens in the synthesized engine");
            let source_id = app.current_source_id.as_deref().unwrap_or_default();
            assert!(source_id.starts_with("local-media-"));
            assert!(!source_id.contains("/home/alice"));
            assert!(!source_id.contains("launch-cut"));
        }
        // Case `reopening_the_same_uri_invalidates_old_dynamic_targets`.
        {
            let mut app = PlayerApp::for_shot(None, Some("metadata"));
            let old_source = app.current_source_id.clone().unwrap();
            let source = media::Source::Remote {
                url: "https://demo.local/clip.mp4".into(),
            };
            app.set_current_source(&source)
                .expect("source identity available");
            assert_ne!(app.current_source_id.as_deref(), Some(old_source.as_str()));
            let track = app.shot_metadata.as_ref().unwrap().audio_tracks[0].clone();
            assert!(
                app.validate_dynamic_target(&PlayerOperation::SelectAudioTrack {
                    source_id: old_source,
                    stream_id: track.stream_id,
                    index: track.index,
                })
                .is_err()
            );
        }
    }

    #[test]
    fn portal_open_is_single_flight_generation_fenced_and_retained() {
        // Case `portal_outcome_opens_the_chosen_file`.
        {
            let mut app = PlayerApp::new();
            let (services, recorder) = ui::host::HostServices::recording();
            recorder.respond_to_pick(PortalOutcome::Opened(PathBuf::from("/tmp/Chosen.webm")));
            ui::host::scoped(&services, || app.trigger_open()).expect("picker starts");
            assert_eq!(recorder.pick_requests().len(), 1);
            app.drain_portal();
            assert_eq!(app.title, "Chosen.webm");
            assert!(
                app.portal_open.is_none(),
                "channel cleared after the outcome"
            );
        }
        // Case `portal_open_is_single_flight_and_a_direct_open_supersedes_it`.
        {
            let mut app = PlayerApp::new();
            app.trigger_open().expect("first picker starts");
            let first_generation = app.portal_open.as_ref().map(|open| open.generation);
            assert!(
                app.trigger_open().is_err(),
                "a second request must not replace the first portal receiver"
            );
            assert_eq!(
                app.portal_open.as_ref().map(|open| open.generation),
                first_generation
            );

            app.open_path(Path::new("/tmp/Direct.mp4"))
                .expect("source identity available");
            assert!(
                app.portal_open.is_none(),
                "a direct open drops the stale picker receiver before it can win later"
            );
        }
        // Case `portal_generation_exhaustion_fails_closed_without_aliasing_old_requests`.
        {
            let mut app = PlayerApp::new();
            app.portal_generation = u64::MAX;
            let error = app.trigger_open().expect_err("generation must not wrap");
            assert!(error.contains("generation exhausted"));
            assert!(app.portal_open.is_none());
            assert_eq!(app.portal_generation, u64::MAX);
        }
        // Case `stale_portal_outcome_cannot_open_media_in_release_or_debug_builds`.
        {
            let mut app = PlayerApp::new();
            let (services, recorder) = ui::host::HostServices::recording();
            recorder.respond_to_pick(PortalOutcome::Opened(PathBuf::from("/tmp/Stale.webm")));
            ui::host::scoped(&services, || app.trigger_open()).expect("picker starts");
            // A newer request generation makes the delivered outcome stale.
            app.portal_generation += 1;
            app.drain_portal();
            assert_eq!(app.title, "No media");
            assert!(app.current_source_id.is_none());
            assert!(app.portal_open.is_none());
        }
        // Case `completed_portal_result_backpressures_new_async_requests_until_polled`.
        {
            let mut app = PlayerApp::new();
            app.queue_resource_result(tilo_actions::InvocationUpdate::Cancelled {
                invocation: tilo_actions::InvocationId::new(),
                message: Some("test completion".into()),
            });
            let error = app
                .trigger_open()
                .expect_err("unpolled completion applies backpressure");
            assert!(error.contains("awaiting host acknowledgement"));
            assert!(app.portal_open.is_none());
            assert_eq!(app.resource_results.len(), 1, "the completion is retained");
        }
        // Case `portal_failure_is_retained_for_the_empty_state`.
        {
            let mut app = PlayerApp::new();
            let (services, recorder) = ui::host::HostServices::recording();
            recorder.respond_to_pick(PortalOutcome::Failed("permission denied".into()));
            ui::host::scoped(&services, || app.trigger_open()).expect("picker starts");
            app.drain_portal();
            assert_eq!(app.open_error.as_deref(), Some("permission denied"));
            assert!(app.player.latest_frame().is_none());
        }
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "merged scenario: each former test is a labelled case block with its own fixture"
    )]
    fn markers_and_metadata_targets_stay_fresh_and_canonical() {
        // Case `durable_marker_media_reference_survives_a_reopen_but_source_actions_stay_fresh`.
        {
            let mut app = synth_app();
            let path = Path::new("/tmp/review-cut.mp4");
            app.open_path(path).expect("first source identity");
            let first_source_id = app.current_source_id.clone();
            let media_ref = app.current_media_ref.clone();
            app.open_path(path).expect("reopened source identity");
            assert_ne!(app.current_source_id, first_source_id);
            assert_eq!(app.current_media_ref, media_ref);
            assert!(
                !app.current_media_ref
                    .as_deref()
                    .unwrap_or_default()
                    .contains("review-cut"),
                "the durable reference remains opaque"
            );
        }
        // Case `marker_jump_control_uses_its_canonical_resource_action`.
        {
            let mut app = synth_app();
            app.open_path(Path::new("/tmp/review-cut.mp4"))
                .expect("source identity");
            let mut doc = AutoCommit::new();
            app.dispatch_shared(
                &mut doc,
                &crate::shared::SharedAction::CreateAnnotation {
                    id: "review-marker".into(),
                    media_ref: app.current_media_ref.clone().unwrap(),
                    time_ms: 12_500,
                    text: "Tighten this cut".into(),
                },
            );
            app.request_annotation_jump(&doc, "review-marker");
            let invocation = ui::widget::controls::take_control_invocation(&mut app)
                .expect("marker jump intent");
            assert_eq!(
                invocation.address.resource,
                crate::actions::annotation_resource_id("review-marker")
            );
            assert_eq!(invocation.address.action, "jump");
        }
        // Case `marker_jump_refuses_a_stale_document_target_before_seeking`.
        {
            let mut app = synth_app();
            app.open_path(Path::new("/tmp/review-cut.mp4"))
                .expect("source identity");
            let media_ref = app.current_media_ref.clone().expect("durable media ref");
            let mut doc = AutoCommit::new();
            app.dispatch_shared(
                &mut doc,
                &crate::shared::SharedAction::CreateAnnotation {
                    id: "review-marker".into(),
                    media_ref: media_ref.clone(),
                    time_ms: 12_500,
                    text: String::new(),
                },
            );
            let encoded = tilo_actions::EncodedAction::encode_operation(
                crate::actions::OPERATION_TYPE,
                &PlayerOperation::JumpToAnnotation {
                    id: "review-marker".into(),
                    media_ref,
                    time_ms: 14_000,
                },
            )
            .expect("operation encodes");
            let error = Widget::start_operation(
                &mut app,
                &ui::widget::ResourceOperation::local(
                    ui::tilo_actions::InvocationId::new(),
                    tilo_actions::OperationKind::View,
                    &encoded.type_id,
                    &encoded.payload,
                ),
                &mut doc,
                &root(),
            )
            .expect_err("changed timestamp must be refused");
            assert!(error.contains("review marker changed"));
            assert_ne!(app.player.position_ms(), 14_000);
        }
        // Case `metadata_revision_boundary_discards_the_cache_before_resetting`.
        {
            let mut app = PlayerApp::for_shot(None, Some("metadata"));
            let _first = ui::search::capability::search_providers(&app);
            assert_eq!(app.search_catalog_builds.get(), 1);
            assert!(app.search_cache.borrow().is_some());

            app.metadata_revision = u64::MAX;
            app.search_metadata = DecoderMetadata::default();
            app.sync_decoder_settings();
            assert_eq!(app.metadata_revision, 0);
            assert!(
                app.search_cache.borrow().is_none(),
                "the old max-revision provider is discarded before reusing revision zero"
            );

            let _second = ui::search::capability::search_providers(&app);
            assert_eq!(
                app.search_catalog_builds.get(),
                2,
                "the fresh post-boundary provider is rebuilt instead of reusing stale metadata"
            );
        }
        // Case `stale_stream_and_chapter_targets_are_refused_before_engine_mutation`.
        {
            let app = PlayerApp::for_shot(None, Some("metadata"));
            let audio = app.shot_metadata.as_ref().unwrap().audio_tracks[0].clone();
            let chapter = app.shot_metadata.as_ref().unwrap().chapters[0].clone();
            let stale_source = "https://old.example/clip.mkv".to_string();
            assert!(
                app.validate_dynamic_target(&PlayerOperation::SelectAudioTrack {
                    source_id: stale_source.clone(),
                    stream_id: audio.stream_id,
                    index: audio.index,
                })
                .is_err()
            );
            assert!(
                app.validate_dynamic_target(&PlayerOperation::JumpToChapter {
                    source_id: stale_source,
                    id: chapter.id,
                    seconds: chapter.seconds,
                })
                .is_err()
            );
            // Reordered array positions are rejected even for the same source.
            let source_id = app.current_source_id.clone().unwrap();
            assert!(
                app.validate_dynamic_target(&PlayerOperation::SelectAudioTrack {
                    source_id,
                    stream_id: "different-stream".into(),
                    index: 0,
                })
                .is_err()
            );
            let source_id = app.current_source_id.clone().unwrap();
            let selected = app.shot_metadata.as_ref().unwrap().subtitle_tracks[0].clone();
            let off = PlayerOperation::SelectSubtitleTrack {
                source_id: source_id.clone(),
                target_stream_id: None,
                target_index: None,
                expected_stream_id: Some(selected.stream_id.clone()),
                expected_index: Some(selected.index),
            };
            assert!(app.validate_dynamic_target(&off).is_ok());
            assert!(
                app.validate_dynamic_target(&PlayerOperation::SelectSubtitleTrack {
                    source_id,
                    target_stream_id: Some(selected.stream_id),
                    target_index: None,
                    expected_stream_id: None,
                    expected_index: None,
                })
                .is_err(),
                "mismatched target pairs are rejected"
            );
        }
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "merged scenario: each former test is a labelled case block with its own fixture"
    )]
    fn library_controls_emit_canonical_host_actions_and_are_accessible() {
        // Case `open_affordance_emits_the_advertised_host_routable_effect`.
        {
            let mut app = PlayerApp::new();
            app.request_open_action();
            let invocation =
                ui::widget::controls::take_control_invocation(&mut app).expect("open intent");
            assert_eq!(invocation.address.resource, "video-player.application");
            assert_eq!(invocation.address.action, "open");
            assert!(
                app.portal_open.is_none(),
                "the control did not bypass the host"
            );
        }
        // Case `library_row_emits_its_canonical_play_action_without_opening_directly`.
        {
            let mut app = PlayerApp::for_shot(None, Some("library"));
            let item_id = app.queue[1].id.clone();
            let source_before = app.current_source_id.clone();
            app.request_library_play(&item_id);
            let invocation =
                ui::widget::controls::take_control_invocation(&mut app).expect("queue play intent");
            assert_eq!(
                invocation.address.resource,
                crate::actions::queue_item_resource_id(&item_id)
            );
            assert_eq!(invocation.address.action, "play");
            assert_eq!(
                app.current_source_id, source_before,
                "click did not bypass host lifecycle"
            );
        }
        // Case `library_remove_and_clear_controls_emit_their_canonical_host_actions`.
        {
            let mut app = PlayerApp::for_shot(None, Some("library"));
            let item_id = app.queue[1].id.clone();
            app.request_library_remove(&item_id);
            let remove =
                ui::widget::controls::take_control_invocation(&mut app).expect("remove intent");
            assert_eq!(
                remove.address.resource,
                crate::actions::queue_item_resource_id(&item_id)
            );
            assert_eq!(remove.address.action, "remove");
            app.request_library_clear();
            let clear =
                ui::widget::controls::take_control_invocation(&mut app).expect("clear intent");
            assert_eq!(clear.address.resource, "video-player.history.local");
            assert_eq!(clear.address.action, "clear");
        }
        // Case `library_play_focus_has_a_real_accessibility_target_and_visual_state`.
        {
            let mut app = PlayerApp::for_shot(None, Some("library"));
            let focused_id = app.queue[1].id.clone();
            app.library_row_hits = vec![
                LibraryRowHit {
                    id: app.queue[0].id.clone(),
                    row: Rect::ZERO,
                    play: Rect::new(0.0, 0.0, 44.0, 44.0),
                    remove: Some(Rect::new(44.0, 0.0, 88.0, 44.0)),
                },
                LibraryRowHit {
                    id: focused_id.clone(),
                    row: Rect::ZERO,
                    play: Rect::new(0.0, 44.0, 44.0, 88.0),
                    remove: Some(Rect::new(44.0, 44.0, 88.0, 88.0)),
                },
            ];
            assert!(Widget::focus_synthetic_accessibility(&mut app, &[7]));
            assert_eq!(app.library_focus_id.as_deref(), Some(focused_id.as_str()));
            assert_eq!(
                Widget::synthetic_accessibility_rect(&app, &[7]),
                Some(Rect::new(0.0, 44.0, 44.0, 88.0))
            );
        }
        // Case `library_close_and_tabs_are_reachable_accessibility_controls`.
        {
            let mut app = PlayerApp::for_shot(None, Some("library"));
            app.library_close_rect = Rect::new(0.0, 0.0, 44.0, 44.0);
            app.library_queue_tab_rect = Rect::new(44.0, 0.0, 132.0, 44.0);
            app.library_recent_tab_rect = Rect::new(132.0, 0.0, 220.0, 44.0);
            app.library_shared_tab_rect = Rect::new(220.0, 0.0, 308.0, 44.0);

            let tree = Widget::accessibility(&app).expect("application accessibility tree");
            assert_eq!(tree.children[2].name.as_deref(), Some("Close library"));
            assert_eq!(tree.children[3].name.as_deref(), Some("Queue"));
            assert!(tree.children[3].state.selected);
            assert_eq!(tree.children[4].name.as_deref(), Some("Recent"));
            assert!(!tree.children[4].state.selected);
            assert_eq!(tree.children[5].name.as_deref(), Some("Shared"));
            assert!(!tree.children[5].state.selected);

            assert!(Widget::focus_synthetic_accessibility(&mut app, &[4]));
            assert_eq!(app.library_focus_id.as_deref(), Some("library-tab-recent"));
            let mut doc = AutoCommit::new();
            let path = root();
            assert!(Widget::perform_semantic_action(
                &mut app,
                "video-player.library.tab.recent",
                &ui::a11y::Action::Activate,
                &mut doc,
                &path,
            ));
            let recent =
                ui::widget::controls::take_control_invocation(&mut app).expect("recent intent");
            assert_eq!(recent.address.resource, "video-player.history.local");
            assert_eq!(recent.address.action, "show");
            assert_eq!(
                app.library_tab,
                LibraryTab::Queue,
                "a11y did not bypass host routing"
            );
            assert!(Widget::perform_semantic_action(
                &mut app,
                "video-player.library.close",
                &ui::a11y::Action::Activate,
                &mut doc,
                &path,
            ));
            let close =
                ui::widget::controls::take_control_invocation(&mut app).expect("close intent");
            assert_eq!(close.address.resource, "video-player.application");
            assert_eq!(close.address.action, "hide-library");
            assert!(app.library_visible, "a11y did not bypass host routing");

            app.perform_resource_operation(PlayerOperation::RevealRecentLibrary);
            assert_eq!(app.library_tab, LibraryTab::Recent);
            app.perform_resource_operation(PlayerOperation::HideLibrary);
            assert!(!app.library_visible);
        }
        // Case `library_has_conventional_escape_close_and_complete_hover_help`.
        {
            let mut app = PlayerApp::for_shot(None, Some("shared"));
            app.library_close_rect = Rect::new(0.0, 0.0, 44.0, 44.0);
            app.last_cursor = Point::new(20.0, 20.0);
            assert_eq!(app.tooltip(), Some("Close library (Escape)"));

            assert!(app.handle_app_shortcut_input(
                &winit::keyboard::Key::Named(winit::keyboard::NamedKey::Escape),
                ElementState::Pressed,
                winit::event::Modifiers::default(),
            ));
            let close = ui::widget::controls::take_control_invocation(&mut app)
                .expect("escape close intent");
            assert_eq!(close.address.resource, "video-player.application");
            assert_eq!(close.address.action, "hide-library");
            assert!(
                app.library_visible,
                "Escape did not bypass the host action lifecycle"
            );
        }
        // Case `library_navigation_is_published_as_canonical_actions`.
        {
            let app = PlayerApp::for_shot(None, Some("library"));
            let resources = ui::resources::resources(&app, &AutoCommit::new());
            let queue =
                find_resource(&resources, "video-player.queue.local").expect("queue resource");
            let recent =
                find_resource(&resources, "video-player.history.local").expect("recent resource");
            let shared = find_resource(&resources, "video-player.playlists.shared")
                .expect("shared library resource");
            let root = &resources[0];

            assert!(
                root.actions
                    .iter()
                    .any(|action| action.id == "show-library")
            );
            assert!(
                root.actions
                    .iter()
                    .any(|action| action.id == "hide-library")
            );
            assert!(queue.actions.iter().any(|action| action.id == "show"));
            assert!(recent.actions.iter().any(|action| action.id == "show"));
            assert!(shared.actions.iter().any(|action| action.id == "show"));
            for action in root
                .actions
                .iter()
                .filter(|action| matches!(action.id.as_str(), "show-library" | "hide-library"))
                .chain(queue.actions.iter().filter(|action| action.id == "show"))
                .chain(recent.actions.iter().filter(|action| action.id == "show"))
                .chain(shared.actions.iter().filter(|action| action.id == "show"))
            {
                assert_eq!(action.contract.retry, tilo_actions::RetryPolicy::Idempotent);
                assert_eq!(
                    action.contract.execution,
                    tilo_actions::ExecutionMode::Immediate
                );
                assert_eq!(
                    action.contract.tags,
                    vec!["device-local", "media", "presentation"]
                );
            }
            let queued_item = queue.children.first().expect("queued item");
            assert_eq!(
                queued_item
                    .actions
                    .iter()
                    .find(|action| action.id == "remove")
                    .expect("remove action")
                    .contract
                    .retry,
                tilo_actions::RetryPolicy::AtMostOnce
            );
        }
        // Case `library_visibility_controls_restore_the_last_explicit_tab`.
        {
            let mut app = PlayerApp::for_shot(None, Some("library"));
            let mut doc = AutoCommit::new();

            // Pointer activation reopens Recent, not the hard-coded Queue default.
            app.library_tab = LibraryTab::Recent;
            app.library_button_rect = Rect::new(0.0, 0.0, 100.0, 44.0);
            app.last_cursor = Point::new(20.0, 20.0);
            assert!(app.handle_app_pointer(&mut doc));
            let hide = ui::widget::controls::take_control_invocation(&mut app)
                .expect("pointer hide intent");
            assert_eq!(hide.address.action, "hide-library");
            app.perform_resource_operation(PlayerOperation::HideLibrary);
            assert!(app.handle_app_pointer(&mut doc));
            let recent = ui::widget::controls::take_control_invocation(&mut app)
                .expect("pointer reopen intent");
            assert_eq!(recent.address.resource, "video-player.history.local");
            assert_eq!(recent.address.action, "show");

            // `L` uses the same explicit intent and retains a Shared view.
            app.perform_resource_operation(PlayerOperation::RevealSharedLibrary);
            let shortcut = winit::keyboard::Key::Character("l".into());
            assert!(app.handle_app_shortcut_input(
                &shortcut,
                ElementState::Pressed,
                winit::event::Modifiers::default(),
            ));
            let hide = ui::widget::controls::take_control_invocation(&mut app)
                .expect("shortcut hide intent");
            assert_eq!(hide.address.action, "hide-library");
            app.perform_resource_operation(PlayerOperation::HideLibrary);
            assert!(app.handle_app_shortcut_input(
                &shortcut,
                ElementState::Pressed,
                winit::event::Modifiers::default(),
            ));
            let shared = ui::widget::controls::take_control_invocation(&mut app)
                .expect("shortcut reopen intent");
            assert_eq!(shared.address.resource, "video-player.playlists.shared");
            assert_eq!(shared.address.action, "show");

            // Assistive-tech activation is routed through the identical Queue
            // action rather than privately changing the current tab.
            app.perform_resource_operation(PlayerOperation::RevealLibrary);
            let path = root();
            assert!(Widget::perform_semantic_action(
                &mut app,
                "video-player.library",
                &ui::a11y::Action::Activate,
                &mut doc,
                &path,
            ));
            let hide =
                ui::widget::controls::take_control_invocation(&mut app).expect("a11y hide intent");
            assert_eq!(hide.address.action, "hide-library");
            app.perform_resource_operation(PlayerOperation::HideLibrary);
            assert!(Widget::perform_semantic_action(
                &mut app,
                "video-player.library",
                &ui::a11y::Action::Activate,
                &mut doc,
                &path,
            ));
            let queue = ui::widget::controls::take_control_invocation(&mut app)
                .expect("a11y reopen intent");
            assert_eq!(queue.address.resource, "video-player.queue.local");
            assert_eq!(queue.address.action, "show");
        }
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "merged scenario: each former test is a labelled case block with its own fixture"
    )]
    fn player_resources_and_transport_guards_are_published_honestly() {
        // Case `picture_in_picture_uses_the_shell_owned_floating_surface_contract`.
        {
            let mut app = synth_app();
            app.open_path(Path::new("/tmp/Clip.mp4"))
                .expect("source identity available");
            let mut app = lay_out(app, (1280.0, 720.0), 1.0);
            app.perform_resource_operation(PlayerOperation::OpenPictureInPicture);
            let request =
                ui::widget::FloatingSurfaceWidgetExt::take_floating_surface_request(&mut *app)
                    .expect("PiP request");
            assert_eq!(request.id, "video-player.current-video");
            assert_eq!(
                request.role,
                ui::floating_surface::FloatingRole::PictureInPicture
            );
            assert_eq!(request.aspect_ratio, Some(16.0 / 9.0));
            assert!(
                matches!(request.source, ui::floating_surface::FloatingSource::AppElement { ref app_id, ref element_id, .. } if app_id == "so.tilo.video-player" && element_id == "video_surface")
            );
        }
        // Case `resources_cover_transport_navigation_audio_metadata_and_settings`.
        {
            let mut app = synth_app();
            app.open_path(Path::new("/tmp/Clip.mp4"))
                .expect("source identity available");
            let resources = ui::resources::resources(&app, &AutoCommit::new());
            let app_resource = &resources[0];
            assert_eq!(app_resource.kind, "video-player.application");
            let media = &app_resource.children[0];
            assert_eq!(media.kind, "media.video");
            let kinds: Vec<&str> = media
                .children
                .iter()
                .map(|child| child.kind.as_str())
                .collect();
            assert_eq!(
                kinds,
                vec![
                    "media.transport",
                    "media.navigation",
                    "media.audio",
                    "media.tracks",
                    "media.chapters",
                    "media.player-settings",
                    "media.presentation",
                ]
            );
        }
        // Case `follower_rejects_transport_operations_even_if_invoked_out_of_band`.
        {
            let mut app = synth_app();
            app.open_path(Path::new("/tmp/Clip.mp4"))
                .expect("source identity available");
            app.collab = Some(CollabPlayback::new(Role::Follower));
            app.player.set_transport_enabled(false);
            let encoded = tilo_actions::EncodedAction::encode_operation(
                crate::actions::OPERATION_TYPE,
                &crate::actions::PlayerOperation::TogglePlayback,
            )
            .unwrap();
            let result = Widget::start_operation(
                &mut app,
                &ui::widget::ResourceOperation::local(
                    ui::tilo_actions::InvocationId::new(),
                    tilo_actions::OperationKind::View,
                    &encoded.type_id,
                    &encoded.payload,
                ),
                &mut AutoCommit::new(),
                &automerge::ROOT,
            );
            assert!(result.is_err());
        }
        // Case `unavailable_transport_is_rejected_even_if_a_caller_bypasses_discovery`.
        {
            let mut app = PlayerApp::new();
            let encoded = tilo_actions::EncodedAction::encode_operation(
                crate::actions::OPERATION_TYPE,
                &crate::actions::PlayerOperation::TogglePlayback,
            )
            .unwrap();
            let result = Widget::start_operation(
                &mut app,
                &ui::widget::ResourceOperation::local(
                    ui::tilo_actions::InvocationId::new(),
                    tilo_actions::OperationKind::View,
                    &encoded.type_id,
                    &encoded.payload,
                ),
                &mut AutoCommit::new(),
                &automerge::ROOT,
            );
            assert!(result.is_err());
        }
        // Case `idle_collaboration_does_not_turn_a_paused_player_into_an_animation`.
        {
            let mut app = PlayerApp::new();
            app.collab = Some(CollabPlayback::new(Role::Host));
            assert!(Widget::needs_idle_sync(&app));
            assert!(
                !Widget::needs_animation_frame(&app),
                "collab receives idle sync without requesting a 60 Hz redraw"
            );
        }
    }

    fn root() -> ObjId {
        automerge::ROOT
    }

    /// Run the production layout passes (sync → build → compute → apply) for
    /// `app` at `viewport`, with the accessibility text scale applied the way
    /// the live host applies it.
    fn lay_out(app: PlayerApp, viewport: (f32, f32), text_scale: f32) -> Framed<PlayerApp> {
        Framed::new(
            ui::State::new(),
            app,
            ui::headless::viewport(viewport),
            Theme::dark().with_text_scale(text_scale),
        )
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "merged scenario: each former test is a labelled case block with its own fixture"
    )]
    fn player_chrome_geometry_uses_measured_widths_and_shared_gutters() {
        // Case `capsules_take_their_measured_label_widths_once_painted`:
        // 2026-09-17 review: the capsules scaled their authored label part by
        // the theme factor. Once the frame's renderer has measured the labels
        // (the paint pass does this), both capsules are the shared Button's
        // padding around the **measured** label, re-placed on the same centre
        // line with the same margins — one geometry for hit-testing and paint.
        {
            for scale in [1.0_f32, 2.0] {
                let mut app = lay_out(
                    PlayerApp::for_shot(None, Some("playing")),
                    (1200.0, 800.0),
                    scale,
                );
                let estimated = (app.library_button_rect, app.open_button_rect);
                let mut tr = TextRenderer::new();
                tr.set_text_scale(scale);
                app.refresh_strip_labels(&mut tr);
                let library_w = f64::from(tr.measure("Library", BUTTON_LABEL_SIZE, None).0);
                let open_w = f64::from(tr.measure("Open", BUTTON_LABEL_SIZE, None).0);
                let (library, open) = (app.library_button_rect, app.open_button_rect);
                assert!(
                    (open.width() - (BUTTON_PAD_X + open_w)).abs() < 0.01
                        && (library.width() - (BUTTON_PAD_X + library_w)).abs() < 0.01,
                    "{scale}×: capsules are padding around the measured labels: {library:?} {open:?}"
                );
                let strip = app.top_strip();
                assert!(
                    (open.center().y - strip.center_y).abs() < 0.01
                        && (library.center().y - strip.center_y).abs() < 0.01
                        && (app.rect.x1 - open.x1 - OVERLAY_MARGIN).abs() < 0.01
                        && (library.x0 - app.rect.x0 - OVERLAY_MARGIN).abs() < 0.01,
                    "{scale}×: re-placed capsules keep the centre line and margins"
                );
                // A second measurement at the same scale changes nothing.
                app.refresh_strip_labels(&mut tr);
                assert_eq!(
                    (app.library_button_rect, app.open_button_rect),
                    (library, open)
                );
                let _ = estimated;
            }
        }
        // Case `library_panel_geometry_stacks_measured_lines_at_2x`:
        // 2026-09-17 review: at 200% the library panel painted its title over
        // the subtitle, the subtitle over the tabs and "Remove" over "Play",
        // with 1× row pitch under 2× glyphs. The geometry keeps the authored 1×
        // chrome and grows each part from the measured line at larger text.
        #[expect(
            clippy::float_cmp,
            reason = "the geometry pins authored whole-pixel constants, which are exact in binary floating point"
        )]
        {
            let panel = Rect::new(840.0, 0.0, 1200.0, 800.0);
            let one = LibraryPanelGeometry::for_panel(panel, 1.0, None);
            assert_eq!(one.close.height(), PANEL_HEADER_BUTTON_H);
            // 1× stack on the panel rhythm (`s3` header top, `s3` between
            // sections): header 12..40, subtitle 40..56, tabs 68..98, rows 110.
            assert_eq!(one.subtitle.y0, PANEL_HEADER_TOP + PANEL_HEADER_BUTTON_H);
            assert_eq!(one.subtitle.y0, 40.0);
            assert_eq!(one.queue_tab.y0, 68.0);
            assert_eq!(one.queue_tab.height(), PANEL_TAB_H);
            assert_eq!(one.rows_top, 110.0);
            assert_eq!(one.row_h, PANEL_ROW_H);
            assert_eq!(one.control_h, PANEL_CONTROL_H);
            assert_eq!((one.play_w, one.remove_w), (PANEL_PLAY_W, PANEL_REMOVE_W));

            let mut tr = TextRenderer::new();
            tr.set_text_scale(2.0);
            let width = |tr: &mut TextRenderer, label: &str, size: f32| {
                f64::from(tr.measure(label, size, None).0)
            };
            let labels = PanelLabels {
                text_scale: 2.0,
                panel_w: panel.width(),
                close_w: width(&mut tr, "Close", PANEL_HEADER_BUTTON_SIZE),
                clear_w: width(&mut tr, "Clear", PANEL_HEADER_BUTTON_SIZE),
                queue_w: width(&mut tr, "Queue", PANEL_TAB_SIZE),
                recent_w: width(&mut tr, "Recent", PANEL_TAB_SIZE),
                shared_w: width(&mut tr, "Shared", PANEL_TAB_SIZE),
                remove_w: width(&mut tr, "Remove", PANEL_ROW_CONTROL_SIZE),
                play_w: width(&mut tr, "Replay", PANEL_ROW_CONTROL_SIZE),
                subtitle_h: f64::from(
                    tr.measure_weighted(
                        PANEL_SUBTITLE,
                        PANEL_SUBTITLE_SIZE,
                        Some((panel.width() - 2.0 * PANEL_GUTTER) as f32),
                        TextWeight::Regular,
                    )
                    .1,
                ),
            };
            let two = LibraryPanelGeometry::for_panel(panel, 2.0, Some(labels));
            let title_line = f64::from(tr.measure("Library", PANEL_TITLE_SIZE, None).1);
            assert!(
                two.title.height() >= title_line,
                "2× header holds the title line"
            );
            assert!(
                two.subtitle.y0 >= two.title.y1 && two.subtitle.height() >= labels.subtitle_h,
                "2× subtitle starts under the header and holds its wrapped lines: {two:?}"
            );
            assert!(
                two.queue_tab.y0 >= two.subtitle.y1,
                "2× tabs start under the subtitle"
            );
            assert!(
                two.rows_top >= two.recent_tab.y1,
                "2× rows start under the tabs"
            );
            let row_lines = f64::from(tr.measure("Ag", PANEL_ROW_TITLE_SIZE, None).1)
                + f64::from(tr.measure("Ag", PANEL_ROW_SUBTITLE_SIZE, None).1);
            assert!(
                two.row_h >= row_lines + 2.0 * PANEL_ROW_PAD_Y,
                "2× rows hold two measured lines: {}",
                two.row_h
            );
            assert!(
                two.play_w >= labels.play_w + PANEL_LABEL_PAD_X
                    && two.remove_w >= labels.remove_w + PANEL_LABEL_PAD_X
                    && two.close.width() >= labels.close_w + PANEL_LABEL_PAD_X,
                "2× controls hold their measured labels: {two:?}"
            );
            assert!(
                two.control_h <= two.row_h
                    && two.control_h
                        >= f64::from(tr.measure("Play", PANEL_ROW_CONTROL_SIZE, None).1),
                "2× controls fit the row and hold their label"
            );
        }
        // Case `top_strip_is_one_centre_line_that_contains_its_buttons`:
        // 2026-09-16 alignment review: the title was centred on its font size in
        // a 44 px scrim while the buttons were `OVERLAY_MARGIN + OPEN_BTN_H`
        // deep — two centre lines, and the buttons overhung the scrim by 2 px. At
        // 200% text the 32 px capsules no longer held their 28 px labels.
        #[expect(
            clippy::float_cmp,
            reason = "the geometry pins authored whole-pixel constants, which are exact in binary floating point"
        )]
        {
            let one = TopStrip::at(0.0, 1.0);
            assert_eq!(
                one.button_h, OPEN_BTN_H,
                "1× keeps the authored pill height"
            );
            assert_eq!(one.center_y, OVERLAY_MARGIN + OPEN_BTN_H / 2.0);
            assert_eq!(one.scrim_h, OPEN_BTN_H + 2.0 * OVERLAY_MARGIN);

            // The 2× label line comes from a real renderer at that scale, not from
            // the same formula the strip uses — so a change to the shared Button's
            // label size or the default leading would surface here.
            let two = TopStrip::at(0.0, 2.0);
            let mut tr = TextRenderer::new();
            tr.set_text_scale(2.0);
            let label_line = f64::from(tr.measure("Library", BUTTON_LABEL_SIZE, None).1);
            assert!(
                two.button_h >= label_line + 2.0 * BUTTON_PAD_Y,
                "2× capsule {} contains a measured {label_line} px label",
                two.button_h
            );
            assert!(two.scrim_h >= two.button_h + 2.0 * OVERLAY_MARGIN);

            for scale in [1.0_f32, 2.0] {
                let app = lay_out(
                    PlayerApp::for_shot(None, Some("playing")),
                    (1200.0, 800.0),
                    scale,
                );
                let strip = app.top_strip();
                let open = app.open_button_rect;
                let library = app.library_button_rect;
                assert!(
                    (open.center().y - strip.center_y).abs() < 0.01
                        && (library.center().y - strip.center_y).abs() < 0.01,
                    "{scale}×: Open {open:?} and Library {library:?} share the strip centre {}",
                    strip.center_y
                );
                assert!(
                    open.y1 <= app.rect.y0 + strip.scrim_h
                        && library.y1 <= app.rect.y0 + strip.scrim_h,
                    "{scale}×: the scrim ({}) contains both buttons",
                    strip.scrim_h
                );
                assert!(
                    (app.rect.x1 - open.x1 - OVERLAY_MARGIN).abs() < 0.01
                        && (library.x0 - app.rect.x0 - OVERLAY_MARGIN).abs() < 0.01,
                    "{scale}×: both buttons keep the overlay margin"
                );
            }
        }
        // Case `library_panel_chrome_shares_one_gutter`:
        // The library panel used three left insets (title 16, tabs 12, row text
        // 24) and two right ones (Play 14, Close 12). One description, one gutter.
        #[expect(
            clippy::float_cmp,
            reason = "the geometry pins authored whole-pixel constants, which are exact in binary floating point"
        )]
        {
            let panel = Rect::new(840.0, 0.0, 1200.0, 800.0);
            let g = LibraryPanelGeometry::for_panel(panel, 1.0, None);
            let left = panel.x0 + PANEL_GUTTER;
            let right = panel.x1 - PANEL_GUTTER;
            assert_eq!(g.title.x0, left);
            assert_eq!(g.subtitle.x0, left);
            assert_eq!(g.queue_tab.x0, left);
            assert_eq!(g.close.x1, right);
            assert_eq!(g.subtitle.x1, right);
            assert!(g.clear.x1 < g.close.x0, "Clear sits left of Close");
            assert!(
                g.title.x1 <= g.clear.x0,
                "title never runs under the header buttons"
            );

            let app = lay_out(
                PlayerApp::for_shot(None, Some("library")),
                (1200.0, 800.0),
                1.0,
            );
            assert_eq!(app.library_close_rect, g.close);
            assert_eq!(app.library_queue_tab_rect, g.queue_tab);
            assert_eq!(app.library_recent_tab_rect, g.recent_tab);
        }
        // Case `fresh_player_shows_a_clean_empty_state_with_no_frame`.
        {
            let mut app = PlayerApp::new();
            let mut doc = AutoCommit::new();
            // Pumping the app before any file is opened must not fabricate a frame.
            app.sync(&mut doc, &root());
            app.sync(&mut doc, &root());
            assert!(
                app.player.latest_frame().is_none(),
                "empty state: no picture until a file is opened"
            );
            assert_eq!(app.title, "No media");
            assert!(!app.player.is_playing());
        }
    }

    /// A player pinned to the deterministic synthesized backend, so the app's
    /// open/transport wiring is exercised hermetically regardless of build
    /// features (real `GStreamer` decode is proven separately by the
    /// `--features decode` integration test with a real generated file).
    fn synth_app() -> PlayerApp {
        let mut app = PlayerApp::new_with_preferences(PlaybackPreferences::default());
        app.player.set_engine(media::MediaEngine::new_synthesized());
        app
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "merged scenario: each former test is a labelled case block with its own fixture"
    )]
    fn decoder_lifecycle_confirms_opens_only_on_a_real_frame() {
        // Case `opening_a_file_then_syncing_lands_a_frame_and_duration`.
        {
            let mut app = synth_app();
            app.open_path(&PathBuf::from("/tmp/Clip.mp4"))
                .expect("source identity available");
            let mut doc = AutoCommit::new();
            app.sync(&mut doc, &root());
            assert!(
                app.player.latest_frame().is_some(),
                "a frame is produced once opened"
            );
            assert!(
                app.player.duration_ms() > 0,
                "duration mirrored from the engine"
            );
            assert!(app.player.is_playing(), "autoplay after open");
        }
        // Case `direct_open_is_accepted_until_a_decoder_frame_confirms_success`.
        {
            let mut app = synth_app();
            let path = std::env::temp_dir().join(format!(
                "tilo-video-player-open-lifecycle-{}-{}.mp4",
                std::process::id(),
                unix_now_ms()
            ));
            std::fs::write(&path, []).expect("create a portal-equivalent local file");
            let operation = PlayerOperation::Open {
                path: Some(path.to_string_lossy().into_owned()),
            };
            let payload = serde_json::to_vec(&operation).expect("serialize operation");
            let invocation = tilo_actions::InvocationId::new();
            let mut doc = AutoCommit::new();
            let started = Widget::start_operation(
                &mut app,
                &ui::widget::ResourceOperation::local(
                    invocation,
                    tilo_actions::OperationKind::Effect,
                    crate::actions::OPERATION_TYPE,
                    &payload,
                ),
                &mut doc,
                &root(),
            )
            .expect("direct open admits");
            assert_eq!(
                started,
                Some(ui::widget::ResourceOperationStart::Accepted),
                "path validation is admission, not decoder completion"
            );
            assert!(
                Widget::poll_resource_operation(&mut app, invocation, false, &doc, &root())
                    .is_none()
            );
            let mut mutable_doc = doc;
            app.sync(&mut mutable_doc, &root());
            assert!(matches!(
                Widget::poll_resource_operation(&mut app, invocation, false, &mutable_doc, &root()),
                Some(tilo_actions::InvocationUpdate::Succeeded { .. })
            ));
            std::fs::remove_file(path).ok();
        }
        // Case `queued_item_uses_the_same_pending_decoder_lifecycle_as_direct_open`.
        {
            let mut app = PlayerApp::for_shot(None, Some("library"));
            let item_id = app.queue[1].id.clone();
            let payload = serde_json::to_vec(&PlayerOperation::OpenQueueItem {
                item_id: item_id.clone(),
            })
            .expect("serialize queue operation");
            let invocation = tilo_actions::InvocationId::new();
            let mut doc = AutoCommit::new();
            assert_eq!(
                Widget::start_operation(
                    &mut app,
                    &ui::widget::ResourceOperation::local(
                        invocation,
                        tilo_actions::OperationKind::Effect,
                        crate::actions::OPERATION_TYPE,
                        &payload
                    ),
                    &mut doc,
                    &root()
                )
                .expect("queue item admits"),
                Some(ui::widget::ResourceOperationStart::Accepted)
            );
            assert_eq!(app.current_queue_id.as_deref(), Some(item_id.as_str()));
            assert!(
                Widget::poll_resource_operation(&mut app, invocation, false, &doc, &root())
                    .is_none(),
                "admission is still not a terminal playback success"
            );
            app.sync(&mut doc, &root());
            assert!(matches!(
                Widget::poll_resource_operation(&mut app, invocation, false, &doc, &root()),
                Some(tilo_actions::InvocationUpdate::Succeeded { .. })
            ));
            assert_eq!(
                app.recent.front().map(|entry| entry.id.as_str()),
                Some(item_id.as_str())
            );
        }
        // Case `admitted_direct_open_reports_a_decoder_timeout_instead_of_false_success`.
        {
            let mut app = synth_app();
            let path = std::env::temp_dir().join(format!(
                "tilo-video-player-open-timeout-{}-{}.mp4",
                std::process::id(),
                unix_now_ms()
            ));
            std::fs::write(&path, []).expect("create a portal-equivalent local file");
            let payload = serde_json::to_vec(&PlayerOperation::Open {
                path: Some(path.to_string_lossy().into_owned()),
            })
            .expect("serialize operation");
            let invocation = tilo_actions::InvocationId::new();
            let mut doc = AutoCommit::new();
            assert_eq!(
                Widget::start_operation(
                    &mut app,
                    &ui::widget::ResourceOperation::local(
                        invocation,
                        tilo_actions::OperationKind::Effect,
                        crate::actions::OPERATION_TYPE,
                        &payload
                    ),
                    &mut doc,
                    &root()
                )
                .expect("direct open admits"),
                Some(ui::widget::ResourceOperationStart::Accepted)
            );
            app.media_open
                .as_mut()
                .expect("pending decoder open")
                .deadline = ui::time::now();
            assert!(matches!(
                Widget::poll_resource_operation(&mut app, invocation, false, &doc, &root()),
                Some(tilo_actions::InvocationUpdate::Failed { error, .. })
                    if error.code == "video_player.open_timed_out"
            ));
            assert_eq!(
                app.open_error.as_deref(),
                Some("The decoder did not produce a first frame in time.")
            );
            std::fs::remove_file(path).ok();
        }
    }

    #[test]
    fn transport_settings_reach_the_engine_and_snapshot() {
        // Case `autoplay_and_repeat_are_real_settings_not_demo_affordances`.
        {
            let mut app = synth_app();
            app.preferences.autoplay = false;
            configure_player_settings(
                &mut app.player,
                app.preferences.autoplay,
                app.preferences.loop_enabled,
            );
            app.open_path(Path::new("/tmp/Clip.mp4"))
                .expect("source identity available");
            assert!(
                !app.player.is_playing(),
                "disabled autoplay is honoured on open"
            );

            let mut doc = AutoCommit::new();
            app.sync(&mut doc, &root());
            app.preferences.loop_enabled = true;
            configure_player_settings(
                &mut app.player,
                app.preferences.autoplay,
                app.preferences.loop_enabled,
            );
            let end = app.player.duration_ms();
            app.player.seek_to_ms(end);
            app.player.pause();
            app.maybe_loop_current_media();
            assert_eq!(app.player.position_ms(), 0, "repeat restarts at the end");
            assert!(app.player.is_playing(), "repeat resumes playback");
        }
        // Case `playhead_advances_while_playing_and_a_mid_play_seek_holds`.
        {
            let mut app = synth_app();
            app.open_path(&PathBuf::from("/tmp/Clip.mp4"))
                .expect("source identity available");
            let mut doc = AutoCommit::new();
            app.sync(&mut doc, &root()); // baseline the synth clock
            std::thread::sleep(std::time::Duration::from_millis(40));
            app.sync(&mut doc, &root());
            assert!(
                app.player.position_ms() > 0,
                "playhead advanced while playing"
            );

            // Seek mid-play: the target lands in both the widget cache and the engine.
            app.player.seek_to_ms(50_000);
            assert_eq!(app.player.position_ms(), 50_000);
            assert_eq!(app.player.engine().unwrap().position_ms(), 50_000);

            // Playback continues forward from the seek target, not back to the start.
            std::thread::sleep(std::time::Duration::from_millis(30));
            app.sync(&mut doc, &root());
            assert!(
                app.player.position_ms() >= 50_000,
                "seek stuck; playback rode on from it"
            );
        }
        // Case `volume_and_mute_propagate_to_the_engine`.
        {
            let mut app = synth_app();
            app.open_path(&PathBuf::from("/tmp/Clip.mp4"))
                .expect("source identity available");
            app.player.set_volume(0.3);
            assert!((app.player.volume() - 0.3).abs() < 1e-6);
            assert!((app.player.engine().unwrap().volume() - 0.3).abs() < 1e-6);
            app.player.toggle_mute();
            assert!(
                (app.player.engine().unwrap().volume() - 0.0).abs() < 1e-6,
                "mute drops the engine volume to zero"
            );
        }
        // Case `playback_speed_action_changes_the_engine_and_collaborative_snapshot`.
        {
            let mut app = synth_app();
            app.open_path(Path::new("/tmp/Clip.mp4"))
                .expect("source identity available");
            app.perform_resource_operation(crate::actions::PlayerOperation::SetPlaybackRate(2.0));
            let engine = app.player.engine().expect("engine");
            assert!((engine.rate() - 2.0).abs() < f32::EPSILON);
            assert!((engine.transport_state(unix_now_ms()).rate - 2.0).abs() < f32::EPSILON);
        }
        // Case `space_key_routed_through_the_app_toggles_playback`.
        {
            let mut app = synth_app();
            app.open_path(&PathBuf::from("/tmp/Clip.mp4"))
                .expect("source identity available");
            app.player.set_focus(true);
            let was_playing = app.player.is_playing();
            assert!(app.route_key_input(
                &winit::keyboard::Key::Named(winit::keyboard::NamedKey::Space),
                ElementState::Pressed,
                winit::event::Modifiers::default(),
            ));
            assert_ne!(
                app.player.is_playing(),
                was_playing,
                "Space reached the focused player through the app"
            );
        }
    }

    #[test]
    fn search_provider_reads_metadata_without_reindex_or_transport() {
        // Case `repeated_search_provider_retrieval_does_not_reindex_metadata`.
        {
            let app = PlayerApp::for_shot(None, Some("metadata"));
            let first = ui::search::capability::search_providers(&app);
            assert_eq!(app.search_catalog_builds.get(), 1);
            let second = ui::search::capability::search_providers(&app);
            assert_eq!(
                app.search_catalog_builds.get(),
                1,
                "unchanged Ctrl+K opens reuse the Arc catalog"
            );
            assert_eq!(
                first[0].search("Deutsch").len(),
                second[0].search("Deutsch").len()
            );
        }
        // Case `search_reveal_operation_never_changes_transport`.
        {
            let mut app = PlayerApp::for_shot(None, None);
            app.player.pause();
            let position = app.player.position_ms();
            app.perform_resource_operation(PlayerOperation::Reveal);
            assert!(!app.player.is_playing());
            assert_eq!(app.player.position_ms(), position);
        }
    }
}
