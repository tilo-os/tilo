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

mod accessibility;
mod bin_paint;
mod inspector_layout;
mod inspector_paint;
mod interaction;
mod layout;
mod paint;
mod project_workflow;
mod scenario;
mod selection;
use inspector_layout::InspectorLayout;
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

impl VideoEditorApp {
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
