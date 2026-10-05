//! The local video-player app: an edge-to-edge reusable
//! [`ui::widget::VideoPlayer`] driven by a shared [`media::MediaEngine`].
//!
//! `PlayerApp` is the root widget. The video fills the whole surface — there is
//! **no persistent app chrome bar** (the OS owns window chrome; HIG §1e wants
//! video immersive). The media title and the Open action float in a top overlay
//! that appears with the player's controls and auto-hides during playback, so
//! immersive video is truly edge-to-edge. It opens files through the XDG
//! `FileChooser` portal (or a path or URL it is launched on), and keeps
//! playback in step with everyone who has the same video open through the
//! shared library's transport.

use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};
use ui::widget::{AppModel, ModelPath};

use taffy::style_helpers::percent;
use taffy::{NodeId, Size, Style, TaffyTree};
use tilo_clock::unix_now_ms;
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect, Stroke};
use vello::peniko::{BlendMode, Color, Fill};
use winit::event::{ElementState, MouseButton, WindowEvent};

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
    Button, ButtonFace, FloatingSurfaceProvider, FullscreenProvider, VideoPlayer, Widget,
};

use ui::framework::{
    Dispatcher, EffectAnswer, EffectCx, EffectReply, EffectStatus, Effected, LaunchInput, RunMode,
    Setting, SurfaceCx,
};
use ui::tilo_resource::ActionDecl;

use crate::actions::VideoEffect;
use crate::preferences::{PlayerSettings, RecentMedia};
use crate::resource::VideoPlayer as PlayerResource;

/// The playback preferences the player follows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct PlaybackPreferences {
    autoplay: bool,
    loop_enabled: bool,
}

impl Default for PlaybackPreferences {
    fn default() -> Self {
        let defaults = PlayerSettings::default();
        Self {
            autoplay: defaults.autoplay,
            loop_enabled: defaults.loop_enabled,
        }
    }
}

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
    /// The caller of the open effect, answered once a picture decodes.
    reply: Option<EffectReply>,
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

/// A picker's answer, with the request generation it answers.
type PortalOpenOutcome = (u64, Result<Picked, Option<String>>);

/// A canonical open invocation awaiting its decoder's first picture. This is
/// separate from the file-picker request: the picker can succeed while decode
/// fails, stalls, or is superseded before a frame appears.
struct PendingMediaOpen {
    reply: EffectReply,
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

impl LibraryTab {
    /// The tab's name in the show-library effect.
    fn name(self) -> &'static str {
        match self {
            Self::Queue => "queue",
            Self::Recent => "recent",
            Self::Shared => "shared",
        }
    }

    fn named(name: &str) -> Option<Self> {
        [Self::Queue, Self::Recent, Self::Shared]
            .into_iter()
            .find(|tab| tab.name() == name)
    }
}

/// The shared playlist the player's Watch later button adds to.
const WATCH_LATER: &str = "watch-later";

