//! The editor's root widget: media bin, preview, transport, timeline and
//! inspector, all drawn into one Vello scene with hand-rolled hit-testing.
//!
//! Why one big custom widget instead of composed toolkit widgets? A
//! timeline is fundamentally a canvas: clips are positioned proportional
//! to time, trim handles live on sub-pixel edges, the playhead crosses
//! every lane. That density is far cleaner to express as direct drawing +
//! geometric hit-tests than as a Taffy tree. The structural state still
//! lives in the CRDT (see [`crate::doc`]) so the edit is collaborative and
//! undoable; only ephemeral *view* state (playhead, zoom, selection) is
//! held in widget fields, matching the toolkit's per-peer-view convention.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::Instant;

use automerge::{AutoCommit, ObjId};
use taffy::{Dimension, NodeId, Size, Style, TaffyTree};
use uuid::Uuid;
use vello::Scene;
use vello::kurbo::{Affine, BezPath, Circle, Line, Point, Rect, RoundedRect, Stroke};
use vello::peniko::{Color, Fill};
use winit::event::{ElementState, MouseButton, MouseScrollDelta, WindowEvent};
use winit::keyboard::{Key, NamedKey};

use ui::ColorCompat;
use ui::a11y::{AccessNode, AccessState, Role};
use ui::permissions::PermissionContext;
use ui::presence::{PresenceCtx, PresenceKey};
use ui::text_render::{TextRenderer, TextWeight};
use ui::theme::Theme;
use ui::widget::{
    ContextMenu, EditOutcome, EditableLabel, FlapState, MenuItem, SplitLayout, Widget,
};

use crate::actions::{
    MarkerTextField, VIDEO_EDITOR_ACTION_TYPE, VideoEditorAction, VideoResourceCtx,
    clip_field_patch_action, delete_clip_action, remove_media_action, ripple_delete_action,
    set_project_meta_action, split_clip_action, upsert_clip_action, upsert_media_action,
    upsert_track_action,
};
use crate::doc::VideoDoc;
use crate::export::{Container, ExportSettings, ExportStatus, FrameRate, Resolution};
use crate::model::{
    Clip, ClipId, EffectKind, Filters, MAX_GAIN_DB, MAX_SPEED, MIN_GAIN_DB, MIN_SPEED, MediaId,
    MediaItem, MediaKind, Project, TextAlign, TextSource, TrackId, TrackKind, Transition,
};
use crate::param::{Interp, Param};
use crate::portal::{self, ImportChannel, ImportOutcome};
#[cfg(any(feature = "decode", test))]
use crate::preview;
#[cfg(test)]
use crate::preview::CompositedFrame;
use crate::probe;
use crate::timecode::{clock, frame_step, snap_to_frame, timecode};

mod layout;
mod paint;
mod scenario;
mod selection;
use layout::Regions;
use paint::*;
use scenario::ShotLifecycle;
pub use scenario::ShotScenario;
use selection::Selection;

// ---- layout constants ----------------------------------------------------
const TOPBAR_H: f64 = 48.0;
/// Width of the export progress / outcome chip under the topbar.
const EXPORT_CHIP_W: f64 = 176.0;
const TIMELINE_H: f64 = 304.0;
const BIN_W: f64 = 268.0;
const INSPECTOR_W: f64 = 300.0;
const GUTTER_W: f64 = 128.0;
const RULER_H: f64 = 30.0;
const TRANSPORT_H: f64 = 58.0;
const LANE_GAP: f64 = 4.0;
const VIDEO_LANE_H: f64 = 62.0;
const AUDIO_LANE_H: f64 = 46.0;
const TRIM_HIT: f64 = 8.0;
const PAD: f64 = 14.0;
/// Overlay-scrollbar geometry, matching [`ui::widget::DocumentViewport`] so
/// the timeline's bar is the same object the rest of the fleet shows.
const SCROLLBAR_THICKNESS: f64 = 8.0;
const SCROLLBAR_INSET: f64 = 3.0;
/// Below this (logical px, either axis) the letterboxed preview canvas has no
/// room to show a frame and the pane says so instead.
const MIN_CANVAS: f64 = 48.0;
/// Below this the transport has no room for a scrubber and draws none.
/// Where the history buttons sit, as an offset into the transport strip:
/// just past the five transport circles, which end at +205.
const HISTORY_X: f64 = 214.0;
const HISTORY_W: f64 = 30.0;
/// Where everything right of the history buttons begins.
const TRANSPORT_RIGHT_X: f64 = HISTORY_X + HISTORY_W * 2.0 + 14.0;
/// And where it begins when the strip is too narrow to hold them.
const TRANSPORT_PLAIN_X: f64 = 220.0;
/// The narrowest transport strip that can carry the history buttons *and*
/// still leave the playhead timecode room to be drawn whole beside them.
///
/// Below this the buttons go rather than the time: the time is the reading
/// the user cannot get anywhere else on the strip, and undo has a keyboard
/// shortcut that a narrow window does not take away.
const HISTORY_MIN_STRIP_W: f64 = TRANSPORT_RIGHT_X + 100.0;

const MIN_SCRUBBER_W: f64 = 90.0;
/// Above this a longer bar buys no precision and just looks stretched.
const MAX_SCRUBBER_W: f64 = 560.0;
/// How dark the overlay-bin scrim gets at full reveal. The editor behind it
/// must stay readable — the scrim says "the bin has the input now", it is not
/// a curtain. Deliberately a number here rather than the alpha of
/// `surfaces.scrim`: that token is opaque in every theme.
const SCRIM_ALPHA: f64 = 0.55;
/// How dark the export modal's backdrop gets. Heavier than the bin flap's
/// veil — a modal really does take the whole app — but still a veil: the user
/// keeps their place in the timeline behind it.
const MODAL_SCRIM_ALPHA: f64 = 0.72;
/// Vertical space the export card's header (title + filename) needs before
/// the first section label can start.
const EXPORT_HEADER_H: f64 = 76.0;
/// Space the card's footer needs: size estimate, status line, buttons.
const EXPORT_FOOTER_H: f64 = 104.0;
/// A section label's own height, above its row of segments.
const EXPORT_LABEL_H: f64 = 18.0;
/// The least a label-plus-segments group can be squeezed to.
const EXPORT_GROUP_H: f64 = 50.0;

/// What the editor says, in plain words, when a source's bytes are not on this
/// device — the reference replicated but the content has not been hydrated
/// from the FUSE (or the file is gone). One string so the bin row, the clip
/// and the preview never disagree about what they are disclosing.
const MEDIA_NOT_LOCAL: &str = "Not on this device";
/// The same claim for a pane too narrow to hold the full phrase (the media
/// bin is a fixed 268px column). Shorter, never vaguer.
const MEDIA_NOT_LOCAL_SHORT: &str = "Not on device";

/// What the editor says when a source's bytes ARE here but are not media — a
/// web page saved with a `.mp4` extension, a truncated download, a format
/// nothing on this box can decode. Distinct from [`MEDIA_NOT_LOCAL`]: fetching
/// it again will not help.
const MEDIA_UNREADABLE: &str = "Unreadable media";
/// The compact form, for the media bin's fixed-width column.
const MEDIA_UNREADABLE_SHORT: &str = "Unreadable";

/// Below this allocated width the media bin stops being a docked pane and
/// collapses into an overlay flap summoned by the topbar hamburger. Above it
/// the bin is docked beside the editor (the wide desktop layout).
/// Height of a collapsible inspector section header.
const SECTION_H: f64 = 24.0;
/// The inset a picture-in-picture preset places a clip at: a third of the
/// frame, a small margin off the corner. Big enough to see, small enough to
/// leave the frame underneath readable.
const PIP_SCALE: f64 = 0.32;
const PIP_INSET: f64 = 0.04;
/// Width of the key control at the head of an animatable inspector row.
const KEY_BOX: f64 = 20.0;

/// The keyboard cursor's ring, in the width every shared widget in `ui` draws
/// focus at (`Button`, `DataTable`, `IconGrid`, `ToggleGroup` all stroke 2px in
/// `Theme::border_focus`). The colour is always read from the theme — in the
/// high-contrast theme it is a yellow nothing else uses, which is the whole
/// point of a focus colour being a token rather than a literal.
const FOCUS_RING_W: f64 = 2.0;
/// How far inside a control's own outline the ring is drawn where the two would
/// otherwise land on the same edge.
///
/// A palette declares one accent and its focus colour normally follows it, so
/// `border_focus` and `accent[5]` are the same colour in most themes. The bin's
/// selected row already carries an accent outline on its edge; a ring laid on
/// that edge would disappear into it, so the cursor's ring sits inside and the
/// two read as concentric. Around the transport's round buttons the offset goes
/// the other way — see [`draw_focus_ring_circle`].
const FOCUS_RING_INSET: f64 = 3.0;

const BIN_BREAKPOINT: f64 = 900.0;

/// A top-bar command needs a target a finger can reliably acquire. The old
/// 28px icon targets were especially easy to miss on a narrow timeline, where
/// the collapsed-bin and collapsed-inspector controls are the only way back to
/// those panes.
const TOPBAR_TOUCH_TARGET: f64 = 36.0;
const TOPBAR_EDGE: f64 = 14.0;
const TOPBAR_GAP: f64 = 10.0;
/// At this width the Split button is redundant with its stable `S` shortcut
/// and prevents the two independent side-panel toggles from fitting.
const TOPBAR_COMPACT_BREAKPOINT: f64 = 480.0;

/// Stable accessibility routes for the controls painted in the top bar.
/// These are deliberately independent from their rectangles: a compact layout
/// can hide Split without letting a stale assistive-technology route operate a
/// control that is no longer on screen.
const TOPBAR_A11Y_KEY: &str = "topbar";
const TOPBAR_BIN_KEY: &str = "topbar.bin";
const TOPBAR_INSPECTOR_KEY: &str = "topbar.inspector";
const TOPBAR_IMPORT_KEY: &str = "topbar.import";
const TOPBAR_SPLIT_KEY: &str = "topbar.split";
const TOPBAR_EXPORT_KEY: &str = "topbar.export";

/// Width the project button takes when the topbar has room for it, and the
/// least it will accept before being dropped instead. The floor exists
/// because a name clipped to a few characters states a *different* project
/// name, and the button's whole job is saying which project this is.
const PROJECT_BTN_W: f64 = 210.0;
const PROJECT_BTN_MIN_W: f64 = 120.0;

/// What the project button's dot means, spelled out wherever the dot cannot
/// be seen. Not a warning — the autosave still holds the work; what the user
/// does not have is a file of their own.
const PROJECT_UNSAVED: &str = "Unsaved changes";

/// The preview's and the inspector's empty states, in one place each so the
/// painted words and the spoken ones cannot drift apart. They did: the
/// inspector said "No clip selected / Click a clip in the timeline" on screen
/// and "Nothing selected. Select a clip on the timeline to edit it." to a
/// reader, which is two descriptions of one state.
const PREVIEW_EMPTY_TITLE: &str = "No clip at playhead";
const PREVIEW_EMPTY_HINT: &str = "Move the playhead over a clip to see it";
/// The marker list's add button, shared by the paint and the accessibility
/// tree so a reader is offered the control the screen actually shows.
const ADD_MARKER_LABEL: &str = "+ Marker at playhead (M)";
/// The key the add button is addressed by, before hashing. A constant rather
/// than a literal at each site so the node and the router cannot drift apart.
const ADD_MARKER_KEY: &str = "add marker";

/// The three controls a marker row carries, in painted order (see
/// [`marker_row_parts`]). They are painted only under the pointer but are
/// hit-tested whether painted or not, so for a reader -- which has no pointer
/// -- they are the only route to a marker's note, its name, or its removal.
const MARKER_VERBS: [&str; 3] = ["Edit note on", "Rename", "Delete"];

/// The key the master mix node is addressed by, before hashing. A constant
/// rather than a literal at each site so the node and the router cannot drift
/// apart.
const MIX_METER_KEY: &str = "mix meter";

const INSPECTOR_EMPTY_TITLE: &str = "No clip selected";
const INSPECTOR_EMPTY_HINT: &str = "Click a clip in the timeline to edit it";

/// Height of the inspector's own "INSPECTOR" heading row, which the empty
/// state starts below.
const INSPECTOR_HEADER_H: f64 = 26.0;

/// Below this window width the inspector collapses to an overlay flap from
/// the right, exactly as the bin does from the left.
///
/// A fixed 300px panel is a third of a 900px window and nearly two thirds of
/// a 500px one — at which point the preview, the transport buttons and the
/// timecode are all squeezed out by a panel that edits a clip the user can no
/// longer see. Below the breakpoint the inspector gets out of the way and is
/// summoned by the topbar toggle.
///
/// Unlike the bin, an open inspector paints **no scrim**: its whole purpose
/// is to change the picture, so dimming the picture while you drag a slider
/// would hide the thing you are adjusting.
const INSPECTOR_BREAKPOINT: f64 = 760.0;

struct BinPaintCtx<'a> {
    scene: &'a mut Scene,
    transform: Affine,
    text: &'a mut TextRenderer,
    theme: &'a Theme,
    scale: f64,
}

/// How a standing notice reads: an ordinary outcome, a partial one, or a
/// failure.
///
/// The severity travels with the message from the place that knows the truth.
/// It used to be re-derived downstream by looking for `"could not"` /
/// `"couldn't"` / `"is not"` in the prose, which had two consequences: a
/// portal refusal (`"the file picker needs the real-desktop build"`) matched
/// none of them and so painted in the *success* tint, and the colour of a
/// message depended on the language it happened to be written in — which the
/// fleet's translated strings cannot honour.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum NoticeLevel {
    /// It happened, and there is nothing left over.
    Ok,
    /// It happened, but not wholly — neither a success nor a failure, and it
    /// must not be dressed as either.
    Warning,
    /// It did not happen.
    Failed,
}

impl NoticeLevel {
    /// The tint this level paints in, read from the theme like every other
    /// colour in the app.
    fn tint(self, theme: &Theme) -> Color {
        let c = &theme.tokens.colors;
        match self {
            Self::Ok => c.accent[5],
            Self::Warning => c.semantic.warning,
            Self::Failed => c.semantic.danger,
        }
    }
}

/// How many refused files a batch names before it starts counting the rest.
/// The bin's notice strip is one line; naming every refusal in a fifty-file
/// batch would elide the reasons away again.
const IMPORT_REFUSALS_NAMED: usize = 2;

/// What one batch import actually did: how many files became clips, and — for
/// every file that did not — its name and the probe's own reason.
#[derive(Clone, Default, PartialEq, Eq, Debug)]
struct ImportReport {
    added: usize,
    refused: Vec<(String, String)>,
}

impl ImportReport {
    fn total(&self) -> usize {
        self.added + self.refused.len()
    }
}

/// The export chip's painted text, cached against everything that produced it.
///
/// The chip is repainted on every animation frame for as long as a render
/// runs, and its words only move when the whole-percent figure does. Without
/// this, each of those frames reformatted a string and re-measured an
/// elision that was already known.
struct ExportChipText {
    /// The render status the words were built from.
    from: ExportStatus,
    /// The chip width the elision was measured against.
    width: f64,
    /// The severity the chip paints at.
    level: NoticeLevel,
    /// The string as painted, already elided to the chip.
    shown: String,
}

/// Whether two render statuses would paint the same chip.
///
/// Progress is shown to the whole percent, so two encoded frames inside one
/// percent are the same picture and must not cost a reformat.
fn chip_text_matches(a: &ExportStatus, b: &ExportStatus) -> bool {
    match (a, b) {
        (ExportStatus::Rendering { progress: x }, ExportStatus::Rendering { progress: y }) => {
            chip_percent(*x) == chip_percent(*y)
        }
        _ => a == b,
    }
}

/// The whole-percent figure the chip shows for `progress`.
fn chip_percent(progress: f64) -> i64 {
    (progress.clamp(0.0, 1.0) * 100.0).round() as i64
}

/// One line of standing feedback, carrying the severity it was created with.
#[derive(Clone, PartialEq, Eq, Debug)]
struct Notice {
    level: NoticeLevel,
    text: String,
}

impl Notice {
    fn ok(text: impl Into<String>) -> Self {
        Self {
            level: NoticeLevel::Ok,
            text: text.into(),
        }
    }

    fn warning(text: impl Into<String>) -> Self {
        Self {
            level: NoticeLevel::Warning,
            text: text.into(),
        }
    }

    fn failed(text: impl Into<String>) -> Self {
        Self {
            level: NoticeLevel::Failed,
            text: text.into(),
        }
    }

    fn is_failure(&self) -> bool {
        self.level == NoticeLevel::Failed
    }
}

#[derive(Clone, Copy, PartialEq)]
enum TransportAction {
    ToStart,
    StepBack,
    PlayPause,
    StepFwd,
    ToEnd,
}

/// One of the editor's four keyboard/reader panes.
///
/// The editor paints itself, so it has no child widgets for the host's focus
/// walk to visit: without a pane cursor of its own the whole application is a
/// single Tab stop, and a keyboard-only user can scrub the playhead and do
/// nothing else. Tab moves between panes, arrows move within the focused one,
/// and the element the cursor is on is exported as the accessibility tree's
/// active descendant, so a screen reader lands on the clip rather than on "the
/// video editor".
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
enum Pane {
    /// The media bin's rows.
    Bin,
    /// The five transport buttons.
    Transport,
    /// Tracks and the clips on them.
    #[default]
    Timeline,
    /// The inspector's section headers and controls.
    Inspector,
}

impl Pane {
    /// Tab order, left to right and top to bottom as the panes are painted.
    const ORDER: [Pane; 4] = [Pane::Bin, Pane::Transport, Pane::Timeline, Pane::Inspector];

    fn label(self) -> &'static str {
        match self {
            Pane::Bin => "Media bin",
            Pane::Transport => "Transport",
            Pane::Timeline => "Timeline",
            Pane::Inspector => "Inspector",
        }
    }

    /// The key an accessibility node uses for this pane's group. Stable across
    /// repaints because it is derived from the pane, not from its position.
    fn key(self) -> u64 {
        ui::a11y::hash_key(&self.label())
    }

    fn step(self, back: bool) -> Pane {
        let i = Self::ORDER.iter().position(|p| *p == self).unwrap_or(0);
        let n = Self::ORDER.len();
        Self::ORDER
            .get(if back { (i + n - 1) % n } else { (i + 1) % n })
            .copied()
            .unwrap_or(self)
    }
}

/// Where the keyboard is inside each pane.
///
/// One cursor per pane rather than one global index, so Tab away and back
/// returns you where you were — the behaviour every pane-based editor has and
/// the only one that makes Tab usable for navigation rather than a reset.
/// The timeline's cursor is the clip selection itself (there is no second
/// notion of "the current clip"), so it is not stored here.
#[derive(Clone, Copy, Debug, Default)]
struct PaneCursor {
    pane: Pane,
    bin: usize,
    transport: usize,
    inspector: usize,
}

/// A collapsible block of inspector controls.
///
/// Sections exist because the inspector grew past what a 300px column shows
/// at once. Which are folded is **presentation** — it is a property of this
/// reader's panel, not of the clip — so it lives in a widget field and never
/// in the CRDT, and it persists across selections: a user who folds Transform
/// away is telling the panel what they are working on, not this clip.
///
/// A folded header still states what is inside it (see `section_summary`), so
/// folding hides controls but never hides state.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
enum Section {
    Curves,
    Transform,
    Speed,
    /// The **lane** the selected clip sits on: its fader and its pan.
    ///
    /// Track-level rather than clip-level, and placed directly after Speed
    /// because Speed is the other control that changes what a clip *sounds*
    /// like (a varispeed pitches it), so the two read as one thought: how
    /// fast it plays, then how loud it sits and where. Everything below is
    /// picture detail, which is why the mixer is above it rather than at the
    /// bottom of a panel nobody scrolls to.
    Mix,
    Key,
    Mask,
    Title,
    Effects,
    Markers,
    /// The project's own format: frame size and rate. Project-level like
    /// `Markers`, so it is listed whether or not a clip is selected, and last
    /// for the same reason — the panel reads clip-first, project-after.
    Project,
}

impl Section {
    fn label(self) -> &'static str {
        match self {
            Section::Curves => "CURVES",
            Section::Transform => "TRANSFORM",
            Section::Speed => "SPEED",
            Section::Mix => "MIX",
            Section::Key => "CHROMA KEY",
            Section::Mask => "MASK",
            Section::Title => "TITLE",
            Section::Effects => "EFFECTS",
            Section::Markers => "MARKERS",
            Section::Project => "PROJECT",
        }
    }
}

/// A numeric inspector control that is not one of the original `0..1`
/// sliders: it has its own range, its own readout, and in two cases it edits
/// the *media item* rather than the clip.
///
/// One enum rather than a `Drag` variant each keeps the press / drag / commit
/// arms from growing a near-identical copy per knob.
#[derive(Clone, Copy, PartialEq, Debug)]
enum NumField {
    /// The clip's level and grade, and its two fades. These are the panel's
    /// **first** controls — drawn above its first section header — and were
    /// the ones no keyboard or assistive technology could reach: the row list
    /// started at the first header, so the walk began below them and could
    /// never climb back up.
    Opacity,
    Volume,
    Brightness,
    Contrast,
    Saturation,
    FadeIn,
    FadeOut,
    /// How long the incoming transition ramps for.
    TransitionLen,
    ScaleX,
    ScaleY,
    PosX,
    PosY,
    Rotation,
    CropLeft,
    CropRight,
    CropTop,
    CropBottom,
    KeyTolerance,
    KeySoftness,
    KeySpill,
    MaskX,
    MaskY,
    MaskW,
    MaskH,
    MaskRotation,
    MaskFeather,
    Speed,
    TitleSize,
    TitleBackdrop,
    /// How much of the clip's tone curve is mixed in.
    CurveMix,
    /// The **track's** fader, in decibels, and where the lane sits across the
    /// stereo field. The only two knobs in the panel that edit the lane the
    /// selected clip is on rather than the clip itself — see
    /// [`NumField::is_track`], which is what routes the write and the commit.
    TrackGain,
    TrackPan,
}

impl NumField {
    fn label(self) -> &'static str {
        match self {
            NumField::Opacity => "Opacity",
            NumField::Volume => "Volume",
            NumField::Brightness => "Brightness",
            NumField::Contrast => "Contrast",
            NumField::Saturation => "Saturation",
            NumField::FadeIn => "Fade in",
            NumField::FadeOut => "Fade out",
            NumField::TransitionLen => "Transition length",
            NumField::ScaleX => "Scale X",
            NumField::ScaleY => "Scale Y",
            NumField::PosX => "Position X",
            NumField::PosY => "Position Y",
            NumField::Rotation => "Rotation",
            NumField::CropLeft => "Left",
            NumField::CropRight => "Right",
            NumField::CropTop => "Top",
            NumField::CropBottom => "Bottom",
            NumField::KeyTolerance => "Tolerance",
            NumField::KeySoftness => "Softness",
            NumField::KeySpill => "Spill removal",
            NumField::MaskX => "Centre X",
            NumField::MaskY => "Centre Y",
            NumField::MaskW => "Width",
            NumField::MaskH => "Height",
            NumField::MaskRotation => "Rotation",
            NumField::MaskFeather => "Feather",
            NumField::Speed => "Rate",
            NumField::TitleSize => "Size",
            NumField::TitleBackdrop => "Backdrop",
            NumField::CurveMix => "Amount",
            NumField::TrackGain => "Gain",
            NumField::TrackPan => "Pan",
        }
    }

    /// A stable tag naming which control this is, which is what the accessible
    /// key hashes.
    ///
    /// It cannot be the visible label. `Rotation` and `MaskRotation` both read
    /// "Rotation" — correctly, each inside its own section — and the key was
    /// `hash_key(&label())`, so the two hashed identically: an action routed at
    /// the mask's tilt landed on whichever of the two rows the walk reached
    /// first, which is the transform's. The tag is derived from the variant
    /// instead, so two rows may share a word without sharing an identity, and a
    /// key stays stable across a re-layout and a relabelling.
    ///
    /// Dotted and prefixed like [`PickField::tag`], for the same reason: the
    /// two live in one namespace once they are hashed.
    fn tag(self) -> &'static str {
        match self {
            NumField::Opacity => "clip.opacity",
            NumField::Volume => "clip.volume",
            NumField::Brightness => "clip.brightness",
            NumField::Contrast => "clip.contrast",
            NumField::Saturation => "clip.saturation",
            NumField::FadeIn => "clip.fade_in",
            NumField::FadeOut => "clip.fade_out",
            NumField::TransitionLen => "clip.transition_len",
            NumField::ScaleX => "clip.scale_x",
            NumField::ScaleY => "clip.scale_y",
            NumField::PosX => "clip.pos_x",
            NumField::PosY => "clip.pos_y",
            NumField::Rotation => "clip.rotation",
            NumField::CropLeft => "clip.crop_left",
            NumField::CropRight => "clip.crop_right",
            NumField::CropTop => "clip.crop_top",
            NumField::CropBottom => "clip.crop_bottom",
            NumField::KeyTolerance => "clip.key.tolerance",
            NumField::KeySoftness => "clip.key.softness",
            NumField::KeySpill => "clip.key.spill",
            NumField::MaskX => "clip.mask.x",
            NumField::MaskY => "clip.mask.y",
            NumField::MaskW => "clip.mask.w",
            NumField::MaskH => "clip.mask.h",
            NumField::MaskRotation => "clip.mask.rotation",
            NumField::MaskFeather => "clip.mask.feather",
            NumField::Speed => "clip.speed",
            NumField::TitleSize => "media.title.size",
            NumField::TitleBackdrop => "media.title.backdrop",
            NumField::CurveMix => "clip.curves.mix",
            // `track.` rather than `clip.`, because that is what they write.
            NumField::TrackGain => "track.gain",
            NumField::TrackPan => "track.pan",
        }
    }

    fn a11y_key(self) -> u64 {
        ui::a11y::hash_key(&self.tag())
    }

    /// The animatable parameter this control drives, if any. Crop and anchor
    /// are deliberately not animatable in the model — they frame the shot
    /// rather than move it — and neither title knob is a clip parameter at
    /// all, so those have no key control.
    fn param(self) -> Option<Param> {
        Some(match self {
            NumField::Opacity => Param::Opacity,
            NumField::Volume => Param::Volume,
            NumField::Brightness => Param::Brightness,
            NumField::Contrast => Param::Contrast,
            NumField::Saturation => Param::Saturation,
            NumField::ScaleX => Param::ScaleX,
            NumField::ScaleY => Param::ScaleY,
            NumField::PosX => Param::PosX,
            NumField::PosY => Param::PosY,
            NumField::Rotation => Param::Rotation,
            NumField::KeyTolerance => Param::KeyTolerance,
            NumField::KeySoftness => Param::KeySoftness,
            NumField::KeySpill => Param::KeySpill,
            NumField::MaskX => Param::MaskX,
            NumField::MaskY => Param::MaskY,
            NumField::MaskW => Param::MaskW,
            NumField::MaskH => Param::MaskH,
            NumField::MaskRotation => Param::MaskRotation,
            NumField::MaskFeather => Param::MaskFeather,
            NumField::CurveMix => Param::CurveMix,
            _ => return None,
        })
    }

    /// Whether this knob writes the selected clip's **media item** instead of
    /// the clip — the two title properties do, and they commit a different
    /// action because of it.
    fn is_media(self) -> bool {
        matches!(self, NumField::TitleSize | NumField::TitleBackdrop)
    }

    /// Whether this knob writes the **track** the selected clip sits on.
    ///
    /// A third destination alongside the clip and its media, and it has to be
    /// named rather than inferred: the write, the undo boundary and the
    /// action a drag commits are all per-track, and a lane fader dispatched
    /// as a clip write would replicate as an edit to the wrong thing.
    fn is_track(self) -> bool {
        matches!(self, NumField::TrackGain | NumField::TrackPan)
    }

    /// The slider's linear span. Cropping stops short of the far edge so a
    /// clip can never be dragged out of existence, and scale stops short of
    /// zero for the same reason.
    fn range(self) -> (f64, f64) {
        match self {
            NumField::Opacity
            | NumField::Volume
            | NumField::Brightness
            | NumField::Contrast
            | NumField::Saturation => (0.0, 1.0),
            // A fade and a transition ramp are seconds measured against the
            // clip they sit on, so their real span is the one
            // `VideoEditorApp::field_span` reads off that clip. This pair is
            // the widest any clip can make them, and is what a caller asking
            // for a fraction without a clip in hand gets.
            NumField::FadeIn | NumField::FadeOut | NumField::TransitionLen => (0.0, 3600.0),
            NumField::ScaleX | NumField::ScaleY => (0.05, 4.0),
            NumField::PosX | NumField::PosY => (-1.0, 1.0),
            NumField::Rotation => (-180.0, 180.0),
            NumField::CropLeft | NumField::CropRight | NumField::CropTop | NumField::CropBottom => {
                (0.0, 0.9)
            }
            // Tolerance is measured in normalised chroma, where a clean
            // plate keys somewhere under 0.3 and the far stop takes out
            // everything sharing the key's half of the colour wheel. The
            // near stop keys nothing, which is a legible state, not a dead
            // end — it is what a key ramped on from zero starts at.
            NumField::KeyTolerance | NumField::KeySoftness => (0.0, 1.0),
            NumField::KeySpill => (0.0, 1.0),
            NumField::MaskX | NumField::MaskY => (-1.0, 1.0),
            // A mask can reach past the frame, which is how an edge is put
            // outside the picture instead of cutting across it.
            NumField::MaskW | NumField::MaskH => (0.0, 2.0),
            NumField::MaskRotation => (-180.0, 180.0),
            NumField::MaskFeather => (0.0, 1.0),
            NumField::Speed => (MIN_SPEED, MAX_SPEED),
            NumField::TitleSize => (0.02, 0.4),
            NumField::TitleBackdrop => (0.0, 1.0),
            NumField::CurveMix => (0.0, 1.0),
            // Decibels, not a fraction. The track runs from off to four
            // times unity; 0 dB — where a lane starts and where most of them
            // stay — sits five sixths of the way along, which is where a
            // fader's unity mark belongs.
            NumField::TrackGain => (MIN_GAIN_DB, MAX_GAIN_DB),
            NumField::TrackPan => (-1.0, 1.0),
        }
    }

    /// Rate is the one knob whose span is multiplicative: 0.05x to 20x is
    /// eight octaves, and on a linear track normal speed would sit at 5% of
    /// the way along with every useful slow-motion rate crushed against the
    /// left stop. On a log track 1x lands dead centre and half speed and
    /// double speed are the same distance from it, which is how a rate reads.
    fn logarithmic(self) -> bool {
        matches!(self, NumField::Speed)
    }

    fn to_frac(self, v: f64) -> f64 {
        let (lo, hi) = self.range();
        if self.logarithmic() {
            let v = v.clamp(lo, hi).max(1e-6);
            return ((v.ln() - lo.ln()) / (hi.ln() - lo.ln())).clamp(0.0, 1.0);
        }
        ((v - lo) / (hi - lo)).clamp(0.0, 1.0)
    }

    fn from_frac(self, f: f64) -> f64 {
        let (lo, hi) = self.range();
        let f = f.clamp(0.0, 1.0);
        if self.logarithmic() {
            return (lo.ln() + (hi.ln() - lo.ln()) * f).exp();
        }
        lo + (hi - lo) * f
    }

    fn readout(self, v: f64) -> String {
        match self {
            NumField::Opacity
            | NumField::Volume
            | NumField::Brightness
            | NumField::Contrast
            | NumField::Saturation => format!("{:.0}%", v * 100.0),
            NumField::FadeIn | NumField::FadeOut | NumField::TransitionLen => {
                format!("{v:.2}s")
            }
            NumField::ScaleX | NumField::ScaleY => format!("{v:.2}×"),
            NumField::PosX | NumField::PosY => format!("{v:+.2}"),
            NumField::Rotation => format!("{v:.0}°"),
            NumField::CropLeft | NumField::CropRight | NumField::CropTop | NumField::CropBottom => {
                format!("{:.0}%", v * 100.0)
            }
            NumField::KeyTolerance
            | NumField::KeySoftness
            | NumField::KeySpill
            | NumField::MaskFeather => format!("{:.0}%", v * 100.0),
            NumField::MaskX | NumField::MaskY => format!("{v:+.2}"),
            NumField::MaskW | NumField::MaskH => format!("{:.0}%", v * 100.0),
            NumField::MaskRotation => format!("{v:.0}°"),
            NumField::Speed => format!("{v:.2}×"),
            NumField::TitleSize => format!("{:.0}%", v * 100.0),
            NumField::TitleBackdrop => format!("{:.0}%", v * 100.0),
            NumField::CurveMix => format!("{:.0}%", v * 100.0),
            // The bottom stop is off, not "-60 dB of something".
            NumField::TrackGain if v <= MIN_GAIN_DB + 1e-9 => "-∞ dB".into(),
            NumField::TrackGain => format!("{v:+.1} dB"),
            NumField::TrackPan => {
                let n = (v.abs() * 100.0).round();
                if n < 0.5 {
                    "Centre".into()
                } else if v < 0.0 {
                    format!("L{n:.0}")
                } else {
                    format!("R{n:.0}")
                }
            }
        }
    }
}

/// Which marker an inspector row addresses. A clip marker and a project
/// marker are edited through different actions (an optimistic clip-field
/// patch versus a project-meta write), so the row has to carry which it is.
#[derive(Clone, Copy, PartialEq, Debug)]
struct MarkerRef {
    id: Uuid,
    /// `Some` for a marker pinned to a clip, `None` for a timeline one.
    clip: Option<ClipId>,
    /// Absolute timeline seconds, already rebased for a clip marker.
    at: f64,
}

#[derive(Clone, Copy, PartialEq)]
enum Drag {
    Playhead,
    Scrubber,
    /// Moving the selection. `id` is the clip under the pointer (the one whose
    /// left edge snaps); `grab_dt` is where inside it the grab landed; `track`
    /// is the lane it should land on, re-resolved from the cursor on every
    /// move so a vertical drag crosses tracks. Every other selected clip
    /// follows by the same time delta.
    ClipMove {
        id: ClipId,
        grab_dt: f64,
        track: TrackId,
    },
    TrimIn {
        id: ClipId,
    },
    TrimOut {
        id: ClipId,
    },
    /// Rubber-band selection from `origin` on empty lane space. `additive`
    /// keeps the previous selection (shift held). A press that never leaves
    /// [`MARQUEE_THRESHOLD`] falls back to scrubbing on release.
    Marquee {
        origin: Point,
        additive: bool,
    },
    /// Dragging a scrollbar thumb. The variant names which pane's bar, so
    /// one drag arm serves the timeline, the inspector and the bin.
    Scrollbar(ScrollPane),
    Opacity,
    Volume,
    Brightness,
    Contrast,
    Saturation,
    FadeIn,
    FadeOut,
    /// Dragging the incoming transition's length.
    TransitionLen,
    /// Dragging the intensity slider of the effect at `idx` on the selected clip.
    EffectAmount {
        idx: usize,
        /// The durable effect target prevents an old drag from applying to the
        /// instance that later happened to occupy this row.
        effect: Uuid,
        /// The amount the gesture started from, used as the action's optimistic
        /// precondition when it is committed.
        expected_amount_bits: u64,
    },
    /// Dragging control point `idx` of `channel`'s tone curve. Released
    /// outside the editor's box, an interior point is removed — the standard
    /// way to take a point off a curve, and the only one that does not need
    /// a second control per point on a 300px column.
    CurvePoint {
        channel: crate::curve::Channel,
        idx: usize,
    },
    /// Dragging one of the ranged controls — geometry, rate, title size. See
    /// [`NumField`], which carries the range, the readout and where the value
    /// is written.
    Num(NumField),
}

/// Pointer travel (logical px) before a press on empty lane space becomes a
/// rubber-band selection rather than a playhead scrub.
const MARQUEE_THRESHOLD: f64 = 4.0;

/// How long after a bin click a second click on the same item still counts as
/// a double-click ("add to timeline") rather than a rename intent.
const BIN_DOUBLE_CLICK_MS: u128 = 400;
/// The upper edge of the deliberate slow-second-click rename window. Past it
/// the previous click is stale and a press is just a fresh press — without
/// this bound a bin item, once clicked, could never be dragged again.
const BIN_RENAME_WINDOW_MS: u128 = 2_000;

/// A scrollable pane of the editor. Each owns an offset, a shared
/// [`ui::widget::Scrollbar`] interaction model and a track rect; naming them
/// lets one press / drag / wheel arm serve all three.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum ScrollPane {
    /// The timeline's horizontal axis (seconds).
    Timeline,
    /// The inspector body's vertical axis (px).
    Inspector,
    /// The media bin's vertical axis (px).
    Bin,
}

/// The configuration and initial selection policy of this editor instance.
#[derive(Default)]
struct SeedState {
    demo: bool,
    selection_initialised: bool,
}

/// Transport-owned presentation state, kept separate from durable project time.
#[derive(Default)]
struct PlaybackState {
    playing: bool,
    chase_playhead: bool,
}

/// Immediate UI overlays and host focus, all local to this participant.
#[derive(Default)]
struct PresentationState {
    export_open: bool,
    file_hovering: bool,
    keyboard_focus: bool,
}

/// Local navigation affordances which never belong in project collaboration.
#[derive(Default)]
struct NavigationState {
    help_open: bool,
    goto_notice: bool,
    flap_settled: bool,
}

/// Import and audio-monitor lifecycle, independent of the render pipeline.
#[derive(Default)]
struct MediaPipelineState {
    auto_place_imports: bool,
    #[cfg(any(feature = "gst", feature = "decode"))]
    monitor_tried: bool,
    #[cfg(any(feature = "gst", feature = "decode"))]
    monitor_stale: bool,
}

/// Preview-renderer lifecycle and deterministic capture warm-up policy.
#[derive(Default)]
struct RenderPipelineState {
    compositor_tried: bool,
    warm_waveforms_sync: bool,
}

/// Per-device project-file work deferred until the next `sync`.
#[derive(Default)]
struct ProjectSyncState {
    reload_pending: bool,
    save_requested: bool,
}

pub struct VideoEditorApp {
    project: Project,
    /// The local state each existing clip had immediately before the current
    /// UI edit began. Optimistic actions diff this baseline against the local
    /// result, then check only those old values against the live CRDT. They
    /// must never diff a stale local result against a newer remote projection:
    /// doing so would turn unrelated remote edits into reverse patches.
    clip_edit_bases: HashMap<ClipId, Clip>,
    /// Deterministic capture scenario applied once during the first `sync`.
    shot_scenario: ShotScenario,
    shot_lifecycle: ShotLifecycle,
    seed: SeedState,
    playback: PlaybackState,
    presentation: PresentationState,
    navigation: NavigationState,
    media_pipeline: MediaPipelineState,
    render_pipeline: RenderPipelineState,
    project_sync: ProjectSyncState,
    /// Current accessibility type scale. Side panes use the effective width
    /// so a large-text desktop promotes them to their existing overlay modes
    /// before they crush the preview or overlap inspector controls.
    text_scale: f64,
    // --- view state (per-peer, never in the CRDT) ---
    playhead: f64,
    zoom: f64,   // pixels per second
    scroll: f64, // seconds at the left edge of the lane area
    /// Shared scrollbar interaction models (thumb geometry, thumb drag, track
    /// paging) — the same ones [`ui::widget::Scrollable`] and
    /// [`ui::widget::DocumentViewport`] use, so every bar here behaves and
    /// looks identical to every other scroll surface in the fleet. One per
    /// scrollable axis: the timeline scrolls horizontally, the inspector and
    /// the media bin vertically.
    hbar: ui::widget::Scrollbar,
    inspector_bar: ui::widget::Scrollbar,
    bin_bar: ui::widget::Scrollbar,
    selection: Selection,
    selected_media: Option<MediaId>,
    /// Decides which one thing to tell the room about this participant on
    /// each poll of [`Widget::presence_payload`], which only carries one.
    /// See [`crate::presence`] for why there are two things to say.
    local_presence: crate::presence::LocalPresence,
    /// Other participants, staged for a `--shots` capture. `None` in the
    /// live app, where they arrive over the collaboration socket like
    /// anyone else; a capture cannot wait on a socket, so the scenario
    /// stages the very frames a peer would have sent and the draw reads
    /// them through the identical path.
    shot_presence: Option<PresenceCtx>,
    // --- ui state ---
    export: ExportSettings,
    export_status: ExportStatus,
    /// A media-import portal request in flight, drained each `sync`.
    import_rx: Option<ImportChannel>,
    /// A short, honest status line shown after an import attempt (how many
    /// clips landed, which files were refused and why, or why the portal
    /// round-trip failed). Never a fake success, and it carries its own
    /// severity rather than leaving the tint to be guessed from its prose.
    import_notice: Option<Notice>,
    /// A file queued to import on the next `sync` (deterministic `--shots` /
    /// tests seed a real clip here instead of opening the portal).
    pending_imports: Vec<PathBuf>,
    /// When set, a queued import is also dropped onto the timeline (creating a
    /// track if the blank project has none) and the playhead parked inside it —
    /// so a `--shots` capture / preload shows the clip in the preview, not just
    /// the bin. Off for a plain portal import (which lands in the bin only).
    /// The history the host owns, as much of it as a widget is allowed to
    /// see: whether undo and redo are available right now, and a channel to
    /// ask for one. The stacks stay with the host — this only carries the
    /// answer, so a control drawn from it can never offer an undo that
    /// Ctrl+Z would decline.
    undo: ui::UndoAccess,
    /// Real preview decoder. Only driven under `--features decode`, where it
    /// decodes the true frame of the clip under the playhead through `GStreamer`.
    #[cfg(feature = "decode")]
    preview_decoder: preview::PreviewDecoder,
    /// The preview's frame source, held for the app's lifetime.
    ///
    /// It owns the decode sessions and one YUV converter per layer slot, so
    /// rebuilding it per frame would re-open a pipeline every frame. It is
    /// re-indexed (not rebuilt) when the media bin changes.
    #[cfg(feature = "decode")]
    frame_source: Option<crate::gpu::MediaFrameSource>,
    /// Signature of the media the frame source was last indexed against.
    #[cfg(feature = "decode")]
    frame_source_sig: u64,
    /// Frames since the frame source last dropped its idle sessions.
    #[cfg(feature = "decode")]
    prune_countdown: u32,
    /// Audio monitoring: the mixed timeline, played while the transport
    /// runs. `None` until the first frame builds it, and it holds a
    /// [`audio::NullSink`] on a box with no output — silent playback rather
    /// than no playback, so `Clip::volume` and the track mutes still have
    /// something on the other end of them.
    #[cfg(any(feature = "gst", feature = "decode"))]
    monitor: Option<crate::audio::AudioMonitor>,
    /// The level the mix is running at, written by whatever is mixing and
    /// read here. **Not** feature-gated: a build with no audio backend still
    /// has a mixer, still has a meter surface, and a `--shots` capture stages
    /// blocks through this exact door rather than through a drawing that
    /// imitates one.
    mix_meters: std::sync::Arc<crate::audio::MixMeters>,
    /// One copy of that, taken in `sync`. `draw` reads plain fields: the
    /// paint path must not take the mixing thread's lock, and it must not
    /// allocate — the number is carried here, never recomputed there.
    mix: crate::audio::MixSnapshot,
    /// What the mix looked like when the meter last measured it. When this
    /// moves, the report describes a timeline that no longer exists and is
    /// thrown away — see [`crate::audio::mix_signature`].
    ///
    /// `None` until the first frame has one: a *first* signature is not a
    /// change, and treating it as one would throw away a report staged before
    /// the project finished loading.
    mix_sig: Option<u64>,
    /// The frame clock the meter's fall is driven from. Separate from
    /// `last_tick`, which the playhead owns: a meter has to keep falling
    /// while the transport is stopped.
    meter_tick: Instant,
    /// The document heads the model was last loaded at — how "the document
    /// changed" is told from "the same document was reloaded".
    doc_heads: Vec<automerge::ChangeHash>,
    /// The GPU compositor that produces the preview picture — the *same*
    /// renderer the export runs, so what you see is byte-for-byte what you
    /// get. Built once and reused; `None` on a box with no usable adapter,
    /// where the preview falls back to the representative fill rather than
    /// pretending it composited anything.
    compositor: Option<crate::gpu::HeadlessCompositor>,
    /// Set once we have tried to build the compositor, so a box that cannot
    /// have one does not retry every frame.
    /// Scratch RGBA readback from the compositor, reused between frames.
    composited: Option<(u32, u32, std::sync::Arc<Vec<u8>>)>,
    drag: Option<Drag>,
    /// Live rubber-band rect while a [`Drag::Marquee`] is past its threshold,
    /// so `draw` can paint it. `None` until the press actually becomes a drag.
    marquee: Option<Rect>,
    /// Set whenever the playhead moves on its own (playback, shuttle, a
    /// transport button, a keyboard jump) so the next frame scrolls it back
    /// into view. Deliberately *not* set by wheel panning: the view follows
    /// the playhead, never fights the user's own scroll.
    last_mouse: Point,
    hover_clip: Option<ClipId>,
    hover_media: Option<MediaId>,
    last_click_media: Option<(MediaId, Instant)>,
    /// A bin press inside the slow-second-click rename window. The rename only
    /// fires if the press is *released* without ever passing
    /// [`BIN_DRAG_THRESHOLD`] — a drag always wins over a rename.
    pending_rename: Option<MediaId>,
    /// When true this is the hermetic `--shots`/demo build: the first sync
    /// seeds the bundled sample project into an empty document. The live app
    /// leaves this false and never fabricates a project — it opens blank.
    last_tick: Instant,
    rect: Rect,
    /// Framework-owned action dispatch. Every document-mutating affordance
    /// dispatches its [`VideoEditorAction`](crate::actions::VideoEditorAction)
    /// through this: the host brackets the undo boundary, applies the action via
    /// the executor (so it replays byte-identically on peers), and remembers it
    /// for the affordance == `ActionSpec` covenant test. Ctrl+Z / Ctrl+Y are
    /// handled OS-wide by the toolkit off the recorded boundary.
    actions: ui::ActionHost,
    /// Copied clips, ready to paste at the playhead (Ctrl+C / Ctrl+X / Ctrl+V).
    /// Holds full clip snapshots — trim window, filters and fades come along —
    /// and pasting preserves their relative offsets and lanes, so copying a
    /// multi-clip selection round-trips as a block rather than a pile.
    clipboard: Vec<Clip>,
    /// JKL shuttle rate: -1 reverse, 0 stop, +1..N forward speed multiplier.
    /// Repeated L speeds up, J reverses, K stops — the standard NLE shuttle.
    shuttle: f64,
    /// `false` until the first laid-out frame resolves the bin flap to the
    /// width's natural state — so a cold launch into a narrow window starts
    /// with the bin already collapsed instead of flashing it open.
    // --- responsive media-bin flap (shared toolkit kernel) ---
    /// Drives the media bin's docked-vs-overlay reveal. The bin is the
    /// primary sidebar: docked beside the editor on a wide window, an
    /// eased overlay flap behind a scrim on a narrow one. `open_when_docked`
    /// keeps the wide layout pixel-identical to before.
    bin_flap: FlapState,
    /// The same kernel driving the inspector's docked-vs-overlay reveal from
    /// the right edge. See [`INSPECTOR_BREAKPOINT`].
    inspector_flap: FlapState,
    /// Real-time clock for the flap's frame-rate-independent eased slide.
    flap_tick: Instant,
    // --- media-bin clip management ---
    /// Right-click context menu for a bin item (Add to timeline / Rename /
    /// Delete). Owned as a field and driven directly (`open_at` / `item_at` /
    /// `take_selection`) because the editor is a canvas, not a taffy tree.
    bin_menu: ContextMenu,
    /// What the open context menu (or a pending menu action) targets — a bin
    /// media item or a timeline track header.
    menu_target: Option<MenuTarget>,
    /// Inspector sections the reader has folded away. Presentation, never in
    /// the document — see [`Section`]. Empty means everything is open.
    collapsed: HashSet<Section>,
    /// The installed font families, read from the text renderer the first
    /// time the title section is drawn. Enumerating them needs a renderer,
    /// which the press path has none of, so the list is cached here and the
    /// picker is built from the cache.
    system_fonts: Vec<String>,
    /// The open font picker: which title it is choosing for, the shared
    /// dropdown, and the throwaway document it writes its choice into.
    ///
    /// The dropdown commits into a CRDT `Text` at a path, and the editor's
    /// document is the *collaborative* one — a scratch key written there
    /// would replicate to everyone sharing the project. So it is handed a
    /// local document of its own, and the real write goes out as the same
    /// media action every other title property uses.
    font_picker: Option<(MediaId, Box<ui::widget::Dropdown>, AutoCommit)>,
    /// Which channel the curve editor is showing. Presentation, like
    /// `collapsed`: the curve itself carries all four channels whatever is on
    /// screen, so this never reaches the document.
    curve_channel: crate::curve::Channel,
    /// In-flight inline edit of the selected title's words, through the
    /// shared [`ui::widget::TextInput`] in **multiline** mode.
    ///
    /// A title is two or three lines as often as it is one, and the single
    /// line `EditableLabel` the bin rename uses could not carry a break: the
    /// field rendered an embedded newline as a glyph, which was honest about
    /// the content and useless for authoring it. The field owns its own
    /// string (`TextInput::local`) rather than binding to the document,
    /// because the words are committed through the media action like every
    /// other title property.
    title_edit: Option<(MediaId, ui::widget::TextInput)>,
    /// In-flight inline edit of a marker in the inspector's list.
    marker_rename: Option<(MarkerRef, EditableLabel)>,
    /// Which of the marker's two texts that editor is on. A marker carries a
    /// name and a free-text note; one inline editor serves both, because the
    /// row has room for one field at a time and the gesture is the same.
    marker_field: MarkerField,
    /// Open keyframe menu: which parameter it belongs to, so choosing an item
    /// knows what to key. The menu itself is the shared `ContextMenu`.
    key_menu_param: Option<Param>,
    /// In-flight inline rename of a bin item: the shared `EditableLabel` gives
    /// the edit real caret / selection / clipboard / a11y for free.
    bin_rename: Option<(MediaId, EditableLabel)>,
    /// A bin item being dragged toward the timeline. Armed on a bin press and
    /// promoted to a live drag once the pointer passes the threshold; on release
    /// over a lane it places a clip at the drop position.
    bin_drag: Option<BinDrag>,
    /// `true` while the OS is hovering a file over the window (an external
    /// drag-and-drop), so the bin can invite the drop. Dropping imports it.
    /// Vertical scroll offset of the inspector body (px), so the effects stack
    /// can grow past the pane without clipping the actions.
    inspector_scroll: f64,
    /// Vertical scroll offset of the media bin (px). Without it a project with
    /// more sources than fit the pane drew the rest outside it, unreachable by
    /// any means.
    bin_scroll: f64,
    /// Representative (or, under decode, real) thumbnail frames per source.
    /// Which pane the keyboard is in, and where inside each one. Drives both
    /// arrow navigation and the accessibility tree's active descendant.
    focus: PaneCursor,
    /// The pane to return to after the narrow media-bin overlay closes. The
    /// overlay owns input while its scrim is visible, but closing it must not
    /// strand a keyboard user at an unrelated bin row.
    bin_focus_before_open: Option<Pane>,
    /// Whether the host has given this widget keyboard focus. The exported
    /// tree only nominates an active descendant while it has — an unfocused
    /// widget must not move a screen reader's cursor.
    /// Whether the keyboard-shortcut sheet is open. ~15 shortcuts exist and
    /// none of them were written down anywhere in the product; a sheet is also
    /// the only way a reader user learns the editor has shortcuts at all.
    /// An in-progress timecode entry, or `None`. Typing a timecode and
    /// pressing Enter jumps the playhead there.
    goto_entry: Option<String>,
    /// Set when the typed timecode could not be read, so the field can say so
    /// instead of closing and going nowhere.
    thumbnails: crate::thumbnail::ThumbnailCache,
    /// Real audio waveform peaks per source, computed off-thread and cached.
    waveforms: crate::waveform::WaveformCache,
    /// Compute waveforms synchronously (so a single-frame `--shots`/demo render
    /// captures them). The live app leaves this off and computes off-thread.
    /// Sources whose bytes are **not** on this device — the media reference
    /// replicated fine but [`Hydration::Projected`](crate::media::Hydration)
    /// says the content has not been fetched from the FUSE (or the file is
    /// gone). Refreshed in `sync` from the same `source()` resolution the
    /// thumbnail and waveform passes already do, so the disclosure costs no
    /// extra stat and never runs in `draw`.
    projected_media: HashSet<MediaId>,
    /// The render in flight, if any. The worker encodes off the UI thread and
    /// publishes progress; this polls it each frame. Dropped once its status
    /// is terminal and the dialog has shown the outcome.
    export_job: Option<crate::export::ExportJob>,
    /// A destination-picker round-trip in flight, drained each frame exactly
    /// like the import one. The user chooses where the file lands (and, under
    /// a strict sandbox, that grant is what earns the write).
    export_dest_rx: Option<portal::ExportDestChannel>,
    /// A short, honest line about the export flow itself — choosing a
    /// destination, or why the round-trip failed. Distinct from
    /// [`ExportStatus`], which reports the *render*.
    export_notice: Option<String>,
    /// Render cache for the topbar export chip. A render holds animation
    /// frames open for its whole duration, so the chip's words are formatted
    /// and elided when they change rather than once a frame.
    export_chip_text: Option<ExportChipText>,
    /// Whether [`VideoDoc::load_project`] still owes this widget a rebuild.
    ///
    /// Set when the document's heads move and cleared by the reload it asks
    /// for. Starting `true` is what makes the first `sync` load, without
    /// having to infer "first" from heads that are legitimately empty for a
    /// document nobody has written to yet. It also survives a drag: an edit
    /// that arrives while one is in flight cannot be applied then (the live
    /// preview lives only in the in-memory project), and this is what
    /// remembers that it is owed.
    // --- project files ----------------------------------------------------
    /// The file this project is saved in, once it has one. `None` means the
    /// project has never been saved: the work is still safe (the autosave
    /// document holds it) but there is nothing on disk to hand anybody, and
    /// the project button says so.
    project_path: Option<PathBuf>,
    /// Whether the document has changed since the last save to
    /// [`Self::project_path`]. Derived from the document's heads rather than
    /// from a flag each edit remembers to set, so an edit that arrives from a
    /// peer — or from the Ctrl+K palette, which never touches this widget —
    /// marks the project unsaved too.
    saved_heads: Option<Vec<automerge::ChangeHash>>,
    /// A project open/save portal round-trip in flight, drained each `sync`
    /// exactly like the import and export-destination ones.
    project_rx: Option<portal::ProjectChannel>,
    /// A short, honest line about the last project-file operation. Never a
    /// fake success: a save that failed says why, and a project opened with
    /// sources this device cannot read says which. Carries its own severity —
    /// see [`NoticeLevel`].
    project_notice: Option<Notice>,
    resource_invocation: Option<tilo_actions::InvocationId>,
    resource_operation: Option<crate::actions::VideoEditorOperation>,
    resource_result: Option<tilo_actions::InvocationUpdate>,
    resource_pending_operation: Option<(
        tilo_actions::InvocationId,
        crate::actions::VideoEditorOperation,
    )>,
    /// The project menu (Open / Save / Save As / recents), on the shared
    /// [`ContextMenu`] every other menu in the app uses.
    project_menu: ContextMenu,
    /// What each row of the open project menu does. Built with the menu, so
    /// a click resolves to an intent rather than to a row index that shifts
    /// as the recents list grows.
    project_menu_items: Vec<ProjectMenuItem>,
    /// Recently-opened projects, most recent first. Per-device and outside
    /// the CRDT — see [`crate::project_file::recents`].
    recents: Vec<PathBuf>,
    /// A project waiting for the user to say what to do about unsaved work.
    /// Opening a file must never silently discard an unsaved project.
    pending_open: Option<PendingOpen>,
    /// What a `Save` should do once the destination comes back: just save, or
    /// save and then continue into the open that was interrupted.
    save_then_open: Option<PathBuf>,
    /// Save requested by either the UI or the resource-operation door. The
    /// effect adapter only has an immutable document borrow, so sync performs
    /// the file write with the normal mutable event-loop document.
    /// The project's format, worded once for the inspector to paint.
    ///
    /// Cached against the numbers it was built from for the same reason the
    /// export chip's words are: `draw` runs for every animation frame, and a
    /// project's frame size and rate change a handful of times in a session.
    project_format_shown: String,
    /// The `(width, height, fps-bits)` the string above was built from. The
    /// rate is carried as its bit pattern so the freshness test is an exact
    /// comparison rather than a float one.
    project_format_from: (u32, u32, u64),
}

/// A row in the project menu, as an intent rather than an index.
#[derive(Clone, Debug, PartialEq, Eq)]
enum ProjectMenuItem {
    Open,
    Save,
    SaveAs,
    Recent(PathBuf),
}

/// An open the user has asked for that is waiting on an answer about the
/// unsaved project it would replace.
#[derive(Clone, Debug, PartialEq, Eq)]
struct PendingOpen {
    path: PathBuf,
    /// The name of the project that would be discarded, so the question can
    /// say what is at stake instead of "you have unsaved changes".
    losing: String,
}

/// The three answers to that question.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum DiscardChoice {
    /// Stay where you are.
    Cancel,
    /// Save the current project first, then continue into the open.
    Save,
    /// Open anyway. Recoverable: the open is one action, so Ctrl+Z restores
    /// the project that was replaced.
    Discard,
}

/// A drag that started on a media-bin item and is heading for the timeline.
#[derive(Clone, Copy)]
struct BinDrag {
    media: MediaId,
    /// Pointer position where the press began, to measure the drag threshold.
    origin: Point,
    /// `true` once the pointer has moved past the threshold — only then does a
    /// ghost follow the cursor and a drop place a clip.
    active: bool,
}

/// Pointer travel (logical px) before a bin press becomes a drag toward the
/// timeline, so a plain click never jitters into a placement.
const BIN_DRAG_THRESHOLD: f64 = 6.0;

/// What an open context menu acts on.
#[derive(Clone, Copy, PartialEq)]
enum MenuTarget {
    /// A media-bin item: Add to timeline / Rename / Delete.
    Media(MediaId),
    /// A timeline track header: mute / lock / add / delete track.
    Track(TrackId),
    /// A timeline clip: cut / copy / duplicate / split / delete.
    Clip(ClipId),
    /// The inspector's "Add effect" picker for the selected clip.
    AddEffect,
    /// A key control's menu: what to do with `param`'s curve at the playhead.
    Keyframe(Param),
}

impl Default for VideoEditorApp {
    fn default() -> Self {
        Self::new()
    }
}

impl VideoEditorApp {
    #[allow(
        clippy::too_many_lines,
        reason = "one constructor makes every persistent and local editor state explicit at its lifetime boundary"
    )]
    pub fn new() -> Self {
        Self {
            project: Project::new("Untitled"),
            clip_edit_bases: HashMap::new(),
            shot_scenario: ShotScenario::Live,
            shot_lifecycle: ShotLifecycle::default(),
            seed: SeedState::default(),
            playback: PlaybackState::default(),
            presentation: PresentationState {
                // Dev aid: `TILO_VE_EXPORT=1` opens straight into the export
                // dialog so its layout can be screenshotted headlessly.
                export_open: std::env::var_os("TILO_VE_EXPORT").is_some(),
                ..PresentationState::default()
            },
            navigation: NavigationState::default(),
            media_pipeline: MediaPipelineState {
                #[cfg(any(feature = "gst", feature = "decode"))]
                monitor_stale: true,
                ..MediaPipelineState::default()
            },
            render_pipeline: RenderPipelineState::default(),
            project_sync: ProjectSyncState {
                reload_pending: true,
                ..ProjectSyncState::default()
            },
            text_scale: 1.0,
            local_presence: crate::presence::LocalPresence::default(),
            shot_presence: None,
            playhead: 10.0,
            zoom: 34.0,
            scroll: 0.0,
            hbar: ui::widget::Scrollbar::horizontal(),
            inspector_bar: ui::widget::Scrollbar::vertical(),
            bin_bar: ui::widget::Scrollbar::vertical(),
            selection: Selection::default(),
            selected_media: None,
            export: ExportSettings::default(),
            export_status: ExportStatus::Idle,
            import_rx: None,
            import_notice: None,
            pending_imports: Vec::new(),
            undo: ui::UndoAccess::default(),
            #[cfg(feature = "decode")]
            preview_decoder: preview::PreviewDecoder::new(),
            #[cfg(feature = "decode")]
            frame_source: None,
            #[cfg(feature = "decode")]
            frame_source_sig: 0,
            #[cfg(feature = "decode")]
            prune_countdown: 0,
            #[cfg(any(feature = "gst", feature = "decode"))]
            monitor: None,
            mix_meters: std::sync::Arc::new(crate::audio::MixMeters::new()),
            mix: crate::audio::MixSnapshot::default(),
            mix_sig: None,
            meter_tick: Instant::now(),
            doc_heads: Vec::new(),
            compositor: None,
            composited: None,
            drag: None,
            marquee: None,
            last_mouse: Point::ZERO,
            hover_clip: None,
            hover_media: None,
            last_click_media: None,
            pending_rename: None,
            last_tick: Instant::now(),
            rect: Rect::ZERO,
            actions: ui::ActionHost::new(),
            clipboard: Vec::new(),
            shuttle: 0.0,
            bin_flap: FlapState::open_when_docked(),
            inspector_flap: FlapState::open_when_docked(),
            flap_tick: Instant::now(),
            bin_menu: ContextMenu::new(),
            menu_target: None,
            // PROJECT starts folded — the one section that does. A folded
            // header still states what is inside it, and what is inside this
            // one is a single line ("1920 × 1080 · 30 fps"), so folding costs
            // no information at all: it says the format at a glance and keeps
            // its ten preset buttons out of both the column and the draw
            // pass, which they were costing ~0.15ms of every frame to sit in.
            // One press opens it, and the format is a decision taken once a
            // project rather than once an edit.
            collapsed: HashSet::from([Section::Project]),
            curve_channel: crate::curve::Channel::Master,
            system_fonts: Vec::new(),
            font_picker: None,
            title_edit: None,
            marker_rename: None,
            marker_field: MarkerField::Name,
            key_menu_param: None,
            bin_rename: None,
            bin_drag: None,
            inspector_scroll: 0.0,
            bin_scroll: 0.0,
            focus: PaneCursor::default(),
            bin_focus_before_open: None,
            goto_entry: None,
            thumbnails: crate::thumbnail::ThumbnailCache::new(),
            waveforms: crate::waveform::WaveformCache::new(),
            projected_media: HashSet::new(),
            export_job: None,
            export_dest_rx: None,
            export_notice: None,
            export_chip_text: None,
            project_path: None,
            saved_heads: None,
            project_rx: None,
            project_notice: None,
            resource_invocation: None,
            resource_operation: None,
            resource_result: None,
            resource_pending_operation: None,
            project_menu: ContextMenu::new(),
            project_menu_items: Vec::new(),
            recents: crate::project_file::recents::load(),
            pending_open: None,
            save_then_open: None,
            // Filled on the first `sync`, from whatever project the document
            // actually holds -- a blank one included.
            project_format_shown: String::new(),
            project_format_from: (0, 0, 0),
        }
    }

    /// The hermetic `--shots`/demo build: seeds the bundled sample project on
    /// the first sync. This is the sole source of sample media/clips — the
    /// live app never fabricates a project.
    pub fn new_demo() -> Self {
        let mut app = Self::new();
        app.seed.demo = true;
        // A single-frame demo/`--shots` render can't wait on a background
        // waveform compute, so warm them synchronously for a deterministic shot.
        app.render_pipeline.warm_waveforms_sync = true;
        app
    }

    /// Construct the editor pre-arranged for a deterministic `--shots`
    /// capture scenario (see [`ShotScenario`]). The scenario is applied once
    /// during the first `sync`, after the sample project loads.
    pub fn new_for_shot(scenario: ShotScenario) -> Self {
        let mut app = Self::new_demo();
        app.shot_scenario = scenario;
        app
    }

    /// Build a blank live app that imports `paths` on its first `sync` (as
    /// real clips, probed for real metadata). Used by `--shots`/tests to seed a
    /// real imported clip deterministically without opening the portal — the
    /// same code path a portal pick drives.
    pub fn new_import(paths: Vec<PathBuf>) -> Self {
        let mut app = Self::new();
        app.pending_imports = paths;
        app
    }

    /// Like [`Self::new_import`] but also drops the first imported clip onto the
    /// timeline (creating a track if needed) and parks the playhead inside it,
    /// so the preview has real content to decode/composite for a `--shots`
    /// capture or a `--import` preload.
    pub fn new_import_placed(paths: Vec<PathBuf>) -> Self {
        let mut app = Self::new_import(paths);
        app.media_pipeline.auto_place_imports = true;
        app
    }

    /// Whether the canvas is currently showing real decoded footage rather
    /// than the representative fill. The `--shots` decode path pumps `sync`
    /// until this is true, so an asynchronously-prerolled frame is captured
    /// instead of a stand-in.
    ///
    /// This must be answered by whatever actually *paints* the canvas. It used
    /// to read `preview_decoder`, which the GPU compositor replaced: the
    /// screenshot path then waited on a decoder that no longer fed the picture,
    /// saw a frame in it, and captured the representative fill believing it had
    /// the footage. Measured on the demo clip, the captured preview's
    /// horizontal detail fell from 11.42 to 0.56 — a flat gradient — while
    /// every test stayed green, because no test compared the *canvas* against
    /// the source. The compositor's frame source now reports whether it put a
    /// decoded picture on any layer, and that is the answer.
    pub fn has_preview_frame(&self) -> bool {
        #[cfg(feature = "decode")]
        {
            self.frame_source
                .as_ref()
                .is_some_and(|s| s.served_real_picture())
        }
        #[cfg(not(feature = "decode"))]
        {
            false
        }
    }

    /// The selected clip's source aspect and the project frame's, which is
    /// what [`crate::model::Mask::over`] needs to place a shape over the
    /// picture. A source with no probed size falls back to the frame's own
    /// aspect — the same assumption the compositor's aspect fit makes.
    fn selected_aspects(&self) -> (f64, f64) {
        let frame = self.project.width.max(1) as f64 / self.project.height.max(1) as f64;
        let src = self
            .selected()
            .and_then(|id| self.project.clip(id))
            .and_then(|c| self.project.media(c.media))
            .filter(|m| m.width > 0 && m.height > 0)
            .map(|m| m.width as f64 / m.height as f64)
            .unwrap_or(frame);
        (src, frame)
    }

    /// The colour of the decoded preview frame at normalised source
    /// coordinates `(u, v)`, or `None` when there is no decoded picture to
    /// read one out of.
    ///
    /// `None` is the honest answer on a build with no decoder, on an
    /// un-hydrated reference, and before the pipeline has prerolled — in
    /// every one of those cases the canvas is showing the representative
    /// fill, and handing back *its* colour as "the colour of your footage"
    /// would be a fabrication. The caller shows the control as unavailable
    /// instead.
    pub fn preview_pixel(&self, normalized_x: f64, normalized_y: f64) -> Option<[f64; 3]> {
        #[cfg(feature = "decode")]
        {
            let frame = self.preview_decoder.frame()?;
            // The sample must belong to the clip under the playhead, not to
            // whatever the decoder last held for a different one.
            let want = self.project.top_video_clip_at(self.playhead)?.media;
            if self.preview_decoder.frame_media() != Some(want) {
                return None;
            }
            if frame.width == 0 || frame.height == 0 {
                return None;
            }
            let pixel_x = ((normalized_x.clamp(0.0, 1.0) * f64::from(frame.width)) as u32)
                .min(frame.width - 1);
            let pixel_y = ((normalized_y.clamp(0.0, 1.0) * f64::from(frame.height)) as u32)
                .min(frame.height - 1);
            let byte_index = ((pixel_y * frame.width + pixel_x) * 4) as usize;
            let [red, green, blue] = frame.rgba.get(byte_index..byte_index + 3)? else {
                return None;
            };
            Some([
                f64::from(*red) / 255.0,
                f64::from(*green) / 255.0,
                f64::from(*blue) / 255.0,
            ])
        }
        #[cfg(not(feature = "decode"))]
        {
            let _ = (normalized_x, normalized_y);
            None
        }
    }

    /// Like [`Self::new`] but with the media bin flap snapped fully open, for
    /// a deterministic narrow-window `--shots` capture that shows the overlay
    /// sidebar revealed in a single frame.
    pub fn new_with_bin_open() -> Self {
        let mut app = Self::new_demo();
        app.bin_flap.snap(true);
        // Pre-settled: the explicit open snap is authoritative, so the first
        // laid-out frame must not re-snap it to the width's default.
        app.navigation.flap_settled = true;
        app
    }

    /// Resolve the bin's docked-vs-overlay layout purely from the allocated
    /// width against the breakpoint. The shared `SplitLayout` is the single
    /// source of truth so the kernel's reconcile/animation agree with our
    /// geometry.
    fn bin_layout(&self) -> SplitLayout {
        SplitLayout::from_width(self.rect.width() / self.text_scale, BIN_BREAKPOINT)
    }

    /// Docked or overlay for the inspector, from the same shared kernel the
    /// bin uses — one rule for "this window is too narrow for a fixed side
    /// panel", applied at each panel's own breakpoint.
    fn inspector_split(&self) -> SplitLayout {
        SplitLayout::from_width(self.rect.width() / self.text_scale, INSPECTOR_BREAKPOINT)
    }

    /// Text in the inspector is rendered at the user-selected scale.  Unlike
    /// the timeline, it is a dense form rather than a canvas, so its rows and
    /// the column that contains them must grow by the same amount.  Capping
    /// at 2× keeps the panel practical on a small display while still making
    /// the platform's largest advertised text setting fully usable.
    fn inspector_ui_scale(&self) -> f64 {
        self.text_scale.clamp(1.0, 2.0)
    }

    /// The inspector gets wider with large text as well as taller.  At 200%
    /// a 300px column only affords 150 logical pixels: labels and their
    /// values then have nowhere truthful to go.  The modest cap preserves a
    /// useful preview even when the panel is an overlay.
    fn inspector_width(&self) -> f64 {
        INSPECTOR_W * self.inspector_ui_scale().min(1.5)
    }

    /// The bin is a compact catalogue at 100%, but its two text lines and
    /// status chip need real vertical room once the platform doubles text.
    /// Scale its *geometry* (not just its glyphs) together with the inspector,
    /// capped where an overlay would otherwise hide the entire small window.
    fn bin_ui_scale(&self) -> f64 {
        self.text_scale.clamp(1.0, 1.6)
    }

    fn bin_width(&self) -> f64 {
        // An overlay may take the whole narrow window, but never extend past
        // it: an off-screen sliver is unreachable by pointer, keyboard and
        // screen reader alike.
        (BIN_W * self.bin_ui_scale()).min(self.rect.width())
    }

    /// The clip identity and four factual properties are persistent context,
    /// not scroll content.  Keeping them in a fixed header makes it obvious
    /// what a deep control edits and prevents a partially scrolled row from
    /// ever painting beneath the editor chrome.
    fn inspector_body_origin(&self, r: &Regions) -> f64 {
        // Four 2× property lines need substantially more air than their 1×
        // baselines suggest; reserve a body viewport of at least 160px even
        // in a short window rather than allowing the last value to cross the
        // first scrolled control.
        let max_header = (r.inspector.height() - 160.0).max(90.0);
        r.inspector.y0 + (190.0 * self.inspector_ui_scale()).min(max_header)
    }

    fn inspector_viewport(&self, r: &Regions) -> Rect {
        Rect::new(
            r.inspector.x0,
            self.inspector_body_origin(r),
            r.inspector.x1,
            r.inspector.y1,
        )
    }

    /// The top bar has a different constraint from the side panes: it must
    /// fit labelled *commands* on one short row.  A 1440px desktop at 200%
    /// text has about the same room for those labels as a 720px window at the
    /// default size, even though its physical rectangles are still 1440px
    /// wide.  Treating only the latter as compact left the scaled labels to
    /// run into one another.
    fn topbar_is_compact(&self) -> bool {
        self.rect.width() < TOPBAR_COMPACT_BREAKPOINT
            || (self.text_scale > 1.25 && self.rect.width() / self.text_scale < 800.0)
    }

    /// Canvas chrome has fixed logical geometry, whereas text is scaled by
    /// `TextRenderer`.  Let the surrounding rhythm grow with large text too,
    /// but cap it before a small laptop loses the entire preview to chrome.
    fn chrome_scale(&self) -> f64 {
        self.text_scale.clamp(1.0, 1.35)
    }

    fn topbar_height(&self) -> f64 {
        TOPBAR_H * self.chrome_scale()
    }

    fn topbar_target(&self) -> f64 {
        TOPBAR_TOUCH_TARGET * self.chrome_scale()
    }

    fn timeline_height(&self) -> f64 {
        // Large type gives audio lanes the same two-row header as video
        // lanes, so reserve their extra vertical rhythm rather than cutting
        // the last AUDIO label off behind the add-track footer.
        if self.text_scale > 1.25 {
            TIMELINE_H * 1.5
        } else {
            TIMELINE_H * self.chrome_scale()
        }
    }

    fn transport_height(&self) -> f64 {
        TRANSPORT_H * self.chrome_scale()
    }

    fn ruler_height(&self) -> f64 {
        RULER_H * self.chrome_scale()
    }

    fn lane_height(&self, kind: TrackKind) -> f64 {
        let base = if kind == TrackKind::Video || self.text_scale > 1.25 {
            VIDEO_LANE_H
        } else {
            AUDIO_LANE_H
        };
        base * self.chrome_scale()
    }

    /// Whether the inspector is on screen right now: always when docked,
    /// only while the flap is open when it is an overlay.
    fn inspector_is_visible(&self, r: &Regions) -> bool {
        !r.inspector_overlay || self.inspector_flap.is_open(self.inspector_split())
    }

    // ---- region geometry -------------------------------------------------

    fn time_to_x(&self, t: f64, lanes: &Rect) -> f64 {
        lanes.x0 + (t - self.scroll) * self.zoom
    }
    fn x_to_time(&self, x: f64, lanes: &Rect) -> f64 {
        (self.scroll + (x - lanes.x0) / self.zoom).max(0.0)
    }

    // ---- horizontal timeline scroll --------------------------------------

    /// How many seconds fit across the lane area at the current zoom.
    fn visible_seconds(&self, r: &Regions) -> f64 {
        (r.lanes.width() / self.zoom.max(0.001)).max(0.0)
    }

    /// The scrollable extent in seconds: the project's content plus a screenful
    /// of headroom past the end, so a clip can be dragged out beyond the last
    /// one instead of hitting a wall at the final frame.
    fn content_seconds(&self, r: &Regions) -> f64 {
        self.project.duration() + self.visible_seconds(r) * 0.5
    }

    /// Furthest left-edge time reachable by scrolling. Zero when everything
    /// already fits.
    fn max_scroll(&self, r: &Regions) -> f64 {
        (self.content_seconds(r) - self.visible_seconds(r)).max(0.0)
    }

    /// The shared scroll geometry for the timeline's horizontal axis, in
    /// **pixels** (the units [`ui::widget::ScrollMetrics`] speaks), so the
    /// toolkit's scrollbar model computes the same thumb every other scroll
    /// surface gets.
    fn h_metrics(&self, r: &Regions) -> ui::widget::ScrollMetrics {
        ui::widget::ScrollMetrics::new(
            self.content_seconds(r) * self.zoom,
            r.lanes.width(),
            self.scroll * self.zoom,
        )
    }

    /// Keep `scroll` inside the content bounds. Called after every zoom,
    /// scroll and project change, so the view can never strand itself past
    /// the end of the timeline.
    fn clamp_scroll(&mut self, r: &Regions) {
        self.scroll = self.scroll.clamp(0.0, self.max_scroll(r));
    }

    /// Pan by `dt` seconds (positive = later), clamped to the content.
    fn scroll_by(&mut self, dt: f64, r: &Regions) {
        self.scroll += dt;
        self.clamp_scroll(r);
    }

    /// Pan the timeline by one readable page. Page Up/Down provide a
    /// keyboard equivalent to the horizontal scrollbar and Shift-wheel path
    /// without stealing Left/Right from frame stepping and clip nudging.
    fn scroll_timeline_page(&mut self, later: bool, r: &Regions) -> bool {
        let page = self.visible_seconds(r) * 0.9;
        if page <= 0.0 {
            return false;
        }
        self.scroll_by(if later { page } else { -page }, r);
        true
    }

    /// Bring `t` into view, keeping a margin at whichever edge it crossed so
    /// the playhead is never pinned against the frame. Used to follow playback
    /// and to chase a keyboard-moved playhead.
    fn scroll_to_time(&mut self, t: f64, r: &Regions) {
        let visible = self.visible_seconds(r);
        if visible <= 0.0 {
            return;
        }
        let margin = (visible * 0.1).min(1.0);
        if t < self.scroll + margin {
            self.scroll = t - margin;
        } else if t > self.scroll + visible - margin {
            // Jump a page ahead rather than creeping, so playback scrolls in
            // readable steps instead of smearing every frame.
            self.scroll = t - visible * 0.25;
        }
        self.clamp_scroll(r);
    }

    /// Ask the thumbnail cache for the frames every **visible** video clip's
    /// filmstrip will draw this frame, at the source times its cells sit on.
    ///
    /// Lives here rather than in `draw_clip` because requesting needs `&mut
    /// self` and drawing does not. `ensure_strip` is idempotent, so calling it
    /// every frame costs a lookup once the frames are in; off-screen clips are
    /// skipped so a long scrolled timeline does not queue decodes nobody can
    /// see.
    fn request_filmstrips(&mut self, r: &Regions) {
        let mut want: Vec<(MediaId, u8, crate::media::MediaSource, f64, f64, usize)> = Vec::new();
        for (tid, kind, lane) in self.lane_rects(r) {
            if kind != TrackKind::Video {
                continue;
            }
            for clip in self.project.clips_on(tid) {
                let cr = self.clip_rect(clip, &lane, r);
                if cr.x1 < r.lanes.x0 || cr.x0 > r.lanes.x1 {
                    continue;
                }
                let inner = clip_inner_rect(cr);
                if inner.height() <= 4.0 || inner.width() <= 4.0 {
                    continue;
                }
                let (_, n) = filmstrip_cells(inner);
                let Some(m) = self.project.media(clip.media) else {
                    continue;
                };
                want.push((
                    clip.media,
                    m.palette,
                    m.source(),
                    clip.in_point,
                    clip.out_point,
                    n as usize,
                ));
            }
        }
        for (id, palette, src, in_point, out_point, cells) in want {
            self.thumbnails
                .ensure_strip(id, palette, &src, in_point, out_point, cells);
        }
    }

    /// The horizontal scrollbar's track rect, or `None` when the timeline
    /// content already fits. Overlaid on the bottom of the lane area, inset
    /// like every other toolkit scroll surface.
    fn hscroll_track(&self, r: &Regions) -> Option<Rect> {
        if !self.h_metrics(r).overflows() {
            return None;
        }
        let y1 = r.timeline.y1 - SCROLLBAR_INSET;
        Some(Rect::new(
            r.lanes.x0 + SCROLLBAR_INSET,
            y1 - SCROLLBAR_THICKNESS,
            r.lanes.x1 - SCROLLBAR_INSET,
            y1,
        ))
    }

    /// Set the scroll offset from a scrollbar interaction, which works in
    /// pixels; convert back to the seconds `scroll` is measured in.
    fn set_scroll_px(&mut self, px: f64, r: &Regions) {
        self.scroll = px / self.zoom.max(0.001);
        self.clamp_scroll(r);
    }

    /// (track id, kind, lane rect) in display order. Lane rect spans the
    /// full lane width.
    fn lane_rects(&self, r: &Regions) -> Vec<(TrackId, TrackKind, Rect)> {
        let mut out = Vec::new();
        let mut y = r.lanes.y0 + LANE_GAP;
        for t in self.project.display_tracks() {
            let h = self.lane_height(t.kind);
            let lane = Rect::new(r.lanes.x0, y, r.lanes.x1, y + h);
            out.push((t.id, t.kind, lane));
            y += h + LANE_GAP;
        }
        out
    }

    fn clip_rect(&self, clip: &Clip, lane: &Rect, r: &Regions) -> Rect {
        let x0 = self.time_to_x(clip.start, &r.lanes);
        let x1 = self.time_to_x(clip.end(), &r.lanes);
        Rect::new(x0, lane.y0 + 2.0, x1.max(x0 + 2.0), lane.y1 - 2.0)
    }

    /// The bounded title band at the top of every clip.  Sharing this small
    /// geometry between drawing and the high-text regression makes a scaled
    /// label unable to grow through the filmstrip or waveform below it.
    fn clip_header_rect(&self, clip_rect: Rect) -> Rect {
        Rect::new(
            clip_rect.x0,
            clip_rect.y0,
            clip_rect.x1,
            (clip_rect.y0 + 18.0 * self.chrome_scale()).min(clip_rect.y1),
        )
    }

    /// The mute pill rect on a track header (shared by draw + hit-test).
    ///
    /// Anchored to the top of the head rather than its middle. A channel
    /// strip reads downward — name and switches, then what the lane is, then
    /// its level — and the middle is now the *only* place the meter could go
    /// on a 46px audio lane. Centring the pills also put them across the row
    /// that now carries the fader's reading, which is why that row had no
    /// width to state it in.
    fn mute_pill_rect(&self, header: Rect) -> Rect {
        let scale = self.chrome_scale();
        let side = 18.0 * scale;
        let inset = 8.0 * scale;
        let y = header.y0 + 7.0 * scale;
        Rect::new(header.x1 - inset - side, y, header.x1 - inset, y + side)
    }

    /// The master strip: the corner of the timeline left of the ruler, above
    /// the column of track heads.
    ///
    /// The one piece of chrome that was genuinely empty, and the right place
    /// for it: a master meter belongs at the head of the channel-strip column,
    /// it is on screen the entire time the timeline is, and it does not have
    /// to be squeezed into the transport, whose strip already drops its
    /// readouts one by one as the window narrows. A warning that disappears at
    /// 900px is not a warning.
    fn master_strip_rect(&self, r: &Regions) -> Rect {
        Rect::new(r.timeline.x0, r.timeline.y0, r.gutter.x1, r.ruler.y1)
    }

    /// The clip flag inside it: what is clicked to walk the overloads.
    fn clip_flag_rect(&self, r: &Regions) -> Rect {
        let strip = self.master_strip_rect(r);
        Rect::new(
            strip.x1 - 52.0,
            strip.y0 + 4.0,
            strip.x1 - 8.0,
            strip.y0 + 18.0,
        )
    }

    /// The lane's level meter, along the foot of its head.
    ///
    /// It lives here — beside the name, the mute and the solo — because this
    /// column *is* the channel strip: everything else about how a lane sounds
    /// is decided in these 128 pixels, and a level you have to open a panel to
    /// see is a level nobody looks at. Full width, so a glance down the
    /// gutter reads every lane at once.
    fn track_meter_rect(&self, header: Rect) -> Rect {
        let scale = self.chrome_scale();
        Rect::new(
            header.x0 + 10.0 * scale,
            header.y1 - 8.0 * scale,
            header.x1 - 10.0 * scale,
            header.y1 - 2.0 * scale,
        )
    }

    /// The lock pill rect, immediately left of the mute pill.
    fn lock_pill_rect(&self, header: Rect) -> Rect {
        let m = self.mute_pill_rect(header);
        let side = m.width();
        let step = if self.text_scale > 1.25 {
            side + 1.0
        } else {
            22.0 * self.chrome_scale()
        };
        Rect::new(m.x0 - step, m.y0, m.x1 - step, m.y1)
    }

    /// The solo pill rect, immediately left of the lock pill.
    fn solo_pill_rect(&self, header: Rect) -> Rect {
        let l = self.lock_pill_rect(header);
        let side = l.width();
        let step = if self.text_scale > 1.25 {
            side + 1.0
        } else {
            22.0 * self.chrome_scale()
        };
        Rect::new(l.x0 - step, l.y0, l.x1 - step, l.y1)
    }

    /// The two "add track" buttons in the gutter footer, if there is room below
    /// the last lane: `(kind, rect)` for a Video and an Audio add.
    fn add_track_buttons(&self, r: &Regions) -> Vec<(TrackKind, Rect)> {
        let lanes = self.lane_rects(r);
        let y = lanes
            .last()
            .map_or(r.gutter.y0 + 6.0, |(_, _, l)| l.y1 + LANE_GAP + 4.0);
        let h = 24.0 * self.chrome_scale();
        if y + h > r.timeline.y1 {
            return Vec::new();
        }
        let pad = 8.0;
        let w = (r.gutter.width() - pad * 2.0 - 6.0) / 2.0;
        let vbtn = Rect::new(r.gutter.x0 + pad, y, r.gutter.x0 + pad + w, y + h);
        let abtn = Rect::new(vbtn.x1 + 6.0, y, vbtn.x1 + 6.0 + w, y + h);
        vec![(TrackKind::Video, vbtn), (TrackKind::Audio, abtn)]
    }

    fn transport_buttons(&self, r: &Regions) -> [(TransportAction, Rect); 5] {
        let scale = self.chrome_scale();
        let cy = r.transport.y0 + 20.0 * scale;
        let cx0 = r.transport.x0 + 30.0 * scale;
        let gap = 40.0 * scale;
        let side = 30.0 * scale;
        let mk = |i: f64, a: TransportAction| {
            let cx = cx0 + i * gap;
            (
                a,
                Rect::new(
                    cx - side / 2.0,
                    cy - side / 2.0,
                    cx + side / 2.0,
                    cy + side / 2.0,
                ),
            )
        };
        [
            mk(0.0, TransportAction::ToStart),
            mk(1.0, TransportAction::StepBack),
            mk(2.0, TransportAction::PlayPause),
            mk(3.0, TransportAction::StepFwd),
            mk(4.0, TransportAction::ToEnd),
        ]
    }

    /// The history buttons, immediately right of the transport cluster.
    ///
    /// Drawn as ghost squares rather than as the transport's filled circles,
    /// because they are not transport: nothing here moves the playhead. They
    /// share the row because that is where the strip's other one-press verbs
    /// already are.
    ///
    /// Empty when the strip is too narrow to hold them without reaching into
    /// the readout — the same rule the readout itself follows, and the reason
    /// the scrubber starts where it does.
    fn history_buttons(&self, r: &Regions) -> Vec<(ui::UndoRequest, Rect)> {
        let scale = self.chrome_scale();
        if r.transport.width() < HISTORY_MIN_STRIP_W * scale {
            return Vec::new();
        }
        let cy = r.transport.y0 + 20.0 * scale;
        let x0 = r.transport.x0 + HISTORY_X * scale;
        let width = HISTORY_W * scale;
        let mk = |i: f64, v: ui::UndoRequest| {
            let x = x0 + i * (width + 4.0 * scale);
            (
                v,
                Rect::new(x, cy - 13.0 * scale, x + width, cy + 13.0 * scale),
            )
        };
        vec![
            mk(0.0, ui::UndoRequest::Undo),
            mk(1.0, ui::UndoRequest::Redo),
        ]
    }

    /// Where everything right of the transport cluster starts: past the
    /// history buttons when the strip is showing them, and where it always
    /// used to be when it is not.
    ///
    /// One answer for the scrubber and the readout, because two would let
    /// the buttons and the timecode drift onto each other — which is
    /// precisely what happened the first time they were laid out apart.
    fn transport_content_x(&self, r: &Regions) -> f64 {
        let scale = self.chrome_scale();
        if self.history_buttons(r).is_empty() {
            TRANSPORT_PLAIN_X * scale
        } else {
            TRANSPORT_RIGHT_X * scale
        }
    }

    /// The transport scrubber's track. `Rect::ZERO` when the transport is too
    /// narrow to hold one — below roughly a 500px window the old unclamped
    /// arithmetic produced an *inverted* rect (x0 > x1) and drew the knob
    /// outside its own track. Capped at [`MAX_SCRUBBER_W`] so a very wide
    /// window does not stretch a 6px knob across three thousand pixels of bar.
    fn scrubber_rect(&self, r: &Regions) -> Rect {
        let left = r.transport.x0 + self.transport_content_x(r);
        let right = r.transport.x1 - 16.0;
        if right - left < MIN_SCRUBBER_W {
            return Rect::ZERO;
        }
        let w = (right - left).min(MAX_SCRUBBER_W);
        let scale = self.chrome_scale();
        let y0 = r.transport.y0 + 38.0 * scale;
        Rect::new(left, y0, left + w, y0 + 8.0 * scale)
    }

    /// Whether the transport currently has room for a scrubber.
    fn has_scrubber(&self, r: &Regions) -> bool {
        self.scrubber_rect(r).width() > 1.0
    }

    /// Whether this source's bytes are absent from this device. Read from the
    /// set `sync` resolves, so a `draw` pass never touches the filesystem.
    fn media_is_projected(&self, mid: MediaId) -> bool {
        self.projected_media.contains(&mid)
    }

    /// Whether this source's bytes are present but are not usable media.
    ///
    /// Import refuses such a file now, but a document written before that
    /// refusal existed still carries the entry, and it renders from whatever
    /// was probed at the time — which for a web page saved as `.mp4` is
    /// nothing at all. Derived from what the item actually carries (no
    /// duration, no dimensions) or from a decode that has since been proven
    /// impossible, so it cannot drift from what the rest of the app knows.
    fn media_is_unreadable(&self, mid: MediaId) -> bool {
        if self.media_is_projected(mid) {
            return false; // absent bytes are a different, recoverable state
        }
        let Some(m) = self.project.media(mid) else {
            return false;
        };
        if self.thumbnails.is_undecodable(mid) {
            return true;
        }
        m.duration <= 0.0 && m.width == 0 && m.height == 0
    }

    /// The bin row's metadata line. Unknown is written as unknown: a source
    /// that carries no dimensions or no length must not be rendered as
    /// "0p · 0:00", which reads as a probed fact and is not one.
    fn bin_subtitle(&self, m: &MediaItem) -> String {
        let dur = if m.duration > 0.0 {
            clock(m.duration)
        } else {
            "length unknown".to_string()
        };
        match m.kind {
            MediaKind::Audio => format!("Audio · {dur}"),
            // A title is drawn at whatever the project frame is, so it has
            // no size of its own. That is not the same as "we could not
            // read the size", which is what an unreadable file gets.
            MediaKind::Text => format!("Title · {dur}"),
            _ if m.height == 0 => format!("size unknown · {dur}"),
            _ => format!("{}p · {}", m.height, dur),
        }
    }

    /// Media for the bin in a stable, meaningful order: visual media
    /// first, then audio, each alphabetised. (The CRDT key iteration order
    /// is otherwise arbitrary.)
    fn bin_order(&self) -> Vec<MediaId> {
        let mut v: Vec<&crate::model::MediaItem> = self.project.media.iter().collect();
        v.sort_by(|a, b| {
            b.kind
                .is_visual()
                .cmp(&a.kind.is_visual())
                .then(a.name.cmp(&b.name))
        });
        v.into_iter().map(|m| m.id).collect()
    }

    /// Row pitch of the media bin list, and where the first row starts.
    const BIN_ROW_H: f64 = 56.0;
    const BIN_LIST_TOP: f64 = 40.0;

    fn bin_row_height(&self) -> f64 {
        Self::BIN_ROW_H * self.bin_ui_scale()
    }

    fn bin_list_top(&self) -> f64 {
        Self::BIN_LIST_TOP * self.bin_ui_scale()
    }

    /// The bin header's "+ Title" button, which makes a title source, places
    /// it at the playhead and opens its words for typing.
    fn add_title_rect(&self, r: &Regions) -> Rect {
        let scale = self.bin_ui_scale();
        let x1 = r.bin.x0 + 152.0 * scale;
        Rect::new(
            x1 - 62.0 * scale,
            r.bin.y0 + 6.0 * scale,
            x1,
            r.bin.y0 + 26.0 * scale,
        )
    }

    fn bin_item_rects(&self, r: &Regions) -> Vec<(MediaId, Rect)> {
        let mut out = Vec::new();
        let row_h = self.bin_row_height();
        let mut y = r.bin.y0 + self.bin_list_top() - self.bin_scroll;
        for mid in self.bin_order() {
            let inset = 8.0 * self.bin_ui_scale();
            let rect = Rect::new(r.bin.x0 + inset, y, r.bin.x1 - inset, y + row_h - 6.0);
            out.push((mid, rect));
            y += row_h;
        }
        out
    }

    /// Whether the media bin is on screen: docked, or an open overlay flap.
    fn bin_is_visible(&self, r: &Regions) -> bool {
        !r.bin_overlay || self.bin_flap.is_open(self.bin_layout())
    }

    /// The bin's visible list viewport — everything below the header band.
    fn bin_viewport(&self, r: &Regions) -> Rect {
        Rect::new(r.bin.x0, r.bin.y0 + 30.0, r.bin.x1, r.bin.y1)
    }

    /// Total height the bin list wants, in px.
    fn bin_content_height(&self) -> f64 {
        self.project.media.len() as f64 * self.bin_row_height() + (self.bin_list_top() - 30.0) + 8.0
    }

    fn bin_metrics(&self, r: &Regions) -> ui::widget::ScrollMetrics {
        ui::widget::ScrollMetrics::new(
            self.bin_content_height(),
            self.bin_viewport(r).height(),
            self.bin_scroll,
        )
    }

    fn bin_max_scroll(&self, r: &Regions) -> f64 {
        self.bin_metrics(r).max_scroll()
    }

    /// The bin's vertical scrollbar track, or `None` when the list fits.
    fn bin_scroll_track(&self, r: &Regions) -> Option<Rect> {
        if !self.bin_metrics(r).overflows() {
            return None;
        }
        let v = self.bin_viewport(r);
        let x1 = v.x1 - SCROLLBAR_INSET;
        Some(Rect::new(
            x1 - SCROLLBAR_THICKNESS,
            v.y0 + SCROLLBAR_INSET,
            x1,
            v.y1 - SCROLLBAR_INSET,
        ))
    }

    fn inspector_metrics(&self, r: &Regions) -> ui::widget::ScrollMetrics {
        let viewport = self.inspector_viewport(r);
        ui::widget::ScrollMetrics::new(
            viewport.height() + self.inspector_max_scroll(r),
            viewport.height(),
            self.inspector_scroll,
        )
    }

    /// The inspector's vertical scrollbar track, or `None` when it all fits.
    /// Replaces a painted-only thumb that had no hit-test at all — the pane's
    /// EFFECTS section was clipped off the bottom and unreachable.
    fn inspector_scroll_track(&self, r: &Regions) -> Option<Rect> {
        if !self.inspector_metrics(r).overflows() {
            return None;
        }
        let v = self.inspector_viewport(r);
        let x1 = v.x1 - SCROLLBAR_INSET;
        Some(Rect::new(
            x1 - SCROLLBAR_THICKNESS,
            v.y0 + SCROLLBAR_INSET,
            x1,
            v.y1 - SCROLLBAR_INSET,
        ))
    }

    /// Track rect + metrics + current offset for one scrollable pane, so the
    /// press / drag / hover paths do not each re-derive them.
    fn scroll_pane(
        &self,
        pane: ScrollPane,
        r: &Regions,
    ) -> Option<(Rect, ui::widget::ScrollMetrics)> {
        match pane {
            ScrollPane::Timeline => self.hscroll_track(r).map(|t| (t, self.h_metrics(r))),
            ScrollPane::Inspector => self
                .inspector_scroll_track(r)
                .map(|t| (t, self.inspector_metrics(r))),
            ScrollPane::Bin => self.bin_scroll_track(r).map(|t| (t, self.bin_metrics(r))),
        }
    }

    fn scroll_bar_mut(&mut self, pane: ScrollPane) -> &mut ui::widget::Scrollbar {
        match pane {
            ScrollPane::Timeline => &mut self.hbar,
            ScrollPane::Inspector => &mut self.inspector_bar,
            ScrollPane::Bin => &mut self.bin_bar,
        }
    }

    fn scroll_bar(&self, pane: ScrollPane) -> &ui::widget::Scrollbar {
        match pane {
            ScrollPane::Timeline => &self.hbar,
            ScrollPane::Inspector => &self.inspector_bar,
            ScrollPane::Bin => &self.bin_bar,
        }
    }

    /// Apply an absolute offset (in the bar's own px units) to `pane`.
    fn set_scroll_offset(&mut self, pane: ScrollPane, offset: f64, r: &Regions) {
        match pane {
            ScrollPane::Timeline => self.set_scroll_px(offset, r),
            ScrollPane::Inspector => {
                let max = self.inspector_max_scroll(r);
                self.inspector_scroll = offset.clamp(0.0, max);
            }
            ScrollPane::Bin => {
                let max = self.bin_max_scroll(r);
                self.bin_scroll = offset.clamp(0.0, max);
            }
        }
    }

    /// Every pane that currently shows a bar, for the hover / press sweeps.
    const SCROLL_PANES: [ScrollPane; 3] =
        [ScrollPane::Inspector, ScrollPane::Bin, ScrollPane::Timeline];

    /// Keep the audio monitor in step with the transport.
    ///
    /// One reconcile per frame rather than a call at every site that moves
    /// the playhead: the transport is set from a dozen places (a key, a
    /// scrub, a click on the ruler, reaching the end), and a monitor that
    /// only heard about some of them would drift out of step with the
    /// picture in exactly the cases the user notices.
    ///
    /// The monitor is *not* re-seeked for small differences while playing.
    /// Its clock is the sample count and the playhead's is wall time, so
    /// they wander apart by a few milliseconds; correcting that every frame
    /// would restart the mix continuously. Only a real jump moves it.
    #[cfg(any(feature = "gst", feature = "decode"))]
    fn pump_monitor(&mut self) {
        if !self.media_pipeline.monitor_tried {
            self.media_pipeline.monitor_tried = true;
            // The demo/shots build must make no sound and open no pipeline:
            // a capture has to stay hermetic.
            if !self.seed.demo {
                self.monitor = Some(crate::audio::AudioMonitor::new(
                    &self.project,
                    crate::audio::open_output(),
                    Box::new(crate::audio::DecodedPcm::new(&self.project)),
                    self.mix_meters.clone(),
                ));
                self.media_pipeline.monitor_stale = false;
            }
        }
        let Some(m) = self.monitor.as_ref() else {
            return;
        };
        if self.media_pipeline.monitor_stale {
            m.set_project(&self.project);
            self.media_pipeline.monitor_stale = false;
        }
        // A jump this big is a scrub or a cut, not clock wander.
        const RESYNC: f64 = 0.35;
        if self.playback.playing {
            if !m.is_playing() {
                m.play(self.playhead);
            } else if (m.position() - self.playhead).abs() > RESYNC {
                m.seek(self.playhead);
            }
        } else {
            if m.is_playing() {
                m.pause();
            }
            if (m.position() - self.playhead).abs() > 1e-6 {
                m.seek(self.playhead);
            }
        }
    }

    // ---- metering --------------------------------------------------------

    /// Age the meters by one frame and take the copy `draw` reads.
    ///
    /// Three things happen here and nothing else does: the bars fall, a
    /// report about a mix the user has since changed is thrown away, and the
    /// shared state is snapshotted. No decode, no analysis, one lock — the
    /// mixer already measured the block, and the whole point is to carry that
    /// number rather than compute a second opinion on the paint path.
    fn pump_mix_meter(&mut self) {
        let now = Instant::now();
        let dt = now.duration_since(self.meter_tick).as_secs_f64();
        self.meter_tick = now;
        let sig = crate::audio::mix_signature(&self.project);
        if self.mix_sig.is_some_and(|had| had != sig) {
            // The levels this was measured from no longer exist. Keeping the
            // warning would be a warning about a mix nobody can hear, and one
            // that never went away is one people learn to ignore.
            self.mix_meters.clear();
        }
        self.mix_sig = Some(sig);
        // The fall is a function of elapsed *wall* time, which a still
        // capture does not have. Left running under `--shots` it made the
        // mixer scenario irreproducible: three captures of one staged mix came
        // out as three different images, because the bars landed wherever
        // process startup and the frame loop happened to leave them — green in
        // one and red in the next, off the same numbers. A visual baseline
        // cannot be taken from that, and neither can a judgement.
        //
        // Same gate the monitor uses (`pump_monitor`) and for the same reason:
        // a capture is hermetic, so it neither makes a sound nor watches a
        // clock.
        if !self.seed.demo {
            self.mix_meters.decay(dt);
        }
        self.mix = self.mix_meters.snapshot();
    }

    /// Take in a block the mixer measured, as the monitor does.
    ///
    /// The one door meter data comes through, so a `--shots` capture and a
    /// test exercise the same recording, latching and merging the live
    /// monitor does rather than a drawing that imitates the result. The
    /// signature is pinned to the project the block describes: without that
    /// the next frame would read the staged report as one about a mix that
    /// has since changed and throw it away.
    fn stage_mix_block(&mut self, at: f64, report: &crate::audio::MixReport) {
        self.mix_meters.record(at, report);
        self.mix_sig = Some(crate::audio::mix_signature(&self.project));
        self.mix = self.mix_meters.snapshot();
    }

    /// Whether the meter still has something to say — a bar in flight, or a
    /// clipping report standing. Keeps frames coming so a meter finishes
    /// falling instead of freezing part-way down when the transport stops.
    fn mix_meter_is_live(&self) -> bool {
        self.mix.master[0] > 0.001 || self.mix.master[1] > 0.001
    }

    /// Park the playhead on the next place the mix went over, wrapping round
    /// to the first. This is the "and find *where*" half: a warning that only
    /// says *that* it happened leaves a person scrubbing a whole timeline.
    fn go_to_next_clipping(&mut self) -> bool {
        let Some(at) = self.mix.next_over_after(self.playhead) else {
            return false;
        };
        self.playhead = at.clamp(0.0, self.project.duration());
        self.playback.chase_playhead = true;
        true
    }

    /// What the mix's state says, in one line, wherever it has to be said in
    /// words — the export dialog and the accessibility tree both read this,
    /// so a reader and the paint cannot disagree about it.
    fn mix_words(&self) -> Option<String> {
        if !crate::audio::has_audio(&self.project) {
            return None;
        }
        if self.mix.clipped() {
            let where_words = self
                .mix
                .overs
                .first()
                .map(|t| timecode(*t, self.project.fps))
                .unwrap_or_default();
            let n = self.mix.overs.len();
            let lanes: Vec<String> = self
                .mix
                .clipped_tracks()
                .iter()
                .filter_map(|id| self.project.track(*id))
                .map(|t| t.name.clone())
                .collect();
            let on = if lanes.is_empty() {
                String::new()
            } else {
                format!(" on {}", lanes.join(", "))
            };
            return Some(format!(
                "Mix clipped{on}: {:+.1} dB over, first at {where_words}{}",
                self.mix.worst_over_db(),
                if n > 1 {
                    format!(" ({n} places)")
                } else {
                    String::new()
                }
            ));
        }
        match self.mix.hold_db() {
            Some(db) => Some(format!("Mix peak {db:.1} dB")),
            // Silence and "nobody has listened" look identical on a meter,
            // and telling someone their levels are fine when nothing has been
            // measured would be the same silent failure in a nicer voice.
            None => Some("Levels not measured — play the timeline to check".into()),
        }
    }

    // ---- playback --------------------------------------------------------
    fn advance_playback(&mut self) {
        let now = Instant::now();
        let dt = now.duration_since(self.last_tick).as_secs_f64();
        self.last_tick = now;
        let dur = self.project.duration();
        if self.playback.playing || self.shuttle != 0.0 {
            // A moving playhead pulls the view along with it.
            self.playback.chase_playhead = true;
        }
        if self.playback.playing {
            self.playhead += dt;
            if self.playhead >= dur {
                self.playhead = dur;
                self.playback.playing = false;
            }
        } else if self.shuttle != 0.0 {
            // Shuttle scrubs at `shuttle`× real time; clamps at the ends.
            self.playhead = (self.playhead + dt * self.shuttle).clamp(0.0, dur);
            if self.playhead <= 0.0 || self.playhead >= dur {
                self.shuttle = 0.0;
            }
        }
    }

    /// Reconcile both side flaps with the current width and advance their
    /// eased reveal using real elapsed time. Kept as the motion-on helper for
    /// direct geometry tests; live drawing calls [`Self::advance_flap_with_motion`].
    #[cfg(test)]
    fn advance_flap(&mut self) {
        self.advance_flap_with_motion(false);
    }

    /// Advance both side flaps, respecting the host's reduced-motion setting.
    /// A reduced-motion frame lands at its target immediately rather than
    /// leaving a half-open panel behind after the host correctly suppresses
    /// subsequent animation frames.
    fn advance_flap_with_motion(&mut self, reduce_motion: bool) {
        let layout = self.bin_layout();
        let insp = self.inspector_split();
        self.bin_flap.reconcile(layout);
        self.inspector_flap.reconcile(insp);
        // First settled frame: snap to the width's natural state so a cold
        // narrow launch starts collapsed (no open→closed flash) and a cold
        // wide launch starts docked-open.
        if !self.navigation.flap_settled {
            self.bin_flap.snap(layout == SplitLayout::Docked);
            self.inspector_flap.snap(insp == SplitLayout::Docked);
            self.navigation.flap_settled = true;
        }
        if reduce_motion {
            let bin_open = self.bin_flap.is_open(layout);
            let inspector_open = self.inspector_flap.is_open(insp);
            self.bin_flap.snap(bin_open);
            self.inspector_flap.snap(inspector_open);
            return;
        }
        let now = Instant::now();
        let dt = now.duration_since(self.flap_tick).as_secs_f64();
        self.flap_tick = now;
        self.bin_flap.tick(layout, dt);
        self.inspector_flap.tick(insp, dt);
    }

    /// Toggle the media bin flap (the topbar hamburger / `B` shortcut).
    fn toggle_bin(&mut self) {
        let layout = self.bin_layout();
        if layout != SplitLayout::Overlay {
            self.bin_flap.toggle(layout);
            return;
        }
        if self.bin_flap.is_open(layout) {
            self.close_bin_overlay();
            return;
        }
        self.bin_focus_before_open = Some(self.focus.pane);
        // A modal surface cannot inherit a half-finished drag from the canvas
        // it is covering. Dropping after opening the bin must not silently
        // commit a trim, marquee, or bin-to-timeline placement behind it.
        self.drag = None;
        self.marquee = None;
        self.bin_drag = None;
        self.pending_rename = None;
        self.bin_flap.open();
        self.focus.pane = Pane::Bin;
        self.clamp_pane_cursor();
    }

    /// Dismiss the narrow media-bin overlay and restore the pane that opened
    /// it. This is shared by the toggle, Escape, and scrim click so every
    /// close path has the same focus-return behaviour.
    fn close_bin_overlay(&mut self) {
        self.bin_flap.close();
        if let Some(previous) = self.bin_focus_before_open.take() {
            self.focus.pane = previous;
            self.clamp_pane_cursor();
        }
    }

    fn bin_overlay_is_open(&self) -> bool {
        self.bin_layout() == SplitLayout::Overlay && self.bin_flap.is_open(SplitLayout::Overlay)
    }

    /// Toggle the inspector flap (the topbar toggle / `Tab`). On a wide
    /// window the inspector is docked and this collapses it away, which is
    /// the same affordance the bin's hamburger offers there.
    fn toggle_inspector(&mut self) {
        self.inspector_flap.toggle(self.inspector_split());
    }

    /// Bring the inspector on screen if it is a closed overlay. Called when
    /// something selects a clip, so "click a clip, edit it" does not need a
    /// second click on a toggle the user has to find first.
    fn reveal_inspector(&mut self) {
        if self.inspector_split() == SplitLayout::Overlay {
            self.inspector_flap.open();
        }
    }

    /// Whether either flap is animating and the host should keep painting.
    fn flap_animating(&self) -> bool {
        self.bin_flap.is_animating(self.bin_layout())
            || self.inspector_flap.is_animating(self.inspector_split())
    }

    /// Whether the playhead is running under its own steam: playing forwards at
    /// 1×, or shuttling at some rate in either direction.
    ///
    /// One predicate, because the play button, the transport readout and the
    /// name a reader hears for that button all have to agree about it. They did
    /// not: every one of them was gated on `playing`, so a shuttle at 16× wore
    /// a play triangle and reported itself nowhere.
    fn is_moving(&self) -> bool {
        self.playback.playing || self.shuttle != 0.0
    }

    /// The words the transport puts above the scrubber for what the playhead is
    /// doing, or `None` when it is standing still.
    fn motion_words(&self) -> Option<String> {
        if self.playback.playing {
            return Some("PLAYING".to_string());
        }
        if self.shuttle == 0.0 {
            return None;
        }
        // Direction as well as rate: "8×" alone does not say which way the
        // footage is running, and reverse is half of what J/L are for.
        let arrows = if self.shuttle > 0.0 {
            "\u{25B6}\u{25B6}"
        } else {
            "\u{25C0}\u{25C0}"
        };
        Some(format!("SHUTTLE {arrows} {:.0}\u{d7}", self.shuttle.abs()))
    }

    fn toggle_play(&mut self) {
        // A running shuttle is what this button stops. Shuttle and playback are
        // mutually exclusive — `shuttle_key` stops playback for the same reason
        // — and the button wears the pause glyph while either is running, so
        // pressing it has to stop whichever one it is. Starting playback on top
        // of a shuttle instead left a non-zero rate behind the readout, which
        // is a state with nothing on screen for it.
        if self.shuttle != 0.0 {
            self.shuttle = 0.0;
            self.last_tick = Instant::now();
            self.playback.chase_playhead = true;
            return;
        }
        self.playback.playing = !self.playback.playing;
        self.last_tick = Instant::now();
        if self.playback.playing && self.playhead >= self.project.duration() {
            self.playhead = 0.0;
        }
        self.playback.chase_playhead = true;
    }

    fn step_frame(&mut self, dir: f64) {
        let step = frame_step(self.project.fps);
        self.playhead = (self.playhead + dir * step).clamp(0.0, self.project.duration());
        self.playback.playing = false;
        self.shuttle = 0.0;
        self.playback.chase_playhead = true;
    }

    /// J/K/L shuttle. Pressing in the *same* direction doubles the rate
    /// (1→2→4…); pressing the opposite direction resets to ±1×; the rate is
    /// advanced each frame in [`Self::advance_playback`].
    fn shuttle_key(&mut self, dir: f64) {
        // Same direction speeds up; reversal restarts at 1×.
        if self.shuttle.signum() == dir.signum() && self.shuttle != 0.0 {
            self.shuttle = (self.shuttle * 2.0).clamp(-16.0, 16.0);
        } else {
            self.shuttle = dir;
        }
        self.playback.playing = false; // shuttle and play are mutually exclusive
        self.last_tick = Instant::now();
    }

    // ---- mutations (dispatched as replayable actions) --------------------
    // Every document-mutating edit dispatches a `VideoEditorAction` through the
    // framework's `ActionHost`: it brackets the undo boundary, applies the
    // action through the executor (so it replays byte-identically on peers), and
    // remembers it for the covenant test. There is no per-edit `needs_commit`
    // flag any more — the host records exactly one undo boundary per event.

    /// Dispatch an optimistic field patch for an existing durable clip. New
    /// clips still need their complete first write; subsequent UI edits never
    /// carry an old snapshot of unrelated fields.
    fn dispatch_clip(&mut self, doc: &mut AutoCommit, id: ClipId) {
        if let Some(c) = self.project.clip(id).cloned() {
            if VideoDoc::load_project(doc).clip(id).is_some() {
                self.dispatch_existing_clips(doc, &[id]);
            } else {
                self.actions
                    .dispatch(doc, VIDEO_EDITOR_ACTION_TYPE, &upsert_clip_action(&c));
            }
        }
    }

    /// Commit several already-persisted clips as one all-or-nothing optimistic
    /// patch. This is the membership path for link/unlink as well as the
    /// ordinary linked geometry edits: every precondition is evaluated against
    /// the same document projection before the action writes its first key.
    fn dispatch_existing_clips(&mut self, doc: &mut AutoCommit, ids: &[ClipId]) {
        let mut ids = ids.to_vec();
        ids.sort_unstable();
        ids.dedup();
        let current = VideoDoc::load_project(doc);
        let mut patches = Vec::new();
        for id in &ids {
            let (Some(before), Some(after)) =
                (self.clip_edit_bases.get(id), self.project.clip(*id))
            else {
                // Existing edits must have captured their local starting
                // point. Without it, comparing the edited local model to the
                // current CRDT could recreate a collaborator's unrelated
                // change as a reverse patch.
                return;
            };
            if current.clip(*id).is_none() {
                return;
            }
            let VideoEditorAction::PatchClipFields { clip, changes } =
                clip_field_patch_action(before, after)
            else {
                return;
            };
            if !changes.is_empty() {
                patches.push(crate::actions::ClipPatchData { clip, changes });
            }
        }
        if !patches.is_empty() {
            self.actions.dispatch(
                doc,
                VIDEO_EDITOR_ACTION_TYPE,
                &VideoEditorAction::PatchClipsFields { patches },
            );
        }
        // Whether the edit changed a field or was a harmless no-op, this
        // authoring attempt is over. A later UI gesture gets a fresh baseline.
        for id in ids {
            self.clip_edit_bases.remove(&id);
        }
    }

    /// Record a clip only once per local interaction. Drag frames all use the
    /// same baseline, so release emits one patch from press to final position.
    fn remember_clip_before(&mut self, id: ClipId) {
        if !self.clip_edit_bases.contains_key(&id)
            && let Some(clip) = self.project.clip(id).cloned()
        {
            self.clip_edit_bases.insert(id, clip);
        }
    }

    /// Write the locally changed members of `id`'s link group together. The
    /// baseline map is the authoritative changed-member set: a locked follower
    /// is intentionally not edited, so it must not abandon editable members
    /// merely because it has no local before snapshot.
    fn dispatch_clip_linked(&mut self, doc: &mut AutoCommit, id: ClipId) {
        let group: Vec<_> = self
            .project
            .link_group(id)
            .into_iter()
            .filter(|member| self.clip_edit_bases.contains_key(member))
            .collect();
        self.dispatch_existing_clips(doc, &group);
    }

    // ---- selection ------------------------------------------------------

    /// The primary (anchor) selected clip — what the inspector edits.
    fn selected(&self) -> Option<ClipId> {
        self.selection.primary()
    }

    /// The selected clips that may actually be edited, in a deterministic
    /// order (by start time, then id) so a multi-clip edit dispatches the same
    /// sequence of actions on every peer. Clips on locked tracks are filtered
    /// out here — that is the single choke point every timeline edit passes
    /// through, so a lock cannot be routed around by a shortcut.
    fn editable_selection(&self) -> Vec<ClipId> {
        let mut v: Vec<(f64, ClipId)> = self
            .selection
            .ids()
            .iter()
            .filter(|id| self.project.clip_editable(**id))
            .filter_map(|id| self.project.clip(*id).map(|c| (c.start, c.id)))
            .collect();
        v.sort_by(|a, b| {
            a.0.partial_cmp(&b.0)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| a.1.cmp(&b.1))
        });
        v.into_iter().map(|(_, id)| id).collect()
    }

    /// The clips an edit actually acts on: the editable selection **plus**
    /// every clip linked to one of them, in the same settled order.
    ///
    /// This is the one place the link is applied to a bulk edit. Move, nudge,
    /// delete, ripple-delete and duplicate all read the selection through here
    /// rather than growing a parallel "…and the linked one" branch each, which
    /// is how the picture and the sound would end up following different
    /// rules. A clip whose track is locked is filtered out here too, so a link
    /// cannot be used to route an edit around a lock.
    fn edit_targets(&self) -> Vec<ClipId> {
        let mut ids = self.editable_selection();
        for id in self.selection.ids() {
            for partner in self.project.link_partners(*id) {
                if !ids.contains(&partner) && self.project.clip_editable(partner) {
                    ids.push(partner);
                }
            }
        }
        let mut v: Vec<(f64, ClipId)> = ids
            .into_iter()
            .filter_map(|id| self.project.clip(id).map(|c| (c.start, c.id)))
            .collect();
        v.sort_by(|a, b| {
            a.0.partial_cmp(&b.0)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| a.1.cmp(&b.1))
        });
        v.into_iter().map(|(_, id)| id).collect()
    }

    /// True when the selection holds at least one clip that a lock forbids
    /// editing — the cue the timeline shows instead of silently doing nothing.
    fn selection_has_locked(&self) -> bool {
        self.selection
            .ids()
            .iter()
            .any(|id| !self.project.clip_editable(*id))
    }

    /// Select every clip on the timeline (Ctrl+A), in display order.
    fn select_all(&mut self) {
        let ids: Vec<ClipId> = self
            .project
            .display_tracks()
            .iter()
            .flat_map(|t| self.project.clips_on(t.id))
            .map(|c| c.id)
            .collect();
        self.selection.set(ids);
    }

    /// Split every editable selected clip at the playhead. Only clips whose
    /// span actually covers the playhead split; the rest are left alone. All
    /// the splits dispatch inside one event, so they collapse into a single
    /// undo boundary (see [`ui::ActionHost::dispatch`]).
    fn split_selected(&mut self, doc: &mut AutoCommit) {
        let mut right_halves = Vec::new();
        // Cut a linked shot on both its lanes as one edit, so the two tails
        // become a link group of their own rather than joining the heads' —
        // four clips in one group would make dragging the second half of the
        // shot drag the first half with it.
        for group in self.target_groups() {
            for (id, rid) in self.project.split_group(&group, self.playhead) {
                let left = self.project.clip(id).cloned();
                let right = self.project.clip(rid).cloned();
                if let (Some(left), Some(right)) = (left, right) {
                    self.actions.dispatch(
                        doc,
                        VIDEO_EDITOR_ACTION_TYPE,
                        &split_clip_action(&left, &right),
                    );
                }
                right_halves.push(rid);
            }
        }
        // The right-hand halves become the selection, matching the
        // single-clip behaviour (S leaves you on the tail).
        if !right_halves.is_empty() {
            self.selection.set(right_halves);
        }
    }

    /// Lift every editable selected clip, leaving the gaps where they were.
    fn delete_selected(&mut self, doc: &mut AutoCommit) {
        let ids = self.edit_targets();
        if ids.is_empty() {
            return;
        }
        for id in ids {
            self.project.delete_clip(id);
            self.actions
                .dispatch(doc, VIDEO_EDITOR_ACTION_TYPE, &delete_clip_action(&id));
        }
        self.selection.clear();
    }

    /// Delete the selected clips and close the gaps, shifting later clips on
    /// the same track left. Each ripple — the delete plus every shifted clip —
    /// is carried in one action, and the whole selection's worth of ripples
    /// dispatches inside one event, so it lands as a single undo boundary and
    /// replays atomically on a peer. Deleting right-to-left keeps each
    /// remaining clip's start valid while the earlier ones are still there.
    fn ripple_delete_selected(&mut self, doc: &mut AutoCommit) {
        let mut ids = self.edit_targets();
        if ids.is_empty() {
            return;
        }
        ids.reverse();
        for id in ids {
            let moved_ids = self.project.ripple_delete(id);
            let moved: Vec<Clip> = moved_ids
                .iter()
                .filter_map(|m| self.project.clip(*m).cloned())
                .collect();
            self.actions.dispatch(
                doc,
                VIDEO_EDITOR_ACTION_TYPE,
                &ripple_delete_action(&id, &moved),
            );
        }
        self.selection.clear();
    }

    /// Copy the whole selection to the clipboard (no CRDT change). A locked
    /// track only forbids *edits*, so its clips can still be copied.
    fn copy_selected(&mut self) {
        // The whole shot, not half of it. A lock forbids *edits*, not copies,
        // so this follows the link with no editable filter — and pasting a
        // picture whose sound stayed behind would be a silent clip with no
        // explanation.
        let mut ids: Vec<ClipId> = self.selection.ids().to_vec();
        for id in self.selection.ids() {
            for partner in self.project.link_partners(*id) {
                if !ids.contains(&partner) {
                    ids.push(partner);
                }
            }
        }
        let mut clips: Vec<Clip> = ids
            .iter()
            .filter_map(|id| self.project.clip(*id).cloned())
            .collect();
        clips.sort_by(|a, b| {
            a.start
                .partial_cmp(&b.start)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        if !clips.is_empty() {
            self.clipboard = clips;
        }
    }

    /// Ctrl+X: copy the selection, then lift it. Both halves happen in one
    /// event, so the removal is one undo boundary.
    fn cut_selected(&mut self, doc: &mut AutoCommit) {
        self.copy_selected();
        self.delete_selected(doc);
    }

    /// Paste the clipboard at the playhead. The earliest copied clip lands on
    /// the playhead and the rest keep their relative offsets, so a copied
    /// block pastes as a block. Each clip goes to a *sensible* track: the one
    /// it came from when that still exists and is unlocked, otherwise the
    /// selected clip's track, otherwise any unlocked track of the right kind
    /// (created if the project has none). The pasted clips become the
    /// selection.
    fn paste_clip(&mut self, doc: &mut AutoCommit) {
        let src = self.clipboard.clone();
        if src.is_empty() {
            return;
        }
        let base = src.iter().map(|c| c.start).fold(f64::INFINITY, f64::min);
        let near = self
            .selected()
            .and_then(|s| self.project.clip(s))
            .map(|c| c.track);
        let mut pasted = Vec::new();
        let mut from: Vec<(Option<Uuid>, ClipId)> = Vec::new();
        for clip in src {
            let kind = self
                .project
                .media(clip.media)
                .map(|m| {
                    if m.kind.is_visual() {
                        TrackKind::Video
                    } else {
                        TrackKind::Audio
                    }
                })
                .unwrap_or(TrackKind::Video);
            // The clip's own track first (that is where the user copied it
            // from), then the current selection's track, then any unlocked
            // track of the right kind — creating one only as a last resort.
            let track = self
                .project
                .placement_track(kind, Some(clip.track))
                .or_else(|| self.project.placement_track(kind, near))
                .unwrap_or_else(|| self.ensure_track_of_kind(doc, kind));
            let start = snap_to_frame(
                (self.playhead + (clip.start - base)).max(0.0),
                self.project.fps,
            );
            let was = clip.link;
            let id = self.project.paste_clip(clip, track, start);
            from.push((was, id));
            pasted.push(id);
        }
        self.relink_copies(doc, &from);
        if !pasted.is_empty() {
            self.selection.set(pasted);
        }
    }

    /// Ctrl+D: drop a copy of the selection immediately after it, on the same
    /// tracks, and select the copies. One event → one undo boundary.
    fn duplicate_selected(&mut self, doc: &mut AutoCommit) {
        let ids = self.edit_targets();
        if ids.is_empty() {
            return;
        }
        let clips: Vec<Clip> = ids
            .iter()
            .filter_map(|id| self.project.clip(*id).cloned())
            .collect();
        // Offset the whole block by its own span so the copies butt up
        // against the originals instead of overlapping them.
        let lo = clips.iter().map(|c| c.start).fold(f64::INFINITY, f64::min);
        let hi = clips.iter().map(|c| c.end()).fold(0.0_f64, f64::max);
        let offset = (hi - lo).max(crate::model::MIN_CLIP);
        let mut copies = Vec::new();
        let mut from: Vec<(Option<Uuid>, ClipId)> = Vec::new();
        for clip in clips {
            let track = clip.track;
            let was = clip.link;
            let start = snap_to_frame(clip.start + offset, self.project.fps);
            let id = self.project.paste_clip(clip, track, start);
            from.push((was, id));
            copies.push(id);
        }
        self.relink_copies(doc, &from);
        self.selection.set(copies);
    }

    /// The clips an edit acts on, gathered into link groups: one entry per
    /// shot rather than one per clip, so an operation that has to treat a
    /// linked pair as a unit (a split, whose two tails become a group of their
    /// own) sees it as one.
    ///
    /// Only editable clips are ever in a group here, because they come out of
    /// [`Self::edit_targets`] — a locked lane does not follow the link.
    fn target_groups(&self) -> Vec<Vec<ClipId>> {
        let targets = self.edit_targets();
        let mut groups: Vec<Vec<ClipId>> = Vec::new();
        for id in &targets {
            if groups.iter().any(|g| g.contains(id)) {
                continue;
            }
            let mut g: Vec<ClipId> = self
                .project
                .link_group(*id)
                .into_iter()
                .filter(|c| targets.contains(c))
                .collect();
            if g.is_empty() {
                g.push(*id);
            }
            groups.push(g);
        }
        groups
    }

    /// Re-link a freshly made block of copies the way the originals were
    /// linked, and write every one of them.
    ///
    /// `from` is `(the source clip's link group, the copy's id)`. Each source
    /// group becomes one **fresh** group among the copies: duplicating a shot
    /// has to give back a shot whose picture and sound still move together,
    /// without tying either of them to the shot they were copied from.
    fn relink_copies(&mut self, doc: &mut AutoCommit, from: &[(Option<Uuid>, ClipId)]) {
        let mut seen: Vec<Uuid> = Vec::new();
        for (link, _) in from {
            if let Some(l) = link
                && !seen.contains(l)
            {
                seen.push(*l);
            }
        }
        for link in seen {
            let members: Vec<ClipId> = from
                .iter()
                .filter(|(l, _)| *l == Some(link))
                .map(|(_, id)| *id)
                .collect();
            self.project.link_clips(&members);
        }
        for (_, id) in from {
            self.dispatch_clip(doc, *id);
        }
    }

    // ---- linked audio and video -----------------------------------------

    /// The clips that follow an edit made on `id`: everything linked to it
    /// that its own track lock allows editing. Empty for an unlinked clip, so
    /// every caller reads as "and whatever follows", with no special case.
    fn linked_followers(&self, id: ClipId) -> Vec<ClipId> {
        self.project
            .link_partners(id)
            .into_iter()
            .filter(|c| self.project.clip_editable(*c))
            .collect()
    }

    /// `id` together with [`Self::linked_followers`] — the set a whole-group
    /// edit (a re-time) writes.
    fn linked_editable(&self, id: ClipId) -> Vec<ClipId> {
        let mut v = vec![id];
        v.extend(self.linked_followers(id));
        v
    }

    /// Give `video_clip` its linked audio half, when the picture it places
    /// carries sound and does not have one yet.
    ///
    /// This is what makes a placed video file into two clips: the picture on a
    /// video lane and its sound on an audio lane (created if the project has
    /// none, exactly as an audio import already does), sharing one link. Both
    /// are written inside the caller's event, so the pair lands on one undo
    /// boundary. Returns the audio clip's id, or `None` when there is nothing
    /// to split off.
    fn attach_linked_audio(&mut self, doc: &mut AutoCommit, video_clip: ClipId) -> Option<ClipId> {
        let clip = self.project.clip(video_clip)?;
        let (media_id, track_id) = (clip.media, clip.track);
        let media = self.project.media(media_id)?;
        if !media.kind.is_visual() || !media.has_audio {
            return None;
        }
        if self.project.track(track_id).map(|t| t.kind) != Some(TrackKind::Video) {
            return None;
        }
        // Already has a sound half — linking a second one would be heard twice
        // the moment the mixer stopped suppressing the picture clip's own.
        if !self.project.link_partners(video_clip).is_empty() {
            return None;
        }
        let audio_track = self
            .project
            .placement_track(TrackKind::Audio, None)
            .unwrap_or_else(|| self.ensure_track_of_kind(doc, TrackKind::Audio));
        self.remember_clip_before(video_clip);
        let audio = self.project.add_linked_audio(video_clip, audio_track)?;
        // The picture is already durable, so only its newly assigned group
        // membership is patched. The audio half is genuinely new and gets its
        // one allowed complete first write below.
        self.dispatch_existing_clips(doc, &[video_clip]);
        self.dispatch_clip(doc, audio);
        Some(audio)
    }

    /// Break the link on every clip in the selection's groups, leaving two
    /// ordinary independent clips. Writes exactly the clips that changed.
    fn unlink_selected(&mut self, doc: &mut AutoCommit) {
        let ids = self.edit_targets();
        for id in &ids {
            self.remember_clip_before(*id);
        }
        let changed = self.project.unlink_clips(&ids);
        self.dispatch_existing_clips(doc, &changed);
    }

    /// Put every editable selected clip into one fresh link group — the way
    /// back after an unlink, and the way to tie a sound to a picture it was
    /// never imported with.
    fn link_selected(&mut self, doc: &mut AutoCommit) {
        let mut ids = self.editable_selection();
        // Re-linking a shot to the sound it was split from: the sound half is
        // not selected — the user picked the picture and asked for the link
        // back — so bring its own detached partner along. Without this,
        // `link_clips` refuses a group of one and the button does nothing.
        if ids.len() == 1
            && let Some(id) = ids.first().copied()
            && let Some(sound) = self.project.detached_sound(id)
        {
            ids.push(sound);
        }
        for id in &ids {
            self.remember_clip_before(*id);
        }
        if self.project.link_clips(&ids).is_none() {
            return;
        }
        self.dispatch_existing_clips(doc, &ids);
    }

    /// Re-time the selected clip and everything linked to it, and write them
    /// all. The one door every rate affordance goes through — the slider, the
    /// presets, the reverse toggle and the keyboard — so none of them can
    /// leave half a shot at the old speed.
    fn set_speed_selected(&mut self, doc: &mut AutoCommit, rate: f64, reverse: bool) {
        let Some(id) = self.selected() else { return };
        if !self.project.clip_editable(id) {
            return;
        }
        let ids = self.linked_editable(id);
        for id in &ids {
            self.remember_clip_before(*id);
        }
        self.project.set_speed_group(&ids, rate, reverse);
        self.dispatch_clip_linked(doc, id);
    }

    /// What the link control offers for the current selection, or `None` when
    /// there is nothing to offer: one clip, unlinked, with nothing to link to.
    ///
    /// One door, so the inspector row, the context menu, the keyboard walk and
    /// the accessibility tree cannot disagree about whether linking is
    /// available or about which way round it is.
    fn link_affordance(&self) -> Option<LinkAffordance> {
        let ids = self.editable_selection();
        if ids.iter().any(|id| self.project.is_linked(*id)) {
            return Some(LinkAffordance::Unlink);
        }
        if ids.len() > 1 {
            return Some(LinkAffordance::Link);
        }
        // One picture clip whose sound is already on the timeline, unlinked.
        // Offering the split here would make a second copy of a sound clip
        // the user can see; offering to tie them back together is what is
        // actually true, and it is the only way back without a manual
        // multi-select.
        if let Some(id) = ids.first()
            && self.project.detached_sound(*id).is_some()
        {
            return Some(LinkAffordance::Link);
        }
        // One picture clip still carrying its own sound: offer to move that
        // sound onto a clip of its own. Without this, a project made before
        // links existed could never get one, and a pair whose sound half was
        // deleted could never get it back.
        ids.first()
            .filter(|id| self.project.can_split_audio(**id))
            .map(|_| LinkAffordance::SplitAudio)
    }

    /// Do whichever of link / unlink the selection currently offers.
    fn toggle_link_selected(&mut self, doc: &mut AutoCommit) {
        match self.link_affordance() {
            Some(LinkAffordance::Unlink) => self.unlink_selected(doc),
            Some(LinkAffordance::Link) => self.link_selected(doc),
            Some(LinkAffordance::SplitAudio) => {
                if let Some(id) = self.selected() {
                    self.attach_linked_audio(doc, id);
                }
            }
            None => {}
        }
    }

    /// Set the in-point of the primary selected clip to the playhead (`I`):
    /// trims the head so the clip starts at the current frame. A single-clip
    /// trim by nature — it acts on the anchor, and only when its track allows
    /// edits.
    fn set_in_at_playhead(&mut self, doc: &mut AutoCommit) {
        let Some(id) = self.selected() else { return };
        if !self.project.clip_editable(id) {
            return;
        }
        let delta = self
            .project
            .clip(id)
            .map(|c| self.playhead - c.start)
            .unwrap_or(0.0);
        if delta.abs() < 1e-6 {
            return;
        }
        let also = self.linked_followers(id);
        self.remember_clip_before(id);
        for linked in &also {
            self.remember_clip_before(*linked);
        }
        self.project.trim_in_with(id, delta, &also);
        self.dispatch_clip_linked(doc, id);
    }

    /// Set the out-point of the primary selected clip to the playhead (`O`):
    /// trims the tail so the clip ends at the current frame.
    fn set_out_at_playhead(&mut self, doc: &mut AutoCommit) {
        let Some(id) = self.selected() else { return };
        if !self.project.clip_editable(id) {
            return;
        }
        let delta = self
            .project
            .clip(id)
            .map(|c| self.playhead - c.end())
            .unwrap_or(0.0);
        if delta.abs() < 1e-6 {
            return;
        }
        let also = self.linked_followers(id);
        self.remember_clip_before(id);
        for linked in &also {
            self.remember_clip_before(*linked);
        }
        self.project.trim_out_with(id, delta, &also);
        self.dispatch_clip_linked(doc, id);
    }

    /// Nudge every editable selected clip by `dt` seconds (arrow keys).
    /// Snaps each result to the frame grid. One event → one undo boundary,
    /// so a multi-clip nudge is a single Ctrl+Z.
    fn nudge_selected(&mut self, doc: &mut AutoCommit, dt: f64) {
        let ids = self.edit_targets();
        for id in &ids {
            self.remember_clip_before(*id);
            let new_start = self
                .project
                .clip(*id)
                .map(|c| snap_to_frame((c.start + dt).max(0.0), self.project.fps))
                .unwrap_or(0.0);
            self.project.move_clip(*id, new_start);
        }
        self.dispatch_existing_clips(doc, &ids);
    }

    /// Apply `edit` to the selected clip and dispatch an optimistic narrow
    /// patch. The single write-through path for a *discrete* per-clip field edit (a
    /// transition / palette click, or a keyboard-driven grade) — each is one
    /// undo step. Continuous slider drags use [`Self::preview_edit`] +
    /// [`Self::commit_drag`] instead, so a drag is one action, not one a frame.
    fn edit_selected(&mut self, doc: &mut AutoCommit, edit: impl FnOnce(&mut Clip)) {
        let Some(id) = self.selected() else { return };
        if !self.project.clip_editable(id) {
            return;
        }
        self.remember_clip_before(id);
        if let Some(c) = self.project.clip_mut(id) {
            edit(c);
        }
        self.dispatch_clip(doc, id);
    }

    /// Apply `edit` to the selected clip **in memory only** — the live drag
    /// preview. No action is dispatched; the final value is committed once when
    /// the drag ends (see [`Self::commit_drag`]).
    fn preview_edit(&mut self, edit: impl FnOnce(&mut Clip)) {
        let Some(id) = self.selected() else { return };
        if !self.project.clip_editable(id) {
            return;
        }
        self.remember_clip_before(id);
        if let Some(c) = self.project.clip_mut(id) {
            edit(c);
        }
    }

    /// The media item behind the selected clip, if any.
    fn selected_media_item(&self) -> Option<&MediaItem> {
        let id = self.selected()?;
        let clip = self.project.clip(id)?;
        self.project.media(clip.media)
    }

    /// Apply `edit` to the media item behind the selected clip, in memory
    /// only — the live preview half of a title-property drag. The two title
    /// knobs edit the *source*, not the clip, so they cannot go through
    /// [`Self::preview_edit`] and commit a different action on release.
    fn preview_media_edit(&mut self, edit: impl FnOnce(&mut MediaItem)) {
        let Some(id) = self.selected() else { return };
        if !self.project.clip_editable(id) {
            return;
        }
        let Some(mid) = self.project.clip(id).map(|c| c.media) else {
            return;
        };
        if let Some(m) = self.project.media.iter_mut().find(|m| m.id == mid) {
            edit(m);
        }
    }

    /// The lane the selected clip sits on — what the MIX section edits.
    fn selected_track(&self) -> Option<&crate::model::Track> {
        let clip = self.selected_clip()?;
        self.project.track(clip.track)
    }

    fn selected_track_id(&self) -> Option<TrackId> {
        self.selected_clip().map(|c| c.track)
    }

    /// Apply `edit` to that lane **in memory only** — the live half of a
    /// fader drag, committed once on release exactly as a clip drag is.
    ///
    /// A locked lane refuses, matching every other edit: the hatch across it
    /// says nothing here will take, and a fader that moved anyway would make
    /// that a lie.
    fn preview_track_edit(&mut self, edit: impl FnOnce(&mut crate::model::Track)) {
        let Some(tid) = self.selected_track_id() else {
            return;
        };
        if self.project.track_locked(tid) {
            return;
        }
        if let Some(t) = self.project.tracks.iter_mut().find(|t| t.id == tid) {
            edit(t);
        }
    }

    /// Dispatch a whole-track write for the lane the selection sits on.
    fn dispatch_selected_track(&mut self, doc: &mut AutoCommit) {
        let Some(t) = self.selected_track().cloned() else {
            return;
        };
        self.actions
            .dispatch(doc, VIDEO_EDITOR_ACTION_TYPE, &upsert_track_action(&t));
    }

    /// Put the selected clip's lane back to unity and centre, as one action
    /// and one undo boundary.
    fn reset_selected_track_mix(&mut self, doc: &mut AutoCommit) {
        self.preview_track_edit(|t| {
            t.gain_db = 0.0;
            t.pan = 0.0;
        });
        self.dispatch_selected_track(doc);
    }

    /// Dispatch a whole-media write for one bin source.
    fn dispatch_media(&mut self, doc: &mut AutoCommit, mid: MediaId) {
        if let Some(m) = self.project.media(mid).cloned() {
            self.actions
                .dispatch(doc, VIDEO_EDITOR_ACTION_TYPE, &upsert_media_action(&m));
        }
    }

    /// Dispatch a whole-media write for the selected clip's source.
    fn dispatch_selected_media(&mut self, doc: &mut AutoCommit) {
        if let Some(m) = self.selected_media_item().cloned() {
            self.actions
                .dispatch(doc, VIDEO_EDITOR_ACTION_TYPE, &upsert_media_action(&m));
        }
    }

    /// Apply `edit` to the selected clip's title styling and dispatch it as
    /// one media write. A no-op on a clip whose source is not a title.
    fn edit_title(&mut self, doc: &mut AutoCommit, edit: impl FnOnce(&mut TextSource)) {
        let has_text = self
            .selected_media_item()
            .map(|m| m.text.is_some())
            .unwrap_or(false);
        if !has_text {
            return;
        }
        self.preview_media_edit(|m| {
            let mut t = m.text.clone().unwrap_or_default();
            edit(&mut t);
            // The bin row names a title by its first line, so keep the two in
            // step rather than leaving a renamed title labelled by old words.
            m.name = t
                .content
                .lines()
                .next()
                .unwrap_or("Title")
                .trim()
                .to_string();
            if m.name.is_empty() {
                m.name = "Title".into();
            }
            m.text = Some(t);
        });
        self.dispatch_selected_media(doc);
    }

    /// What a ranged control currently shows. `None` when the control has
    /// nothing behind it — a title knob with no title selected.
    ///
    /// Animatable channels read through [`Clip::param_at`], the same door the
    /// compositor uses, so a keyframed slider tracks the playhead instead of
    /// showing a constant the picture is ignoring.
    /// The span a control has **on the current clip**, for the controls whose
    /// range is not a constant. `None` for every other field, which reads its
    /// span off [`NumField::range`].
    fn field_span(&self, f: NumField) -> Option<(f64, f64)> {
        let clip = self.selected_clip()?;
        match f {
            NumField::FadeIn | NumField::FadeOut => Some((0.0, clip.duration())),
            NumField::TransitionLen => Some((0.01, self.transition_len_max())),
            _ => None,
        }
    }

    fn num_value(&self, f: NumField) -> Option<f64> {
        // The two lane knobs read the track, not the clip, and they read it
        // first: `param()` is `None` for both and the clip has no field to
        // fall through to.
        if f.is_track() {
            let t = self.selected_track()?;
            return Some(match f {
                NumField::TrackPan => t.pan,
                _ => t.gain_db,
            });
        }
        let clip = self.selected().and_then(|id| self.project.clip(id))?;
        if let Some(p) = f.param() {
            return Some(clip.param_at(p, self.playhead));
        }
        Some(match f {
            NumField::FadeIn => clip.fade_in,
            NumField::FadeOut => clip.fade_out,
            NumField::TransitionLen => clip.transition_len(),
            NumField::CropLeft => clip.transform.crop_left,
            NumField::CropRight => clip.transform.crop_right,
            NumField::CropTop => clip.transform.crop_top,
            NumField::CropBottom => clip.transform.crop_bottom,
            NumField::Speed => clip.speed.rate(),
            NumField::TitleSize => self.selected_media_item()?.text.as_ref()?.size,
            NumField::TitleBackdrop => self.selected_media_item()?.text.as_ref()?.backdrop,
            _ => return None,
        })
    }

    /// Write a ranged control's value in memory (the live drag preview).
    ///
    /// An animated parameter is **keyed** at the playhead rather than having
    /// its dead constant written: once a parameter has a curve the constant
    /// is never read again, so writing it would look like the slider had
    /// stopped working. This is also what makes a second keyframe cost one
    /// drag — key it, move the playhead, drag.
    /// Write an animatable parameter in memory: a key at the playhead when it
    /// is animated, its constant when it is not.
    ///
    /// Every parameter-backed slider goes through here, the original grade and
    /// level rows included. They used to write their constant field directly,
    /// which meant that once a parameter was keyed its slider silently stopped
    /// having any effect — the curve overrides the constant everywhere.
    fn preview_param(&mut self, param: Param, v: f64) {
        let at = self.playhead;
        self.preview_edit(move |c| c.set_param_at(param, at, v));
    }

    fn set_num(&mut self, f: NumField, v: f64) {
        if let Some(p) = f.param() {
            self.preview_param(p, v);
            return;
        }
        match f {
            NumField::TrackGain => {
                let db = v.clamp(MIN_GAIN_DB, MAX_GAIN_DB);
                self.preview_track_edit(move |t| t.gain_db = db);
                return;
            }
            NumField::TrackPan => {
                let pan = v.clamp(-1.0, 1.0);
                self.preview_track_edit(move |t| t.pan = pan);
                return;
            }
            _ => {}
        }
        let at = self.playhead;
        let _ = at;
        match f {
            // Clamped against the clip, exactly as the drag does: a fade
            // longer than the clip it is on is not a fade.
            NumField::FadeIn => self.preview_edit(move |c| c.fade_in = v.clamp(0.0, c.duration())),
            NumField::FadeOut => {
                self.preview_edit(move |c| c.fade_out = v.clamp(0.0, c.duration()));
            }
            NumField::TransitionLen => {
                self.preview_edit(move |c| c.transition_duration = v.max(0.01));
            }
            NumField::CropLeft => self.preview_edit(move |c| c.transform.crop_left = v),
            NumField::CropRight => self.preview_edit(move |c| c.transform.crop_right = v),
            NumField::CropTop => self.preview_edit(move |c| c.transform.crop_top = v),
            NumField::CropBottom => self.preview_edit(move |c| c.transform.crop_bottom = v),
            NumField::Speed => {
                // Rate is the one knob that follows the link, because it is
                // the one whose halves drift *further* apart the longer the
                // shot runs — a fault with no cause visible anywhere on
                // screen. Everything else here (a level, a fade, a grade) is
                // per-clip on purpose: sliding the sound and levelling it
                // separately is what the audio clip is for.
                let Some(id) = self.selected() else { return };
                if !self.project.clip_editable(id) {
                    return;
                }
                let rev = self
                    .project
                    .clip(id)
                    .map(|c| c.speed.reverse)
                    .unwrap_or(false);
                let ids = self.linked_editable(id);
                for linked in &ids {
                    self.remember_clip_before(*linked);
                }
                self.project.set_speed_group(&ids, v, rev);
            }
            NumField::TitleSize => self.preview_media_edit(move |m| {
                if let Some(t) = m.text.as_mut() {
                    t.size = v;
                }
            }),
            NumField::TitleBackdrop => self.preview_media_edit(move |m| {
                if let Some(t) = m.text.as_mut() {
                    t.backdrop = v;
                }
            }),
            _ => {}
        }
    }

    /// The three-state reading a key control shows for `param` on the
    /// selection right now.
    fn key_state(&self, param: Param) -> KeyState {
        let Some(clip) = self.selected().and_then(|id| self.project.clip(id)) else {
            return KeyState::Off;
        };
        if !clip.anim.is_animated(param) {
            KeyState::Off
        } else if clip.has_key_at(param, self.playhead) {
            KeyState::OnKey
        } else {
            KeyState::Between
        }
    }

    /// Plant a key for `param` at the playhead, holding whatever the
    /// parameter reads there. The first key never moves the picture — it is
    /// the *second* one that makes anything animate — so keying is safe to
    /// try, which is what makes the control learnable.
    fn key_here(&mut self, doc: &mut AutoCommit, param: Param) {
        let at = self.playhead;
        self.edit_selected(doc, move |c| {
            let now = c.param_at(param, at);
            let interp = c.key_interp_at(param, at).unwrap_or_default();
            c.set_key_at(param, at, now, interp);
        });
    }

    /// Open the keyframe menu for `param`. What it offers depends on whether
    /// a key sits on the playhead: with one there the menu shapes or removes
    /// it, without one it plants one. Both ways it can step to the
    /// neighbouring keys and clear the whole curve.
    fn open_key_menu(&mut self, param: Param, at: Point) {
        let clip = self
            .selected()
            .and_then(|id| self.project.clip(id))
            .cloned();
        let Some(clip) = clip else { return };
        let on_key = clip.has_key_at(param, self.playhead);
        let interp = clip.key_interp_at(param, self.playhead).unwrap_or_default();
        let mut menu = ContextMenu::new();
        if on_key {
            menu.push(MenuItem::action("Remove key here", || {}));
            menu.push(MenuItem::separator());
            for i in Interp::ALL {
                // The shape in force is ticked, so the menu reports the key's
                // state as well as offering to change it.
                let mark = if i == interp { "✓ " } else { "    " };
                menu.push(MenuItem::action(format!("{mark}{}", i.label()), || {}));
            }
        } else {
            menu.push(MenuItem::action("Add key here", || {}));
        }
        menu.push(MenuItem::separator());
        menu.push(MenuItem::action("Previous key", || {}));
        menu.push(MenuItem::action("Next key", || {}));
        menu.push(MenuItem::separator());
        menu.push(MenuItem::action("Remove all keys", || {}));
        menu.open_at([at.x as f32, at.y as f32]);
        self.bin_menu = menu;
        self.key_menu_param = Some(param);
        self.menu_target = Some(MenuTarget::Keyframe(param));
    }

    /// Act on a keyframe-menu choice. The index scheme mirrors what
    /// [`Self::open_key_menu`] built for this playhead position.
    /// Indices count separators, because that is what `ContextMenu::item_at`
    /// reports. The two shapes the menu takes are laid out here beside the
    /// builder that produces them so they cannot drift apart.
    fn key_menu_choose(&mut self, doc: &mut AutoCommit, param: Param, idx: usize) {
        self.key_menu_param = None;
        let at = self.playhead;
        let on_key = self
            .selected()
            .and_then(|id| self.project.clip(id))
            .map(|c| c.has_key_at(param, at))
            .unwrap_or(false);
        if on_key {
            // 0 remove · 1 sep · 2..4 shapes · 5 sep · 6 prev · 7 next · 8 sep · 9 clear
            match idx {
                0 => self.edit_selected(doc, move |c| {
                    c.remove_key_at(param, at);
                }),
                2..=4 => {
                    let Some(&shape) = Interp::ALL.get(idx - 2) else {
                        return;
                    };
                    self.edit_selected(doc, move |c| {
                        c.set_interp_at(param, at, shape);
                    })
                }
                6 => self.jump_to_key(false),
                7 => self.jump_to_key(true),
                9 => self.edit_selected(doc, move |c| c.anim.clear(param)),
                _ => {}
            }
        } else {
            // 0 add · 1 sep · 2 prev · 3 next · 4 sep · 5 clear
            match idx {
                0 => self.key_here(doc, param),
                2 => self.jump_to_key(false),
                3 => self.jump_to_key(true),
                5 => self.edit_selected(doc, move |c| c.anim.clear(param)),
                _ => {}
            }
        }
    }

    /// Move the playhead to the selected clip's next / previous keyframe.
    /// Presentation, so no action: the playhead is this reader's, not the
    /// document's.
    fn jump_to_key(&mut self, forward: bool) {
        let Some(clip) = self.selected().and_then(|id| self.project.clip(id)) else {
            return;
        };
        let to = if forward {
            clip.next_key_after(self.playhead)
        } else {
            clip.prev_key_before(self.playhead)
        };
        if let Some(t) = to {
            self.playhead = t.clamp(0.0, self.project.duration());
            self.playback.playing = false;
            self.shuttle = 0.0;
            self.playback.chase_playhead = true;
        }
    }

    /// Move the playhead to the next / previous marked instant, across the
    /// project's markers and every clip's alike.
    fn jump_to_marker(&mut self, forward: bool) {
        let to = if forward {
            self.project.next_marker_at(self.playhead)
        } else {
            self.project.prev_marker_at(self.playhead)
        };
        if let Some(t) = to {
            self.playhead = t.clamp(0.0, self.project.duration());
            self.playback.playing = false;
            self.shuttle = 0.0;
            self.playback.chase_playhead = true;
        }
    }

    /// Write the project's own settings as one action, mirroring them in
    /// memory the way every other local edit does: the document is the truth
    /// and `sync` will reload it, but the model must not disagree with it in
    /// between.
    fn dispatch_project_meta(&mut self, doc: &mut AutoCommit, next: Project) {
        let action = set_project_meta_action(&next);
        self.actions
            .dispatch(doc, VIDEO_EDITOR_ACTION_TYPE, &action);
        self.project = next;
    }

    /// Set the project's frame size — the same whole-settings write the
    /// `set-frame-size` action dispatches.
    fn set_project_frame_size(&mut self, doc: &mut AutoCommit, width: u32, height: u32) {
        let mut next = self.project.clone();
        next.set_frame_size(width, height);
        self.dispatch_project_meta(doc, next);
    }

    fn set_project_fps(&mut self, doc: &mut AutoCommit, fps: f64) {
        let mut next = self.project.clone();
        next.set_fps(fps);
        self.dispatch_project_meta(doc, next);
    }

    /// Take the shape of whatever [`Self::format_source`] names as the
    /// project's own — deliberately, so a later import cannot undo it.
    fn match_project_format(&mut self, doc: &mut AutoCommit) {
        let Some(m) = self.format_source().cloned() else {
            return;
        };
        let mut next = self.project.clone();
        next.set_frame_size(m.width, m.height);
        if m.fps > 0.0 {
            next.set_fps(m.fps);
        }
        self.dispatch_project_meta(doc, next);
    }

    /// Re-word the project's format if it has moved since the last frame.
    fn refresh_project_format_words(&mut self) {
        let now = (
            self.project.width,
            self.project.height,
            self.project.fps.to_bits(),
        );
        if now != self.project_format_from {
            self.project_format_from = now;
            self.project_format_shown =
                format_words(self.project.width, self.project.height, self.project.fps);
        }
    }

    /// Switch the chroma key on or off, opening the section so its sliders
    /// are on screen — the one edit both the enable button and an assistive
    /// technology's activation run, so the two cannot diverge.
    fn toggle_key_enabled(&mut self, doc: &mut AutoCommit) {
        self.edit_selected(doc, |c| {
            c.key.enabled = !c.key.enabled;
            // A key switched on with the tolerance wound to zero removes
            // nothing, which reads as the button not working; give it back a
            // tolerance that keys.
            if c.key.enabled && c.key.tolerance <= 1e-6 {
                c.key.tolerance = crate::model::ChromaKey::default().tolerance;
            }
        });
        self.collapsed.remove(&Section::Key);
    }

    /// Choose the mask's shape. Turning a mask on lands it over the picture
    /// rather than in the middle of an empty frame, so a masked
    /// picture-in-picture is one press.
    fn set_mask_shape(&mut self, doc: &mut AutoCommit, shape: crate::model::MaskShape) {
        let (src_aspect, frame_aspect) = self.selected_aspects();
        self.edit_selected(doc, move |c| {
            if shape == crate::model::MaskShape::None || c.mask.is_active() {
                c.mask.shape = shape;
            } else {
                c.mask = crate::model::Mask::over(shape, &c.transform, src_aspect, frame_aspect);
            }
        });
        self.collapsed.remove(&Section::Mask);
    }

    /// A blank project has no shape of its own, so the first clip to land on
    /// its timeline gives it one.
    ///
    /// Without this, shooting vertically meant a 1920×1080 project holding
    /// 1080×1920 footage, and — because the export scales its preset height by
    /// the *project's* aspect — an exported file letterboxed into 16:9 with no
    /// exit. A format that was set deliberately is never overridden, and
    /// neither is one whose timeline already has clips cut against it.
    fn adopt_project_format(&mut self, doc: &mut AutoCommit, media: MediaId) {
        if self.project.format_explicit || !self.project.clips.is_empty() {
            return;
        }
        let Some(m) = self.project.media(media).cloned() else {
            return;
        };
        let mut next = self.project.clone();
        if next.adopt_format(&m) {
            self.dispatch_project_meta(doc, next);
        }
    }

    /// Mark **in** at the playhead — the start of what a render covers.
    ///
    /// The clip trim on plain I / O is untouched: that trims the *selected
    /// clip*, which is a different thing from the span of the timeline that
    /// gets rendered, and the export loop ran the whole timeline
    /// unconditionally — so checking a ten-second fix meant re-rendering
    /// everything.
    fn mark_in_here(&mut self, doc: &mut AutoCommit) {
        let mut next = self.project.clone();
        next.set_mark_in(self.playhead);
        self.dispatch_project_meta(doc, next);
    }

    fn mark_out_here(&mut self, doc: &mut AutoCommit) {
        let mut next = self.project.clone();
        next.set_mark_out(self.playhead);
        self.dispatch_project_meta(doc, next);
    }

    /// Drop both marks, putting the render back to the whole timeline.
    fn clear_marks(&mut self, doc: &mut AutoCommit) {
        if !self.project.has_marks() {
            return;
        }
        let mut next = self.project.clone();
        next.clear_marks();
        self.dispatch_project_meta(doc, next);
    }

    /// Add a timeline marker at the playhead and open it for naming, so the
    /// marker is never left as an anonymous tick the user has to hunt for.
    fn add_marker_here(&mut self, doc: &mut AutoCommit) {
        let mut next = self.project.clone();
        let palette = (next.markers.len() as u8 % 6) + 1;
        let id = next.add_marker(self.playhead, "Marker", palette);
        let Some(marker) = next.marker(id).cloned() else {
            return;
        };
        self.project = next.clone();
        self.actions.dispatch(
            doc,
            VIDEO_EDITOR_ACTION_TYPE,
            &VideoEditorAction::AddProjectMarker { marker },
        );
        self.collapsed.remove(&Section::Markers);
        self.begin_marker_rename(MarkerRef {
            id,
            clip: None,
            at: self.playhead,
        });
    }

    fn begin_marker_rename(&mut self, m: MarkerRef) {
        self.begin_marker_edit(m, MarkerField::Name);
    }

    /// The label for "no family chosen" — the system's default sans, which
    /// is a real answer and the one a box with no fonts installed can honour.
    const DEFAULT_FONT_LABEL: &'static str = "System sans";

    /// Open the family chooser for the selected title.
    ///
    /// The list is whatever is installed, with the default at the head so
    /// there is always a way back to it. It is empty only before the first
    /// draw has filled the cache, and then the picker is not opened at all
    /// rather than opened over nothing.
    fn open_font_picker(&mut self) {
        let Some(m) = self.selected_media_item() else {
            return;
        };
        let (mid, current) = (
            m.id,
            m.text.as_ref().map(|t| t.font.clone()).unwrap_or_default(),
        );
        if self.system_fonts.is_empty() {
            return;
        }
        let mut options = vec![Self::DEFAULT_FONT_LABEL.to_string()];
        options.extend(self.system_fonts.iter().cloned());
        let shown = if current.trim().is_empty() {
            Self::DEFAULT_FONT_LABEL.to_string()
        } else {
            current
        };
        // The dropdown reads its current value out of the document it is
        // given, so the family in force is seeded there and synced in —
        // which is also what lands the highlight on it when the list opens.
        let mut doc = AutoCommit::new();
        let _ = automerge::transaction::Transactable::put(
            &mut doc,
            &automerge::ROOT,
            "font",
            shown.as_str(),
        );
        let mut dd = ui::widget::Dropdown::new(options, "font")
            .with_id("title_font")
            .open();
        dd.sync(&mut doc, &automerge::ROOT);
        self.font_picker = Some((mid, Box::new(dd), doc));
    }

    /// Take whatever the open picker settled on, if it has closed.
    ///
    /// Returns the family to write — an empty string for the default — so
    /// the caller commits it through the ordinary media action.
    fn take_font_choice(&mut self) -> Option<(MediaId, String)> {
        let (mid, dd, _) = self.font_picker.as_ref()?;
        if dd.is_open() {
            return None;
        }
        let chosen = dd.selected().to_string();
        let mid = *mid;
        self.font_picker = None;
        let family = if chosen == Self::DEFAULT_FONT_LABEL {
            String::new()
        } else {
            chosen
        };
        Some((mid, family))
    }

    /// Where the marker row's inline editor is placed: over the name on the
    /// first line, or over the note's line beneath it.
    ///
    /// A note edited in the name's box would look like a rename, and a row
    /// with no note yet has no second line — so the box is opened there,
    /// which is also where the note will be once it exists.
    fn marker_edit_rect(&self, row: Rect) -> Rect {
        let (name, _, _, _) = marker_row_parts(row);
        match self.marker_field {
            MarkerField::Name => name,
            MarkerField::Note => Rect::new(row.x0 + 16.0, row.y0 + 24.0, name.x1, row.y0 + 44.0),
        }
    }

    /// Open the inline editor on one of a marker's two texts.
    fn begin_marker_edit(&mut self, m: MarkerRef, field: MarkerField) {
        let current = self
            .project
            .all_markers()
            .into_iter()
            .find(|(_, mk, _)| mk.id == m.id)
            .map(|(_, mk, _)| match field {
                MarkerField::Name => mk.name.clone(),
                MarkerField::Note => mk.note.clone(),
            })
            .unwrap_or_default();
        let mut label = EditableLabel::new(&current)
            .with_font_size(12.0)
            .with_id("marker_rename");
        label.begin_edit();
        self.marker_field = field;
        self.marker_rename = Some((m, label));
    }

    /// Commit a marker rename through whichever action owns that marker: a
    /// project-meta write for a timeline marker, a clip-field patch for one
    /// pinned to a clip.
    /// Returns whether the write was accepted — an empty name is refused,
    /// and the caller has to say so rather than silently restoring the old
    /// one.
    fn commit_marker_rename(&mut self, doc: &mut AutoCommit, m: MarkerRef, text: &str) -> bool {
        let field = self.marker_field;
        self.commit_marker_text(doc, m, field, text)
    }

    /// Write one of a marker's texts through whichever action owns that
    /// marker: a project-meta write for a timeline marker, a clip-field patch
    /// for one pinned to a clip.
    ///
    /// An empty **name** is refused — a marker with no name is a dot nobody
    /// can find in the list — while an empty **note** is accepted, because
    /// clearing a note is the only way to take one back off.
    /// Returns whether the document changed, so a caller that has to report
    /// the outcome -- an assistive technology setting the text outright --
    /// can say "refused" rather than claim a write that never happened.
    fn commit_marker_text(
        &mut self,
        doc: &mut AutoCommit,
        m: MarkerRef,
        field: MarkerField,
        text: &str,
    ) -> bool {
        let text = text.trim().to_string();
        if field == MarkerField::Name && text.is_empty() {
            return false;
        }
        let write = |mk: &mut crate::model::Marker| -> bool {
            let slot = match field {
                MarkerField::Name => &mut mk.name,
                MarkerField::Note => &mut mk.note,
            };
            if *slot == text {
                return false;
            }
            *slot = text.clone();
            true
        };
        match m.clip {
            None => {
                let Some(expected) = self.project.marker(m.id).map(|mk| match field {
                    MarkerField::Name => mk.name.clone(),
                    MarkerField::Note => mk.note.clone(),
                }) else {
                    return false;
                };
                let mut next = self.project.clone();
                let Some(mk) = next.marker_mut(m.id) else {
                    return false;
                };
                if !write(mk) {
                    return false;
                }
                self.project = next.clone();
                self.actions.dispatch(
                    doc,
                    VIDEO_EDITOR_ACTION_TYPE,
                    &VideoEditorAction::SetProjectMarkerText {
                        marker: m.id.to_string(),
                        field: match field {
                            MarkerField::Name => MarkerTextField::Name,
                            MarkerField::Note => MarkerTextField::Note,
                        },
                        expected,
                        value: text,
                    },
                );
                true
            }
            Some(cid) => {
                let Some(expected) = self
                    .project
                    .clip(cid)
                    .and_then(|clip| clip.markers.iter().find(|mk| mk.id == m.id))
                    .map(|mk| match field {
                        MarkerField::Name => mk.name.clone(),
                        MarkerField::Note => mk.note.clone(),
                    })
                else {
                    return false;
                };
                self.remember_clip_before(cid);
                let changed = self
                    .project
                    .clip_mut(cid)
                    .and_then(|c| c.markers.iter_mut().find(|mk| mk.id == m.id).map(write))
                    .unwrap_or(false);
                if changed {
                    self.actions.dispatch(
                        doc,
                        VIDEO_EDITOR_ACTION_TYPE,
                        &VideoEditorAction::SetClipMarkerText {
                            clip: cid.to_string(),
                            marker: m.id.to_string(),
                            field: match field {
                                MarkerField::Name => MarkerTextField::Name,
                                MarkerField::Note => MarkerTextField::Note,
                            },
                            expected,
                            value: text,
                        },
                    );
                }
                changed
            }
        }
    }

    /// Park the playhead on an instant: stop whatever it was doing, clamp the
    /// position into the timeline, and pull the view along after it.
    ///
    /// The one door for "go to this moment". Clicking a marker row, the
    /// accessibility route onto that row and a chosen global-search hit all
    /// arrive here, so none of them can drift into a slightly different move
    /// — an earlier copy that only assigned `playhead` left a running
    /// transport running, so the moment you had just asked for slid away
    /// under you.
    fn seek_to(&mut self, at: f64) {
        self.playhead = at.clamp(0.0, self.project.duration());
        self.playback.playing = false;
        self.shuttle = 0.0;
        self.playback.chase_playhead = true;
    }

    /// Park the playhead on a marker -- what clicking a marker row's body
    /// does. Extracted so the accessibility route runs the identical move
    /// rather than a second copy of it that could drift.
    fn seek_to_marker(&mut self, m: MarkerRef) {
        self.seek_to(m.at);
    }

    fn delete_marker(&mut self, doc: &mut AutoCommit, m: MarkerRef) {
        match m.clip {
            None => {
                let mut next = self.project.clone();
                if !next.remove_marker(m.id) {
                    return;
                }
                self.project = next.clone();
                self.actions.dispatch(
                    doc,
                    VIDEO_EDITOR_ACTION_TYPE,
                    &VideoEditorAction::RemoveProjectMarker {
                        marker: m.id.to_string(),
                    },
                );
            }
            Some(cid) => {
                self.remember_clip_before(cid);
                let gone = self
                    .project
                    .clip_mut(cid)
                    .map(|c| c.remove_marker(m.id))
                    .unwrap_or(false);
                if gone {
                    self.actions.dispatch(
                        doc,
                        VIDEO_EDITOR_ACTION_TYPE,
                        &VideoEditorAction::RemoveClipMarker {
                            clip: cid.to_string(),
                            marker: m.id.to_string(),
                        },
                    );
                }
            }
        }
        if self.marker_rename.as_ref().map(|(r, _)| r.id) == Some(m.id) {
            self.marker_rename = None;
        }
    }

    /// Create a title, place it at the playhead, select it and open its words
    /// for typing.
    ///
    /// A title is the one source with no bytes: nothing to import, nothing to
    /// hydrate, and it renders on every peer immediately — so "add a title"
    /// can go all the way to an editable clip on the timeline in one step
    /// rather than leaving an unnamed item in the bin to be dragged out.
    fn add_title(&mut self, doc: &mut AutoCommit) {
        let palette = (self.project.media.len() as u8) % 7;
        let item = MediaItem {
            id: Uuid::new_v4(),
            name: "Title".into(),
            kind: MediaKind::Text,
            duration: 5.0,
            width: self.project.width,
            height: self.project.height,
            fps: self.project.fps,
            palette,
            path: String::new(),
            hydration: crate::media::Hydration::Local,
            text: Some(TextSource::default()),
            ..Default::default()
        };
        let mid = item.id;
        self.project.media.push(item.clone());
        self.actions
            .dispatch(doc, VIDEO_EDITOR_ACTION_TYPE, &upsert_media_action(&item));
        self.selected_media = Some(mid);
        if let Some(cid) = self.place_title_clip(doc, mid) {
            self.selection.select_only(cid);
            self.reveal_inspector();
        }
        self.collapsed.remove(&Section::Title);
        self.begin_title_edit(mid);
    }

    /// Place a fresh title clip at the playhead on the topmost video track
    /// that will take it, so a title lands *over* the picture rather than
    /// under it.
    fn place_title_clip(&mut self, doc: &mut AutoCommit, mid: MediaId) -> Option<ClipId> {
        let track = self
            .project
            .display_tracks()
            .into_iter()
            .find(|t| t.kind == TrackKind::Video && !t.locked)
            .map(|t| t.id)
            .unwrap_or_else(|| self.add_track(doc, TrackKind::Video));
        let at = snap_to_frame(self.playhead, self.project.fps);
        let cid = self.project.add_clip(track, mid, at)?;
        self.dispatch_clip(doc, cid);
        Some(cid)
    }

    /// Commit new words for a title: one media write, so the picture, the bin
    /// row and every clip that placed this title all follow from it.
    fn commit_title_text(&mut self, doc: &mut AutoCommit, mid: MediaId, words: &str) {
        let Some(m) = self.project.media(mid) else {
            return;
        };
        let mut next = m.clone();
        let mut t = next.text.clone().unwrap_or_default();
        if t.content == words {
            return;
        }
        t.content = words.to_string();
        next.name = words.lines().next().unwrap_or("Title").trim().to_string();
        if next.name.is_empty() {
            next.name = "Title".into();
        }
        next.text = Some(t);
        if let Some(slot) = self.project.media.iter_mut().find(|m| m.id == mid) {
            *slot = next.clone();
        }
        self.actions
            .dispatch(doc, VIDEO_EDITOR_ACTION_TYPE, &upsert_media_action(&next));
    }

    fn begin_title_edit(&mut self, mid: MediaId) {
        let content = self
            .project
            .media(mid)
            .and_then(|m| m.text.as_ref())
            .map(|t| t.content.clone())
            .unwrap_or_default();
        let mut field = ui::widget::TextInput::local()
            .multiline(true)
            .with_initial_value(&content)
            .with_font_size(13.0)
            .with_id("title_text");
        field.set_focus(true);
        self.title_edit = Some((mid, field));
    }

    /// Longest transition the selected clip can carry: its own length, since
    /// a ramp cannot outlast the footage it ramps into.
    fn transition_len_max(&self) -> f64 {
        self.selected()
            .and_then(|id| self.project.clip(id))
            .map(|c| c.duration())
            .unwrap_or(0.0)
            .max(0.01)
    }

    fn set_transition(&mut self, doc: &mut AutoCommit, t: Transition) {
        self.edit_selected(doc, move |c| c.transition = t);
    }
    fn add_effect_to_selected(&mut self, doc: &mut AutoCommit, kind: EffectKind) {
        let Some(id) = self.selected() else { return };
        let Some(clip) = self.project.clip(id) else {
            return;
        };
        let ordinal = clip.effects.len();
        let effect = crate::model::ClipEffect::new(kind);
        if let Some(clip) = self.project.clip_mut(id) {
            clip.effects.push(effect);
        }
        self.actions.dispatch(
            doc,
            VIDEO_EDITOR_ACTION_TYPE,
            &VideoEditorAction::AddEffect {
                clip: id.to_string(),
                expected_stack: Vec::new(),
                ordinal,
                effect,
            },
        );
    }
    fn remove_effect_from_selected(&mut self, doc: &mut AutoCommit, idx: usize) {
        let Some(id) = self.selected() else { return };
        let Some(effect) = self
            .project
            .clip(id)
            .and_then(|clip| clip.effects.get(idx))
            .copied()
        else {
            return;
        };
        if let Some(clip) = self.project.clip_mut(id) {
            clip.effects.remove(idx);
        }
        self.actions.dispatch(
            doc,
            VIDEO_EDITOR_ACTION_TYPE,
            &VideoEditorAction::RemoveEffect {
                clip: id.to_string(),
                effect: effect.id.to_string(),
                expected_kind: effect.kind,
                expected_amount_bits: effect.amount.to_bits(),
            },
        );
    }
    fn move_effect_selected(&mut self, doc: &mut AutoCommit, idx: usize, up: bool) {
        let Some(id) = self.selected() else { return };
        let Some(clip) = self.project.clip(id) else {
            return;
        };
        let Some(target) = (if up {
            idx.checked_sub(1)
        } else {
            (idx + 1 < clip.effects.len()).then_some(idx + 1)
        }) else {
            return;
        };
        let Some(effect) = clip.effects.get(idx) else {
            return;
        };
        let neighbour = clip.effects[target].id;
        let effect_id = effect.id;
        if let Some(clip) = self.project.clip_mut(id) {
            clip.effects.swap(idx, target);
        }
        self.actions.dispatch(
            doc,
            VIDEO_EDITOR_ACTION_TYPE,
            &VideoEditorAction::MoveEffect {
                clip: id.to_string(),
                effect: effect_id.to_string(),
                expected_index: idx,
                neighbour: neighbour.to_string(),
                up,
            },
        );
    }
    /// Open the "Add effect" picker (the shared context menu) at `at`.
    fn open_effect_menu(&mut self, at: Point) {
        let mut menu = ContextMenu::new();
        for k in EffectKind::ALL {
            menu.push(MenuItem::action(k.label(), || {}));
        }
        menu.open_at([at.x as f32, at.y as f32]);
        self.bin_menu = menu;
        self.menu_target = Some(MenuTarget::AddEffect);
    }
    fn set_palette(&mut self, doc: &mut AutoCommit, p: u8) {
        self.edit_selected(doc, move |c| c.palette = p);
    }

    /// Return the lowest-index *unlocked* track of `kind`, creating one
    /// (dispatched as an action, in the same undo boundary as the caller's
    /// edit) when the project has none — so placing a clip from the bin works
    /// even on a blank project, and a locked track never silently swallows a
    /// drop.
    fn ensure_track_of_kind(&mut self, doc: &mut AutoCommit, kind: TrackKind) -> TrackId {
        if let Some(t) = self
            .project
            .tracks
            .iter()
            .filter(|t| t.kind == kind && !t.locked)
            .min_by_key(|t| t.index)
        {
            return t.id;
        }
        let index = self
            .project
            .tracks
            .iter()
            .filter(|t| t.kind == kind)
            .map(|t| t.index)
            .max()
            .unwrap_or(0)
            + 1;
        let name = format!(
            "{}{index}",
            if kind == TrackKind::Video { "V" } else { "A" }
        );
        let track = crate::model::Track {
            id: Uuid::new_v4(),
            name,
            kind,
            index,
            muted: false,
            locked: false,
            solo: false,
            gain_db: 0.0,
            pan: 0.0,
        };
        let id = track.id;
        self.actions
            .dispatch(doc, VIDEO_EDITOR_ACTION_TYPE, &upsert_track_action(&track));
        self.project.tracks.push(track);
        id
    }

    /// Append the given media as a clip on a matching track (creating that track
    /// if the project has none), at the end of that track's content. Returns the
    /// new clip id.
    fn add_media_to_timeline(&mut self, doc: &mut AutoCommit, media: MediaId) -> Option<ClipId> {
        let m = self.project.media(media).cloned()?;
        // Before the clip lands, while the timeline is still empty: a blank
        // project takes its shape from the footage rather than making the
        // user go and find a dialog.
        self.adopt_project_format(doc, media);
        let kind = if m.kind.is_visual() {
            TrackKind::Video
        } else {
            TrackKind::Audio
        };
        let track = self.ensure_track_of_kind(doc, kind);
        let start = self.project.clips_on(track).last().map_or(0.0, |c| c.end());
        let id = self.project.add_clip(track, media, start)?;
        self.dispatch_clip(doc, id);
        // A picture that carries sound lands as two clips, not one: the sound
        // on its own lane, linked to the picture. Without it there is nothing
        // to slide, level, fade or cut the sound with.
        self.attach_linked_audio(doc, id);
        self.selection.select_only(id);
        Some(id)
    }

    /// Place `media` on `track` at timeline time `start` (a drag-drop from the
    /// bin), creating and selecting the clip. Returns the new clip id.
    fn place_media_at(
        &mut self,
        doc: &mut AutoCommit,
        media: MediaId,
        track: TrackId,
        start: f64,
    ) -> Option<ClipId> {
        // A drag from the bin is just as much "the project's first clip" as
        // an append is, so it adopts the same way.
        self.adopt_project_format(doc, media);
        let start = snap_to_frame(start.max(0.0), self.project.fps);
        let id = self.project.add_clip(track, media, start)?;
        self.dispatch_clip(doc, id);
        // Dropping a source on a lane is just as much a placement as
        // appending one, so it splits the sound off the same way.
        self.attach_linked_audio(doc, id);
        self.selection.select_only(id);
        Some(id)
    }

    // ---- bin → timeline drag ---------------------------------------------

    /// Promote an armed bin drag to a live one once the pointer passes the
    /// threshold. Called on every pointer move.
    fn update_bin_drag(&mut self) {
        let mouse = self.last_mouse;
        if let Some(bd) = self.bin_drag.as_mut() {
            if !bd.active && bd.origin.distance(mouse) > BIN_DRAG_THRESHOLD {
                bd.active = true;
                // A press that becomes a drag is never a rename.
                self.pending_rename = None;
            }
        }
    }

    /// The lane under the current cursor while a bin drag is live, as
    /// `(track, kind, lane rect)` — the drop target.
    ///
    /// Only the pointer's **y** picks the lane. Requiring `x >= lanes.x0` used
    /// to mean a straight-down drag from a bin row — the natural gesture,
    /// since the bin sits above the gutter — landed in the 128px track-header
    /// gutter, missed every lane, and silently appended the clip at the end of
    /// the timeline instead of placing it. A drop anywhere across the
    /// timeline's width now hits the lane it is level with; the gutter simply
    /// resolves to the leftmost visible time (see [`Self::drop_time`]).
    fn bin_drop_lane(&self, r: &Regions) -> Option<(TrackId, TrackKind, Rect)> {
        let p = self.last_mouse;
        if p.x < r.timeline.x0 || p.x > r.timeline.x1 {
            return None;
        }
        self.lane_rects(r)
            .into_iter()
            .find(|(_, _, lane)| p.y >= lane.y0 && p.y <= lane.y1)
    }

    /// The timeline time a drop at `x` means. Inside the lanes it is the time
    /// under the pointer; over the gutter it clamps to the lane area's left
    /// edge, i.e. the earliest visible time (0 on an unscrolled timeline).
    fn drop_time(&self, x: f64, r: &Regions) -> f64 {
        self.x_to_time(x.max(r.lanes.x0), &r.lanes)
    }

    /// Finish a bin drag: place the dragged media as a clip at the drop
    /// position. A drop on a matching-kind unlocked lane lands at the drop time
    /// on that track; a kind-mismatched or locked lane lands at the drop time
    /// on a matching-kind track (created if needed); a drop elsewhere on the
    /// timeline appends to a matching track. A drop off the timeline cancels.
    ///
    /// A press that never became a drag ends here too: if it armed a rename
    /// intent, this is where the rename actually begins — release-time, so a
    /// drag that started from the same press has already cancelled it.
    fn release_bin_drag(&mut self, doc: &mut AutoCommit, r: &Regions) {
        let Some(bd) = self.bin_drag.take() else {
            return;
        };
        if !bd.active {
            // A plain click that never moved. If it landed on the name of an
            // item clicked a moment ago, that is the Finder-style slow second
            // click — begin the inline rename now.
            if self.pending_rename.take() == Some(bd.media) {
                self.begin_rename(bd.media);
                self.last_click_media = None;
            }
            return;
        }
        self.pending_rename = None;
        let p = self.last_mouse;
        let want_kind = if self
            .project
            .media(bd.media)
            .is_none_or(|m| m.kind.is_visual())
        {
            TrackKind::Video
        } else {
            TrackKind::Audio
        };
        if let Some((tid, kind, _)) = self.bin_drop_lane(r) {
            let t = self.drop_time(p.x, r);
            if kind == want_kind && !self.project.track_locked(tid) {
                self.place_media_at(doc, bd.media, tid, t);
            } else {
                // Wrong kind, or the lane refuses edits: fall to a track that
                // can take it, at the same drop time.
                let track = self
                    .project
                    .placement_track(want_kind, None)
                    .unwrap_or_else(|| self.ensure_track_of_kind(doc, want_kind));
                self.place_media_at(doc, bd.media, track, t);
            }
        } else if r.timeline.contains(p) {
            // On the timeline but not level with any lane (below the last one)
            // → append to a matching track.
            self.add_media_to_timeline(doc, bd.media);
        }
    }

    // ---- media-bin clip management ---------------------------------------

    /// The sub-rect of a bin row where the media *name* is drawn — clicking it
    /// (on an already-selected item) begins an inline rename, and it's where the
    /// rename field is placed.
    fn bin_name_rect(&self, row: Rect) -> Rect {
        // Mirrors draw_bin: a 64px thumbnail at the left, then the name text.
        let x0 = row.x0 + 8.0 + 64.0 + 12.0;
        Rect::new(x0, row.y0 + 4.0, row.x1 - 8.0, row.y0 + 30.0)
    }

    /// Open the bin context menu for `mid` at logical point `at`.
    fn open_bin_menu(&mut self, mid: MediaId, at: Point) {
        let mut menu = ContextMenu::new();
        menu.push(MenuItem::action("Add to timeline", || {}));
        menu.push(MenuItem::action("Rename", || {}));
        menu.push(MenuItem::separator());
        menu.push(MenuItem::action("Delete", || {}));
        menu.open_at([at.x as f32, at.y as f32]);
        self.bin_menu = menu;
        self.menu_target = Some(MenuTarget::Media(mid));
        self.selected_media = Some(mid);
    }

    /// Open the track-header context menu for `tid` at logical point `at`.
    /// Carries the two per-track state toggles as well as the structural
    /// actions, so mute and lock are reachable without hunting for the pills.
    fn open_track_menu(&mut self, tid: TrackId, at: Point) {
        let t = self.project.track(tid);
        let muted = t.map(|t| t.muted).unwrap_or(false);
        let locked = t.map(|t| t.locked).unwrap_or(false);
        let solo = t.map(|t| t.solo).unwrap_or(false);
        let mut menu = ContextMenu::new();
        menu.push(MenuItem::action(
            if muted { "Unmute track" } else { "Mute track" },
            || {},
        ));
        menu.push(MenuItem::action(
            if solo { "Unsolo track" } else { "Solo track" },
            || {},
        ));
        menu.push(MenuItem::action(
            if locked { "Unlock track" } else { "Lock track" },
            || {},
        ));
        menu.push(MenuItem::separator());
        menu.push(MenuItem::action("Add video track", || {}));
        menu.push(MenuItem::action("Add audio track", || {}));
        menu.push(MenuItem::separator());
        menu.push(MenuItem::action("Delete track", || {}));
        menu.open_at([at.x as f32, at.y as f32]);
        self.bin_menu = menu;
        self.menu_target = Some(MenuTarget::Track(tid));
    }

    /// Open the timeline clip context menu for `cid` at logical point `at`.
    /// Mirrors the bin and track-header menus, and offers exactly the
    /// selection-wide edits the keyboard has: cut, copy, duplicate, split,
    /// delete and ripple delete.
    fn open_clip_menu(&mut self, cid: ClipId, at: Point) {
        // Right-clicking outside the current selection re-targets it, so the
        // menu always acts on what the user just pointed at.
        if !self.selection.contains(cid) {
            self.selection.select_only(cid);
        } else {
            self.selection.set_primary(cid);
        }
        let mut menu = ContextMenu::new();
        menu.push(MenuItem::action("Cut", || {}));
        menu.push(MenuItem::action("Copy", || {}));
        menu.push(MenuItem::action("Duplicate", || {}));
        menu.push(MenuItem::separator());
        menu.push(MenuItem::action("Split at playhead", || {}));
        menu.push(MenuItem::separator());
        menu.push(MenuItem::action("Delete", || {}));
        menu.push(MenuItem::action("Ripple delete", || {}));
        // Link / unlink, last and behind its own rule: a person who wants the
        // sound three frames late has to be able to find out how, and the
        // clip's own right-click menu is where they will look. Appended only
        // when there is something to do, so the rows above never shift.
        if let Some(offer) = self.link_affordance() {
            menu.push(MenuItem::separator());
            menu.push(MenuItem::action(offer.label(), || {}));
        }
        menu.open_at([at.x as f32, at.y as f32]);
        self.bin_menu = menu;
        self.menu_target = Some(MenuTarget::Clip(cid));
    }

    /// If the cursor is over a timeline clip, open its context menu there.
    fn open_clip_menu_at_cursor(&mut self, r: &Regions) -> bool {
        let p = self.last_mouse;
        if !r.lanes.contains(p) {
            return false;
        }
        let Some((cid, _lane)) = self.clip_at(r, p) else {
            return false;
        };
        self.open_clip_menu(cid, p);
        true
    }

    /// If the cursor is over a visible bin item, open its context menu there.
    fn open_bin_menu_at_cursor(&mut self, r: &Regions) -> bool {
        let p = self.last_mouse;
        let bin_visible = self.bin_is_visible(r);
        if !bin_visible {
            return false;
        }
        for (mid, rect) in self.bin_item_rects(r) {
            if rect.contains(p) {
                self.open_bin_menu(mid, p);
                return true;
            }
        }
        false
    }

    /// If the cursor is over a timeline track header (the gutter), open its
    /// track context menu there.
    fn open_track_menu_at_cursor(&mut self, r: &Regions) -> bool {
        let p = self.last_mouse;
        if !r.gutter.contains(p) {
            return false;
        }
        for (tid, _kind, lane) in self.lane_rects(r) {
            let header = Rect::new(r.gutter.x0, lane.y0, r.gutter.x1, lane.y1);
            if header.contains(p) {
                self.open_track_menu(tid, p);
                return true;
            }
        }
        false
    }

    /// Act on a chosen context-menu row (indices match the `open_*_menu`
    /// builders for the current [`MenuTarget`]).
    fn bin_menu_choose(&mut self, doc: &mut AutoCommit, idx: usize) {
        match self.menu_target {
            Some(MenuTarget::Media(mid)) => match idx {
                0 => {
                    self.add_media_to_timeline(doc, mid);
                }
                1 => self.begin_rename(mid),
                3 => self.delete_media(doc, mid),
                _ => {}
            },
            Some(MenuTarget::Track(tid)) => match idx {
                0 => self.toggle_track_mute(doc, tid),
                1 => self.toggle_track_solo(doc, tid),
                2 => self.toggle_track_lock(doc, tid),
                4 => {
                    self.add_track(doc, TrackKind::Video);
                }
                5 => {
                    self.add_track(doc, TrackKind::Audio);
                }
                7 => self.delete_track(doc, tid),
                _ => {}
            },
            Some(MenuTarget::Clip(_)) => match idx {
                0 => self.cut_selected(doc),
                1 => self.copy_selected(),
                2 => self.duplicate_selected(doc),
                4 => self.split_selected(doc),
                6 => self.delete_selected(doc),
                7 => self.ripple_delete_selected(doc),
                // 8 is the separator; 9 exists only when `open_clip_menu`
                // found something to link or unlink, so it can only be chosen
                // when it does.
                9 => self.toggle_link_selected(doc),
                _ => {}
            },
            Some(MenuTarget::AddEffect) => {
                if let Some(&kind) = EffectKind::ALL.get(idx) {
                    self.add_effect_to_selected(doc, kind);
                }
            }
            Some(MenuTarget::Keyframe(param)) => self.key_menu_choose(doc, param, idx),
            None => {}
        }
    }

    /// Add a fresh empty track of `kind` above the others of its kind
    /// (index = current max of that kind + 1), dispatched as an action.
    fn add_track(&mut self, doc: &mut AutoCommit, kind: TrackKind) -> TrackId {
        let index = self
            .project
            .tracks
            .iter()
            .filter(|t| t.kind == kind)
            .map(|t| t.index)
            .max()
            .unwrap_or(0)
            + 1;
        let name = format!(
            "{}{index}",
            if kind == TrackKind::Video { "V" } else { "A" }
        );
        let track = crate::model::Track {
            id: Uuid::new_v4(),
            name,
            kind,
            index,
            muted: false,
            locked: false,
            solo: false,
            gain_db: 0.0,
            pan: 0.0,
        };
        let id = track.id;
        self.actions
            .dispatch(doc, VIDEO_EDITOR_ACTION_TYPE, &upsert_track_action(&track));
        self.project.tracks.push(track);
        id
    }

    /// Delete a track and every clip on it, as one action.
    fn delete_track(&mut self, doc: &mut AutoCommit, tid: TrackId) {
        let clip_ids: Vec<ClipId> = self
            .project
            .clips
            .iter()
            .filter(|c| c.track == tid)
            .map(|c| c.id)
            .collect();
        self.selection.remove_all(&clip_ids);
        self.actions.dispatch(
            doc,
            VIDEO_EDITOR_ACTION_TYPE,
            &crate::actions::delete_track_action(&tid, &clip_ids),
        );
    }

    /// Toggle a track's mute flag (the header pill), dispatched as an action.
    fn toggle_track_mute(&mut self, doc: &mut AutoCommit, tid: TrackId) {
        if let Some(t) = self.project.track(tid) {
            let mut next = t.clone();
            next.muted = !next.muted;
            let muted = next.muted;
            self.actions
                .dispatch(doc, VIDEO_EDITOR_ACTION_TYPE, &upsert_track_action(&next));
            if let Some(t) = self.project.tracks.iter_mut().find(|t| t.id == tid) {
                t.muted = muted;
            }
        }
    }

    /// Toggle a track's solo, dispatched as an action. Wired exactly like
    /// mute: the flag lives on the track, replicates, and undoes the same way.
    /// [`Project::track_audible`] is where the two combine.
    fn toggle_track_solo(&mut self, doc: &mut AutoCommit, tid: TrackId) {
        if let Some(t) = self.project.track(tid) {
            let mut next = t.clone();
            next.solo = !next.solo;
            let solo = next.solo;
            self.actions
                .dispatch(doc, VIDEO_EDITOR_ACTION_TYPE, &upsert_track_action(&next));
            if let Some(t) = self.project.tracks.iter_mut().find(|t| t.id == tid) {
                t.solo = solo;
            }
        }
    }

    /// Toggle a track's lock (the header pill next to mute), dispatched as an
    /// action so it replicates and undoes like every other edit. Locking is
    /// what [`Project::clip_editable`] reads, so the flag really does hold
    /// every move / trim / delete / split off the track's clips.
    fn toggle_track_lock(&mut self, doc: &mut AutoCommit, tid: TrackId) {
        if let Some(t) = self.project.track(tid) {
            let mut next = t.clone();
            next.locked = !next.locked;
            // Reflect it in memory too so the very next press is already
            // guarded, before the document round-trips back through `sync`.
            let locked = next.locked;
            self.actions
                .dispatch(doc, VIDEO_EDITOR_ACTION_TYPE, &upsert_track_action(&next));
            if let Some(t) = self.project.tracks.iter_mut().find(|t| t.id == tid) {
                t.locked = locked;
            }
        }
    }

    /// Begin an inline rename of bin media `mid` using the shared editable
    /// label (real caret / selection / clipboard). No-op if the media is gone.
    fn begin_rename(&mut self, mid: MediaId) {
        let Some(m) = self.project.media(mid) else {
            return;
        };
        let mut label = EditableLabel::new(&m.name)
            .with_font_size(13.0)
            .with_id("bin_rename");
        label.begin_edit();
        self.bin_rename = Some((mid, label));
        self.selected_media = Some(mid);
    }

    /// Commit an inline rename: dispatch a whole-media write with the new name.
    fn commit_rename(&mut self, doc: &mut AutoCommit, mid: MediaId, name: &str) {
        let trimmed = name.trim();
        if trimmed.is_empty() {
            return;
        }
        if let Some(m) = self.project.media(mid) {
            if m.name == trimmed {
                return;
            }
            let mut next = m.clone();
            next.name = trimmed.to_string();
            self.actions
                .dispatch(doc, VIDEO_EDITOR_ACTION_TYPE, &upsert_media_action(&next));
        }
    }

    /// Delete a bin media and every clip that placed it, as one action.
    fn delete_media(&mut self, doc: &mut AutoCommit, mid: MediaId) {
        let clip_ids: Vec<ClipId> = self
            .project
            .clips
            .iter()
            .filter(|c| c.media == mid)
            .map(|c| c.id)
            .collect();
        // Keep the in-memory selection consistent before the reload.
        self.selection.remove_all(&clip_ids);
        self.actions.dispatch(
            doc,
            VIDEO_EDITOR_ACTION_TYPE,
            &remove_media_action(&mid, &clip_ids),
        );
        // Mirror the action in memory, the way the import path does. The
        // document is the truth and `sync` will reload it, but the model
        // must not disagree with it in between — a caller that deletes and
        // then reads back its own project would otherwise still be handed
        // the source it just removed.
        self.project.media.retain(|m| m.id != mid);
        self.project.clips.retain(|c| c.media != mid);
        if self.selected_media == Some(mid) {
            self.selected_media = None;
        }
        if self.hover_media == Some(mid) {
            self.hover_media = None;
        }
    }

    // ---- media import ----------------------------------------------------
    /// Open the host file picker to import media through the shared XDG portal
    /// (`ui::portal`). The picker is a pure-Rust round-trip compiled into every
    /// build; the chosen files are drained each frame in [`Self::drain_import`].
    fn trigger_import(&mut self) {
        self.import_notice = Some(Notice::ok("Choose media to import…"));
        self.import_rx = Some(portal::request_import());
    }

    fn perform_resource_operation(
        &mut self,
        operation: crate::actions::VideoEditorOperation,
    ) -> Result<(), String> {
        use crate::actions::VideoEditorOperation;
        match operation {
            VideoEditorOperation::RevealClip { clip } => {
                let id = clip
                    .parse::<Uuid>()
                    .map_err(|_| format!("invalid clip id: {clip}"))?;
                let clip = self
                    .project
                    .clip(id)
                    .cloned()
                    .ok_or_else(|| format!("clip is no longer available: {id}"))?;
                self.selection.select_only(id);
                self.selected_media = Some(clip.media);
                self.seek_to(clip.start);
                self.reveal_inspector();
            }
            VideoEditorOperation::RevealEffect { clip, effect } => {
                let clip_id = clip
                    .parse::<Uuid>()
                    .map_err(|_| format!("invalid clip id: {clip}"))?;
                let effect_id = effect
                    .parse::<Uuid>()
                    .map_err(|_| format!("invalid effect id: {effect}"))?;
                let clip = self
                    .project
                    .clip(clip_id)
                    .ok_or_else(|| format!("effect's clip is no longer available: {clip_id}"))?;
                if clip.effect_index(effect_id).is_none() {
                    return Err(format!("effect is no longer available: {effect_id}"));
                }
                self.selection.select_only(clip_id);
                self.selected_media = Some(clip.media);
                self.seek_to(clip.start);
                self.reveal_inspector();
            }
            VideoEditorOperation::RevealTrack { track } => {
                let id = track
                    .parse::<Uuid>()
                    .map_err(|_| format!("invalid track id: {track}"))?;
                if self.project.track(id).is_none() {
                    return Err(format!("track is no longer available: {id}"));
                }
                let mut clips: Vec<_> = self
                    .project
                    .clips_on(id)
                    .into_iter()
                    .map(|clip| (clip.id, clip.start, clip.media))
                    .collect();
                clips.sort_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal));
                let ids = clips.iter().map(|(clip, _, _)| *clip).collect();
                self.selection
                    .set_with_primary(ids, clips.first().map(|(clip, _, _)| *clip));
                self.selected_media = clips.first().map(|(_, _, media)| *media);
                if let Some((_, at, _)) = clips.first() {
                    self.seek_to(*at);
                    self.reveal_inspector();
                }
            }
            VideoEditorOperation::RevealMarker { marker, clip } => {
                let marker_id = marker
                    .parse::<Uuid>()
                    .map_err(|_| format!("invalid marker id: {marker}"))?;
                let (at, owner, media) = match clip {
                    Some(clip) => {
                        let clip_id = clip
                            .parse::<Uuid>()
                            .map_err(|_| format!("invalid clip id: {clip}"))?;
                        let clip = self.project.clip(clip_id).ok_or_else(|| {
                            format!("marker's clip is no longer available: {clip_id}")
                        })?;
                        let marker = clip
                            .markers
                            .iter()
                            .find(|marker| marker.id == marker_id)
                            .ok_or_else(|| format!("marker is no longer available: {marker_id}"))?;
                        (clip.start + marker.t, Some(clip_id), Some(clip.media))
                    }
                    None => {
                        let marker = self
                            .project
                            .markers
                            .iter()
                            .find(|marker| marker.id == marker_id)
                            .ok_or_else(|| format!("marker is no longer available: {marker_id}"))?;
                        (marker.t, None, None)
                    }
                };
                if let Some(owner) = owner {
                    self.selection.select_only(owner);
                    self.selected_media = media;
                    self.reveal_inspector();
                } else {
                    self.selection.clear();
                }
                self.seek_to(at);
            }
            VideoEditorOperation::RevealMedia { media } => {
                let id = media
                    .parse::<Uuid>()
                    .map_err(|_| format!("invalid media id: {media}"))?;
                if self.project.media(id).is_none() {
                    return Err(format!("media is no longer available: {id}"));
                }
                self.selected_media = Some(id);
                self.selection.clear();
                self.bin_flap.open();
            }
            VideoEditorOperation::ImportMedia { path: None } => self.trigger_import(),
            VideoEditorOperation::OpenProject { path: None } => self.request_open_project(),
            VideoEditorOperation::SaveProject { path: None } => self.queue_save_project(),
            VideoEditorOperation::SaveProjectAs { path: None } => self.request_save_as(),
            VideoEditorOperation::Export { path: None } => self.request_export(),
            VideoEditorOperation::ImportMedia { path: Some(path) } => {
                self.pending_imports.push(PathBuf::from(path))
            }
            VideoEditorOperation::Export { path: Some(path) } => {
                self.begin_export_to(PathBuf::from(path))
            }
            VideoEditorOperation::OpenProject { path: Some(_) }
            | VideoEditorOperation::SaveProject { path: Some(_) }
            | VideoEditorOperation::SaveProjectAs { path: Some(_) } => {
                return Err("direct project I/O requires lifecycle dispatch".into());
            }
            VideoEditorOperation::CancelExport => {
                let Some(job) = self.export_job.as_ref() else {
                    return Err("no export is running".into());
                };
                job.cancel();
            }
        }
        Ok(())
    }

    fn finish_resource_file_operation_if_ready(&mut self) {
        use crate::actions::VideoEditorOperation;
        let Some(operation) = self.resource_operation.as_ref() else {
            return;
        };
        let outcome: Option<Result<(), String>> = match operation {
            VideoEditorOperation::ImportMedia { .. }
                if self.import_rx.is_none() && self.pending_imports.is_empty() =>
            {
                self.import_notice.as_ref().map(|notice| {
                    if notice.level == NoticeLevel::Failed {
                        Err(notice.text.clone())
                    } else {
                        Ok(())
                    }
                })
            }
            VideoEditorOperation::OpenProject { .. }
            | VideoEditorOperation::SaveProject { .. }
            | VideoEditorOperation::SaveProjectAs { .. }
                if self.project_rx.is_none()
                    && self.resource_pending_operation.is_none()
                    && self.pending_open.is_none() =>
            {
                self.project_notice.as_ref().map(|notice| {
                    if notice.level == NoticeLevel::Failed {
                        Err(notice.text.clone())
                    } else {
                        Ok(())
                    }
                })
            }
            VideoEditorOperation::Export { .. } => match &self.export_status {
                ExportStatus::Done { .. } => Some(Ok(())),
                ExportStatus::Failed { reason } => Some(Err(reason.clone())),
                _ => None,
            },
            _ => None,
        };
        let (Some(invocation), Some(outcome)) = (self.resource_invocation.take(), outcome) else {
            return;
        };
        self.resource_operation = None;
        self.resource_result = Some(match outcome {
            Ok(()) => tilo_actions::InvocationUpdate::Succeeded {
                invocation,
                output: tilo_actions::ActionOutput::default(),
            },
            Err(message) => tilo_actions::InvocationUpdate::Failed {
                invocation,
                error: tilo_actions::ActionFailure {
                    code: "video_editor.operation_failed".into(),
                    message,
                    details: None,
                    retryable: false,
                },
            },
        });
    }

    /// Drain any pending imports (queued deterministic files first, then a
    /// portal outcome), bringing each chosen file into the bin as a real clip.
    fn drain_import(&mut self, doc: &mut AutoCommit) {
        if !self.pending_imports.is_empty() {
            let paths = std::mem::take(&mut self.pending_imports);
            let report = self.import_files(doc, &paths);
            self.import_notice = Some(import_report_notice(&report));
            if self.media_pipeline.auto_place_imports
                && let Some(mid) = self.selected_media
            {
                self.ensure_track_and_place(doc, mid);
            }
        }
        let mut clear = false;
        let mut outcome = None;
        if let Some(rx) = self.import_rx.as_ref() {
            match rx.try_recv() {
                Ok(o) => {
                    outcome = Some(o);
                    clear = true;
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => {}
                Err(std::sync::mpsc::TryRecvError::Disconnected) => clear = true,
            }
        }
        if clear {
            self.import_rx = None;
        }
        match outcome {
            Some(ImportOutcome::Selected(paths)) => {
                let report = self.import_files(doc, &paths);
                self.import_notice = Some(import_report_notice(&report));
            }
            Some(ImportOutcome::Cancelled) => {
                self.import_notice = None;
                if let Some(invocation) = self.resource_invocation.take() {
                    self.resource_operation = None;
                    self.resource_result = Some(tilo_actions::InvocationUpdate::Cancelled {
                        invocation,
                        message: Some("media selection was cancelled".into()),
                    });
                }
            }
            // The picker itself refused. Its wording ("the file picker needs
            // the real-desktop build") is nothing like a probe's, which is
            // exactly why the tint cannot be read back out of the words.
            Some(ImportOutcome::Failed(reason)) => {
                self.import_notice = Some(Notice::failed(reason));
            }
            None => {}
        }
    }

    /// Import a batch of files, reporting what landed *and* what did not.
    ///
    /// Every refusal is kept with the probe's own reason. The batch used to
    /// return only a count, so a partly-failed import collapsed to "Imported
    /// 3 of 5 files" — which names neither of the two files that were
    /// refused nor why, even though the probe knew exactly.
    fn import_files(&mut self, doc: &mut AutoCommit, paths: &[PathBuf]) -> ImportReport {
        self.import_notice = None;
        let mut report = ImportReport::default();
        for path in paths {
            match self.try_import_one(doc, path) {
                Ok(()) => report.added += 1,
                Err(reason) => report.refused.push((display_name(path), reason)),
            }
        }
        report
    }

    /// Import one file: probe **real** metadata from the container and add it to
    /// the media bin as a real [`MediaItem`], replicated through the CRDT. The
    /// stored path is workspace-relative for a file under the FUSE mount (so it
    /// replicates to peers) or absolute for an imported local file.
    ///
    /// A refusal hands back the probe's own reason ("is not a media file - it
    /// is HTML document") rather than writing a notice itself, so the batch
    /// above can keep *every* refusal instead of whichever one happened to be
    /// written last.
    fn try_import_one(&mut self, doc: &mut AutoCommit, path: &Path) -> Result<(), String> {
        let probed = match probe::probe_file(path) {
            Ok(p) => p,
            Err(e) => return Err(e.to_string()),
        };
        let resolved = crate::media::resolve(&path.to_string_lossy());
        let stored = crate::media::stored_path_for_import(&resolved.abs_path);
        let name = path
            .file_name()
            .map_or_else(|| stored.clone(), |n| n.to_string_lossy().into_owned());
        let palette = (self.project.media.len() as u8) % 7;
        let item = MediaItem {
            id: Uuid::new_v4(),
            name,
            kind: probed.kind,
            duration: probed.duration,
            width: probed.width,
            height: probed.height,
            // 0.0 means the container did not say. Carry that through rather
            // than inventing 30 — the bin renders unknown as unknown.
            fps: probed.fps,
            palette,
            path: stored,
            hydration: resolved.hydration,
            // An imported file is decoded, never generated: only a title
            // carries text.
            text: None,
            // What the container said. A video file with a sound track is
            // the case the mixer needs told about — it reads audio off video
            // tracks only for media that claims to carry any.
            has_audio: probed.has_audio,
        };
        let id = item.id;
        self.project.media.push(item.clone());
        self.actions
            .dispatch(doc, VIDEO_EDITOR_ACTION_TYPE, &upsert_media_action(&item));
        self.selected_media = Some(id);
        Ok(())
    }

    /// Drop `media_id` onto the timeline (creating a track of the right kind if
    /// the blank project has none) and park the playhead inside the new clip.
    /// Used by the `--shots`/preload placed-import path so the preview shows the
    /// clip; a normal portal import lands in the bin only.
    fn ensure_track_and_place(&mut self, doc: &mut AutoCommit, media_id: MediaId) {
        // `add_media_to_timeline` now creates a matching track when the project
        // has none, so this just places and parks the playhead in the new clip.
        let Some(cid) = self.add_media_to_timeline(doc, media_id) else {
            return;
        };
        if let Some(c) = self.project.clip(cid) {
            let inside = c.start + (c.duration() * 0.4);
            self.playhead = inside.clamp(c.start, (c.end() - 0.05).max(c.start));
        }
    }

    /// The clickable "Import media" button drawn in the empty-bin state, if the
    /// bin is on-screen and has no media yet.
    fn empty_bin_import_rect(&self, r: &Regions) -> Option<Rect> {
        if !self.project.media.is_empty() {
            return None;
        }
        let cx = r.bin.center().x;
        let cy = r.bin.y0 + 150.0;
        Some(Rect::new(cx - 92.0, cy, cx + 92.0, cy + 34.0))
    }

    /// Pump the real preview decoder for the clip under the playhead. No-op
    /// build carries no decoder; the decode build seeks + decodes the frame.
    #[cfg(feature = "decode")]
    fn pump_preview_decode(&mut self) {
        // The bundled demo/shots project references placeholder sample-media,
        // not real bytes, so decoding it would spin a real GStreamer pipeline
        // for no usable frame and make a headless render depend on the gst
        // runtime. Keep the demo preview on its deterministic representative
        // composite. `decode` stays compiled in: the live app and the
        // `--import` real-decode capture (which loads genuine files) still
        // decode. This is the runtime half of "features compile the code in;
        // the runtime mode decides live vs sample".
        if self.seed.demo {
            return;
        }
        self.preview_decoder.update(&self.project, self.playhead);
    }

    /// Composite the preview picture for the current playhead, into
    /// `self.composited`.
    ///
    /// Runs the shared GPU compositor at the canvas's own pixel size (capped,
    /// so a huge window does not read back a huge buffer every frame) and
    /// keeps the RGBA for `draw_preview` to blit. Under `--features decode`
    /// the compositor's layers are filled with really-decoded pictures; with
    /// no decoder — the hermetic `--shots` path — it gets `NoSources` and each
    /// layer draws its representative fill, which keeps a capture
    /// deterministic and free of any media pipeline.
    fn pump_compositor(&mut self, r: &Regions) {
        if !self.render_pipeline.compositor_tried {
            self.render_pipeline.compositor_tried = true;
            match crate::gpu::HeadlessCompositor::new() {
                Ok(c) => self.compositor = Some(c),
                Err(e) => tracing::warn!(error = %e, "no GPU compositor for the preview"),
            }
        }
        let Some(compositor) = self.compositor.as_mut() else {
            return;
        };
        if self.project.clips.is_empty() || r.canvas.width() < 1.0 {
            self.composited = None;
            return;
        }
        // Match the canvas's aspect at a sane readback size.
        const MAX_W: f64 = 960.0;
        let scale = (MAX_W / r.canvas.width()).min(1.0);
        let w = (r.canvas.width() * scale).round().max(2.0) as u32;
        let h = (r.canvas.height() * scale).round().max(2.0) as u32;

        #[cfg(feature = "decode")]
        let rgba = {
            // The preview resolves its layers straight off the decode
            // sessions, which hand back a GPU-resident planar picture. The
            // CPU path this replaced converted every frame to RGBA on the
            // host first — a full-frame colour convert per layer per frame,
            // for a picture that was already on the GPU.
            //
            // `Responsive` never blocks: it serves the cached frame if there
            // is one and otherwise nudges the pipeline and holds the last
            // picture, so a scrub cannot stall the UI thread.
            let sig = crate::gpu::MediaFrameSource::project_signature(&self.project);
            if self.frame_source.is_none() {
                self.frame_source =
                    // A capture has no interactive deadline to miss, and it
                    // renders exactly once — so it must block for the real
                    // picture rather than paint the representative fill and
                    // move on. The live editor must never block: a scrub that
                    // stalls the UI thread is worse than a frame of lag.
                    Some(crate::gpu::MediaFrameSource::new(
                        &self.project,
                        if self.media_pipeline.auto_place_imports {
                            crate::gpu::Fetch::Exact
                        } else {
                            crate::gpu::Fetch::Responsive
                        },
                    ));
                self.frame_source_sig = sig;
            } else if sig != self.frame_source_sig {
                if let Some(src) = self.frame_source.as_mut() {
                    src.set_project(&self.project);
                }
                self.frame_source_sig = sig;
            }
            if let Some(sources) = self.frame_source.as_mut() {
                // An open pipeline holds a decoder and a slice of the GPU. Drop
                // the idle ones periodically rather than every frame, which
                // would churn sessions the playhead is about to want again.
                self.prune_countdown = self.prune_countdown.wrapping_add(1);
                if self.prune_countdown.is_multiple_of(120) {
                    sources.prune();
                }
                compositor.frame(&self.project, self.playhead, w, h, sources)
            } else {
                // A decoder setup failure is a preview fallback, never a UI
                // thread panic. The media bin continues to disclose it.
                let mut sources = crate::gpu::NoSources;
                compositor.frame(&self.project, self.playhead, w, h, &mut sources)
            }
        };
        #[cfg(not(feature = "decode"))]
        let rgba = {
            let mut sources = crate::gpu::NoSources;
            compositor.frame(&self.project, self.playhead, w, h, &mut sources)
        };

        if rgba.len() == (w as usize) * (h as usize) * 4 {
            self.composited = Some((w, h, std::sync::Arc::new(rgba)));
        } else {
            self.composited = None;
        }
    }

    /// Zoom so the whole project fits the lane area, and park the view at the
    /// start. The answer to "I zoomed in and now I am lost", which the editor
    /// previously had no button for at all.
    fn zoom_to_fit(&mut self, r: &Regions) {
        let dur = self.project.duration().max(0.01);
        let w = r.lanes.width().max(1.0);
        self.zoom = (w / dur).clamp(6.0, 220.0);
        self.scroll = 0.0;
        self.clamp_scroll(r);
    }

    /// The zoom-readout chip in the timeline's gutter footer — also the
    /// fit-to-window button.
    /// The zoom readout, at the **right end of the ruler** it scales.
    ///
    /// It used to sit in the corner over the track heads, which is the one
    /// piece of chrome a master meter can have: that corner is the head of the
    /// channel-strip column, it is the only place a mix warning is on screen
    /// whatever the window does, and the zoom of the timeline has nothing to
    /// do with the column of lane names underneath it. Anchored to the ruler's
    /// far edge the chip is still in the ruler, still one click, and out of
    /// the way of both.
    fn zoom_chip_rect(&self, r: &Regions) -> Rect {
        let y = r.ruler.y0 + 4.0;
        // TextRenderer applies the accessibility scale, while this custom
        // chip's old 84px width did not.  At 2× the value was painted through
        // its border and over the final ruler label.  Reserve proportional
        // room; the ruler intentionally omits ticks behind this control.
        let width = (84.0 * self.text_scale).min((r.ruler.width() - 8.0).max(0.0));
        let x1 = r.ruler.x1 - 8.0;
        Rect::new(x1 - width, y, x1, y + 22.0)
    }

    fn zoom_by(&mut self, factor: f64) {
        self.zoom = (self.zoom * factor).clamp(6.0, 220.0);
        let r = self.regions();
        self.clamp_scroll(&r);
    }

    /// Zoom keeping the time currently under `anchor_x` pinned to that pixel —
    /// the wheel-over-the-timeline gesture. Outside the lane area it falls
    /// back to anchoring on the playhead, which is what the keyboard's +/- do.
    fn zoom_at(&mut self, factor: f64, anchor_x: f64, r: &Regions) {
        let anchor_t = if anchor_x >= r.lanes.x0 && anchor_x <= r.lanes.x1 {
            self.x_to_time(anchor_x, &r.lanes)
        } else {
            self.playhead
        };
        let anchor_px = (anchor_t - self.scroll) * self.zoom;
        self.zoom = (self.zoom * factor).clamp(6.0, 220.0);
        self.scroll = anchor_t - anchor_px / self.zoom;
        self.clamp_scroll(r);
    }

    #[allow(
        clippy::too_many_lines,
        reason = "one geometry pass keeps every inspector section aligned to the same scroll coordinate system"
    )]
    fn inspector_layout(&self, r: &Regions) -> InspectorLayout {
        // Layout is authored in logical inspector pixels, then projected into
        // the physical column.  This keeps text, rows, hit targets and the
        // accessibility focus bands in one coordinate system at every text
        // scale instead of merely making the glyphs larger inside 1× rows.
        let scale = self.inspector_ui_scale();
        let physical_body_origin = self.inspector_body_origin(r);
        let body_origin = r.inspector.y0 + (physical_body_origin - r.inspector.y0) / scale;
        let mut logical = *r;
        logical.inspector = Rect::new(
            r.inspector.x0,
            r.inspector.y0,
            r.inspector.x0 + r.inspector.width() / scale,
            r.inspector.y0 + r.inspector.height() / scale,
        );
        let r = &logical;
        let mut l = InspectorLayout::default();
        // Markers belong to the project, not to any clip, so the list is laid
        // out with nothing selected too. Without this the only way to see or
        // remove a marker was to first pick a clip that had nothing to do
        // with it.
        let selected = self.selected().and_then(|id| self.project.clip(id));
        if selected.is_none() {
            let x0 = r.inspector.x0 + PAD;
            let x1 = r.inspector.x1 - PAD;
            let mut y = body_origin - self.inspector_scroll / scale + 16.0;
            self.layout_markers(&mut l, x0, x1, &mut y);
            self.layout_project(&mut l, x0, x1, &mut y);
            l.content_height = (y - (body_origin - self.inspector_scroll / scale)) * scale;
            l.scale_from(r.inspector.x0, r.inspector.y0, scale);
            return l;
        }
        let Some(clip) = selected else {
            return l;
        };
        let media = self.project.media(clip.media);
        let is_video = media.is_none_or(|item| item.kind.is_visual());
        let is_title = media.is_some_and(|item| item.text.is_some());
        let x0 = r.inspector.x0 + PAD;
        let x1 = r.inspector.x1 - PAD;
        // Static header + four property rows consume a fixed block; the
        // editable controls start below it. Everything scrolls together by
        // `inspector_scroll` (the pane clips + is wheel-scrollable) so the
        // effects stack can grow without clipping the actions.
        let top = body_origin - self.inspector_scroll / scale;
        // The four property rows end on a baseline at `top + 132`; a 12px
        // line reaches ~15px past it, so the first control label cannot start
        // before `top + 147` without sitting on top of "Source Out".
        let mut y = top;
        let row = 34.0; // label sits at y, the 9px track at y+19
        let track_at = |y: f64| Rect::new(x0, y + 19.0, x1, y + 28.0);
        // An animatable row indents its label past a key control at the head.
        // The indent is unconditional so the row does not shuffle sideways the
        // moment a parameter is keyed.
        let key_at = |y: f64| Rect::new(x0, y - 3.0, x0 + KEY_BOX, y + 15.0);

        if is_video {
            l.opacity = Some(track_at(y));
            l.keys.push((Param::Opacity, key_at(y)));
            y += row;
            l.brightness = Some(track_at(y));
            l.keys.push((Param::Brightness, key_at(y)));
            y += row;
            l.contrast = Some(track_at(y));
            l.keys.push((Param::Contrast, key_at(y)));
            y += row;
            l.saturation = Some(track_at(y));
            l.keys.push((Param::Saturation, key_at(y)));
            y += row;
        } else {
            l.volume = Some(track_at(y));
            l.keys.push((Param::Volume, key_at(y)));
            y += row;
        }
        // Fade in / out share a row (two half-width tracks).
        let half = (x1 - x0) / 2.0 - 6.0;
        l.fade_in = Some(Rect::new(x0, y + 19.0, x0 + half, y + 28.0));
        l.fade_out = Some(Rect::new(x1 - half, y + 19.0, x1, y + 28.0));
        y += row + 2.0;

        // Transition segmented control.
        let seg_w = (x1 - x0) / Transition::ALL.len() as f64;
        l.transition_y = y + 15.0;
        for (i, t) in Transition::ALL.iter().enumerate() {
            let sx0 = x0 + i as f64 * seg_w;
            l.transitions
                .push((*t, Rect::new(sx0, y + 15.0, sx0 + seg_w - 3.0, y + 37.0)));
        }
        y += 48.0;
        // Transition length, only when there is a transition to size.
        if clip.transition != Transition::None {
            l.transition_len = Some(Rect::new(x0, y + 19.0, x1, y + 28.0));
            y += row;
        }
        // Label color swatches.
        l.colors_y = y + 15.0;
        let sw = 24.0;
        for i in 0..7u8 {
            let sx0 = x0 + f64::from(i) * (sw + 4.0);
            if sx0 + sw > x1 {
                break;
            }
            l.colors
                .push((i, Rect::new(sx0, y + 15.0, sx0 + sw, y + 15.0 + sw)));
        }
        y += 44.0;
        // The link control. It sits in the top block, with the clip's own
        // identity rather than down among the picture sections, because it
        // says what this clip *is* — half of a shot, or a clip on its own —
        // and because an audio clip has to be able to reach it too.
        if self.link_affordance().is_some() {
            l.link_label_y = y + 15.0;
            l.link = Some(Rect::new(x0, y + 20.0, x1, y + 46.0));
            y += 56.0;
        }

        // ---- collapsible sections ------------------------------------
        // Each opens with a header row that folds it; a folded section
        // contributes only its header (and still states what is inside it).
        let header = |l: &mut InspectorLayout, y: &mut f64, s: Section| -> bool {
            l.sections.push((s, Rect::new(x0, *y, x1, *y + SECTION_H)));
            *y += SECTION_H + 4.0;
            !self.collapsed.contains(&s)
        };
        // Tone curves, video only — it is the same grade the three sliders
        // above start, so it sits directly under them.
        if is_video && header(&mut l, &mut y, Section::Curves) {
            let cw = (x1 - x0) / crate::curve::Channel::ALL.len() as f64;
            for (i, ch) in crate::curve::Channel::ALL.iter().enumerate() {
                let cx = x0 + i as f64 * cw;
                l.curve_channels
                    .push((*ch, Rect::new(cx, y, cx + cw - 3.0, y + 24.0)));
            }
            y += 32.0;
            // Square, so the diagonal that means "unchanged" really is a
            // diagonal — on a squashed box a curve reads as steeper or
            // flatter than it is.
            let side = (x1 - x0).min(200.0);
            l.curve_canvas = Some(Rect::new(x0, y, x0 + side, y + side));
            // Room for the gesture hint under the box *and* clear air before
            // the amount row's own label: at the bare 12px gap the two shared
            // a baseline and drew through each other.
            y += side + 34.0;
            l.nums.push((NumField::CurveMix, track_at(y)));
            l.keys.push((Param::CurveMix, key_at(y)));
            y += row;
            l.curve_reset = Some(Rect::new(x0, y, x1, y + 26.0));
            y += 34.0;
        }
        // Geometry, video only — an audio clip has no picture to place.
        if is_video && header(&mut l, &mut y, Section::Transform) {
            for f in [
                NumField::ScaleX,
                NumField::ScaleY,
                NumField::PosX,
                NumField::PosY,
                NumField::Rotation,
            ] {
                l.nums.push((f, track_at(y)));
                if let Some(p) = f.param() {
                    l.keys.push((p, key_at(y)));
                }
                y += row;
            }
            // Crop: two half-width pairs under their own caption. No key
            // control — crop frames the shot rather than moving it, and the
            // model does not animate it.
            y += 12.0;
            l.crop_label_y = y;
            y += 20.0;
            let half = (x1 - x0) / 2.0 - 6.0;
            for pair in [
                [NumField::CropLeft, NumField::CropRight],
                [NumField::CropTop, NumField::CropBottom],
            ] {
                l.nums
                    .push((pair[0], Rect::new(x0, y + 19.0, x0 + half, y + 28.0)));
                l.nums
                    .push((pair[1], Rect::new(x1 - half, y + 19.0, x1, y + 28.0)));
                y += row;
            }
            // Anchor: the point scale and rotation turn about. Nine presets
            // as three segmented rows rather than a free pair of sliders —
            // an anchor is chosen, not dialled, and every value anyone wants
            // is a corner, an edge or the middle.
            y += 12.0;
            l.anchor_label_y = y;
            y += 20.0;
            let cell = (x1 - x0) / 3.0;
            for ay in ANCHOR_STOPS {
                for (col, ax) in ANCHOR_STOPS.iter().enumerate() {
                    let cx = x0 + col as f64 * cell;
                    l.anchor
                        .push(((*ax, ay), Rect::new(cx, y, cx + cell - 3.0, y + 22.0)));
                }
                y += 25.0;
            }
            y += 6.0;
            // Picture-in-picture, as the four corners it is actually used for.
            y += 16.0;
            let bw = (x1 - x0 - 18.0) / 4.0;
            for (i, (label, corner)) in [
                ("↖", (-1.0, -1.0)),
                ("↗", (1.0, -1.0)),
                ("↙", (-1.0, 1.0)),
                ("↘", (1.0, 1.0)),
            ]
            .into_iter()
            .enumerate()
            {
                let bx = x0 + i as f64 * (bw + 6.0);
                l.pip
                    .push((label, corner, Rect::new(bx, y, bx + bw, y + 26.0)));
            }
            y += 32.0;
            l.reset_transform = Some(Rect::new(x0, y, x1, y + 26.0));
            y += 34.0;
        }
        // Rate and direction.
        if header(&mut l, &mut y, Section::Speed) {
            l.nums.push((NumField::Speed, track_at(y)));
            y += row;
            let pw = (x1 - x0) / SPEED_PRESETS.len() as f64;
            for (i, rate) in SPEED_PRESETS.iter().enumerate() {
                let px0 = x0 + i as f64 * pw;
                l.speed_presets
                    .push((*rate, Rect::new(px0, y, px0 + pw - 3.0, y + 24.0)));
            }
            y += 32.0;
            l.reverse = Some(Rect::new(x0, y, x1, y + 26.0));
            y += 34.0;
        }
        // The lane's own fader and pan. Present for a picture clip as well as
        // an audio one: a shot on a video track carries its source's sound,
        // so that lane has a level whether or not anyone has split it off.
        //
        // No key control on either row. A fader is a mix decision, not an
        // animation — a level that has to move through a shot is what the
        // clip's own volume curve is for, and two animatable levels stacked
        // on one signal is two places to look for why it faded.
        if header(&mut l, &mut y, Section::Mix) {
            l.mix_track_y = y;
            y += 18.0;
            l.mix_meter = Some(Rect::new(x0, y, x1, y + 12.0));
            y += 22.0;
            for f in [NumField::TrackGain, NumField::TrackPan] {
                l.nums.push((f, track_at(y)));
                y += row;
            }
            if self
                .selected_track()
                .is_some_and(crate::model::Track::mix_is_set)
            {
                l.mix_reset = Some(Rect::new(x0, y, x1, y + 26.0));
                y += 34.0;
            } else {
                y += 4.0;
            }
        }
        // Colour key, video only — there is nothing to key out of audio.
        // The four sliders appear only once the key is switched on, so an
        // unkeyed clip does not carry a row of controls that do nothing.
        if is_video {
            let open = header(&mut l, &mut y, Section::Key);
            if open {
                l.key_enable = Some(Rect::new(x0, y, x1, y + 26.0));
                y += 34.0;
                if clip.key.enabled {
                    // The caption's own line, then the swatches under it —
                    // the same 20px the Crop caption takes, so the two read
                    // as one convention rather than two.
                    l.key_color_y = y;
                    y += 20.0;
                    let sw = 26.0;
                    for (i, (_, col)) in crate::model::KEY_PRESETS.iter().enumerate() {
                        let sx = x0 + i as f64 * (sw + 5.0);
                        if sx + sw > x1 {
                            break;
                        }
                        l.key_colors.push((*col, Rect::new(sx, y, sx + sw, y + sw)));
                    }
                    // Sampling the plate is how a key is really set; the
                    // presets are only a starting point.
                    let pick_x = x0 + 3.0 * (sw + 5.0);
                    if pick_x + 96.0 <= x1 {
                        l.key_pick = Some(Rect::new(pick_x, y, x1, y + sw));
                    }
                    y += sw + 14.0;
                    for f in [
                        NumField::KeyTolerance,
                        NumField::KeySoftness,
                        NumField::KeySpill,
                    ] {
                        l.nums.push((f, track_at(y)));
                        if let Some(p) = f.param() {
                            l.keys.push((p, key_at(y)));
                        }
                        y += row;
                    }
                    y += 4.0;
                }
            }
        }
        // Shape mask, video only. The geometry rows appear once a shape is
        // chosen — with the mask off there is nothing for them to move.
        if is_video {
            let open = header(&mut l, &mut y, Section::Mask);
            if open {
                let mw = (x1 - x0) / crate::model::MaskShape::ALL.len() as f64;
                for (i, s) in crate::model::MaskShape::ALL.iter().enumerate() {
                    let sx = x0 + i as f64 * mw;
                    l.mask_shapes
                        .push((*s, Rect::new(sx, y, sx + mw - 3.0, y + 24.0)));
                }
                y += 32.0;
                if clip.mask.is_active() {
                    for f in [
                        NumField::MaskX,
                        NumField::MaskY,
                        NumField::MaskW,
                        NumField::MaskH,
                        NumField::MaskRotation,
                        NumField::MaskFeather,
                    ] {
                        l.nums.push((f, track_at(y)));
                        if let Some(p) = f.param() {
                            l.keys.push((p, key_at(y)));
                        }
                        y += row;
                    }
                    let hw = (x1 - x0) / 2.0 - 4.0;
                    l.mask_fit = Some(Rect::new(x0, y, x0 + hw, y + 26.0));
                    l.mask_invert = Some(Rect::new(x1 - hw, y, x1, y + 26.0));
                    y += 34.0;
                }
            }
        }
        // A title's words and setting — only for a clip whose source is one.
        if is_title && header(&mut l, &mut y, Section::Title) {
            // Captioned controls need the caption's line above them; without
            // it the caption lands on the section header it sits under.
            y += 16.0;
            // Three lines' worth: a title is two or three lines as often as
            // it is one, and a one-line box could not show what was typed.
            l.title_text = Some(Rect::new(x0, y, x1, y + 66.0));
            y += 92.0;
            l.nums.push((NumField::TitleSize, track_at(y)));
            y += row;
            let aw = (x1 - x0) / TextAlign::ALL.len() as f64;
            for (i, a) in TextAlign::ALL.iter().enumerate() {
                let ax = x0 + i as f64 * aw;
                l.title_align
                    .push((*a, Rect::new(ax, y, ax + aw - 3.0, y + 24.0)));
            }
            // The caption above the chooser needs its own line: at the
            // ordinary 16px gap it landed on the alignment row's buttons.
            y += 32.0 + 22.0;
            l.title_font = Some(Rect::new(x0, y, x1, y + 28.0));
            y += 36.0 + 16.0;
            let sw = 24.0;
            for (i, col) in TITLE_COLORS.iter().enumerate() {
                let sx = x0 + i as f64 * (sw + 4.0);
                if sx + sw > x1 {
                    break;
                }
                l.title_colors
                    .push((*col, Rect::new(sx, y, sx + sw, y + sw)));
            }
            y += sw + 12.0;
            let hw = (x1 - x0) / 2.0 - 4.0;
            l.title_bold = Some(Rect::new(x0, y, x0 + hw, y + 26.0));
            l.title_italic = Some(Rect::new(x1 - hw, y, x1, y + 26.0));
            y += 34.0;
            l.nums.push((NumField::TitleBackdrop, track_at(y)));
            y += row + 4.0;
        }
        // Effects stack (video only): one row per effect, then an "Add
        // effect" button.
        if is_video {
            let open = header(&mut l, &mut y, Section::Effects);
            l.effects_label_y = y;
            if open {
                for i in 0..clip.effects.len() {
                    let row = Rect::new(x0, y, x1, y + 24.0);
                    l.effect_rows.push(row);
                    // The row's key control joins the panel's one list of
                    // them, so drawing, hit-testing and the keyframe menu all
                    // reach it without a second code path.
                    l.keys
                        .push((Param::EffectAmount(i as u32), Self::effect_row_parts(row).0));
                    y += 28.0;
                }
                l.add_effect = Rect::new(x0, y, x0 + 132.0, y + 24.0);
                y += 34.0;
            }
        }
        // Markers: every marked instant on the timeline, the project's own and
        // each clip's, so one list is the whole answer to "what is marked".
        self.layout_markers(&mut l, x0, x1, &mut y);
        self.layout_project(&mut l, x0, x1, &mut y);
        // Actions.
        y += 6.0;
        l.split = Rect::new(x0, y, (x0 + x1) / 2.0 - 4.0, y + 28.0);
        l.delete = Rect::new((x0 + x1) / 2.0 + 4.0, y, x1, y + 28.0);
        y += 34.0;
        l.content_height = (y - top) * scale;
        l.valid = true;
        l.scale_from(r.inspector.x0, r.inspector.y0, scale);
        l
    }

    /// Draw the tone-curve editor: the grid, the "unchanged" diagonal, the
    /// curve, and its control points.
    ///
    /// This is canvas drawing rather than a toolkit widget for the same
    /// reason the timeline and the waveform are: a 2D control-point editor
    /// is content, not a control the fleet shares. Everything around it —
    /// the channel chooser, the amount slider, the reset — goes through the
    /// shared widgets.
    fn draw_curve_editor(
        &self,
        scene: &mut Scene,
        tf: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        canvas: Rect,
        clip: &Clip,
    ) {
        let c = &theme.tokens.colors;
        let ch = self.curve_channel;
        let spline = clip.curves.spline(ch);
        fill_rrect(scene, tf, c.lift(0.35), canvas, 8.0);
        stroke_rrect(scene, tf, c.border_standard, canvas, 8.0, 1.0);

        // Quarter grid. Faint, because it is a reading aid and the curve is
        // the content.
        let grid = with_alpha(c.text_dim, 0.22);
        for i in 1..4 {
            let f = i as f64 / 4.0;
            let x = canvas.x0 + canvas.width() * f;
            let y = canvas.y0 + canvas.height() * f;
            let mut v = BezPath::new();
            v.move_to((x, canvas.y0 + 1.0));
            v.line_to((x, canvas.y1 - 1.0));
            let mut h = BezPath::new();
            h.move_to((canvas.x0 + 1.0, y));
            h.line_to((canvas.x1 - 1.0, y));
            scene.stroke(&Stroke::new(1.0), tf, grid, None, &v);
            scene.stroke(&Stroke::new(1.0), tf, grid, None, &h);
        }
        // The identity: where every tone would land if the curve did nothing.
        // Without it a lifted curve and a steep one look the same.
        let mut diag = BezPath::new();
        diag.move_to((canvas.x0, canvas.y1));
        diag.line_to((canvas.x1, canvas.y0));
        scene.stroke(
            &Stroke::new(1.0),
            tf,
            with_alpha(c.text_dim, 0.45),
            None,
            &diag,
        );

        // The curve itself, sampled densely enough that the cubic reads as a
        // curve rather than as the polyline it is drawn with.
        let tint = curve_channel_color(theme, ch);
        let mut path = BezPath::new();
        const STEPS: usize = 64;
        for i in 0..=STEPS {
            let t = i as f64 / STEPS as f64;
            let p = curve_point(canvas, t, spline.eval(t));
            if i == 0 {
                path.move_to((p.x, p.y));
            } else {
                path.line_to((p.x, p.y));
            }
        }
        scene.stroke(&Stroke::new(2.0), tf, tint, None, &path);

        // Control points. The one under the pointer (or being dragged) is
        // larger as well as brighter, so it is legible without relying on a
        // hue difference.
        let dragging = match self.drag {
            Some(Drag::CurvePoint { channel, idx }) if channel == ch => Some(idx),
            _ => None,
        };
        let hovered = canvas
            .contains(self.last_mouse)
            .then(|| {
                let (mx, my) = curve_coords(canvas, self.last_mouse);
                spline.point_near(mx, my, POINT_GRAB)
            })
            .flatten();
        for (i, cp) in spline.points.iter().enumerate() {
            let at = curve_point(canvas, cp.x, cp.y);
            let live = dragging == Some(i) || (dragging.is_none() && hovered == Some(i));
            let r = if live { 6.0 } else { 4.0 };
            scene.fill(Fill::NonZero, tf, tint, None, &Circle::new(at, r));
            scene.stroke(
                &Stroke::new(1.5),
                tf,
                if live {
                    c.text_primary
                } else {
                    c.surfaces.base
                },
                None,
                &Circle::new(at, r),
            );
        }

        // What the gesture is, stated rather than left to be discovered. It
        // sits under the box so it never covers the curve.
        let hint = if dragging.is_some() {
            "Drag out of the box to remove"
        } else {
            "Click to add a point · drag to shape"
        };
        text(
            scene,
            tr,
            tf,
            hint,
            10.5,
            c.text_dim,
            canvas.x0,
            canvas.y1 + 12.0,
        );
    }

    /// Draw the marker list: one row per marked instant, with rename and
    /// delete revealed under the pointer. Shared by both inspector states.
    /// Paint the project's format section: what the frame is now, the size
    /// and rate presets with the one in force marked, and the match button.
    fn draw_project_section(
        &self,
        scene: &mut Scene,
        tf: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        l: &InspectorLayout,
        x0: f64,
    ) {
        let c = &theme.tokens.colors;
        if l.project_format_y > 0.0 {
            // The cached words, not a fresh format!: `draw` runs for every
            // animation frame and this string only changes when the project's
            // format does.
            text_w(
                scene,
                tr,
                tf,
                &self.project_format_shown,
                12.0,
                c.text_primary,
                x0,
                l.project_format_y,
                TextWeight::Medium,
            );
        }
        if !l.project_frames.is_empty() {
            let rects: Vec<Rect> = l.project_frames.iter().map(|(_, r)| *r).collect();
            let labels: Vec<&str> = FRAME_PRESETS.iter().map(|(name, _, _)| *name).collect();
            // Nothing is marked unless the frame is exactly a preset: a
            // segmented control that always highlights something would claim
            // a 1440x1080 project was 1080p.
            let sel = l
                .project_frames
                .iter()
                .position(|((w, h), _)| *w == self.project.width && *h == self.project.height)
                .unwrap_or(usize::MAX);
            draw_segmented(scene, tf, tr, theme, &rects, &labels, sel, self.last_mouse);
        }
        if l.project_rate_y > 0.0 {
            text(
                scene,
                tr,
                tf,
                "Rate",
                11.5,
                c.text_dim,
                x0,
                l.project_rate_y,
            );
        }
        if !l.project_rates.is_empty() {
            let rects: Vec<Rect> = l.project_rates.iter().map(|(_, r)| *r).collect();
            let labels: Vec<String> = RATE_PRESETS.iter().map(|r| rate_words(*r)).collect();
            let refs: Vec<&str> = labels.iter().map(std::string::String::as_str).collect();
            let sel = l
                .project_rates
                .iter()
                .position(|(rate, _)| (rate - self.project.fps).abs() < 1e-6)
                .unwrap_or(usize::MAX);
            draw_segmented(scene, tf, tr, theme, &rects, &refs, sel, self.last_mouse);
        }
        if let Some(rect) = l.project_match {
            // The button names the source it will copy, so pressing it is not
            // a guess about which of several sources it meant.
            let label = self
                .format_source()
                .map(|m| format!("Match {}", m.name))
                .unwrap_or_else(|| "Match media".to_string());
            let shown = elide_w(&label, (rect.width() - 20.0) as f32, tr);
            draw_button(
                scene,
                tf,
                tr,
                theme,
                rect,
                &shown,
                false,
                rect.contains(self.last_mouse),
            );
        }
    }

    #[allow(
        clippy::too_many_lines,
        reason = "marker rows share one measured scroll coordinate system for rendering, hit targets, and empty-state actions"
    )]
    fn draw_marker_rows(
        &self,
        scene: &mut Scene,
        tf: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        l: &InspectorLayout,
        x0: f64,
    ) {
        let c = &theme.tokens.colors;
        let inspector_scale = self.inspector_ui_scale();
        for (m, row) in &l.marker_rows {
            let (name_rect, note_btn, rename, del) = marker_row_parts(*row);
            let hovered = row.contains(self.last_mouse);
            if hovered {
                fill_rrect(scene, tf, c.lift(0.35), *row, 6.0);
            }
            let Some((_, mk, _)) = self
                .project
                .all_markers()
                .into_iter()
                .find(|(_, k, _)| k.id == m.id)
            else {
                continue;
            };
            let col = clip_color(theme, mk.palette);
            scene.fill(
                Fill::NonZero,
                tf,
                col,
                None,
                &Circle::new(Point::new(row.x0 + 7.0, row.y0 + 13.0), 4.0),
            );
            let editing = self.marker_rename.as_ref().map(|(r, _)| r.id) == Some(m.id);
            let renaming = editing && self.marker_field == MarkerField::Name;
            if !renaming {
                let name = if mk.name.trim().is_empty() {
                    "Marker"
                } else {
                    mk.name.as_str()
                };
                let shown = elide_w(name, name_rect.width() as f32, tr);
                let baseline = text_baseline(name_rect, 12.0, tr);
                text(
                    scene,
                    tr,
                    tf,
                    &shown,
                    12.0,
                    c.text_primary,
                    name_rect.x0,
                    baseline,
                );
                // A clip marker says which clip it travels with, because
                // moving that clip moves the marker and nothing else would
                // explain why the time changed.
                let stamp = timecode(m.at, self.project.fps);
                let (sw2, _) = tr.measure(&stamp, 11.0, None);
                let stamp_col = if m.clip.is_some() {
                    c.text_tertiary
                } else {
                    c.text_dim
                };
                text(
                    scene,
                    tr,
                    tf,
                    &stamp,
                    11.0,
                    stamp_col,
                    note_btn.x0 - 8.0 - sw2 as f64,
                    baseline,
                );
            }
            // The note, on its own line under the name. Present only when
            // there is one, so an unannotated list stays a list of names.
            let editing_note = editing && self.marker_field == MarkerField::Note;
            if !mk.note.trim().is_empty() && !editing_note {
                let shown = elide_w(mk.note.trim(), (row.width() - 34.0) as f32, tr);
                let line = Rect::new(row.x0 + 16.0, row.y0 + 26.0, row.x1, row.y0 + 44.0);
                let base = text_baseline(line, 11.0, tr);
                text(scene, tr, tf, &shown, 11.0, c.text_tertiary, line.x0, base);
            }
            // Rename and delete only appear under the pointer, so a resting
            // list reads as names and times rather than as a wall of icons.
            if hovered {
                for (rect, icon, tint) in [
                    (note_btn, ui::icons::Icon::FileText, c.text_primary),
                    (rename, ui::icons::Icon::Pencil, c.text_primary),
                    (del, ui::icons::Icon::X, c.semantic.danger),
                ] {
                    if rect.contains(self.last_mouse) {
                        fill_rrect(scene, tf, with_alpha(tint, 0.18), rect, 4.0);
                    }
                    let g = 11.0;
                    let box_ = Rect::new(
                        rect.center().x - g / 2.0,
                        rect.center().y - g / 2.0,
                        rect.center().x + g / 2.0,
                        rect.center().y + g / 2.0,
                    );
                    ui::icons::draw_icon(
                        scene,
                        tf,
                        box_,
                        icon,
                        tint,
                        ui::icons::LUCIDE_DEFAULT_STROKE_PX,
                    );
                }
            }
        }
        if let Some(rect) = l.add_marker {
            if l.marker_rows.is_empty() {
                text(
                    scene,
                    tr,
                    tf,
                    "Nothing marked yet",
                    11.5,
                    c.text_dim,
                    x0,
                    rect.y0 - 16.0 * inspector_scale,
                );
            }
            draw_button(
                scene,
                tf,
                tr,
                theme,
                rect,
                ADD_MARKER_LABEL,
                false,
                rect.contains(self.last_mouse),
            );
        }
    }

    /// Lay out the markers section — its header, one row per marked instant,
    /// and the add button — advancing `y` past it.
    ///
    /// Shared by both inspector states: the list is the same whether a clip
    /// is selected or not, because a marker belongs to the project.
    fn layout_markers(&self, l: &mut InspectorLayout, x0: f64, x1: f64, y: &mut f64) {
        l.sections
            .push((Section::Markers, Rect::new(x0, *y, x1, *y + SECTION_H)));
        *y += SECTION_H + 4.0;
        if self.collapsed.contains(&Section::Markers) {
            return;
        }
        let markers = self.project.all_markers();
        if markers.is_empty() {
            // Room for the "nothing marked yet" line, which would otherwise
            // land on the section header it sits under.
            *y += 18.0;
        }
        for (at, m, on_clip) in markers {
            // A row grows for a note it has — and for one being typed into it
            // for the first time, or the editor would open over the row below.
            let writing_note = self.marker_field == MarkerField::Note
                && self.marker_rename.as_ref().map(|(r, _)| r.id) == Some(m.id);
            let h = marker_row_height(!m.note.trim().is_empty() || writing_note);
            l.marker_rows.push((
                MarkerRef {
                    id: m.id,
                    clip: on_clip,
                    at,
                },
                Rect::new(x0, *y, x1, *y + h),
            ));
            *y += h + 4.0;
        }
        l.add_marker = Some(Rect::new(x0, *y, x1, *y + 26.0));
        *y += 34.0;
    }

    /// Lay out the project's format section — the readout of what the frame
    /// currently is, the size presets, the rate presets, and the button that
    /// takes the selected source's shape — advancing `y` past it.
    ///
    /// Project-level like the markers above it, so it is laid out in both
    /// inspector states. The frame was hard-wired to 1920x1080 at 30: there
    /// was exactly one place in the model that ever wrote those three numbers
    /// and every other reference read them, so a vertical or 4K shoot had no
    /// way to say so and came out letterboxed into 16:9.
    fn layout_project(&self, l: &mut InspectorLayout, x0: f64, x1: f64, y: &mut f64) {
        l.sections
            .push((Section::Project, Rect::new(x0, *y, x1, *y + SECTION_H)));
        *y += SECTION_H + 4.0;
        if self.collapsed.contains(&Section::Project) {
            return;
        }
        // The readout's own line, then the two segmented rows under their
        // captions -- the same 20px a caption takes everywhere else in this
        // panel, so the three read as one convention.
        // The readout is drawn from `y` downwards, like every caption in this
        // panel, so it needs a caption's worth of room: at a 12px gap its
        // descenders sat on the preset row's top border.
        l.project_format_y = *y;
        *y += 24.0;
        let seg_w = (x1 - x0) / FRAME_PRESETS.len() as f64;
        for (i, (_, w, h)) in FRAME_PRESETS.iter().enumerate() {
            let sx0 = x0 + i as f64 * seg_w;
            l.project_frames
                .push(((*w, *h), Rect::new(sx0, *y, sx0 + seg_w - 3.0, *y + 24.0)));
        }
        *y += 32.0;
        l.project_rate_y = *y;
        *y += 20.0;
        let rate_w = (x1 - x0) / RATE_PRESETS.len() as f64;
        for (i, rate) in RATE_PRESETS.iter().enumerate() {
            let sx0 = x0 + i as f64 * rate_w;
            l.project_rates
                .push((*rate, Rect::new(sx0, *y, sx0 + rate_w - 3.0, *y + 24.0)));
        }
        *y += 32.0;
        // Offered only when there is a source to copy a shape from, and only
        // when that source has one -- audio and titles carry no frame.
        if self.format_source().is_some() {
            l.project_match = Some(Rect::new(x0, *y, x1, *y + 26.0));
            *y += 34.0;
        }
    }

    /// The source whose shape "Match media" would take: the bin selection
    /// when it has a frame, else the media under the selected clip, else the
    /// first source in the bin that has one.
    ///
    /// One resolver so the button, the press that runs it and the accessible
    /// node all name the same source -- a button that copied a shape from
    /// something other than the one it named would be worse than absent.
    fn format_source(&self) -> Option<&MediaItem> {
        let has_frame = |m: &&MediaItem| m.width > 0 && m.height > 0;
        self.selected_media
            .and_then(|id| self.project.media(id))
            .filter(has_frame)
            .or_else(|| {
                self.selected()
                    .and_then(|id| self.project.clip(id))
                    .and_then(|c| self.project.media(c.media))
                    .filter(has_frame)
            })
            .or_else(|| self.project.media.iter().find(has_frame))
    }

    /// Sub-rects of one effect row:
    /// `(key, reorder-up, reorder-down, slider, remove)`.
    ///
    /// The key control leads the row, exactly as it leads every other
    /// animatable row in the panel. An effect's intensity is as animatable as
    /// a scale or a mask edge, and it was the one such control with no
    /// diamond — so it was keyframable only from the command palette, which
    /// is not somewhere a user looking at the slider would think to go. What
    /// it costs is width off the effect's name, and the names are one word.
    fn effect_row_parts(row: Rect) -> (Rect, Rect, Rect, Rect, Rect) {
        let cy = row.center().y;
        let scale = (row.height() / 24.0).clamp(1.0, 2.0);
        // 22px tall targets — the row is 24px, so this is as close to the
        // toolkit's minimum as the row allows, and a real improvement on the
        // 16x18 boxes these were.
        let h = 11.0 * scale;
        let key = Rect::new(
            row.x0,
            cy - 9.0 * scale,
            row.x0 + KEY_BOX * scale,
            cy + 9.0 * scale,
        );
        let up = Rect::new(key.x1 + 4.0 * scale, cy - h, key.x1 + 26.0 * scale, cy + h);
        let down = Rect::new(up.x1 + scale, cy - h, up.x1 + 23.0 * scale, cy + h);
        let remove = Rect::new(row.x1 - 22.0 * scale, cy - h, row.x1, cy + h);
        let slider = Rect::new(
            row.x1 - 96.0 * scale,
            cy - 4.0 * scale,
            row.x1 - 30.0 * scale,
            cy + 4.0 * scale,
        );
        (key, up, down, slider, remove)
    }

    /// The maximum inspector scroll offset for the current selection.
    /// `content_height` is scroll-independent (the scroll is a pure translation),
    /// so the current layout's extent is the intrinsic content height.
    fn inspector_max_scroll(&self, r: &Regions) -> f64 {
        (self.inspector_layout(r).content_height - self.inspector_viewport(r).height()).max(0.0)
    }

    fn export_layout(&self, r: &Regions) -> ExportLayout {
        // Clamp to the window: a fixed 460x360 hung off both edges of a small
        // one, putting the Cancel and Export buttons outside the surface.
        let card_w = 460.0_f64.min(self.rect.width() - 32.0).max(240.0);
        let card_h = 360.0_f64.min(self.rect.height() - 32.0).max(200.0);
        let cx = (r.timeline.x0 + r.timeline.x1) / 2.0;
        let cy = (self.rect.y0 + self.rect.y1) / 2.0;
        let card = Rect::new(
            cx - card_w / 2.0,
            cy - card_h / 2.0,
            cx + card_w / 2.0,
            cy + card_h / 2.0,
        );
        let x0 = card.x0 + 24.0;
        let x1 = card.x1 - 24.0;
        let mut l = ExportLayout {
            card,
            ..Default::default()
        };
        // The three groups are spaced within the band between the header and
        // the footer rather than from fixed offsets off the card top. The old
        // constants assumed a 360px card AND that the header was one line: the
        // filename subtitle and the RESOLUTION label were both computed to land
        // at the same place, and drew through each other.
        let content_top = card.y0 + EXPORT_HEADER_H;
        let footer_top = (card.y1 - EXPORT_FOOTER_H).max(content_top + EXPORT_GROUP_H);
        let step = ((footer_top - content_top) / 3.0).clamp(EXPORT_GROUP_H, 64.0);
        let seg = |x0: f64, x1: f64, n: usize, y: f64| -> Vec<Rect> {
            let w = (x1 - x0) / n as f64;
            (0..n)
                .map(|i| {
                    Rect::new(
                        x0 + i as f64 * w,
                        y,
                        x0 + (i as f64 + 1.0) * w - 4.0,
                        y + 32.0,
                    )
                })
                .collect()
        };
        // Each group's label is drawn 18px above its segments, so the segment
        // row starts one label-height into the group's slot.
        let mut y = content_top + EXPORT_LABEL_H;
        for (resolution, rect) in
            Resolution::ALL
                .iter()
                .copied()
                .zip(seg(x0, x1, Resolution::ALL.len(), y))
        {
            l.resolution.push((resolution, rect));
        }
        y += step;
        for (container, rect) in
            Container::ALL
                .iter()
                .copied()
                .zip(seg(x0, x1, Container::ALL.len(), y))
        {
            l.container.push((container, rect));
        }
        y += step;
        for (frame_rate, rect) in
            FrameRate::ALL
                .iter()
                .copied()
                .zip(seg(x0, x1, FrameRate::ALL.len(), y))
        {
            l.fps.push((frame_rate, rect));
        }
        l.cancel = Rect::new(
            x0,
            card.y1 - 50.0,
            f64::midpoint(x0, x1) - 6.0,
            card.y1 - 18.0,
        );
        l.confirm = Rect::new(
            f64::midpoint(x0, x1) + 6.0,
            card.y1 - 50.0,
            x1,
            card.y1 - 18.0,
        );
        l
    }

    fn ensure_initial_selection(&mut self) {
        if self.seed.selection_initialised || self.project.clips.is_empty() {
            return;
        }
        let first = self
            .project
            .top_video_clip_at(self.playhead)
            .map(|c| c.id)
            .or_else(|| self.project.clips.first().map(|c| c.id));
        if let Some(id) = first {
            self.selection.select_only(id);
        }
        // The bin points at the clip that was selected, so the two panes open
        // agreeing with each other. It used to take `project.media.first()`,
        // which is the document's map-key order — arbitrary, and re-rolled
        // every run because the ids are fresh UUIDs, so the same project
        // opened twice highlighted two different bin rows.
        self.selected_media = self.project.media.first().map(|m| m.id);
        self.seed.selection_initialised = true;
    }

    /// The presentation context Ctrl+K's resource tree is built from: a snapshot
    /// of the open project plus the live playhead. Returns `None` when nothing
    /// is open (a blank project), so the palette shows an empty tree — a steady,
    /// level-triggered value recomputed each frame.
    pub(crate) fn video_resource_ctx(&self) -> Option<VideoResourceCtx> {
        Some(VideoResourceCtx {
            project: self.project.clone(),
            playhead: self.playhead,
            // The copy `draw` reads, so the palette cannot report a level the
            // meter on screen disagrees with.
            mix: self.mix.clone(),
        })
    }
}

#[derive(Default)]
struct InspectorLayout {
    valid: bool,
    opacity: Option<Rect>,
    volume: Option<Rect>,
    brightness: Option<Rect>,
    contrast: Option<Rect>,
    saturation: Option<Rect>,
    fade_in: Option<Rect>,
    fade_out: Option<Rect>,
    transition_y: f64,
    transitions: Vec<(Transition, Rect)>,
    /// Length slider for the incoming transition. `None` when the clip's
    /// transition is `None` — there is no ramp to size.
    transition_len: Option<Rect>,
    colors_y: f64,
    /// The link / unlink button, laid out only when the selection actually has
    /// one thing or the other to offer — a control that is present and inert
    /// says nothing.
    link: Option<Rect>,
    /// Baseline of the caption above it.
    link_label_y: f64,
    colors: Vec<(u8, Rect)>,
    /// y of the "EFFECTS" label; each row rect is a full-width effect row.
    effects_label_y: f64,
    effect_rows: Vec<Rect>,
    add_effect: Rect,
    split: Rect,
    delete: Rect,
    // ---- collapsible sections -------------------------------------------
    /// Section header rows, in draw order. Clicking one folds or unfolds it.
    sections: Vec<(Section, Rect)>,
    /// Every ranged control currently laid out, with the track to drag.
    nums: Vec<(NumField, Rect)>,
    /// Picture-in-picture presets: label, corner in `-1..1`, and the button.
    pip: Vec<(&'static str, (f64, f64), Rect)>,
    /// "Fill frame" — clears the transform back to the plain aspect-fit.
    reset_transform: Option<Rect>,
    /// Baseline y of the "Crop" caption above the two crop rows.
    crop_label_y: f64,
    /// Baseline y of the "Anchor" caption above the 3×3 grid.
    anchor_label_y: f64,
    /// The nine anchor presets: the point in source uv, and its cell.
    anchor: Vec<((f64, f64), Rect)>,
    /// Rate presets (`0.5×`, `1×`, …) as a segmented row.
    speed_presets: Vec<(f64, Rect)>,
    // ---- the lane's mix ---------------------------------------------------
    /// Baseline y of the line naming which lane these two knobs belong to.
    /// The panel is otherwise entirely about one clip, so a control that
    /// quietly edits something shared has to say what it is editing.
    mix_track_y: f64,
    /// The lane's level meter, at full panel width above its fader — the same
    /// measurement the track head shows, where the number is being set.
    mix_meter: Option<Rect>,
    /// Back to unity and centre. Laid out only when the lane is somewhere
    /// else, because a reset that resets nothing is a control that lies about
    /// having something to undo.
    mix_reset: Option<Rect>,
    /// The reverse toggle.
    reverse: Option<Rect>,
    // ---- colour key --------------------------------------------------------
    /// Switches the key on and off. The sliders below it only appear once it
    /// is on, so an unkeyed clip does not carry four dead controls.
    key_enable: Option<Rect>,
    /// Baseline y of the "Key colour" caption over the swatches.
    key_color_y: f64,
    /// Preset key colours, and the swatch each is picked from.
    key_colors: Vec<([f64; 3], Rect)>,
    /// Samples the key colour out of the frame under the playhead.
    key_pick: Option<Rect>,
    // ---- mask ---------------------------------------------------------------
    /// Shape choice, as a segmented row: Off / Rect / Ellipse.
    mask_shapes: Vec<(crate::model::MaskShape, Rect)>,
    /// Snaps the mask onto whatever the transform currently puts on screen.
    mask_fit: Option<Rect>,
    /// Keep the outside rather than the inside.
    mask_invert: Option<Rect>,
    // ---- title ------------------------------------------------------------
    /// The words, edited inline through the shared editable label.
    title_text: Option<Rect>,
    title_align: Vec<(TextAlign, Rect)>,
    /// The font-family chooser: a button that opens the shared dropdown.
    title_font: Option<Rect>,
    /// Preset colours for the title's own (authored, non-palette) colour.
    title_colors: Vec<([f64; 4], Rect)>,
    title_bold: Option<Rect>,
    title_italic: Option<Rect>,
    // ---- tone curves ------------------------------------------------------
    /// Which channel the editor is showing, as a segmented row.
    curve_channels: Vec<(crate::curve::Channel, Rect)>,
    /// The square the curve is drawn and dragged in.
    curve_canvas: Option<Rect>,
    /// Puts the shown channel back to the straight line.
    curve_reset: Option<Rect>,
    // ---- keyframes --------------------------------------------------------
    /// The key control at the head of each animatable row.
    keys: Vec<(Param, Rect)>,
    // ---- project format ---------------------------------------------------
    /// Baseline y of the readout that states the project's current format.
    project_format_y: f64,
    /// Frame-size presets as a segmented row: the dimensions each writes.
    project_frames: Vec<((u32, u32), Rect)>,
    /// Baseline y of the "Rate" caption over the rate row.
    project_rate_y: f64,
    project_rates: Vec<(f64, Rect)>,
    /// Takes the selected source's shape as the project's -- the button
    /// behind the one thing a preset row cannot offer, which is whatever the
    /// footage actually is.
    project_match: Option<Rect>,
    // ---- markers ----------------------------------------------------------
    marker_rows: Vec<(MarkerRef, Rect)>,
    /// Sub-rects of the row above: the name (click to rename) and the delete.
    add_marker: Option<Rect>,
    /// Total content extent (px) from the inspector top, for scroll clamping.
    content_height: f64,
}

impl InspectorLayout {
    /// Project a logical inspector layout into physical pixels.  The layout
    /// owns every interactive rectangle, so doing this once keeps painting,
    /// hit testing, the scrollbar's extent and AT focus rings identical.
    fn scale_from(&mut self, ox: f64, oy: f64, scale: f64) {
        if (scale - 1.0).abs() < f64::EPSILON {
            return;
        }
        let map = |r: Rect| {
            Rect::new(
                ox + (r.x0 - ox) * scale,
                oy + (r.y0 - oy) * scale,
                ox + (r.x1 - ox) * scale,
                oy + (r.y1 - oy) * scale,
            )
        };
        let one = |r: &mut Option<Rect>| *r = r.map(map);
        one(&mut self.opacity);
        one(&mut self.volume);
        one(&mut self.brightness);
        one(&mut self.contrast);
        one(&mut self.saturation);
        one(&mut self.fade_in);
        one(&mut self.fade_out);
        one(&mut self.transition_len);
        one(&mut self.link);
        one(&mut self.add_marker);
        one(&mut self.reset_transform);
        one(&mut self.mix_meter);
        one(&mut self.mix_reset);
        one(&mut self.reverse);
        one(&mut self.key_enable);
        one(&mut self.key_pick);
        one(&mut self.mask_fit);
        one(&mut self.mask_invert);
        one(&mut self.title_text);
        one(&mut self.title_font);
        one(&mut self.title_bold);
        one(&mut self.title_italic);
        one(&mut self.curve_canvas);
        one(&mut self.curve_reset);
        one(&mut self.project_match);
        self.split = map(self.split);
        self.delete = map(self.delete);
        self.transition_y = oy + (self.transition_y - oy) * scale;
        self.colors_y = oy + (self.colors_y - oy) * scale;
        self.link_label_y = oy + (self.link_label_y - oy) * scale;
        self.effects_label_y = oy + (self.effects_label_y - oy) * scale;
        self.crop_label_y = oy + (self.crop_label_y - oy) * scale;
        self.anchor_label_y = oy + (self.anchor_label_y - oy) * scale;
        self.mix_track_y = oy + (self.mix_track_y - oy) * scale;
        self.key_color_y = oy + (self.key_color_y - oy) * scale;
        self.project_format_y = oy + (self.project_format_y - oy) * scale;
        self.project_rate_y = oy + (self.project_rate_y - oy) * scale;
        for (_, r) in &mut self.transitions {
            *r = map(*r);
        }
        for (_, r) in &mut self.colors {
            *r = map(*r);
        }
        for r in &mut self.effect_rows {
            *r = map(*r);
        }
        for (_, r) in &mut self.sections {
            *r = map(*r);
        }
        for (_, r) in &mut self.nums {
            *r = map(*r);
        }
        for (_, _, r) in &mut self.pip {
            *r = map(*r);
        }
        for (_, r) in &mut self.anchor {
            *r = map(*r);
        }
        for (_, r) in &mut self.speed_presets {
            *r = map(*r);
        }
        for (_, r) in &mut self.key_colors {
            *r = map(*r);
        }
        for (_, r) in &mut self.mask_shapes {
            *r = map(*r);
        }
        for (_, r) in &mut self.title_align {
            *r = map(*r);
        }
        for (_, r) in &mut self.title_colors {
            *r = map(*r);
        }
        for (_, r) in &mut self.curve_channels {
            *r = map(*r);
        }
        for (_, r) in &mut self.keys {
            *r = map(*r);
        }
        for (_, r) in &mut self.project_frames {
            *r = map(*r);
        }
        for (_, r) in &mut self.project_rates {
            *r = map(*r);
        }
        for (_, r) in &mut self.marker_rows {
            *r = map(*r);
        }
    }

    /// The track laid out for `field`, if it is on screen right now.
    fn num(&self, field: NumField) -> Option<Rect> {
        self.nums.iter().find(|(f, _)| *f == field).map(|(_, r)| *r)
    }
}

/// A clip's animatable geometry, sampled at one instant.
///
/// Named rather than a tuple because both summaries read all five and getting
/// two of them the wrong way round is invisible in a `(f64, f64, f64, f64,
/// f64)`.
#[derive(Clone, Copy, Debug)]
struct GeometryNow {
    scale_x: f64,
    scale_y: f64,
    pos_x: f64,
    pos_y: f64,
    rotation: f64,
}

impl GeometryNow {
    /// Whether the picture is anywhere other than the plain aspect-fit.
    fn is_placed(self) -> bool {
        (self.scale_x - 1.0).abs() > 1e-6
            || (self.scale_y - 1.0).abs() > 1e-6
            || self.pos_x.abs() > 1e-6
            || self.pos_y.abs() > 1e-6
            || self.rotation.abs() > 1e-6
    }
}

/// The three states a key control shows, which have to stay distinguishable:
/// a parameter that is not animated at all, one that is animated with no key
/// on the playhead, and one with a key right here.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum KeyState {
    Off,
    Between,
    OnKey,
}

/// Which of a marker's two texts an inline edit is on.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
enum MarkerField {
    #[default]
    Name,
    /// Free text hanging off the marker — why this instant matters, which a
    /// name of a few words cannot carry.
    Note,
}

/// Sub-rects of a marker row: `(name, note, rename, delete)`.
///
/// Renaming gets its own target rather than living on the name text, because
/// the row's main job is navigation: a marker exists to be jumped to, and
/// that has to be what clicking it does. `name` is where the label is drawn
/// and where the inline editor is placed, not a hit target.
fn marker_row_parts(row: Rect) -> (Rect, Rect, Rect, Rect) {
    // The icons sit on the row's first line, so a row grown to carry a note
    // keeps them beside the name rather than floating down beside the note.
    let cy = row.y0 + 13.0;
    let del = Rect::new(row.x1 - 22.0, cy - 11.0, row.x1, cy + 11.0);
    let rename = Rect::new(del.x0 - 24.0, cy - 11.0, del.x0 - 2.0, cy + 11.0);
    let note = Rect::new(rename.x0 - 24.0, cy - 11.0, rename.x0 - 2.0, cy + 11.0);
    let name = Rect::new(row.x0 + 16.0, row.y0, note.x0 - 60.0, row.y0 + 26.0);
    (name, note, rename, del)
}

/// How tall a marker's row is: one line, or two when it carries a note.
fn marker_row_height(has_note: bool) -> f64 {
    if has_note { 44.0 } else { 26.0 }
}

/// The colours a title can be set to from the inspector.
///
/// A title's colour is authored content, not chrome: the user picked it and it
/// must survive a theme change, so unlike a clip's label these are stored as
/// real values rather than palette slots. The set is deliberately small and
/// legible against video — whites and blacks for titling, then a few
/// saturated hues — with a full picker left to Ctrl+K.
const TITLE_COLORS: [[f64; 4]; 8] = [
    [1.0, 1.0, 1.0, 1.0],
    [0.08, 0.08, 0.09, 1.0],
    [0.98, 0.83, 0.25, 1.0],
    [0.95, 0.42, 0.31, 1.0],
    [0.40, 0.78, 0.98, 1.0],
    [0.44, 0.85, 0.55, 1.0],
    [0.79, 0.55, 0.96, 1.0],
    [1.0, 1.0, 1.0, 0.55],
];

/// Rate presets offered as a segmented row beside the rate slider.
const SPEED_PRESETS: [f64; 5] = [0.25, 0.5, 1.0, 2.0, 4.0];

/// Frame-size presets for the project, as `(label, width, height)`.
///
/// The set is the shapes people actually shoot, not a resolution ladder: two
/// landscape sizes, 4K, vertical and square. Anything else is a number, and
/// the `set-frame-size` action takes numbers — a picker is for the shapes
/// worth one press, and until now there was no press at all.
const FRAME_PRESETS: [(&str, u32, u32); 5] = [
    ("720p", 1280, 720),
    ("1080p", 1920, 1080),
    ("4K", 3840, 2160),
    ("9:16", 1080, 1920),
    ("1:1", 1080, 1080),
];

/// Project frame-rate presets. Five that fit the column; the
/// `set-frame-rate` action takes any rate, and a clip's own rate arrives
/// through it when a blank project adopts its first clip's shape.
const RATE_PRESETS: [f64; 5] = [24.0, 25.0, 30.0, 50.0, 60.0];

/// The project's format in words: "1920 x 1080 - 30 fps".
///
/// A free function so the draw path, the folded header's summary and the
/// accessible node all read from one place; the drawn copy is cached against
/// the numbers it was built from (see `VideoEditorApp::project_format_words`)
/// rather than being formatted per frame.
fn format_words(width: u32, height: u32, fps: f64) -> String {
    format!("{width} \u{d7} {height} \u{b7} {} fps", rate_words(fps))
}

/// A frame rate with no trailing zeros: "30", not "30.00", and "23.98" for a
/// rate that really is fractional.
fn rate_words(fps: f64) -> String {
    if (fps - fps.round()).abs() < 1e-6 {
        #[expect(
            clippy::cast_possible_truncation,
            reason = "rates are clamped to 1..240 by Project::set_fps"
        )]
        return format!("{}", fps.round() as i64);
    }
    format!("{fps:.2}")
}

#[derive(Default)]
struct ExportLayout {
    card: Rect,
    resolution: Vec<(Resolution, Rect)>,
    container: Vec<(Container, Rect)>,
    fps: Vec<(FrameRate, Rect)>,
    cancel: Rect,
    confirm: Rect,
}

impl VideoEditorApp {
    /// Advance frame-owned state and resolve geometry once before painting.
    /// Keeping this lifecycle work separate makes the draw pass read as the
    /// actual visual stacking order.
    fn prepare_draw_frame(&mut self, theme: &Theme) -> Regions {
        self.advance_playback();
        #[cfg(any(feature = "gst", feature = "decode"))]
        self.pump_monitor();
        self.advance_flap_with_motion(theme.reduce_motion);
        self.poll_export();
        #[cfg(feature = "decode")]
        self.pump_preview_decode();

        let mut regions = self.regions();
        if !self.shot_lifecycle.interaction_applied
            && self.shot_scenario.is_interaction()
            && self.rect.width() > 1.0
        {
            self.apply_shot_interaction(&regions);
            self.shot_lifecycle.interaction_applied = true;
            // The scenario may move a side flap, invalidating the first pass.
            regions = self.regions();
        }
        regions
    }
}

// ==========================================================================
// Widget impl
// ==========================================================================
impl VideoEditorApp {
    fn draw_inspector_editors(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
        regions: &Regions,
    ) {
        if !self.inspector_is_visible(regions) {
            return;
        }
        let layout = self.inspector_layout(regions);
        // Enumerating installed families needs the renderer, and this is the
        // only draw path that has one — so the cache is filled here and the
        // picker is built from it on press.
        if layout.title_font.is_some() && self.system_fonts.is_empty() {
            self.system_fonts = tr.system_families();
        }
        if let (Some((_, dropdown, _)), Some(rect)) = (self.font_picker.as_mut(), layout.title_font)
        {
            dropdown.apply_layout_external(rect);
            dropdown.draw(scene, transform, tr, theme, presence);
        }
        if let (Some((media_id, field)), Some(rect)) = (self.title_edit.as_mut(), layout.title_text)
        {
            let media_id = *media_id;
            field.apply_layout_external(rect.inset(-4.0));
            field.draw(scene, transform, tr, theme, presence);
            crate::presence::draw_peer_field_carets(
                &mut crate::presence::PeerPaintCtx::new(scene, transform, tr, theme, presence),
                crate::presence::FIELD_TITLE_WORDS,
                media_id,
                |byte| field.caret_rect(theme, byte),
            );
        }
        let marker_row = self
            .marker_rename
            .as_ref()
            .and_then(|(marker, _)| {
                layout
                    .marker_rows
                    .iter()
                    .find(|(row, _)| row.id == marker.id)
            })
            .map(|(_, row)| self.marker_edit_rect(*row));
        if let (Some((_, label)), Some(rect)) = (self.marker_rename.as_mut(), marker_row) {
            label.apply_layout_external(rect);
            label.draw(scene, transform, tr, theme, presence);
        }
    }
}

impl Widget for VideoEditorApp {
    fn local_resource_operation_handlers(&self) -> &'static [&'static str] {
        &[crate::actions::VIDEO_EDITOR_OPERATION_TYPE]
    }

    fn sync(&mut self, state: &mut AutoCommit, _path: &ObjId) {
        if std::mem::take(&mut self.project_sync.save_requested) {
            self.save_project(state);
        }
        if !self.shot_lifecycle.seeded {
            // The live app opens on a blank project; only the hermetic demo
            // build seeds the bundled sample project into a fresh document.
            if self.seed.demo && VideoDoc::is_empty(state) {
                VideoDoc::seed_sample(state);
            }
            self.shot_lifecycle.seeded = true;
        }
        if let Some((invocation, operation)) = self.resource_pending_operation.take() {
            use crate::actions::VideoEditorOperation;
            match &operation {
                VideoEditorOperation::OpenProject { path: Some(path) } => {
                    self.begin_open(state, PathBuf::from(path))
                }
                VideoEditorOperation::SaveProject { path: Some(path) }
                | VideoEditorOperation::SaveProjectAs { path: Some(path) } => {
                    self.write_project_to(state, PathBuf::from(path))
                }
                _ => {}
            }
            self.resource_invocation = Some(invocation);
        }
        // The document's heads are the cheap "something actually changed"
        // signal. Reloading the model every frame says nothing; a new head
        // means a real edit, local or from a peer.
        let heads = state.get_heads();
        if heads != self.doc_heads {
            self.doc_heads = heads;
            self.project_sync.reload_pending = true;
            #[cfg(any(feature = "gst", feature = "decode"))]
            {
                self.media_pipeline.monitor_stale = true;
            }
        }
        // Rebuilding the whole model from the CRDT is the most expensive thing
        // in this pass — measured at ~1.1ms on the sample project, roughly a
        // third of the frame budget, and it grows with the project while
        // `get_heads` does not. `sync` runs every frame for as long as
        // anything animates (playback, a running render, an open flap), so
        // this used to re-derive an unchanged model sixty times a second.
        //
        // While a clip-editing drag is in flight, the live preview lives only
        // in the in-memory project — the CRDT isn't written until the drag
        // commits — so don't clobber it by reloading a stale document
        // mid-drag. The reload stays owed and lands the moment the drag lets
        // go, so an edit that arrived meanwhile is not lost.
        if self.project_sync.reload_pending && !self.drag_is_editing() {
            self.project = VideoDoc::load_project(state);
            self.clip_edit_bases.clear();
            self.project_sync.reload_pending = false;
        }
        // Bring in any pending / portal-picked media as real clips (this pushes
        // straight into `self.project.media` and the CRDT).
        self.drain_import(state);
        // A project open/save chooser that has come back. Drained after the
        // model load above so an Open replaces a project that is current, and
        // before the passes below so they see what was opened.
        self.drain_project_portal(state);
        self.finish_resource_file_operation_if_ready();
        // Drop selected clips that vanished (e.g. a remote delete), keeping
        // whatever is left selected rather than clearing the lot.
        self.selection.retain_existing(&self.project);
        // Drop a rename / menu whose target media vanished (e.g. remote delete).
        if let Some((mid, _)) = &self.bin_rename
            && self.project.media(*mid).is_none()
        {
            self.bin_rename = None;
        }
        match self.menu_target {
            Some(MenuTarget::Media(mid)) if self.project.media(mid).is_none() => {
                self.bin_menu.close();
                self.menu_target = None;
            }
            Some(MenuTarget::Track(tid)) if self.project.track(tid).is_none() => {
                self.bin_menu.close();
                self.menu_target = None;
            }
            Some(MenuTarget::Clip(cid)) if self.project.clip(cid).is_none() => {
                self.bin_menu.close();
                self.menu_target = None;
            }
            _ => {}
        }
        if let Some(bd) = self.bin_drag
            && self.project.media(bd.media).is_none()
        {
            self.bin_drag = None;
        }
        self.ensure_initial_selection();
        // Resolve which sources actually have bytes here. The reference always
        // replicates; the content is fetched lazily, so a project opened on a
        // second peer legitimately holds media this device has never seen. The
        // editor must say that in words rather than drawing a stand-in picture
        // and leaving the user to guess.
        self.projected_media = self
            .project
            .media
            .iter()
            .filter(|m| !m.source().hydration.is_local())
            .map(|m| m.id)
            .collect();
        // Keep audio waveforms current: drain completed background computes, then
        // request (off-thread) or warm (synchronous, for --shots) each source.
        self.waveforms.drain();
        let audio: Vec<(MediaId, crate::media::MediaSource)> = self
            .project
            .media
            .iter()
            // A picture whose file carries sound is asked for peaks as well:
            // its linked audio clip draws a waveform, and without this the
            // sound half of every shot was a flat line — which is what the
            // editor draws for "no audio here".
            .filter(|m| m.kind == MediaKind::Audio || m.has_audio)
            .map(|m| (m.id, m.source()))
            .collect();
        for (id, src) in audio {
            if self.render_pipeline.warm_waveforms_sync {
                self.waveforms.warm_blocking(id, &src);
            } else {
                self.waveforms.request(id, &src);
            }
        }
        // Thumbnails for visual sources: a representative frame now (cached),
        // and a real one off-thread under the decode feature.
        self.thumbnails.drain();
        let visual: Vec<(MediaId, u8, crate::media::MediaSource, f64)> = self
            .project
            .media
            .iter()
            .filter(|m| m.kind.is_visual())
            .map(|m| {
                (
                    m.id,
                    m.palette,
                    m.source(),
                    (m.duration * 0.3).clamp(0.2, 3.0),
                )
            })
            .collect();
        for (id, palette, src, at) in visual {
            self.thumbnails.ensure(id, palette, &src, at);
        }
        // Apply a one-shot capture scenario once the project is loaded.
        if !self.shot_lifecycle.applied
            && self.shot_scenario != ShotScenario::Live
            && !self.project.clips.is_empty()
        {
            self.apply_shot_scenario(state);
            self.shot_lifecycle.applied = true;
        }
        // Let the meters fall, and take the one copy the frame will draw
        // from. After the scenario, so a staged capture's levels survive into
        // the frame that captures them.
        self.pump_mix_meter();
        // Keep the real preview decoder tracking the clip under the playhead so
        // the `--shots` warm-up (which pumps `sync`) captures a decoded frame.
        #[cfg(feature = "decode")]
        self.pump_preview_decode();
    }

    fn needs_idle_sync(&self) -> bool {
        // Keep syncing until the demo/shared project is loaded so a
        // freshly-spawned, input-free instance still seeds + paints. The live
        // app opens blank, so once seeded it must not idle-sync forever. Also
        // keep syncing while an import is pending so its outcome is drained.
        !self.shot_lifecycle.seeded
            || (self.seed.demo && self.project.clips.is_empty())
            || self.import_rx.is_some()
            || !self.pending_imports.is_empty()
            // A project chooser is a round-trip in flight exactly like an
            // import; without this the chosen file would sit undrained until
            // the user happened to move the mouse.
            || self.project_rx.is_some()
            || self.waveforms.has_pending() // keep syncing until peaks land
            || self.thumbnails.has_pending() // …and until real frames land
    }

    fn needs_animation_frame(&self) -> bool {
        self.playback.playing
            || self.shuttle != 0.0
            || self.flap_animating()
            || self.import_rx.is_some()
            || self.bin_rename.is_some() // a live rename blinks its caret
            || self.export_job.is_some() // a running render moves its progress bar
            || self.export_dest_rx.is_some() // …and a picker is still an action in flight
            // A meter that stopped mid-fall reads as a level that is still
            // there. Frames keep coming until the bars are down.
            || self.mix_meter_is_live()
    }

    #[allow(
        clippy::expect_used,
        reason = "a complete local root style is infallible; Widget::build_layout cannot return Result"
    )]
    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        self.text_scale = f64::from(theme.text_scale);
        let node = taffy
            .new_leaf(Style {
                size: Size {
                    width: Dimension::percent(1.0),
                    height: Dimension::percent(1.0),
                },
                ..Default::default()
            })
            // A leaf made from this complete local style cannot violate a
            // Taffy tree invariant; the host API cannot surface the error.
            .expect("a complete video-editor root leaf is valid");
        node_map.insert(node, std::ptr::from_mut::<dyn Widget>(self));
        node
    }

    #[allow(
        clippy::expect_used,
        reason = "the host supplies the exact node produced by build_layout in the same lifecycle"
    )]
    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        let layout = taffy
            .layout(node)
            // The node was just installed by `build_layout`; a missing node
            // would be a host lifecycle violation rather than user input.
            .expect("the video-editor root node remains in the Taffy tree");
        self.rect = Rect::new(
            origin.x + f64::from(layout.location.x),
            origin.y + f64::from(layout.location.y),
            origin.x + f64::from(layout.location.x) + f64::from(layout.size.width),
            origin.y + f64::from(layout.location.y) + f64::from(layout.size.height),
        );
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        let r = self.prepare_draw_frame(theme);
        // Everything below draws the room from here. In the live app that is
        // the host's own context, fed by the collaboration socket; a capture
        // scenario can stage one instead (see `seed_shot_collaborators`).
        // Cloned because the draws below need `&mut self`.
        let staged = self.shot_presence.clone();
        let presence: &PresenceCtx = staged.as_ref().unwrap_or(presence);
        // Keep the inspector scroll within the current selection's content.
        let max_scroll = self.inspector_max_scroll(&r);
        self.inspector_scroll = self.inspector_scroll.clamp(0.0, max_scroll);
        // Same for the bin: deleting sources shortens the list under us.
        let bin_max = self.bin_max_scroll(&r);
        self.bin_scroll = self.bin_scroll.clamp(0.0, bin_max);
        // Keep the playhead on screen while it is moving under its own steam,
        // and the horizontal scroll inside a timeline whose length may have
        // just changed under us (a delete, a remote edit).
        if std::mem::take(&mut self.playback.chase_playhead) {
            self.scroll_to_time(self.playhead, &r);
        }
        self.clamp_scroll(&r);
        // The format readout's words, re-built only when the format has moved
        // — three scalar comparisons a frame instead of a `format!`. It runs
        // here rather than in `sync` because the inspector paints from it: a
        // format changed after the last reload (a local press, or a capture
        // scenario staged in this very pass) would otherwise paint the numbers
        // the project had a frame ago.
        self.refresh_project_format_words();
        // Request the filmstrip frames the timeline is about to draw, now that
        // the scroll is settled and we know which clips are on screen.
        self.request_filmstrips(&r);
        // Composite this frame's preview picture before anything paints, so
        // `draw_preview` only has to blit it.
        self.pump_compositor(&r);
        self.draw_chrome(scene, transform, tr, theme, &r);
        // Editor body. When the bin is docked it draws in its column; when
        // it's an overlay flap it is drawn LAST so it floats over the body
        // behind a scrim.
        if !r.bin_overlay {
            self.draw_bin(scene, transform, tr, theme, &r);
        }
        self.draw_preview(scene, transform, tr, theme, &r);
        self.draw_transport(scene, transform, tr, theme, &r);
        // Docked, the inspector draws in its column. As an overlay it floats
        // over the preview it is editing — no scrim, so the picture the
        // sliders are changing stays visible while they change it.
        if !r.inspector_overlay {
            self.draw_inspector(scene, transform, tr, theme, &r);
        }
        self.draw_timeline(scene, transform, tr, theme, presence, &r);
        if r.inspector_overlay && self.inspector_flap.reveal() > 0.001 {
            self.draw_inspector(scene, transform, tr, theme, &r);
        }
        if r.bin_overlay {
            // Dimming scrim, then the bin sliding in on top of it.
            if r.scrim.width() > 0.0 {
                // The themed scrim token gives the hue (raw black over a light
                // theme reads as a hole); the *strength* is ours. The token is
                // built with `from_rgb8`/`mix8`, so its own alpha is 1.0 in
                // every theme — multiplying by it dimmed nothing and painted an
                // opaque slab over the preview, transport, inspector and
                // timeline, leaving a narrow window looking like a black void.
                let a = self.bin_flap.reveal() * SCRIM_ALPHA;
                let scrim = theme.tokens.colors.surfaces.scrim;
                fill_rect(scene, transform, with_alpha(scrim, a), r.scrim);
            }
            if self.bin_flap.reveal() > 0.001 {
                self.draw_bin(scene, transform, tr, theme, &r);
            }
        }
        // Live bin→timeline drag: drop-lane highlight, insertion marker + ghost.
        self.draw_bin_drag(scene, transform, tr, theme, &r);
        // Inline rename field, drawn over the bin item's name band.
        self.draw_bin_rename(scene, transform, tr, theme, presence, &r);
        // Inline inspector editors paint after the panel so carets and open
        // pickers float over its body.
        self.draw_inspector_editors(scene, transform, tr, theme, presence, &r);
        if self.presentation.export_open {
            self.draw_export_modal(scene, transform, tr, theme, &r);
        }
        if self.goto_entry.is_some() {
            self.draw_goto(scene, transform, tr, theme, &r);
        }
        if self.navigation.help_open {
            self.draw_help_sheet(scene, transform, tr, theme, &r);
        }
        self.draw_file_drop_invite(scene, transform, tr, theme);
        self.draw_project_notice(scene, transform, tr, theme, &r);
        // Above the dialog's own scrim: a render that is still going is
        // reported whether or not the dialog that started it is up.
        self.draw_export_chip(scene, transform, tr, theme, &r);
        if self.pending_open.is_some() {
            self.draw_discard_dialog(scene, transform, tr, theme);
        }
        // The context menus float above everything else.
        self.bin_menu.draw(scene, transform, tr, theme, presence);
        self.project_menu
            .draw(scene, transform, tr, theme, presence);
    }

    #[allow(
        clippy::unwrap_used,
        reason = "each inline-editor Option is checked immediately before borrowing its mutable editor for this event dispatch"
    )]
    fn handle_event(
        &mut self,
        event: &WindowEvent,
        scale_factor: f64,
        modifiers: &winit::event::Modifiers,
        state: &mut AutoCommit,
        path: &ObjId,
        handled: &mut bool,
        focus_target: &mut Option<NodeId>,
        node_id: NodeId,
        taffy: &TaffyTree<()>,
    ) {
        let r = self.regions();
        let bin_overlay_open = self.bin_overlay_is_open();
        // An in-flight inline rename owns input: forward every event to the
        // shared editable label, then reconcile its outcome. A blur-commit
        // (click outside) is intentionally not consumed, so the same click also
        // reaches the host — native inline-rename behaviour.
        if self.bin_rename.is_some() {
            let name_rect = self
                .bin_item_rects(&r)
                .into_iter()
                .find(|(m, _)| Some(*m) == self.bin_rename.as_ref().map(|(id, _)| *id))
                .map(|(_, row)| self.bin_name_rect(row));
            let (mid, outcome) = {
                let (mid, label) = self.bin_rename.as_mut().unwrap();
                if let Some(rect) = name_rect {
                    label.apply_layout_external(rect);
                }
                label.handle_event(
                    event,
                    scale_factor,
                    modifiers,
                    state,
                    path,
                    handled,
                    focus_target,
                    node_id,
                    taffy,
                );
                (*mid, label.take_outcome())
            };
            match outcome {
                EditOutcome::Committed(name) => {
                    self.commit_rename(state, mid, &name);
                    self.bin_rename = None;
                }
                EditOutcome::Cancelled => self.bin_rename = None,
                EditOutcome::None => {}
            }
            if *handled {
                return;
            }
        }
        // The font chooser, while it is open, gets the event first — it is a
        // list floating over the panel, so a click in it must not also reach
        // whatever it is covering.
        if self.font_picker.is_some() {
            let l = self.inspector_layout(&r);
            if let Some((_, dd, local)) = self.font_picker.as_mut() {
                if let Some(rect) = l.title_font {
                    dd.apply_layout_external(rect);
                }
                let root = automerge::ROOT;
                dd.handle_event(
                    event,
                    scale_factor,
                    modifiers,
                    local,
                    &root,
                    handled,
                    focus_target,
                    node_id,
                    taffy,
                );
            }
            if let Some((mid, family)) = self.take_font_choice() {
                let same = self
                    .project
                    .media(mid)
                    .and_then(|m| m.text.as_ref())
                    .is_some_and(|t| t.font == family);
                if !same {
                    self.edit_title(state, move |t| t.font = family);
                }
            }
            if *handled {
                return;
            }
        }
        // The title's words, edited in place in the inspector through the
        // shared multiline field. Committing writes the media item, so the
        // title's picture, its bin row and every clip that placed it all
        // update from one action.
        //
        // Enter is a line break here, so it cannot also be Accept. Ctrl+Enter
        // accepts, Escape cancels, and a click anywhere else accepts what was
        // typed — losing a title to a stray click would be the worse
        // outcome, and the field says which keys do what under the box.
        if self.title_edit.is_some() {
            if let Some(done) = title_edit_verdict(event, modifiers) {
                let (mid, words) = {
                    let (mid, field) = self.title_edit.as_ref().unwrap();
                    (*mid, field.value().to_string())
                };
                if done {
                    self.commit_title_text(state, mid, &words);
                }
                self.title_edit = None;
                *handled = true;
                return;
            }
            let l = self.inspector_layout(&r);
            let outside_press = matches!(
                event,
                WindowEvent::MouseInput {
                    state: ElementState::Pressed,
                    ..
                }
            ) && l
                .title_text
                .is_none_or(|rect| !rect.inset(-4.0).contains(self.last_mouse));
            if outside_press {
                let (mid, words) = {
                    let (mid, field) = self.title_edit.as_ref().unwrap();
                    (*mid, field.value().to_string())
                };
                self.commit_title_text(state, mid, &words);
                self.title_edit = None;
                // Not marked handled: the click still lands on whatever it
                // was aimed at, which is what makes accepting-on-click-away
                // feel like nothing happened rather than like a lost click.
            }
            if let Some((_, field)) = self.title_edit.as_mut() {
                if let Some(rect) = l.title_text {
                    field.apply_layout_external(rect.inset(-4.0));
                }
                field.handle_event(
                    event,
                    scale_factor,
                    modifiers,
                    state,
                    path,
                    handled,
                    focus_target,
                    node_id,
                    taffy,
                );
            }
            if *handled {
                return;
            }
        }
        // A marker's name, edited in place in the inspector's list.
        if self.marker_rename.is_some() {
            let l = self.inspector_layout(&r);
            let row = self
                .marker_rename
                .as_ref()
                .and_then(|(m, _)| l.marker_rows.iter().find(|(r, _)| r.id == m.id))
                .map(|(_, row)| self.marker_edit_rect(*row));
            let (target, outcome) = {
                let (target, label) = self.marker_rename.as_mut().unwrap();
                if let Some(rect) = row {
                    label.apply_layout_external(rect);
                }
                label.handle_event(
                    event,
                    scale_factor,
                    modifiers,
                    state,
                    path,
                    handled,
                    focus_target,
                    node_id,
                    taffy,
                );
                (*target, label.take_outcome())
            };
            match outcome {
                EditOutcome::Committed(name) => {
                    // A refused write leaves the editor open. Closing it
                    // regardless put the old name back with nothing said, so
                    // a marker whose name you had just cleared looked as
                    // though it had simply ignored you.
                    if self.commit_marker_rename(state, target, &name) {
                        self.marker_rename = None;
                    } else {
                        self.project_notice =
                            Some(Notice::failed("A marker needs a name to be found by"));
                    }
                }
                EditOutcome::Cancelled => self.marker_rename = None,
                EditOutcome::None => {}
            }
            if *handled {
                return;
            }
        }
        // An open bin context menu is a transient overlay that owns clicks/keys.
        if self.bin_menu.is_open() {
            self.bin_menu.handle_event(
                event,
                scale_factor,
                modifiers,
                state,
                path,
                handled,
                focus_target,
                node_id,
                taffy,
            );
            if let Some(idx) = self.bin_menu.take_selection() {
                self.bin_menu_choose(state, idx);
            }
            if *handled {
                return;
            }
        }
        // Same for the project menu.
        if self.project_menu.is_open() {
            self.project_menu.handle_event(
                event,
                scale_factor,
                modifiers,
                state,
                path,
                handled,
                focus_target,
                node_id,
                taffy,
            );
            if let Some(idx) = self.project_menu.take_selection() {
                if let Some(item) = self.project_menu_intent(idx) {
                    self.run_project_menu_item(state, item);
                }
                self.project_menu.close();
            }
            if *handled {
                return;
            }
        }
        // The bin flap's full-window scrim is modal for secondary input too.
        // Do this before context-menu hit testing: a hidden track must not
        // receive a right click merely because its rect is under the scrim.
        if bin_overlay_open
            && matches!(
                event,
                WindowEvent::MouseInput {
                    state: ElementState::Pressed,
                    button: MouseButton::Right,
                    ..
                }
            )
        {
            *handled = true;
            return;
        }
        // Right-click on a bin item or a track header opens its context menu.
        if let WindowEvent::MouseInput {
            state: ElementState::Pressed,
            button: MouseButton::Right,
            ..
        } = event
            && (self.open_bin_menu_at_cursor(&r)
                || self.open_track_menu_at_cursor(&r)
                || self.open_clip_menu_at_cursor(&r))
        {
            *handled = true;
            return;
        }
        match event {
            WindowEvent::KeyboardInput {
                event: key_event, ..
            } if key_event.state == ElementState::Pressed => {
                if self.handle_key(&key_event.logical_key, modifiers, state, &r) {
                    *handled = true;
                }
            }
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            } => {
                // Before the press, while the layout still describes what the
                // user was actually looking at when they clicked.
                // A click on the bin's scrim is only a dismissal; it must not
                // first move the hidden timeline or inspector focus cursor.
                if !bin_overlay_open
                    || r.bin.contains(self.last_mouse)
                    || r.menu_btn.contains(self.last_mouse)
                {
                    self.place_cursor_at(&r, self.last_mouse);
                }
                self.handle_press(state, &r, modifiers.state().shift_key());
                // Take the keyboard, the way every shared widget in `ui` does
                // on a press. Without it the editor only ever held focus after
                // a Tab, so a user who clicked into it and then used the arrow
                // keys moved a pane cursor that nothing was allowed to paint.
                *focus_target = Some(node_id);
                *handled = true;
            }
            WindowEvent::MouseInput {
                state: ElementState::Released,
                button: MouseButton::Left,
                ..
            } => {
                if bin_overlay_open {
                    // Opening the flap cancels active drags. Consume the
                    // matching release so it cannot complete an edit below.
                    *handled = true;
                    return;
                }
                if self.bin_drag.is_some() {
                    // A bin→timeline drag drops a clip at the release position;
                    // a bin press that never moved may instead begin a rename.
                    self.release_bin_drag(state, &r);
                    *handled = true;
                } else if self.drag.is_some() {
                    // A rubber band resolves to a selection; a press that never
                    // travelled falls back to scrubbing where it landed.
                    if let Some(Drag::Marquee { origin, additive }) = self.drag {
                        self.release_marquee(origin, additive, &r);
                    }
                    for pane in Self::SCROLL_PANES {
                        self.scroll_bar_mut(pane).release();
                    }
                    // Commit the drag's final position / trim / grade as one
                    // action (one undo boundary), then end the drag.
                    self.commit_drag(state);
                    self.drag = None;
                    self.marquee = None;
                    *handled = true;
                }
            }
            WindowEvent::CursorMoved { position, .. } => {
                let p = Point::new(position.x / scale_factor, position.y / scale_factor);
                self.last_mouse = p;
                self.update_hover(&r);
                if !bin_overlay_open {
                    self.apply_drag(&r);
                    self.update_bin_drag();
                }
                *handled = true;
            }
            // External file drop (an OS drag-and-drop onto the window) imports
            // media, exactly like the portal pick — the whole editor accepts it.
            WindowEvent::DroppedFile(path) => {
                self.presentation.file_hovering = false;
                if bin_overlay_open {
                    // Importing through a covered canvas would contradict the
                    // scrim's input boundary. Dismiss first, then drop again.
                    *handled = true;
                    return;
                }
                // A drop is a batch of one, and reports like one: a refusal
                // still names the file and carries the probe's own reason.
                let report = self.import_files(state, std::slice::from_ref(path));
                self.import_notice = Some(import_report_notice(&report));
                *handled = true;
            }
            WindowEvent::HoveredFile(_) => {
                self.presentation.file_hovering = !bin_overlay_open;
                *handled = true;
            }
            WindowEvent::HoveredFileCancelled => {
                self.presentation.file_hovering = false;
                *handled = true;
            }
            WindowEvent::MouseWheel { delta, .. } => {
                if bin_overlay_open && !r.bin.contains(self.last_mouse) {
                    // Do not pan/zoom a timeline which is visibly unavailable
                    // behind the narrow bin overlay.
                    *handled = true;
                    return;
                }
                let (dx, dy) = match delta {
                    MouseScrollDelta::LineDelta(x, y) => (*x as f64, *y as f64),
                    MouseScrollDelta::PixelDelta(p) => (p.x / 40.0, p.y / 40.0),
                };
                let shift = modifiers.state().shift_key();
                let over_bin = self.bin_is_visible(&r) && r.bin.contains(self.last_mouse);
                let over_timeline = r.timeline.contains(self.last_mouse);
                if self.inspector_is_visible(&r)
                    && r.inspector.contains(self.last_mouse)
                    && !over_bin
                {
                    // Scroll the inspector body (its content can exceed the pane).
                    if dy.abs() > 0.0 {
                        let max = self.inspector_max_scroll(&r);
                        self.inspector_scroll = (self.inspector_scroll - dy * 28.0).clamp(0.0, max);
                        *handled = true;
                    }
                } else if over_bin {
                    // Scroll the media bin list.
                    if dy.abs() > 0.0 {
                        let max = self.bin_max_scroll(&r);
                        self.bin_scroll = (self.bin_scroll - dy * 40.0).clamp(0.0, max);
                        *handled = true;
                    }
                } else if !over_timeline {
                    // The preview and transport have nothing to scroll. A wheel
                    // there used to zoom the timeline, which is a control the
                    // pointer is nowhere near; leave the event unhandled so it
                    // reaches the host instead of moving something off-screen.
                } else if dx.abs() > 0.0 || (shift && dy.abs() > 0.0) {
                    // A horizontal wheel, or Shift + a vertical one, pans the
                    // timeline. One notch moves a fixed slice of the visible
                    // span, so panning feels the same at every zoom.
                    let notches = if dx.abs() > 0.0 { -dx } else { -dy };
                    let step = self.visible_seconds(&r) * 0.15;
                    self.scroll_by(notches * step, &r);
                    *handled = true;
                } else if dy.abs() > 0.0 {
                    // Plain wheel zooms, anchored on the time under the pointer
                    // so the frame you are looking at stays put.
                    self.zoom_at(
                        if dy > 0.0 { 1.12 } else { 1.0 / 1.12 },
                        self.last_mouse.x,
                        &r,
                    );
                    *handled = true;
                }
            }
            _ => {}
        }
    }

    /// What the pointer is over. The editor is a canvas, so without this every
    /// region — a draggable bin row, a trim handle, a scrubber — looked
    /// identical to dead space.
    fn cursor_shape(&self, point: Point) -> Option<winit::window::CursorIcon> {
        use winit::window::CursorIcon;
        let r = self.regions();
        if self.presentation.export_open {
            let l = self.export_layout(&r);
            let over_btn = l.cancel.contains(point)
                || l.confirm.contains(point)
                || l.resolution.iter().any(|(_, rc)| rc.contains(point))
                || l.container.iter().any(|(_, rc)| rc.contains(point))
                || l.fps.iter().any(|(_, rc)| rc.contains(point));
            return Some(if over_btn {
                CursorIcon::Pointer
            } else {
                CursorIcon::Default
            });
        }
        // A live drag owns the cursor for its whole duration.
        match self.drag {
            Some(Drag::ClipMove { .. }) => return Some(CursorIcon::Grabbing),
            Some(Drag::TrimIn { .. } | Drag::TrimOut { .. }) => {
                return Some(CursorIcon::EwResize);
            }
            Some(Drag::Playhead | Drag::Scrubber) => return Some(CursorIcon::ColResize),
            Some(Drag::Marquee { .. }) => return Some(CursorIcon::Crosshair),
            _ => {}
        }
        if self.bin_drag.map(|b| b.active).unwrap_or(false) {
            return Some(CursorIcon::Grabbing);
        }
        // Scrollbars first — they float over their panes.
        for pane in Self::SCROLL_PANES {
            if let Some((track, m)) = self.scroll_pane(pane, &r) {
                if self
                    .scroll_bar(pane)
                    .thumb_rect(track, m)
                    .is_some_and(|t| t.contains(point))
                {
                    return Some(CursorIcon::Pointer);
                }
            }
        }
        // Topbar buttons, and the export chip that hangs below them.
        if [
            r.import_btn,
            r.split_btn,
            r.export_btn,
            r.menu_btn,
            r.inspector_btn,
        ]
        .iter()
        .any(|b| b.contains(point))
            || (self.export_chip_is_shown() && r.export_chip.contains(point))
        {
            return Some(CursorIcon::Pointer);
        }
        // Bin rows can be dragged onto the timeline.
        if self.bin_is_visible(&r) && r.bin.contains(point) {
            if self
                .bin_item_rects(&r)
                .iter()
                .any(|(_, rect)| rect.contains(point))
            {
                return Some(CursorIcon::Grab);
            }
            return Some(CursorIcon::Default);
        }
        // Transport.
        if self
            .history_buttons(&r)
            .iter()
            .any(|(w, rect)| rect.contains(point) && self.undo.status().can(*w))
        {
            return Some(CursorIcon::Pointer);
        }
        if self
            .transport_buttons(&r)
            .iter()
            .any(|(_, rect)| rect.contains(point))
        {
            return Some(CursorIcon::Pointer);
        }
        if self.has_scrubber(&r) {
            let sc = self.scrubber_rect(&r);
            if point.x >= sc.x0 && point.x <= sc.x1 && (point.y - sc.center().y).abs() < 14.0 {
                return Some(CursorIcon::ColResize);
            }
        }
        if r.ruler.contains(point) {
            return Some(CursorIcon::ColResize);
        }
        // Track-header pills and the add-track buttons.
        if r.gutter.contains(point) {
            for (_, _, lane) in self.lane_rects(&r) {
                let header = Rect::new(r.gutter.x0, lane.y0, r.gutter.x1, lane.y1);
                if header.contains(point)
                    && [
                        self.mute_pill_rect(header),
                        self.lock_pill_rect(header),
                        self.solo_pill_rect(header),
                    ]
                    .iter()
                    .any(|pill| pill.contains(point))
                {
                    return Some(CursorIcon::Pointer);
                }
            }
            if self
                .add_track_buttons(&r)
                .iter()
                .any(|(_, rect)| rect.contains(point))
            {
                return Some(CursorIcon::Pointer);
            }
            return Some(CursorIcon::Default);
        }
        // Timeline clips: trim handles at the edges, grab in the body.
        if r.lanes.contains(point) {
            if let Some((cid, lane)) = self.clip_at(&r, point) {
                if !self.project.clip_editable(cid) {
                    return Some(CursorIcon::NotAllowed);
                }
                if let Some(clip) = self.project.clip(cid) {
                    let cr = self.clip_rect(clip, &lane, &r);
                    if (point.x - cr.x0).abs() <= TRIM_HIT || (point.x - cr.x1).abs() <= TRIM_HIT {
                        return Some(CursorIcon::EwResize);
                    }
                }
                return Some(CursorIcon::Grab);
            }
            return Some(CursorIcon::Crosshair);
        }
        // Inspector controls.
        if self.inspector_is_visible(&r) && self.inspector_viewport(&r).contains(point) {
            let l = self.inspector_layout(&r);
            let sliders = [
                l.opacity,
                l.volume,
                l.brightness,
                l.contrast,
                l.saturation,
                l.fade_in,
                l.fade_out,
                l.transition_len,
            ];
            if sliders.iter().flatten().any(|t| hit_slider(*t, point))
                || l.nums.iter().any(|(_, t)| hit_slider(*t, point))
            {
                return Some(CursorIcon::Pointer);
            }
            // Everything else that answers to a click here says so, including
            // the sections, the key controls and the marker rows — a control
            // the pointer does not acknowledge reads as decoration.
            let clickable = l.sections.iter().any(|(_, rc)| rc.contains(point))
                || l.keys.iter().any(|(_, rc)| rc.contains(point))
                || l.marker_rows.iter().any(|(_, rc)| rc.contains(point))
                || l.add_marker.map(|rc| rc.contains(point)).unwrap_or(false)
                || l.project_frames.iter().any(|(_, rc)| rc.contains(point))
                || l.project_rates.iter().any(|(_, rc)| rc.contains(point))
                || l.project_match
                    .map(|rc| rc.contains(point))
                    .unwrap_or(false);
            if clickable {
                return Some(CursorIcon::Pointer);
            }
            if l.valid
                && (l.split.contains(point)
                    || l.delete.contains(point)
                    || l.add_effect.contains(point)
                    || l.transitions.iter().any(|(_, rc)| rc.contains(point))
                    || l.colors.iter().any(|(_, rc)| rc.contains(point))
                    || l.pip.iter().any(|(_, _, rc)| rc.contains(point))
                    || l.speed_presets.iter().any(|(_, rc)| rc.contains(point))
                    || l.title_align.iter().any(|(_, rc)| rc.contains(point))
                    || l.title_colors.iter().any(|(_, rc)| rc.contains(point))
                    || l.key_colors.iter().any(|(_, rc)| rc.contains(point))
                    || l.mask_shapes.iter().any(|(_, rc)| rc.contains(point))
                    || [
                        l.link,
                        l.reset_transform,
                        l.reverse,
                        l.mix_reset,
                        l.title_text,
                        l.title_bold,
                        l.title_italic,
                        l.key_enable,
                        l.key_pick,
                        l.mask_fit,
                        l.mask_invert,
                    ]
                    .iter()
                    .flatten()
                    .any(|rc| rc.contains(point)))
            {
                return Some(CursorIcon::Pointer);
            }
        }
        Some(CursorIcon::Default)
    }

    /// This editor publishes about the timeline it holds.
    fn presence_key(&self) -> Option<PresenceKey> {
        Some(crate::presence::timeline_key())
    }

    /// Where this participant is, for the room to draw.
    ///
    /// Two facts -- the playhead and the selected clip -- through a hook that
    /// carries one at a time, so [`crate::presence::LocalPresence`] hands over
    /// whichever has moved since it was last sent. Both survive on the
    /// receiving side because the caret store and the selection store are
    /// separate; see [`crate::presence`].
    ///
    /// None of this is an action and none of it reaches the document. A
    /// playhead is view state: replaying someone else's is meaningless, and
    /// putting it on an undo stack would be worse than meaningless.
    fn presence_payload(&self) -> Option<(PresenceKey, ui::PresencePayload)> {
        let editing = self
            .title_edit
            .as_ref()
            .map(|(mid, field)| (crate::presence::FIELD_TITLE_WORDS, *mid, field.cursor_pos()));
        let selected = self
            .selected()
            .and_then(|id| self.project.clip(id))
            .map(|c| (c.id, c.start, c.end()));
        self.local_presence.next([
            crate::presence::point_frame(editing, self.playhead),
            crate::presence::range_frame(selected),
        ])
    }

    fn name(&self) -> &str {
        "VideoEditorApp"
    }
    fn test_id(&self) -> Option<&str> {
        Some("video_editor_root")
    }

    /// Publish the open project + its tracks and clips so the host's Ctrl+K
    /// action palette scopes to them. Delegates to the
    /// [`ResourceProvider`](tilo_actions::ResourceProvider) impl in
    /// [`crate::actions`].
    fn resources(&self, doc: &AutoCommit) -> Vec<tilo_actions::Resource> {
        <Self as tilo_actions::ResourceProvider>::resources(self, doc)
    }

    fn invoke_resource_operation(
        &mut self,
        kind: tilo_actions::OperationKind,
        type_id: &str,
        payload: &[u8],
        _doc: &AutoCommit,
        _path: &ObjId,
    ) -> Result<bool, String> {
        if kind == tilo_actions::OperationKind::Document {
            return Ok(false);
        }
        let Some(operation) = crate::actions::decode_operation(type_id, payload)? else {
            return Ok(false);
        };
        let expected = crate::actions::operation_kind(&operation);
        if kind != expected {
            return Err(format!(
                "operation kind {kind:?} does not match video editor operation kind {expected:?}"
            ));
        }
        self.perform_resource_operation(operation)?;
        Ok(true)
    }

    fn start_resource_operation(
        &mut self,
        invocation: tilo_actions::InvocationId,
        kind: tilo_actions::OperationKind,
        type_id: &str,
        payload: &[u8],
        doc: &AutoCommit,
        path: &ObjId,
    ) -> Result<Option<ui::widget::ResourceOperationStart>, String> {
        let Some(operation) = crate::actions::decode_operation(type_id, payload)? else {
            return Ok(None);
        };
        let expected = crate::actions::operation_kind(&operation);
        if kind != expected {
            return Err(format!(
                "operation kind {kind:?} does not match video editor operation kind {expected:?}"
            ));
        }
        if matches!(
            operation,
            crate::actions::VideoEditorOperation::CancelExport
        ) {
            self.invoke_resource_operation(kind, type_id, payload, doc, path)?;
            return Ok(Some(ui::widget::ResourceOperationStart::Completed(
                tilo_actions::ActionOutput::default(),
            )));
        }
        if matches!(
            operation,
            crate::actions::VideoEditorOperation::OpenProject { path: Some(_) }
                | crate::actions::VideoEditorOperation::SaveProject { path: Some(_) }
                | crate::actions::VideoEditorOperation::SaveProjectAs { path: Some(_) }
        ) {
            self.resource_operation = Some(operation.clone());
            self.resource_pending_operation = Some((invocation, operation));
            return Ok(Some(ui::widget::ResourceOperationStart::Accepted));
        }
        self.perform_resource_operation(operation)?;
        self.resource_operation = crate::actions::decode_operation(type_id, payload)?;
        self.resource_invocation = Some(invocation);
        Ok(Some(ui::widget::ResourceOperationStart::Accepted))
    }

    fn poll_resource_operation(
        &mut self,
        invocation: tilo_actions::InvocationId,
        _cancellation_requested: bool,
        _doc: &AutoCommit,
        _path: &ObjId,
    ) -> Option<tilo_actions::InvocationUpdate> {
        self.poll_export();
        self.finish_resource_file_operation_if_ready();
        if self
            .resource_result
            .as_ref()
            .is_some_and(|u| u.invocation() == invocation)
        {
            self.resource_result.take()
        } else {
            None
        }
    }

    /// Fold the open project into Tilo global search: every marker, title and
    /// bin source becomes a findable *place*, and choosing one seeks there.
    ///
    /// A snapshot rather than a borrow, because the host queries the provider
    /// off the paint path — see
    /// [`ProjectSearchProvider`](crate::search_provider::ProjectSearchProvider).
    fn search_providers(&self) -> Vec<Box<dyn ui::search::SearchProvider>> {
        vec![Box::new(
            crate::search_provider::ProjectSearchProvider::new(&self.project),
        )]
    }

    /// Hand the host the document heads captured before an on-screen affordance
    /// dispatched its timeline action, so it records exactly one undo boundary
    /// bracketing it (Ctrl+Z reverts it OS-wide). Rising-edge: cleared on read.
    fn take_committed_action(&mut self) -> Option<Vec<automerge::ChangeHash>> {
        self.actions.take_committed()
    }

    /// The host's reading of the history, which is what the strip's undo and
    /// redo buttons are drawn from. Scoped to this author exactly as Ctrl+Z
    /// is, so the buttons and the shortcut can never disagree about whether
    /// there is anything to go back to.
    fn set_undo_status(&mut self, status: &ui::UndoStatus) {
        self.undo.publish(status);
    }

    fn take_undo_request(&mut self) -> Option<ui::UndoRequest> {
        self.undo.take_request()
    }
    fn is_focusable(&self) -> bool {
        true
    }

    fn set_focus(&mut self, focused: bool) {
        self.presentation.keyboard_focus = focused;
    }

    fn is_focused(&self) -> bool {
        self.presentation.keyboard_focus
    }

    /// The export dialog traps the keyboard while it is open.
    ///
    /// Without this, Tab out of a modal walks the content behind it — the user
    /// is editing a dialog they can no longer see, and a screen reader reads a
    /// pane the dimming scrim says is unavailable.
    fn traps_focus(&self) -> bool {
        // Every surface that paints the full-window scrim, not just the
        // export dialog. A scrim is a promise that what is behind it is
        // unavailable; if the click still lands there, the promise is a lie
        // and the edit it makes is one the user cannot see coming.
        self.is_modal() || self.bin_overlay_is_open()
    }

    /// The widgets this editor mounts inside itself. They paint and take
    /// events already; exposing them here is what puts them in the
    /// accessibility tree and the host's hit-testing, so an open context menu
    /// or an in-flight rename is something a reader can find rather than an
    /// invisible mode.
    fn children(&self) -> Vec<&dyn Widget> {
        let mut v: Vec<&dyn Widget> = Vec::new();
        if self.bin_menu.is_open() {
            v.push(&self.bin_menu);
        }
        if self.project_menu.is_open() {
            v.push(&self.project_menu);
        }
        if let Some((_, label)) = &self.bin_rename {
            v.push(label);
        }
        if let Some((_, label)) = &self.title_edit {
            v.push(label);
        }
        if let Some((_, label)) = &self.marker_rename {
            v.push(label);
        }
        v
    }

    fn children_mut(&mut self) -> Vec<&mut (dyn Widget + '_)> {
        let mut v: Vec<&mut (dyn Widget + '_)> = Vec::new();
        if self.bin_menu.is_open() {
            v.push(&mut self.bin_menu);
        }
        if self.project_menu.is_open() {
            v.push(&mut self.project_menu);
        }
        if let Some((_, label)) = &mut self.bin_rename {
            v.push(label);
        }
        if let Some((_, label)) = &mut self.title_edit {
            v.push(label);
        }
        if let Some((_, label)) = &mut self.marker_rename {
            v.push(label);
        }
        v
    }

    /// The whole editor's semantics: an application region whose contents are
    /// entirely synthetic, because the editor paints its own panes rather than
    /// composing child widgets.
    ///
    /// Every pane, track, clip, control and toggle is described here. The
    /// element the pane cursor is on carries `focused`, which the export layer
    /// turns into the tree's active descendant — so a reader lands on the clip
    /// the user is on rather than on "video editor, canvas".
    fn accessibility(&self) -> Option<AccessNode> {
        let mut kids = if self.bin_overlay_is_open() {
            // The scrim says the bin owns input, so the semantic tree must
            // not leave a reader walking controls it cannot use. The topbar
            // contributes only the pressed bin toggle in this state.
            vec![self.a11y_topbar(), self.a11y_bin()]
        } else {
            vec![
                self.a11y_topbar(),
                self.a11y_project(),
                self.a11y_bin(),
                self.a11y_transport(),
                self.a11y_timeline(),
                self.a11y_inspector(),
            ]
        };
        if self.presentation.export_open {
            kids.push(self.a11y_export_dialog());
        }
        // A render that outlived its dialog. Gated on the *render*, never on
        // the dialog — a dismissed export used to vanish from the tree
        // entirely while it was still encoding.
        if let Some(status) = self.a11y_export_status() {
            kids.push(status);
        }
        if self.pending_open.is_some() {
            kids.push(self.a11y_discard_dialog());
        }
        if self.navigation.help_open {
            kids.push(self.a11y_help_sheet());
        }
        Some(
            AccessNode::new(Role::Canvas)
                .with_name("Video editor")
                .with_value(format!(
                    "{}, {} in {} pane",
                    // The same line the project button's dot and tooltip
                    // carry, so a reader is told which project is open AND
                    // whether it is written down. A bare name would say only
                    // the first, which is the tidier and less true story.
                    self.project_status_line(),
                    crate::timecode::timecode(self.playhead, self.project.fps),
                    self.focus.pane.label()
                ))
                .with_state(AccessState {
                    focusable: true,
                    focused: self.presentation.keyboard_focus,
                    ..Default::default()
                })
                .with_children(kids)
                .with_keyed_action_routes("video-editor"),
        )
    }

    /// Route an assistive technology's action to the element it named.
    ///
    /// `key_path` is the chain of keys from this widget down: `[pane]` for a
    /// pane, `[pane, item]` for a bin row / transport button / track, and
    /// `[timeline, track, clip]` for a clip. Keys are identity hashes, so a
    /// path resolves to the same clip after it has been dragged somewhere else
    /// — and a path naming something that no longer exists matches nothing and
    /// is dropped rather than applied to whatever took its place.
    #[allow(
        clippy::indexing_slicing,
        reason = "a decoded accessibility action has its fixed argument shape validated before its command-specific fields are read"
    )]
    fn perform_action_at(
        &mut self,
        action: &ui::a11y::Action,
        key_path: &[u64],
        state: &mut AutoCommit,
        _path: &ObjId,
    ) -> bool {
        use ui::a11y::{Action, hash_key};
        let Some(&pane_key) = key_path.first() else {
            return false;
        };
        let Some(pane) = Pane::ORDER.iter().copied().find(|p| p.key() == pane_key) else {
            // Not a pane: the export dialog or the help sheet.
            if pane_key == hash_key(&"export") {
                return self.a11y_export_action(action, &key_path[1..]);
            }
            if pane_key == hash_key(&"export-status") {
                // Activating the chip does what clicking it does: opens the
                // dialog it is reporting for. With nothing to report there is
                // nothing to open, and it says so rather than claiming a
                // success.
                if matches!(action, Action::Activate) && self.export_chip_is_shown() {
                    self.presentation.export_open = true;
                    return true;
                }
                return false;
            }
            if pane_key == hash_key(&"project") {
                // Activating it does what clicking it does: opens the menu.
                // Whether the menu is reachable this way is exactly whether
                // the button is painted, so the node's focusable flag and this
                // cannot disagree.
                if matches!(action, Action::Activate) {
                    let btn = self.regions().project_btn;
                    if btn.width() > 0.0 {
                        self.open_project_menu(Point::new(btn.x0, btn.y1 + 4.0));
                        return true;
                    }
                }
                return false;
            }
            if pane_key == hash_key(&TOPBAR_A11Y_KEY) {
                return self.a11y_topbar_action(action, &key_path[1..], state);
            }
            if pane_key == hash_key(&"discard") {
                let Some(&which) = key_path.get(1) else {
                    return false;
                };
                if !matches!(action, Action::Activate) {
                    return false;
                }
                let choice = match which {
                    1 => DiscardChoice::Cancel,
                    2 => DiscardChoice::Discard,
                    3 => DiscardChoice::Save,
                    _ => return false,
                };
                self.resolve_pending_open(state, choice);
                return true;
            }
            if pane_key == hash_key(&"help") && matches!(action, Action::Activate) {
                self.navigation.help_open = false;
                return true;
            }
            return false;
        };
        if self.bin_overlay_is_open() && pane != Pane::Bin {
            return false;
        }
        let rest = &key_path[1..];
        // Focusing anything inside a pane also moves the pane cursor there, so
        // the keyboard and the reader stay on the same element.
        match pane {
            Pane::Bin => self.a11y_bin_action(action, rest, state),
            Pane::Transport => self.a11y_transport_action(action, rest),
            Pane::Timeline => self.a11y_timeline_action(action, rest, state),
            Pane::Inspector => self.a11y_inspector_action(action, rest, state),
        }
    }

    /// The label an icon-only control shows on hover — the same words its
    /// accessible name uses, so the tooltip and the reader never disagree.
    ///
    /// The editor is one widget, so the host asks it once and it answers for
    /// whatever the pointer is actually over.
    fn tooltip(&self) -> Option<&str> {
        self.tooltip_at(self.last_mouse)
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }

    fn draw_with_presence(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
        _permissions: &PermissionContext,
    ) {
        self.draw(scene, transform, tr, theme, presence);
    }
}

// ==========================================================================
// Accessibility
// ==========================================================================

/// Which transport button each index addresses, with the name a reader speaks.
///
/// The glyphs alone do not carry the meaning — two of the five pairs were once
/// drawn near-identically — so the name is the control's real label, not a
/// decoration on top of a picture that already says it.
const TRANSPORT_NAMES: [(TransportAction, &str); 5] = [
    (TransportAction::ToStart, "Jump to start"),
    (TransportAction::StepBack, "Step back one frame"),
    (TransportAction::PlayPause, "Play"),
    (TransportAction::StepFwd, "Step forward one frame"),
    (TransportAction::ToEnd, "Jump to end"),
];

impl VideoEditorApp {
    /// Execute a top-bar command through the same helpers the pointer uses.
    /// Visibility is checked again at activation time so a delayed assistive
    /// technology event cannot fire Split after a resize hid that button.
    fn a11y_topbar_action(
        &mut self,
        action: &ui::a11y::Action,
        rest: &[u64],
        state: &mut AutoCommit,
    ) -> bool {
        use ui::a11y::{Action, hash_key};
        let Some(&key) = rest.first() else {
            return false;
        };
        let r = self.regions();
        if self.bin_overlay_is_open() && key != hash_key(&TOPBAR_BIN_KEY) {
            return false;
        }
        match action {
            Action::Focus => [
                (TOPBAR_BIN_KEY, r.menu_btn.width() > 0.0),
                (TOPBAR_INSPECTOR_KEY, r.inspector_btn.width() > 0.0),
                (TOPBAR_IMPORT_KEY, r.import_btn.width() > 0.0),
                (TOPBAR_SPLIT_KEY, r.split_btn.width() > 0.0),
                (TOPBAR_EXPORT_KEY, r.export_btn.width() > 0.0),
            ]
            .iter()
            .any(|(name, visible)| *visible && hash_key(name) == key),
            Action::Activate => {
                if key == hash_key(&TOPBAR_BIN_KEY) && r.menu_btn.width() > 0.0 {
                    self.toggle_bin();
                    return true;
                }
                if key == hash_key(&TOPBAR_INSPECTOR_KEY) && r.inspector_btn.width() > 0.0 {
                    self.toggle_inspector();
                    return true;
                }
                if key == hash_key(&TOPBAR_IMPORT_KEY) && r.import_btn.width() > 0.0 {
                    self.trigger_import();
                    return true;
                }
                if key == hash_key(&TOPBAR_SPLIT_KEY) && r.split_btn.width() > 0.0 {
                    self.split_selected(state);
                    return true;
                }
                if key == hash_key(&TOPBAR_EXPORT_KEY) && r.export_btn.width() > 0.0 {
                    self.presentation.export_open = true;
                    return true;
                }
                false
            }
            _ => false,
        }
    }

    /// The media rows the bin currently lists, in painted order.
    fn a11y_bin_items(&self) -> Vec<MediaId> {
        // `bin_order`, not the project's own order: that is the order the rows
        // are painted in, and a reader walking a differently-ordered list
        // would be told the second row is something the second row is not.
        // It also indexes `focus.bin`, so the keyboard cursor and the
        // highlighted row have to be counting the same list.
        self.bin_order()
    }

    /// The marker rows the inspector lists, in painted order: the reference a
    /// row addresses, the name it is drawn with, and what a reader is told
    /// about it beyond that name.
    ///
    /// Built from [`Project::all_markers`] -- the very call `layout_markers`
    /// makes -- so the spoken list and the drawn list cannot come apart.
    fn a11y_marker_rows(&self) -> Vec<(MarkerRef, String, String)> {
        self.project
            .all_markers()
            .into_iter()
            .map(|(at, m, clip)| {
                let r = MarkerRef { id: m.id, clip, at };
                // The stand-in an unnamed marker is painted with. A reader
                // hearing nothing where the screen shows a word is being
                // told a different story about the same row.
                let name = if m.name.trim().is_empty() {
                    "Marker".to_string()
                } else {
                    m.name.clone()
                };
                let mut value = crate::timecode::timecode(at, self.project.fps);
                // A clip marker says which clip it travels with, because
                // moving that clip moves the marker -- the same thing the
                // row's tinted timecode says to the eye.
                if let Some(c) = clip.and_then(|c| self.project.clip(c)) {
                    value.push_str(", on ");
                    value.push_str(&self.a11y_clip_name(c));
                }
                // The note is a second painted line under the name. Leaving
                // it out here would be the tidier story, not the true one.
                if !m.note.trim().is_empty() {
                    value.push_str(", note: ");
                    value.push_str(m.note.trim());
                }
                (r, name, value)
            })
            .collect()
    }

    /// The inspector rows the keyboard can land on, in painted order: every
    /// section header (so a folded one can be opened without a mouse) followed
    /// by that section's controls when it is open.
    fn a11y_inspector_rows(&self) -> Vec<InspectorRow> {
        let mut out = Vec::new();
        let clip = self
            .selected()
            .and_then(|id| self.project.clip(id))
            .cloned();
        // The controls the panel draws **above** its first section header, in
        // painted order. This list is both the accessibility tree and the
        // keyboard walk, and it used to begin at the first header — so Opacity
        // was on screen, above the cursor's first stop, and unreachable.
        out.extend(self.a11y_top_rows(clip.as_ref()));
        for section in self.a11y_sections(clip.as_ref()) {
            out.push(InspectorRow::Header(section));
            if self.collapsed.contains(&section) {
                continue;
            }
            // The control that opens a section whose contents are conditional
            // on it. Emitted before the section's knobs, which is where it is
            // drawn, and without it the knobs are unreachable rather than
            // merely awkward.
            match section {
                Section::Key => out.push(InspectorRow::Pick(PickField::KeyEnable)),
                Section::Mask => out.extend(
                    crate::model::MaskShape::ALL
                        .iter()
                        .map(|s| InspectorRow::Pick(PickField::MaskShape(*s))),
                ),
                _ => {}
            }
            // The marker list is not a run of numeric knobs, so it is walked
            // on its own terms: one stop per marked instant, then the button
            // that adds another -- exactly the rows `layout_markers` draws.
            if section == Section::Markers {
                out.extend(
                    self.a11y_marker_rows()
                        .into_iter()
                        .map(|(m, _, _)| InspectorRow::Marker(m)),
                );
                out.push(InspectorRow::AddMarker);
                continue;
            }
            // The project's format is two preset rows and a button, not a run
            // of knobs — exactly the stops `layout_project` draws.
            if section == Section::Project {
                out.extend(
                    (0..FRAME_PRESETS.len()).map(|i| InspectorRow::Pick(PickField::Frame(i))),
                );
                out.extend((0..RATE_PRESETS.len()).map(|i| InspectorRow::Pick(PickField::Rate(i))));
                if self.format_source().is_some() {
                    out.push(InspectorRow::Pick(PickField::MatchFormat));
                }
                continue;
            }
            for f in Self::a11y_section_fields(section, clip.as_ref()) {
                out.push(InspectorRow::Field(f));
            }
            // The buttons a section draws *below* its knobs. Emitted after
            // them, which is where they are painted, and on the same condition
            // the layout uses — a stop on a control the panel is not drawing
            // would put a reader on a different panel from the one on screen.
            if section == Section::Mix
                && self
                    .selected_track()
                    .is_some_and(crate::model::Track::mix_is_set)
            {
                out.push(InspectorRow::Pick(PickField::MixReset));
            }
        }
        out
    }

    /// The stops the panel draws above its first section header: the clip's
    /// level or its grade, its two fades, the transition at its head cut, the
    /// transition's length when there is one, and the seven label colours.
    ///
    /// Built from the same `is_video` test the layout makes, so the walk is
    /// offered exactly the controls that are drawn — an audio clip has a
    /// volume slider where a video clip has four grade sliders, and neither
    /// is offered the other's.
    fn a11y_top_rows(&self, clip: Option<&Clip>) -> Vec<InspectorRow> {
        let Some(clip) = clip else {
            return Vec::new();
        };
        let is_video = self
            .project
            .media(clip.media)
            .is_none_or(|m| m.kind.is_visual());
        let mut out: Vec<InspectorRow> = Vec::new();
        if is_video {
            for f in [
                NumField::Opacity,
                NumField::Brightness,
                NumField::Contrast,
                NumField::Saturation,
            ] {
                out.push(InspectorRow::Field(f));
            }
        } else {
            out.push(InspectorRow::Field(NumField::Volume));
        }
        out.push(InspectorRow::Field(NumField::FadeIn));
        out.push(InspectorRow::Field(NumField::FadeOut));
        out.extend(
            Transition::ALL
                .iter()
                .map(|t| InspectorRow::Pick(PickField::Transition(*t))),
        );
        // The length slider is laid out only when there is a ramp to size.
        if clip.transition != Transition::None {
            out.push(InspectorRow::Field(NumField::TransitionLen));
        }
        out.extend((0..7u8).map(|slot| InspectorRow::Pick(PickField::Label(slot))));
        // The link control, in the place it is drawn. Offered only when the
        // selection has something to link or unlink, exactly as the layout
        // decides — a walk that stopped on a control nobody can see would be
        // describing a different panel from the one on screen.
        if self.link_affordance().is_some() {
            out.push(InspectorRow::Pick(PickField::LinkToggle));
        }
        out
    }

    /// Which sections the inspector shows for the current selection. Title is
    /// only meaningful for a title clip; Markers belong to the project and are
    /// listed whether or not a clip is selected.
    fn a11y_sections(&self, clip: Option<&Clip>) -> Vec<Section> {
        let Some(clip) = clip else {
            // Both project-level sections, in the order they are laid out.
            return vec![Section::Markers, Section::Project];
        };
        let media = self.project.media(clip.media);
        let is_title = media.map(|m| m.text.is_some()).unwrap_or(false);
        let is_video = media.map(|m| m.kind.is_visual()).unwrap_or(true);
        // The same `is_video` test the layout makes, so the reader is
        // offered exactly the sections that are drawn — no more and no
        // fewer. An audio clip has no picture to place, key, mask or
        // effect, and the panel draws none of those headers for one.
        let mut v = Vec::new();
        if is_video {
            v.push(Section::Curves);
            v.push(Section::Transform);
        }
        v.push(Section::Speed);
        v.push(Section::Mix);
        if is_video {
            v.push(Section::Key);
            v.push(Section::Mask);
        }
        if is_title {
            v.push(Section::Title);
        }
        if is_video {
            v.push(Section::Effects);
        }
        v.push(Section::Markers);
        v.push(Section::Project);
        v
    }

    /// The numeric knobs inside a section, in painted order.
    fn a11y_section_fields(section: Section, clip: Option<&Clip>) -> Vec<NumField> {
        let Some(c) = clip else {
            return Vec::new();
        };
        match section {
            // The amount slider is the section's only ranged control; the
            // curve itself is a 2D editor, which is reported as its own node
            // rather than as a knob (see `a11y_section_summary`).
            Section::Curves => vec![NumField::CurveMix],
            Section::Transform => vec![
                NumField::ScaleX,
                NumField::ScaleY,
                NumField::PosX,
                NumField::PosY,
                NumField::Rotation,
                NumField::CropLeft,
                NumField::CropRight,
                NumField::CropTop,
                NumField::CropBottom,
            ],
            Section::Speed => vec![NumField::Speed],
            Section::Mix => vec![NumField::TrackGain, NumField::TrackPan],
            // The key's sliders are only laid out once the key is on, and
            // the mask's only once a shape is chosen. A reader offered
            // controls a sighted user cannot see would be navigating a
            // different panel from the one on screen.
            Section::Key if c.key.enabled => vec![
                NumField::KeyTolerance,
                NumField::KeySoftness,
                NumField::KeySpill,
            ],
            Section::Mask if c.mask.is_active() => vec![
                NumField::MaskX,
                NumField::MaskY,
                NumField::MaskW,
                NumField::MaskH,
                NumField::MaskRotation,
                NumField::MaskFeather,
            ],
            Section::Title => vec![NumField::TitleSize, NumField::TitleBackdrop],
            // `Project` carries no ranged knob: its two controls are preset
            // rows, which reach the walk as `InspectorRow::Pick`.
            Section::Key
            | Section::Mask
            | Section::Effects
            | Section::Markers
            | Section::Project => Vec::new(),
        }
    }

    /// A clip's accessible name: the media it places, so the reader says
    /// "Interview" rather than a UUID the user has never seen.
    fn a11y_clip_name(&self, clip: &Clip) -> String {
        self.project
            .media(clip.media)
            .map(|m| m.name.clone())
            .unwrap_or_else(|| "Missing media".into())
    }

    /// What a reader says about a clip beyond its name: where it sits, how long
    /// it runs, and which track it is on. Everything the eye reads off the bar's
    /// position and width, in words.
    fn a11y_clip_value(&self, clip: &Clip) -> String {
        let fps = self.project.fps;
        let len = (clip.out_point - clip.in_point).max(0.0);
        let track = self
            .project
            .track(clip.track)
            .map(|t| t.name.clone())
            .unwrap_or_else(|| "track".into());
        format!(
            "{} to {}, {} long, on {track}",
            crate::timecode::timecode(clip.start, fps),
            crate::timecode::timecode(clip.start + len, fps),
            crate::timecode::clock(len),
        )
    }

    /// Build one pane's group node, marking it focused when the keyboard is in
    /// it so the export layer can nominate an active descendant inside.
    fn a11y_pane(&self, pane: Pane, children: Vec<AccessNode>) -> AccessNode {
        AccessNode::new(Role::Group)
            .with_name(pane.label())
            .with_key(pane.key())
            .with_children(children)
    }

    /// The media bin's rows.
    fn a11y_bin(&self) -> AccessNode {
        let items = self.a11y_bin_items();
        let n = items.len();
        let cursor = self.focus.pane == Pane::Bin;
        let rows: Vec<AccessNode> = items
            .iter()
            .enumerate()
            .filter_map(|(i, mid)| {
                let m = self.project.media(*mid)?;
                let projected = self.projected_media.contains(mid);
                let mut value =
                    format!("{}, {}", m.kind.label(), crate::timecode::clock(m.duration));
                if projected {
                    // A source whose bytes are not here yet is a real, visible
                    // state; saying so is the difference between "empty" and
                    // "not fetched".
                    value.push_str(", not downloaded yet");
                }
                Some(
                    AccessNode::new(Role::ListItem)
                        .with_name(m.name.clone())
                        .with_value(value)
                        // Identity-keyed: a bin row keeps its node across a
                        // rename, a re-sort, or another import landing above it.
                        .with_hashed_key(m.id)
                        .with_position(i + 1, n)
                        .with_state(AccessState {
                            focusable: true,
                            focused: cursor && self.focus.bin == i,
                            selected: self.selected_media == Some(*mid),
                            ..Default::default()
                        }),
                )
            })
            .collect();
        if rows.is_empty() {
            return self.a11y_pane(
                Pane::Bin,
                vec![AccessNode::new(Role::StaticText)
                    .with_name("No media yet. Press Ctrl+Shift+O to import, or drop files anywhere in the window.")
                    .with_key(0)],
            );
        }
        self.a11y_pane(Pane::Bin, rows)
    }

    /// The five transport buttons, named.
    fn a11y_transport(&self) -> AccessNode {
        let cursor = self.focus.pane == Pane::Transport;
        let mut buttons: Vec<AccessNode> = TRANSPORT_NAMES
            .iter()
            .enumerate()
            .map(|(i, (action, name))| {
                // The play button is the one whose name changes with state; a
                // reader must hear what pressing it will do now. A running
                // shuttle counts: pressing it there stops the shuttle, so
                // "Play" would name something the press does not do.
                let name = if *action == TransportAction::PlayPause && self.is_moving() {
                    "Pause"
                } else {
                    name
                };
                AccessNode::new(Role::Button)
                    .with_name(name)
                    .with_key(ui::a11y::hash_key(&i))
                    .with_position(i + 1, TRANSPORT_NAMES.len())
                    .with_state(AccessState {
                        focusable: true,
                        focused: cursor && self.focus.transport == i,
                        ..Default::default()
                    })
            })
            .collect();
        // Undo / redo, exported only when the strip actually drew them, and
        // reporting `disabled` exactly when the verb is unavailable. A
        // control that cannot act must not be focusable — undoing the last
        // action empties the stack under the control you are standing on,
        // and a focus ring left on a dead button is how that reads wrong.
        for (which, _) in self.history_buttons(&self.regions()) {
            let can = self.undo.status().can(which);
            buttons.push(
                AccessNode::new(Role::Button)
                    .with_name(which.title())
                    .with_description(self.undo.status().description(which))
                    .with_key(ui::a11y::hash_key(&which.verb()))
                    .with_state(AccessState {
                        focusable: can,
                        disabled: !can,
                        ..Default::default()
                    }),
            );
        }
        let mut node = self.a11y_pane(Pane::Transport, buttons);
        // The playhead is a live readout: it changes without focus moving, so
        // it rides on the pane as a value the reader can re-read on demand.
        node = node.with_value(crate::timecode::timecode(self.playhead, self.project.fps));
        node
    }

    /// Tracks as containers, clips as the items inside them, plus each track's
    /// mute / solo / lock toggles.
    fn a11y_timeline(&self) -> AccessNode {
        let cursor = self.focus.pane == Pane::Timeline;
        let primary = self.selection.primary();
        let mut tracks: Vec<AccessNode> = Vec::new();
        let track_count = self.project.tracks.len();
        for (ti, track) in self.project.tracks.iter().enumerate() {
            let mut kids: Vec<AccessNode> = Vec::new();
            for (name, on) in [
                ("Mute", track.muted),
                ("Solo", track.solo),
                ("Lock", track.locked),
            ] {
                kids.push(
                    AccessNode::new(Role::ToggleButton)
                        .with_name(format!("{name} {}", track.name))
                        .with_hashed_key((track.id, name))
                        .with_state(AccessState {
                            focusable: true,
                            checked: Some(on),
                            ..Default::default()
                        }),
                );
            }
            let clips = self.project.clips_on(track.id);
            let n = clips.len();
            for (i, clip) in clips.iter().enumerate() {
                kids.push(
                    AccessNode::new(Role::ListItem)
                        .with_name(self.a11y_clip_name(clip))
                        .with_value(self.a11y_clip_value(clip))
                        // Identity-keyed: a clip dragged along its track, or to
                        // another track, keeps the node an AT is holding.
                        .with_hashed_key(clip.id)
                        .with_position(i + 1, n)
                        .with_state(AccessState {
                            focusable: true,
                            selected: self.selection.contains(clip.id),
                            focused: cursor && primary == Some(clip.id),
                            ..Default::default()
                        }),
                );
            }
            // The lane's fader, its pan and whether it has been caught
            // clipping ride on the group's value. A meter is a picture, and a
            // picture is the one thing a reader cannot have — so everything
            // the bar says has to be said here too, in the place a reader is
            // already standing when they ask about a track.
            let level = self.mix.track(track.id);
            let mix_words = format!(
                ", gain {}, pan {}{}",
                track.gain_words(),
                track.pan_words(),
                if level.is_some_and(|l| l.clipped) {
                    ", clipping"
                } else {
                    ""
                }
            );
            tracks.push(
                AccessNode::new(Role::Group)
                    .with_name(track.name.clone())
                    .with_value(format!(
                        "{} track, {} clip{}{mix_words}",
                        match track.kind {
                            TrackKind::Video => "video",
                            TrackKind::Audio => "audio",
                        },
                        n,
                        if n == 1 { "" } else { "s" }
                    ))
                    .with_hashed_key(track.id)
                    .with_position(ti + 1, track_count)
                    .with_children(kids),
            );
        }
        if tracks.is_empty() {
            tracks.push(
                AccessNode::new(Role::StaticText)
                    .with_name("Timeline is empty. Add media from the bin to start editing.")
                    .with_key(0),
            );
        }
        // The master strip, as the first thing in the pane. A reader meets
        // the mix's state before the lanes, which is the order the screen
        // puts it in — and when it has clipped, the node is a *button*,
        // because pressing it walks the playhead to the next place it did.
        if let Some(words) = self.mix_words() {
            let clipped = self.mix.clipped();
            tracks.insert(
                0,
                AccessNode::new(if clipped {
                    Role::Button
                } else {
                    Role::StaticText
                })
                .with_name(if clipped {
                    "Go to the next place the mix clipped"
                } else {
                    "Mix level"
                })
                .with_value(words)
                .with_key(ui::a11y::hash_key(&MIX_METER_KEY))
                .with_state(AccessState {
                    focusable: clipped,
                    ..Default::default()
                }),
            );
        }
        let mut node = self.a11y_pane(Pane::Timeline, tracks);
        node = node.with_value(format!(
            "playhead at {}, {} of {} clips selected, timeline view starts at {}; Page Up and Page Down scroll the timeline",
            crate::timecode::timecode(self.playhead, self.project.fps),
            self.selection.len(),
            self.project.clips.len(),
            crate::timecode::timecode(self.scroll, self.project.fps),
        ));
        node
    }

    /// The inspector: one group per section carrying its folded state, with the
    /// section's controls inside when it is open. A folded header still states
    /// what it contains, so folding hides controls and never hides state.
    #[allow(
        clippy::indexing_slicing,
        reason = "inspector children are emitted from the same validated section list that supplies their stable semantic keys"
    )]
    fn a11y_inspector(&self) -> AccessNode {
        let cursor = self.focus.pane == Pane::Inspector;
        let rows = self.a11y_inspector_rows();
        let clip = self
            .selected()
            .and_then(|id| self.project.clip(id))
            .cloned();
        let markers = self.a11y_marker_rows();
        // With nothing selected on a project holding no clips at all, the
        // panel paints its empty state -- and, under it, the marker list,
        // which belongs to the project rather than to any clip. Returning
        // only the empty-state line here hid every marker on the screen from
        // a reader. The line is now a sibling of the sections, which is what
        // the paint does with it.
        let empty_state = (clip.is_none() && self.project.clips.is_empty()).then(|| {
            // The painted empty state's own words, so one state is not
            // described two ways.
            AccessNode::new(Role::StaticText)
                .with_name(format!("{INSPECTOR_EMPTY_TITLE}. {INSPECTOR_EMPTY_HINT}."))
                .with_key(0)
        });
        // Rows are a flat painted list but a nested tree: walk it once, opening
        // a group at each header and dropping the controls into it.
        let mut groups: Vec<AccessNode> = Vec::new();
        let mut current: Option<(Section, Vec<AccessNode>)> = None;
        for (i, row) in rows.iter().enumerate() {
            let focused = cursor && self.focus.inspector == i;
            match row {
                InspectorRow::Header(section) => {
                    if let Some((sec, kids)) = current.take() {
                        groups.push(self.a11y_section_node(sec, kids, false));
                    }
                    current = Some((*section, Vec::new()));
                    if let Some((_, kids)) = current.as_mut() {
                        // The header itself is the focus stop that folds and
                        // unfolds; it lives on the group as its own state, and
                        // the group carries `focused` for the cursor.
                        let _ = kids;
                    }
                    if focused {
                        groups.push(AccessNode::new(Role::Unknown).with_key(u64::MAX)); // placeholder, replaced below
                        groups.pop();
                    }
                }
                InspectorRow::Field(f) => {
                    let value = self.num_value(*f);
                    let node = AccessNode::new(Role::Slider)
                        .with_name(f.label())
                        .with_optional_value(value.map(|v| f.readout(v)))
                        // Keyed on the variant, not the label: two rows may
                        // legitimately read "Rotation" in their own sections.
                        .with_key(f.a11y_key())
                        .with_state(AccessState {
                            focusable: true,
                            focused,
                            ..Default::default()
                        });
                    if let Some((_, kids)) = current.as_mut() {
                        kids.push(node);
                    } else {
                        groups.push(node);
                    }
                }
                InspectorRow::Marker(m) => {
                    let pos = markers.iter().position(|(r, _, _)| r.id == m.id);
                    let Some(pos) = pos else { continue };
                    let (_, name, value) = &markers[pos];
                    let node = Self::a11y_marker_node(m, name, value, pos, markers.len(), focused);
                    if let Some((_, kids)) = current.as_mut() {
                        kids.push(node);
                    } else {
                        groups.push(node);
                    }
                }
                InspectorRow::AddMarker => {
                    let node = AccessNode::new(Role::Button)
                        .with_name(ADD_MARKER_LABEL)
                        .with_hashed_key(ADD_MARKER_KEY)
                        .with_state(AccessState {
                            focusable: true,
                            focused,
                            ..Default::default()
                        });
                    if let Some((_, kids)) = current.as_mut() {
                        kids.push(node);
                    } else {
                        groups.push(node);
                    }
                }
                InspectorRow::Pick(pk) => {
                    let node = self.a11y_pick_node(*pk, focused);
                    if let Some((_, kids)) = current.as_mut() {
                        kids.push(node);
                    } else {
                        groups.push(node);
                    }
                }
            }
        }
        if let Some((sec, kids)) = current.take() {
            groups.push(self.a11y_section_node(sec, kids, false));
        }
        if let Some(line) = empty_state {
            groups.insert(0, line);
        }
        // Re-mark the focused header now that every group exists: a header's
        // focus lives on its group node.
        if cursor {
            if let Some(InspectorRow::Header(target)) = rows.get(self.focus.inspector) {
                for g in groups.iter_mut() {
                    if g.key == Some(ui::a11y::hash_key(&target.label())) {
                        g.state.focused = true;
                    }
                }
            }
        }
        self.a11y_pane(Pane::Inspector, groups)
    }

    /// One inspector section, carrying whether it is folded and — when it is —
    /// a summary of what is inside, so a reader never has to unfold a section
    /// to learn its state.
    /// One marker as a reader meets it: the name on the row, when it is, the
    /// clip it travels with and the note written on it -- then the row's three
    /// controls, which the pointer reveals on hover and a reader has no other
    /// way to reach.
    fn a11y_marker_node(
        m: &MarkerRef,
        name: &str,
        value: &str,
        pos: usize,
        n: usize,
        focused: bool,
    ) -> AccessNode {
        let verbs: Vec<AccessNode> = MARKER_VERBS
            .iter()
            .map(|verb| {
                AccessNode::new(Role::Button)
                    .with_name(format!("{verb} {name}"))
                    .with_hashed_key((m.id, *verb))
                    .with_state(AccessState {
                        focusable: true,
                        ..Default::default()
                    })
            })
            .collect();
        AccessNode::new(Role::ListItem)
            .with_name(name.to_string())
            .with_value(value.to_string())
            // Identity-keyed: a marker keeps the node an assistive technology
            // is holding across a rename, and across another marker landing
            // earlier on the timeline and pushing this one down the list.
            .with_hashed_key(m.id)
            .with_position(pos + 1, n)
            .with_description("Activate to move the playhead here")
            .with_state(AccessState {
                focusable: true,
                focused,
                ..Default::default()
            })
            .with_children(verbs)
    }

    /// The clip the inspector is editing, if there is one. A convenience over
    /// the `selected()` + `clip()` pair the panel does constantly.
    fn selected_clip(&self) -> Option<&Clip> {
        self.selected().and_then(|id| self.project.clip(id))
    }

    /// One picker segment or switch, as a reader meets it: a radio inside its
    /// group when it is one of a set, a plain button when it stands alone —
    /// and, either way, whether it is the one in force.
    fn a11y_pick_node(&self, pk: PickField, focused: bool) -> AccessNode {
        let (name, role, selected) = match pk {
            PickField::Frame(i) => {
                let Some((label, w, h)) = FRAME_PRESETS.get(i) else {
                    return AccessNode::new(Role::Unknown).with_key(pk.a11y_key());
                };
                (
                    // The dimensions, not only the shorthand: "9:16" alone
                    // does not say what a reader is choosing.
                    format!("{label}, {w} by {h}"),
                    Role::RadioButton,
                    *w == self.project.width && *h == self.project.height,
                )
            }
            PickField::Rate(i) => {
                let Some(rate) = RATE_PRESETS.get(i) else {
                    return AccessNode::new(Role::Unknown).with_key(pk.a11y_key());
                };
                (
                    format!("{} fps", rate_words(*rate)),
                    Role::RadioButton,
                    (rate - self.project.fps).abs() < 1e-6,
                )
            }
            PickField::MatchFormat => (
                self.format_source()
                    .map(|m| format!("Match the shape of {}", m.name))
                    .unwrap_or_else(|| "Match media".to_string()),
                Role::Button,
                false,
            ),
            PickField::KeyEnable => {
                let on = self.selected_clip().is_some_and(|c| c.key.enabled);
                (
                    // The name says what pressing it will do now, the way the
                    // play button does — a switch whose name never changes
                    // leaves a reader guessing which way it is.
                    if on {
                        "Switch chroma key off".to_string()
                    } else {
                        "Switch chroma key on".to_string()
                    },
                    Role::Checkbox,
                    on,
                )
            }
            PickField::MaskShape(shape) => (
                format!("Mask shape {}", shape.label().to_lowercase()),
                Role::RadioButton,
                self.selected_clip().is_some_and(|c| c.mask.shape == shape),
            ),
            PickField::Transition(t) => (
                format!("Transition {}", t.label().to_lowercase()),
                Role::RadioButton,
                self.selected_clip().is_some_and(|c| c.transition == t),
            ),
            PickField::Label(slot) => (
                format!("Label colour {}", slot + 1),
                Role::RadioButton,
                self.selected_clip().is_some_and(|c| c.palette == slot),
            ),
            // A button, not a switch: the two directions are different
            // operations on different sets of clips, and its name says which
            // one pressing it will do — the same words the button carries.
            PickField::LinkToggle => (
                self.link_affordance().map_or_else(
                    || "Link audio and video".to_string(),
                    |o| o.label().to_string(),
                ),
                Role::Button,
                false,
            ),
            // Named for the lane it puts back, because a reader standing on it
            // has no fader in view to tell them which one that is.
            PickField::MixReset => (
                self.selected_track().map_or_else(
                    || "Reset gain and pan".to_string(),
                    |t| format!("Reset {} to 0.0 dB, centre", t.name),
                ),
                Role::Button,
                false,
            ),
        };
        // `checked` is what accesskit turns into a radio's or a checkbox's
        // "toggled": `selected` alone reads as a highlighted list row, which
        // is not what a reader needs to hear about a picker.
        let checkable = role != Role::Button;
        AccessNode::new(role)
            .with_name(name)
            .with_hashed_key(pk.tag())
            .with_state(AccessState {
                focusable: true,
                focused,
                selected,
                checked: checkable.then_some(selected),
                ..Default::default()
            })
    }

    /// Run whatever a pick row does — the same call the pointer runs on that
    /// control, so a reader and a mouse cannot do two different things.
    fn a11y_activate_pick(&mut self, pk: PickField, doc: &mut AutoCommit) -> bool {
        match pk {
            PickField::Frame(i) => {
                let Some((_, w, h)) = FRAME_PRESETS.get(i).copied() else {
                    return false;
                };
                self.set_project_frame_size(doc, w, h);
                true
            }
            PickField::Rate(i) => {
                let Some(rate) = RATE_PRESETS.get(i).copied() else {
                    return false;
                };
                self.set_project_fps(doc, rate);
                true
            }
            PickField::MatchFormat => {
                if self.format_source().is_none() {
                    return false;
                }
                self.match_project_format(doc);
                true
            }
            // Each runs the very call the pointer runs on that control.
            PickField::KeyEnable => {
                if self.selected().is_none() {
                    return false;
                }
                self.toggle_key_enabled(doc);
                true
            }
            PickField::MaskShape(shape) => {
                if self.selected().is_none() {
                    return false;
                }
                self.set_mask_shape(doc, shape);
                true
            }
            PickField::Transition(t) => {
                if self.selected().is_none() {
                    return false;
                }
                self.set_transition(doc, t);
                true
            }
            PickField::Label(slot) => {
                if self.selected().is_none() {
                    return false;
                }
                self.set_palette(doc, slot);
                true
            }
            PickField::LinkToggle => {
                if self.link_affordance().is_none() {
                    return false;
                }
                self.toggle_link_selected(doc);
                true
            }
            // Refused when the lane is already at unity and centre, which is
            // exactly when the panel draws no button: activating a control
            // that is not on screen would be a route only a reader has.
            PickField::MixReset => {
                if !self
                    .selected_track()
                    .is_some_and(crate::model::Track::mix_is_set)
                {
                    return false;
                }
                self.reset_selected_track_mix(doc);
                true
            }
        }
    }

    fn a11y_section_node(
        &self,
        section: Section,
        kids: Vec<AccessNode>,
        focused: bool,
    ) -> AccessNode {
        let folded = self.collapsed.contains(&section);
        AccessNode::new(Role::Group)
            .with_name(section.label())
            .with_optional_value(folded.then(|| self.a11y_section_summary(section)))
            .with_hashed_key(section.label())
            .with_state(AccessState {
                focusable: true,
                focused,
                expanded: Some(!folded),
                ..Default::default()
            })
            .with_children(kids)
    }

    /// A clip's geometry as it is at the playhead: every channel read through
    /// `param_at`, so an animated one gives the value the compositor is
    /// actually placing the picture with.
    ///
    /// Cropping and the anchor are absent because the model does not animate
    /// them — they frame the shot rather than move it — so their constants
    /// really are their values, and a caller reads them off `clip.transform`.
    fn geometry_at_playhead(&self, clip: &Clip) -> GeometryNow {
        let at = self.playhead;
        GeometryNow {
            scale_x: clip.param_at(Param::ScaleX, at),
            scale_y: clip.param_at(Param::ScaleY, at),
            pos_x: clip.param_at(Param::PosX, at),
            pos_y: clip.param_at(Param::PosY, at),
            rotation: clip.param_at(Param::Rotation, at),
        }
    }

    /// Whether any of the geometry channels carries a curve.
    ///
    /// A fact about the section in its own right, not merely a way of reading
    /// its numbers: keying a parameter from its default leaves the constant at
    /// exactly the aspect-fit, so a summary gated on the constants alone
    /// reported nothing at all about a clip whose picture crosses the frame.
    fn transform_is_animated(clip: &Clip) -> bool {
        [
            Param::ScaleX,
            Param::ScaleY,
            Param::PosX,
            Param::PosY,
            Param::Rotation,
        ]
        .into_iter()
        .any(|p| clip.anim.is_animated(p))
    }

    /// What a folded section says about itself.
    ///
    /// Every animatable number here is read through `param_at` at the playhead
    /// — the one door every other reader (compositor, mixer, export, the
    /// sliders themselves) already goes through. Read as raw fields they told a
    /// reader the value the clip is *not* playing: a keyframe supersedes the
    /// constant, so a clip pushing in reported "scale 1.00×" and a key ramping
    /// on reported that it was keying nothing.
    fn a11y_section_summary(&self, section: Section) -> String {
        let clip = self.selected().and_then(|id| self.project.clip(id));
        let at = self.playhead;
        match (section, clip) {
            (Section::Transform, Some(c)) => {
                let g = self.geometry_at_playhead(c);
                let mut s = format!(
                    "scale {:.2}×, position {:+.2}, {:+.2}, rotation {:.0}°",
                    g.scale_x, g.pos_x, g.pos_y, g.rotation
                );
                // Whether those numbers are going anywhere. Without it a
                // reader hears one still frame's worth of a moving picture and
                // has no way to know the section is animated at all.
                if Self::transform_is_animated(c) {
                    s.push_str(", animated");
                }
                s
            }
            (Section::Speed, Some(c)) => c.speed.label(),
            (Section::Mix, Some(c)) => match self.project.track(c.track) {
                Some(t) => {
                    let level = self.mix.track(t.id);
                    format!(
                        "{}, gain {}, pan {}{}",
                        t.name,
                        t.gain_words(),
                        t.pan_words(),
                        if level.is_some_and(|l| l.clipped) {
                            ", clipping"
                        } else {
                            ""
                        }
                    )
                }
                None => "no lane".into(),
            },
            // Worded from the same fields the visible summary reads, so a
            // reader is never told a tidier story than the screen tells.
            (Section::Key, Some(c)) => {
                let tolerance = c.param_at(Param::KeyTolerance, at);
                if !c.key.enabled {
                    "off".into()
                } else if tolerance <= 1e-6 {
                    "on, but the tolerance keys nothing".into()
                } else {
                    format!(
                        "keying {:.0}% tolerance, {:.0}% softness, {:.0}% spill removal",
                        tolerance * 100.0,
                        c.param_at(Param::KeySoftness, at) * 100.0,
                        c.param_at(Param::KeySpill, at) * 100.0
                    )
                }
            }
            (Section::Mask, Some(c)) => {
                if !c.mask.is_active() {
                    "off".into()
                } else {
                    format!(
                        "{} {:.0}% by {:.0}%, {}",
                        c.mask.shape.label().to_lowercase(),
                        c.param_at(Param::MaskW, at) * 100.0,
                        c.param_at(Param::MaskH, at) * 100.0,
                        if c.mask.invert {
                            "keeping the outside"
                        } else {
                            "keeping the inside"
                        }
                    )
                }
            }
            (Section::Effects, Some(c)) => {
                let n = c.effects.len();
                format!("{n} effect{}", if n == 1 { "" } else { "s" })
            }
            (Section::Markers, _) => {
                // Every marked instant the list shows -- the project's own and
                // each clip's, which is what `layout_markers` walks and what
                // the painted summary counts. Counting only the project's told
                // a reader a smaller number than the panel had rows for.
                let n = self.project.all_markers().len();
                format!("{n} marker{}", if n == 1 { "" } else { "s" })
            }
            // The same words the folded header shows, so a reader is never
            // told a tidier story than the panel tells.
            (Section::Curves, Some(c)) => {
                curve_summary(c, self.playhead).unwrap_or_else(|| "unchanged".into())
            }
            (Section::Title, Some(_)) => "title properties".into(),
            // Worded, not counted: the frame is the fact a reader needs from
            // this header whether or not it is folded.
            (Section::Project, _) => {
                format_words(self.project.width, self.project.height, self.project.fps)
            }
            _ => "collapsed".into(),
        }
    }

    /// The export dialog as a real dialog: modal, named, and reporting its own
    /// progress. It traps focus while open (see `Widget::traps_focus`).
    /// The project button, as a reader meets it.
    ///
    /// The dot on the button and this node say the same thing: a sighted user
    /// sees a badge, a reader hears [`PROJECT_UNSAVED`]. Neither is given the
    /// tidier story — and when the topbar is too narrow to paint the control
    /// at all, the node says that instead of offering a button nobody can
    /// press.
    fn a11y_project(&self) -> AccessNode {
        let visible = self.regions().project_btn.width() > 0.0;
        let mut node = AccessNode::new(Role::Button)
            .with_name(self.project_label())
            .with_value(self.project_status_line())
            .with_key(ui::a11y::hash_key(&"project"))
            .with_state(AccessState {
                focusable: visible,
                ..Default::default()
            });
        node = if !visible {
            node.with_description("Not shown at this width — use Ctrl+O to open, Ctrl+S to save")
        } else if self.project_is_dirty() {
            node.with_description(PROJECT_UNSAVED)
        } else {
            node
        };
        node
    }

    /// The command strip is custom-painted, so it needs an explicit semantic
    /// toolbar rather than relying on the visible labels. In particular, the
    /// compact layout hides Split and turns both side panes into icons; a
    /// reader must get the exact same currently-operable controls as a pointer.
    fn a11y_topbar(&self) -> AccessNode {
        let r = self.regions();
        let mut kids = Vec::new();
        if r.menu_btn.width() > 0.0 {
            kids.push(
                AccessNode::new(Role::ToggleButton)
                    .with_name("Toggle media bin")
                    .with_value("B")
                    .with_hashed_key(TOPBAR_BIN_KEY)
                    .with_state(AccessState {
                        focusable: true,
                        checked: Some(self.bin_flap.is_open(self.bin_layout())),
                        ..Default::default()
                    }),
            );
        }
        if self.bin_overlay_is_open() {
            return AccessNode::new(Role::Toolbar)
                .with_name("Video editor commands")
                .with_key(ui::a11y::hash_key(&TOPBAR_A11Y_KEY))
                .with_children(kids);
        }
        if r.inspector_btn.width() > 0.0 {
            kids.push(
                AccessNode::new(Role::ToggleButton)
                    .with_name("Toggle inspector")
                    .with_value("Ctrl+I")
                    .with_hashed_key(TOPBAR_INSPECTOR_KEY)
                    .with_state(AccessState {
                        focusable: true,
                        checked: Some(self.inspector_flap.is_open(self.inspector_split())),
                        ..Default::default()
                    }),
            );
        }
        kids.push(
            AccessNode::new(Role::Button)
                .with_name("Import media")
                .with_value("Ctrl+Shift+O")
                .with_hashed_key(TOPBAR_IMPORT_KEY)
                .with_state(AccessState {
                    focusable: true,
                    ..Default::default()
                }),
        );
        if r.split_btn.width() > 0.0 {
            kids.push(
                AccessNode::new(Role::Button)
                    .with_name("Split at playhead")
                    .with_value("S")
                    .with_hashed_key(TOPBAR_SPLIT_KEY)
                    .with_state(AccessState {
                        focusable: true,
                        ..Default::default()
                    }),
            );
        }
        kids.push(
            AccessNode::new(Role::Button)
                .with_name("Export video")
                .with_value("Ctrl+E")
                .with_hashed_key(TOPBAR_EXPORT_KEY)
                .with_state(AccessState {
                    focusable: true,
                    ..Default::default()
                }),
        );
        AccessNode::new(Role::Toolbar)
            .with_name("Editor commands")
            .with_hashed_key(TOPBAR_A11Y_KEY)
            .with_children(kids)
    }

    /// The unsaved-work question, carrying the same three answers the card
    /// paints and the same account of what each one costs.
    fn a11y_discard_dialog(&self) -> AccessNode {
        let Some(pending) = &self.pending_open else {
            return AccessNode::new(Role::Dialog).with_name("Save first?");
        };
        let opening = crate::project_file::recents::label(&pending.path);
        let kids = vec![
            AccessNode::new(Role::StaticText)
                .with_name("What happens")
                .with_value(format!(
                    "{} has never been saved to a file. Opening {opening} replaces it. \
                     Ctrl+Z brings it back.",
                    pending.losing,
                ))
                .with_key(0),
            AccessNode::new(Role::Button)
                .with_name("Cancel")
                .with_key(1)
                .with_state(AccessState {
                    focusable: true,
                    ..Default::default()
                }),
            AccessNode::new(Role::Button)
                .with_name("Open anyway")
                .with_key(2)
                .with_state(AccessState {
                    focusable: true,
                    ..Default::default()
                }),
            AccessNode::new(Role::Button)
                .with_name("Save…")
                .with_key(3)
                .with_state(AccessState {
                    focusable: true,
                    ..Default::default()
                }),
        ];
        AccessNode::new(Role::Dialog)
            .with_name(format!("Save {} first?", pending.losing))
            .with_key(ui::a11y::hash_key(&"discard"))
            .with_state(AccessState {
                modal: true,
                ..Default::default()
            })
            .with_children(kids)
    }

    /// The render's state as a node of its own, outside the dialog's subtree.
    ///
    /// `export_status` was read in exactly two places and both were inside the
    /// `show_export` gate, so an assistive technology whose user had dismissed
    /// the dialog was told nothing at all about a render that was still going:
    /// no progress, no outcome, no failure. This is the same live region in
    /// the tree whenever the dialog is not there to carry it — so a running
    /// export is announced exactly once at any moment, and never zero times.
    fn a11y_export_status(&self) -> Option<AccessNode> {
        if self.presentation.export_open {
            // The dialog carries its own progress node; two live regions
            // reporting one render would narrate it twice.
            return None;
        }
        let key = ui::a11y::hash_key(&"export-status");
        match &self.export_status {
            ExportStatus::Idle => None,
            ExportStatus::Rendering { progress } => Some(
                AccessNode::new(Role::ProgressBar)
                    .with_name("Export progress")
                    .with_value(format!("{}%", (progress * 100.0).round() as i32))
                    .with_key(key)
                    .with_live(ui::a11y::Politeness::Polite)
                    .with_state(AccessState {
                        busy: true,
                        ..Default::default()
                    }),
            ),
            // Finished: operable, because activating it opens the dialog that
            // holds the whole path and the settings that produced it.
            ExportStatus::Done { path } => Some(
                AccessNode::new(Role::Button)
                    .with_name("Export finished")
                    .with_value(path.clone())
                    .with_key(key)
                    .with_live(ui::a11y::Politeness::Polite)
                    .with_state(AccessState {
                        focusable: true,
                        ..Default::default()
                    }),
            ),
            // A render that did not happen interrupts: the user is very
            // likely to act on the next thing believing a file exists.
            ExportStatus::Failed { reason } => Some(
                AccessNode::new(Role::Button)
                    .with_name("Export failed")
                    .with_value(reason.clone())
                    .with_key(key)
                    .with_live(ui::a11y::Politeness::Assertive)
                    .with_state(AccessState {
                        focusable: true,
                        ..Default::default()
                    }),
            ),
        }
    }

    fn a11y_export_dialog(&self) -> AccessNode {
        let mut kids = vec![
            // The same honest size line the dialog paints. On a box whose
            // encoders ignore their bitrate target this withholds the megabyte
            // figure rather than stating a confident wrong number — and a
            // reader must get that caveat too, not a tidier fiction.
            AccessNode::new(Role::StaticText)
                .with_name("Output")
                .with_value(format!(
                    "{}, {}, {}",
                    self.export.resolution.label(),
                    self.export.container.label(),
                    self.export.estimate_label(&self.project),
                ))
                .with_key(0),
            AccessNode::new(Role::Button)
                .with_name("Start export")
                .with_key(1)
                .with_state(AccessState {
                    focusable: true,
                    disabled: !self.export_is_offered(),
                    ..Default::default()
                }),
            AccessNode::new(Role::Button)
                .with_name("Close export")
                .with_key(2)
                .with_state(AccessState {
                    focusable: true,
                    ..Default::default()
                }),
        ];
        if let ExportStatus::Rendering { progress } = self.export_status {
            kids.push(
                AccessNode::new(Role::ProgressBar)
                    .with_name("Export progress")
                    .with_value(format!("{}%", (progress * 100.0).round() as i32))
                    .with_key(3)
                    // A render's progress changes with no focus movement, so it
                    // must announce itself or the user is left guessing.
                    .with_live(ui::a11y::Politeness::Polite)
                    .with_state(AccessState {
                        busy: true,
                        ..Default::default()
                    }),
            );
        }
        if let ExportStatus::Failed { reason } = &self.export_status {
            // The compact topbar chip is intentionally elided, but it must
            // never be the only place the reason exists. Ctrl+E opens this
            // dialog from the keyboard and this value is deliberately the
            // complete worker/encoder message for a screen reader.
            kids.push(
                AccessNode::new(Role::StaticText)
                    .with_name("Export failure details")
                    .with_value(reason.clone())
                    .with_key(ui::a11y::hash_key(&"export.failure-details"))
                    .with_live(ui::a11y::Politeness::Assertive),
            );
        }
        AccessNode::new(Role::Dialog)
            .with_name("Export video")
            .with_key(ui::a11y::hash_key(&"export"))
            .with_state(AccessState {
                modal: true,
                ..Default::default()
            })
            .with_children(kids)
    }
}

/// The keyboard shortcuts the editor answers to, grouped as the sheet shows
/// them. Fifteen shortcuts existed and none of them were written down anywhere
/// in the product; a sheet is also how a reader user learns they exist at all,
/// which is why it is an accessibility affordance and not just a convenience.
const SHORTCUTS: [(&str, &[(&str, &str)]); 5] = [
    (
        "Project",
        &[
            ("Ctrl+O", "Open a project"),
            ("Ctrl+S", "Save (Shift: save as…)"),
            ("Ctrl+Shift+O", "Import media"),
            ("Ctrl+E", "Export video"),
        ],
    ),
    (
        "Playback",
        &[
            ("Space", "Play / pause"),
            ("J / K / L", "Shuttle back / stop / fwd"),
            ("Home / End", "Jump to start / end"),
            (", / .", "Previous / next marker"),
            ("C", "Go to where the mix clipped"),
            ("G", "Go to timecode"),
        ],
    ),
    (
        "Editing",
        &[
            ("I / O", "Trim clip in / out at playhead"),
            ("Shift+I / O", "Mark render range in / out"),
            ("Shift+X", "Clear the render range"),
            ("S", "Split at playhead"),
            ("M", "Add marker"),
            ("T", "Add title"),
            ("Delete", "Delete (Shift: ripple)"),
            ("Arrows", "Nudge, or step playhead"),
        ],
    ),
    (
        "Clipboard",
        &[
            ("Ctrl+X / C / V", "Cut, copy, paste"),
            ("Ctrl+D", "Duplicate selection"),
            ("Ctrl+A", "Select all clips"),
            ("Ctrl+Z", "Undo"),
            // Both spellings work and neither was written down, so the sheet
            // claimed the editor had undo and no redo.
            ("Ctrl+Shift+Z", "Redo (or Ctrl+Y)"),
        ],
    ),
    (
        "View",
        &[
            ("Tab", "Next pane (Shift+Tab: previous)"),
            ("Page Up / Down", "Scroll timeline (when it has focus)"),
            ("Z", "Zoom to fit"),
            ("+ / -", "Zoom in / out"),
            ("B", "Toggle media bin"),
            ("Ctrl+I", "Toggle inspector"),
            ("?", "This sheet"),
        ],
    ),
];

/// Where the shortcut table starts inside its card, below the title and the
/// "Esc or ? to close" line.
const HELP_TOP: f64 = 76.0;
/// One group header, one row, and the gap after a group, in pixels.
const HELP_HEADER_H: f64 = 20.0;
const HELP_ROW_H: f64 = 19.0;
const HELP_GROUP_GAP: f64 = 10.0;

/// How tall each shortcut group draws.
fn help_group_h(rows: usize) -> f64 {
    HELP_HEADER_H + rows as f64 * HELP_ROW_H + HELP_GROUP_GAP
}

/// Which group starts the second column, and how tall the taller column is.
///
/// Both derived from the table. A hard-coded split with a hard-coded card
/// height cuts rows off the bottom the moment a group is added — silently,
/// because the row loop simply stops drawing — so the sheet claimed to be the
/// list of shortcuts while omitting two of them.
#[allow(
    clippy::indexing_slicing,
    reason = "the shortcut label prefix is split only at the ASCII command separator emitted by this module"
)]
fn help_columns(two_col: bool) -> (usize, f64) {
    let heights: Vec<f64> = SHORTCUTS
        .iter()
        .map(|(_, rows)| help_group_h(rows.len()))
        .collect();
    let total: f64 = heights.iter().sum();
    if !two_col {
        return (SHORTCUTS.len(), total);
    }
    // Fill the first column until taking one more group would put it further
    // past half than stopping leaves it short.
    let mut first = 0.0;
    let mut split = heights.len();
    for (i, h) in heights.iter().enumerate() {
        if i > 0 && (first + h - total / 2.0).abs() > (total / 2.0 - first).abs() {
            split = i;
            break;
        }
        first += h;
    }
    let second: f64 = heights[split.min(heights.len())..].iter().sum();
    (split, first.max(second))
}

impl VideoEditorApp {
    /// The shortcut sheet as accessible content — every binding readable in
    /// order, so the sheet is as useful to a reader as it is to the eye.
    fn a11y_help_sheet(&self) -> AccessNode {
        let mut kids: Vec<AccessNode> = Vec::new();
        let mut key = 0u64;
        for (group, rows) in SHORTCUTS {
            kids.push(
                AccessNode::new(Role::Heading { level: 2 })
                    .with_name(group)
                    .with_key(key),
            );
            key += 1;
            for (combo, what) in rows {
                kids.push(
                    AccessNode::new(Role::StaticText)
                        .with_name(*what)
                        .with_value(*combo)
                        .with_key(key),
                );
                key += 1;
            }
        }
        kids.push(
            AccessNode::new(Role::Button)
                .with_name("Close shortcuts")
                .with_key(key)
                .with_state(AccessState {
                    focusable: true,
                    ..Default::default()
                }),
        );
        AccessNode::new(Role::Dialog)
            .with_name("Keyboard shortcuts")
            .with_key(ui::a11y::hash_key(&"help"))
            .with_state(AccessState {
                modal: true,
                ..Default::default()
            })
            .with_children(kids)
    }

    // ---- keyboard navigation ---------------------------------------------

    /// Move to the next / previous pane, clamping each pane's cursor to what it
    /// currently holds so Tab never lands on a row that has since been deleted.
    fn focus_pane(&mut self, back: bool) {
        if self.bin_overlay_is_open() {
            self.focus.pane = Pane::Bin;
            self.clamp_pane_cursor();
            return;
        }
        self.focus.pane = self.focus.pane.step(back);
        self.clamp_pane_cursor();
    }

    /// Whether `pane` should paint the keyboard cursor right now.
    ///
    /// Gated on the host having actually given this widget the keyboard: a ring
    /// drawn while the focus is somewhere else entirely would point at a
    /// control the next key will not reach, which is the opposite of what a
    /// focus indicator is for. Only the pane holding the cursor paints one —
    /// the other three keep their cursors so Tab returns to them, but they are
    /// not where the keyboard is.
    fn shows_focus_cursor(&self, pane: Pane) -> bool {
        self.presentation.keyboard_focus && self.focus.pane == pane
    }

    /// Move the keyboard cursor to whatever a press landed on.
    ///
    /// A press claims the keyboard, but until now it left the cursor wherever
    /// it already was — so clicking a bin row and then pressing Down carried
    /// on from some other row entirely, and the ring appeared somewhere the
    /// user had not been looking. The pointer and the keyboard address the
    /// same controls; a click is the plainest possible statement of which one
    /// you mean next.
    ///
    /// Resolved through the same geometry the ring and the hit test read, and
    /// the bin is matched by media id rather than by position so the painted
    /// order and the walked order cannot drift into disagreeing.
    fn place_cursor_at(&mut self, r: &Regions, p: Point) {
        if let Some((mid, _)) = self
            .bin_item_rects(r)
            .into_iter()
            .find(|(_, rect)| rect.contains(p))
            && let Some(i) = self.a11y_bin_items().iter().position(|m| *m == mid)
        {
            self.focus.pane = Pane::Bin;
            self.focus.bin = i;
            return;
        }
        if let Some(i) = self
            .transport_buttons(r)
            .iter()
            .position(|(_, rect)| rect.contains(p))
        {
            self.focus.pane = Pane::Transport;
            self.focus.transport = i;
            return;
        }
        if self.inspector_viewport(r).contains(p) {
            let l = self.inspector_layout(r);
            let rows = self.a11y_inspector_rows();
            if let Some(i) = rows.iter().position(|row| {
                Self::inspector_row_rect(&l, *row).is_some_and(|rect| rect.contains(p))
            }) {
                self.focus.pane = Pane::Inspector;
                self.focus.inspector = i;
            }
        }
    }

    /// The box the keyboard cursor's ring is drawn in for one inspector row,
    /// resolved through the very layout the renderer and the hit test read.
    ///
    /// A slider row is more than its 9px track — the label above it and the key
    /// control at its head belong to the same stop — so a `Field`'s ring is
    /// grown up over them, which is also what keeps the ring inside the 34px
    /// row pitch instead of hugging a bar the eye reads as the value. Every
    /// other row is its own painted box already.
    ///
    /// `None` means the panel is not drawing that control right now, in which
    /// case there is nothing to ring.
    fn inspector_focus_band(l: &InspectorLayout, row: InspectorRow) -> Option<Rect> {
        let rect = Self::inspector_row_rect(l, row)?;
        if matches!(row, InspectorRow::Field(_)) {
            return Some(Rect::new(
                rect.x0 - 3.0,
                rect.y0 - 21.0,
                rect.x1 + 3.0,
                rect.y1 + 2.0,
            ));
        }
        Some(rect.inset(2.0))
    }

    /// The rect the panel laid out for one walked row. The row list and the
    /// layout are built by different code, so this is the single place that
    /// maps between them — the keyboard cursor and the tests that prove every
    /// stop is a control the panel really drew both read it.
    fn inspector_row_rect(l: &InspectorLayout, row: InspectorRow) -> Option<Rect> {
        match row {
            InspectorRow::Header(s) => l.sections.iter().find(|(x, _)| *x == s).map(|(_, r)| *r),
            InspectorRow::Field(NumField::Opacity) => l.opacity,
            InspectorRow::Field(NumField::Volume) => l.volume,
            InspectorRow::Field(NumField::Brightness) => l.brightness,
            InspectorRow::Field(NumField::Contrast) => l.contrast,
            InspectorRow::Field(NumField::Saturation) => l.saturation,
            InspectorRow::Field(NumField::FadeIn) => l.fade_in,
            InspectorRow::Field(NumField::FadeOut) => l.fade_out,
            InspectorRow::Field(NumField::TransitionLen) => l.transition_len,
            InspectorRow::Field(f) => l.num(f),
            InspectorRow::Marker(m) => l
                .marker_rows
                .iter()
                .find(|(x, _)| x.id == m.id)
                .map(|(_, r)| *r),
            InspectorRow::AddMarker => l.add_marker,
            InspectorRow::Pick(PickField::Frame(i)) => {
                FRAME_PRESETS.get(i).and_then(|(_, w, h)| {
                    l.project_frames
                        .iter()
                        .find(|((pw, ph), _)| pw == w && ph == h)
                        .map(|(_, r)| *r)
                })
            }
            InspectorRow::Pick(PickField::Rate(i)) => RATE_PRESETS.get(i).and_then(|rate| {
                l.project_rates
                    .iter()
                    .find(|(x, _)| (x - rate).abs() < 1e-9)
                    .map(|(_, r)| *r)
            }),
            InspectorRow::Pick(PickField::MatchFormat) => l.project_match,
            InspectorRow::Pick(PickField::KeyEnable) => l.key_enable,
            InspectorRow::Pick(PickField::MaskShape(shape)) => l
                .mask_shapes
                .iter()
                .find(|(x, _)| *x == shape)
                .map(|(_, r)| *r),
            InspectorRow::Pick(PickField::Transition(t)) => {
                l.transitions.iter().find(|(x, _)| *x == t).map(|(_, r)| *r)
            }
            InspectorRow::Pick(PickField::Label(slot)) => {
                l.colors.iter().find(|(x, _)| *x == slot).map(|(_, r)| *r)
            }
            InspectorRow::Pick(PickField::LinkToggle) => l.link,
            InspectorRow::Pick(PickField::MixReset) => l.mix_reset,
        }
    }

    fn clamp_pane_cursor(&mut self) {
        let bin = self.a11y_bin_items().len();
        self.focus.bin = self.focus.bin.min(bin.saturating_sub(1));
        self.focus.transport = self.focus.transport.min(TRANSPORT_NAMES.len() - 1);
        let rows = self.a11y_inspector_rows().len();
        self.focus.inspector = self.focus.inspector.min(rows.saturating_sub(1));
    }

    /// Keep the keyboard/AT active descendant in the inspector's actual body
    /// viewport.  The fixed identity card deliberately is not scrollable, so
    /// its height must be part of this calculation rather than treating the
    /// whole side panel as available space.
    fn reveal_inspector_focus(&mut self) {
        let r = self.regions();
        let rows = self.a11y_inspector_rows();
        let Some(row) = rows.get(self.focus.inspector).copied() else {
            return;
        };
        let Some(band) = Self::inspector_focus_band(&self.inspector_layout(&r), row) else {
            return;
        };
        let view = self.inspector_viewport(&r);
        if band.y0 < view.y0 {
            self.inspector_scroll -= view.y0 - band.y0;
        } else if band.y1 > view.y1 {
            self.inspector_scroll += band.y1 - view.y1;
        }
        let max = self.inspector_max_scroll(&r);
        self.inspector_scroll = self.inspector_scroll.clamp(0.0, max);
    }

    /// Arrow navigation inside the focused pane. Returns whether the key was
    /// consumed — the timeline deliberately consumes only Up/Down, leaving
    /// Left/Right to nudge the selection and step the playhead as every
    /// non-linear editor does.
    fn nav_arrow(&mut self, up: bool, vertical: bool, state: &mut AutoCommit) -> bool {
        let _ = state;
        match self.focus.pane {
            Pane::Bin if vertical => {
                let n = self.a11y_bin_items().len();
                if n == 0 {
                    return false;
                }
                self.focus.bin = step_index(self.focus.bin, n, up);
                self.selected_media = self.a11y_bin_items().get(self.focus.bin).copied();
                true
            }
            Pane::Transport if !vertical => {
                self.focus.transport = step_index(self.focus.transport, TRANSPORT_NAMES.len(), up);
                true
            }
            Pane::Inspector if vertical => {
                let n = self.a11y_inspector_rows().len();
                if n == 0 {
                    return false;
                }
                self.focus.inspector = step_index(self.focus.inspector, n, up);
                self.reveal_inspector_focus();
                true
            }
            // Up/Down walk the clips across tracks; Left/Right stay editing
            // keys, which is what an editor's arrows mean.
            Pane::Timeline if vertical => self.step_clip_across_tracks(up),
            _ => false,
        }
    }

    /// Move the selection to the clip on the previous / next track that sits
    /// nearest the current one in time — the "same place, one track up" move,
    /// not an arbitrary index walk.
    #[allow(
        clippy::indexing_slicing,
        reason = "track stepping computes the target ordinal from the non-empty ordered track list before reading it"
    )]
    fn step_clip_across_tracks(&mut self, up: bool) -> bool {
        let Some(current) = self
            .selection
            .primary()
            .and_then(|id| self.project.clip(id))
            .cloned()
        else {
            // Nothing selected: take the first clip on the first track that has one.
            if let Some(first) = self.project.clips.first().map(|c| c.id) {
                self.selection.select_only(first);
                return true;
            }
            return false;
        };
        let Some(cur_track) = self
            .project
            .tracks
            .iter()
            .position(|t| t.id == current.track)
        else {
            return false;
        };
        let n = self.project.tracks.len();
        // Walk outward until a track with any clip is found, so an empty track
        // between two full ones is stepped over rather than swallowing the key.
        for hop in 1..=n {
            let idx = if up {
                cur_track.checked_sub(hop)
            } else {
                (cur_track + hop < n).then_some(cur_track + hop)
            };
            let Some(idx) = idx else { break };
            let tid = self.project.tracks[idx].id;
            let clips = self.project.clips_on(tid);
            if clips.is_empty() {
                continue;
            }
            // Nearest by start time to where we already are.
            let best = clips
                .iter()
                .min_by(|a, b| {
                    (a.start - current.start)
                        .abs()
                        .total_cmp(&(b.start - current.start).abs())
                })
                .map(|c| c.id);
            if let Some(best) = best {
                self.selection.select_only(best);
                return true;
            }
        }
        false
    }

    /// Activate whatever the pane cursor is on — the keyboard's Enter, routed
    /// through the same `perform_action_at` an assistive technology uses, so
    /// the two can never do different things.
    fn activate_focused(&mut self, state: &mut AutoCommit) -> bool {
        use ui::a11y::{Action, hash_key};
        let pane = self.focus.pane;
        let rest: Vec<u64> = match pane {
            Pane::Bin => match self.a11y_bin_items().get(self.focus.bin) {
                Some(m) => vec![hash_key(m)],
                None => return false,
            },
            Pane::Transport => vec![hash_key(&self.focus.transport)],
            Pane::Timeline => {
                let Some(clip) = self
                    .selection
                    .primary()
                    .and_then(|id| self.project.clip(id))
                else {
                    return false;
                };
                vec![hash_key(&clip.track), hash_key(&clip.id)]
            }
            Pane::Inspector => match self.a11y_inspector_rows().get(self.focus.inspector) {
                Some(InspectorRow::Header(sec)) => vec![hash_key(&sec.label())],
                // A marker's path runs through the MARKERS section, the same
                // shape a control's does.
                Some(InspectorRow::Marker(m)) => {
                    vec![hash_key(&Section::Markers.label()), hash_key(&m.id)]
                }
                Some(InspectorRow::AddMarker) => {
                    vec![
                        hash_key(&Section::Markers.label()),
                        hash_key(&ADD_MARKER_KEY),
                    ]
                }
                Some(InspectorRow::Field(f)) => {
                    let key = f.a11y_key();
                    Self::a11y_row_path(&self.a11y_inspector_rows(), self.focus.inspector, key)
                }
                Some(InspectorRow::Pick(pk)) => {
                    let key = pk.a11y_key();
                    Self::a11y_row_path(&self.a11y_inspector_rows(), self.focus.inspector, key)
                }
                None => return false,
            },
        };
        let mut path = vec![pane.key()];
        path.extend(rest);
        let root = automerge::ROOT;
        self.perform_action_at(&Action::Activate, &path, state, &root)
    }

    /// The hover label for whatever icon-only control sits under `p`, or
    /// `None` over anything that already carries its own words.
    fn tooltip_at(&self, p: Point) -> Option<&str> {
        let r = self.regions();
        for (action, rect) in self.transport_buttons(&r) {
            if rect.contains(p) {
                return Some(match action {
                    TransportAction::ToStart => "Jump to start",
                    TransportAction::StepBack => "Step back one frame",
                    TransportAction::PlayPause => {
                        if self.is_moving() {
                            "Pause"
                        } else {
                            "Play"
                        }
                    }
                    TransportAction::StepFwd => "Step forward one frame",
                    TransportAction::ToEnd => "Jump to end",
                });
            }
        }
        if r.project_btn.area() > 0.0 && r.project_btn.contains(p) {
            // The one control here whose label is not fixed: it names the
            // project, so the tooltip carries the verb the label cannot.
            return Some("Open, save, or switch project");
        }
        if r.export_btn.contains(p) {
            return Some("Export video (Ctrl+E)");
        }
        if self.export_chip_is_shown() && r.export_chip.contains(p) {
            // The chip is 176px wide, so a long failure is elided in it. The
            // hover carries the whole thing rather than making the reason
            // something only the dialog knows — and the accessible node
            // carries it too, for a reader who cannot hover.
            return match &self.export_status {
                ExportStatus::Failed { reason } => Some(reason.as_str()),
                ExportStatus::Done { path } => Some(path.as_str()),
                _ => Some("Export in progress — press to show it"),
            };
        }
        if r.import_btn.contains(p) {
            return Some("Import media (Ctrl+Shift+O)");
        }
        if r.split_btn.contains(p) {
            return Some("Split at playhead (S)");
        }
        if r.menu_btn.area() > 0.0 && r.menu_btn.contains(p) {
            return Some("Toggle media bin (B)");
        }
        if r.inspector_btn.area() > 0.0 && r.inspector_btn.contains(p) {
            return Some("Toggle inspector (Ctrl+I)");
        }
        None
    }

    // ---- routed accessibility actions ------------------------------------

    #[allow(
        clippy::indexing_slicing,
        reason = "bin commands decode only the fixed key layout emitted by a11y_bin"
    )]
    fn a11y_bin_action(
        &mut self,
        action: &ui::a11y::Action,
        rest: &[u64],
        state: &mut AutoCommit,
    ) -> bool {
        use ui::a11y::{Action, hash_key};
        let Some(&key) = rest.first() else {
            return false;
        };
        let items = self.a11y_bin_items();
        // A row an AT is holding can be gone; match nothing rather than acting
        // on whichever import now occupies that slot.
        let Some(i) = items.iter().position(|m| hash_key(m) == key) else {
            return false;
        };
        self.focus.pane = Pane::Bin;
        self.focus.bin = i;
        match action {
            Action::Focus => {
                self.selected_media = Some(items[i]);
                true
            }
            Action::Activate => {
                self.selected_media = Some(items[i]);
                self.add_media_to_timeline(state, items[i]);
                true
            }
            _ => false,
        }
    }

    #[allow(
        clippy::indexing_slicing,
        reason = "transport commands decode only the fixed key layout emitted by a11y_transport"
    )]
    fn a11y_transport_action(&mut self, action: &ui::a11y::Action, rest: &[u64]) -> bool {
        use ui::a11y::{Action, hash_key};
        let Some(&key) = rest.first() else {
            return false;
        };
        // History first: its keys are hashed from the verb, so they cannot
        // collide with the transport's positional ones.
        for which in [ui::UndoRequest::Undo, ui::UndoRequest::Redo] {
            if hash_key(&which.verb()) != key {
                continue;
            }
            return match action {
                Action::Focus => true,
                // `request` refuses an unavailable verb, so activating a node
                // the tree reported disabled does nothing rather than
                // pretending to.
                Action::Activate => self.undo.request(which),
                _ => false,
            };
        }
        let Some(i) = (0..TRANSPORT_NAMES.len()).find(|i| hash_key(i) == key) else {
            return false;
        };
        self.focus.pane = Pane::Transport;
        self.focus.transport = i;
        match action {
            Action::Focus => true,
            Action::Activate => {
                self.transport_action(TRANSPORT_NAMES[i].0);
                true
            }
            _ => false,
        }
    }

    fn a11y_timeline_action(
        &mut self,
        action: &ui::a11y::Action,
        rest: &[u64],
        state: &mut AutoCommit,
    ) -> bool {
        use ui::a11y::{Action, hash_key};
        let Some(&track_key) = rest.first() else {
            return false;
        };
        // The master strip's node. Activating it is the same walk the red
        // chip does under the pointer — one route, so a reader and a mouse
        // land in the same place.
        if track_key == hash_key(&MIX_METER_KEY) {
            return match action {
                Action::Focus => true,
                Action::Activate => self.go_to_next_clipping(),
                _ => false,
            };
        }
        let Some(track) = self
            .project
            .tracks
            .iter()
            .find(|t| hash_key(&t.id) == track_key)
        else {
            return false;
        };
        let tid = track.id;
        self.focus.pane = Pane::Timeline;
        let Some(&child_key) = rest.get(1) else {
            // The track itself: focus is all a container affords.
            return matches!(action, Action::Focus);
        };
        // A track's own toggles come first, then its clips.
        for name in ["Mute", "Solo", "Lock"] {
            if hash_key(&(tid, name)) == child_key {
                return match action {
                    Action::Focus => true,
                    Action::Activate | Action::ToggleCheck => {
                        match name {
                            "Mute" => self.toggle_track_mute(state, tid),
                            "Solo" => self.toggle_track_solo(state, tid),
                            _ => self.toggle_track_lock(state, tid),
                        }
                        true
                    }
                    _ => false,
                };
            }
        }
        let Some(clip) = self
            .project
            .clips_on(tid)
            .into_iter()
            .find(|c| hash_key(&c.id) == child_key)
            .map(|c| c.id)
        else {
            return false;
        };
        match action {
            Action::Focus => {
                self.selection.set_primary(clip);
                true
            }
            Action::Activate => {
                // Activating a clip selects it alone and parks the playhead at
                // its head — what double-clicking it does.
                self.selection.select_only(clip);
                if let Some(c) = self.project.clip(clip) {
                    self.playhead = c.start;
                    self.playback.chase_playhead = true;
                }
                true
            }
            _ => false,
        }
    }

    fn a11y_inspector_action(
        &mut self,
        action: &ui::a11y::Action,
        rest: &[u64],
        doc: &mut AutoCommit,
    ) -> bool {
        use ui::a11y::{Action, hash_key};
        let Some(&section_key) = rest.first() else {
            return false;
        };
        self.focus.pane = Pane::Inspector;
        let rows = self.a11y_inspector_rows();
        // A control lives inside its section, so its path is
        // `[inspector, section, control]`; the section header itself is one
        // level shorter. Address whichever the path actually names.
        let key = rest.get(1).copied().unwrap_or(section_key);
        for (i, row) in rows.iter().enumerate() {
            match row {
                InspectorRow::Header(sec) if rest.len() == 1 && hash_key(&sec.label()) == key => {
                    self.focus.inspector = i;
                    self.reveal_inspector_focus();
                    return match action {
                        Action::Focus => true,
                        Action::Activate | Action::Expand | Action::Collapse => {
                            if self.collapsed.contains(sec) {
                                self.collapsed.remove(sec);
                            } else {
                                self.collapsed.insert(*sec);
                            }
                            true
                        }
                        _ => false,
                    };
                }
                InspectorRow::Field(f) if f.a11y_key() == key => {
                    self.focus.inspector = i;
                    self.reveal_inspector_focus();
                    return match action {
                        Action::Focus => true,
                        Action::Increment => self.a11y_nudge_field(*f, 1.0, doc),
                        Action::Decrement => self.a11y_nudge_field(*f, -1.0, doc),
                        _ => false,
                    };
                }
                InspectorRow::Marker(m) if hash_key(&m.id) == key => {
                    self.focus.inspector = i;
                    self.reveal_inspector_focus();
                    let m = *m;
                    // `[inspector, MARKERS, marker, verb]` names one of the
                    // row's three buttons; one level shorter is the row
                    // itself, whose body seeks when it is clicked.
                    if let Some(&verb) = rest.get(2) {
                        return self.a11y_marker_verb(m, verb, action, doc);
                    }
                    return match action {
                        Action::Focus => true,
                        Action::Activate => {
                            self.seek_to_marker(m);
                            true
                        }
                        _ => false,
                    };
                }
                InspectorRow::AddMarker if hash_key(&ADD_MARKER_KEY) == key => {
                    self.focus.inspector = i;
                    self.reveal_inspector_focus();
                    return match action {
                        Action::Focus => true,
                        Action::Activate => {
                            self.add_marker_here(doc);
                            true
                        }
                        _ => false,
                    };
                }
                InspectorRow::Pick(pk) if pk.a11y_key() == key => {
                    self.focus.inspector = i;
                    self.reveal_inspector_focus();
                    let pk = *pk;
                    return match action {
                        Action::Focus => true,
                        Action::Activate => self.a11y_activate_pick(pk, doc),
                        _ => false,
                    };
                }
                _ => {}
            }
        }
        false
    }

    /// One of a marker row's three buttons, addressed by an assistive
    /// technology. Each runs the same call the pointer runs on that button,
    /// so a reader and a mouse cannot do two different things to a marker.
    ///
    /// `Activate` opens the inline editor the pointer opens, for a user
    /// driving the real keyboard through the row. `SetValue` writes the text
    /// outright, through the same commit the inline editor ends at -- an
    /// assistive technology has no caret to type into an `EditableLabel`
    /// with, so an editor it cannot reach would be an affordance in name
    /// only. It reports the commit's own verdict, so the refusal of an empty
    /// name reaches the caller as a refusal rather than a silent no-op.
    fn a11y_marker_verb(
        &mut self,
        m: MarkerRef,
        verb: u64,
        action: &ui::a11y::Action,
        doc: &mut AutoCommit,
    ) -> bool {
        use ui::a11y::{Action, hash_key};
        let field = if hash_key(&(m.id, MARKER_VERBS[0])) == verb {
            MarkerField::Note
        } else if hash_key(&(m.id, MARKER_VERBS[1])) == verb {
            MarkerField::Name
        } else if hash_key(&(m.id, MARKER_VERBS[2])) == verb {
            return match action {
                Action::Focus => true,
                Action::Activate => {
                    self.delete_marker(doc, m);
                    true
                }
                _ => false,
            };
        } else {
            return false;
        };
        match action {
            Action::Focus => true,
            Action::Activate => {
                self.begin_marker_edit(m, field);
                true
            }
            Action::SetValue(text) => {
                let text = text.clone();
                self.commit_marker_text(doc, m, field, &text)
            }
            _ => false,
        }
    }

    /// Step a numeric control one twentieth of its span — the same grain a
    /// keyboard arrow gives a slider elsewhere in the toolkit — through the
    /// same `set_num` a mouse drag writes, then commit it the way releasing a
    /// drag does. An adjustment an assistive technology makes must land in the
    /// document exactly like a dragged one, or the two disagree about what the
    /// clip is.
    fn a11y_nudge_field(&mut self, f: NumField, dir: f64, doc: &mut AutoCommit) -> bool {
        let Some(v) = self.num_value(f) else {
            return false;
        };
        // A fade and a transition ramp are measured against the clip they sit
        // on, so a twentieth of *their* span is a twentieth of the clip — not
        // of `NumField::range`'s widest-possible pair, which would step an
        // eight-second clip's fade in three-minute jumps.
        let next = match self.field_span(f) {
            Some((lo, hi)) => (v + dir * 0.05 * (hi - lo)).clamp(lo, hi),
            None => f.from_frac((f.to_frac(v) + dir * 0.05).clamp(0.0, 1.0)),
        };
        self.set_num(f, next);
        if f.is_media() {
            self.dispatch_selected_media(doc);
        } else if f.is_track() {
            // The lane fader writes the lane, exactly as the drag commits it.
            // Dispatching a clip write here would report success and change
            // nothing — a keyboard and a pointer must not disagree about what
            // they did.
            self.dispatch_selected_track(doc);
        } else if let Some(id) = self.selected() {
            self.dispatch_clip(doc, id);
        }
        true
    }

    fn a11y_export_action(&mut self, action: &ui::a11y::Action, rest: &[u64]) -> bool {
        use ui::a11y::Action;
        let Some(&key) = rest.first() else {
            return false;
        };
        match (key, action) {
            // A disabled node that answers `true` tells an assistive
            // technology the action succeeded when nothing happened — the
            // node is correctly marked `disabled`, and this used to claim the
            // press worked anyway. The undo path above already refuses
            // properly; this now matches it.
            (1, Action::Activate) => {
                if self.export_is_offered() {
                    self.request_export();
                    true
                } else {
                    false
                }
            }
            // Closing is safe now that the render reports from the chip and
            // its own live region whether or not this dialog is up.
            (2, Action::Activate) => {
                self.presentation.export_open = false;
                true
            }
            (_, Action::Focus) => true,
            _ => false,
        }
    }
}

impl VideoEditorApp {
    /// The path a control's key hangs off: through its section header when it
    /// has one, and directly off the pane when it does not.
    ///
    /// Not every inspector control lives inside a section — the panel draws
    /// its level, grade, fade, transition and label controls **above** the
    /// first header — and the old lookup returned "no path" for those, which
    /// is one of the reasons pressing Enter on them did nothing.
    fn a11y_row_path(rows: &[InspectorRow], i: usize, key: u64) -> Vec<u64> {
        let section = rows
            .get(..=i)
            .unwrap_or(rows)
            .iter()
            .rev()
            .find_map(|r| match r {
                InspectorRow::Header(sec) => Some(*sec),
                _ => None,
            });
        match section {
            Some(sec) => vec![ui::a11y::hash_key(&sec.label()), key],
            None => vec![key],
        }
    }
}

/// Step an index within `0..n`, clamping at both ends rather than wrapping:
/// arrow navigation that wraps silently teleports a reader from the last row
/// to the first, which reads as the list having restarted.
fn step_index(i: usize, n: usize, up: bool) -> usize {
    if n == 0 {
        return 0;
    }
    if up {
        i.saturating_sub(1)
    } else {
        (i + 1).min(n - 1)
    }
}

/// An inspector control that is **chosen or switched** rather than dialled.
///
/// [`NumField`] covers the panel's ranged knobs; these are the rest of its
/// stops. They are here because the keyboard walk and the accessibility tree
/// are the same list, and that list held only headers and knobs — so every
/// picker and switch in the panel was reachable with a pointer and no other
/// way.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum PickField {
    /// One project frame-size preset, by index into [`FRAME_PRESETS`].
    Frame(usize),
    /// One project frame-rate preset, by index into [`RATE_PRESETS`].
    Rate(usize),
    /// Take the frame of the source `format_source` names.
    MatchFormat,
    /// The chroma key's enable switch. The catch-22 control: the key's three
    /// sliders are only laid out once it is on, so a user who could not reach
    /// the switch could never reach the sliders either — the section was
    /// pointer-only all the way down.
    KeyEnable,
    /// One mask shape button. Same catch-22: the mask's geometry rows appear
    /// only once a shape is chosen.
    MaskShape(crate::model::MaskShape),
    /// One transition at the clip's head cut.
    Transition(Transition),
    /// One label colour, by palette slot.
    Label(u8),
    /// The link / unlink button. Which of the two it is depends on the
    /// selection — see [`LinkAffordance`] — so the row is one stop whichever
    /// way round it reads.
    LinkToggle,
    /// Put the selected clip's lane back to unity and centre. Laid out under
    /// the fader and the pan, and offered only when one of them has been
    /// moved — the same condition the panel draws it on.
    ///
    /// It is a stop for the reason the key switch and the mask shapes are: the
    /// two knobs above it were on the walk from the day they were added and
    /// the button that undoes both of them was not, so a keyboard could set a
    /// lane and then had no way back to unity that did not go through two
    /// sliders.
    MixReset,
}

impl PickField {
    /// A stable tag, which is what the accessible key hashes. Stable across a
    /// re-layout and across a project edit, so an assistive technology holding
    /// a node keeps holding the same control.
    fn tag(self) -> String {
        match self {
            PickField::Frame(i) => format!(
                "project.frame.{}",
                FRAME_PRESETS.get(i).map_or("?", |(name, _, _)| *name)
            ),
            PickField::Rate(i) => format!(
                "project.rate.{}",
                RATE_PRESETS
                    .get(i)
                    .map_or_else(String::new, |r| rate_words(*r))
            ),
            PickField::MatchFormat => "project.match".to_string(),
            PickField::KeyEnable => "clip.key.enable".to_string(),
            PickField::MaskShape(shape) => format!("clip.mask.{}", shape.as_str()),
            PickField::Transition(t) => format!("clip.transition.{}", t.as_str()),
            PickField::Label(slot) => format!("clip.label.{slot}"),
            PickField::LinkToggle => "clip.link".to_string(),
            PickField::MixReset => "track.mix.reset".to_string(),
        }
    }

    fn a11y_key(self) -> u64 {
        ui::a11y::hash_key(&self.tag())
    }
}

/// Which way round the inspector's link control reads for the current
/// selection.
///
/// The two are never both available: a selection that already holds a linked
/// clip offers the break, and one that does not offers the tie. Keeping it a
/// single control rather than two rows means the row's position never moves
/// under the cursor, and the keyboard walk has one stop instead of two that
/// swap places.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum LinkAffordance {
    /// Break the link on the selection's clips.
    Unlink,
    /// Tie the selected clips into one group.
    Link,
    /// Give a picture clip that carries sound its own audio clip. This is what
    /// a project saved before linked audio existed is offered, and what a shot
    /// whose sound half was deleted is offered again — so neither is a
    /// one-way door.
    SplitAudio,
}

impl LinkAffordance {
    /// What the control says it will do. The button names the *change*, not
    /// the state; the caption above it names the state.
    fn label(self) -> &'static str {
        match self {
            LinkAffordance::Unlink => "Unlink audio and video",
            LinkAffordance::Link => "Link audio and video",
            LinkAffordance::SplitAudio => "Split off audio",
        }
    }

    /// What is true now, above the button.
    fn state(self) -> &'static str {
        match self {
            LinkAffordance::Unlink => "Linked audio and video",
            LinkAffordance::Link => "Not linked",
            // True whether the sound has never left the picture or the clip it
            // left for is gone; the editor knows which of those it is, and
            // neither changes what the button does.
            LinkAffordance::SplitAudio => "No linked audio clip",
        }
    }
}

/// A focus stop in the inspector: a section header, or one control inside an
/// open section.
#[derive(Clone, Copy, PartialEq, Debug)]
enum InspectorRow {
    Header(Section),
    Field(NumField),
    /// One picker segment or switch — see [`PickField`].
    Pick(PickField),
    /// One row of the marker list. A marker is not a numeric knob, so it is
    /// its own stop: a reader that could only ever land on knobs could not
    /// reach a marker at all.
    Marker(MarkerRef),
    /// The button that closes the marker list.
    AddMarker,
}

// ==========================================================================
// Event handling helpers
// ==========================================================================
impl VideoEditorApp {
    fn handle_key(
        &mut self,
        key: &Key,
        modifiers: &winit::event::Modifiers,
        state: &mut AutoCommit,
        r: &Regions,
    ) -> bool {
        let ctrl = modifiers.state().control_key() || modifiers.state().super_key();
        let shift = modifiers.state().shift_key();
        // The unsaved-work question owns the keyboard: Esc stays put, Enter
        // takes the safe answer. Nothing else fires meanwhile, so a stray key
        // cannot edit the project that is being asked about.
        if self.pending_open.is_some() {
            match key {
                Key::Named(NamedKey::Escape) => {
                    self.resolve_pending_open(state, DiscardChoice::Cancel);
                    return true;
                }
                Key::Named(NamedKey::Enter) => {
                    self.resolve_pending_open(state, DiscardChoice::Save);
                    return true;
                }
                _ => return false,
            }
        }
        if self.presentation.export_open {
            if matches!(key, Key::Named(NamedKey::Escape)) {
                // Esc closes the dialog, not the render: the chip under the
                // Export button carries the job on.
                self.presentation.export_open = false;
                return true;
            }
            if matches!(key, Key::Named(NamedKey::Enter)) {
                if self.export_is_offered() {
                    self.request_export();
                }
                return true;
            }
            return false;
        }
        // A timecode being typed owns the keyboard: every key either edits it,
        // commits it, or abandons it. Nothing else fires meanwhile, so typing
        // "1:30" cannot also split a clip and add a marker on the way.
        if let Some(entry) = self.goto_entry.as_mut() {
            match key {
                Key::Named(NamedKey::Escape) => {
                    self.goto_entry = None;
                }
                Key::Named(NamedKey::Backspace) => {
                    entry.pop();
                }
                Key::Named(NamedKey::Enter) => {
                    let typed = entry.clone();
                    match crate::timecode::parse(&typed, self.project.fps) {
                        Some(t) => {
                            self.playhead = t.clamp(0.0, self.project.duration().max(t));
                            self.playback.playing = false;
                            self.shuttle = 0.0;
                            self.playback.chase_playhead = true;
                            self.goto_entry = None;
                        }
                        // Keep the field open on a value that cannot be read,
                        // rather than closing it and silently going nowhere.
                        None => self.navigation.goto_notice = true,
                    }
                }
                Key::Character(c)
                    if entry.len() < 12 && c.chars().all(|ch| ch.is_ascii_digit() || ch == ':') =>
                {
                    entry.push_str(c);
                    self.navigation.goto_notice = false;
                }
                _ => {}
            }
            return true;
        }
        if self.bin_overlay_is_open() {
            // The narrow bin paints a full-window scrim, so it owns keyboard
            // input as well as pointer input. Keep the one useful pane walk
            // inside the bin and give both Escape and B the visible toggle's
            // close behaviour; no timeline edit may happen behind the veil.
            return match key {
                Key::Named(NamedKey::Escape) => {
                    self.close_bin_overlay();
                    true
                }
                Key::Named(NamedKey::Tab) => {
                    self.focus.pane = Pane::Bin;
                    self.clamp_pane_cursor();
                    true
                }
                Key::Named(NamedKey::ArrowUp) => self.nav_arrow(true, true, state),
                Key::Named(NamedKey::ArrowDown) => self.nav_arrow(false, true, state),
                Key::Named(NamedKey::Enter) => self.activate_focused(state),
                Key::Character(c) if c.eq_ignore_ascii_case("b") => {
                    self.close_bin_overlay();
                    true
                }
                _ => false,
            };
        }
        match key {
            Key::Named(NamedKey::Space) => {
                self.toggle_play();
                true
            }
            Key::Named(NamedKey::PageUp) if self.focus.pane == Pane::Timeline => {
                self.scroll_timeline_page(false, r)
            }
            Key::Named(NamedKey::PageDown) if self.focus.pane == Pane::Timeline => {
                self.scroll_timeline_page(true, r)
            }
            // Arrows nudge every selected clip a frame (Shift = one second);
            // with nothing selected they step the playhead instead.
            Key::Named(NamedKey::ArrowLeft) if self.focus.pane == Pane::Transport => {
                self.nav_arrow(true, false, state)
            }
            Key::Named(NamedKey::ArrowRight) if self.focus.pane == Pane::Transport => {
                self.nav_arrow(false, false, state)
            }
            Key::Named(NamedKey::ArrowLeft) => {
                let step = if shift {
                    1.0
                } else {
                    frame_step(self.project.fps)
                };
                if !self.selection.is_empty() {
                    self.nudge_selected(state, -step);
                } else {
                    self.step_frame(if shift { -self.project.fps } else { -1.0 });
                }
                true
            }
            Key::Named(NamedKey::ArrowRight) => {
                let step = if shift {
                    1.0
                } else {
                    frame_step(self.project.fps)
                };
                if !self.selection.is_empty() {
                    self.nudge_selected(state, step);
                } else {
                    self.step_frame(if shift { self.project.fps } else { 1.0 });
                }
                true
            }
            Key::Named(NamedKey::Home) => {
                self.playhead = 0.0;
                self.playback.playing = false;
                self.shuttle = 0.0;
                self.playback.chase_playhead = true;
                true
            }
            Key::Named(NamedKey::End) => {
                self.playhead = self.project.duration();
                self.playback.playing = false;
                self.shuttle = 0.0;
                self.playback.chase_playhead = true;
                true
            }
            // Shift+Delete ripples (closes the gap); plain Delete lifts.
            Key::Named(NamedKey::Delete | NamedKey::Backspace) => {
                if shift {
                    self.ripple_delete_selected(state);
                } else {
                    self.delete_selected(state);
                }
                true
            }
            Key::Character(c) if ctrl && c.eq_ignore_ascii_case("e") => {
                self.presentation.export_open = true;
                let _ = r;
                true
            }
            // Ctrl+O opens a project. It used to import media, from before the
            // editor had projects to open — and Ctrl+O means "open a document"
            // in every app a person has ever used, so leaving it on import
            // would make the one shortcut everybody already knows do the one
            // thing they did not ask for. Import keeps the same letter, one
            // modifier along, and both are in the shortcut sheet.
            Key::Character(c) if ctrl && shift && c.eq_ignore_ascii_case("o") => {
                self.trigger_import();
                true
            }
            Key::Character(c) if ctrl && c.eq_ignore_ascii_case("o") => {
                self.request_open_project();
                true
            }
            Key::Character(c) if ctrl && shift && c.eq_ignore_ascii_case("s") => {
                self.request_save_as();
                true
            }
            Key::Character(c) if ctrl && c.eq_ignore_ascii_case("s") => {
                self.queue_save_project();
                true
            }
            Key::Character(c) if ctrl && c.eq_ignore_ascii_case("c") => {
                self.copy_selected();
                true
            }
            Key::Character(c) if ctrl && c.eq_ignore_ascii_case("x") => {
                self.cut_selected(state);
                true
            }
            Key::Character(c) if ctrl && c.eq_ignore_ascii_case("v") => {
                self.paste_clip(state);
                true
            }
            // Ctrl+D drops a copy of the selection immediately after it.
            Key::Character(c) if ctrl && c.eq_ignore_ascii_case("d") => {
                self.duplicate_selected(state);
                true
            }
            // Ctrl+A selects every clip on the timeline.
            Key::Character(c) if ctrl && c.eq_ignore_ascii_case("a") => {
                self.select_all();
                true
            }
            // J / K / L shuttle: J reverse, K stop, L forward. Repeated J/L
            // doubles the rate, the standard NLE shuttle.
            Key::Character(c) if c.eq_ignore_ascii_case("j") && !ctrl => {
                self.shuttle_key(-1.0);
                true
            }
            Key::Character(c) if c.eq_ignore_ascii_case("k") && !ctrl => {
                self.shuttle = 0.0;
                self.playback.playing = false;
                true
            }
            Key::Character(c) if c.eq_ignore_ascii_case("l") && !ctrl => {
                self.shuttle_key(1.0);
                true
            }
            // Shift+I / Shift+O mark the timeline's render range; Shift+X
            // drops both marks. Plain I / O keep trimming the selected clip,
            // which is what they have always done and what every NLE does
            // with them — the range is a property of the timeline, the trim
            // one of a clip, and they are not the same decision.
            Key::Character(c) if shift && !ctrl && c.eq_ignore_ascii_case("i") => {
                self.mark_in_here(state);
                true
            }
            Key::Character(c) if shift && !ctrl && c.eq_ignore_ascii_case("o") => {
                self.mark_out_here(state);
                true
            }
            Key::Character(c) if shift && !ctrl && c.eq_ignore_ascii_case("x") => {
                self.clear_marks(state);
                true
            }
            // I / O set the selected clip's in / out point to the playhead.
            Key::Character(c) if c.eq_ignore_ascii_case("i") && !ctrl => {
                self.set_in_at_playhead(state);
                true
            }
            Key::Character(c) if c.eq_ignore_ascii_case("o") && !ctrl => {
                self.set_out_at_playhead(state);
                true
            }
            Key::Character(c) if c.eq_ignore_ascii_case("s") && !ctrl => {
                self.split_selected(state);
                true
            }
            // Toggle the media bin flap. Useful on narrow windows where the
            // bin is an overlay; harmless (collapses the docked bin) on wide.
            Key::Character(c) if c.eq_ignore_ascii_case("b") && !ctrl => {
                self.toggle_bin();
                true
            }
            // M drops a marker at the playhead and opens it for naming — an
            // unnamed tick is a marker you cannot find again.
            Key::Character(c) if c.eq_ignore_ascii_case("m") && !ctrl => {
                self.add_marker_here(state);
                true
            }
            // T makes a title, places it and puts the caret in its words.
            Key::Character(c) if c.eq_ignore_ascii_case("t") && !ctrl => {
                self.add_title(state);
                true
            }
            // , and . step between marked instants, the project's own and
            // every clip's alike.
            Key::Character(c) if (c.as_str() == "," || c.as_str() == "<") && !ctrl => {
                self.jump_to_marker(false);
                true
            }
            Key::Character(c) if (c.as_str() == "." || c.as_str() == ">") && !ctrl => {
                self.jump_to_marker(true);
                true
            }
            // C walks the places the mix went over, the same walk the red CLIP
            // chip does under the pointer. Without it the *only* route to the
            // answer was clicking a 60-pixel chip: the warning told a keyboard
            // user their mix was ruined and then gave them a whole timeline to
            // scrub. Returns false when nothing has clipped, so the key falls
            // through as unhandled rather than reporting a jump it did not
            // make.
            Key::Character(c) if c.eq_ignore_ascii_case("c") && !ctrl => self.go_to_next_clipping(),
            // Shift+Z fits the whole project to the window, the NLE standard.
            Key::Character(c) if c.eq_ignore_ascii_case("z") && !ctrl => {
                self.zoom_to_fit(r);
                true
            }
            Key::Character(c) if c.as_str() == "+" || c.as_str() == "=" => {
                self.zoom_by(1.25);
                true
            }
            Key::Character(c) if c.as_str() == "-" || c.as_str() == "_" => {
                self.zoom_by(1.0 / 1.25);
                true
            }
            // Tab moves between panes and the arrows move within the focused
            // one. Tab used to toggle the inspector, but an editor whose only
            // Tab stop is the whole application is one a keyboard-only user
            // cannot navigate at all; the inspector toggle moved to Ctrl+I,
            // which the shortcut sheet states.
            Key::Named(NamedKey::Tab) => {
                self.focus_pane(shift);
                true
            }
            Key::Named(NamedKey::ArrowUp) => self.nav_arrow(true, true, state),
            Key::Named(NamedKey::ArrowDown) => self.nav_arrow(false, true, state),
            Key::Named(NamedKey::Enter) => self.activate_focused(state),
            Key::Character(c) if ctrl && c.eq_ignore_ascii_case("i") => {
                self.toggle_inspector();
                true
            }
            // The shortcut sheet. Fifteen shortcuts existed and none of them
            // were written down anywhere in the product.
            Key::Character(c) if c.as_str() == "?" || (shift && c.as_str() == "/") => {
                self.navigation.help_open = !self.navigation.help_open;
                true
            }
            // G opens a timecode field: type a time, press Enter, land there.
            Key::Character(c) if c.eq_ignore_ascii_case("g") && !ctrl => {
                self.goto_entry = Some(String::new());
                true
            }
            Key::Named(NamedKey::Escape) => {
                // Esc dismisses an open overlay bin first, then an open
                // overlay inspector, then clears the selection — matching the
                // flap kernel's Esc-to-close.
                if self.navigation.help_open {
                    self.navigation.help_open = false;
                    return true;
                }
                if self.bin_layout() == SplitLayout::Overlay
                    && self.bin_flap.is_open(SplitLayout::Overlay)
                {
                    self.close_bin_overlay();
                } else if self.inspector_split() == SplitLayout::Overlay
                    && self.inspector_flap.is_open(SplitLayout::Overlay)
                {
                    self.inspector_flap.close();
                } else {
                    self.selection.clear();
                }
                true
            }
            _ => false,
        }
    }

    /// The go-to-timecode card, for the painter and the hit test alike.
    ///
    /// Shared rather than derived twice: a click has to land on the card that
    /// was actually drawn, or dismiss-on-click-away fires while the pointer
    /// is over the field.
    fn goto_layout(&self, r: &Regions) -> Rect {
        let w = 260.0_f64.min(self.rect.width() - 32.0);
        Rect::from_center_size(
            Point::new(self.rect.center().x, r.transport.y0 - 46.0),
            (w, 76.0),
        )
    }

    /// Whether a full-window scrim is up — the editor behind it is
    /// unavailable, and every click belongs to the surface on top.
    fn is_modal(&self) -> bool {
        self.pending_open.is_some()
            || self.presentation.export_open
            || self.navigation.help_open
            || self.goto_entry.is_some()
    }

    fn handle_press(&mut self, state: &mut AutoCommit, r: &Regions, shift: bool) {
        let p = self.last_mouse;

        // Modals own all clicks. The unsaved-work question is asked over the
        // export dialog too (Ctrl+O works while it is up), so it is first.
        if self.pending_open.is_some() {
            let (_, cancel, discard, save) = self.discard_layout();
            if cancel.contains(p) {
                self.resolve_pending_open(state, DiscardChoice::Cancel);
            } else if discard.contains(p) {
                self.resolve_pending_open(state, DiscardChoice::Discard);
            } else if save.contains(p) {
                self.resolve_pending_open(state, DiscardChoice::Save);
            }
            return;
        }
        if self.presentation.export_open {
            self.press_export(state, r, p);
            return;
        }
        // The shortcut sheet and the go-to-timecode field paint the same
        // full-window scrim as the dialogs above, so they swallow clicks the
        // same way. Before this, a click "on" the dimmed background reached
        // the clip, slider or Export button underneath — a real edit, made
        // invisibly, through a surface that said the editor was unavailable.
        // Clicking away dismisses, which is what a scrim invites.
        if self.navigation.help_open {
            self.navigation.help_open = false;
            return;
        }
        if self.goto_entry.is_some() {
            if !self.goto_layout(r).contains(p) {
                self.goto_entry = None;
            }
            return;
        }
        // Scrollbars float over their panes, so they are hit-tested before any
        // content underneath them.
        if self.press_scrollbars(r, p) {
            return;
        }
        // The export chip floats over whatever pane is under it, so it is hit
        // first. It is the way back into the render it is reporting: the full
        // path, the failure's own words and Cancel all live in the dialog.
        if self.export_chip_is_shown() && r.export_chip.contains(p) {
            self.presentation.export_open = true;
            return;
        }
        // Hamburger toggle (overlay layout only).
        if r.bin_overlay && r.menu_btn.contains(p) {
            self.toggle_bin();
            return;
        }
        // An open bin overlay owns every click outside its flap. This check
        // must precede the other top-bar controls: otherwise the scrim could
        // look modal while Import, Export, or the inspector still changed
        // behind it.
        let bin_visible = self.bin_is_visible(r);
        if r.bin_overlay && bin_visible && !r.bin.contains(p) && !r.menu_btn.contains(p) {
            self.close_bin_overlay();
            return;
        }
        // Inspector toggle (overlay layout only).
        if r.inspector_overlay && r.inspector_btn.contains(p) {
            self.toggle_inspector();
            return;
        }
        // An open inspector overlay owns its own rect, before the preview and
        // timeline underneath it get a look.
        if r.inspector_overlay && self.inspector_is_visible(r) && r.inspector.contains(p) {
            self.press_inspector(state, r, p);
            return;
        }
        // Top bar buttons.
        if r.project_btn.width() > 0.0 && r.project_btn.contains(p) {
            if self.project_menu.is_open() {
                self.project_menu.close();
            } else {
                self.open_project_menu(Point::new(r.project_btn.x0, r.project_btn.y1 + 4.0));
            }
            return;
        }
        if r.export_btn.contains(p) {
            self.presentation.export_open = true;
            return;
        }
        if r.import_btn.contains(p) {
            self.trigger_import();
            return;
        }
        if r.split_btn.contains(p) {
            self.split_selected(state);
            return;
        }
        // Empty-bin "Import media" affordance.
        if let Some(btn) = self.empty_bin_import_rect(r)
            && btn.contains(p)
        {
            self.trigger_import();
            return;
        }
        // "+ Title" in the bin header.
        if self.bin_is_visible(r) && self.add_title_rect(r).contains(p) {
            self.add_title(state);
            return;
        }
        // Media bin (only when it's on-screen: docked, or overlay open).
        if bin_visible {
            for (mid, rect) in self.bin_item_rects(r) {
                if rect.contains(p) {
                    self.selected_media = Some(mid);
                    // How long ago this same item was last *clicked* (not
                    // programmatically selected) — `None` for a different item
                    // or a fresh one. Both the double-click and the rename are
                    // gated on it, so a freshly-imported, auto-selected item is
                    // never renamed on the first click.
                    let since = self
                        .last_click_media
                        .and_then(|(m, t)| (m == mid).then(|| t.elapsed().as_millis()));
                    let dbl = matches!(since, Some(ms) if ms < BIN_DOUBLE_CLICK_MS);
                    // The rename window is *bounded at both ends*: fast enough
                    // and it is a double-click, slow enough and the earlier
                    // click is stale and this is simply a new press. Without
                    // the upper bound an item, once clicked, could never be
                    // dragged to the timeline again — the name band covers
                    // most of the row, which is exactly where you grab it.
                    let rename_window = matches!(
                        since,
                        Some(ms) if (BIN_DOUBLE_CLICK_MS..BIN_RENAME_WINDOW_MS).contains(&ms)
                    );
                    if dbl {
                        self.add_media_to_timeline(state, mid);
                        self.last_click_media = None;
                        self.pending_rename = None;
                    } else {
                        // Always arm a potential bin→timeline drag. When the
                        // press also falls in the rename window, the rename is
                        // only an *intent*: it fires on release if the pointer
                        // never moved, and is cancelled the moment the press
                        // becomes a drag (see `update_bin_drag`). A drag
                        // therefore always wins over a rename.
                        self.bin_drag = Some(BinDrag {
                            media: mid,
                            origin: p,
                            active: false,
                        });
                        self.pending_rename =
                            (rename_window && self.bin_name_rect(rect).contains(p)).then_some(mid);
                        self.last_click_media = Some((mid, Instant::now()));
                    }
                    return;
                }
            }
        }
        // Inspector controls.
        if self.inspector_is_visible(r) && r.inspector.contains(p) {
            self.press_inspector(state, r, p);
            return;
        }
        // Transport controls.
        for (action, rect) in self.transport_buttons(r) {
            if rect.contains(p) {
                self.transport_action(action);
                return;
            }
        }
        // History. `request` refuses when the verb is unavailable, which is
        // the same test the drawing makes — there is no second availability
        // rule here to drift from the first.
        for (which, rect) in self.history_buttons(r) {
            if rect.contains(p) {
                self.undo.request(which);
                return;
            }
        }
        let scrub = self.scrubber_rect(r);
        if self.has_scrubber(r)
            && (scrub.contains(p)
                || (p.x >= scrub.x0 && p.x <= scrub.x1 && (p.y - scrub.center().y).abs() < 14.0))
        {
            self.drag = Some(Drag::Scrubber);
            self.apply_scrubber(p, r);
            return;
        }
        // The standing clipping warning, in the master strip. Pressing it
        // walks the playhead through the places the mix went over, wrapping
        // round — which is the difference between a warning you can act on
        // and one you can only be annoyed by.
        if self.mix.clipped() && self.clip_flag_rect(r).contains(p) {
            self.go_to_next_clipping();
            return;
        }
        // Zoom readout / fit-to-window. **Before** the ruler scrub, because
        // it now sits at the ruler's right end: a chip inside a region that
        // scrubs has to be claimed before the region is, or pressing it just
        // moves the playhead to the far right.
        if self.zoom_chip_rect(r).contains(p) {
            self.zoom_to_fit(r);
            return;
        }
        // Ruler scrub.
        if r.ruler.contains(p) {
            self.drag = Some(Drag::Playhead);
            self.playhead = snap_to_frame(self.x_to_time(p.x, &r.lanes), self.project.fps)
                .clamp(0.0, self.project.duration());
            return;
        }
        // Timeline gutter: add-track buttons and per-track mute pills.
        if r.gutter.contains(p) {
            for (kind, rect) in self.add_track_buttons(r) {
                if rect.contains(p) {
                    self.add_track(state, kind);
                    return;
                }
            }
            for (tid, _kind, lane) in self.lane_rects(r) {
                let header = Rect::new(r.gutter.x0, lane.y0, r.gutter.x1, lane.y1);
                if header.contains(p) {
                    if self.mute_pill_rect(header).contains(p) {
                        self.toggle_track_mute(state, tid);
                    } else if self.lock_pill_rect(header).contains(p) {
                        self.toggle_track_lock(state, tid);
                    } else if self.solo_pill_rect(header).contains(p) {
                        self.toggle_track_solo(state, tid);
                    }
                    return;
                }
            }
            return;
        }
        // Timeline lanes — clips, trim handles, rubber-band / scrub.
        if r.lanes.contains(p) {
            self.press_timeline(r, p, shift);
        }
    }

    /// Give every visible scrollbar first refusal on a press. Bars float over
    /// their pane's content, so they must be hit-tested before it. Returns
    /// `true` when one took the press.
    fn press_scrollbars(&mut self, r: &Regions, p: Point) -> bool {
        for pane in Self::SCROLL_PANES {
            let Some((track, m)) = self.scroll_pane(pane, r) else {
                continue;
            };
            match self.scroll_bar_mut(pane).press(track, m, p) {
                ui::widget::PressOutcome::Thumb => {
                    self.drag = Some(Drag::Scrollbar(pane));
                    return true;
                }
                ui::widget::PressOutcome::Page(target) => {
                    self.set_scroll_offset(pane, target, r);
                    return true;
                }
                ui::widget::PressOutcome::Miss => {}
            }
        }
        false
    }

    /// Finish a rubber-band drag. If the band ever opened, select every clip
    /// it touches (extending the previous selection when `additive`).
    /// Otherwise the press was a plain click on empty space: clear the
    /// selection and scrub the playhead there, the behaviour a bare click on
    /// the lanes has always had.
    fn release_marquee(&mut self, origin: Point, additive: bool, r: &Regions) {
        let Some(band) = self.marquee else {
            if !additive {
                self.selection.clear();
            }
            self.playhead = snap_to_frame(self.x_to_time(origin.x, &r.lanes), self.project.fps)
                .clamp(0.0, self.project.duration());
            return;
        };
        let mut hits: Vec<ClipId> = if additive {
            self.selection.ids().to_vec()
        } else {
            Vec::new()
        };
        for (tid, _kind, lane) in self.lane_rects(r) {
            if lane.y1 < band.y0 || lane.y0 > band.y1 {
                continue;
            }
            for c in self.project.clips_on(tid) {
                let cr = self.clip_rect(c, &lane, r);
                // Any horizontal overlap with the band counts — the lane test
                // above already established the vertical one.
                if cr.x1 >= band.x0 && cr.x0 <= band.x1 && !hits.contains(&c.id) {
                    hits.push(c.id);
                }
            }
        }
        self.selection.set(hits);
    }

    /// The clip under `p`, as `(clip id, its lane rect)`. `None` over empty
    /// lane space.
    fn clip_at(&self, r: &Regions, p: Point) -> Option<(ClipId, Rect)> {
        for (tid, _kind, lane) in self.lane_rects(r) {
            if p.y < lane.y0 || p.y > lane.y1 {
                continue;
            }
            for c in self.project.clips_on(tid) {
                let cr = self.clip_rect(c, &lane, r);
                if p.x >= cr.x0 - 2.0 && p.x <= cr.x1 + 2.0 {
                    return Some((c.id, lane));
                }
            }
        }
        None
    }

    /// Press inside the lane area. On a clip: select (shift extends), then arm
    /// a trim or a move — unless its track is locked, in which case the clip
    /// still selects (so the inspector can show it) but no edit drag begins.
    /// On empty space: arm a rubber-band selection, which falls back to a
    /// playhead scrub if the press never moves.
    fn press_timeline(&mut self, r: &Regions, p: Point, shift: bool) {
        if let Some((cid, lane)) = self.clip_at(r, p) {
            if shift {
                self.selection.toggle(cid);
            } else if !self.selection.contains(cid) {
                // Clicking a clip that is already part of a multi-selection
                // keeps that selection, so the whole block can be dragged.
                self.selection.select_only(cid);
            } else {
                self.selection.set_primary(cid);
            }
            // A narrow window keeps the inspector out of the way until there
            // is something to inspect; picking a clip is exactly that moment.
            self.reveal_inspector();
            let Some(clip) = self.project.clip(cid).cloned() else {
                return;
            };
            let cr = self.clip_rect(&clip, &lane, r);
            // A key diamond on the clip's keyframe lane takes the playhead to
            // that key. Checked before the trim handles and the move drag,
            // because it is the smaller, more specific target inside the same
            // body.
            if !shift
                && p.y >= cr.y1 - 13.0
                && let Some(t) = clip
                    .key_times()
                    .into_iter()
                    .find(|t| (self.time_to_x(*t, &r.lanes) - p.x).abs() <= 5.0)
            {
                self.playhead = t.clamp(0.0, self.project.duration());
                self.playback.playing = false;
                self.shuttle = 0.0;
                return;
            }
            if !self.project.clip_editable(cid) || shift {
                // A locked clip is inspectable but not draggable; a shift-click
                // is a selection gesture, not the start of a move.
                return;
            }
            if (p.x - cr.x0).abs() <= TRIM_HIT {
                self.drag = Some(Drag::TrimIn { id: cid });
            } else if (p.x - cr.x1).abs() <= TRIM_HIT {
                self.drag = Some(Drag::TrimOut { id: cid });
            } else {
                self.drag = Some(Drag::ClipMove {
                    id: cid,
                    grab_dt: self.x_to_time(p.x, &r.lanes) - clip.start,
                    track: clip.track,
                });
            }
            return;
        }
        // Empty lane area: a rubber band, resolved on release. A press that
        // never travels past MARQUEE_THRESHOLD scrubs the playhead instead,
        // so click-to-scrub still works exactly as it did.
        self.drag = Some(Drag::Marquee {
            origin: p,
            additive: shift,
        });
        self.marquee = None;
    }

    #[allow(
        clippy::too_many_lines,
        reason = "the inspector hit test deliberately mirrors its ordered, scroll-clipped control layout"
    )]
    fn press_inspector(&mut self, state: &mut AutoCommit, r: &Regions, p: Point) {
        // The fixed identity card is context, not a hidden control surface.
        // Do not let a translated row behind it receive a click.
        if !self.inspector_viewport(r).contains(p) {
            return;
        }
        let l = self.inspector_layout(r);
        // The markers section and the section folds are project-level, so
        // they answer to a press whether or not a clip is selected. Every
        // control below this point edits the selection and needs one.
        for (s, rect) in &l.sections {
            if rect.contains(p) {
                if !self.collapsed.remove(s) {
                    self.collapsed.insert(*s);
                }
                return;
            }
        }
        for (m, row) in l.marker_rows.clone() {
            if !row.contains(p) {
                continue;
            }
            let (_, note_btn, rename, del) = marker_row_parts(row);
            if del.contains(p) {
                self.delete_marker(state, m);
            } else if note_btn.contains(p) {
                self.begin_marker_edit(m, MarkerField::Note);
            } else if rename.contains(p) {
                self.begin_marker_rename(m);
            } else {
                self.seek_to_marker(m);
            }
            return;
        }
        if let Some(rect) = l.add_marker {
            if rect.contains(p) {
                self.add_marker_here(state);
                return;
            }
        }
        // The project's format is project-level too, so it answers whether or
        // not a clip is selected -- above the `l.valid` gate, which is what
        // marks "a clip's controls were laid out".
        for ((w, h), rect) in l.project_frames.clone() {
            if rect.contains(p) {
                self.set_project_frame_size(state, w, h);
                return;
            }
        }
        for (rate, rect) in l.project_rates.clone() {
            if rect.contains(p) {
                self.set_project_fps(state, rate);
                return;
            }
        }
        if let Some(rect) = l.project_match
            && rect.contains(p)
        {
            self.match_project_format(state);
            return;
        }
        if !l.valid {
            return;
        }
        // The slider controls start a drag and preview the click position in
        // memory; the final value is dispatched as one action on mouse-up
        // (see `commit_drag`), so a slider interaction is one undo step.
        for (track, param, drag) in [
            (l.opacity, Param::Opacity, Drag::Opacity),
            (l.volume, Param::Volume, Drag::Volume),
            (l.brightness, Param::Brightness, Drag::Brightness),
            (l.contrast, Param::Contrast, Drag::Contrast),
            (l.saturation, Param::Saturation, Drag::Saturation),
        ] {
            if let Some(track) = track {
                if hit_slider(track, p) {
                    self.drag = Some(drag);
                    let v = ((p.x - track.x0) / track.width()).clamp(0.0, 1.0);
                    self.preview_param(param, v);
                    return;
                }
            }
        }
        // Fade in / out sliders (value in seconds, 0..clip-duration).
        let max = self
            .selected()
            .and_then(|id| self.project.clip(id))
            .map(|c| c.duration())
            .unwrap_or(0.0);
        if let Some(track) = l.fade_in {
            if hit_slider(track, p) {
                self.drag = Some(Drag::FadeIn);
                let frac = ((p.x - track.x0) / track.width()).clamp(0.0, 1.0);
                self.preview_edit(move |c| c.fade_in = (frac * max).clamp(0.0, c.duration()));
                return;
            }
        }
        if let Some(track) = l.fade_out
            && hit_slider(track, p)
        {
            self.drag = Some(Drag::FadeOut);
            let frac = ((p.x - track.x0) / track.width()).clamp(0.0, 1.0);
            self.preview_edit(move |c| c.fade_out = (frac * max).clamp(0.0, c.duration()));
            return;
        }
        if let Some(track) = l.transition_len {
            if hit_slider(track, p) {
                self.drag = Some(Drag::TransitionLen);
                let frac = ((p.x - track.x0) / track.width()).clamp(0.0, 1.0);
                let max = self.transition_len_max();
                self.preview_edit(move |c| c.transition_duration = (frac * max).max(0.01));
                return;
            }
        }
        for (t, rect) in &l.transitions {
            if rect.contains(p) {
                self.set_transition(state, *t);
                return;
            }
        }
        if let Some(rect) = l.link
            && rect.contains(p)
        {
            self.toggle_link_selected(state);
            return;
        }
        for (pal, rect) in &l.colors {
            if rect.contains(p) {
                self.set_palette(state, *pal);
                return;
            }
        }
        // Effect rows: reorder / intensity slider / remove.
        for (i, row) in l.effect_rows.iter().enumerate() {
            let (_key, up, down, slider, remove) = Self::effect_row_parts(*row);
            if up.contains(p) {
                self.move_effect_selected(state, i, true);
                return;
            }
            if down.contains(p) {
                self.move_effect_selected(state, i, false);
                return;
            }
            if remove.contains(p) {
                self.remove_effect_from_selected(state, i);
                return;
            }
            if hit_slider(slider, p) {
                let Some(effect) = self
                    .selected()
                    .and_then(|id| self.project.clip(id))
                    .and_then(|clip| clip.effects.get(i))
                    .copied()
                else {
                    return;
                };
                self.drag = Some(Drag::EffectAmount {
                    idx: i,
                    effect: effect.id,
                    expected_amount_bits: effect.amount.to_bits(),
                });
                let v = ((p.x - slider.x0) / slider.width()).clamp(0.0, 1.0);
                self.preview_edit(move |c| {
                    if let Some(e) = c.effects.get_mut(i) {
                        e.amount = v;
                    }
                });
                return;
            }
        }
        if l.add_effect.width() > 0.0 && l.add_effect.contains(p) {
            self.open_effect_menu(p);
            return;
        }
        // Tone curves: the channel chooser, then the editor's box.
        for (ch, rect) in l.curve_channels.clone() {
            if rect.contains(p) {
                self.curve_channel = ch;
                self.collapsed.remove(&Section::Curves);
                return;
            }
        }
        if let Some(canvas) = l.curve_canvas {
            if canvas.contains(p) {
                self.press_curve(state, canvas, p);
                return;
            }
        }
        if let Some(rect) = l.curve_reset {
            if rect.contains(p) {
                let ch = self.curve_channel;
                self.edit_selected(state, move |c| {
                    *c.curves.spline_mut(ch) = crate::curve::Spline::identity();
                });
                return;
            }
        }
        // Key controls. An unkeyed parameter takes one click to start
        // animating; a keyed one opens the menu that shapes, removes and
        // steps between its keys, because "what do I do with this key" has
        // more than one answer and a second click cannot express them all.
        for (param, rect) in &l.keys {
            if rect.contains(p) {
                if self.key_state(*param) == KeyState::Off {
                    self.key_here(state, *param);
                } else {
                    self.open_key_menu(*param, p);
                }
                return;
            }
        }
        // Ranged controls — geometry, rate, title size.
        for (f, track) in l.nums.clone() {
            if hit_slider(track, p) {
                self.drag = Some(Drag::Num(f));
                let frac = ((p.x - track.x0) / track.width()).clamp(0.0, 1.0);
                self.set_num(f, f.from_frac(frac));
                return;
            }
        }
        // Picture-in-picture presets, and the way back to filling the frame.
        for (_, corner, rect) in l.pip.clone() {
            if rect.contains(p) {
                self.edit_selected(state, move |c| {
                    c.transform = crate::model::Transform::pip(PIP_SCALE, corner, PIP_INSET);
                });
                return;
            }
        }
        for (anchor, rect) in l.anchor.clone() {
            if rect.contains(p) {
                self.edit_selected(state, move |c| {
                    c.transform.anchor_x = anchor.0;
                    c.transform.anchor_y = anchor.1;
                });
                return;
            }
        }
        if let Some(rect) = l.reset_transform {
            if rect.contains(p) {
                self.edit_selected(state, |c| c.transform = crate::model::Transform::default());
                return;
            }
        }
        for (rate, rect) in l.speed_presets.clone() {
            if rect.contains(p) {
                let rev = self.selected_clip().map(|c| c.speed.reverse);
                if let Some(rev) = rev {
                    self.set_speed_selected(state, rate, rev);
                }
                return;
            }
        }
        if let Some(rect) = l.reverse {
            if rect.contains(p) {
                let now = self
                    .selected_clip()
                    .map(|c| (c.speed.rate(), c.speed.reverse));
                if let Some((rate, rev)) = now {
                    self.set_speed_selected(state, rate, !rev);
                }
                return;
            }
        }
        // Colour key.
        if let Some(rect) = l.key_enable {
            if rect.contains(p) {
                self.toggle_key_enabled(state);
                return;
            }
        }
        for (col, rect) in l.key_colors.clone() {
            if rect.contains(p) {
                self.edit_selected(state, move |c| c.key.color = col);
                return;
            }
        }
        if let Some(rect) = l.key_pick {
            if rect.contains(p) {
                // Only writes a colour when there really is a decoded frame
                // to read one from; with none the button is drawn disabled
                // and this is a no-op rather than a made-up sample.
                if let Some(col) = self.preview_pixel(0.5, 0.5) {
                    self.edit_selected(state, move |c| c.key.color = col);
                }
                return;
            }
        }
        // Mask.
        for (shape, rect) in l.mask_shapes.clone() {
            if rect.contains(p) {
                self.set_mask_shape(state, shape);
                return;
            }
        }
        if let Some(rect) = l.mask_fit {
            if rect.contains(p) {
                let (src_aspect, frame_aspect) = self.selected_aspects();
                self.edit_selected(state, move |c| {
                    let shape = c.mask.shape;
                    let (feather, invert) = (c.mask.feather, c.mask.invert);
                    c.mask =
                        crate::model::Mask::over(shape, &c.transform, src_aspect, frame_aspect);
                    c.mask.feather = feather;
                    c.mask.invert = invert;
                });
                return;
            }
        }
        if let Some(rect) = l.mask_invert {
            if rect.contains(p) {
                self.edit_selected(state, |c| c.mask.invert = !c.mask.invert);
                return;
            }
        }
        if let Some(rect) = l.mix_reset {
            if rect.contains(p) {
                self.reset_selected_track_mix(state);
                return;
            }
        }
        // Title properties.
        if let Some(rect) = l.title_text {
            if rect.contains(p) {
                if let Some(m) = self.selected_media_item().map(|m| m.id) {
                    self.begin_title_edit(m);
                }
                return;
            }
        }
        for (align, rect) in l.title_align.clone() {
            if rect.contains(p) {
                self.edit_title(state, move |t| t.align = align);
                return;
            }
        }
        if let Some(rect) = l.title_font {
            if rect.contains(p) && self.font_picker.is_none() {
                self.open_font_picker();
                return;
            }
        }
        for (col, rect) in l.title_colors.clone() {
            if rect.contains(p) {
                self.edit_title(state, move |t| t.color = col);
                return;
            }
        }
        if let Some(rect) = l.title_bold {
            if rect.contains(p) {
                self.edit_title(state, |t| t.bold = !t.bold);
                return;
            }
        }
        if let Some(rect) = l.title_italic {
            if rect.contains(p) {
                self.edit_title(state, |t| t.italic = !t.italic);
                return;
            }
        }
        if l.split.contains(p) {
            self.split_selected(state);
            return;
        }
        if l.delete.contains(p) {
            self.delete_selected(state);
        }
    }

    /// A press inside the curve editor: grab the point under the pointer, or
    /// put a new one there.
    ///
    /// Adding on a plain click is what makes the curve editable at all — a
    /// separate "add point" button would need somewhere to add it, and the
    /// answer is always "where I clicked". A click too close to an existing
    /// point grabs that one instead, so points cannot be stacked.
    fn press_curve(&mut self, doc: &mut AutoCommit, canvas: Rect, p: Point) {
        let Some(clip) = self.selected().and_then(|id| self.project.clip(id)) else {
            return;
        };
        let ch = self.curve_channel;
        let (x, y) = curve_coords(canvas, p);
        let existing = clip.curves.spline(ch).point_near(x, y, POINT_GRAB);
        let idx = match existing {
            Some(i) => i,
            None => {
                // A new point is a discrete edit, so it commits now; the drag
                // that follows previews and commits again on release.
                let mut placed = None;
                self.edit_selected(doc, |c| {
                    placed = c.curves.spline_mut(ch).set_point(x, y);
                });
                match placed {
                    Some(i) => i,
                    // Refused — too close to a neighbour for a point to mean
                    // anything. Grab the neighbour rather than doing nothing.
                    None => {
                        let Some(clip) = self.selected().and_then(|id| self.project.clip(id))
                        else {
                            return;
                        };
                        match clip.curves.spline(ch).point_near(x, y, POINT_GRAB * 2.0) {
                            Some(i) => i,
                            None => return,
                        }
                    }
                }
            }
        };
        self.drag = Some(Drag::CurvePoint { channel: ch, idx });
    }

    fn press_export(&mut self, state: &mut AutoCommit, r: &Regions, p: Point) {
        let l = self.export_layout(r);
        if !l.card.contains(p) {
            // Click outside the card dismisses. A render in flight is not
            // abandoned by that: it keeps reporting from the topbar chip and
            // its own live region, and the chip is the way back in here.
            self.presentation.export_open = false;
            return;
        }
        for (res, rect) in &l.resolution {
            if rect.contains(p) {
                self.export.resolution = *res;
                return;
            }
        }
        for (c, rect) in &l.container {
            if rect.contains(p) {
                self.export.container = *c;
                return;
            }
        }
        for (f, rect) in &l.fps {
            if rect.contains(p) {
                self.export.frame_rate = *f;
                return;
            }
        }
        if l.cancel.contains(p) {
            if self.export_running() {
                // A render in flight: this is Cancel, not Close. Stop it and
                // leave the dialog up so the outcome is visible here as well
                // as in the chip.
                if let Some(job) = self.export_job.as_ref() {
                    job.cancel();
                }
            } else {
                self.presentation.export_open = false;
            }
            return;
        }
        if l.confirm.contains(p) {
            // Matches what the button paints: a disabled Export refuses rather
            // than starting something that can only fail.
            if self.export_is_offered() {
                self.request_export();
            }
            let _ = state;
        }
    }

    // ---- project files ---------------------------------------------------
    //
    // ## What happens to the implicit autosave
    //
    // The autosave document does not go away and is not demoted. It remains
    // the thing that makes work impossible to lose: it holds whatever is open
    // right now, saved or not, and it is written back on a debounce whether or
    // not the project has ever been given a file. An unsaved project therefore
    // survives a crash, a reboot and a close, exactly as it did before files
    // existed — which is why closing the window is not made to interrogate the
    // user about saving.
    //
    // What files add is a *second*, explicit copy the user names, can put
    // where they like, can have several of, and can hand to someone. The two
    // never race: a Save writes the file from the same project the autosave
    // holds, and an Open replaces the document — so the autosave then holds
    // the opened project, and the file that was open before is untouched on
    // disk.
    //
    // The one place that could destroy work is opening a file over a project
    // that has never been saved anywhere, because the Open replaces the
    // document the autosave is tracking. That is the only case
    // [`Self::begin_open`] stops to ask about, and Ctrl+Z undoes it even if
    // the user says yes.

    /// Whether the document has changed since the last save.
    ///
    /// A project that has never been saved counts as unsaved as soon as it
    /// has any content — an empty project nobody has touched is not
    /// "unsaved work" and must not raise a question about discarding it.
    fn project_is_dirty(&self) -> bool {
        match &self.saved_heads {
            Some(heads) => *heads != self.doc_heads,
            None => !self.project_is_empty(),
        }
    }

    /// Whether the project holds nothing a user would mind losing.
    fn project_is_empty(&self) -> bool {
        self.project.clips.is_empty()
            && self.project.media.is_empty()
            && self.project.tracks.is_empty()
    }

    /// What the project button says: the file's name, or the project's name
    /// while it has no file yet.
    fn project_label(&self) -> String {
        match &self.project_path {
            Some(p) => crate::project_file::recents::label(p),
            None if self.project.name.trim().is_empty() => "Untitled".to_string(),
            None => self.project.name.clone(),
        }
    }

    /// The one-line answer to "what am I editing, and is it safe?".
    ///
    /// Spoken to a screen reader and used as the project button's tooltip
    /// text, so both say the same thing the dot on the button means.
    fn project_status_line(&self) -> String {
        let label = self.project_label();
        match (&self.project_path, self.project_is_dirty()) {
            (Some(p), true) => format!("{label} — unsaved changes, saved at {}", p.display()),
            (Some(p), false) => format!("{label} — saved at {}", p.display()),
            // Not a warning: the autosave really does hold it. What the user
            // does not have is a file of their own.
            (None, true) => format!("{label} — not saved to a file yet"),
            (None, false) => format!("{label} — empty project"),
        }
    }

    /// Save straight back to the file the project came from, or fall through
    /// to Save As when it has never had one.
    fn save_project(&mut self, doc: &mut AutoCommit) {
        match self.project_path.clone() {
            Some(path) => self.write_project_to(doc, path),
            None => self.request_save_as(),
        }
    }

    fn queue_save_project(&mut self) {
        self.project_sync.save_requested = true;
    }

    /// Ask the host where to write the project. Under a strict sandbox this
    /// round-trip is what earns the write at all.
    fn request_save_as(&mut self) {
        if self.project_rx.is_some() {
            return; // one chooser at a time
        }
        let suggested = crate::project_file::with_extension(&self.project_label());
        self.project_notice = Some(Notice::ok("Choose where to save…"));
        self.project_rx = Some(portal::request_save_project(&suggested));
    }

    /// Ask the host for a project to open.
    fn request_open_project(&mut self) {
        if self.project_rx.is_some() {
            return;
        }
        self.project_notice = None;
        self.project_rx = Some(portal::request_open_project());
    }

    /// Write the project to `path` and remember it as the project's home.
    ///
    /// A failure is reported in the words the file layer produced and the
    /// project keeps its old path: claiming a save that did not happen is the
    /// one outcome that could actually cost someone their work.
    fn write_project_to(&mut self, doc: &mut AutoCommit, path: PathBuf) {
        match crate::project_file::save(&path, &self.project) {
            Ok(()) => {
                self.recents = crate::project_file::recents::remember(&self.recents, &path);
                self.project_notice = Some(Notice::ok(format!("Saved to {}", path.display())));
                // Save As can move the project; relative media follows it.
                crate::media::set_project_anchor(path.parent().map(|p| p.to_path_buf()));
                self.project_path = Some(path);
                // The document's heads as they are *now*, not the copy `sync`
                // cached at the start of the frame. Saving from a key press
                // that followed an edit in the same frame would otherwise
                // stamp the pre-edit heads and leave the project reading as
                // unsaved the instant after it was written.
                self.doc_heads = doc.get_heads();
                self.saved_heads = Some(self.doc_heads.clone());
                // A save that was only standing in the way of an open now
                // lets it through.
                if let Some(next) = self.save_then_open.take() {
                    self.pending_open = None;
                    self.load_project_file(doc, next);
                }
            }
            Err(e) => {
                self.save_then_open = None;
                self.project_notice = Some(Notice::failed(e.to_string()));
            }
        }
    }

    /// Start opening `path`, stopping to ask first if that would drop work
    /// that exists nowhere but this session.
    fn begin_open(&mut self, doc: &mut AutoCommit, path: PathBuf) {
        // Already saved somewhere, or nothing worth keeping: just open it.
        // A project with a file behind it is not at risk — that file is still
        // there afterwards — so only a never-saved project raises the question.
        if self.project_path.is_some() || !self.project_is_dirty() {
            self.load_project_file(doc, path);
            return;
        }
        let losing = self.project_label();
        self.pending_open = Some(PendingOpen { path, losing });
    }

    /// Read `path` and replace the project with what is in it.
    ///
    /// The replacement goes through the ordinary action pipeline, so opening
    /// a file is one undo boundary and replicates to a peer — see
    /// [`crate::actions::VideoEditorAction::LoadProject`].
    fn load_project_file(&mut self, doc: &mut AutoCommit, path: PathBuf) {
        let mut project = match crate::project_file::load(&path) {
            Ok(p) => p,
            Err(e) => {
                self.project_notice = Some(Notice::failed(e.to_string()));
                return;
            }
        };
        // Relative media in this file means "beside this file" — set before
        // anything asks whether a source can be read, including the notice
        // below, or a project copied somewhere with its footage would report
        // the footage missing.
        crate::media::set_project_anchor(path.parent().map(|p| p.to_path_buf()));
        // The name follows the file unless the file's own project carries a
        // real one — a project still called "Untitled" opened from
        // `Holiday.qvp` should read as Holiday.
        if project.name.trim().is_empty() || project.name == "Untitled" {
            if let Some(n) = crate::project_file::name_from_path(&path) {
                project.name = n;
            }
        }
        // Whether this Open is throwing a timeline away. If it is, the notice
        // says the way back, because an accidental Open otherwise feels final
        // when it is not. It now names the control rather than only the key:
        // the transport strip carries a real Undo button, and this dispatch
        // records the boundary it reverts.
        let replaced = !self.project_is_empty();
        let mut notice = Self::opened_notice(&path, &project);
        if replaced {
            notice.text.push_str(" · Undo to go back");
        }
        self.project_notice = Some(notice);

        self.actions.dispatch(
            doc,
            crate::actions::VIDEO_EDITOR_ACTION_TYPE,
            &crate::actions::load_project_action(&project),
        );
        self.project = VideoDoc::load_project(doc);
        self.clip_edit_bases.clear();
        self.doc_heads = doc.get_heads();
        self.saved_heads = Some(self.doc_heads.clone());
        self.project_path = Some(path.clone());
        self.recents = crate::project_file::recents::remember(&self.recents, &path);
        // The view belongs to the project that was open, not to this one.
        self.selection = Selection::default();
        self.selected_media = None;
        self.playhead = 0.0;
        self.scroll = 0.0;
        self.playback.playing = false;
        self.seed.selection_initialised = false;
        self.ensure_initial_selection();
        #[cfg(any(feature = "gst", feature = "decode"))]
        {
            self.media_pipeline.monitor_stale = true;
        }
    }

    /// What to say after an open: how much arrived, and — if any of it cannot
    /// be read here — which sources, in the same words the bin and the
    /// timeline use for the same states.
    fn opened_notice(path: &Path, project: &crate::model::Project) -> Notice {
        let name = crate::project_file::recents::label(path);
        let missing = crate::project_file::missing_sources(project);
        if missing.is_empty() {
            return Notice::ok(format!("Opened {name}"));
        }
        // Split on the distinction the app already draws everywhere else:
        // bytes that are not here yet, versus bytes that are here and are not
        // media. Blurring them would tell a user to re-fetch a file that will
        // never decode.
        let absent = missing.len();
        let names: Vec<&str> = missing.iter().take(3).map(|(n, _)| n.as_str()).collect();
        let listed = names.join(", ");
        let tail = if absent > names.len() {
            format!("{listed} and {} more", absent - names.len())
        } else {
            listed
        };
        // Opened, but not everything came with it: a warning, not a success
        // and not a failure.
        Notice::warning(format!("Opened {name} — {MEDIA_NOT_LOCAL}: {tail}"))
    }

    /// Drain an in-flight project chooser. Mirrors [`Self::poll_export`].
    fn drain_project_portal(&mut self, doc: &mut AutoCommit) {
        let mut outcome = None;
        let mut clear = false;
        if let Some(rx) = self.project_rx.as_ref() {
            match rx.try_recv() {
                Ok(o) => {
                    outcome = Some(o);
                    clear = true;
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => {}
                Err(std::sync::mpsc::TryRecvError::Disconnected) => clear = true,
            }
        }
        if clear {
            self.project_rx = None;
        }
        match outcome {
            Some(portal::ProjectOutcome::Save(path)) => self.write_project_to(doc, path),
            Some(portal::ProjectOutcome::Open(path)) => self.begin_open(doc, path),
            Some(portal::ProjectOutcome::Cancelled) => {
                self.save_then_open = None;
                self.project_notice = None;
                if let Some(invocation) = self.resource_invocation.take() {
                    self.resource_operation = None;
                    self.resource_result = Some(tilo_actions::InvocationUpdate::Cancelled {
                        invocation,
                        message: Some("project file selection was cancelled".into()),
                    });
                }
            }
            Some(portal::ProjectOutcome::Failed(reason)) => {
                // A portal that refused is a failure, tinted as one. Its own
                // wording ("the file picker needs the real-desktop build")
                // names no English the old substring test looked for, so this
                // used to paint in the success tint.
                self.save_then_open = None;
                self.project_notice = Some(Notice::failed(reason));
            }
            None => {}
        }
    }

    /// Answer the "this project has never been saved" question.
    ///
    /// `Discard` opens anyway — recoverable, because the open is one action
    /// and Ctrl+Z puts the old project back. `Save` writes first and then
    /// continues into the open on its own, so the user does not have to
    /// remember what they were doing.
    fn resolve_pending_open(&mut self, doc: &mut AutoCommit, choice: DiscardChoice) {
        let Some(pending) = self.pending_open.take() else {
            return;
        };
        match choice {
            DiscardChoice::Cancel => {
                self.save_then_open = None;
            }
            DiscardChoice::Discard => self.load_project_file(doc, pending.path),
            DiscardChoice::Save => {
                self.save_then_open = Some(pending.path);
                self.save_project(doc);
            }
        }
    }

    /// The rows the project menu offers right now.
    fn project_menu_rows(&self) -> Vec<(ProjectMenuItem, String)> {
        let mut rows = vec![
            (ProjectMenuItem::Open, "Open…  (Ctrl+O)".to_string()),
            (
                ProjectMenuItem::Save,
                match &self.project_path {
                    Some(_) => "Save  (Ctrl+S)".to_string(),
                    // Naming what Save will actually do, rather than letting
                    // the user press it and be surprised by a file chooser.
                    None => "Save…  (Ctrl+S)".to_string(),
                },
            ),
            (
                ProjectMenuItem::SaveAs,
                "Save As…  (Ctrl+Shift+S)".to_string(),
            ),
        ];
        // Recents, minus the project already open — an entry that reopens
        // what you are looking at is a row that does nothing.
        let recent: Vec<&PathBuf> = self
            .recents
            .iter()
            .filter(|p| Some(*p) != self.project_path.as_ref())
            .collect();
        if !recent.is_empty() {
            for p in recent {
                rows.push((
                    ProjectMenuItem::Recent(p.clone()),
                    crate::project_file::recents::label(p),
                ));
            }
        }
        rows
    }

    /// Open the project menu under the project button.
    fn open_project_menu(&mut self, at: Point) {
        let rows = self.project_menu_rows();
        self.project_menu = ContextMenu::new();
        self.project_menu_items.clear();
        let mut first_recent = true;
        for (item, label) in rows {
            if matches!(item, ProjectMenuItem::Recent(_)) && first_recent {
                self.project_menu.push(ui::widget::MenuItem::separator());
                first_recent = false;
            }
            self.project_menu
                .push(ui::widget::MenuItem::action(label, || {}));
            self.project_menu_items.push(item);
        }
        self.project_menu.open_at([at.x as f32, at.y as f32]);
        self.project_menu.layout_at_position();
    }

    /// Resolve a click on the open project menu. Separators occupy a row in
    /// the menu but not in [`Self::project_menu_items`], so the index is
    /// mapped through the same skip the build used rather than assumed equal.
    fn project_menu_intent(&self, idx: usize) -> Option<ProjectMenuItem> {
        let mut actionable = 0usize;
        for (i, row) in self.project_menu.items().iter().enumerate() {
            if matches!(row, ui::widget::MenuItem::Separator) {
                continue;
            }
            if i == idx {
                return self.project_menu_items.get(actionable).cloned();
            }
            actionable += 1;
        }
        None
    }

    /// Act on a chosen project-menu row.
    fn run_project_menu_item(&mut self, doc: &mut AutoCommit, item: ProjectMenuItem) {
        match item {
            ProjectMenuItem::Open => self.request_open_project(),
            ProjectMenuItem::Save => self.queue_save_project(),
            ProjectMenuItem::SaveAs => self.request_save_as(),
            ProjectMenuItem::Recent(p) => self.begin_open(doc, p),
        }
    }

    // ---- export ----------------------------------------------------------
    /// Ask the user where the render should land, then encode there. The
    /// picker is the grant: under a strict sandbox it is what earns the write
    /// to somewhere like `~/Videos`, and without it the file lands in a temp
    /// directory the user was never told about and cannot find.
    fn request_export(&mut self) {
        if self.export_job.is_some() {
            return; // one render at a time
        }
        if let Some(reason) = crate::export::unavailable_reason() {
            self.export_status = ExportStatus::Failed {
                reason: reason.to_string(),
            };
            return;
        }
        let name = self.export.suggested_filename(&self.project.name);
        let ext = self.export.container.extension();
        self.export_notice = Some("Choose where to save…".to_string());
        self.export_dest_rx = Some(portal::request_export_destination(&name, ext));
    }

    /// Begin the render at `destination` on a worker thread.
    fn begin_export_to(&mut self, destination: PathBuf) {
        self.export_notice = None;
        self.export_status = ExportStatus::Rendering { progress: 0.0 };
        self.export_job = Some(crate::export::start_export(
            &self.project,
            &self.export,
            destination,
        ));
    }

    /// Drain the destination picker and poll the running render. Called every
    /// frame; `needs_animation_frame` keeps frames coming while a job runs, so
    /// the progress bar actually moves.
    fn poll_export(&mut self) {
        // Destination round-trip.
        let mut outcome = None;
        let mut clear = false;
        if let Some(rx) = self.export_dest_rx.as_ref() {
            match rx.try_recv() {
                Ok(o) => {
                    outcome = Some(o);
                    clear = true;
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => {}
                Err(std::sync::mpsc::TryRecvError::Disconnected) => clear = true,
            }
        }
        if clear {
            self.export_dest_rx = None;
        }
        match outcome {
            Some(portal::ExportDestOutcome::Opened(path)) => self.begin_export_to(path),
            Some(portal::ExportDestOutcome::Cancelled) => {
                self.export_notice = None;
                if let Some(invocation) = self.resource_invocation.take() {
                    self.resource_operation = None;
                    self.resource_result = Some(tilo_actions::InvocationUpdate::Cancelled {
                        invocation,
                        message: Some("export destination selection was cancelled".into()),
                    });
                }
            }
            Some(portal::ExportDestOutcome::Failed(reason)) => {
                // No portal on this box. Say where the file will go rather than
                // silently dropping it somewhere the user will never look.
                let fallback = crate::export::output_path(&self.export, &self.project);
                self.export_notice = Some(format!("{reason} — saving to {}", fallback.display()));
                self.begin_export_to(fallback);
            }
            None => {}
        }
        // Running render.
        if let Some(job) = self.export_job.as_ref() {
            let status = job.status();
            let finished = !matches!(status, ExportStatus::Rendering { .. });
            self.export_status = status;
            if finished {
                self.export_job = None;
            }
        }
    }

    /// Whether a render is in flight (so Cancel means "stop it", not "close").
    fn export_running(&self) -> bool {
        self.export_job.is_some()
    }

    /// Whether the Export action is actually available. The dialog paints the
    /// button from this and the press path gates on it, so the offer and the
    /// behaviour cannot disagree — which they did: the dialog said "Export
    /// needs the GStreamer build" and put an enabled Export button under it.
    fn export_is_offered(&self) -> bool {
        crate::export::unavailable_reason().is_none() && !self.export_running()
    }

    fn transport_action(&mut self, action: TransportAction) {
        match action {
            TransportAction::ToStart => {
                self.playhead = 0.0;
                self.playback.playing = false;
            }
            TransportAction::StepBack => self.step_frame(-1.0),
            TransportAction::PlayPause => self.toggle_play(),
            TransportAction::StepFwd => self.step_frame(1.0),
            TransportAction::ToEnd => {
                self.playhead = self.project.duration();
                self.playback.playing = false;
            }
        }
        self.playback.chase_playhead = true;
    }

    /// Track the live drag, updating **only the in-memory project** for a
    /// smooth preview. Nothing is written to the CRDT here — the clip's final
    /// position / trim / grade is dispatched as one action when the drag ends
    /// (see [`Self::commit_drag`]), so a drag is one undo step, not sixty a
    /// second of doc churn. The playhead / scrubber drags are pure presentation.
    fn apply_drag(&mut self, r: &Regions) {
        let Some(drag) = self.drag else { return };
        let p = self.last_mouse;
        match drag {
            Drag::CurvePoint { channel, idx } => {
                // The box is re-derived from the live layout rather than
                // captured at press: the inspector scrolls, and a captured
                // rect would leave the point tracking a box that has moved.
                let Some(canvas) = self.inspector_layout(r).curve_canvas else {
                    return;
                };
                let (x, y) = curve_coords(canvas, p);
                self.preview_edit(move |c| {
                    c.curves.spline_mut(channel).move_point(idx, x, y);
                });
            }
            Drag::Playhead => {
                self.playhead = snap_to_frame(self.x_to_time(p.x, &r.lanes), self.project.fps)
                    .clamp(0.0, self.project.duration());
                self.playback.chase_playhead = true;
            }
            Drag::Scrubber => self.apply_scrubber(p, r),
            Drag::Scrollbar(pane) => {
                if let Some((track, m)) = self.scroll_pane(pane, r) {
                    if let Some(px) = self.scroll_bar(pane).drag_to(track, m, p) {
                        self.set_scroll_offset(pane, px, r);
                    }
                }
            }
            Drag::Marquee { origin, .. } => {
                if origin.distance(p) > MARQUEE_THRESHOLD {
                    self.marquee = Some(Rect::new(
                        origin.x.min(p.x),
                        origin.y.min(p.y),
                        origin.x.max(p.x),
                        origin.y.max(p.y),
                    ));
                }
            }
            Drag::ClipMove { id, grab_dt, track } => {
                let raw = (self.x_to_time(p.x, &r.lanes) - grab_dt).max(0.0);
                // Snap the grabbed clip's left edge to a neighbouring edge /
                // playhead when within a few pixels, then to the frame grid.
                let snapped = self.snap_time(raw, id);
                let new_start = snap_to_frame(snapped, self.project.fps);
                let old_start = self.project.clip(id).map(|c| c.start).unwrap_or(new_start);
                let dt = new_start - old_start;
                // Resolve the lane under the cursor: a vertical drag moves the
                // clip between tracks. Only a lane that will actually accept it
                // (right kind, unlocked) becomes the target — over anything
                // else the clip keeps the track it already had, so the drag
                // degrades to a pure horizontal move instead of vanishing.
                let target = self
                    .lane_rects(r)
                    .into_iter()
                    .find(|(_, _, lane)| p.y >= lane.y0 && p.y <= lane.y1)
                    .map(|(tid, _, _)| tid)
                    .filter(|tid| self.project.can_place(id, *tid))
                    .unwrap_or(track);
                // Move the grabbed clip, then carry the rest of the selection
                // by the same time delta so a multi-clip drag stays rigid.
                // Only the grabbed clip changes lane; the others would need a
                // parallel lane each, which the timeline cannot promise.
                self.remember_clip_before(id);
                self.project.move_clip_to(id, target, new_start);
                if dt != 0.0 {
                    for other in self.edit_targets() {
                        if other == id {
                            continue;
                        }
                        self.remember_clip_before(other);
                        let s = self.project.clip(other).map(|c| c.start).unwrap_or(0.0);
                        self.project.move_clip(other, (s + dt).max(0.0));
                    }
                }
                if let Some(d) = self.drag.as_mut() {
                    *d = Drag::ClipMove {
                        id,
                        grab_dt,
                        track: target,
                    };
                }
            }
            Drag::TrimIn { id } => {
                let raw = self.x_to_time(p.x, &r.lanes);
                let target = snap_to_frame(self.snap_time(raw, id), self.project.fps);
                let delta = self.project.clip(id).map_or(0.0, |c| target - c.start);
                // The linked half takes the amount this clip could actually
                // take, so a trim that clamps on one of them clamps on both.
                let also = self.linked_followers(id);
                self.remember_clip_before(id);
                for linked in &also {
                    self.remember_clip_before(*linked);
                }
                self.project.trim_in_with(id, delta, &also);
            }
            Drag::TrimOut { id } => {
                let raw = self.x_to_time(p.x, &r.lanes);
                let target = snap_to_frame(self.snap_time(raw, id), self.project.fps);
                let delta = self.project.clip(id).map_or(0.0, |c| target - c.end());
                let also = self.linked_followers(id);
                self.remember_clip_before(id);
                for linked in &also {
                    self.remember_clip_before(*linked);
                }
                self.project.trim_out_with(id, delta, &also);
            }
            Drag::Opacity | Drag::Volume | Drag::Brightness | Drag::Contrast | Drag::Saturation => {
                let l = self.inspector_layout(r);
                let (track, param) = match drag {
                    Drag::Opacity => (l.opacity, Param::Opacity),
                    Drag::Volume => (l.volume, Param::Volume),
                    Drag::Brightness => (l.brightness, Param::Brightness),
                    Drag::Contrast => (l.contrast, Param::Contrast),
                    _ => (l.saturation, Param::Saturation),
                };
                if let Some(track) = track {
                    let v = ((p.x - track.x0) / track.width()).clamp(0.0, 1.0);
                    self.preview_param(param, v);
                }
            }
            Drag::FadeIn => {
                if let Some(track) = self.inspector_layout(r).fade_in {
                    let frac = ((p.x - track.x0) / track.width()).clamp(0.0, 1.0);
                    let max = self
                        .selected()
                        .and_then(|id| self.project.clip(id))
                        .map(|c| c.duration())
                        .unwrap_or(0.0);
                    self.preview_edit(move |c| c.fade_in = (frac * max).clamp(0.0, c.duration()));
                }
            }
            Drag::TransitionLen => {
                if let Some(track) = self.inspector_layout(r).transition_len {
                    let frac = ((p.x - track.x0) / track.width()).clamp(0.0, 1.0);
                    let max = self.transition_len_max();
                    self.preview_edit(move |c| c.transition_duration = (frac * max).max(0.01));
                }
            }
            Drag::FadeOut => {
                if let Some(track) = self.inspector_layout(r).fade_out {
                    let frac = ((p.x - track.x0) / track.width()).clamp(0.0, 1.0);
                    let max = self
                        .selected()
                        .and_then(|id| self.project.clip(id))
                        .map(|c| c.duration())
                        .unwrap_or(0.0);
                    self.preview_edit(move |c| c.fade_out = (frac * max).clamp(0.0, c.duration()));
                }
            }
            Drag::EffectAmount { idx, .. } => {
                if let Some(slider) = self
                    .inspector_layout(r)
                    .effect_rows
                    .get(idx)
                    .map(|row| Self::effect_row_parts(*row).3)
                {
                    let v = ((p.x - slider.x0) / slider.width()).clamp(0.0, 1.0);
                    self.preview_edit(move |c| {
                        if let Some(e) = c.effects.get_mut(idx) {
                            e.amount = v;
                        }
                    });
                }
            }
            Drag::Num(f) => {
                if let Some(track) = self.inspector_layout(r).num(f) {
                    let frac = ((p.x - track.x0) / track.width()).clamp(0.0, 1.0);
                    self.set_num(f, f.from_frac(frac));
                }
            }
        }
    }

    /// Commit the edit a drag just made: dispatch one action carrying the final
    /// shape of the clip the drag touched. Called on mouse-up, before the drag
    /// is cleared. Playhead / scrubber drags are presentation and commit
    /// nothing.
    fn commit_drag(&mut self, doc: &mut AutoCommit) {
        let Some(drag) = self.drag else { return };
        match drag {
            Drag::Playhead | Drag::Scrubber | Drag::Scrollbar(_) | Drag::Marquee { .. } => {}
            Drag::ClipMove { id, .. } => {
                // The whole moved selection lands in one event, so the move —
                // including the lane change and every clip carried along —
                // is a single all-or-nothing field patch rather than a row of
                // stale whole-clip snapshots.
                let mut ids = self.edit_targets();
                if !ids.contains(&id) {
                    ids.push(id);
                }
                self.dispatch_existing_clips(doc, &ids);
            }
            Drag::TrimIn { id } | Drag::TrimOut { id } => {
                self.dispatch_clip_linked(doc, id);
            }
            Drag::EffectAmount {
                idx,
                effect,
                expected_amount_bits,
            } => {
                let Some(id) = self.selected() else { return };
                let Some(amount) = self
                    .project
                    .clip(id)
                    .and_then(|clip| clip.effects.get(idx))
                    .filter(|current| current.id == effect)
                    .map(|current| current.amount)
                else {
                    return;
                };
                self.actions.dispatch(
                    doc,
                    VIDEO_EDITOR_ACTION_TYPE,
                    &VideoEditorAction::SetEffectAmount {
                        clip: id.to_string(),
                        effect: effect.to_string(),
                        expected_amount_bits,
                        amount,
                    },
                );
            }
            Drag::Opacity
            | Drag::Volume
            | Drag::Brightness
            | Drag::Contrast
            | Drag::Saturation
            | Drag::FadeIn
            | Drag::FadeOut
            | Drag::TransitionLen => {
                if let Some(id) = self.selected() {
                    self.dispatch_clip(doc, id);
                }
            }
            // A title knob writes the *source*, so it commits a media write;
            // everything else on this path is a clip field.
            Drag::Num(f) if f.is_media() => self.dispatch_selected_media(doc),
            // …and the two lane knobs write the *track*, which is a third
            // destination again. Committed as one whole-track action, so the
            // fader lands on one undo boundary and replays on a peer as the
            // same change the palette's `set-gain` makes.
            Drag::Num(f) if f.is_track() => self.dispatch_selected_track(doc),
            // Rate is the one field the drag applied to the whole link group,
            // so it is the one that has to commit the whole group.
            Drag::Num(NumField::Speed) => {
                if let Some(id) = self.selected() {
                    self.dispatch_clip_linked(doc, id);
                }
            }
            Drag::Num(_) => {
                if let Some(id) = self.selected() {
                    self.dispatch_clip(doc, id);
                }
            }
            Drag::CurvePoint { channel, idx } => {
                // Released well outside the box, an interior point comes off
                // the curve — the standard gesture, and the only one that
                // fits without a second control per point. The endpoints
                // stay: a curve with no value at 0 or 1 has undefined ends.
                if let Some(canvas) = self.curve_canvas_now() {
                    let out = self.last_mouse.x < canvas.x0 - POINT_DROP_MARGIN
                        || self.last_mouse.x > canvas.x1 + POINT_DROP_MARGIN
                        || self.last_mouse.y < canvas.y0 - POINT_DROP_MARGIN
                        || self.last_mouse.y > canvas.y1 + POINT_DROP_MARGIN;
                    if out {
                        self.preview_edit(move |c| {
                            c.curves.spline_mut(channel).remove_point(idx);
                        });
                    }
                }
                if let Some(id) = self.selected() {
                    self.dispatch_clip(doc, id);
                }
            }
        }
    }

    /// The curve editor's box as the inspector has it laid out right now, or
    /// `None` when the section is not on screen.
    ///
    /// `commit_drag` has no `Regions` to hand, and re-deriving them is what
    /// keeps the drop test honest — a stale rect would decide "outside the
    /// box" against a box that is no longer there.
    fn curve_canvas_now(&self) -> Option<Rect> {
        let r = self.regions();
        self.inspector_is_visible(&r)
            .then(|| self.inspector_layout(&r).curve_canvas)
            .flatten()
    }

    /// True while a drag that edits a clip is in flight, so [`Widget::sync`]
    /// leaves the live in-memory preview alone instead of reloading it from the
    /// (not-yet-written) document mid-drag.
    fn drag_is_editing(&self) -> bool {
        matches!(
            self.drag,
            Some(
                Drag::ClipMove { .. }
                    | Drag::TrimIn { .. }
                    | Drag::TrimOut { .. }
                    | Drag::Opacity
                    | Drag::Volume
                    | Drag::Brightness
                    | Drag::Contrast
                    | Drag::Saturation
                    | Drag::FadeIn
                    | Drag::FadeOut
                    | Drag::TransitionLen
                    | Drag::EffectAmount { .. }
                    | Drag::Num(_)
                    | Drag::CurvePoint { .. }
            )
        )
    }

    /// Magnetic snap for a dragged clip edge at raw time `t`: snaps to the
    /// nearest neighbouring clip edge / playhead / origin within a small
    /// pixel tolerance (converted to seconds via the current zoom), so clips
    /// butt up cleanly. Excludes the dragged clip's own edges.
    fn snap_time(&self, t: f64, dragged: ClipId) -> f64 {
        let tol = 8.0 / self.zoom.max(0.001); // 8px in seconds
        let cands = self.project.snap_candidates(dragged, self.playhead);
        Project::snap_to_candidates(t, &cands, tol)
    }

    fn apply_scrubber(&mut self, p: Point, r: &Regions) {
        let scrub = self.scrubber_rect(r);
        if scrub.width() <= 1.0 {
            return;
        }
        let frac = ((p.x - scrub.x0) / scrub.width()).clamp(0.0, 1.0);
        self.playhead = (frac * self.project.duration()).clamp(0.0, self.project.duration());
        self.playback.playing = false;
        // The transport scrubber spans the whole project, so it can land the
        // playhead anywhere — bring the timeline along.
        self.playback.chase_playhead = true;
    }

    fn update_hover(&mut self, r: &Regions) {
        let p = self.last_mouse;
        // Brighten whichever scrollbar thumb the pointer is on.
        for pane in Self::SCROLL_PANES {
            match self.scroll_pane(pane, r) {
                Some((track, m)) => {
                    self.scroll_bar_mut(pane).update_hover(track, m, p);
                }
                None => {
                    self.scroll_bar_mut(pane).set_hover(false);
                }
            }
        }
        let bin_visible = self.bin_is_visible(r);
        self.hover_media = if bin_visible {
            self.bin_item_rects(r)
                .into_iter()
                .find(|(_, rect)| rect.contains(p))
                .map(|(m, _)| m)
        } else {
            None
        };
        self.hover_clip = None;
        if r.lanes.contains(p) {
            for (tid, _k, lane) in self.lane_rects(r) {
                if p.y < lane.y0 || p.y > lane.y1 {
                    continue;
                }
                for c in self.project.clips_on(tid) {
                    let cr = self.clip_rect(c, &lane, r);
                    if p.x >= cr.x0 && p.x <= cr.x1 {
                        self.hover_clip = Some(c.id);
                    }
                }
            }
        }
    }
}

// ==========================================================================
// Drawing helpers
// ==========================================================================
impl VideoEditorApp {
    #[allow(
        clippy::too_many_lines,
        reason = "chrome composition keeps the top-bar's shared geometry and semantic controls in one paint pass"
    )]
    fn draw_chrome(
        &self,
        scene: &mut Scene,
        tf: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        r: &Regions,
    ) {
        let c = &theme.tokens.colors;
        fill_rect(scene, tf, c.surfaces.base, self.rect);
        // Top bar.
        fill_rect(scene, tf, c.surfaces.raised, r.topbar);
        stroke_line(
            scene,
            tf,
            c.border_standard,
            r.topbar.x0,
            r.topbar.y1,
            r.topbar.x1,
            r.topbar.y1,
            1.0,
        );
        // Leading slot: a hamburger toggle when the bin is collapsed to an
        // overlay flap. The OS owns the window title/app-name/logo, so the app
        // draws no glyph or wordmark here.
        if r.bin_overlay {
            self.draw_hamburger(scene, tf, theme, r.menu_btn);
        }
        // Inspector toggle, present only while the inspector is collapsed.
        if r.inspector_overlay && r.inspector_btn.width() > 0.0 {
            let rect = r.inspector_btn;
            let open = self.inspector_flap.is_open(self.inspector_split());
            let hovered = rect.contains(self.last_mouse);
            let bg = if open {
                c.accent[5]
            } else if hovered {
                c.lift(0.72)
            } else {
                c.neutral[3]
            };
            fill_rrect(scene, tf, bg, rect, 7.0);
            let fg = if open {
                on_fill(theme, bg).1
            } else {
                c.text_primary
            };
            // A panel glyph: the pane outline with its right column filled,
            // which is where the inspector lives and what the toggle reveals.
            let g = rect.inset(-8.0);
            stroke_rrect(scene, tf, fg, g, 2.0, 1.4);
            fill_rect(
                scene,
                tf,
                fg,
                Rect::new(g.x0 + g.width() * 0.55, g.y0, g.x1, g.y1),
            );
        }
        // The project button. The frame size and rate still live in the
        // transport beside the timecode they belong with — this is a *control*
        // (it opens Open / Save / Save As / recents) that happens to be
        // labelled with what it acts on, not the project meta that used to be
        // centred here and collided with the buttons on the right.
        //
        // The app name and the window title remain the OS's, and this is
        // neither: which *file* is open, and whether it is written down, are
        // things only the app knows.
        self.draw_project_button(scene, tf, tr, theme, r);
        if self.topbar_is_compact() {
            draw_icon_button(
                scene,
                tf,
                tr,
                theme,
                IconButtonSpec {
                    rect: r.import_btn,
                    icon: ui::icons::Icon::FolderOpen,
                    accessible_name: "Import media",
                    primary: false,
                    hovered: r.import_btn.contains(self.last_mouse),
                },
            );
        } else {
            draw_button(
                scene,
                tf,
                tr,
                theme,
                r.import_btn,
                "Import…",
                false,
                r.import_btn.contains(self.last_mouse),
            );
        }
        if r.split_btn.width() > 0.0 {
            draw_button(
                scene,
                tf,
                tr,
                theme,
                r.split_btn,
                "Split (S)",
                false,
                r.split_btn.contains(self.last_mouse),
            );
        }
        if self.topbar_is_compact() {
            draw_icon_button(
                scene,
                tf,
                tr,
                theme,
                IconButtonSpec {
                    rect: r.export_btn,
                    icon: ui::icons::Icon::ArrowUp,
                    accessible_name: "Export video",
                    primary: true,
                    hovered: r.export_btn.contains(self.last_mouse),
                },
            );
        } else {
            draw_button(
                scene,
                tf,
                tr,
                theme,
                r.export_btn,
                "Export…",
                true,
                r.export_btn.contains(self.last_mouse),
            );
        }
    }

    /// The project button: which project is open, whether it has been written
    /// to a file, and the way to open or save one.
    ///
    /// Painted through the shared [`ui::widget::Button`] face so it carries
    /// the same fill, border, hover and press treatment as every other button
    /// in the fleet. The unsaved dot is a corner badge rather than part of the
    /// label, because the label is centred and a marker inside it would sit
    /// wherever the name's width happened to put it.
    fn draw_project_button(
        &self,
        scene: &mut Scene,
        tf: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        r: &Regions,
    ) {
        let rect = r.project_btn;
        if rect.width() <= 0.0 {
            return;
        }
        let c = &theme.tokens.colors;
        let hovered = rect.contains(self.last_mouse) || self.project_menu.is_open();
        // Room for the leading icon, the padding either side, and the badge.
        let label = if self.topbar_is_compact() {
            String::new()
        } else {
            elide_at(
                &self.project_label(),
                (rect.width() - 52.0) as f32,
                13.0,
                tr,
            )
        };
        let face = ui::widget::ButtonFace::new(rect, &label)
            .style(ui::widget::ButtonStyle::Neutral)
            .icon(ui::icons::Icon::Film)
            .radius(7.0)
            .text_size(13.0)
            .hovered(hovered)
            .pressed(self.project_menu.is_open());
        ui::widget::Button::paint(scene, tf, tr, theme, &face);

        if self.project_is_dirty() {
            // A filled dot on the corner, ringed in the topbar's own colour so
            // it reads as a badge on the button rather than a speck in it.
            let ctr = Point::new(rect.x1 - 7.0, rect.y0 + 7.0);
            scene.fill(
                Fill::NonZero,
                tf,
                c.surfaces.raised,
                None,
                &Circle::new(ctr, 5.0),
            );
            scene.fill(Fill::NonZero, tf, c.accent[5], None, &Circle::new(ctr, 3.5));
        }
    }

    /// Invite an external file drop, over the whole window.
    ///
    /// `DroppedFile` is accepted anywhere in the window, so the invitation has
    /// to cover the same ground the handler does. Highlighting the media bin
    /// alone said "drop here", which is narrower than the truth, and when the
    /// bin was collapsed to a closed flap it said nothing at all — a drop that
    /// would have worked looked like one that would not.
    fn draw_file_drop_invite(
        &self,
        scene: &mut Scene,
        tf: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
    ) {
        if !self.presentation.file_hovering {
            return;
        }
        let c = &theme.tokens.colors;
        let w = self.rect;
        fill_rect(scene, tf, with_alpha(c.accent[5], 0.10), w);
        stroke_rrect(scene, tf, c.accent[5], w.inset(-4.0), 10.0, 3.0);
        // One label, centred on the window, on a plate so it stays readable
        // over the timeline and the preview alike.
        let hint = "Drop to import";
        let (hw, _) = tr.measure_weighted(hint, 15.0, None, TextWeight::Semibold);
        let ctr = w.center();
        let plate = Rect::new(
            ctr.x - hw as f64 / 2.0 - 18.0,
            ctr.y - 20.0,
            ctr.x + hw as f64 / 2.0 + 18.0,
            ctr.y + 20.0,
        );
        fill_rrect(scene, tf, c.surfaces.raised, plate, 10.0);
        stroke_rrect(scene, tf, c.accent[5], plate, 10.0, 1.5);
        let baseline = text_baseline(plate, 15.0, tr);
        text_w(
            scene,
            tr,
            tf,
            hint,
            15.0,
            c.accent[5],
            plate.x0 + 18.0,
            baseline,
            TextWeight::Semibold,
        );
    }

    /// The last thing that happened to the project file, on a strip under the
    /// topbar. Full width and above the panes, because it is not about the bin
    /// or the timeline — and because a save that failed must not be findable
    /// only by opening a panel.
    fn draw_project_notice(
        &self,
        scene: &mut Scene,
        tf: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        r: &Regions,
    ) {
        let Some(notice) = &self.project_notice else {
            return;
        };
        let c = &theme.tokens.colors;
        // The severity was decided where the outcome was known and travels
        // with the message, so a notice in any language is tinted correctly.
        let tint = notice.level.tint(theme);
        // A floating plate, not a full-width band. A translucent band across
        // the whole window let the media bin's own header show through it, so
        // "Saved to …" and "MEDIA BIN · 7 clips" were painted on top of each
        // other; this is opaque, sized to its words, and plainly a transient
        // thing sitting over the editor rather than a row of the layout.
        let x0 = if r.project_btn.width() > 0.0 {
            r.project_btn.x0
        } else {
            r.topbar.x0 + 12.0
        };
        let room = (self.project_notice_limit(r) - x0).max(80.0);
        let shown = elide_at(&notice.text, (room - 24.0) as f32, 11.5, tr);
        let (w, _) = tr.measure(&shown, 11.5, None);
        let plate = Rect::new(
            x0,
            r.topbar.y1 + 6.0,
            (x0 + f64::from(w) + 24.0).min(self.project_notice_limit(r)),
            r.topbar.y1 + 32.0,
        );
        fill_rrect(scene, tf, c.surfaces.raised, plate, 7.0);
        fill_rrect(scene, tf, with_alpha(tint, 0.18), plate, 7.0);
        stroke_rrect(scene, tf, with_alpha(tint, 0.55), plate, 7.0, 1.0);
        let baseline = text_baseline(plate, 11.5, tr);
        text(
            scene,
            tr,
            tf,
            &shown,
            11.5,
            c.text_primary,
            plate.x0 + 12.0,
            baseline,
        );
    }

    /// Whether a render has anything to report right now. A job that is still
    /// encoding, and the outcome of the one that just finished, both stand —
    /// the outcome until the next export replaces it, exactly like the
    /// project notice above it.
    fn export_chip_is_shown(&self) -> bool {
        !matches!(self.export_status, ExportStatus::Idle)
    }

    /// The words the export chip carries and the severity they read at.
    ///
    /// This is the whole point of the chip: `export_status` used to be read in
    /// two places and both were gated on the dialog being open, so dismissing
    /// the dialog while a render was in flight left the render with nowhere at
    /// all to report — no bar, no outcome, no failure, and nothing in the
    /// accessibility tree. The only evidence was a file appearing on disk.
    fn export_chip_notice(&self) -> Option<Notice> {
        match &self.export_status {
            ExportStatus::Idle => None,
            ExportStatus::Rendering { progress } => Some(Notice::ok(format!(
                "Exporting… {}%",
                (progress * 100.0).round() as i64
            ))),
            ExportStatus::Done { path } => Some(Notice::ok(format!(
                "Exported {}",
                display_name(Path::new(path))
            ))),
            ExportStatus::Failed { reason } => Some(Notice::failed(reason.clone())),
        }
    }

    /// The chip's words and severity, formatted and elided only when they
    /// have actually changed.
    ///
    /// A render keeps animation frames coming for its whole duration, so this
    /// is on the hot path: the text is rebuilt when the whole-percent figure
    /// moves (or the window width does), not on every encoded frame.
    fn export_chip_text(&mut self, chip: Rect, tr: &mut TextRenderer) -> Option<&ExportChipText> {
        if !self.export_chip_is_shown() {
            self.export_chip_text = None;
            return None;
        }
        let fresh = self.export_chip_text.as_ref().is_some_and(|c| {
            (c.width - chip.width()).abs() < 0.5 && chip_text_matches(&c.from, &self.export_status)
        });
        if !fresh {
            // Only on a real change: the format and the elision both live
            // behind this branch, so a steady render costs neither.
            let notice = self.export_chip_notice()?;
            self.export_chip_text = Some(ExportChipText {
                from: self.export_status.clone(),
                width: chip.width(),
                level: notice.level,
                shown: elide_at(&notice.text, (chip.width() - 20.0) as f32, 11.0, tr),
            });
        }
        self.export_chip_text.as_ref()
    }

    /// Paint the render's own state into the window chrome.
    ///
    /// The worker was built to run while the editor stays editable — it clones
    /// the project precisely so the UI thread can keep working — so closing
    /// the dialog is a reasonable thing to do mid-render, and the render has
    /// to keep reporting somewhere when it happens.
    fn draw_export_chip(
        &mut self,
        scene: &mut Scene,
        tf: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        r: &Regions,
    ) {
        let chip = r.export_chip;
        if chip.width() <= 0.0 {
            self.export_chip_text = None;
            return;
        }
        let progress = match self.export_status {
            ExportStatus::Rendering { progress } => Some(progress.clamp(0.0, 1.0)),
            _ => None,
        };
        let Some(cached) = self.export_chip_text(chip, tr) else {
            return;
        };
        let level = cached.level;
        // Cloned off the cache so the scene draws below can borrow `tr` again;
        // one short string, and only while a render has something to say.
        let shown = cached.shown.clone();
        let c = &theme.tokens.colors;
        let tint = level.tint(theme);
        fill_rrect(scene, tf, c.surfaces.raised, chip, 7.0);
        fill_rrect(scene, tf, with_alpha(tint, 0.18), chip, 7.0);
        // A render in flight fills the chip as it encodes: the progress bar
        // the dialog used to hold alone, in a place that survives the dialog.
        // Clipped to the chip's own shape so the fill's leading edge is a
        // straight line rather than a second pill nested inside the first.
        if let Some(p) = progress {
            let done = chip.width() * p;
            if done > 0.5 {
                scene.push_layer(
                    Fill::NonZero,
                    vello::peniko::BlendMode::default(),
                    1.0,
                    tf,
                    &RoundedRect::from_rect(chip, 7.0),
                );
                fill_rect(
                    scene,
                    tf,
                    with_alpha(tint, 0.34),
                    Rect::new(chip.x0, chip.y0, chip.x0 + done, chip.y1),
                );
                scene.pop_layer();
            }
        }
        stroke_rrect(scene, tf, with_alpha(tint, 0.55), chip, 7.0, 1.0);
        let baseline = text_baseline(chip, 11.0, tr);
        text(
            scene,
            tr,
            tf,
            &shown,
            11.0,
            c.text_primary,
            chip.x0 + 10.0,
            baseline,
        );
    }

    /// How far right the project-notice plate may run. It shares the strip
    /// under the topbar with the export chip, which is anchored to the right;
    /// a plate that ran to the window edge would be drawn straight through it.
    fn project_notice_limit(&self, r: &Regions) -> f64 {
        if self.export_chip_is_shown() {
            r.export_chip.x0 - 8.0
        } else {
            r.topbar.x1 - 12.0
        }
    }

    /// Card and buttons for the unsaved-work question.
    fn discard_layout(&self) -> (Rect, Rect, Rect, Rect) {
        let w = 420.0_f64.min(self.rect.width() - 48.0).max(240.0);
        let h = 172.0;
        let ctr = self.rect.center();
        let card = Rect::new(
            ctr.x - w / 2.0,
            ctr.y - h / 2.0,
            ctr.x + w / 2.0,
            ctr.y + h / 2.0,
        );
        let by1 = card.y1 - 20.0;
        let by0 = by1 - 32.0;
        let save = Rect::new(card.x1 - 20.0 - 110.0, by0, card.x1 - 20.0, by1);
        let discard = Rect::new(save.x0 - 10.0 - 104.0, by0, save.x0 - 10.0, by1);
        let cancel = Rect::new(discard.x0 - 10.0 - 84.0, by0, discard.x0 - 10.0, by1);
        (card, cancel, discard, save)
    }

    /// Ask before an Open replaces a project that exists nowhere else.
    fn draw_discard_dialog(
        &self,
        scene: &mut Scene,
        tf: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
    ) {
        let Some(pending) = &self.pending_open else {
            return;
        };
        let c = &theme.tokens.colors;
        fill_rect(
            scene,
            tf,
            with_alpha(c.surfaces.scrim, MODAL_SCRIM_ALPHA),
            self.rect,
        );
        let (card, cancel, discard, save) = self.discard_layout();
        fill_rrect(scene, tf, c.surfaces.raised, card, 14.0);
        stroke_rrect(scene, tf, c.border_standard, card, 14.0, 1.0);
        let x0 = card.x0 + 24.0;
        text_w(
            scene,
            tr,
            tf,
            &format!("Save “{}” first?", pending.losing),
            17.0,
            c.text_primary,
            x0,
            card.y0 + 22.0,
            TextWeight::Bold,
        );
        // Say what is true: the work is not gone if they open anyway, because
        // the open is undoable. A dialog that overstates the danger trains
        // people to click through it.
        for (i, line) in [
            "It has never been saved to a file.".to_string(),
            format!(
                "Opening {} replaces it. Ctrl+Z brings it back.",
                crate::project_file::recents::label(&pending.path)
            ),
        ]
        .iter()
        .enumerate()
        {
            let shown = elide_at(line, (card.width() - 48.0) as f32, 12.5, tr);
            text(
                scene,
                tr,
                tf,
                &shown,
                12.5,
                c.text_dim,
                x0,
                card.y0 + 56.0 + i as f64 * 20.0,
            );
        }
        draw_button(
            scene,
            tf,
            tr,
            theme,
            cancel,
            "Cancel",
            false,
            cancel.contains(self.last_mouse),
        );
        draw_button(
            scene,
            tf,
            tr,
            theme,
            discard,
            "Open anyway",
            false,
            discard.contains(self.last_mouse),
        );
        draw_button(
            scene,
            tf,
            tr,
            theme,
            save,
            "Save…",
            true,
            save.contains(self.last_mouse),
        );
    }

    /// Three-bar hamburger that summons the collapsed media bin. Highlights
    /// while the flap is open so it reads as a pressed toggle.
    fn draw_hamburger(&self, scene: &mut Scene, tf: Affine, theme: &Theme, rect: Rect) {
        if rect.width() <= 0.0 {
            return;
        }
        let c = &theme.tokens.colors;
        let open = self.bin_flap.is_open(self.bin_layout());
        let hovered = rect.contains(self.last_mouse);
        let bg = if open {
            c.accent[5]
        } else if hovered {
            c.lift(0.72)
        } else {
            c.neutral[3]
        };
        fill_rrect(scene, tf, bg, rect, 7.0);
        let bar = if open {
            on_fill(theme, bg).1
        } else {
            c.text_primary
        };
        let bx0 = rect.x0 + 7.0;
        let bx1 = rect.x1 - 7.0;
        for i in 0..3 {
            let by = rect.center().y - 5.0 + f64::from(i) * 5.0;
            fill_rrect(scene, tf, bar, Rect::new(bx0, by - 1.0, bx1, by + 1.0), 1.0);
        }
    }

    fn draw_bin_header(&self, ctx: &mut BinPaintCtx<'_>, r: &Regions) {
        let c = &ctx.theme.tokens.colors;
        fill_rect(ctx.scene, ctx.transform, c.surfaces.raised, r.bin);
        stroke_line(
            ctx.scene,
            ctx.transform,
            c.border_standard,
            r.bin.x1,
            r.bin.y0,
            r.bin.x1,
            r.bin.y1,
            1.0,
        );
        text_w(
            ctx.scene,
            ctx.text,
            ctx.transform,
            "MEDIA BIN",
            11.0,
            c.text_dim,
            r.bin.x0 + 14.0 * ctx.scale,
            r.bin.y0 + 14.0 * ctx.scale,
            TextWeight::Semibold,
        );
        let add = self.add_title_rect(r);
        let add_hover = add.contains(self.last_mouse);
        fill_rrect(
            ctx.scene,
            ctx.transform,
            if add_hover {
                c.lift(0.72)
            } else {
                c.neutral[3]
            },
            add,
            5.0,
        );
        stroke_rrect(ctx.scene, ctx.transform, c.border_standard, add, 5.0, 1.0);
        let at = text_centre(add, "+ Title", 10.5, TextWeight::Medium, ctx.text);
        text_w(
            ctx.scene,
            ctx.text,
            ctx.transform,
            "+ Title",
            10.5,
            c.text_primary,
            at.x,
            at.y,
            TextWeight::Medium,
        );
        let count = format!("{} clips", self.project.media.len());
        let (count_width, _) = ctx.text.measure(&count, 11.0, None);
        let count_x = r.bin.x1 - 14.0 * ctx.scale - f64::from(count_width);
        if count_x > add.x1 + 8.0 * ctx.scale {
            text(
                ctx.scene,
                ctx.text,
                ctx.transform,
                &count,
                11.0,
                c.text_dim,
                count_x,
                r.bin.y0 + 14.0,
            );
        }
    }

    fn draw_bin_row_visual(
        &self,
        ctx: &mut BinPaintCtx<'_>,
        media_id: MediaId,
        media: &MediaItem,
        rect: Rect,
    ) -> Rect {
        let colors = &ctx.theme.tokens.colors;
        if self.selected_media == Some(media_id) {
            fill_rrect(
                ctx.scene,
                ctx.transform,
                with_alpha(colors.accent[5], 0.22),
                rect,
                8.0,
            );
            stroke_rrect(ctx.scene, ctx.transform, colors.accent[5], rect, 8.0, 1.5);
        } else if self.hover_media == Some(media_id) {
            fill_rrect(ctx.scene, ctx.transform, colors.lift(0.35), rect, 8.0);
        }

        let row_inset = 8.0 * ctx.scale;
        let thumb_height = (rect.height() - 2.0 * row_inset).clamp(0.0, 64.0);
        let thumb_y0 = rect.center().y - thumb_height / 2.0;
        let thumb = Rect::new(
            rect.x0 + row_inset,
            thumb_y0,
            rect.x0 + row_inset + 64.0,
            thumb_y0 + thumb_height,
        );
        let base = clip_color(ctx.theme, media.palette);
        fill_rrect(ctx.scene, ctx.transform, darken(base, 0.25), thumb, 5.0);
        if media.kind.is_visual() {
            if let Some(thumbnail) = self.thumbnails.get(media_id) {
                blit_thumb(ctx.scene, ctx.transform, thumbnail, thumb, 5.0);
            } else {
                for index in 0..3 {
                    let x = thumb.x0 + 6.0 + f64::from(index) * 18.0;
                    let block = Rect::new(x, thumb.y0 + 6.0, x + 14.0, thumb.y1 - 6.0);
                    fill_rrect(ctx.scene, ctx.transform, lighten(base, 0.18), block, 2.0);
                }
            }
        } else {
            let color = lighten(base, 0.25);
            let inner = Rect::new(
                thumb.x0 + 4.0,
                thumb.y0 + 6.0,
                thumb.x1 - 4.0,
                thumb.y1 - 6.0,
            );
            match (
                self.waveforms.state(media.id),
                self.waveforms.peaks(media.id),
            ) {
                (crate::waveform::WaveformState::Ready, Some(peaks)) => draw_waveform(
                    ctx.scene,
                    ctx.transform,
                    WaveformPaint {
                        area: inner,
                        peaks,
                        source_in: 0.0,
                        source_out: media.duration,
                        media_duration: media.duration,
                        volume: 1.0,
                        color,
                    },
                ),
                _ => draw_flat_signal(ctx.scene, ctx.transform, inner, color),
            }
        }

        let pending = if media.kind.is_visual() {
            self.thumbnails.get(media_id).is_none()
        } else {
            matches!(
                self.waveforms.state(media.id),
                crate::waveform::WaveformState::Pending
                    | crate::waveform::WaveformState::Unrequested
            )
        };
        if pending {
            for index in 0..3 {
                let x = thumb.x0 + 10.0 + f64::from(index) * 8.0;
                ctx.scene.fill(
                    Fill::NonZero,
                    ctx.transform,
                    with_alpha(Color::WHITE, 0.35 + 0.2 * f64::from(index)),
                    None,
                    &Circle::new(Point::new(x, thumb.y1 - 8.0), 2.5),
                );
            }
        }
        let kind_color = match media.kind {
            MediaKind::Video => colors.semantic.info,
            MediaKind::Audio => colors.semantic.success,
            MediaKind::Image => colors.semantic.warning,
            MediaKind::Text => colors.accent[5],
        };
        ctx.scene.fill(
            Fill::NonZero,
            ctx.transform,
            kind_color,
            None,
            &Circle::new(Point::new(thumb.x1 - 8.0, thumb.y0 + 8.0), 4.0),
        );
        thumb
    }

    fn draw_bin_row_text(
        &self,
        ctx: &mut BinPaintCtx<'_>,
        media_id: MediaId,
        media: &MediaItem,
        rect: Rect,
        thumb: Rect,
    ) {
        let colors = &ctx.theme.tokens.colors;
        let text_x = thumb.x1 + 12.0 * ctx.scale;
        let renaming = self.bin_rename.as_ref().map(|(id, _)| *id) == Some(media_id);
        if !renaming {
            let available = (rect.x1 - 8.0 - text_x) as f32;
            let shown = elide_w(&media.name, available, ctx.text);
            text_w(
                ctx.scene,
                ctx.text,
                ctx.transform,
                &shown,
                13.0,
                colors.text_primary,
                text_x,
                rect.y0 + 10.0 * ctx.scale,
                TextWeight::Medium,
            );
        }
        let subtitle = self.bin_subtitle(media);
        let (long, short) = if self.media_is_unreadable(media_id) {
            (Some(MEDIA_UNREADABLE), Some(MEDIA_UNREADABLE_SHORT))
        } else if self.media_is_projected(media_id) {
            (Some(MEDIA_NOT_LOCAL), Some(MEDIA_NOT_LOCAL_SHORT))
        } else {
            (None, None)
        };
        let (subtitle_width, _) = ctx.text.measure(&subtitle, 11.0, None);
        let right = rect.x1 - 8.0 * ctx.scale;
        let chip = long.zip(short).and_then(|(long, short)| {
            [
                (text_x + f64::from(subtitle_width) + 8.0, long),
                (text_x + f64::from(subtitle_width) + 8.0, short),
                (text_x, long),
                (text_x, short),
            ]
            .into_iter()
            .find(|(x0, label)| {
                x0 + f64::from(ctx.text.measure(label, 10.5, None).0) + 14.0 <= right
            })
        });
        if chip.is_none_or(|(x0, _)| x0 > text_x) {
            text(
                ctx.scene,
                ctx.text,
                ctx.transform,
                &subtitle,
                11.0,
                colors.text_dim,
                text_x,
                rect.y0 + 34.0 * ctx.scale,
            );
        }
        if let Some((x0, label)) = chip {
            let width = f64::from(ctx.text.measure(label, 10.5, None).0) + 14.0 * ctx.scale;
            let top = rect.y0 + 31.0 * ctx.scale;
            let chip = Rect::new(x0, top, x0 + width, top + 17.0 * ctx.scale);
            fill_rrect(
                ctx.scene,
                ctx.transform,
                with_alpha(colors.semantic.warning, 0.20),
                chip,
                4.0,
            );
            stroke_rrect(
                ctx.scene,
                ctx.transform,
                with_alpha(colors.semantic.warning, 0.65),
                chip,
                4.0,
                1.0,
            );
            let baseline = text_baseline(chip, 10.5, ctx.text);
            text(
                ctx.scene,
                ctx.text,
                ctx.transform,
                label,
                10.5,
                colors.semantic.warning,
                chip.x0 + 7.0 * ctx.scale,
                baseline,
            );
        }
    }

    fn draw_bin_rows(&self, ctx: &mut BinPaintCtx<'_>, r: &Regions) {
        for (media_id, rect) in self.bin_item_rects(r) {
            let Some(media) = self.project.media(media_id) else {
                continue;
            };
            let thumb = self.draw_bin_row_visual(ctx, media_id, media, rect);
            self.draw_bin_row_text(ctx, media_id, media, rect, thumb);
        }
        if self.shows_focus_cursor(Pane::Bin)
            && let Some((_, row)) = self.bin_item_rects(r).get(self.focus.bin)
        {
            draw_focus_ring(
                ctx.scene,
                ctx.transform,
                ctx.theme,
                row.inset(-FOCUS_RING_INSET),
                8.0 - FOCUS_RING_INSET,
            );
        }
    }

    fn draw_bin(
        &self,
        scene: &mut Scene,
        tf: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        r: &Regions,
    ) {
        let bin_scale = self.bin_ui_scale();
        self.draw_bin_header(
            &mut BinPaintCtx {
                scene,
                transform: tf,
                text: tr,
                theme,
                scale: bin_scale,
            },
            r,
        );
        let c = &theme.tokens.colors;

        // Clip bin content so a long list can't bleed over the timeline.
        scene.push_layer(
            Fill::NonZero,
            vello::peniko::BlendMode::default(),
            1.0,
            tf,
            &Rect::new(r.bin.x0, r.bin.y0 + 30.0, r.bin.x1, r.bin.y1),
        );
        if self.project.media.is_empty() {
            // Empty state: a real "Import media" affordance (the button opens the
            // host file picker) plus an honest status line.
            let cx = r.bin.center().x;
            let cy = r.bin.y0 + 120.0;
            let line1 = "No media yet";
            let (w1, _) = tr.measure(line1, 14.0, None);
            text(
                scene,
                tr,
                tf,
                line1,
                14.0,
                c.text_primary,
                cx - f64::from(w1) / 2.0,
                cy,
            );
            if let Some(btn) = self.empty_bin_import_rect(r) {
                draw_button(
                    scene,
                    tf,
                    tr,
                    theme,
                    btn,
                    "Import media",
                    true,
                    btn.contains(self.last_mouse),
                );
            }
        }
        self.draw_bin_rows(
            &mut BinPaintCtx {
                scene,
                transform: tf,
                text: tr,
                theme,
                scale: bin_scale,
            },
            r,
        );
        self.draw_scrollbar(scene, tf, theme, ScrollPane::Bin, r);
        scene.pop_layer();
        // Last import outcome, pinned to the foot of the pane. It used to be
        // drawn only inside the empty-bin branch, so once the bin held
        // anything at all every import error was silent — six sites wrote it
        // and one read it. Errors tint danger; a plain count stays quiet.
        if let Some(notice) = &self.import_notice {
            let failed = notice.is_failure();
            let strip = Rect::new(r.bin.x0, r.bin.y1 - 34.0, r.bin.x1, r.bin.y1);
            // The severity came with the message from the place that knew the
            // outcome — a portal refusal is tinted as a refusal even though
            // its wording looks nothing like a probe's.
            let tint = notice.level.tint(theme);
            fill_rect(scene, tf, with_alpha(tint, 0.16), strip);
            stroke_line(
                scene,
                tf,
                with_alpha(tint, 0.5),
                strip.x0,
                strip.y0,
                strip.x1,
                strip.y0,
                1.0,
            );
            let col = if failed {
                c.semantic.danger
            } else {
                c.text_primary
            };
            let shown = elide_w(&notice.text, (strip.width() - 24.0) as f32, tr);
            let baseline = text_baseline(strip, 11.0, tr);
            text(scene, tr, tf, &shown, 11.0, col, strip.x0 + 12.0, baseline);
        }
    }

    /// Draw feedback for a live bin→timeline drag: highlight the drop lane, mark
    /// the insertion point at the drop time, and float a ghost chip at the cursor.
    fn draw_bin_drag(
        &self,
        scene: &mut Scene,
        tf: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        r: &Regions,
    ) {
        let Some(bd) = self.bin_drag else { return };
        if !bd.active {
            return;
        }
        let c = &theme.tokens.colors;
        let p = self.last_mouse;
        // Drop-lane highlight + insertion marker at the snapped drop time.
        if let Some((_, _, lane)) = self.bin_drop_lane(r) {
            fill_rect(
                scene,
                tf,
                with_alpha(c.accent[5], 0.16),
                Rect::new(r.lanes.x0, lane.y0, r.lanes.x1, lane.y1),
            );
            let t = snap_to_frame(self.x_to_time(p.x, &r.lanes), self.project.fps);
            let x = self.time_to_x(t, &r.lanes).clamp(r.lanes.x0, r.lanes.x1);
            stroke_line(scene, tf, c.accent[5], x, lane.y0, x, lane.y1, 2.0);
        }
        // Ghost chip following the cursor.
        if let Some(m) = self.project.media(bd.media) {
            let (w, h) = (154.0, 30.0);
            let gr = Rect::new(p.x + 12.0, p.y - h / 2.0, p.x + 12.0 + w, p.y + h / 2.0);
            let ghost_fill = clip_color(theme, m.palette);
            fill_rrect(scene, tf, with_alpha(ghost_fill, 0.92), gr, 6.0);
            stroke_rrect(scene, tf, with_alpha(c.border_focus, 0.9), gr, 6.0, 1.0);
            let ghost_label = elide_w(&m.name, (gr.width() - 20.0) as f32, tr);
            let ghost_y = text_baseline(gr, 12.0, tr);
            text_w(
                scene,
                tr,
                tf,
                &ghost_label,
                12.0,
                on_fill(theme, ghost_fill).1,
                gr.x0 + 10.0,
                ghost_y,
                TextWeight::Medium,
            );
        }
    }

    /// Draw the in-flight inline rename field over its bin row's name band.
    fn draw_bin_rename(
        &mut self,
        scene: &mut Scene,
        tf: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
        r: &Regions,
    ) {
        let mid = match &self.bin_rename {
            Some((m, _)) => *m,
            None => return,
        };
        let bin_visible = self.bin_is_visible(r);
        if !bin_visible {
            return;
        }
        let Some(row) = self
            .bin_item_rects(r)
            .into_iter()
            .find(|(m, _)| *m == mid)
            .map(|(_, row)| row)
        else {
            return;
        };
        let rect = self.bin_name_rect(row);
        if let Some((_, label)) = self.bin_rename.as_mut() {
            label.apply_layout_external(rect);
            label.draw(scene, tf, tr, theme, presence);
        }
    }

    fn draw_preview(
        &self,
        scene: &mut Scene,
        tf: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        r: &Regions,
    ) {
        let c = &theme.tokens.colors;
        fill_rect(scene, tf, c.surfaces.base, r.preview);
        let canvas = r.canvas;
        // A short window crushes the letterboxed canvas to a few pixels. There
        // is nothing to show at that size, and the HUD and safe-area guide
        // drawn at fixed offsets would land outside it; say so instead.
        if canvas.width() < MIN_CANVAS || canvas.height() < MIN_CANVAS {
            let msg = "Not enough room for the preview";
            let (w, _) = tr.measure(msg, 11.5, None);
            let area = Rect::new(r.preview.x0, r.preview.y0, r.preview.x1, r.transport.y0);
            if area.height() > 16.0 {
                text(
                    scene,
                    tr,
                    tf,
                    msg,
                    11.5,
                    c.text_dim,
                    area.center().x - w as f64 / 2.0,
                    area.center().y - 7.0,
                );
            }
            return;
        }
        // Everything below is clipped to the preview pane, so an overlay drawn
        // at a fixed offset from the canvas can never bleed into the bin, the
        // inspector or the transport.
        scene.push_layer(
            Fill::NonZero,
            vello::peniko::BlendMode::default(),
            1.0,
            tf,
            &Rect::new(r.preview.x0, r.preview.y0, r.preview.x1, r.transport.y0),
        );
        // Canvas backdrop (letterbox black).
        fill_rect(scene, tf, Color::BLACK, canvas);
        // One picture, produced by the GPU compositor in `crate::gpu` — the
        // *same* renderer the export runs, so the preview is byte-for-byte
        // what the export writes. It resolves every layer, grade, fade,
        // transition and effect itself; nothing here re-implements any of it.
        // The HUD below still draws on top, which is why this reads the frame
        // back and blits it rather than hooking the render target.
        let showed_real = match &self.composited {
            Some((w, h, rgba)) if *w > 0 && *h > 0 => {
                let frame = ::media::VideoFrame {
                    width: *w,
                    height: *h,
                    rgba: rgba.clone(),
                    pts_ms: 0,
                };
                blit_frame(scene, tf, &frame, canvas);
                true
            }
            _ => false,
        };
        if !showed_real {
            // Nothing was composited, so there is no picture — and with no
            // picture the letterbox black is not standing for anything. Repaint
            // the canvas in the theme's own surface so the words below are
            // legible: theme text on a hard-coded black is dark-on-dark in
            // every light theme, which is how "No clip at playhead" came to be
            // near-invisible on a white desktop.
            fill_rect(scene, tf, c.surfaces.base, canvas);
            stroke_rrect(scene, tf, c.border_standard, canvas, 2.0, 1.0);
            // Nothing to composite, or nothing to composite *with*. Both are
            // said in words rather than papered over with an invented picture,
            // and both go through the shared empty state so the preview reads
            // like every other empty surface in the fleet instead of like a
            // stray caption on a black rectangle.
            let (icon, title, hint) = if self.project.top_video_clip_at(self.playhead).is_none() {
                (
                    ui::icons::Icon::Film,
                    PREVIEW_EMPTY_TITLE,
                    PREVIEW_EMPTY_HINT,
                )
            } else {
                // Not an empty state — a capability the box does not have.
                // Same treatment, different words, and it must not suggest
                // there is anything to do about it here.
                (
                    ui::icons::Icon::Monitor,
                    "Preview needs a GPU adapter",
                    "The timeline and export still work",
                )
            };
            ui::widget::EmptyState::paint(
                scene,
                tf,
                tr,
                theme,
                canvas,
                Some(icon),
                title,
                Some(hint),
                None,
                Point::new(f64::MIN, f64::MIN),
            );
        }
        // HUD overlays: the playhead's time, and nothing else.
        //
        // The frame size used to be painted in the canvas's top-right, which
        // is exactly where a corner picture-in-picture sits — and now that
        // `Transform` is real and `Transform::pip()` puts a layer there by
        // design, that collision is systematic rather than unlucky. Frame size
        // is static project metadata, so it belongs beside the timecode in the
        // transport (which states it) and in the export dialog, not painted
        // over the user's composition for the life of the project.
        //
        // What stays is per-frame information, and it takes a chip so it reads
        // over a bright picture as well as a dark one.
        let tc = timecode(self.playhead, self.project.fps);
        let (tcw, _) = tr.measure_weighted(&tc, 13.0, None, TextWeight::Bold);
        let tc_chip = Rect::new(
            canvas.x0 + 8.0,
            canvas.y0 + 6.0,
            canvas.x0 + 8.0 + tcw as f64 + 14.0,
            canvas.y0 + 28.0,
        );
        if tc_chip.x1 < canvas.x1 {
            fill_rrect(scene, tf, with_alpha(Color::BLACK, 0.55), tc_chip, 5.0);
            let baseline = text_baseline(tc_chip, 13.0, tr);
            text_w(
                scene,
                tr,
                tf,
                &tc,
                13.0,
                Color::WHITE,
                tc_chip.x0 + 7.0,
                baseline,
                TextWeight::Bold,
            );
        }
        // The clip under the playhead has no bytes on this device: say so on the
        // canvas, because the picture behind this line is a stand-in generated
        // from metadata, not the user's footage.
        if let Some(clip) = self.project.top_video_clip_at(self.playhead) {
            let unusable = if self.media_is_unreadable(clip.media) {
                Some(MEDIA_UNREADABLE)
            } else if self.media_is_projected(clip.media) {
                Some(MEDIA_NOT_LOCAL)
            } else {
                None
            };
            if let Some(msg) = unusable {
                let (mw, _) = tr.measure_weighted(msg, 12.0, None, TextWeight::Semibold);
                let pad = 8.0;
                let chip = Rect::new(
                    canvas.x0 + 10.0,
                    canvas.y1 - 32.0,
                    canvas.x0 + 10.0 + mw as f64 + pad * 2.0,
                    canvas.y1 - 10.0,
                );
                fill_rrect(scene, tf, with_alpha(Color::BLACK, 0.62), chip, 5.0);
                stroke_rrect(scene, tf, with_alpha(Color::WHITE, 0.35), chip, 5.0, 1.0);
                let baseline = text_baseline(chip, 12.0, tr);
                text_w(
                    scene,
                    tr,
                    tf,
                    msg,
                    12.0,
                    Color::WHITE,
                    chip.x0 + pad,
                    baseline,
                    TextWeight::Semibold,
                );
            }
        }
        // Safe-area guide.
        stroke_rrect(
            scene,
            tf,
            with_alpha(Color::WHITE, 0.12),
            canvas.inset(-canvas.width() * 0.05),
            0.0,
            1.0,
        );
        scene.pop_layer();
    }

    #[allow(
        clippy::too_many_lines,
        reason = "transport controls, scaled time readouts, and scrubber share one collision-free composition"
    )]
    fn draw_transport(
        &self,
        scene: &mut Scene,
        tf: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        r: &Regions,
    ) {
        let c = &theme.tokens.colors;
        let chrome_scale = self.chrome_scale();
        fill_rect(scene, tf, c.surfaces.raised, r.transport);
        stroke_line(
            scene,
            tf,
            c.border_standard,
            r.transport.x0,
            r.transport.y0,
            r.transport.x1,
            r.transport.y0,
            1.0,
        );
        // Everything below is clipped to the transport, so no control drawn
        // at a fixed offset can reach into the pane beside it.
        scene.push_layer(
            Fill::NonZero,
            vello::peniko::BlendMode::default(),
            1.0,
            tf,
            &r.transport,
        );
        let cursor = self.shows_focus_cursor(Pane::Transport);
        for (i, (action, rect)) in self.transport_buttons(r).into_iter().enumerate() {
            let is_play = action == TransportAction::PlayPause;
            let hovered = rect.contains(self.last_mouse);
            let bg = if is_play {
                c.accent[5]
            } else if hovered {
                c.lift(0.72)
            } else {
                c.neutral[3]
            };
            scene.fill(
                Fill::NonZero,
                tf,
                bg,
                None,
                &Circle::new(rect.center(), rect.width() / 2.0),
            );
            let col = if is_play {
                on_fill(theme, bg).1
            } else {
                c.text_primary
            };
            self.draw_transport_icon(scene, tf, action, rect.center(), col);
            // The keyboard cursor. `focus.transport` indexes `TRANSPORT_NAMES`,
            // which is this array's order, so the ring lands on the button a
            // reader is told it is on.
            if cursor && self.focus.transport == i {
                draw_focus_ring_circle(scene, tf, theme, rect.center(), rect.width() / 2.0);
            }
        }
        // Undo / redo. Ghost squares rather than the transport's filled
        // circles: nothing here moves the playhead, and the difference in
        // shape is what says so before the icon is read.
        for (which, rect) in self.history_buttons(r) {
            let can = self.undo.status().can(which);
            let icon = match which {
                ui::UndoRequest::Undo => ui::icons::Icon::Undo2,
                ui::UndoRequest::Redo => ui::icons::Icon::Redo2,
            };
            let face = ui::widget::ButtonFace::new(rect, "")
                .ghost()
                .icon(icon)
                .radius(6.0)
                .accessible_name(which.title())
                // A control that cannot act must not light up under the
                // pointer — a hover state on a dead button reads as one
                // that is about to work.
                .hovered(rect.contains(self.last_mouse) && can)
                .disabled(!can);
            ui::widget::Button::paint(scene, tf, tr, theme, &face);
        }
        // Timecode readout. Each part is drawn only when it fits *whole*
        // inside the transport: a number the pane edge cuts mid-digit does not
        // read as a clipped number, it reads as a shorter one — "00:42:00"
        // arriving as "00:4" states a project length that is simply false.
        // Dropping the part is the honest narrow-window answer, and the parts
        // go in order of what the user cannot get elsewhere.
        let big = timecode(self.playhead, self.project.fps);
        let total = format!("/  {}", timecode(self.project.duration(), self.project.fps));
        // Project format — the app's own content area, rather than the OS's
        // title slot. This is the only place the frame size is stated now that
        // the canvas HUD no longer paints it over the picture.
        let meta = format!(
            "{}×{} · {:.0} fps",
            self.project.width, self.project.height, self.project.fps
        );
        // What the playhead is *doing* — playing, or shuttling in some
        // direction at some rate. It shares the readout's line rather than the
        // 16px band above the scrubber the old "PLAYING" used, which on a 58px
        // strip sits inside the 18px timecode's own line box.
        let motion = self.motion_words();
        let (bw, _) = tr.measure_weighted(&big, 18.0, None, TextWeight::Bold);
        let (tw, _) = tr.measure(&total, 13.0, None);
        let (mw, _) = tr.measure(&meta, 11.5, None);
        let motion_w = motion
            .as_deref()
            .map_or(0.0, |s| f64::from(tr.measure(s, 10.0, None).0));
        let fit = readout_fit(
            r.transport,
            self.transport_content_x(r),
            bw as f64,
            tw as f64,
            motion_w,
            mw as f64,
        );
        if let Some(x) = fit.big_x {
            text_w(
                scene,
                tr,
                tf,
                &big,
                18.0,
                c.text_primary,
                x,
                r.transport.y0 + 8.0 * chrome_scale,
                TextWeight::Bold,
            );
        }
        if let Some(x) = fit.total_x {
            text(
                scene,
                tr,
                tf,
                &total,
                13.0,
                c.text_dim,
                x,
                r.transport.y0 + 13.0 * chrome_scale,
            );
        }
        // Playing is reported in the success tint it always was; a shuttle
        // takes the accent, so the two states are not one word apart. This used
        // to be gated on `playing` alone, which meant a shuttle running at 16×
        // reported itself nowhere at all: the playhead moved under its own
        // steam with nothing on screen naming why, or how fast, or which way.
        if let (Some(x), Some(words)) = (fit.motion_x, motion.as_deref()) {
            let tint = if self.playback.playing {
                c.semantic.success
            } else {
                c.accent[5]
            };
            text(
                scene,
                tr,
                tf,
                words,
                10.0,
                tint,
                x,
                r.transport.y0 + 13.0 * chrome_scale,
            );
        }
        if let Some(x) = fit.meta_x {
            text(
                scene,
                tr,
                tf,
                &meta,
                11.5,
                c.text_dim,
                x,
                r.transport.y0 + 14.0 * chrome_scale,
            );
        }
        // Scrubber.
        let scrub = self.scrubber_rect(r);
        if scrub.width() <= 1.0 {
            scene.pop_layer();
            return;
        }
        fill_rrect(scene, tf, c.neutral[3], scrub, 4.0);
        let frac = (self.playhead / self.project.duration()).clamp(0.0, 1.0);
        let filled = Rect::new(
            scrub.x0,
            scrub.y0,
            scrub.x0 + scrub.width() * frac,
            scrub.y1,
        );
        fill_rrect(scene, tf, c.accent[5], filled, 4.0);
        let knob = Circle::new(Point::new(filled.x1, scrub.center().y), 6.0 * chrome_scale);
        scene.fill(Fill::NonZero, tf, c.surfaces.raised, None, &knob);
        scene.stroke(&Stroke::new(1.5), tf, c.border_focus, None, &knob);
        scene.pop_layer();
    }

    fn draw_transport_icon(
        &self,
        scene: &mut Scene,
        tf: Affine,
        action: TransportAction,
        ctr: Point,
        col: Color,
    ) {
        let s = 6.0;
        match action {
            TransportAction::PlayPause => {
                // Pause bars whenever the playhead is running, shuttle
                // included: a play triangle over a moving playhead offers to
                // start something that is already going.
                if self.is_moving() {
                    fill_rect(
                        scene,
                        tf,
                        col,
                        Rect::new(ctr.x - 5.0, ctr.y - 6.0, ctr.x - 1.0, ctr.y + 6.0),
                    );
                    fill_rect(
                        scene,
                        tf,
                        col,
                        Rect::new(ctr.x + 1.0, ctr.y - 6.0, ctr.x + 5.0, ctr.y + 6.0),
                    );
                } else {
                    let mut p = BezPath::new();
                    p.move_to((ctr.x - 5.0, ctr.y - 7.0));
                    p.line_to((ctr.x + 7.0, ctr.y));
                    p.line_to((ctr.x - 5.0, ctr.y + 7.0));
                    p.close_path();
                    scene.fill(Fill::NonZero, tf, col, None, &p);
                }
            }
            // Step-by-frame is a *double* chevron with no bar. Jump-to-end is
            // a single chevron against a bar. They used to differ by one pixel
            // of bar position and read as the same button twice.
            TransportAction::StepBack => {
                for dx in [1.0, 7.0] {
                    let mut p = BezPath::new();
                    p.move_to((ctr.x - dx + s, ctr.y - s));
                    p.line_to((ctr.x - dx, ctr.y));
                    p.line_to((ctr.x - dx + s, ctr.y + s));
                    p.close_path();
                    scene.fill(Fill::NonZero, tf, col, None, &p);
                }
            }
            TransportAction::StepFwd => {
                for dx in [1.0, 7.0] {
                    let mut p = BezPath::new();
                    p.move_to((ctr.x + dx - s, ctr.y - s));
                    p.line_to((ctr.x + dx, ctr.y));
                    p.line_to((ctr.x + dx - s, ctr.y + s));
                    p.close_path();
                    scene.fill(Fill::NonZero, tf, col, None, &p);
                }
            }
            TransportAction::ToStart => {
                fill_rect(
                    scene,
                    tf,
                    col,
                    Rect::new(ctr.x - 7.0, ctr.y - s, ctr.x - 4.5, ctr.y + s),
                );
                let mut p = BezPath::new();
                p.move_to((ctr.x + 7.0, ctr.y - s));
                p.line_to((ctr.x - 3.0, ctr.y));
                p.line_to((ctr.x + 7.0, ctr.y + s));
                p.close_path();
                scene.fill(Fill::NonZero, tf, col, None, &p);
            }
            TransportAction::ToEnd => {
                fill_rect(
                    scene,
                    tf,
                    col,
                    Rect::new(ctr.x + 4.5, ctr.y - s, ctr.x + 7.0, ctr.y + s),
                );
                let mut p = BezPath::new();
                p.move_to((ctr.x - 7.0, ctr.y - s));
                p.line_to((ctr.x + 3.0, ctr.y));
                p.line_to((ctr.x - 7.0, ctr.y + s));
                p.close_path();
                scene.fill(Fill::NonZero, tf, col, None, &p);
            }
        }
    }

    fn draw_inspector(
        &self,
        scene: &mut Scene,
        tf: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        r: &Regions,
    ) {
        let c = &theme.tokens.colors;
        fill_rect(scene, tf, c.surfaces.raised, r.inspector);
        stroke_line(
            scene,
            tf,
            c.border_standard,
            r.inspector.x0,
            r.inspector.y0,
            r.inspector.x0,
            r.inspector.y1,
            1.0,
        );
        // Clip everything to the inspector rect so controls never bleed into
        // the timeline if the selected clip's control stack runs tall.
        scene.push_layer(
            Fill::NonZero,
            vello::peniko::BlendMode::default(),
            1.0,
            tf,
            &r.inspector,
        );
        // The scroll body gets its own clip, below the fixed identity card.
        // Painting an opaque header over it is not enough: antialiased glyph
        // edges and controls halfway through a translate can otherwise still
        // show through at the seam.
        let body_view = self.inspector_viewport(r);
        scene.push_layer(
            Fill::NonZero,
            vello::peniko::BlendMode::default(),
            1.0,
            tf,
            &body_view,
        );
        self.draw_inspector_body(scene, tf, tr, theme, r);
        scene.pop_layer();
        // The body may be translated all the way to its final action row.
        // Paint the persistent context last, over that translated content, so
        // a clipped control can never masquerade as part of the header.
        self.draw_inspector_fixed_header(scene, tf, tr, theme, r);
        // A clip on a locked track has inert controls — every edit path checks
        // the lock — so they are veiled rather than left looking live. The
        // header above stays crisp, because that is where the reason is
        // written ("on V2 · locked").
        let locked = self
            .selected()
            .map(|id| !self.project.clip_editable(id))
            .unwrap_or(false);
        if locked {
            let l = self.inspector_layout(r);
            let top = l
                .opacity
                .or(l.volume)
                .map(|t| t.y0 - 24.0)
                .unwrap_or(r.inspector.y0 + 170.0);
            let veil = Rect::new(
                r.inspector.x0,
                top.max(r.inspector.y0),
                r.inspector.x1,
                r.inspector.y1,
            );
            fill_rect(scene, tf, with_alpha(c.surfaces.raised, 0.6), veil);
        }
        // The keyboard cursor, drawn over the lock veil rather than under it:
        // where the keyboard *is* stays legible even where the controls are
        // inert, because the walk still visits them. Inside the pane's clip
        // layer, so a row scrolled out of view keeps its ring off the timeline.
        if self.shows_focus_cursor(Pane::Inspector) {
            let rows = self.a11y_inspector_rows();
            if let Some(row) = rows.get(self.focus.inspector).copied()
                && let Some(band) = Self::inspector_focus_band(&self.inspector_layout(r), row)
                && band.intersect(self.inspector_viewport(r)).area() > 0.0
            {
                draw_focus_ring(scene, tf, theme, band, 5.0);
            }
        }
        // Soft edges where the pane cuts its content. A row sliced in half by
        // a hard boundary reads as a broken control; the same row fading out
        // reads as "this scrolls", which is what it is. Only drawn on the edge
        // that actually has content beyond it.
        let max = self.inspector_max_scroll(r);
        let fade = |scene: &mut Scene, from_y: f64, dir: f64| {
            const STEPS: i32 = 14;
            const H: f64 = 22.0;
            for i in 0..STEPS {
                let f = i as f64 / STEPS as f64;
                let y0 = from_y + dir * H * f;
                let y1 = y0 + dir * (H / STEPS as f64);
                let band = Rect::new(r.inspector.x0, y0.min(y1), r.inspector.x1, y0.max(y1));
                fill_rect(
                    scene,
                    tf,
                    with_alpha(c.surfaces.raised, 0.10 + 0.9 * f),
                    band,
                );
            }
        };
        if self.inspector_scroll > 0.5 {
            fade(scene, self.inspector_viewport(r).y0, 1.0);
        }
        if self.inspector_scroll < max - 0.5 {
            fade(scene, self.inspector_viewport(r).y1, -1.0);
        }
        scene.pop_layer();
        // Real, grabbable scrollbar. What stood here was a painted indicator
        // with no hit-test, so the EFFECTS section below the fold could be
        // seen to exist and never reached.
        self.draw_scrollbar(scene, tf, theme, ScrollPane::Inspector, r);
    }

    #[allow(
        clippy::too_many_lines,
        reason = "the fixed identity card's measured text, clip metadata, and viewport boundary must be composed together"
    )]
    fn draw_inspector_fixed_header(
        &self,
        scene: &mut Scene,
        tf: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        r: &Regions,
    ) {
        let c = &theme.tokens.colors;
        let s = self.inspector_ui_scale();
        let header_bottom = self.inspector_body_origin(r);
        let header = Rect::new(
            r.inspector.x0,
            r.inspector.y0,
            r.inspector.x1,
            header_bottom,
        );
        fill_rect(scene, tf, c.surfaces.raised, header);
        let x0 = r.inspector.x0 + PAD * s;
        let x1 = r.inspector.x1 - PAD * s;
        let top = r.inspector.y0;
        text_w(
            scene,
            tr,
            tf,
            "INSPECTOR",
            11.0,
            c.text_dim,
            x0,
            top + 14.0 * s,
            TextWeight::Semibold,
        );
        if self.selection.len() > 1 {
            let label = format!("{} clips selected", self.selection.len());
            let (lw, _) = tr.measure_weighted(&label, 10.5, None, TextWeight::Medium);
            let pillr = Rect::new(
                x1 - f64::from(lw) - 14.0 * s,
                top + 4.0 * s,
                x1,
                top + 24.0 * s,
            );
            fill_rrect(scene, tf, with_alpha(c.accent[5], 0.22), pillr, 10.0 * s);
            text_w(
                scene,
                tr,
                tf,
                &label,
                10.5,
                c.accent[5],
                pillr.x0 + 7.0 * s,
                top + 9.0 * s,
                TextWeight::Medium,
            );
        }
        let Some(clip) = self.selected().and_then(|id| self.project.clip(id)) else {
            return;
        };
        let tag = Rect::new(x0, top + 36.0 * s, x0 + 6.0 * s, top + 68.0 * s);
        fill_rrect(scene, tf, clip_color(theme, clip.palette), tag, 3.0 * s);
        let name = self
            .project
            .media(clip.media)
            .map_or("Missing media", |item| item.name.as_str());
        let shown = elide_w(name, (x1 - x0 - 16.0 * s) as f32, tr);
        text_w(
            scene,
            tr,
            tf,
            &shown,
            15.0,
            c.text_primary,
            x0 + 14.0 * s,
            top + 39.0 * s,
            TextWeight::Semibold,
        );
        let track_name = self
            .project
            .track(clip.track)
            .map_or("track", |track| track.name.as_str());
        let sub = if self.selection_has_locked() {
            format!("on {track_name} · locked")
        } else {
            format!("on {track_name}")
        };
        text(
            scene,
            tr,
            tf,
            &sub,
            11.5,
            if self.selection_has_locked() {
                c.semantic.warning
            } else {
                c.text_dim
            },
            x0 + 14.0 * s,
            top + 62.0 * s,
        );
        let mut prop = |label: &str, value: String, y: f64| {
            text(scene, tr, tf, label, 12.0, c.text_dim, x0, y);
            let (w, _) = tr.measure_weighted(&value, 12.0, None, TextWeight::Medium);
            text_w(
                scene,
                tr,
                tf,
                &value,
                12.0,
                c.text_primary,
                x1 - f64::from(w),
                y,
                TextWeight::Medium,
            );
        };
        let fps = self.project.fps;
        prop("Position", timecode(clip.start, fps), top + 84.0 * s);
        prop("Duration", timecode(clip.duration(), fps), top + 106.0 * s);
        prop("Source In", timecode(clip.in_point, fps), top + 128.0 * s);
        prop("Source Out", timecode(clip.out_point, fps), top + 150.0 * s);
        stroke_line(
            scene,
            tf,
            c.border_standard,
            r.inspector.x0,
            header_bottom,
            r.inspector.x1,
            header_bottom,
            1.0,
        );
    }

    #[allow(
        clippy::indexing_slicing,
        clippy::too_many_lines,
        reason = "inspector controls index fixed option tables and share one scroll coordinate system with their validated hit targets"
    )]
    fn draw_inspector_body(
        &self,
        scene: &mut Scene,
        tf: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        r: &Regions,
    ) {
        let c = &theme.tokens.colors;
        let inspector_scale = self.inspector_ui_scale();
        let x0 = r.inspector.x0 + PAD * inspector_scale;
        let x1 = r.inspector.x1 - PAD * inspector_scale;
        // The whole body scrolls together by `inspector_scroll` (clamped to 0
        // when nothing is selected, so the empty state stays put).
        let top = r.inspector.y0;
        text_w(
            scene,
            tr,
            tf,
            "INSPECTOR",
            11.0,
            c.text_dim,
            x0,
            top + 14.0,
            TextWeight::Semibold,
        );
        // With more than one clip selected, say so where the count matters —
        // every edit below applies to all of them.
        if self.selection.len() > 1 {
            let label = format!("{} clips selected", self.selection.len());
            let (lw, _) = tr.measure_weighted(&label, 10.5, None, TextWeight::Medium);
            let pillr = Rect::new(x1 - lw as f64 - 14.0, top + 4.0, x1, top + 22.0);
            fill_rrect(scene, tf, with_alpha(c.accent[5], 0.22), pillr, 9.0);
            text_w(
                scene,
                tr,
                tf,
                &label,
                10.5,
                c.accent[5],
                pillr.x0 + 7.0,
                top + 8.0,
                TextWeight::Medium,
            );
        }

        let selected = self
            .selected()
            .and_then(|id| self.project.clip(id).cloned());
        let Some(clip) = selected else {
            // The markers still belong to the project, so they stay reachable
            // with nothing selected. Everything else here edits a clip.
            let l = self.inspector_layout(r);
            let blank = Clip::default();
            // The empty state gets the room above the first section header,
            // not the whole pane — the sections below are real controls and
            // must not be drawn through.
            let floor = l
                .sections
                .first()
                .map(|(_, rect)| rect.y0 - 8.0)
                .unwrap_or(r.inspector.y1);
            let area = Rect::new(
                x0,
                top + INSPECTOR_HEADER_H,
                x1,
                floor.max(top + INSPECTOR_HEADER_H),
            );
            // The shared empty state, so this pane says nothing is selected in
            // the same voice — icon, title, one line of what to do — that the
            // rest of the fleet uses. It was a pair of bare left-aligned lines
            // that read as a label for the sections beneath them rather than
            // as the state of the pane.
            ui::widget::EmptyState::paint(
                scene,
                tf,
                tr,
                theme,
                area,
                Some(ui::icons::Icon::Layers),
                INSPECTOR_EMPTY_TITLE,
                Some(INSPECTOR_EMPTY_HINT),
                None,
                Point::new(f64::MIN, f64::MIN),
            );
            for (s, rect) in &l.sections {
                self.draw_section_header(scene, tf, tr, theme, *rect, *s, &blank, None);
            }
            self.draw_marker_rows(scene, tf, tr, theme, &l, x0);
            self.draw_project_section(scene, tf, tr, theme, &l, x0);
            return;
        };
        let media = self.project.media(clip.media).cloned();
        let is_video = media.as_ref().is_none_or(|m| m.kind.is_visual());
        let track_name = self
            .project
            .track(clip.track)
            .map(|t| t.name.clone())
            .unwrap_or_default();

        // Header card with color tag + name (scrolls with the body).
        let tag = Rect::new(x0, top + 36.0, x0 + 6.0, top + 64.0);
        fill_rrect(scene, tf, clip_color(theme, clip.palette), tag, 3.0);
        let name = media
            .as_ref()
            .map(|m| m.name.clone())
            .unwrap_or_else(|| "clip".into());
        let shown_name = elide_w(&name, (x1 - x0 - 14.0) as f32, tr);
        text_w(
            scene,
            tr,
            tf,
            &shown_name,
            15.0,
            c.text_primary,
            x0 + 14.0,
            top + 38.0,
            TextWeight::Semibold,
        );
        // Name the lane, and say plainly when it refuses edits — the same
        // reason the timeline hatches it.
        let sub = if self.selection_has_locked() {
            format!("on {track_name} · locked")
        } else {
            format!("on {track_name}")
        };
        let sub_col = if self.selection_has_locked() {
            c.semantic.warning
        } else {
            c.text_dim
        };
        text(scene, tr, tf, &sub, 11.5, sub_col, x0 + 14.0, top + 58.0);

        // Property rows.
        let mut y = top + 78.0;
        let fps = self.project.fps;
        let prop = |scene: &mut Scene, tr: &mut TextRenderer, label: &str, value: &str, y: f64| {
            text(scene, tr, tf, label, 12.0, c.text_dim, x0, y);
            let (vw, _) = tr.measure_weighted(value, 12.0, None, TextWeight::Medium);
            text_w(
                scene,
                tr,
                tf,
                value,
                12.0,
                c.text_primary,
                x1 - f64::from(vw),
                y,
                TextWeight::Medium,
            );
        };
        prop(scene, tr, "Position", &timecode(clip.start, fps), y);
        y += 18.0;
        prop(scene, tr, "Duration", &timecode(clip.duration(), fps), y);
        y += 18.0;
        prop(scene, tr, "Source In", &timecode(clip.in_point, fps), y);
        y += 18.0;
        prop(scene, tr, "Source Out", &timecode(clip.out_point, fps), y);

        let l = self.inspector_layout(r);
        // Every slider row: a label, a right-aligned readout measured like all
        // the others, and the shared slider face carrying hover / drag state.
        let row = |scene: &mut Scene,
                   tr: &mut TextRenderer,
                   track: Option<Rect>,
                   label: &str,
                   readout: String,
                   v: f64,
                   size: f32,
                   drag: bool,
                   // `Some` when this row carries a key control, whose box
                   // the label starts after. The indent is unconditional
                   // so keying a parameter does not shuffle its row
                   // sideways; an animated one takes the accent instead.
                   keyed: Option<bool>| {
            let Some(track) = track else { return };
            let lx = if keyed.is_some() {
                track.x0 + KEY_BOX + 6.0
            } else {
                track.x0
            };
            let col = if keyed == Some(true) {
                c.accent[5]
            } else {
                c.text_dim
            };
            text(scene, tr, tf, label, size, col, lx, track.y0 - 20.0);
            let (pw, _) = tr.measure(&readout, size, None);
            text(
                scene,
                tr,
                tf,
                &readout,
                size,
                c.text_primary,
                track.x1 - pw as f64,
                track.y0 - 20.0,
            );
            draw_track(
                scene,
                tf,
                theme,
                track,
                v,
                hit_slider(track, self.last_mouse),
                drag,
            );
        };
        let dragging = |d: Option<Drag>, want: Drag| d == Some(want);
        // Animatable rows read through `param_at`, the same door the
        // compositor uses, so a keyframed slider follows the playhead rather
        // than showing a constant the picture is already ignoring.
        let animated = |p: Param| self.key_state(p) != KeyState::Off;
        let at = self.playhead;
        let opacity = clip.param_at(Param::Opacity, at);
        row(
            scene,
            tr,
            l.opacity,
            "Opacity",
            format!("{:.0}%", opacity * 100.0),
            opacity,
            12.0,
            dragging(self.drag, Drag::Opacity),
            Some(animated(Param::Opacity)),
        );
        let volume = clip.param_at(Param::Volume, at);
        row(
            scene,
            tr,
            l.volume,
            "Volume",
            format!("{:.0}%", volume * 100.0),
            volume,
            12.0,
            dragging(self.drag, Drag::Volume),
            Some(animated(Param::Volume)),
        );
        // Colour-grade rows. The readout shows the multiplier (0.5 == 1.00×,
        // the identity).
        for (track, label, p, d) in [
            (
                l.brightness,
                "Brightness",
                Param::Brightness,
                Drag::Brightness,
            ),
            (l.contrast, "Contrast", Param::Contrast, Drag::Contrast),
            (
                l.saturation,
                "Saturation",
                Param::Saturation,
                Drag::Saturation,
            ),
        ] {
            let v = clip.param_at(p, at);
            row(
                scene,
                tr,
                track,
                label,
                format!("{:.2}×", Filters::gain(v)),
                v,
                12.0,
                dragging(self.drag, d),
                Some(animated(p)),
            );
        }
        // Fade in / out (seconds). Two half-width tracks share a row; their
        // readouts right-align by measurement like every other row, rather
        // than by a magic inset that only fitted one string.
        let cd = clip.duration().max(1e-6);
        row(
            scene,
            tr,
            l.fade_in,
            "Fade In",
            format!("{:.1}s", clip.fade_in),
            (clip.fade_in / cd).clamp(0.0, 1.0),
            11.0,
            dragging(self.drag, Drag::FadeIn),
            None,
        );
        row(
            scene,
            tr,
            l.fade_out,
            "Fade Out",
            format!("{:.1}s", clip.fade_out),
            (clip.fade_out / cd).clamp(0.0, 1.0),
            11.0,
            dragging(self.drag, Drag::FadeOut),
            None,
        );
        // Transition segmented control, through the shared painter.
        if !l.transitions.is_empty() {
            text(
                scene,
                tr,
                tf,
                "Transition",
                12.0,
                c.text_dim,
                x0,
                l.transition_y - 20.0,
            );
            let rects: Vec<Rect> = l.transitions.iter().map(|(_, r)| *r).collect();
            let labels: Vec<&str> = l.transitions.iter().map(|(t, _)| t.label()).collect();
            let sel = l
                .transitions
                .iter()
                .position(|(t, _)| *t == clip.transition)
                .unwrap_or(0);
            draw_segmented(scene, tf, tr, theme, &rects, &labels, sel, self.last_mouse);
        }
        // Transition length.
        if let Some(track) = l.transition_len {
            let max = clip.duration().max(0.01);
            row(
                scene,
                tr,
                Some(track),
                "Transition length",
                format!("{:.2}s", clip.transition_len()),
                (clip.transition_len() / max).clamp(0.0, 1.0),
                11.0,
                self.drag == Some(Drag::TransitionLen),
                None,
            );
        }
        // Colour swatches.
        if !l.colors.is_empty() {
            text(
                scene,
                tr,
                tf,
                "Label",
                12.0,
                c.text_dim,
                x0,
                l.colors_y - 20.0,
            );
            for (pal, rect) in &l.colors {
                let sw = clip_color(theme, *pal);
                fill_rrect(scene, tf, sw, *rect, 6.0);
                if rect.contains(self.last_mouse) && clip.palette != *pal {
                    stroke_rrect(scene, tf, with_alpha(c.text_primary, 0.5), *rect, 6.0, 1.5);
                }
                if clip.palette == *pal {
                    // The selection ring has to read against whichever colour
                    // it rings, so it comes from the focus token rather than
                    // a hardcoded white that vanishes on the pale swatches.
                    stroke_rrect(scene, tf, c.border_focus, rect.inset(1.5), 7.0, 2.0);
                }
            }
        }
        // Link / unlink. The caption names the state so the button can name
        // the *change*: a control labelled only "Unlink" leaves a reader
        // guessing whether it describes what is true or what will happen.
        if let (Some(rect), Some(offer)) = (l.link, self.link_affordance()) {
            let linked = offer == LinkAffordance::Unlink;
            text(
                scene,
                tr,
                tf,
                offer.state(),
                12.0,
                if linked { c.accent[5] } else { c.text_dim },
                x0,
                l.link_label_y - 15.0,
            );
            draw_button_enabled(
                scene,
                tf,
                tr,
                theme,
                rect,
                offer.label(),
                false,
                rect.contains(self.last_mouse),
                true,
            );
        }
        // ---- collapsible sections ------------------------------------
        // Headers first, so a folded section still says what is inside it.
        for (s, rect) in &l.sections {
            self.draw_section_header(scene, tf, tr, theme, *rect, *s, &clip, media.as_ref());
        }
        // Every ranged control laid out this frame: geometry, rate, title
        // size. Each reads its value back through `num_value`, so a keyframed
        // one tracks the playhead rather than showing a dead constant.
        for (f, track) in &l.nums {
            let Some(v) = self.num_value(*f) else {
                continue;
            };
            let keyed = f
                .param()
                .map(|p| self.key_state(p) != KeyState::Off)
                .unwrap_or(false);
            let lx = if f.param().is_some() {
                track.x0 + KEY_BOX + 6.0
            } else {
                track.x0
            };
            let label_col = if keyed { c.accent[5] } else { c.text_dim };
            text(
                scene,
                tr,
                tf,
                f.label(),
                11.5,
                label_col,
                lx,
                track.y0 - 20.0,
            );
            let readout = f.readout(v);
            let (pw, _) = tr.measure(&readout, 11.5, None);
            text(
                scene,
                tr,
                tf,
                &readout,
                11.5,
                c.text_primary,
                track.x1 - pw as f64,
                track.y0 - 20.0,
            );
            draw_track(
                scene,
                tf,
                theme,
                *track,
                f.to_frac(v),
                hit_slider(*track, self.last_mouse),
                self.drag == Some(Drag::Num(*f)),
            );
        }
        // Picture-in-picture presets. The glyph is the corner it puts the
        // picture in, which is the whole of what the button does.
        if !l.pip.is_empty() {
            let top = l.pip[0].2;
            text(
                scene,
                tr,
                tf,
                "Picture in picture",
                11.5,
                c.text_dim,
                x0,
                top.y0 - 16.0 * inspector_scale,
            );
            let inset = crate::model::Transform::pip(PIP_SCALE, (1.0, -1.0), PIP_INSET);
            let is_pip = (clip.transform.scale_x - inset.scale_x).abs() < 1e-6;
            for (_, corner, rect) in &l.pip {
                let on = is_pip
                    && (clip.transform.pos_x.signum() - corner.0).abs() < 1e-6
                    && (clip.transform.pos_y.signum() - corner.1).abs() < 1e-6;
                let hovered = rect.contains(self.last_mouse);
                let bg = if on {
                    c.accent[5]
                } else if hovered {
                    c.lift(0.72)
                } else {
                    c.neutral[3]
                };
                fill_rrect(scene, tf, bg, *rect, 6.0);
                stroke_rrect(scene, tf, c.border_standard, *rect, 6.0, 1.0);
                // A miniature frame with the inset drawn in the named corner.
                let fg = if on {
                    on_fill(theme, bg).1
                } else {
                    c.text_primary
                };
                let g = Rect::new(
                    rect.center().x - 11.0,
                    rect.center().y - 7.0,
                    rect.center().x + 11.0,
                    rect.center().y + 7.0,
                );
                stroke_rrect(scene, tf, with_alpha(fg, 0.5), g, 2.0, 1.0);
                let iw = g.width() * 0.42;
                let ih = g.height() * 0.42;
                let ix = if corner.0 < 0.0 {
                    g.x0 + 2.0
                } else {
                    g.x1 - 2.0 - iw
                };
                let iy = if corner.1 < 0.0 {
                    g.y0 + 2.0
                } else {
                    g.y1 - 2.0 - ih
                };
                fill_rrect(scene, tf, fg, Rect::new(ix, iy, ix + iw, iy + ih), 1.5);
            }
        }
        if l.crop_label_y > 0.0 {
            text(scene, tr, tf, "Crop", 11.5, c.text_dim, x0, l.crop_label_y);
        }
        if l.anchor_label_y > 0.0 {
            text(
                scene,
                tr,
                tf,
                "Anchor",
                11.5,
                c.text_dim,
                x0,
                l.anchor_label_y,
            );
        }
        // Three segmented rows make the 3×3 grid, so each row is a real
        // shared toggle group rather than nine hand-drawn cells.
        for band in l.anchor.chunks(3) {
            let rects: Vec<Rect> = band.iter().map(|(_, r)| *r).collect();
            let labels: Vec<&str> = band
                .iter()
                .map(|(a, _)| {
                    let col = ANCHOR_STOPS.iter().position(|s| *s == a.0).unwrap_or(1);
                    let row = ANCHOR_STOPS.iter().position(|s| *s == a.1).unwrap_or(1);
                    ANCHOR_GLYPHS[row * 3 + col]
                })
                .collect();
            let sel = band
                .iter()
                .position(|(a, _)| {
                    (clip.transform.anchor_x - a.0).abs() < 1e-6
                        && (clip.transform.anchor_y - a.1).abs() < 1e-6
                })
                .unwrap_or(usize::MAX);
            draw_segmented(scene, tf, tr, theme, &rects, &labels, sel, self.last_mouse);
        }
        if let Some(rect) = l.reset_transform {
            let plain = clip.transform.is_identity();
            draw_button_enabled(
                scene,
                tf,
                tr,
                theme,
                rect,
                "Fill frame",
                false,
                rect.contains(self.last_mouse),
                !plain,
            );
        }
        // Rate presets + the reverse toggle.
        if !l.speed_presets.is_empty() {
            let rects: Vec<Rect> = l.speed_presets.iter().map(|(_, r)| *r).collect();
            let labels: Vec<String> = l
                .speed_presets
                .iter()
                .map(|(rate, _)| {
                    if (rate - rate.round()).abs() < 1e-6 {
                        format!("{}×", rate.round() as i64)
                    } else {
                        format!("{rate}×")
                    }
                })
                .collect();
            let refs: Vec<&str> = labels.iter().map(|s| s.as_str()).collect();
            // No preset is "selected" unless the rate is exactly on one —
            // a segmented control that always highlights something would
            // claim 1.37× was 1×.
            let sel = l
                .speed_presets
                .iter()
                .position(|(rate, _)| (rate - clip.speed.rate()).abs() < 1e-6)
                .unwrap_or(usize::MAX);
            draw_segmented(scene, tf, tr, theme, &rects, &refs, sel, self.last_mouse);
        }
        if let Some(rect) = l.reverse {
            let on = clip.speed.reverse;
            draw_button(
                scene,
                tf,
                tr,
                theme,
                rect,
                if on {
                    "Playing backwards"
                } else {
                    "Play backwards"
                },
                on,
                rect.contains(self.last_mouse),
            );
        }
        // ---- the lane's mix ---------------------------------------------
        // The panel is otherwise entirely about one clip, so the two knobs
        // that reach past it have to say what they reach — including that
        // everything else on the lane moves with them.
        if let Some(t) = self.selected_track() {
            let level = self.mix.track(t.id);
            if l.mix_track_y > 0.0 {
                let others = self.project.clips_on(t.id).len().saturating_sub(1);
                let words = match others {
                    0 => format!("{} — this lane", t.name),
                    1 => format!("{} — this lane, 1 other clip", t.name),
                    n => format!("{} — this lane, {n} other clips", t.name),
                };
                let words = elide_at(&words, (x1 - x0) as f32, 11.0, tr);
                text(scene, tr, tf, &words, 11.0, c.text_dim, x0, l.mix_track_y);
            }
            if let Some(meter) = l.mix_meter {
                draw_level_meter(
                    scene,
                    tf,
                    theme,
                    meter,
                    level.map(|v| v.level).unwrap_or_default(),
                    level.map(|v| v.hold).unwrap_or(0.0),
                    level.is_some_and(|v| v.clipped),
                );
            }
        }
        if let Some(rect) = l.mix_reset {
            draw_button(
                scene,
                tf,
                tr,
                theme,
                rect,
                "Reset to 0.0 dB, centre",
                false,
                rect.contains(self.last_mouse),
            );
        }
        // ---- tone curves ------------------------------------------------
        if !l.curve_channels.is_empty() {
            let rects: Vec<Rect> = l.curve_channels.iter().map(|(_, r)| *r).collect();
            let labels: Vec<&str> = l.curve_channels.iter().map(|(c, _)| c.label()).collect();
            let sel = l
                .curve_channels
                .iter()
                .position(|(c, _)| *c == self.curve_channel)
                .unwrap_or(0);
            draw_segmented(scene, tf, tr, theme, &rects, &labels, sel, self.last_mouse);
        }
        if let Some(canvas) = l.curve_canvas {
            self.draw_curve_editor(scene, tf, tr, theme, canvas, &clip);
        }
        if let Some(rect) = l.curve_reset {
            let ch = self.curve_channel;
            let shaped = !clip.curves.spline(ch).is_identity();
            draw_button_enabled(
                scene,
                tf,
                tr,
                theme,
                rect,
                &format!("Reset {}", ch.label()),
                false,
                rect.contains(self.last_mouse),
                shaped,
            );
        }
        // ---- colour key -----------------------------------------------
        if let Some(rect) = l.key_enable {
            let on = clip.key.enabled;
            draw_button(
                scene,
                tf,
                tr,
                theme,
                rect,
                if on {
                    "Keying this colour out"
                } else {
                    "Key a colour out"
                },
                on,
                rect.contains(self.last_mouse),
            );
        }
        if l.key_color_y > 0.0 {
            text(
                scene,
                tr,
                tf,
                "Key colour",
                11.5,
                c.text_dim,
                x0,
                l.key_color_y,
            );
        }
        for (col, rect) in &l.key_colors {
            let on = clip
                .key
                .color
                .iter()
                .zip(col)
                .all(|(a, b)| (a - b).abs() < 1e-6);
            let swatch = Color::new([col[0] as f32, col[1] as f32, col[2] as f32, 1.0]);
            fill_rrect(scene, tf, swatch, *rect, 5.0);
            // The chosen swatch takes a ring rather than a tick, so the
            // colour it names is never covered by the mark that selects it.
            if on {
                stroke_rrect(scene, tf, c.text_primary, rect.inflate(2.0, 2.0), 7.0, 2.0);
            } else if rect.contains(self.last_mouse) {
                stroke_rrect(scene, tf, c.border_focus, rect.inflate(1.0, 1.0), 6.0, 1.0);
            }
        }
        if let Some(rect) = l.key_pick {
            // Sampling needs a picture to sample: with no decoded frame the
            // button says so rather than quietly handing back a colour the
            // footage never had.
            let have = self.preview_pixel(0.5, 0.5).is_some();
            draw_button_enabled(
                scene,
                tf,
                tr,
                theme,
                rect,
                if have {
                    "Sample centre"
                } else {
                    "No frame to sample"
                },
                false,
                rect.contains(self.last_mouse),
                have,
            );
        }
        // ---- mask -------------------------------------------------------
        if !l.mask_shapes.is_empty() {
            let rects: Vec<Rect> = l.mask_shapes.iter().map(|(_, r)| *r).collect();
            let labels: Vec<&str> = l.mask_shapes.iter().map(|(s, _)| s.label()).collect();
            let sel = l
                .mask_shapes
                .iter()
                .position(|(s, _)| *s == clip.mask.shape)
                .unwrap_or(usize::MAX);
            draw_segmented(scene, tf, tr, theme, &rects, &labels, sel, self.last_mouse);
        }
        if let Some(rect) = l.mask_fit {
            draw_button(
                scene,
                tf,
                tr,
                theme,
                rect,
                "Fit to picture",
                false,
                rect.contains(self.last_mouse),
            );
        }
        if let Some(rect) = l.mask_invert {
            let on = clip.mask.invert;
            draw_button(
                scene,
                tf,
                tr,
                theme,
                rect,
                if on { "Keeping outside" } else { "Invert" },
                on,
                rect.contains(self.last_mouse),
            );
        }
        // ---- title ----------------------------------------------------
        if let Some(rect) = l.title_text {
            let text_src = media
                .as_ref()
                .and_then(|m| m.text.clone())
                .unwrap_or_default();
            text(
                scene,
                tr,
                tf,
                "Words",
                11.5,
                c.text_dim,
                rect.x0,
                rect.y0 - 16.0,
            );
            let editing = self
                .title_edit
                .as_ref()
                .map(|(m, _)| Some(*m) == media.as_ref().map(|m| m.id));
            fill_rrect(scene, tf, c.lift(0.35), rect, 6.0);
            stroke_rrect(
                scene,
                tf,
                if editing == Some(true) {
                    c.border_focus
                } else {
                    c.border_standard
                },
                rect,
                6.0,
                1.0,
            );
            // While the inline editor owns this box it draws the words itself
            // (with a caret); otherwise show them, or say plainly that there
            // are none rather than leaving an empty box that looks broken.
            if editing != Some(true) {
                if text_src.content.trim().is_empty() {
                    let baseline = text_baseline(rect, 13.0, tr);
                    text(
                        scene,
                        tr,
                        tf,
                        "Click to type a title",
                        13.0,
                        c.text_dim,
                        rect.x0 + 10.0,
                        baseline,
                    );
                } else {
                    // Real line breaks, on their own lines. The field used to
                    // fold them into a `⏎` glyph because it could not carry
                    // one; it can now, so showing the words as written is
                    // both honest and what the frame will look like.
                    for (i, line) in text_src.content.lines().take(3).enumerate() {
                        let shown = elide_w(line, (rect.width() - 20.0) as f32, tr);
                        text(
                            scene,
                            tr,
                            tf,
                            &shown,
                            13.0,
                            c.text_primary,
                            rect.x0 + 10.0,
                            rect.y0 + 18.0 + i as f64 * 18.0,
                        );
                    }
                    if text_src.content.lines().count() > 3 {
                        text(
                            scene,
                            tr,
                            tf,
                            "…",
                            13.0,
                            c.text_dim,
                            rect.x0 + 10.0,
                            rect.y0 + 18.0 + 3.0 * 18.0,
                        );
                    }
                }
            } else {
                // While the field is open, say what ends the edit — Enter is
                // a line break here, so it cannot also be Accept.
                text(
                    scene,
                    tr,
                    tf,
                    "Ctrl+Enter to accept · Esc to cancel",
                    10.5,
                    c.text_dim,
                    rect.x0,
                    rect.y1 + 14.0,
                );
            }
        }
        if !l.title_align.is_empty() {
            let cur = media
                .as_ref()
                .and_then(|m| m.text.as_ref())
                .map(|t| t.align)
                .unwrap_or_default();
            let rects: Vec<Rect> = l.title_align.iter().map(|(_, r)| *r).collect();
            let labels: Vec<&str> = l.title_align.iter().map(|(a, _)| a.label()).collect();
            let sel = l
                .title_align
                .iter()
                .position(|(a, _)| *a == cur)
                .unwrap_or(0);
            draw_segmented(scene, tf, tr, theme, &rects, &labels, sel, self.last_mouse);
        }
        if let Some(rect) = l.title_font {
            let current = media
                .as_ref()
                .and_then(|m| m.text.as_ref())
                .map(|t| t.font.clone())
                .unwrap_or_default();
            text(
                scene,
                tr,
                tf,
                "Font",
                11.5,
                c.text_dim,
                rect.x0,
                rect.y0 - 16.0,
            );
            // The open list is drawn later, over the panel, because it floats
            // above everything under it. Here is the resting control: a
            // button naming the family in force. "The system's default sans"
            // is a real answer, not a missing one, so it is named rather than
            // left as an empty field.
            if self.font_picker.is_none() {
                let label = if current.trim().is_empty() {
                    Self::DEFAULT_FONT_LABEL.to_string()
                } else {
                    current
                };
                draw_button(
                    scene,
                    tf,
                    tr,
                    theme,
                    rect,
                    &label,
                    false,
                    rect.contains(self.last_mouse),
                );
            }
        }
        if !l.title_colors.is_empty() {
            let cur = media
                .as_ref()
                .and_then(|m| m.text.as_ref())
                .map(|t| t.color)
                .unwrap_or([1.0; 4]);
            text(
                scene,
                tr,
                tf,
                "Colour",
                11.5,
                c.text_dim,
                x0,
                l.title_colors[0].1.y0 - 16.0,
            );
            for (col, rect) in &l.title_colors {
                let sw = title_color(*col);
                // A swatch can be white, near-black or translucent, so it is
                // drawn on a chequer-free plate with a border: without one the
                // white swatch vanishes on a light theme and the dark one on a
                // dark theme.
                fill_rrect(scene, tf, c.neutral[3], *rect, 6.0);
                fill_rrect(scene, tf, sw, *rect, 6.0);
                stroke_rrect(scene, tf, with_alpha(c.text_primary, 0.25), *rect, 6.0, 1.0);
                if col
                    .iter()
                    .zip(cur.iter())
                    .all(|(a, b)| (a - b).abs() < 1e-6)
                {
                    stroke_rrect(scene, tf, c.border_focus, rect.inset(1.5), 7.0, 2.0);
                } else if rect.contains(self.last_mouse) {
                    stroke_rrect(scene, tf, with_alpha(c.text_primary, 0.6), *rect, 6.0, 1.5);
                }
            }
        }
        for (rect, on, label) in [
            (
                l.title_bold,
                media
                    .as_ref()
                    .and_then(|m| m.text.as_ref())
                    .map(|t| t.bold)
                    .unwrap_or(false),
                "Bold",
            ),
            (
                l.title_italic,
                media
                    .as_ref()
                    .and_then(|m| m.text.as_ref())
                    .map(|t| t.italic)
                    .unwrap_or(false),
                "Italic",
            ),
        ] {
            let Some(rect) = rect else { continue };
            draw_button(
                scene,
                tf,
                tr,
                theme,
                rect,
                label,
                on,
                rect.contains(self.last_mouse),
            );
        }
        self.draw_marker_rows(scene, tf, tr, theme, &l, x0);
        self.draw_project_section(scene, tf, tr, theme, &l, x0);
        // ---- key controls ---------------------------------------------
        // Drawn last so a diamond sits over its row rather than under it.
        for (param, rect) in &l.keys {
            draw_key_control(
                scene,
                tf,
                theme,
                *rect,
                self.key_state(*param),
                rect.contains(self.last_mouse),
            );
        }
        // Effects stack (video only).
        if is_video {
            for (i, row) in l.effect_rows.iter().enumerate() {
                let Some(e) = clip.effects.get(i) else {
                    continue;
                };
                let (_key, up, down, slider, remove) = Self::effect_row_parts(*row);
                fill_rrect(scene, tf, c.lift(0.35), *row, 6.0);
                // Reorder / remove: real icons on hover-lit hit targets, sized to
                // the pointer rather than to the glyph. These were raw Unicode
                // arrows in 16x18px boxes with no hover state at all.
                let last = i + 1 == clip.effects.len();
                for (rect, icon, enabled) in [
                    (up, ui::icons::Icon::ChevronUp, i > 0),
                    (down, ui::icons::Icon::ChevronDown, !last),
                ] {
                    if enabled && rect.contains(self.last_mouse) {
                        fill_rrect(scene, tf, c.overlay_medium, rect, 4.0);
                    }
                    let col = if enabled {
                        c.text_primary
                    } else {
                        c.text_tertiary
                    };
                    let g = 12.0;
                    let box_ = Rect::new(
                        rect.center().x - g / 2.0,
                        rect.center().y - g / 2.0,
                        rect.center().x + g / 2.0,
                        rect.center().y + g / 2.0,
                    );
                    ui::icons::draw_icon(
                        scene,
                        tf,
                        box_,
                        icon,
                        col,
                        ui::icons::LUCIDE_DEFAULT_STROKE_PX,
                    );
                }
                // Name.
                let name_y = text_baseline(*row, 12.0, tr);
                text_w(
                    scene,
                    tr,
                    tf,
                    e.kind.label(),
                    12.0,
                    c.text_primary,
                    down.x1 + 8.0,
                    name_y,
                    TextWeight::Medium,
                );
                // Intensity slider.
                draw_track(
                    scene,
                    tf,
                    theme,
                    slider,
                    e.amount,
                    hit_slider(slider, self.last_mouse),
                    matches!(self.drag, Some(Drag::EffectAmount { idx, .. }) if idx == i),
                );
                // Remove.
                if remove.contains(self.last_mouse) {
                    fill_rrect(scene, tf, with_alpha(c.semantic.danger, 0.18), remove, 4.0);
                }
                let g = 12.0;
                let xbox = Rect::new(
                    remove.center().x - g / 2.0,
                    remove.center().y - g / 2.0,
                    remove.center().x + g / 2.0,
                    remove.center().y + g / 2.0,
                );
                ui::icons::draw_icon(
                    scene,
                    tf,
                    xbox,
                    ui::icons::Icon::X,
                    c.semantic.danger,
                    ui::icons::LUCIDE_DEFAULT_STROKE_PX,
                );
            }
            // Add-effect button.
            let add_hover = l.add_effect.contains(self.last_mouse);
            fill_rrect(
                scene,
                tf,
                if add_hover {
                    c.lift(0.72)
                } else {
                    c.neutral[3]
                },
                l.add_effect,
                6.0,
            );
            stroke_rrect(scene, tf, c.border_standard, l.add_effect, 6.0, 1.0);
            text(
                scene,
                tr,
                tf,
                "+ Add effect",
                12.0,
                c.text_primary,
                l.add_effect.x0 + 12.0,
                l.add_effect.center().y - 8.0,
            );
        }

        if l.valid {
            draw_button(
                scene,
                tf,
                tr,
                theme,
                l.split,
                "Split",
                false,
                l.split.contains(self.last_mouse),
            );
            draw_danger_button(
                scene,
                tf,
                tr,
                theme,
                l.delete,
                "Delete",
                l.delete.contains(self.last_mouse),
            );
        }
    }

    /// One collapsible section header: a fold arrow, the name, and — when it
    /// is folded — a summary of what is inside it.
    ///
    /// The summary is the point. Folding a section away is a view choice, but
    /// a fold that hid "this clip plays at 4x in reverse" would turn a view
    /// choice into missing state, and the user would have no way to know the
    /// clip was re-timed without unfolding every section on every clip.
    fn draw_section_header(
        &self,
        scene: &mut Scene,
        tf: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        rect: Rect,
        section: Section,
        clip: &Clip,
        media: Option<&MediaItem>,
    ) {
        let c = &theme.tokens.colors;
        let open = !self.collapsed.contains(&section);
        if rect.contains(self.last_mouse) {
            fill_rrect(scene, tf, c.lift(0.35), rect, 5.0);
        }
        let g = 11.0;
        let arrow = Rect::new(
            rect.x0 + 1.0,
            rect.center().y - g / 2.0,
            rect.x0 + 1.0 + g,
            rect.center().y + g / 2.0,
        );
        ui::icons::draw_icon(
            scene,
            tf,
            arrow,
            if open {
                ui::icons::Icon::ChevronDown
            } else {
                ui::icons::Icon::ChevronRight
            },
            c.text_dim,
            ui::icons::LUCIDE_DEFAULT_STROKE_PX,
        );
        let baseline = text_baseline(rect, 11.0, tr);
        text_w(
            scene,
            tr,
            tf,
            section.label(),
            11.0,
            c.text_dim,
            rect.x0 + 18.0,
            baseline,
            TextWeight::Semibold,
        );
        if open {
            return;
        }
        let Some(summary) = self.section_summary(section, clip, media) else {
            return;
        };
        let (w, _) = tr.measure(&summary, 11.0, None);
        // A summary that reports something other than the default is worth
        // reading, so it takes the accent rather than the dim tone.
        text(
            scene,
            tr,
            tf,
            &summary,
            11.0,
            c.accent[5],
            rect.x1 - w as f64,
            baseline,
        );
    }

    /// What a folded section reports about itself, or `None` when everything
    /// inside it is at its default and there is nothing to report.
    #[allow(
        clippy::indexing_slicing,
        reason = "section summaries read the paired fixed labels and values that their section builder created"
    )]
    fn section_summary(
        &self,
        section: Section,
        clip: &Clip,
        media: Option<&MediaItem>,
    ) -> Option<String> {
        match section {
            Section::Curves => curve_summary(clip, self.playhead),
            Section::Transform => {
                // The geometry the clip is *playing*, not the constants under
                // it: a keyframe supersedes the constant for every other
                // reader, so a folded header reading `transform.scale_x`
                // reported 1.00× beside a canvas that was pushing in.
                let g = self.geometry_at_playhead(clip);
                // `is_identity()` on the constants alone was the whole gate,
                // and keying a parameter from its default leaves that constant
                // exactly at the aspect-fit — so a clip whose picture swings
                // across the frame between two keys reported nothing at all.
                let animated = Self::transform_is_animated(clip);
                // `is_identity()` still carries the crop and the anchor, which
                // the model does not animate; the two new disjuncts are what it
                // could never see — a keyed channel away from its rest value,
                // and the existence of a curve at all.
                if clip.transform.is_identity() && !g.is_placed() && !animated {
                    return None;
                }
                let mut parts = Vec::new();
                if (g.scale_x - 1.0).abs() > 1e-6 || (g.scale_y - 1.0).abs() > 1e-6 {
                    parts.push(format!("{:.2}×", g.scale_x));
                }
                if g.rotation.abs() > 1e-6 {
                    parts.push(format!("{:.0}°", g.rotation));
                }
                if clip.transform.is_cropped() {
                    parts.push("cropped".into());
                }
                if animated {
                    parts.push("animated".into());
                }
                if parts.is_empty() {
                    parts.push("moved".into());
                }
                Some(parts.join(" · "))
            }
            Section::Speed => (!clip.speed.is_normal()).then(|| clip.speed.label()),
            // The lane's fader, and whether it has been caught clipping. The
            // second is the part that must survive a fold: hiding the warning
            // because the panel is tidied away is exactly the silence this
            // section exists to end.
            Section::Mix => {
                let t = self.project.track(clip.track)?;
                let clipped = self
                    .mix
                    .track(t.id)
                    .is_some_and(|level| level.clipped)
                    .then_some(" · CLIP")
                    .unwrap_or_default();
                if !t.mix_is_set() && clipped.is_empty() {
                    return None;
                }
                Some(format!("{} {}{clipped}", t.gain_words(), t.pan_words()))
            }
            // A key that is switched on but keying nothing still has to
            // say so: "on" and "removing something" are different states,
            // and a fold that reported neither would hide both. The tolerance
            // is read at the playhead, because it is animatable: a key ramping
            // on read as "keys nothing" for the whole clip.
            Section::Key => clip.key.enabled.then(|| {
                let tolerance = clip.param_at(Param::KeyTolerance, self.playhead);
                if tolerance <= 1e-6 {
                    "on · keys nothing".to_string()
                } else {
                    format!("keyed {:.0}%", tolerance * 100.0)
                }
            }),
            Section::Mask => clip.mask.is_active().then(|| {
                let shape = clip.mask.shape.label().to_lowercase();
                if clip.mask.invert {
                    format!("{shape} · inverted")
                } else {
                    shape
                }
            }),
            Section::Title => {
                let t = media?.text.as_ref()?;
                let first = t.content.lines().next().unwrap_or("").trim();
                Some(if first.is_empty() {
                    "no words yet".into()
                } else {
                    first.to_string()
                })
            }
            Section::Effects => (!clip.effects.is_empty()).then(|| {
                if clip.effects.len() == 1 {
                    clip.effects[0].kind.label().to_string()
                } else {
                    format!("{} effects", clip.effects.len())
                }
            }),
            Section::Markers => {
                let n = self.project.all_markers().len();
                (n > 0).then(|| {
                    if n == 1 {
                        "1 marker".into()
                    } else {
                        format!("{n} markers")
                    }
                })
            }
            // A folded PROJECT header is where the frame most needs stating:
            // the format is a fact about every clip below it, and this is the
            // one line that carries it when the section is away.
            Section::Project => Some(self.project_format_shown.clone()),
        }
    }

    /// The master meter and the standing clipping warning.
    ///
    /// Two states and they read completely differently, which is the point.
    /// Resting, it is a stereo bar and the loudest the mix has been, in
    /// decibels — the reading a person mixing wants anyway. Once the mix has
    /// gone over, the right-hand chip turns into a red **CLIP** stating how
    /// far over, and it stays that way: it survives the transport stopping,
    /// the playhead moving and the panel being folded, and it is cleared only
    /// by changing the levels it is a report about.
    fn draw_master_strip(
        &self,
        scene: &mut Scene,
        tf: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        r: &Regions,
    ) {
        let c = &theme.tokens.colors;
        let strip = self.master_strip_rect(r);
        if strip.width() < 60.0 || strip.height() < 20.0 {
            return;
        }
        fill_rect(scene, tf, c.surfaces.raised, strip);
        // Nothing to mix, nothing to meter. A dead meter on a picture-only
        // timeline would read as an output that is broken.
        if !crate::audio::has_audio(&self.project) {
            text(
                scene,
                tr,
                tf,
                "NO AUDIO",
                9.0,
                c.text_dim,
                strip.x0 + 12.0,
                strip.y0 + 10.0,
            );
            return;
        }
        text(
            scene,
            tr,
            tf,
            "MIX",
            9.0,
            c.text_dim,
            strip.x0 + 12.0,
            strip.y0 + 5.0,
        );
        let flag = self.clip_flag_rect(r);
        if self.mix.clipped() {
            let hot = flag.contains(self.last_mouse);
            let bg = if hot {
                lighten(c.semantic.danger, 0.15)
            } else {
                c.semantic.danger
            };
            let (bg, fg) = on_fill(theme, bg);
            fill_rrect(scene, tf, bg, flag, 4.0);
            let words = format!("CLIP {:+.1}", self.mix.worst_over_db());
            let at = text_centre(flag, &words, 9.0, TextWeight::Semibold, tr);
            text_w(
                scene,
                tr,
                tf,
                &words,
                9.0,
                fg,
                at.x,
                at.y,
                TextWeight::Semibold,
            );
        } else {
            // The peak-hold, or an honest blank. "-∞" would say the mix is
            // silent; nothing has been measured, which is a different claim.
            let words = self
                .mix
                .hold_db()
                .map_or_else(|| "—".to_string(), |db| format!("{db:.1} dB"));
            let (w, _) = tr.measure(&words, 9.0, None);
            text(
                scene,
                tr,
                tf,
                &words,
                9.0,
                c.text_dim,
                flag.x1 - f64::from(w),
                strip.y0 + 5.0,
            );
        }
        draw_level_meter(
            scene,
            tf,
            theme,
            Rect::new(
                strip.x0 + 12.0,
                strip.y1 - 12.0,
                strip.x1 - 8.0,
                strip.y1 - 5.0,
            ),
            self.mix.master,
            self.mix.master_hold,
            self.mix.clipped(),
        );
    }

    #[allow(
        clippy::indexing_slicing,
        clippy::too_many_lines,
        reason = "timeline rendering indexes validated lane geometry and keeps ruler, headers, clips, and scrollbars in one coordinate pass"
    )]
    fn draw_timeline(
        &self,
        scene: &mut Scene,
        tf: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
        r: &Regions,
    ) {
        let c = &theme.tokens.colors;
        fill_rect(scene, tf, c.surfaces.base, r.timeline);
        stroke_line(
            scene,
            tf,
            c.border_standard,
            r.timeline.x0,
            r.timeline.y0,
            r.timeline.x1,
            r.timeline.y0,
            1.0,
        );

        // --- master strip (the corner above the track heads) ---
        self.draw_master_strip(scene, tf, tr, theme, r);

        // --- ruler ticks ---
        fill_rect(scene, tf, c.surfaces.raised, r.ruler);
        // Ruler labels are rendered at the user's text scale while time is
        // measured in physical timeline pixels.  Ask for proportionally more
        // spacing so large labels remain individual readings instead of a
        // single unreadable run of digits.
        let tick_step = nice_tick(self.zoom / self.text_scale);
        let start_t = self.scroll.max(0.0);
        let end_t = self.x_to_time(r.lanes.x1, &r.lanes);
        let mut t = (start_t / tick_step).floor() * tick_step;
        while t <= end_t {
            let x = self.time_to_x(t, &r.lanes);
            if x >= r.ruler.x0 - 1.0 && x <= r.ruler.x1 {
                stroke_line(
                    scene,
                    tf,
                    c.border_standard,
                    x,
                    r.ruler.y1 - 8.0,
                    x,
                    r.ruler.y1,
                    1.0,
                );
                let label = timecode(t, self.project.fps);
                let (label_w, _) = tr.measure(&label, 10.0, None);
                // The fit-to-window chip owns the ruler's right end.  Tick
                // labels are helpful context, not a reason to draw through a
                // control (especially when text scaling widens both).
                if x + 4.0 + f64::from(label_w) <= self.zoom_chip_rect(r).x0 - 4.0 {
                    text(
                        scene,
                        tr,
                        tf,
                        &label,
                        10.0,
                        c.text_dim,
                        x + 4.0,
                        r.ruler.y0 + 6.0,
                    );
                }
                // faint grid line down the lanes
                stroke_line(
                    scene,
                    tf,
                    with_alpha(c.border_standard, 0.4),
                    x,
                    r.lanes.y0,
                    x,
                    r.timeline.y1,
                    1.0,
                );
            }
            t += tick_step;
        }

        // --- where the mix went over ---
        // The other half of the warning. Knowing the mix clipped and not
        // knowing where is knowing you have to scrub ninety seconds; these
        // marks put it on the ruler, in the same place a person already looks
        // to find a moment, and they stay there until the levels change.
        for over in &self.mix.overs {
            let x = self.time_to_x(*over, &r.lanes);
            if x < r.ruler.x0 || x > r.ruler.x1 {
                continue;
            }
            fill_rect(
                scene,
                tf,
                c.semantic.danger,
                Rect::new(x - 1.5, r.ruler.y0 + 2.0, x + 1.5, r.ruler.y0 + 9.0),
            );
        }

        // --- render range ---
        // The in / out marks the export honours, drawn before the markers so
        // a marker flag sits over the band rather than under it. A range that
        // is set has to be visible: an export that is eight seconds long when
        // the timeline is ninety needs a reason on screen.
        if self.project.has_marks() {
            let (from, to) = self.project.export_range();
            let ax = self.time_to_x(from, &r.lanes).clamp(r.lanes.x0, r.lanes.x1);
            let bx = self.time_to_x(to, &r.lanes).clamp(r.lanes.x0, r.lanes.x1);
            if bx > ax {
                fill_rect(
                    scene,
                    tf,
                    c.accent[5],
                    Rect::new(ax, r.ruler.y0, bx, r.ruler.y0 + 3.0),
                );
            }
            // A bracket at each mark, opening towards the span it keeps, and
            // a hairline down the lanes so the mark can be lined up against
            // every track at once — the same story the marker flags tell.
            for (mark, towards) in [(self.project.mark_in, 1.0), (self.project.mark_out, -1.0)] {
                let Some(t) = mark else { continue };
                let x = self.time_to_x(t, &r.lanes);
                if x < r.lanes.x0 - 8.0 || x > r.lanes.x1 + 8.0 {
                    continue;
                }
                let far = x + towards * 8.0;
                fill_rect(
                    scene,
                    tf,
                    c.accent[5],
                    Rect::new(x.min(far), r.ruler.y0, x.max(far), r.ruler.y0 + 8.0),
                );
                // Only down the ruler. A line carried into the lanes is
                // what the playhead and a marker are, and a third one in the
                // same colour reads as one of those rather than as a mark;
                // the scrim below already draws the boundary across the
                // lanes.
                stroke_line(scene, tf, c.accent[5], x, r.ruler.y0, x, r.ruler.y1, 2.0);
            }
        }

        // --- markers ---
        // Every marker on one absolute timeline: the project's own, plus
        // each clip's rebased onto where that clip sits. A flag on the
        // ruler with a hairline down the lanes, so a marked instant can be
        // lined up against every track at once.
        for (at, marker, on_clip) in self.project.all_markers() {
            let x = self.time_to_x(at, &r.lanes);
            if x < r.ruler.x0 - 1.0 || x > r.ruler.x1 {
                continue;
            }
            let col = clip_color(theme, marker.palette);
            stroke_line(
                scene,
                tf,
                with_alpha(col, 0.55),
                x,
                r.lanes.y0,
                x,
                r.timeline.y1,
                1.0,
            );
            // A clip's marker is drawn as a smaller tick than a project
            // one: it belongs to the footage, not to the timeline.
            let h = if on_clip.is_some() { 6.0 } else { 10.0 };
            let flag = Rect::new(x, r.ruler.y1 - h, x + 3.0, r.ruler.y1);
            fill_rect(scene, tf, col, flag);
            if on_clip.is_none() && !marker.name.is_empty() {
                text(
                    scene,
                    tr,
                    tf,
                    &marker.name,
                    9.0,
                    col,
                    x + 6.0,
                    r.ruler.y1 - 11.0,
                );
            }
        }

        // --- lanes + headers + clips ---
        let lanes = self.lane_rects(r);
        let chrome_scale = self.chrome_scale();
        for (tid, kind, lane) in &lanes {
            // lane background stripe
            let stripe = if *kind == TrackKind::Video {
                c.lift(0.35)
            } else {
                with_alpha(c.semantic.success, 0.05)
            };
            fill_rect(
                scene,
                tf,
                stripe,
                Rect::new(r.lanes.x0, lane.y0, r.lanes.x1, lane.y1),
            );
            // header
            let header = Rect::new(r.gutter.x0, lane.y0, r.gutter.x1, lane.y1);
            fill_rect(scene, tf, c.surfaces.raised, header);
            let track = self.project.track(*tid);
            let name = track.map(|t| t.name.as_str()).unwrap_or("?");
            let large_track_header = self.text_scale > 1.25;
            if large_track_header {
                // The widened S/L/M targets deliberately reserve the right
                // half of the first row.  Centre the short track identity in
                // the left remainder rather than letting its scaled glyphs
                // collide with the Solo control.
                let name_slot = Rect::new(
                    header.x0 + 6.0,
                    header.y0 + 7.0 * chrome_scale,
                    self.solo_pill_rect(header).x0 - 3.0,
                    header.y0 + 7.0 * chrome_scale + self.mute_pill_rect(header).height(),
                );
                let shown = elide_at(name, name_slot.width() as f32, 10.0, tr);
                let at = text_centre(name_slot, &shown, 10.0, TextWeight::Semibold, tr);
                text_w(
                    scene,
                    tr,
                    tf,
                    &shown,
                    10.0,
                    c.text_primary,
                    at.x,
                    at.y,
                    TextWeight::Semibold,
                );
            } else {
                text_w(
                    scene,
                    tr,
                    tf,
                    name,
                    13.0,
                    c.text_primary,
                    header.x0 + 12.0 * chrome_scale,
                    header.y0 + 8.0 * chrome_scale,
                    TextWeight::Semibold,
                );
            }
            // What the lane is, and — when it has been moved off unity or
            // centre — where its fader is. The reading belongs *here*, on the
            // lane, rather than only in a panel: a lane at +6 dB that says so
            // nowhere is how a mix ends up loud for a reason nobody can find.
            let knd = if *kind == TrackKind::Video {
                "VIDEO"
            } else {
                "AUDIO"
            };
            let mut line = knd.to_string();
            if let Some(t) = track.filter(|t| t.mix_is_set()) {
                if t.gain_db.abs() > 1e-9 {
                    line.push_str(&format!(" · {}", t.gain_words()));
                }
                if t.pan.abs() > 1e-9 {
                    line.push_str(&format!(" · {}", t.pan_words()));
                }
            }
            if large_track_header {
                // The compact track code is the identity; the second row
                // preserves the explicit VIDEO/AUDIO type without trying to
                // squeeze gain/pan prose under a full-size control row.
                let line = elide_at(knd, (header.width() - 12.0 * chrome_scale) as f32, 8.0, tr);
                text(
                    scene,
                    tr,
                    tf,
                    &line,
                    8.0,
                    c.text_dim,
                    header.x0 + 8.0 * chrome_scale,
                    header.y0 + 31.0 * chrome_scale,
                );
            } else {
                let line = elide_at(
                    &line,
                    (header.x1 - 10.0 * chrome_scale - (header.x0 + 12.0 * chrome_scale)) as f32,
                    9.0,
                    tr,
                );
                text(
                    scene,
                    tr,
                    tf,
                    &line,
                    9.0,
                    c.text_dim,
                    header.x0 + 12.0 * chrome_scale,
                    header.y0 + 26.0 * chrome_scale,
                );
            }
            // The lane's level. Fed from the mix that made the sound, so a
            // bar moving here is the same measurement the master meter and
            // the clip warning are reading — never a second opinion.
            //
            // Drawn only on a lane that can carry sound. A picture lane whose
            // footage is silent has no level and never will, and a bar that
            // can never move does not read as "quiet" — it reads as an output
            // that is broken, which is the same lie the master strip already
            // refuses to tell on a timeline with no audio in it.
            if crate::audio::track_carries_sound(&self.project, *tid) {
                let level = self.mix.track(*tid);
                draw_level_meter(
                    scene,
                    tf,
                    theme,
                    self.track_meter_rect(header),
                    level.map(|l| l.level).unwrap_or_default(),
                    level.map(|l| l.hold).unwrap_or(0.0),
                    level.is_some_and(|l| l.clipped),
                );
            }
            // mute + lock pills (clickable toggles)
            let muted = track.map(|t| t.muted).unwrap_or(false);
            let locked = track.map(|t| t.locked).unwrap_or(false);
            let solo = track.map(|t| t.solo).unwrap_or(false);
            for (pill, on, label, on_bg) in [
                (self.mute_pill_rect(header), muted, "M", c.semantic.danger),
                (self.lock_pill_rect(header), locked, "L", c.semantic.warning),
                (self.solo_pill_rect(header), solo, "S", c.semantic.info),
            ] {
                let pill_hover = pill.contains(self.last_mouse);
                let pill_bg = if on {
                    on_bg
                } else if pill_hover {
                    c.lift(0.72)
                } else {
                    c.neutral[3]
                };
                let (pill_bg, on_fg) = on_fill(theme, pill_bg);
                fill_rrect(scene, tf, pill_bg, pill, 4.0);
                let fg = if on { on_fg } else { c.text_dim };
                let at = text_centre(pill, label, 10.0, TextWeight::Regular, tr);
                text(scene, tr, tf, label, 10.0, fg, at.x, at.y);
            }
            let _ = locked;
        }

        // Clip bodies clipped to the lane area so they don't bleed over the gutter/ruler.
        scene.push_layer(
            Fill::NonZero,
            vello::peniko::BlendMode::default(),
            1.0,
            tf,
            &Rect::new(r.lanes.x0, r.lanes.y0, r.lanes.x1, r.timeline.y1),
        );
        let links = LinkMarks::build(&self.project, &self.selection);
        let mut drawn: Vec<(ClipId, Rect)> = Vec::new();
        let mut tied: Vec<(Uuid, Rect)> = Vec::new();
        for (tid, kind, lane) in &lanes {
            for clip in self.project.clips_on(*tid) {
                self.draw_clip(scene, tf, tr, theme, clip, lane, *kind, r, &links);
                let rect = self.clip_rect(clip, lane, r);
                drawn.push((clip.id, rect));
                if let Some(link) = clip.link.filter(|_| links.group_selected(clip)) {
                    tied.push((link, rect));
                }
            }
        }
        // The tie between the halves of a selected shot: a hairline down the
        // gap between their lanes, capped at each end. Drawn only while the
        // group is selected — including all the way through a drag, which is
        // what makes "dragging one drags both" legible while it happens rather
        // than only after it lands.
        tied.sort_by(|a, b| {
            a.0.cmp(&b.0).then_with(|| {
                a.1.y0
                    .partial_cmp(&b.1.y0)
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
        });
        for pair in tied.windows(2) {
            let (link_a, above) = pair[0];
            let (link_b, below) = pair[1];
            if link_a != link_b {
                continue;
            }
            let x = above.x0.max(below.x0).clamp(r.lanes.x0, r.lanes.x1) + 1.0;
            if x <= r.lanes.x0 || x >= r.lanes.x1 {
                continue;
            }
            stroke_line(
                scene,
                tf,
                with_alpha(c.border_focus, 0.85),
                x,
                above.y1,
                x,
                below.y0,
                1.5,
            );
            for y in [above.y1, below.y0] {
                fill_rrect(
                    scene,
                    tf,
                    with_alpha(c.border_focus, 0.9),
                    Rect::new(x - 2.5, y - 1.25, x + 2.5, y + 1.25),
                    1.25,
                );
            }
        }
        // A lane that will not be heard or seen — muted, or not soloed while
        // something else of its kind is — greys back, so the pill state is
        // legible from the clips themselves and not only from the header.
        for (tid, _kind, lane) in &lanes {
            if self.project.track_audible(*tid) {
                continue;
            }
            let full = Rect::new(r.lanes.x0, lane.y0, r.lanes.x1, lane.y1);
            fill_rect(scene, tf, with_alpha(c.surfaces.base, 0.45), full);
        }
        // Everything outside the render range greys back, over the clips it
        // covers: the range is what will be written to a file, and a clip
        // that will not be in it must not look like one that will.
        if self.project.has_marks() {
            let (from, to) = self.project.export_range();
            let ax = self.time_to_x(from, &r.lanes).clamp(r.lanes.x0, r.lanes.x1);
            let bx = self.time_to_x(to, &r.lanes).clamp(r.lanes.x0, r.lanes.x1);
            for out in [
                Rect::new(r.lanes.x0, r.lanes.y0, ax, r.timeline.y1),
                Rect::new(bx, r.lanes.y0, r.lanes.x1, r.timeline.y1),
            ] {
                if out.width() > 0.5 {
                    fill_rect(scene, tf, with_alpha(c.surfaces.base, 0.55), out);
                }
            }
        }
        // A locked lane says so across its whole width, drawn *over* its clips:
        // they grey back under a diagonal hatch, so "my drag did nothing"
        // always has a visible reason rather than reading as a dead app.
        for (tid, _kind, lane) in &lanes {
            if !self.project.track(*tid).map(|t| t.locked).unwrap_or(false) {
                continue;
            }
            let full = Rect::new(r.lanes.x0, lane.y0, r.lanes.x1, lane.y1);
            fill_rect(scene, tf, with_alpha(c.surfaces.base, 0.5), full);
            let h = full.height();
            let mut x = full.x0 - h;
            while x < full.x1 {
                stroke_line(
                    scene,
                    tf,
                    with_alpha(c.text_dim, 0.16),
                    x,
                    full.y1,
                    x + h,
                    full.y0,
                    1.0,
                );
                x += 12.0;
            }
        }
        // Which clips other participants are holding, outlined in their
        // colours. Over the mute/lock scrims, because a peer's selection has
        // to stay legible on a lane that is greyed back -- knowing someone
        // else is working on a locked clip is exactly when it matters.
        crate::presence::draw_peer_clips(
            &mut crate::presence::PeerPaintCtx::new(scene, tf, tr, theme, presence),
            &drawn,
            Rect::new(r.lanes.x0, r.lanes.y0, r.lanes.x1, r.timeline.y1),
        );

        // Live rubber band, over the clips it is sweeping.
        if let Some(band) = self.marquee {
            fill_rect(scene, tf, with_alpha(c.accent[5], 0.24), band);
            stroke_rrect(scene, tf, c.border_focus, band, 2.0, 1.5);
        }
        scene.pop_layer();

        // --- playhead ---
        let px = self.time_to_x(self.playhead, &r.lanes);
        if px >= r.lanes.x0 - 0.5 && px <= r.lanes.x1 + 0.5 {
            stroke_line(
                scene,
                tf,
                c.accent[5],
                px,
                r.ruler.y0 + 4.0,
                px,
                r.timeline.y1,
                1.5,
            );
            let head = triangle_down(px, r.ruler.y0 + 4.0, 10.0);
            scene.fill(Fill::NonZero, tf, c.accent[5], None, &head);
        }
        // playhead timecode tag in ruler
        let tag = timecode(self.playhead, self.project.fps);
        let (tw, _) = tr.measure_weighted(&tag, 10.0, None, TextWeight::Bold);
        let tagx = (px + 6.0)
            .min(r.ruler.x1 - f64::from(tw) - 8.0)
            .max(r.ruler.x0);
        let tagrect = Rect::new(
            tagx - 3.0,
            r.ruler.y0 + 2.0,
            tagx + f64::from(tw) + 5.0,
            r.ruler.y0 + 16.0,
        );
        fill_rrect(scene, tf, c.accent[5], tagrect, 3.0);
        text_w(
            scene,
            tr,
            tf,
            &tag,
            10.0,
            on_fill(theme, c.accent[5]).1,
            tagx,
            r.ruler.y0 + 3.0,
            TextWeight::Bold,
        );

        // Everyone else's playhead, after the local one so a peer parked on
        // the same frame reads as an addition to it rather than as something
        // that replaced it. Drawn through this editor's own seconds-to-pixels
        // mapping, so a peer's line lands on the pixel the local one would at
        // the same instant under the same zoom and scroll.
        crate::presence::draw_peer_playheads(
            &mut crate::presence::PeerPaintCtx::new(scene, tf, tr, theme, presence),
            r.lanes,
            r.timeline.y1,
            |t| self.time_to_x(t, &r.lanes),
        );

        // Zoom readout, doubling as fit-to-window. Without it there was no way
        // to tell how far in you were, and no way back out but wheeling.
        let chip = self.zoom_chip_rect(r);
        let chip_hover = chip.contains(self.last_mouse);
        fill_rrect(
            scene,
            tf,
            if chip_hover {
                c.overlay_medium
            } else {
                c.neutral[3]
            },
            chip,
            5.0,
        );
        stroke_rrect(scene, tf, c.border_standard, chip, 5.0, 1.0);
        let zl = if self.zoom >= 100.0 {
            format!("{:.0} px/s", self.zoom)
        } else {
            format!("{:.1} px/s", self.zoom)
        };
        let at = text_centre(chip, &zl, 10.0, TextWeight::Regular, tr);
        text(scene, tr, tf, &zl, 10.0, c.text_dim, at.x, at.y);

        // Empty timeline: say what to do, rather than showing bare lanes.
        // Through the same shared empty state the preview and the inspector
        // use, so the three empty surfaces of one window are one design.
        if self.project.clips.is_empty() {
            let area = Rect::new(r.lanes.x0, r.lanes.y0, r.lanes.x1, r.timeline.y1);
            let (title, hint) = if self.project.media.is_empty() {
                ("Nothing on the timeline", "Import media, then drag it here")
            } else {
                (
                    "Nothing on the timeline",
                    "Drag a clip from the media bin onto a track",
                )
            };
            ui::widget::EmptyState::paint(
                scene,
                tf,
                tr,
                theme,
                area,
                Some(ui::icons::Icon::Layers),
                title,
                Some(hint),
                None,
                Point::new(f64::MIN, f64::MIN),
            );
        }

        // Add-track buttons in the gutter footer.
        for (kind, rect) in self.add_track_buttons(r) {
            let hover = rect.contains(self.last_mouse);
            fill_rrect(
                scene,
                tf,
                if hover { c.lift(0.72) } else { c.neutral[3] },
                rect,
                6.0,
            );
            stroke_rrect(scene, tf, c.border_standard, rect, 6.0, 1.0);
            // The footer reserves two equally-sized real targets.  At large
            // type the full words exceed those targets and were drawn into
            // one another; V/A remain familiar track vocabulary while the
            // accessible controls continue to say the full action.
            let label = if self.text_scale > 1.25 {
                if kind == TrackKind::Video {
                    "+ V"
                } else {
                    "+ A"
                }
            } else if kind == TrackKind::Video {
                "+ Video"
            } else {
                "+ Audio"
            };
            let at = text_centre(rect, label, 11.0, TextWeight::Regular, tr);
            text(scene, tr, tf, label, 11.0, c.text_primary, at.x, at.y);
        }

        // Horizontal scrollbar, floated over the bottom of the lane area.
        self.draw_scrollbar(scene, tf, theme, ScrollPane::Timeline, r);
    }

    /// Paint one pane's overlay scrollbar. Geometry and thumb come from the
    /// toolkit's shared scrollbar model and the colours from the same theme
    /// tokens `DocumentViewport` reads, so all three bars here are the fleet's
    /// overlay scrollbar rather than three lookalikes.
    fn draw_scrollbar(
        &self,
        scene: &mut Scene,
        tf: Affine,
        theme: &Theme,
        pane: ScrollPane,
        r: &Regions,
    ) {
        let c = &theme.tokens.colors;
        let Some((track, m)) = self.scroll_pane(pane, r) else {
            return;
        };
        let bar = self.scroll_bar(pane);
        let Some(thumb) = bar.thumb_rect(track, m) else {
            return;
        };
        let radius = SCROLLBAR_THICKNESS / 2.0;
        fill_rrect(scene, tf, with_alpha(c.surfaces.base, 0.55), track, radius);
        let col = if bar.is_active() {
            with_alpha(c.text_dim, 0.85)
        } else {
            with_alpha(c.text_dim, 0.55)
        };
        fill_rrect(scene, tf, col, thumb, radius);
    }

    #[allow(
        clippy::too_many_lines,
        reason = "each clip's linked, selected, media, and accessibility treatments must share the exact same lane geometry"
    )]
    fn draw_clip(
        &self,
        scene: &mut Scene,
        tf: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        clip: &Clip,
        lane: &Rect,
        kind: TrackKind,
        r: &Regions,
        links: &LinkMarks,
    ) {
        let c = &theme.tokens.colors;
        let cr = self.clip_rect(clip, lane, r);
        if cr.x1 < r.lanes.x0 || cr.x0 > r.lanes.x1 {
            return;
        }
        let base = clip_color(theme, clip.palette);
        let selected = self.selection.contains(clip.id);
        let hovered = self.hover_clip == Some(clip.id);
        let body = if kind == TrackKind::Video {
            base
        } else {
            darken(base, 0.05)
        };
        let body = if hovered { lighten(body, 0.08) } else { body };
        fill_rrect(
            scene,
            tf,
            with_alpha(
                body,
                if kind == TrackKind::Video {
                    clip.param_at(Param::Opacity, self.playhead) * 0.85 + 0.15
                } else {
                    0.92
                },
            ),
            cr,
            6.0,
        );
        // header strip
        let chrome_scale = self.chrome_scale();
        let strip = self.clip_header_rect(cr);
        fill_rrect(scene, tf, darken(base, 0.3), strip, 6.0);

        // content: filmstrip or waveform
        let inner = Rect::new(cr.x0 + 2.0, strip.y1, cr.x1 - 2.0, cr.y1 - 2.0);
        if inner.height() > 4.0 && inner.width() > 4.0 {
            if kind == TrackKind::Video {
                // A filmstrip: one frame **per cell**, walking the clip's
                // trimmed source window, so the strip shows the shot changing.
                // Tiling one thumbnail across every cell is not a filmstrip;
                // it is one picture repeated. The frames themselves are
                // requested in `request_filmstrips` (which needs `&mut self`).
                let (cell_w, n) = filmstrip_cells(inner);
                let span = clip.out_point - clip.in_point;
                for i in 0..n {
                    let bx = inner.x0 + 4.0 + i as f64 * (cell_w + 2.0);
                    if bx + cell_w > inner.x1 {
                        break;
                    }
                    let cell = Rect::new(bx, inner.y0 + 3.0, bx + cell_w, inner.y1 - 3.0);
                    let t = clip.in_point + ((i as f64 + 0.5) / n as f64) * span;
                    // `frame_at` already degrades exact → nearby-decoded →
                    // the generated representative, so a cell only comes back
                    // empty when the source has no picture at all.
                    match self.thumbnails.frame_at(clip.media, t) {
                        Some(th) => blit_thumb(scene, tf, th, cell, 2.0),
                        None => fill_rrect(scene, tf, lighten(base, 0.16), cell, 2.0),
                    }
                }
                // A source that could not be decoded says so once, over the
                // strip, rather than leaving a row of blanks to be read as
                // "this clip has no picture".
                if self.thumbnails.is_undecodable(clip.media) && cr.width() > 90.0 {
                    let msg = "No preview";
                    let (mw, _) = tr.measure(msg, 10.0, None);
                    text(
                        scene,
                        tr,
                        tf,
                        msg,
                        10.0,
                        with_alpha(Color::WHITE, 0.7),
                        inner.center().x - mw as f64 / 2.0,
                        inner.center().y - 6.0,
                    );
                }
            } else {
                let color = lighten(base, 0.3);
                let media_dur = self
                    .project
                    .media(clip.media)
                    .map(|m| m.duration)
                    .unwrap_or(0.0);
                // Only real peaks are drawn as a waveform. Every other state —
                // still computing, never asked, or unreadable — draws the flat
                // line, because none of them know what the audio looks like.
                match (
                    self.waveforms.state(clip.media),
                    self.waveforms.peaks(clip.media),
                ) {
                    (crate::waveform::WaveformState::Ready, Some(peaks)) => draw_waveform(
                        scene,
                        tf,
                        WaveformPaint {
                            area: inner,
                            peaks,
                            source_in: clip.in_point,
                            source_out: clip.out_point,
                            media_duration: media_dur,
                            // The animated level, for the same reason as the clip
                            // body: a keyframed ramp is heard rising and was drawn
                            // flat.
                            volume: clip.param_at(Param::Volume, self.playhead),
                            color,
                        },
                    ),
                    _ => draw_flat_signal(scene, tf, inner, color),
                }
            }
        }
        // A source the clip cannot draw from: hatch the body so it cannot be
        // mistaken for one whose footage is here, and name which kind of
        // problem it is when the clip is wide enough to hold the words.
        let unusable = if self.media_is_unreadable(clip.media) {
            Some(MEDIA_UNREADABLE)
        } else if self.media_is_projected(clip.media) {
            Some(MEDIA_NOT_LOCAL)
        } else {
            None
        };
        if let Some(msg) = unusable {
            hatch_rect(scene, tf, with_alpha(c.semantic.warning, 0.30), cr, 7.0);
            if cr.width() > 118.0 && inner.height() > 14.0 {
                let (mw, mh) = tr.measure(msg, 10.0, None);
                let pill = Rect::new(
                    inner.center().x - mw as f64 / 2.0 - 6.0,
                    inner.center().y - mh as f64 / 2.0 - 2.0,
                    inner.center().x + mw as f64 / 2.0 + 6.0,
                    inner.center().y + mh as f64 / 2.0 + 2.0,
                );
                fill_rrect(scene, tf, with_alpha(Color::BLACK, 0.55), pill, 4.0);
                text(
                    scene,
                    tr,
                    tf,
                    msg,
                    10.0,
                    with_alpha(Color::WHITE, 0.95),
                    pill.x0 + 6.0,
                    pill.y0 + 2.0,
                );
            }
        }
        // The part of the clip the lane actually shows. The lane layer is
        // clipped, so a clip whose start is scrolled off to the left had its
        // name, its speed badge and its head marker all drawn outside the
        // visible strip — and a clip wider than the viewport rendered as an
        // anonymous coloured bar. Everything below anchors to this, not to
        // the clip's own edges.
        let vx0 = cr.x0.max(r.lanes.x0);
        let vx1 = cr.x1.min(r.lanes.x1);
        let seen = (vx1 - vx0).max(0.0);

        // name label
        let label = self
            .project
            .media(clip.media)
            .map(|m| m.name.clone())
            .unwrap_or_default();
        // A re-timed clip carries a rate badge in the header's right end, so
        // the name has to elide before it rather than running underneath it.
        let badge_w = if clip.speed.is_normal() {
            0.0
        } else {
            tr.measure_weighted(&clip.speed.label(), 9.5, None, TextWeight::Bold)
                .0 as f64
                + 18.0
        };
        // A linked clip wears a chain at the head of its header, so "these two
        // move together" is legible standing still — before anything is
        // dragged and without anything being selected. The name starts after
        // it, and after the transition triangle when the head also carries
        // one, rather than running underneath either.
        let head_col = on_fill(theme, darken(base, 0.3)).1;
        let linked = links.linked(clip);
        let corner = if clip.transition == Transition::None {
            0.0
        } else {
            9.0
        };
        let chain_w = if linked { CHAIN_W } else { 0.0 };
        if linked && seen > corner + CHAIN_W + 6.0 {
            draw_chain(
                scene,
                tf,
                with_alpha(head_col, 0.85),
                vx0 + 4.0 + corner,
                strip.center().y,
            );
        }
        // The width available is the width that can be *seen*, measured from
        // where the name is actually drawn.
        let avail = (seen - 10.0 - corner - chain_w - badge_w).max(4.0) as f32;
        if seen > 30.0 + corner + chain_w {
            let shown = elide_w(&label, avail, tr);
            // The name sits on the clip's header strip, whose colour comes
            // from the user's chosen label — so the text colour has to be
            // resolved against it rather than assumed white.
            text_w(
                scene,
                tr,
                tf,
                &shown,
                11.0,
                head_col,
                vx0 + 5.0 + corner + chain_w,
                cr.y0 + 3.0 * chrome_scale,
                TextWeight::Medium,
            );
        }
        // transition marker at head
        if clip.transition != Transition::None && seen > 16.0 {
            let mk = triangle_corner(vx0, cr.y0, 12.0);
            scene.fill(
                Fill::NonZero,
                tf,
                with_alpha(on_fill(theme, base).1, 0.85),
                None,
                &mk,
            );
        }
        // A re-timed clip says its rate on its own body. Speed changes the
        // clip's *length*, so without a badge a 2x clip is indistinguishable
        // from one that was simply trimmed shorter.
        if !clip.speed.is_normal() && seen > 52.0 {
            let label = clip.speed.label();
            let (lw, _) = tr.measure_weighted(&label, 9.5, None, TextWeight::Bold);
            let pill = Rect::new(
                vx1 - 8.0 - lw as f64 - 10.0,
                cr.y0 + 2.0 * chrome_scale,
                vx1 - 4.0,
                cr.y0 + 16.0 * chrome_scale,
            );
            if pill.x0 > vx0 + 4.0 {
                let bg = darken(base, 0.45);
                fill_rrect(scene, tf, with_alpha(bg, 0.9), pill, 4.0);
                let baseline = text_baseline(pill, 9.5, tr);
                text_w(
                    scene,
                    tr,
                    tf,
                    &label,
                    9.5,
                    on_fill(theme, bg).1,
                    pill.x0 + 5.0,
                    baseline,
                    TextWeight::Bold,
                );
            }
        }
        // Keyframes, as diamonds on a lane along the foot of the clip. This is
        // where animation becomes visible without opening the inspector — and
        // clicking one takes the playhead there, which makes stepping through
        // a curve direct rather than a menu round-trip.
        //
        // The foot, not the header: the header carries the clip's name, and
        // marks laid over it turned "Drone_Coastline.mov" into
        // "Drone_Coastli◆e.mov".
        let key_times = clip.key_times();
        if cr.width() > 12.0 && !key_times.is_empty() && cr.height() > 24.0 {
            let mark = on_fill(theme, base).1;
            let y = cr.y1 - 7.0;
            stroke_line(
                scene,
                tf,
                with_alpha(darken(base, 0.45), 0.75),
                (cr.x0 + 3.0).max(r.lanes.x0),
                y,
                (cr.x1 - 3.0).min(r.lanes.x1),
                y,
                1.0,
            );
            for t in key_times {
                let x = self.time_to_x(t, &r.lanes);
                if x < cr.x0 - 1.0 || x > cr.x1 + 1.0 || x < r.lanes.x0 || x > r.lanes.x1 {
                    continue;
                }
                let s = 3.5;
                let mut d = BezPath::new();
                d.move_to((x, y - s));
                d.line_to((x + s, y));
                d.line_to((x, y + s));
                d.line_to((x - s, y));
                d.close_path();
                scene.fill(Fill::NonZero, tf, with_alpha(mark, 0.95), None, &d);
                scene.stroke(
                    &Stroke::new(1.0),
                    tf,
                    with_alpha(darken(base, 0.5), 0.9),
                    None,
                    &d,
                );
            }
        }
        // Selection. The primary (anchor) clip — the one the inspector edits —
        // keeps the full white outline and trim handles; the rest of a
        // multi-selection gets a lighter outline and no handles, so it is
        // always clear which clip a trim or a grade will land on.
        let primary = self.selection.primary() == Some(clip.id);
        if primary {
            stroke_rrect(scene, tf, c.border_focus, cr, 6.0, 2.0);
            for hx in [cr.x0, cr.x1] {
                let hrect = Rect::new(hx - 3.0, cr.y0 + 2.0, hx + 3.0, cr.y1 - 2.0);
                fill_rrect(scene, tf, c.accent[5], hrect, 2.0);
            }
        } else if selected {
            stroke_rrect(scene, tf, with_alpha(c.border_focus, 0.6), cr, 6.0, 2.0);
        } else if links.group_selected(clip) {
            // The other half of a selected shot. Dashed rather than solid, so
            // "this will move too" never reads as "this is selected" — an edit
            // aimed at it (a grade, a level) still lands on the anchor.
            scene.stroke(
                &Stroke::new(1.6).with_dashes(0.0, [4.0, 3.0]),
                tf,
                with_alpha(c.border_focus, 0.75),
                None,
                &RoundedRect::from_rect(cr, 6.0),
            );
        } else {
            stroke_rrect(scene, tf, with_alpha(c.surfaces.base, 0.55), cr, 6.0, 1.0);
        }
    }

    /// The shortcut sheet.
    ///
    /// Fifteen shortcuts existed and none of them were written down anywhere in
    /// the product, so the only way to learn one was to be told. The sheet is
    /// also how a reader user learns the editor has shortcuts at all, which is
    /// why it is an accessibility affordance rather than a convenience.
    fn draw_help_sheet(
        &self,
        scene: &mut Scene,
        tf: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        _r: &Regions,
    ) {
        let c = &theme.tokens.colors;
        fill_rect(
            scene,
            tf,
            with_alpha(c.surfaces.scrim, MODAL_SCRIM_ALPHA),
            self.rect,
        );
        // Two columns on anything wide enough, one on a narrow window, so the
        // sheet is readable rather than clipped at the phone-ish sizes the rest
        // of the editor already adapts to.
        let two_col = self.rect.width() >= 780.0;
        let w = if two_col {
            700.0
        } else {
            (self.rect.width() - 48.0).max(280.0)
        };
        // Where the second column starts, and how tall the card has to be to
        // hold the taller of the two. Both are computed from the table rather
        // than fixed, because a fixed height and a fixed split silently *cut
        // rows off* the moment a group is added — which is exactly what
        // happened when the project shortcuts arrived and the last two View
        // rows stopped being drawn at all.
        let (split, content_h) = help_columns(two_col);
        let h = if two_col {
            (HELP_TOP + content_h + 16.0).min(self.rect.height() - 32.0)
        } else {
            (self.rect.height() - 48.0).max(320.0)
        };
        let card = Rect::from_center_size(self.rect.center(), (w, h));
        fill_rrect(scene, tf, c.surfaces.raised, card, 14.0);
        stroke_rrect(scene, tf, c.border_standard, card, 14.0, 1.0);
        text_w(
            scene,
            tr,
            tf,
            "Keyboard shortcuts",
            18.0,
            c.text_primary,
            card.x0 + 24.0,
            card.y0 + 20.0,
            TextWeight::Bold,
        );
        text(
            scene,
            tr,
            tf,
            "Esc or ? to close",
            12.0,
            c.text_dim,
            card.x0 + 24.0,
            card.y0 + 44.0,
        );

        let col_w = if two_col { (w - 64.0) / 2.0 } else { w - 48.0 };
        let mut x = card.x0 + 24.0;
        let mut y = card.y0 + HELP_TOP;
        let bottom = card.y1 - 16.0;
        for (gi, (group, rows)) in SHORTCUTS.iter().enumerate() {
            if two_col && gi == split {
                x = card.x0 + 24.0 + col_w + 16.0;
                y = card.y0 + HELP_TOP;
            }
            text_w(
                scene,
                tr,
                tf,
                group,
                11.0,
                c.text_dim,
                x,
                y,
                TextWeight::Bold,
            );
            y += 20.0;
            for (combo, what) in rows.iter() {
                if y > bottom {
                    break;
                }
                text_w(
                    scene,
                    tr,
                    tf,
                    combo,
                    12.0,
                    c.accent[6],
                    x,
                    y,
                    TextWeight::Bold,
                );
                text(
                    scene,
                    tr,
                    tf,
                    what,
                    12.0,
                    c.text_primary,
                    x + 120.0_f64.min(col_w * 0.45),
                    y,
                );
                y += 19.0;
            }
            y += 10.0;
        }
    }

    /// The go-to-timecode field: type a time, press Enter, land there.
    ///
    /// Typing a timecode is table stakes in an editor and there was no way to
    /// do it — the playhead could only be dragged or stepped. A value that
    /// cannot be read keeps the field open and says so, rather than closing and
    /// silently going nowhere.
    fn draw_goto(
        &self,
        scene: &mut Scene,
        tf: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        r: &Regions,
    ) {
        let Some(entry) = self.goto_entry.as_ref() else {
            return;
        };
        let c = &theme.tokens.colors;
        let card = self.goto_layout(r);
        fill_rrect(scene, tf, c.surfaces.raised, card, 12.0);
        stroke_rrect(
            scene,
            tf,
            if self.navigation.goto_notice {
                c.semantic.danger
            } else {
                c.accent[6]
            },
            card,
            12.0,
            1.5,
        );
        text(
            scene,
            tr,
            tf,
            "Go to timecode",
            11.0,
            c.text_dim,
            card.x0 + 16.0,
            card.y0 + 12.0,
        );
        let shown = if entry.is_empty() {
            "0:00"
        } else {
            entry.as_str()
        };
        let col = if entry.is_empty() {
            c.text_dim
        } else {
            c.text_primary
        };
        text_w(
            scene,
            tr,
            tf,
            shown,
            20.0,
            col,
            card.x0 + 16.0,
            card.y0 + 32.0,
            TextWeight::Bold,
        );
        // A caret, so an empty field reads as "type here" rather than as a
        // placeholder someone forgot to remove.
        let (tw, _) = tr.measure(shown, 20.0, None);
        if !entry.is_empty() {
            fill_rect(
                scene,
                tf,
                c.accent[6],
                Rect::new(
                    card.x0 + 18.0 + tw as f64,
                    card.y0 + 32.0,
                    card.x0 + 20.0 + tw as f64,
                    card.y0 + 54.0,
                ),
            );
        }
        let hint = if self.navigation.goto_notice {
            "Not a timecode — try 1:30"
        } else {
            "Enter to jump, Esc to cancel"
        };
        let hc = if self.navigation.goto_notice {
            c.semantic.danger
        } else {
            c.text_dim
        };
        text(
            scene,
            tr,
            tf,
            hint,
            10.0,
            hc,
            card.x0 + 16.0,
            card.y1 - 16.0,
        );
    }

    #[allow(
        clippy::too_many_lines,
        reason = "the export dialog paint pass shares one geometry calculation across its presets, status, and controls"
    )]
    fn draw_export_modal(
        &self,
        scene: &mut Scene,
        tf: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        r: &Regions,
    ) {
        let c = &theme.tokens.colors;
        // Backdrop. `surfaces.overlay` is the opaque deep-menu surface — using
        // it here painted solid black and deleted the editor behind the dialog.
        // The scrim token carries the theme's hue; the strength is ours.
        fill_rect(
            scene,
            tf,
            with_alpha(c.surfaces.scrim, MODAL_SCRIM_ALPHA),
            self.rect,
        );
        let l = self.export_layout(r);
        let card = l.card;
        fill_rrect(scene, tf, c.surfaces.raised, card, 14.0);
        stroke_rrect(scene, tf, c.border_standard, card, 14.0, 1.0);
        let x0 = card.x0 + 24.0;
        text_w(
            scene,
            tr,
            tf,
            "Export Project",
            18.0,
            c.text_primary,
            x0,
            card.y0 + 20.0,
            TextWeight::Bold,
        );
        text(
            scene,
            tr,
            tf,
            &self.export.suggested_filename(&self.project.name),
            12.0,
            c.text_dim,
            x0,
            card.y0 + 44.0,
        );
        // What the levels did, stated **here**, before the button that
        // renders them. A mix that clipped and only said so in the file is
        // the fault this whole surface exists to end, and the dialog is the
        // last place before it becomes one.
        if let Some(words) = self.mix_words() {
            let tint = if self.mix.clipped() {
                c.semantic.danger
            } else {
                c.text_dim
            };
            let words = elide_w(&words, (card.width() - 48.0) as f32, tr);
            text(scene, tr, tf, &words, 10.5, tint, x0, card.y0 + 60.0);
        }

        let seg_group = |scene: &mut Scene,
                         tr: &mut TextRenderer,
                         label: &str,
                         items: &[(String, Rect, bool)]| {
            if let Some((_, first, _)) = items.first() {
                text(
                    scene,
                    tr,
                    tf,
                    label,
                    11.0,
                    c.text_dim,
                    first.x0,
                    first.y0 - 18.0,
                );
            }
            for (name, rect, on) in items {
                let bg = if *on { c.accent[5] } else { c.neutral[3] };
                fill_rrect(scene, tf, bg, *rect, 6.0);
                let col = if *on {
                    on_fill(theme, c.accent[5]).1
                } else {
                    c.text_dim
                };
                let (lw, _) = tr.measure(name, 12.0, None);
                text(
                    scene,
                    tr,
                    tf,
                    name,
                    12.0,
                    col,
                    rect.center().x - f64::from(lw) / 2.0,
                    rect.center().y - 7.0,
                );
            }
        };
        let res: Vec<_> = l
            .resolution
            .iter()
            .map(|(v, r)| (v.label().to_string(), *r, *v == self.export.resolution))
            .collect();
        seg_group(scene, tr, "RESOLUTION", &res);
        let con: Vec<_> = l
            .container
            .iter()
            .map(|(v, r)| (v.label().to_string(), *r, *v == self.export.container))
            .collect();
        seg_group(scene, tr, "FORMAT", &con);
        let fps: Vec<_> = l
            .fps
            .iter()
            .map(|(v, r)| {
                (
                    format!("{} fps", v.label()),
                    *r,
                    *v == self.export.frame_rate,
                )
            })
            .collect();
        seg_group(scene, tr, "FRAME RATE", &fps);

        // Estimate + status.
        // One source of truth for the size line: the export module knows which
        // encoder will run here and whether a size can honestly be predicted
        // from the rate it is given, which this dialog does not.
        let est = self.export.estimate_label(&self.project);
        // What will actually be written, not the preset's own 16:9 pair: the
        // output keeps the *project's* aspect, so a vertical project writes a
        // vertical file and the dialog has to say so.
        let (w, h) = crate::export::output_dims(&self.export, &self.project);
        // And how long it will run — the marked range when there is one,
        // which is the whole reason for marking one.
        let info = format!(
            "{est}   ·   {w}×{h}   ·   {}{}",
            timecode(self.project.export_duration(), self.project.fps),
            if self.project.has_marks() {
                " (marked)"
            } else {
                ""
            }
        );
        text(scene, tr, tf, &info, 12.0, c.text_dim, x0, card.y1 - 96.0);
        match &self.export_status {
            ExportStatus::Rendering { progress } => {
                let bar = Rect::new(x0, card.y1 - 74.0, card.x1 - 24.0, card.y1 - 66.0);
                fill_rrect(scene, tf, c.neutral[3], bar, 4.0);
                let f = Rect::new(
                    bar.x0,
                    bar.y0,
                    bar.x0 + bar.width() * progress.max(0.04),
                    bar.y1,
                );
                fill_rrect(scene, tf, c.accent[5], f, 4.0);
                text(
                    scene,
                    tr,
                    tf,
                    "Rendering…",
                    10.5,
                    c.text_dim,
                    x0,
                    card.y1 - 62.0,
                );
            }
            ExportStatus::Done { path } => {
                text(
                    scene,
                    tr,
                    tf,
                    &format!("Done: {path}"),
                    11.0,
                    c.semantic.success,
                    x0,
                    card.y1 - 68.0,
                );
            }
            ExportStatus::Failed { reason } => {
                // Danger, not warning: the export did not happen. The status
                // slot is deliberately bounded so an encoder's untrusted
                // error cannot paint through the buttons; the full, unelided
                // reason remains in the dialog's accessible static-text node.
                let lines = tr.clamp_to_lines(reason, 10.5, (card.width() - 48.0) as f32, 2);
                for (line, words) in lines.iter().enumerate() {
                    text(
                        scene,
                        tr,
                        tf,
                        words,
                        10.5,
                        c.semantic.danger,
                        x0,
                        card.y1 - 80.0 + line as f64 * 14.0,
                    );
                }
            }
            ExportStatus::Idle => {
                // One source of truth for the availability copy — the export
                // module knows what it can actually do, this dialog does not.
                if let Some(reason) = crate::export::unavailable_reason() {
                    text(scene, tr, tf, reason, 10.5, c.text_dim, x0, card.y1 - 68.0);
                }
            }
        }
        // The flow's own status (choosing a destination, a portal refusal)
        // sits under the render status, so an action is never silent.
        if let Some(notice) = &self.export_notice {
            let shown = elide_w(notice, (card.width() - 48.0) as f32, tr);
            text(scene, tr, tf, &shown, 10.5, c.text_dim, x0, card.y1 - 82.0);
        }
        // A render in flight turns Close into Cancel — the only way to stop it.
        let running = self.export_running();
        draw_button(
            scene,
            tf,
            tr,
            theme,
            l.cancel,
            if running { "Cancel" } else { "Close" },
            false,
            l.cancel.contains(self.last_mouse),
        );
        // Export is offered only when it can actually happen. The dialog used
        // to say "Export needs the GStreamer build" and put a bright, enabled
        // Export button directly underneath it.
        let can_export = self.export_is_offered();
        draw_button_enabled(
            scene,
            tf,
            tr,
            theme,
            l.confirm,
            if running { "Rendering…" } else { "Export" },
            true,
            l.confirm.contains(self.last_mouse),
            can_export,
        );
    }
}

#[cfg(test)]
mod tests;