/// A player for a window built in `mode`: only a window someone uses
/// decodes with the system's engine and announces itself to the session's
/// media controls. A capture or a past version gets the deterministic
/// synthesized engine and reaches nothing outside the process.
fn configured_player(mode: RunMode, autoplay: bool, loop_enabled: bool) -> VideoPlayer {
    let engine = if mode.is_live() {
        media::MediaEngine::new()
    } else {
        media::MediaEngine::new_synthesized()
    };
    let mut player = VideoPlayer::new()
        .with_engine(engine)
        .with_test_id("video_surface");
    if mode.is_live() {
        player.enable_mpris("tilo_video_player", "Video Player");
    }
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

/// The source a launch argument names: a stream's `http(s)` URL, else a
/// path.
pub fn source_for_argument(argument: &str) -> media::Source {
    if argument.starts_with("http://") || argument.starts_with("https://") {
        media::Source::Remote {
            url: argument.to_owned(),
        }
    } else {
        source_for_path(Path::new(argument))
    }
}

/// What a source is called before anyone names it: a file's name, or a
/// stream's.
fn source_title(source: &media::Source) -> String {
    match source {
        media::Source::LocalFile(path) | media::Source::Workspace(path) => path
            .file_name()
            .and_then(|name| name.to_str())
            .filter(|name| !name.is_empty())
            .unwrap_or("selected video")
            .to_string(),
        media::Source::Remote { .. } => "Streaming video".to_string(),
    }
}

/// The synthesized clip a capture plays when its launch named no video.
fn demo_source() -> media::Source {
    media::Source::Remote {
        url: "https://demo.local/clip.mp4".into(),
    }
}

/// The shareable identity of the capture's demo clip, which its scenarios'
/// shared annotations point at.
pub(crate) fn demo_media_ref() -> String {
    local_source_token(&demo_source(), 0)
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
    open_focused: bool,
    library_focused: bool,
    library_focus_id: Option<String>,
    /// Playback watched together with everyone who has the same video open.
    together: Together,
    /// The playback preferences the player follows.
    preferences: PlaybackPreferences,
    /// This device's settings, where a live window keeps its preferences
    /// and recent videos; `None` for a capture or a past version.
    setting: Option<Setting<PlayerSettings>>,
    /// Deterministic decoder metadata used only by the explicit headless
    /// `metadata` screenshot fixture. Live runs always read the active engine.
    shot_metadata: Option<DecoderMetadata>,
    /// Deterministically holds the production no-frame/loading presentation
    /// for visual regression capture while the synthesized engine stays
    /// hermetic. Live runs always derive loading from decoder frame state.
    shot_loading: bool,
    pending_pip: Option<ui::floating_surface::FloatingSurfaceRequest>,
    /// Where the shared library's edits and the player's own controls go:
    /// the framework's door, handed over when the surface is attached.
    dispatcher: Option<Dispatcher<PlayerResource>>,
    /// The shared library as last presented.
    shared_library: crate::shared::SharedLibrary,
    /// What the window is built for: only a live one starts a decoder
    /// service, media controls or collaboration.
    mode: RunMode,
    /// What a capture waits for before its frame is taken.
    capture_awaits: CaptureAwaits,
    /// The video a capture's launch named, played once the scenario stages.
    pub(crate) capture_launched: Option<String>,
    /// Opaque source identity for durable shared playlists and markers. It
    /// survives reopening the same local media, unlike the per-open source ID.
    current_media_ref: Option<String>,
    current_source_id: Option<String>,
    source_generation: u64,
    /// The decoder metadata the settings menu shows.
    shown_metadata: DecoderMetadata,
    /// A human-readable failure from the latest open request. It is only
    /// painted in the no-frame state, preserving a successfully playing video
    /// when a later picker request fails.
    open_error: Option<String>,
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

/// What a capture of the player waits for before it is taken: a decoded
/// picture of what it plays, or what became of opening it (a picture or the
/// decoder's error).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CaptureAwaits {
    Nothing,
    Frame,
    Outcome,
}

/// How far the playhead here may stray from where it stands for everyone
/// before this player says it moved it: past the drift a follower corrects
/// on its own, so following never echoes back as a move.
const TOGETHER_MOVE_MS: u64 = 1_500;
/// How long after following a peer's move this player waits before it says
/// anything itself, while its engine catches up.
const TOGETHER_SETTLE_MS: u64 = 1_500;
/// How long a paused shared transport stays something to follow: a video
/// left paused long ago starts on its own when it is opened again.
const TOGETHER_PAUSE_LIVES_MS: u64 = 10 * 60 * 1000;

/// What watching together does this frame.
#[derive(Debug, PartialEq)]
enum TogetherStep {
    /// Bring the engine to where playback stands for everyone.
    Follow(media::TransportState),
    /// Say where playback stands here: this player moved it.
    Publish(media::TransportState),
    Stay,
}

/// What this player last knew of playback watched together.
#[derive(Default)]
struct Together {
    /// The video it concerns.
    media_ref: Option<String>,
    /// The shared transport last seen, to tell a new one from an old one.
    seen: Option<crate::shared::SharedTransport>,
    /// What this player itself said last, to know its own change when it
    /// comes back.
    said: Option<media::TransportState>,
    /// Until when this player only follows, after it followed a move.
    settling_until: u64,
    /// Whether what was seen is over (left paused long ago, run past the
    /// end): nothing to follow or to compare with.
    over: bool,
    /// Whether what was seen is this player's own: it leads, so there is
    /// nothing to ride along with. Correcting its engine toward its own
    /// announcement would seek it back every frame while it starts playing
    /// or resumes, before its playhead moves.
    leading: bool,
}

impl Together {
    /// Decide this frame's step for the video `media_ref`, given where
    /// playback stands for everyone (`shared`) and here (`local`), at `now`.
    fn step(
        &mut self,
        media_ref: &str,
        shared: Option<&crate::shared::SharedTransport>,
        local: media::TransportState,
        now: u64,
        duration_ms: u64,
    ) -> TogetherStep {
        if self.media_ref.as_deref() != Some(media_ref) {
            // A video just opened: what stood before is the past unless it
            // is still running, or was paused a moment ago.
            let over = shared.filter(|shared| !Self::current(shared, now, duration_ms));
            *self = Self {
                media_ref: Some(media_ref.to_owned()),
                seen: over.cloned(),
                over: over.is_some(),
                ..Self::default()
            };
        }
        if shared != self.seen.as_ref() {
            self.seen = shared.cloned();
            self.over = false;
            self.leading = false;
            if let Some(shared) = shared {
                let state = shared.state();
                if self.said.is_some_and(|said| same(said, state)) {
                    // This player's own change, come back.
                    self.leading = true;
                    return TogetherStep::Stay;
                }
                self.settling_until = now.saturating_add(TOGETHER_SETTLE_MS);
                return TogetherStep::Follow(state);
            }
        }
        if now < self.settling_until {
            return match self.seen.as_ref() {
                Some(shared) => TogetherStep::Follow(shared.state()),
                None => TogetherStep::Stay,
            };
        }
        let joined = self.seen.as_ref().filter(|_| !self.over);
        let moved = match joined.map(crate::shared::SharedTransport::state) {
            None => local.playing,
            Some(shared) => {
                shared.playing != local.playing
                    || (shared.rate - local.rate).abs() > f32::EPSILON
                    || shared.projected_position(now).abs_diff(local.position_ms) > TOGETHER_MOVE_MS
            }
        };
        if moved {
            self.said = Some(local);
            return TogetherStep::Publish(local);
        }
        match joined {
            // Ride along with a peer's playback: small drift rides, the
            // engine corrects larger drift.
            Some(shared) if shared.playing && !self.leading => TogetherStep::Follow(shared.state()),
            _ => TogetherStep::Stay,
        }
    }

    /// Whether `shared` is still something to join: running inside the
    /// video, or paused a moment ago.
    fn current(shared: &crate::shared::SharedTransport, now: u64, duration_ms: u64) -> bool {
        if shared.playing {
            duration_ms == 0 || shared.state().projected_position(now) < duration_ms
        } else {
            now.saturating_sub(shared.epoch_ms) < TOGETHER_PAUSE_LIVES_MS
        }
    }
}

/// Whether two transports say the same, as the document stores them.
fn same(a: media::TransportState, b: media::TransportState) -> bool {
    a.playing == b.playing
        && a.position_ms == b.position_ms
        && a.epoch_ms == b.epoch_ms
        && (a.rate - b.rate).abs() < f32::EPSILON
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

impl PlayerApp {
    /// What the player shows that the document does not hold, for its live
    /// rows.
    pub(crate) fn player_rows(&self) -> crate::actions::PlayerRows<'_> {
        crate::actions::PlayerRows {
            title: self.player.has_media().then_some(self.title.as_str()),
            source_id: self.current_source_id.as_deref(),
            audio_tracks: &self.shown_metadata.audio_tracks,
            subtitle_tracks: &self.shown_metadata.subtitle_tracks,
            chapters: &self.shown_metadata.chapters,
            queue: self.library_items(self.queue.iter()),
            recent: self.library_items(self.recent.iter()),
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
        if self.shown_metadata == metadata {
            return;
        }
        self.shown_metadata = metadata;
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

    /// A player with a fresh engine and no media loaded, for a window built
    /// in `mode`, on the default preferences until a live window reads this
    /// device's settings when it is attached.
    pub fn new(mode: RunMode) -> Self {
        // Tests exercise the real request and completion paths; a recording
        // host answers every picker so no desktop file chooser ever opens.
        #[cfg(test)]
        ui::host::install_default(ui::host::HostServices::recording().0);
        let preferences = PlaybackPreferences::default();
        let player = configured_player(mode, preferences.autoplay, preferences.loop_enabled);
        Self::from_prepared(player, preferences, VecDeque::new(), mode)
    }

    fn from_prepared(
        player: VideoPlayer,
        preferences: PlaybackPreferences,
        recent: VecDeque<LocalMediaEntry>,
        mode: RunMode,
    ) -> Self {
        let kept_retained = recent
            .iter()
            .filter_map(|entry| crate::kept::token_of(&entry.source).map(str::to_owned))
            .collect();
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
            open_focused: false,
            library_focused: false,
            library_focus_id: None,
            together: Together::default(),
            preferences,
            setting: None,
            shot_metadata: None,
            shot_loading: false,
            pending_pip: None,
            dispatcher: None,
            shared_library: crate::shared::SharedLibrary::default(),
            mode,
            capture_awaits: CaptureAwaits::Nothing,
            capture_launched: None,
            current_media_ref: None,
            current_source_id: None,
            source_generation: 0,
            shown_metadata: DecoderMetadata::default(),
            open_error: None,
            kept: HashMap::new(),
            kept_retained,
            kept_recall: None,
            kept_wake: ui::wake::WakeHandle::new(),
            dirty: true,
        }
    }

    /// Open what the launch named: a path or a stream's URL, continuing the
    /// playback another device handed over when it says where that stood.
    /// The handoff's position and play state land after the open, so local
    /// autoplay preferences cannot override them.
    pub fn open_launch(&mut self, launch: &LaunchInput<'_>) {
        let Some(argument) = launch.argument() else {
            return;
        };
        let handoff = launch.handoff();
        let source = source_for_argument(argument);
        let title = handoff
            .and_then(|handoff| handoff.title.clone())
            .unwrap_or_else(|| source_title(&source));
        let queue_id = self.ensure_queued(source.clone(), title.clone());
        if let Err(error) = self.open_local_source(source, title, Some(queue_id)) {
            tracing::warn!(%error, "video player: could not open the launch's video");
            return;
        }
        if let Some(handoff) = handoff {
            self.player.seek_to_ms(handoff.position_ms);
            if handoff.resume_playing {
                self.player.play();
            } else {
                self.player.pause();
            }
        }
    }

    /// Stage a capture of `scenario` (`None` for a plain `--shots`): the
    /// video the launch named, else a synthesized demo clip, the scenario's
    /// view state, and what the capture waits for before it is taken.
    pub(crate) fn stage_capture(&mut self, launched: Option<&str>, scenario: Option<&str>) {
        self.stage_shot_source(launched, scenario);
        self.stage_shot_scenario(scenario);
        if !matches!(scenario, Some("empty" | "error")) {
            self.player.reveal_controls();
        }
    }

    /// A player staged as a capture of `scenario` over the demo clip.
    #[cfg(test)]
    pub(crate) fn for_shot(scenario: Option<&str>) -> Self {
        let mut app = Self::new(RunMode::Capture);
        app.stage_capture(None, scenario);
        app
    }

    fn stage_shot_source(&mut self, launched: Option<&str>, scenario: Option<&str>) {
        if matches!(scenario, Some("empty" | "error")) {
            return;
        }
        if let Some(argument) = launched {
            // A capture of a real file shows that file decoded, never the
            // synthesized stand-in: without the system decoder it waits for
            // a picture that cannot come and refuses to capture.
            let engine = media::MediaEngine::new();
            if !engine.is_real_decode() {
                self.open_error =
                    Some("A real video capture requires the GStreamer decoder and runtime".into());
                self.capture_awaits = CaptureAwaits::Frame;
                return;
            }
            self.player.set_engine(engine);
            let source = source_for_argument(argument);
            let title = source_title(&source);
            let queue_id = self.ensure_queued(source.clone(), title.clone());
            let _ = self.open_local_source(source, title, Some(queue_id));
            self.capture_awaits = match scenario {
                Some("native-error") => CaptureAwaits::Outcome,
                Some("loading") => CaptureAwaits::Nothing,
                _ => CaptureAwaits::Frame,
            };
            return;
        }
        // The demo stays hermetic: the synthesized engine draws a
        // deterministic frame and the placeholder host is never contacted.
        let source = demo_source();
        let queue_id = self.ensure_queued(source.clone(), "Demo clip".into());
        let _ = self.set_current_source(&source);
        self.player.open(source);
        self.title = "Demo clip".to_string();
        self.current_queue_id = Some(queue_id);
        // Park at a fixed position for a stable reference frame. The
        // explicit `playing` fixture starts the engine below; default and
        // `paused` deliberately stay deterministic.
        self.player.seek_to_ms(8_000);
        self.player.pause();
        if scenario != Some("loading") {
            self.capture_awaits = CaptureAwaits::Frame;
        }
    }

    /// Whether a capture still waits for what it shows.
    fn capture_pending(&self) -> bool {
        let pictured = self.player.latest_frame().is_some();
        match self.capture_awaits {
            CaptureAwaits::Nothing => false,
            CaptureAwaits::Frame => !pictured,
            CaptureAwaits::Outcome => !pictured && self.open_error.is_none(),
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
            }
            Some("fullscreen") => self.player.toggle_fullscreen(),
            _ => {}
        }
    }

    /// Show the shared library the document holds.
    pub(crate) fn present_library(&mut self, state: &crate::shared::SharedLibrary) {
        if state != &self.shared_library {
            self.shared_library = state.clone();
            self.dirty = true;
        }
    }

    /// Take part in the framework: its door for edits and controls, and in
    /// a live window this device's settings and what the launch named. A
    /// capture keeps what its launch named for the scenario to stage.
    pub(crate) fn attach_framework(&mut self, cx: &SurfaceCx<PlayerResource>) {
        self.dispatcher = Some(cx.dispatcher());
        match cx.mode() {
            RunMode::Live => {
                let setting = cx.settings::<PlayerSettings>();
                self.adopt_settings(&setting.get());
                self.setting = Some(setting);
                self.open_launch(&cx.launch());
            }
            RunMode::Capture => {
                self.capture_launched = cx.launch().argument().map(str::to_owned);
            }
            RunMode::Past => {}
        }
    }

    /// Follow this device's stored preferences and recent videos.
    fn adopt_settings(&mut self, stored: &PlayerSettings) {
        self.preferences = PlaybackPreferences {
            autoplay: stored.autoplay,
            loop_enabled: stored.loop_enabled,
        };
        configure_player_settings(
            &mut self.player,
            self.preferences.autoplay,
            self.preferences.loop_enabled,
        );
        self.recent = stored
            .recent_history
            .iter()
            .map(|entry| LocalMediaEntry {
                id: entry.id.clone(),
                title: entry.title.clone(),
                source: entry.source.clone(),
            })
            .collect();
        self.kept_retained = self
            .recent
            .iter()
            .filter_map(|entry| crate::kept::token_of(&entry.source).map(str::to_owned))
            .collect();
        self.dirty = true;
    }

    /// The open video's shareable identity, which the shared library's
    /// notes and transport name it by; never a path.
    pub fn media_ref(&self) -> Option<&str> {
        self.current_media_ref.as_deref()
    }

    /// Whether the library panel shows.
    pub fn library_visible(&self) -> bool {
        self.library_visible
    }

    /// The library tab shown or last shown: `queue`, `recent` or `shared`.
    pub fn library_tab_name(&self) -> &'static str {
        self.library_tab.name()
    }

    /// Shared read access to the inner video surface, for tests that assert
    /// a decoded frame landed.
    pub fn player(&self) -> &VideoPlayer {
        &self.player
    }

    fn open_path(&mut self, path: &Path) -> Result<(), String> {
        let source = source_for_path(path);
        let title = source_title(&source);
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
        self.fail_open("a newer video open replaced this decoder request".into());
        self.open_error = None;
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
        match &self.setting {
            Some(setting) => setting.update(|stored| stored.recent_history = entries),
            None => Ok(()),
        }
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

    fn close_media(&mut self) {
        self.fail_open("video was closed before decoding a first frame".into());
        let player = configured_player(
            self.mode,
            self.preferences.autoplay,
            self.preferences.loop_enabled,
        );
        self.player = player;
        self.title = "No media".into();
        self.current_source_id = None;
        self.current_media_ref = None;
        self.current_queue_id = None;
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

    /// Invoke `effect` through the framework's door, as the palette and
    /// agents do: its availability and permission are checked there.
    fn request(&self, effect: &VideoEffect) {
        if let Some(dispatcher) = &self.dispatcher {
            dispatcher.effect(effect);
        }
    }

    fn request_open_action(&self) {
        self.request(&VideoEffect::Open { path: None });
    }

    fn request_library_play(&self, item_id: &str) {
        self.request(&VideoEffect::PlayQueued {
            item: item_id.to_owned(),
        });
    }

    fn request_library_remove(&self, item_id: &str) {
        self.request(&VideoEffect::RemoveQueued {
            item: item_id.to_owned(),
        });
    }

    fn request_library_clear(&self) {
        self.request(&VideoEffect::ClearRecent);
    }

    fn request_library_hide(&self) {
        self.request(&VideoEffect::HideLibrary);
    }

    fn request_library_tab(&self, tab: LibraryTab) {
        self.request(&VideoEffect::ShowLibrary {
            tab: tab.name().to_owned(),
        });
    }

    /// The Library button and `L` shortcut choose a concrete, replay-safe
    /// effect from the current presentation. Nothing is a toggle: a replay
    /// or delayed agent invocation must not invert a newer user choice.
    fn request_library_visibility_change(&self) {
        if self.library_visible {
            self.request_library_hide();
        } else {
            self.request_library_tab(self.library_tab);
        }
    }

    fn request_annotation_jump(&self, annotation_id: &str) {
        self.request(&VideoEffect::JumpToAnnotation {
            annotation: annotation_id.to_owned(),
        });
    }

    /// Submit a shared library edit through the framework's door. The
    /// library shown follows once the document presents the change.
    fn dispatch_shared(&self, action: &crate::shared::SharedAction) {
        if let Some(dispatcher) = &self.dispatcher {
            dispatcher.apply(action);
        }
    }

    fn delete_annotation(&self, id: &str) {
        self.dispatch_shared(&crate::shared::SharedAction::DeleteAnnotation { id: id.into() });
    }

    /// Delete playlist `id` with the items it held when the user chose to.
    fn delete_playlist(&self, id: &str) {
        self.dispatch_shared(&crate::shared::SharedAction::DeletePlaylist {
            id: id.into(),
            observed_item_ids: self
                .shared_library
                .items
                .iter()
                .filter(|item| item.playlist_id == id)
                .map(|item| item.id.clone())
                .collect(),
        });
    }

    /// The edits that add the open video to the shared Watch later playlist,
    /// creating the playlist first when it is missing.
    fn watch_later_actions(&self) -> Vec<crate::shared::SharedAction> {
        let Some(media_ref) = self.current_media_ref.clone() else {
            return Vec::new();
        };
        let library = &self.shared_library;
        let playlist_id = WATCH_LATER.to_string();
        if library
            .items
            .iter()
            .any(|item| item.playlist_id == playlist_id && item.media_ref == media_ref)
        {
            return Vec::new();
        }
        let mut actions = Vec::new();
        if !library
            .playlists
            .iter()
            .any(|playlist| playlist.id == playlist_id)
        {
            actions.push(crate::shared::SharedAction::CreatePlaylist {
                id: playlist_id.clone(),
                name: "Watch later".into(),
            });
        }
        let position = library
            .items
            .iter()
            .filter(|item| item.playlist_id == playlist_id)
            .map(|item| item.position)
            .max()
            .unwrap_or(0)
            .saturating_add(1024);
        actions.push(crate::shared::SharedAction::AddPlaylistItem {
            id: format!(
                "item-{}-{}",
                crate::actions::identity_token_for_action(&media_ref),
                unix_now_ms()
            ),
            playlist_id,
            media_ref,
            title: self.title.clone(),
            position,
        });
        actions
    }

    fn add_current_to_watch_later(&self) {
        let actions = self.watch_later_actions();
        if let Some(dispatcher) = &self.dispatcher {
            // Creating the playlist and adding the video is one step.
            dispatcher.group(|| {
                for action in &actions {
                    dispatcher.apply(action);
                }
            });
        }
    }

    /// A bookmark of the moment the player shows, on the open video.
    fn bookmark_action(&self) -> Option<crate::shared::SharedAction> {
        let media_ref = self.current_media_ref.clone()?;
        let time_ms = self.player.position_ms();
        Some(crate::shared::SharedAction::CreateAnnotation {
            id: format!(
                "annotation-{}-{}-{time_ms}",
                crate::actions::identity_token_for_action(&media_ref),
                unix_now_ms()
            ),
            media_ref,
            time_ms,
            text: String::new(),
        })
    }

    fn bookmark_current_time(&self) {
        if let Some(action) = self.bookmark_action() {
            self.dispatch_shared(&action);
        }
    }

    /// The shared library edits bound to the playhead, for the palette and
    /// agents: bookmark the moment shown, add the open video to Watch later.
    pub(crate) fn playhead_declarations(&self) -> Vec<ActionDecl<PlayerResource>> {
        if self.current_media_ref.is_none() {
            return Vec::new();
        }
        let media_ref = self.current_media_ref.clone().unwrap_or_default();
        let time_ms = self.player.position_ms();
        let watch_later = self.watch_later_actions();
        let mut declarations = vec![
            ActionDecl::new(
                "bookmark-current-time",
                "Bookmark this moment",
                Vec::new(),
                move |_, now_ms| {
                    Ok(crate::shared::SharedAction::CreateAnnotation {
                        id: format!(
                            "annotation-{}-{now_ms}-{time_ms}",
                            crate::actions::identity_token_for_action(&media_ref)
                        ),
                        media_ref: media_ref.clone(),
                        time_ms,
                        text: String::new(),
                    })
                },
            )
            .describe("Add a shared bookmark at the playback time shown."),
        ];
        // Only the item when the playlist exists; a missing playlist is
        // created by the button, which submits both as one step.
        if let [crate::shared::SharedAction::AddPlaylistItem { .. }] = watch_later.as_slice() {
            let add = watch_later.into_iter().next();
            declarations.push(
                ActionDecl::new(
                    "add-to-watch-later",
                    "Add to Watch later",
                    Vec::new(),
                    move |_, _| add.clone().ok_or_else(|| "nothing to add".to_owned()),
                )
                .describe("Add the open video to the shared Watch later playlist."),
            );
        }
        declarations
    }

    /// Kick off a file-picker open, answering `reply` once a picture of the
    /// chosen video decodes. A second request is refused rather than
    /// replacing the first, which would make its answer race.
    fn trigger_open(&mut self, reply: Option<EffectReply>) -> Result<(), String> {
        if self.portal_open.is_some() {
            return Err("a video picker is already open".into());
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
            reply,
        });
        self.dirty = true;
        Ok(())
    }

    /// Drop a picker receiver that no longer represents the user's intent.
    /// The picker may still answer, but nothing receives it.
    fn cancel_pending_portal(&mut self, message: &str) {
        let Some(pending) = self.portal_open.take() else {
            return;
        };
        if let Some(reply) = pending.reply {
            reply.finish(Err(message.into()));
        }
    }

    /// Answer `reply` once the open video's first picture decodes, or with
    /// why it did not.
    fn await_open_frame(&mut self, reply: EffectReply) {
        if let Some(previous) = self.media_open.take() {
            previous
                .reply
                .finish(Err("a newer video open replaced this one".into()));
        }
        let Some(source_id) = self.current_source_id.clone() else {
            reply.finish(Err("the video did not open".into()));
            return;
        };
        self.media_open = Some(PendingMediaOpen {
            reply,
            source_id,
            deadline: ui::time::now() + OPEN_FRAME_TIMEOUT,
        });
    }

    /// Answer the pending open: a picture of its video decoded, another
    /// video replaced it, or the decoder ran out of time.
    fn poll_open_frame(&mut self) {
        let Some(pending) = self.media_open.as_ref() else {
            return;
        };
        let outcome = if self.current_source_id.as_deref() != Some(pending.source_id.as_str()) {
            Some(Err(
                "The requested video was superseded before decoding a frame.".to_string(),
            ))
        } else if self.player.latest_frame().is_some() {
            self.record_current_history();
            Some(Ok(()))
        } else if ui::time::now() >= pending.deadline {
            let message = "The decoder did not produce a first frame in time.".to_string();
            self.open_error = Some(message.clone());
            self.dirty = true;
            Some(Err(message))
        } else {
            None
        };
        if let Some(outcome) = outcome
            && let Some(pending) = self.media_open.take()
        {
            pending
                .reply
                .finish(outcome.map(|()| tilo_actions::ActionOutput::default()));
        }
    }

    /// Fail the pending open with the decoder's `message`.
    fn fail_open(&mut self, message: String) {
        if let Some(pending) = self.media_open.take() {
            pending.reply.finish(Err(message));
        }
    }

    /// Answer `effect`, invoked by a control, the palette, an agent or a
    /// script. Opening answers once a picture decodes.
    pub(crate) fn answer_effect(
        &mut self,
        effect: VideoEffect,
        cx: &mut EffectCx<PlayerResource, Self>,
    ) -> EffectAnswer {
        if effect.requires_media() && !self.player.has_media() {
            return Err("open a video before using this playback control".into());
        }
        match effect {
            VideoEffect::Open { path: None } => {
                self.trigger_open(Some(cx.reply()))?;
                Ok(Effected::Pending)
            }
            VideoEffect::Open { path: Some(path) } => {
                self.open_named(&path)?;
                self.await_open_frame(cx.reply());
                Ok(Effected::Pending)
            }
            VideoEffect::PlayQueued { item } => {
                self.open_local_entry(&item)?;
                self.await_open_frame(cx.reply());
                Ok(Effected::Pending)
            }
            effect => self.perform(effect).map(|()| Effected::done()),
        }
    }

    /// Open the video `path` names on this device: a file, or a stream's URL.
    fn open_named(&mut self, path: &str) -> Result<(), String> {
        let source = source_for_argument(path);
        if let media::Source::LocalFile(file) | media::Source::Workspace(file) = &source
            && !file.is_file()
        {
            let message = format!("Video does not exist: {}", source_title(&source));
            self.open_error = Some(message.clone());
            self.dirty = true;
            return Err(message);
        }
        let title = source_title(&source);
        let queue_id = self.ensure_queued(source.clone(), title.clone());
        self.open_local_source(source, title, Some(queue_id))
    }

    /// What differs from the effects' declarations right now: playback
    /// controls need an open video, and clearing needs a recent one.
    pub(crate) fn effect_statuses(&self) -> Vec<EffectStatus> {
        let mut statuses = Vec::new();
        if !self.player.has_media() {
            for effect in [
                VideoEffect::CLOSE.id,
                VideoEffect::TOGGLE_PLAYBACK.id,
                VideoEffect::TOGGLE_MUTE.id,
                VideoEffect::REVEAL.id,
                VideoEffect::SEEK.id,
                VideoEffect::SEEK_BY.id,
                VideoEffect::RESTART.id,
                VideoEffect::JUMP_TO_END.id,
                VideoEffect::SET_VOLUME.id,
                VideoEffect::SET_PLAYBACK_RATE.id,
                VideoEffect::SUBTITLES_OFF.id,
                VideoEffect::PICTURE_IN_PICTURE.id,
            ] {
                statuses.push(EffectStatus::unavailable(
                    ui::tilo_resource::ActionRef::<VideoEffect, ()>::new(effect, &[]),
                    "no_media",
                    "Open a video first",
                ));
            }
        }
        if self.recent.is_empty() {
            statuses.push(EffectStatus::unavailable(
                VideoEffect::CLEAR_RECENT,
                "no_recent",
                "No video has been played on this device",
            ));
        }
        statuses
    }

    /// Do `effect` now; refused when its target changed since it was
    /// offered.
    #[allow(
        clippy::too_many_lines,
        reason = "all effects stay together so their local semantics cannot drift"
    )]
    fn perform(&mut self, effect: VideoEffect) -> Result<(), String> {
        match effect {
            VideoEffect::Open { path: Some(path) } => self.open_named(&path)?,
            VideoEffect::Open { path: None } => self.trigger_open(None)?,
            VideoEffect::PlayQueued { item } => self.open_local_entry(&item)?,
            VideoEffect::RemoveQueued { item } => self.remove_queue_entry(&item)?,
            VideoEffect::ClearRecent => self.clear_recent_history()?,
            VideoEffect::ShowLibrary { tab } => {
                self.library_tab = LibraryTab::named(&tab)
                    .ok_or_else(|| format!("no library tab `{tab}`: queue, recent or shared"))?;
                self.library_visible = true;
                self.player.reveal_controls();
            }
            VideoEffect::HideLibrary => {
                self.library_visible = false;
                self.library_focus_id = None;
            }
            VideoEffect::Close => self.close_media(),
            VideoEffect::TogglePlayback => self.player.toggle_play(),
            VideoEffect::ToggleMute => self.player.toggle_mute(),
            VideoEffect::Reveal => self.player.reveal_controls(),
            VideoEffect::Seek { seconds } => {
                self.player.seek_to_ms((seconds.max(0.0) * 1000.0) as u64);
            }
            VideoEffect::SeekBy { seconds } => {
                let current = self.player.position_ms() as f64;
                let duration = self.player.duration_ms() as f64;
                let target =
                    (current + seconds.clamp(-3600.0, 3600.0) * 1000.0).clamp(0.0, duration) as u64;
                self.player.seek_to_ms(target);
            }
            VideoEffect::Restart => self.player.seek_to_ms(0),
            VideoEffect::JumpToEnd => self.player.seek_to_ms(self.player.duration_ms()),
            VideoEffect::SetVolume { volume } => {
                self.player.set_volume(volume.clamp(0.0, 1.0) as f32);
            }
            VideoEffect::SetAutoplay { enabled } => {
                if self.preferences.autoplay != enabled {
                    self.preferences.autoplay = enabled;
                    self.preferences_changed();
                }
            }
            VideoEffect::SetLoop { enabled } => {
                if self.preferences.loop_enabled != enabled {
                    self.preferences.loop_enabled = enabled;
                    self.preferences_changed();
                }
            }
            VideoEffect::SetPlaybackRate { rate } => {
                let rate = rate.clamp(0.25, 4.0);
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
            VideoEffect::SelectAudioTrack { track } => {
                let index = self
                    .track_named(&track, true)
                    .ok_or("the audio track changed; refresh and try again")?
                    .index;
                if !self
                    .player
                    .engine_mut()
                    .is_some_and(|engine| engine.select_audio_track(index))
                {
                    return Err("the decoder refused that audio track".into());
                }
            }
            VideoEffect::SelectSubtitles { track } => {
                let index = self
                    .track_named(&track, false)
                    .ok_or("the subtitle track changed; refresh and try again")?
                    .index;
                if !self
                    .player
                    .engine_mut()
                    .is_some_and(|engine| engine.select_subtitle_track(Some(index)))
                {
                    return Err("the decoder refused that subtitle track".into());
                }
            }
            VideoEffect::SubtitlesOff => {
                if !self
                    .player
                    .engine_mut()
                    .is_some_and(|engine| engine.select_subtitle_track(None))
                {
                    return Err("the decoder refused to turn subtitles off".into());
                }
            }
            VideoEffect::JumpToChapter { chapter } => {
                let source_id = self.current_source_id.clone().unwrap_or_default();
                let seconds = self
                    .shown_metadata
                    .chapters
                    .iter()
                    .find(|candidate| {
                        crate::actions::chapter_resource_id(&source_id, &candidate.id) == chapter
                    })
                    .ok_or("the chapter changed; refresh and try again")?
                    .seconds;
                self.player.seek_to_ms((seconds.max(0.0) * 1000.0) as u64);
            }
            VideoEffect::JumpToAnnotation { annotation } => {
                let time_ms = self
                    .shared_library
                    .annotations
                    .iter()
                    .find(|candidate| candidate.id == annotation)
                    .ok_or("the review note is gone")?;
                if self.current_media_ref.as_deref() != Some(time_ms.media_ref.as_str()) {
                    return Err("this review note belongs to a different video".into());
                }
                let time_ms = time_ms.time_ms;
                self.player.seek_to_ms(time_ms);
                self.player.reveal_controls();
            }
            VideoEffect::PictureInPicture => self.request_picture_in_picture(),
            VideoEffect::ToggleFullscreen => self.player.toggle_fullscreen(),
        }
        self.dirty = true;
        Ok(())
    }

    /// The decoder's audio (`audio`) or subtitle track a row id names, for
    /// the open source.
    fn track_named(&self, row: &str, audio: bool) -> Option<&crate::actions::TrackState> {
        let source_id = self.current_source_id.as_deref()?;
        let tracks = if audio {
            &self.shown_metadata.audio_tracks
        } else {
            &self.shown_metadata.subtitle_tracks
        };
        tracks.iter().find(|track| {
            let id = if audio {
                crate::actions::audio_resource_id(source_id, &track.stream_id)
            } else {
                crate::actions::subtitle_resource_id(source_id, &track.stream_id)
            };
            id == row
        })
    }

    /// Show and keep the preferences just changed.
    fn preferences_changed(&mut self) {
        configure_player_settings(
            &mut self.player,
            self.preferences.autoplay,
            self.preferences.loop_enabled,
        );
        if let Some(setting) = &self.setting {
            let preferences = self.preferences;
            // A device-local preference must never prevent playback; the
            // next change retries the write.
            if let Err(error) = setting.update(|stored| {
                stored.autoplay = preferences.autoplay;
                stored.loop_enabled = preferences.loop_enabled;
            }) {
                tracing::warn!(%error, "video player: could not keep playback preferences");
            }
        }
    }

    /// Drain a pending picker answer, opening the chosen file.
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
                Ok(result) => outcome = Some((request.generation, result)),
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    outcome = Some((
                        request.generation,
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
        let reply = pending.and_then(|pending| pending.reply);
        // The receiver is dropped before opening the chosen file, so the
        // open cannot cancel the request it completes.
        let Some((generation, outcome)) = outcome else {
            return;
        };
        if generation != self.portal_generation {
            tracing::warn!(
                generation,
                current_generation = self.portal_generation,
                "video player: ignored a stale portal outcome"
            );
            if let Some(reply) = reply {
                reply.finish(Err(
                    "ignored a stale video picker response; open again".into()
                ));
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
        let Some(reply) = reply else {
            return;
        };
        match result {
            // Picking is not decoding: answer once a picture arrives.
            Ok(true) => self.await_open_frame(reply),
            Ok(false) => reply.finish(Err("video selection was cancelled".into())),
            Err(message) => reply.finish(Err(message)),
        }
    }

    /// Watch together: follow where playback of the open video stands for
    /// everyone, and say where it stands here when this player moved it
    /// (played, paused, sought, changed speed). Only a live window with the
    /// right to edit says so; a viewer follows.
    fn tick_together(&mut self) {
        if !self.mode.is_live() {
            return;
        }
        let (Some(media_ref), Some(dispatcher)) =
            (self.current_media_ref.clone(), self.dispatcher.clone())
        else {
            return;
        };
        let now = unix_now_ms();
        let shared = self.shared_library.transport_for(&media_ref).cloned();
        let Some(engine) = self.player.engine_mut() else {
            return;
        };
        let local = engine.transport_state(now);
        let step = self.together.step(
            &media_ref,
            shared.as_ref(),
            local,
            now,
            engine.duration_ms().unwrap_or(0),
        );
        match step {
            TogetherStep::Follow(state) => {
                engine.apply_transport(&state, now);
                self.dirty = true;
            }
            TogetherStep::Publish(state) if !dispatcher.read_only() => {
                dispatcher.apply(&crate::shared::SharedAction::SetTransport {
                    media_ref,
                    playing: state.playing,
                    position_ms: state.position_ms,
                    rate: f64::from(state.rate),
                    epoch_ms: state.epoch_ms,
                });
            }
            TogetherStep::Publish(_) | TogetherStep::Stay => {}
        }
    }

    /// Do what the player's own settings menu chose.
    fn drain_player_settings(&mut self) {
        const RATES: [f64; 8] = [0.25, 0.5, 0.75, 1.0, 1.25, 1.5, 2.0, 4.0];
        for event in self.player.take_setting_events() {
            let source_id = self.current_source_id.clone().unwrap_or_default();
            let effect = match event {
                PlayerSettingEvent::Toggled { id, on } if id == "autoplay" => {
                    Some(VideoEffect::SetAutoplay { enabled: on })
                }
                PlayerSettingEvent::Toggled { id, on } if id == "loop" => {
                    Some(VideoEffect::SetLoop { enabled: on })
                }
                PlayerSettingEvent::Chose { id, index } if id == "playback-rate" => RATES
                    .get(index)
                    .map(|rate| VideoEffect::SetPlaybackRate { rate: *rate }),
                PlayerSettingEvent::Chose { id, index } if id == "audio-track" => {
                    self.shown_metadata.audio_tracks.get(index).map(|track| {
                        VideoEffect::SelectAudioTrack {
                            track: crate::actions::audio_resource_id(&source_id, &track.stream_id),
                        }
                    })
                }
                PlayerSettingEvent::Chose { id, index } if id == "subtitle-track" => {
                    Some(match index.checked_sub(1) {
                        None => VideoEffect::SubtitlesOff,
                        Some(index) => match self.shown_metadata.subtitle_tracks.get(index) {
                            Some(track) => VideoEffect::SelectSubtitles {
                                track: crate::actions::subtitle_resource_id(
                                    &source_id,
                                    &track.stream_id,
                                ),
                            },
                            None => continue,
                        },
                    })
                }
                PlayerSettingEvent::Chose { id, index } if id == "chapters" => {
                    self.shown_metadata.chapters.get(index).map(|chapter| {
                        VideoEffect::JumpToChapter {
                            chapter: crate::actions::chapter_resource_id(&source_id, &chapter.id),
                        }
                    })
                }
                PlayerSettingEvent::Activated { id } if id == "picture-in-picture" => {
                    Some(VideoEffect::PictureInPicture)
                }
                PlayerSettingEvent::Toggled { .. }
                | PlayerSettingEvent::Chose { .. }
                | PlayerSettingEvent::Activated { .. } => None,
            };
            if let Some(effect) = effect
                && let Err(error) = self.perform(effect)
            {
                tracing::warn!(%error, "video player: a settings choice was refused");
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
                    self.fail_open(message);
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

    fn maybe_loop_current_media(&mut self) {
        if !self.preferences.loop_enabled || !self.player.has_media() {
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

    fn handle_app_pointer(&mut self) -> bool {
        if self.library_visible && self.library_panel_rect.contains(self.last_cursor) {
            if self.library_close_rect.contains(self.last_cursor) {
                self.request_library_hide();
            } else if self.library_queue_tab_rect.contains(self.last_cursor) {
                self.request_library_tab(LibraryTab::Queue);
            } else if self.library_recent_tab_rect.contains(self.last_cursor) {
                self.request_library_tab(LibraryTab::Recent);
            } else if self.library_shared_tab_rect.contains(self.last_cursor) {
                self.request_library_tab(LibraryTab::Shared);
            } else if self.library_tab == LibraryTab::Shared
                && self.shared_watch_later_rect.contains(self.last_cursor)
            {
                self.add_current_to_watch_later();
            } else if self.library_tab == LibraryTab::Shared
                && self.shared_bookmark_rect.contains(self.last_cursor)
            {
                self.bookmark_current_time();
            } else if self.library_tab == LibraryTab::Shared
                && let Some(row) = self
                    .shared_row_hits
                    .iter()
                    .find(|row| row.jump.is_some_and(|jump| jump.contains(self.last_cursor)))
                    .cloned()
            {
                self.request_annotation_jump(&row.id);
            } else if self.library_tab == LibraryTab::Shared
                && let Some(row) = self
                    .shared_row_hits
                    .iter()
                    .find(|row| row.remove.contains(self.last_cursor))
                    .cloned()
            {
                if row.annotation {
                    self.delete_annotation(&row.id);
                } else {
                    self.delete_playlist(&row.id);
                }
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
        &self,
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
            self.request(&VideoEffect::PictureInPicture);
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
    );

    fn sync(&mut self, state: &AppModel, path: &ModelPath) {
        let (state, path) = ui::doc::document_ref(state, path);
        self.poll_kept_recall();
        self.drain_portal();
        self.tick_together();
        self.player.sync(state, path);
        self.drain_player_media_events();
        self.sync_decoder_settings();
        self.drain_player_settings();
        self.maybe_loop_current_media();
        self.poll_open_frame();
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
            && self.handle_app_pointer()
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
                    disabled: self.portal_open.is_some(),
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
        _state: &mut AppModel,
        _path: &ModelPath,
    ) -> bool {
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
            self.request_library_tab(LibraryTab::Queue);
            true
        } else if target == "video-player.library.tab.recent"
            && matches!(action, ui::a11y::Action::Activate)
        {
            self.request_library_tab(LibraryTab::Recent);
            true
        } else if target == "video-player.library.tab.shared"
            && matches!(action, ui::a11y::Action::Activate)
        {
            self.request_library_tab(LibraryTab::Shared);
            true
        } else if target == "video-player.shared.watch-later"
            && matches!(action, ui::a11y::Action::Activate)
        {
            self.add_current_to_watch_later();
            true
        } else if target == "video-player.shared.bookmark"
            && matches!(action, ui::a11y::Action::Activate)
        {
            self.bookmark_current_time();
            true
        } else if let Some(rest) = target.strip_prefix("video-player.shared.jump.annotation.")
            && matches!(action, ui::a11y::Action::Activate)
        {
            self.request_annotation_jump(rest);
            true
        } else if let Some(rest) = target.strip_prefix("video-player.shared.delete.annotation.")
            && matches!(action, ui::a11y::Action::Activate)
        {
            self.delete_annotation(rest);
            true
        } else if let Some(rest) = target.strip_prefix("video-player.shared.delete.playlist.")
            && matches!(action, ui::a11y::Action::Activate)
        {
            self.delete_playlist(rest);
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
        // Playback and the portal are the only frame-rate work; a paused
        // player wakes for a peer's change when the document presents it.
        self.player.needs_animation_frame()
            || self.portal_open.is_some()
            || self.media_open.is_some()
    }

    fn has_pending_visual_work(&self) -> bool {
        self.player.has_pending_visual_work() || self.capture_pending()
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
            let mut app = PlayerApp::new(RunMode::Capture);
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
            let mut app = PlayerApp::for_shot(Some("metadata"));
            let old_source = app.current_source_id.clone().unwrap();
            let source = media::Source::Remote {
                url: "https://demo.local/clip.mp4".into(),
            };
            app.set_current_source(&source)
                .expect("source identity available");
            assert_ne!(app.current_source_id.as_deref(), Some(old_source.as_str()));
            let track = app.shot_metadata.as_ref().unwrap().audio_tracks[0].clone();
            assert!(
                app.perform(VideoEffect::SelectAudioTrack {
                    track: crate::actions::audio_resource_id(&old_source, &track.stream_id),
                })
                .is_err(),
                "a row of the previous open names no stream of this one"
            );
        }
    }

    #[test]
    fn portal_open_is_single_flight_generation_fenced_and_retained() {
        // Case `portal_outcome_opens_the_chosen_file`.
        {
            let mut app = PlayerApp::new(RunMode::Capture);
            let (services, recorder) = ui::host::HostServices::recording();
            recorder.respond_to_pick(PortalOutcome::Opened(PathBuf::from("/tmp/Chosen.webm")));
            ui::host::scoped(&services, || app.trigger_open(None)).expect("picker starts");
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
            let mut app = PlayerApp::new(RunMode::Capture);
            app.trigger_open(None).expect("first picker starts");
            let first_generation = app.portal_open.as_ref().map(|open| open.generation);
            assert!(
                app.trigger_open(None).is_err(),
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
            let mut app = PlayerApp::new(RunMode::Capture);
            app.portal_generation = u64::MAX;
            let error = app
                .trigger_open(None)
                .expect_err("generation must not wrap");
            assert!(error.contains("generation exhausted"));
            assert!(app.portal_open.is_none());
            assert_eq!(app.portal_generation, u64::MAX);
        }
        // Case `stale_portal_outcome_cannot_open_media_in_release_or_debug_builds`.
        {
            let mut app = PlayerApp::new(RunMode::Capture);
            let (services, recorder) = ui::host::HostServices::recording();
            recorder.respond_to_pick(PortalOutcome::Opened(PathBuf::from("/tmp/Stale.webm")));
            ui::host::scoped(&services, || app.trigger_open(None)).expect("picker starts");
            // A newer request generation makes the delivered outcome stale.
            app.portal_generation += 1;
            app.drain_portal();
            assert_eq!(app.title, "No media");
            assert!(app.current_source_id.is_none());
            assert!(app.portal_open.is_none());
        }
        // Case `portal_failure_is_retained_for_the_empty_state`.
        {
            let mut app = PlayerApp::new(RunMode::Capture);
            let (services, recorder) = ui::host::HostServices::recording();
            recorder.respond_to_pick(PortalOutcome::Failed("permission denied".into()));
            ui::host::scoped(&services, || app.trigger_open(None)).expect("picker starts");
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
        // Case `marker_jump_seeks_the_video_its_note_belongs_to`.
        {
            let mut app = synth_app();
            app.open_path(Path::new("/tmp/review-cut.mp4"))
                .expect("source identity");
            let media_ref = app.current_media_ref.clone().expect("durable media ref");
            app.present_library(&crate::shared::SharedLibrary {
                annotations: vec![
                    crate::shared::TimestampAnnotation {
                        id: "review-marker".into(),
                        media_ref,
                        time_ms: 12_500,
                        text: "Tighten this cut".into(),
                    },
                    crate::shared::TimestampAnnotation {
                        id: "elsewhere".into(),
                        media_ref: "another-video".into(),
                        time_ms: 30_000,
                        text: String::new(),
                    },
                ],
                ..crate::shared::SharedLibrary::default()
            });
            app.perform(VideoEffect::JumpToAnnotation {
                annotation: "review-marker".into(),
            })
            .expect("the note belongs to the open video");
            assert_eq!(app.player.position_ms(), 12_500);
            let error = app
                .perform(VideoEffect::JumpToAnnotation {
                    annotation: "elsewhere".into(),
                })
                .expect_err("a note on another video is refused");
            assert!(error.contains("different video"));
            assert!(
                app.perform(VideoEffect::JumpToAnnotation {
                    annotation: "gone".into(),
                })
                .is_err(),
                "a deleted note is refused"
            );
            assert_eq!(app.player.position_ms(), 12_500);
        }
        // Case `stale_stream_and_chapter_targets_are_refused_before_engine_mutation`.
        {
            let mut app = PlayerApp::for_shot(Some("metadata"));
            let audio = app.shot_metadata.as_ref().unwrap().audio_tracks[0].clone();
            let chapter = app.shot_metadata.as_ref().unwrap().chapters[0].clone();
            let stale_source = "https://old.example/clip.mkv";
            assert!(
                app.perform(VideoEffect::SelectAudioTrack {
                    track: crate::actions::audio_resource_id(stale_source, &audio.stream_id),
                })
                .is_err()
            );
            let position = app.player.position_ms();
            assert!(
                app.perform(VideoEffect::JumpToChapter {
                    chapter: crate::actions::chapter_resource_id(stale_source, &chapter.id),
                })
                .is_err()
            );
            assert_eq!(app.player.position_ms(), position, "nothing moved");
            let source_id = app.current_source_id.clone().unwrap();
            app.perform(VideoEffect::JumpToChapter {
                chapter: crate::actions::chapter_resource_id(&source_id, &chapter.id),
            })
            .expect("a chapter of the open source");
            assert_eq!(
                app.player.position_ms(),
                0,
                "the first chapter starts the clip"
            );
        }
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "merged scenario: each former test is a labelled case block with its own fixture"
    )]
    fn library_controls_emit_canonical_host_actions_and_are_accessible() {
        // Case `library_play_focus_has_a_real_accessibility_target_and_visual_state`.
        {
            let mut app = PlayerApp::for_shot(Some("library"));
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
            let mut app = PlayerApp::for_shot(Some("library"));
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

            app.perform(VideoEffect::ShowLibrary {
                tab: "recent".into(),
            })
            .unwrap();
            assert_eq!(app.library_tab, LibraryTab::Recent);
            assert!(
                app.perform(VideoEffect::ShowLibrary {
                    tab: "elsewhere".into()
                })
                .is_err(),
                "an unknown tab is refused"
            );
            app.perform(VideoEffect::HideLibrary).unwrap();
            assert!(!app.library_visible);
        }
        // Case `library_has_conventional_escape_close_and_complete_hover_help`.
        {
            let mut app = PlayerApp::for_shot(Some("shared"));
            app.library_close_rect = Rect::new(0.0, 0.0, 44.0, 44.0);
            app.last_cursor = Point::new(20.0, 20.0);
            assert_eq!(app.tooltip(), Some("Close library (Escape)"));

            assert!(
                app.handle_app_shortcut_input(
                    &winit::keyboard::Key::Named(winit::keyboard::NamedKey::Escape),
                    ElementState::Pressed,
                    winit::event::Modifiers::default(),
                ),
                "Escape is the library's close key"
            );
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
            app.perform(VideoEffect::PictureInPicture).unwrap();
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
        // Case `live_rows_cover_the_open_video_its_streams_and_the_library`.
        {
            let app = PlayerApp::for_shot(Some("metadata"));
            let rows: Vec<_> = crate::actions::live_rows(&app.player_rows(), 32)
                .into_iter()
                .map(ui::framework::LiveRow::into_resource)
                .collect();
            let kinds: Vec<&str> = rows.iter().map(|row| row.kind.as_str()).collect();
            assert_eq!(kinds, ["media.video", "media.queue", "media.history"]);
            let media = &rows[0];
            assert_eq!(media.label, "Demo clip");
            let tracks = &media.children[0];
            assert_eq!(tracks.kind, "media.tracks");
            assert_eq!(
                tracks
                    .children
                    .iter()
                    .map(|track| track.label.as_str())
                    .collect::<Vec<_>>(),
                [
                    "English stereo · selected",
                    "Deutsch 5.1",
                    "English captions · selected"
                ]
            );
            assert!(
                tracks.children.iter().all(|track| track
                    .actions
                    .iter()
                    .all(|action| action.kind != tilo_actions::OperationKind::Document)),
                "a live row never changes the document"
            );
            let source_id = app.current_source_id.clone().unwrap();
            let chapter = crate::actions::chapter_resource_id(&source_id, "scene");
            assert!(
                crate::actions::live_row(&app.player_rows(), &chapter).is_some(),
                "a row resolves by id"
            );
        }
        // Case `missing_media_leaves_transport_unavailable`.
        {
            let empty = PlayerApp::new(RunMode::Capture);
            assert!(empty.effect_statuses().iter().any(|status| {
                format!("{status:?}").contains("toggle-playback")
                    && format!("{status:?}").contains("no_media")
            }));
            let mut app = synth_app();
            app.open_path(Path::new("/tmp/Clip.mp4"))
                .expect("source identity available");
            assert!(
                !app.effect_statuses()
                    .iter()
                    .any(|status| { format!("{status:?}").contains("toggle-playback") })
            );
        }
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
                let mut app = lay_out(PlayerApp::for_shot(Some("playing")), (1200.0, 800.0), scale);
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
                let app = lay_out(PlayerApp::for_shot(Some("playing")), (1200.0, 800.0), scale);
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

            let app = lay_out(PlayerApp::for_shot(Some("library")), (1200.0, 800.0), 1.0);
            assert_eq!(app.library_close_rect, g.close);
            assert_eq!(app.library_queue_tab_rect, g.queue_tab);
            assert_eq!(app.library_recent_tab_rect, g.recent_tab);
        }
        // Case `fresh_player_shows_a_clean_empty_state_with_no_frame`.
        {
            let mut app = PlayerApp::new(RunMode::Capture);
            let mut doc = automerge::AutoCommit::new();
            // Pumping the app before any file is opened must not fabricate a frame.
            app.sync(&mut doc, &automerge::ROOT);
            app.sync(&mut doc, &automerge::ROOT);
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
        let mut app = PlayerApp::new(RunMode::Capture);
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
            let mut doc = automerge::AutoCommit::new();
            app.sync(&mut doc, &automerge::ROOT);
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

            let mut doc = automerge::AutoCommit::new();
            app.sync(&mut doc, &automerge::ROOT);
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
            let mut doc = automerge::AutoCommit::new();
            app.sync(&mut doc, &automerge::ROOT); // baseline the synth clock
            std::thread::sleep(std::time::Duration::from_millis(40));
            app.sync(&mut doc, &automerge::ROOT);
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
            app.sync(&mut doc, &automerge::ROOT);
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
            app.perform(VideoEffect::SetPlaybackRate { rate: 2.0 })
                .unwrap();
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

    fn transport(playing: bool, position_ms: u64, epoch_ms: u64) -> media::TransportState {
        media::TransportState {
            playing,
            position_ms,
            rate: 1.0,
            epoch_ms,
        }
    }

    fn shared(state: media::TransportState) -> crate::shared::SharedTransport {
        crate::shared::SharedTransport {
            media_ref: "clip".into(),
            playing: state.playing,
            position_ms: state.position_ms,
            rate: f64::from(state.rate),
            epoch_ms: state.epoch_ms,
        }
    }

    #[test]
    fn watching_together_follows_moves_and_says_its_own_once() {
        let start = 1_000_000;
        // A player that starts playing says so; its own change coming back
        // is not followed.
        let mut here = Together::default();
        assert_eq!(
            here.step("clip", None, transport(false, 0, start), start, 60_000),
            TogetherStep::Stay,
            "a paused player that nobody else watches says nothing"
        );
        let playing = transport(true, 0, start + 10);
        assert_eq!(
            here.step("clip", None, playing, start + 10, 60_000),
            TogetherStep::Publish(playing)
        );
        assert_eq!(
            here.step("clip", Some(&shared(playing)), playing, start + 20, 60_000),
            TogetherStep::Stay,
            "its own change is not followed back"
        );
        // Its engine still starting: the playhead has not moved yet. A
        // leader never seeks itself toward its own announcement.
        assert_eq!(
            here.step(
                "clip",
                Some(&shared(playing)),
                transport(true, 0, start + 900),
                start + 900,
                60_000
            ),
            TogetherStep::Stay,
            "a leader does not ride along with itself"
        );

        // Another player joins the same video and follows; while its engine
        // catches up it says nothing of its own.
        let mut there = Together::default();
        let local = transport(false, 0, start + 500);
        assert_eq!(
            there.step("clip", Some(&shared(playing)), local, start + 500, 60_000),
            TogetherStep::Follow(playing)
        );
        assert_eq!(
            there.step("clip", Some(&shared(playing)), local, start + 900, 60_000),
            TogetherStep::Follow(playing),
            "settling: a lagging engine is not a move"
        );
        // Small drift rides; a seek far away is a move everyone follows.
        let riding = transport(true, 2_100, start + 2_000);
        assert_eq!(
            there.step(
                "clip",
                Some(&shared(playing)),
                riding,
                start + 2_000,
                60_000
            ),
            TogetherStep::Follow(playing)
        );
        let sought = transport(true, 30_000, start + 2_100);
        assert_eq!(
            there.step(
                "clip",
                Some(&shared(playing)),
                sought,
                start + 2_100,
                60_000
            ),
            TogetherStep::Publish(sought)
        );
        // Pausing is a move too.
        assert_eq!(
            here.step(
                "clip",
                Some(&shared(sought)),
                playing,
                start + 2_200,
                60_000
            ),
            TogetherStep::Follow(sought),
            "the first player follows the seek"
        );
    }

    #[test]
    fn a_video_watched_long_ago_starts_on_its_own() {
        let now = 50_000_000;
        let long_ago = shared(transport(false, 12_000, now - TOGETHER_PAUSE_LIVES_MS - 1));
        let mut here = Together::default();
        assert_eq!(
            here.step(
                "clip",
                Some(&long_ago),
                transport(false, 0, now),
                now,
                60_000
            ),
            TogetherStep::Stay,
            "an old pause is not joined"
        );
        let finished = shared(transport(true, 0, now - 120_000));
        let mut there = Together::default();
        assert_eq!(
            there.step(
                "clip",
                Some(&finished),
                transport(false, 0, now),
                now,
                60_000
            ),
            TogetherStep::Stay,
            "a playback that ran past the end is over"
        );
    }

    #[test]
    fn revealing_the_controls_never_changes_transport() {
        let mut app = PlayerApp::for_shot(None);
        app.player.pause();
        let position = app.player.position_ms();
        app.perform(VideoEffect::Reveal).unwrap();
        assert!(!app.player.is_playing());
        assert_eq!(app.player.position_ms(), position);
    }
}
