//! Pure data model for the editor — media bin, tracks, clips, project.
//!
//! This layer is rendering- and CRDT-free: it is the in-memory mirror of
//! the Automerge document (see [`crate::doc`]). The app reloads it from
//! the document every `sync`, mutates the document on every edit, then
//! reloads — so this struct is always a faithful projection of shared
//! state. All the non-trivial editing math (split, trim, move, ripple
//! duration) lives here behind unit tests so the widget layer stays thin.

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::media::{self, Hydration};
use crate::param::{ClipAnimation, Interp, Param};

pub type MediaId = Uuid;
pub type TrackId = Uuid;
pub type ClipId = Uuid;
pub type MarkerId = Uuid;

/// Smallest clip we allow a trim/split to produce, in seconds. Stops a
/// drag from collapsing a clip to a zero-width sliver that can't be
/// grabbed again.
pub const MIN_CLIP: f64 = 0.10;

/// Per-clip colour + blend filters. All are linear, normalised so the
/// inspector sliders run 0..1 and a fresh clip is the identity:
///   * `brightness`/`contrast`/`saturation`: 0.5 = unchanged, 0 = none,
///     1 = doubled. (Stored as 0..1 so one slider widget fits them all.)
///
/// These are render parameters only — the model stores them, the preview
/// applies them, and they replicate field-by-field through the CRDT so two
/// peers grading the same clip converge.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Filters {
    pub brightness: f64,
    pub contrast: f64,
    pub saturation: f64,
}

impl Default for Filters {
    fn default() -> Self {
        // 0.5 is the neutral midpoint of every slider.
        Self { brightness: 0.5, contrast: 0.5, saturation: 0.5 }
    }
}

impl Filters {
    /// Identity filters leave the frame untouched.
    pub fn is_identity(&self) -> bool {
        (self.brightness - 0.5).abs() < 1e-6
            && (self.contrast - 0.5).abs() < 1e-6
            && (self.saturation - 0.5).abs() < 1e-6
    }
    /// Map a 0..1 slider value to a multiplier in roughly 0..2 (0.5→1.0).
    pub fn gain(v: f64) -> f64 {
        (v.clamp(0.0, 1.0) * 2.0).max(0.0)
    }
}

/// A stackable per-clip image effect, applied to the frame *after* the base
/// colour grade ([`Filters`]). Unlike the grade (three fixed axes), effects are
/// an ordered list the user builds: add, remove and reorder. `amount` is the
/// effect's intensity in 0..1 (1 = full).
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct ClipEffect {
    pub kind: EffectKind,
    pub amount: f64,
}

impl ClipEffect {
    pub fn new(kind: EffectKind) -> Self {
        Self { kind, amount: kind.default_amount() }
    }
}

/// The kinds of stackable effect a clip can carry. Each transforms the graded
/// colour; the preview and the (gated) export apply them in list order.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum EffectKind {
    Grayscale,
    Sepia,
    Invert,
    Warm,
    Cool,
    Vignette,
}

impl EffectKind {
    pub const ALL: [EffectKind; 6] = [
        EffectKind::Grayscale,
        EffectKind::Sepia,
        EffectKind::Invert,
        EffectKind::Warm,
        EffectKind::Cool,
        EffectKind::Vignette,
    ];
    pub fn as_str(self) -> &'static str {
        match self {
            EffectKind::Grayscale => "grayscale",
            EffectKind::Sepia => "sepia",
            EffectKind::Invert => "invert",
            EffectKind::Warm => "warm",
            EffectKind::Cool => "cool",
            EffectKind::Vignette => "vignette",
        }
    }
    pub fn label(self) -> &'static str {
        match self {
            EffectKind::Grayscale => "Grayscale",
            EffectKind::Sepia => "Sepia",
            EffectKind::Invert => "Invert",
            EffectKind::Warm => "Warm",
            EffectKind::Cool => "Cool",
            EffectKind::Vignette => "Vignette",
        }
    }
    pub fn from_str(s: &str) -> Option<Self> {
        Some(match s.trim().to_lowercase().as_str() {
            "grayscale" | "greyscale" | "grey" | "gray" | "b&w" => EffectKind::Grayscale,
            "sepia" => EffectKind::Sepia,
            "invert" | "negative" => EffectKind::Invert,
            "warm" => EffectKind::Warm,
            "cool" => EffectKind::Cool,
            "vignette" => EffectKind::Vignette,
            _ => return None,
        })
    }
    /// A sensible default intensity when the effect is first added.
    pub fn default_amount(self) -> f64 {
        match self {
            EffectKind::Vignette => 0.5,
            _ => 0.8,
        }
    }
}

/// Where a clip's picture lands inside the project frame.
///
/// The identity transform is the aspect-fit ("contain") placement the
/// compositor has always used, so a clip that has never been touched
/// renders exactly as before. Everything is expressed in **frame-relative**
/// units rather than pixels, so changing the project resolution rescales a
/// composition instead of breaking it.
///
/// Order of operations, source → screen: crop the source, translate the
/// anchor to the origin, scale, rotate, then offset by `pos`.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Transform {
    /// Horizontal scale about the anchor. 1.0 = the aspect-fit size.
    pub scale_x: f64,
    /// Vertical scale about the anchor.
    pub scale_y: f64,
    /// Offset in frame widths / heights. `(0, 0)` is centred; `(0.25, 0)`
    /// is a quarter of the frame to the right. Y grows downward, matching
    /// the frame's pixel coordinates.
    pub pos_x: f64,
    pub pos_y: f64,
    /// Clockwise rotation in degrees about the anchor.
    pub rotation: f64,
    /// The point of the source that scale and rotation pivot around and
    /// that `pos` places, in normalised source coordinates. `(0.5, 0.5)`
    /// is the centre.
    pub anchor_x: f64,
    pub anchor_y: f64,
    /// Fraction of the source cut off each edge before it is placed.
    /// Cropping removes picture; it does not move or resize what remains.
    pub crop_left: f64,
    pub crop_right: f64,
    pub crop_top: f64,
    pub crop_bottom: f64,
}

impl Default for Transform {
    fn default() -> Self {
        Self {
            scale_x: 1.0,
            scale_y: 1.0,
            pos_x: 0.0,
            pos_y: 0.0,
            rotation: 0.0,
            anchor_x: 0.5,
            anchor_y: 0.5,
            crop_left: 0.0,
            crop_right: 0.0,
            crop_top: 0.0,
            crop_bottom: 0.0,
        }
    }
}

impl Transform {
    /// True when this transform is the plain aspect-fit — the compositor
    /// can then take its original path and the inspector can show
    /// "untransformed".
    pub fn is_identity(&self) -> bool {
        (self.scale_x - 1.0).abs() < 1e-9
            && (self.scale_y - 1.0).abs() < 1e-9
            && self.pos_x.abs() < 1e-9
            && self.pos_y.abs() < 1e-9
            && self.rotation.abs() < 1e-9
            && (self.anchor_x - 0.5).abs() < 1e-9
            && (self.anchor_y - 0.5).abs() < 1e-9
            && !self.is_cropped()
    }

    pub fn is_cropped(&self) -> bool {
        self.crop_left.abs() > 1e-9
            || self.crop_right.abs() > 1e-9
            || self.crop_top.abs() > 1e-9
            || self.crop_bottom.abs() > 1e-9
    }

    /// The visible source rect after cropping, as `(left, top, right,
    /// bottom)` in normalised source coordinates. Crops that would meet or
    /// cross are clamped so the rect always has area — a fully cropped-away
    /// clip would just be an invisible layer, and a degenerate rect would
    /// make the shader's inside-test meaningless.
    pub fn crop_rect(&self) -> (f64, f64, f64, f64) {
        let l = self.crop_left.clamp(0.0, 1.0);
        let t = self.crop_top.clamp(0.0, 1.0);
        let r = (1.0 - self.crop_right).clamp(0.0, 1.0);
        let b = (1.0 - self.crop_bottom).clamp(0.0, 1.0);
        // Keep at least a sliver in each axis.
        let (l, r) = if r - l < 1e-4 { (l.min(1.0 - 1e-4), l.min(1.0 - 1e-4) + 1e-4) } else { (l, r) };
        let (t, b) = if b - t < 1e-4 { (t.min(1.0 - 1e-4), t.min(1.0 - 1e-4) + 1e-4) } else { (t, b) };
        (l, t, r, b)
    }

    /// A picture-in-picture placement: `scale` of the frame, tucked into a
    /// corner with `inset` (in frame widths) of margin. `corner` is
    /// `(x, y)` each in `-1..1`, so `(1, -1)` is top-right.
    pub fn pip(scale: f64, corner: (f64, f64), inset: f64) -> Self {
        let s = scale.clamp(0.02, 1.0);
        // The half-extent of the scaled clip in frame units, plus the
        // margin, is how far from the centre its centre must sit.
        let off = (0.5 - s * 0.5 - inset).max(0.0);
        Self {
            scale_x: s,
            scale_y: s,
            pos_x: corner.0.clamp(-1.0, 1.0) * off,
            pos_y: corner.1.clamp(-1.0, 1.0) * off,
            ..Self::default()
        }
    }
}

/// How fast a clip plays, and in which direction.
///
/// `rate` is a multiplier on real time: 2.0 plays twice as fast, so the
/// same source window occupies half as long on the timeline. This is why
/// [`Clip::duration`] divides by the rate — speeding a clip up shortens
/// it, exactly as dragging its speed handle should.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Speed {
    pub rate: f64,
    /// Play the source window backwards. The clip still occupies the same
    /// stretch of timeline; it just reads from the out-point down.
    pub reverse: bool,
}

impl Default for Speed {
    fn default() -> Self {
        Self { rate: 1.0, reverse: false }
    }
}

/// Slowest and fastest rate the model accepts. Below the floor a clip's
/// timeline length explodes; above the ceiling the source is skipped so
/// coarsely that the result is noise.
pub const MIN_SPEED: f64 = 0.05;
pub const MAX_SPEED: f64 = 20.0;

impl Speed {
    pub fn new(rate: f64, reverse: bool) -> Self {
        Self { rate: rate.clamp(MIN_SPEED, MAX_SPEED), reverse }
    }
    /// The rate, guaranteed usable as a divisor.
    pub fn rate(&self) -> f64 {
        if self.rate.is_finite() { self.rate.clamp(MIN_SPEED, MAX_SPEED) } else { 1.0 }
    }
    pub fn is_normal(&self) -> bool {
        !self.reverse && (self.rate() - 1.0).abs() < 1e-9
    }
    /// Signed rate: negative when playing backwards. What the source-time
    /// map and the audio resampler both step by.
    pub fn signed(&self) -> f64 {
        if self.reverse { -self.rate() } else { self.rate() }
    }
    /// A short label like `2×` or `0.5× rev` for the inspector and the
    /// timeline badge.
    pub fn label(&self) -> String {
        let r = self.rate();
        let n = if (r - r.round()).abs() < 1e-6 {
            format!("{}×", r.round() as i64)
        } else {
            format!("{r:.2}×")
        };
        if self.reverse { format!("{n} rev") } else { n }
    }
}

/// A named point of interest, at a time. Timeline markers hang off the
/// project in absolute seconds; clip markers hang off a clip in clip-local
/// seconds so they travel with the footage.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Marker {
    pub id: Uuid,
    /// Seconds. Absolute for a project marker, clip-local for a clip one.
    pub t: f64,
    pub name: String,
    /// Palette slot, resolved to a real colour by the theme — the model
    /// never stores raw RGB, so markers restyle with the OS.
    pub palette: u8,
    /// Optional free-text note.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub note: String,
}

impl Marker {
    pub fn new(t: f64, name: impl Into<String>, palette: u8) -> Self {
        Self { id: Uuid::new_v4(), t, name: name.into(), palette, note: String::new() }
    }
}

/// Horizontal placement of a title's lines within its box.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum TextAlign {
    Left,
    #[default]
    Center,
    Right,
}

impl TextAlign {
    pub const ALL: [TextAlign; 3] = [TextAlign::Left, TextAlign::Center, TextAlign::Right];
    pub fn as_str(self) -> &'static str {
        match self {
            TextAlign::Left => "left",
            TextAlign::Center => "center",
            TextAlign::Right => "right",
        }
    }
    pub fn label(self) -> &'static str {
        match self {
            TextAlign::Left => "Left",
            TextAlign::Center => "Center",
            TextAlign::Right => "Right",
        }
    }
    pub fn from_str(s: &str) -> Self {
        match s {
            "left" => TextAlign::Left,
            "right" => TextAlign::Right,
            _ => TextAlign::Center,
        }
    }
}

/// A title: the text of a [`MediaKind::Text`] source and how it is set.
///
/// Sizes are fractions of the **frame height**, not pixels, so a title
/// composed against a 1080p project still reads correctly when the project
/// is switched to 4K. The colour is stored explicitly (unlike clip palette
/// slots) because a title's colour is authored content, not chrome — the
/// user picked it and it must survive a theme change.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TextSource {
    pub content: String,
    /// Cap height as a fraction of the frame height. 0.1 ≈ 108px at 1080p.
    pub size: f64,
    /// Straight (non-premultiplied) sRGB in 0..1, plus alpha.
    pub color: [f64; 4],
    pub align: TextAlign,
    /// Font family. Empty means "the system's default sans", which is what
    /// a box with no fonts installed can actually honour.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub font: String,
    #[serde(default)]
    pub bold: bool,
    #[serde(default)]
    pub italic: bool,
    /// Opacity of a full-frame backing plate behind the text, 0 = none.
    #[serde(default)]
    pub backdrop: f64,
}

impl Default for TextSource {
    fn default() -> Self {
        Self {
            content: String::new(),
            size: 0.12,
            color: [1.0, 1.0, 1.0, 1.0],
            align: TextAlign::Center,
            font: String::new(),
            bold: true,
            italic: false,
            backdrop: 0.0,
        }
    }
}

impl TextSource {
    pub fn new(content: impl Into<String>) -> Self {
        Self { content: content.into(), ..Self::default() }
    }
    /// A cache key covering everything that changes the rasterised picture.
    /// The frame size is the caller's to add.
    pub fn raster_key(&self) -> String {
        format!(
            "{}|{:.5}|{:?}|{}|{}|{}|{}|{:.4}",
            self.content,
            self.size,
            self.color,
            self.align.as_str(),
            self.font,
            self.bold,
            self.italic,
            self.backdrop,
        )
    }
}

/// What a piece of source media is. Drives the bin icon, whether it
/// lands on a video or audio track by default, and which inspector
/// controls (opacity vs. volume) apply.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum MediaKind {
    Video,
    Audio,
    Image,
    /// A title. Its picture is rasterised from the item's [`TextSource`]
    /// rather than decoded from a file, so it has no path to hydrate.
    Text,
}

impl MediaKind {
    pub fn as_str(self) -> &'static str {
        match self {
            MediaKind::Video => "video",
            MediaKind::Audio => "audio",
            MediaKind::Image => "image",
            MediaKind::Text => "text",
        }
    }
    pub fn from_str(s: &str) -> Self {
        match s {
            "audio" => MediaKind::Audio,
            "image" => MediaKind::Image,
            "text" => MediaKind::Text,
            _ => MediaKind::Video,
        }
    }
    /// Does this media carry a picture? Titles, images and video do; audio
    /// does not.
    pub fn is_visual(self) -> bool {
        matches!(self, MediaKind::Video | MediaKind::Image | MediaKind::Text)
    }
    /// Is this media's picture generated from the document rather than
    /// decoded from bytes? A generated source is never "un-hydrated" — it
    /// is always available, on any peer, with no decoder.
    pub fn is_generated(self) -> bool {
        matches!(self, MediaKind::Text)
    }
    pub fn label(self) -> &'static str {
        match self {
            MediaKind::Video => "Video",
            MediaKind::Audio => "Audio",
            MediaKind::Image => "Image",
            MediaKind::Text => "Title",
        }
    }
}

/// A track is a horizontal lane. Video lanes stack above audio lanes;
/// `index` orders within a kind (V1 below V2, A1 above A2).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum TrackKind {
    Video,
    Audio,
}

impl TrackKind {
    pub fn as_str(self) -> &'static str {
        match self {
            TrackKind::Video => "video",
            TrackKind::Audio => "audio",
        }
    }
    pub fn from_str(s: &str) -> Self {
        match s {
            "audio" => TrackKind::Audio,
            _ => TrackKind::Video,
        }
    }
}

/// Edit-point transition applied at a clip's head. `None` is a hard cut.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Transition {
    None,
    CrossDissolve,
    FadeIn,
    DipToBlack,
    Wipe,
}

impl Transition {
    pub const ALL: [Transition; 5] = [
        Transition::None,
        Transition::CrossDissolve,
        Transition::FadeIn,
        Transition::DipToBlack,
        Transition::Wipe,
    ];
    pub fn as_str(self) -> &'static str {
        match self {
            Transition::None => "none",
            Transition::CrossDissolve => "dissolve",
            Transition::FadeIn => "fade",
            Transition::DipToBlack => "dip",
            Transition::Wipe => "wipe",
        }
    }
    pub fn label(self) -> &'static str {
        match self {
            Transition::None => "Cut",
            Transition::CrossDissolve => "Dissolve",
            Transition::FadeIn => "Fade",
            Transition::DipToBlack => "Dip",
            Transition::Wipe => "Wipe",
        }
    }
    pub fn from_str(s: &str) -> Self {
        match s {
            "dissolve" => Transition::CrossDissolve,
            "fade" => Transition::FadeIn,
            "dip" => Transition::DipToBlack,
            "wipe" => Transition::Wipe,
            _ => Transition::None,
        }
    }
}

/// An imported source clip sitting in the media bin. `duration` is the
/// full source length; clips reference a sub-range of it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct MediaItem {
    pub id: MediaId,
    pub name: String,
    pub kind: MediaKind,
    pub duration: f64,
    pub width: u32,
    pub height: u32,
    pub fps: f64,
    /// Palette slot (0..N) used to tint the bin thumbnail and timeline
    /// clip body. Resolved to a real `Color` from the active theme — the
    /// model never stores raw RGB so theming stays OS-controlled.
    pub palette: u8,
    /// Workspace-relative path to the source bytes on the P2P FUSE
    /// (`/var/workspace/<path>`). This — not the bytes — is what the CRDT
    /// replicates; the decode path hydrates the bytes on demand. See
    /// [`crate::media`] for the seam.
    pub path: String,
    /// Whether the source bytes are hydrated locally yet. Defaults to
    /// `Projected`; the decode path flips it to `Local` once the FUSE
    /// resolves real bytes. View-derived, but persisted so a re-opened
    /// project remembers what was already fetched.
    #[serde(default = "default_hydration", with = "hydration_serde")]
    pub hydration: Hydration,
    /// The title's text and styling, for a [`MediaKind::Text`] item. `None`
    /// for every decoded source — a title is the one media whose picture
    /// comes out of the document instead of out of a file.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<TextSource>,
}

impl Default for MediaItem {
    fn default() -> Self {
        Self {
            id: Uuid::nil(),
            name: String::new(),
            kind: MediaKind::Video,
            duration: 0.0,
            width: 0,
            height: 0,
            fps: 30.0,
            palette: 0,
            path: String::new(),
            hydration: Hydration::Projected,
            text: None,
        }
    }
}

fn default_hydration() -> Hydration {
    Hydration::Projected
}

mod hydration_serde {
    use super::Hydration;
    use serde::{Deserialize, Deserializer, Serializer};
    pub fn serialize<S: Serializer>(h: &Hydration, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(h.as_str())
    }
    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Hydration, D::Error> {
        Ok(Hydration::from_str(&String::deserialize(d)?))
    }
}

impl MediaItem {
    /// Resolve this item's bytes through the FUSE-media seam, reporting
    /// where they live and whether they're hydrated. The one place a
    /// reference becomes a (possibly-local) file.
    pub fn source(&self) -> media::MediaSource {
        media::resolve(&self.path)
    }
}

/// A track instance in the timeline.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Track {
    pub id: TrackId,
    pub name: String,
    pub kind: TrackKind,
    pub index: u32,
    pub muted: bool,
    pub locked: bool,
    /// Solo: while any track is soloed, only soloed tracks of that kind are
    /// heard/seen. Defaults false, and `#[serde(default)]` so a project
    /// written before solo existed still loads.
    #[serde(default)]
    pub solo: bool,
}

impl Default for Track {
    fn default() -> Self {
        Self {
            id: Uuid::nil(),
            name: String::new(),
            kind: TrackKind::Video,
            index: 1,
            muted: false,
            locked: false,
            solo: false,
        }
    }
}

/// A placed clip: a reference to media, positioned on a track, sourcing a
/// `[in_point, out_point]` window of that media.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Clip {
    pub id: ClipId,
    pub track: TrackId,
    pub media: MediaId,
    /// Timeline position of the clip's left edge, seconds.
    pub start: f64,
    /// Source in-point, seconds into the media.
    pub in_point: f64,
    /// Source out-point, seconds into the media.
    pub out_point: f64,
    pub opacity: f64,
    pub volume: f64,
    pub transition: Transition,
    pub palette: u8,
    /// Per-clip colour grade. Identity by default.
    #[serde(default)]
    pub filters: Filters,
    /// Fade-in length in seconds (audio ramp / video dip from black at the
    /// head). 0 = no fade.
    #[serde(default)]
    pub fade_in: f64,
    /// Fade-out length in seconds at the tail. 0 = no fade.
    #[serde(default)]
    pub fade_out: f64,
    /// Length of this clip's incoming transition, seconds. `0.0` means "use
    /// the default" — read it through [`Clip::transition_len`], never raw, so
    /// a clip written before the field existed still gets a sensible ramp
    /// instead of an instant cut.
    #[serde(default)]
    pub transition_duration: f64,
    /// Ordered stack of image effects applied after the base grade. Empty by
    /// default; the user adds / removes / reorders them.
    #[serde(default)]
    pub effects: Vec<ClipEffect>,
    /// Where the picture lands in the frame: crop, scale, rotation,
    /// position, anchor. Identity is the aspect-fit the compositor has
    /// always drawn, so an untouched clip is pixel-identical to before.
    #[serde(default)]
    pub transform: Transform,
    /// Playback rate and direction. Normal by default.
    #[serde(default)]
    pub speed: Speed,
    /// Keyframe curves. A parameter with a curve here ignores its constant
    /// field — see [`Clip::param_at`], the one door every reader goes
    /// through.
    #[serde(default, skip_serializing_if = "ClipAnimation::is_empty")]
    pub anim: ClipAnimation,
    /// Markers pinned to this clip, in clip-local seconds.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub markers: Vec<Marker>,
}

impl Default for Clip {
    /// A blank clip, chiefly so callers can write `..Default::default()`
    /// and stop having to change every literal when the model grows.
    fn default() -> Self {
        Self {
            id: Uuid::nil(),
            track: Uuid::nil(),
            media: Uuid::nil(),
            start: 0.0,
            in_point: 0.0,
            out_point: 0.0,
            opacity: 1.0,
            volume: 1.0,
            transition: Transition::None,
            palette: 0,
            filters: Filters::default(),
            fade_in: 0.0,
            fade_out: 0.0,
            transition_duration: 0.0,
            effects: Vec::new(),
            transform: Transform::default(),
            speed: Speed::default(),
            anim: ClipAnimation::default(),
            markers: Vec::new(),
        }
    }
}

/// Default incoming-transition length, seconds, for a clip that has not been
/// given one. Matches the constant the compositor used to hardcode.
pub const DEFAULT_TRANSITION_SECS: f64 = 0.6;

impl Clip {
    /// The effective length of this clip's incoming transition: its own
    /// `transition_duration` when set, else [`DEFAULT_TRANSITION_SECS`], and
    /// never longer than the clip itself — a ramp cannot outlast the footage
    /// it ramps into.
    pub fn transition_len(&self) -> f64 {
        let want = if self.transition_duration > 0.0 {
            self.transition_duration
        } else {
            DEFAULT_TRANSITION_SECS
        };
        want.min(self.duration()).max(0.0)
    }

    /// The length of the *source* window this clip reads, seconds. Speed
    /// does not change it — a 4-second window is 4 seconds of footage
    /// however fast it is played.
    pub fn source_span(&self) -> f64 {
        (self.out_point - self.in_point).max(0.0)
    }

    /// On-timeline length, seconds.
    ///
    /// Speed divides it: play a 4-second window at 2× and it occupies 2
    /// seconds of timeline. At the default rate of 1.0 this is exactly the
    /// source window, so nothing that predates speed changes behaviour.
    pub fn duration(&self) -> f64 {
        self.source_span() / self.speed.rate()
    }
    /// Timeline position of the clip's right edge, seconds.
    pub fn end(&self) -> f64 {
        self.start + self.duration()
    }
    /// True if timeline time `t` falls within this clip's span.
    pub fn covers(&self, t: f64) -> bool {
        t >= self.start && t < self.end()
    }

    /// Append an effect of `kind` (at its default intensity) to the stack.
    pub fn add_effect(&mut self, kind: EffectKind) {
        self.effects.push(ClipEffect::new(kind));
    }
    /// Remove the effect at `idx` (no-op if out of range).
    pub fn remove_effect(&mut self, idx: usize) {
        if idx < self.effects.len() {
            self.effects.remove(idx);
        }
    }
    /// Move the effect at `idx` one slot earlier (toward the start of the
    /// stack), changing the order it's applied. No-op at the top / out of range.
    pub fn move_effect_up(&mut self, idx: usize) {
        if idx > 0 && idx < self.effects.len() {
            self.effects.swap(idx, idx - 1);
        }
    }
    /// Move the effect at `idx` one slot later. No-op at the bottom / out of range.
    pub fn move_effect_down(&mut self, idx: usize) {
        if idx + 1 < self.effects.len() {
            self.effects.swap(idx, idx + 1);
        }
    }

    /// Envelope gain in 0..1 from the fade-in / fade-out ramps at timeline
    /// time `t`. 1.0 in the clip's steady middle; ramps linearly across the
    /// `fade_in`/`fade_out` regions. Used by both the preview (video dip)
    /// and the (gated) audio mix.
    pub fn envelope(&self, t: f64) -> f64 {
        let into = t - self.start;
        let dur = self.duration();
        if dur <= 0.0 {
            return 1.0;
        }
        let mut g: f64 = 1.0;
        if self.fade_in > 1e-6 && into < self.fade_in {
            g = g.min((into / self.fade_in).clamp(0.0, 1.0));
        }
        if self.fade_out > 1e-6 {
            let from_end = dur - into;
            if from_end < self.fade_out {
                g = g.min((from_end / self.fade_out).clamp(0.0, 1.0));
            }
        }
        g.clamp(0.0, 1.0)
    }

    // --- animation -------------------------------------------------------

    /// The constant this parameter falls back to when it has no curve.
    ///
    /// `EffectAmount` past the end of the stack has no constant either;
    /// the caller treats `None` as "there is nothing to drive".
    pub fn param_const(&self, param: Param) -> Option<f64> {
        Some(match param {
            Param::Opacity => self.opacity,
            Param::Volume => self.volume,
            Param::Brightness => self.filters.brightness,
            Param::Contrast => self.filters.contrast,
            Param::Saturation => self.filters.saturation,
            Param::ScaleX => self.transform.scale_x,
            Param::ScaleY => self.transform.scale_y,
            Param::PosX => self.transform.pos_x,
            Param::PosY => self.transform.pos_y,
            Param::Rotation => self.transform.rotation,
            Param::EffectAmount(i) => self.effects.get(i as usize)?.amount,
        })
    }

    /// **The one door.** The value of `param` at absolute timeline time
    /// `playhead`: the curve if this clip has one, else the constant.
    ///
    /// Every reader — compositor, audio mixer, export, inspector — goes
    /// through here, which is why animating a parameter needs no second
    /// code path anywhere.
    pub fn param_at(&self, param: Param, playhead: f64) -> f64 {
        let local = playhead - self.start;
        self.anim
            .sample(param, local)
            .or_else(|| self.param_const(param))
            .unwrap_or(0.0)
    }

    /// Write a keyframe for `param` at absolute timeline time `playhead`.
    /// The first key on a parameter is planted at its current value unless
    /// `value` says otherwise, so keying does not jump the picture.
    pub fn set_key_at(&mut self, param: Param, playhead: f64, value: f64, interp: Interp) {
        let local = playhead - self.start;
        self.anim.ensure(param).set_key(local, value, interp);
    }

    /// Remove `param`'s keyframe near `playhead` (within half a frame at
    /// 30fps). Returns whether one went.
    pub fn remove_key_at(&mut self, param: Param, playhead: f64) -> bool {
        let local = playhead - self.start;
        let Some(track) = self.anim.track_mut(param) else { return false };
        let gone = track.remove_key_near(local, 1.0 / 60.0);
        if gone && track.keys.is_empty() {
            self.anim.clear(param);
        }
        gone
    }

    /// The grade at `playhead`, with any animated axis substituted in.
    pub fn filters_at(&self, playhead: f64) -> Filters {
        if self.anim.is_empty() {
            return self.filters;
        }
        Filters {
            brightness: self.param_at(Param::Brightness, playhead),
            contrast: self.param_at(Param::Contrast, playhead),
            saturation: self.param_at(Param::Saturation, playhead),
        }
    }

    /// The geometry at `playhead`. Crop and anchor are not animatable —
    /// they describe the clip's framing rather than its motion — so they
    /// come straight off the static transform.
    pub fn transform_at(&self, playhead: f64) -> Transform {
        if self.anim.is_empty() {
            return self.transform;
        }
        Transform {
            scale_x: self.param_at(Param::ScaleX, playhead),
            scale_y: self.param_at(Param::ScaleY, playhead),
            pos_x: self.param_at(Param::PosX, playhead),
            pos_y: self.param_at(Param::PosY, playhead),
            rotation: self.param_at(Param::Rotation, playhead),
            ..self.transform
        }
    }

    /// The effect stack at `playhead`, with animated intensities applied.
    pub fn effects_at(&self, playhead: f64) -> Vec<ClipEffect> {
        if self.anim.is_empty() {
            return self.effects.clone();
        }
        self.effects
            .iter()
            .enumerate()
            .map(|(i, e)| ClipEffect {
                kind: e.kind,
                amount: self.param_at(Param::EffectAmount(i as u32), playhead),
            })
            .collect()
    }

    // --- time remap ------------------------------------------------------

    /// Seconds into the source this clip is showing at timeline time
    /// `playhead`.
    ///
    /// Forward, the playhead's distance into the clip is multiplied by the
    /// rate and added to the in-point. Reversed, it is subtracted from the
    /// out-point, so the clip's first timeline frame is the source's last.
    /// The result is held inside `[in, out]`: past either end a clip shows
    /// its edge frame rather than footage it does not own.
    pub fn source_time(&self, playhead: f64) -> f64 {
        let into = (playhead - self.start).max(0.0);
        let travelled = into * self.speed.rate();
        let raw = if self.speed.reverse {
            self.out_point - travelled
        } else {
            self.in_point + travelled
        };
        raw.clamp(self.in_point.min(self.out_point), self.out_point.max(self.in_point))
            .max(0.0)
    }

    /// Set the speed, keeping the clip's **source window** and therefore
    /// re-timing its on-timeline length. Returns the new timeline length.
    pub fn set_speed(&mut self, rate: f64, reverse: bool) -> f64 {
        self.speed = Speed::new(rate, reverse);
        self.duration()
    }

    // --- markers ---------------------------------------------------------

    /// Add a marker at absolute timeline time `t`, stored clip-local.
    /// Ignored when `t` is outside the clip.
    pub fn add_marker(&mut self, t: f64, name: impl Into<String>, palette: u8) -> Option<Uuid> {
        if !self.covers(t) {
            return None;
        }
        let m = Marker::new(t - self.start, name, palette);
        let id = m.id;
        self.markers.push(m);
        self.markers
            .sort_by(|a, b| a.t.partial_cmp(&b.t).unwrap_or(std::cmp::Ordering::Equal));
        Some(id)
    }

    pub fn remove_marker(&mut self, id: Uuid) -> bool {
        let before = self.markers.len();
        self.markers.retain(|m| m.id != id);
        self.markers.len() != before
    }
}

/// The whole editable project.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Project {
    pub name: String,
    pub fps: f64,
    pub width: u32,
    pub height: u32,
    pub media: Vec<MediaItem>,
    pub tracks: Vec<Track>,
    pub clips: Vec<Clip>,
    /// Timeline markers, in absolute seconds, kept sorted by time.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub markers: Vec<Marker>,
}

impl Project {
    pub fn new(name: &str) -> Self {
        Self {
            name: name.to_string(),
            fps: 30.0,
            width: 1920,
            height: 1080,
            media: Vec::new(),
            tracks: Vec::new(),
            clips: Vec::new(),
            markers: Vec::new(),
        }
    }

    // --- timeline markers ------------------------------------------------

    /// Drop a marker at absolute time `t`. Kept sorted so the timeline can
    /// walk them in order and "next marker" is a scan, not a sort.
    pub fn add_marker(&mut self, t: f64, name: impl Into<String>, palette: u8) -> MarkerId {
        let m = Marker::new(t.max(0.0), name, palette);
        let id = m.id;
        self.markers.push(m);
        self.markers
            .sort_by(|a, b| a.t.partial_cmp(&b.t).unwrap_or(std::cmp::Ordering::Equal));
        id
    }

    pub fn marker(&self, id: MarkerId) -> Option<&Marker> {
        self.markers.iter().find(|m| m.id == id)
    }

    pub fn marker_mut(&mut self, id: MarkerId) -> Option<&mut Marker> {
        self.markers.iter_mut().find(|m| m.id == id)
    }

    pub fn remove_marker(&mut self, id: MarkerId) -> bool {
        let before = self.markers.len();
        self.markers.retain(|m| m.id != id);
        self.markers.len() != before
    }

    /// The first project marker strictly after `t`, for "jump to next
    /// marker". `None` at the end of the list rather than wrapping, so the
    /// playhead never silently teleports backwards.
    pub fn next_marker(&self, t: f64) -> Option<&Marker> {
        self.markers.iter().find(|m| m.t > t + 1e-9)
    }

    /// The last project marker strictly before `t`.
    pub fn prev_marker(&self, t: f64) -> Option<&Marker> {
        self.markers.iter().rev().find(|m| m.t < t - 1e-9)
    }

    /// Every marker in the project on one absolute timeline: the project's
    /// own, plus each clip's rebased onto where that clip sits. What the
    /// ruler draws.
    pub fn all_markers(&self) -> Vec<(f64, &Marker, Option<ClipId>)> {
        let mut out: Vec<(f64, &Marker, Option<ClipId>)> =
            self.markers.iter().map(|m| (m.t, m, None)).collect();
        for c in &self.clips {
            for m in &c.markers {
                out.push((c.start + m.t, m, Some(c.id)));
            }
        }
        out.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));
        out
    }

    pub fn media(&self, id: MediaId) -> Option<&MediaItem> {
        self.media.iter().find(|m| m.id == id)
    }
    pub fn track(&self, id: TrackId) -> Option<&Track> {
        self.tracks.iter().find(|t| t.id == id)
    }
    pub fn clip(&self, id: ClipId) -> Option<&Clip> {
        self.clips.iter().find(|c| c.id == id)
    }
    pub fn clip_mut(&mut self, id: ClipId) -> Option<&mut Clip> {
        self.clips.iter_mut().find(|c| c.id == id)
    }

    /// Tracks in vertical display order, top → bottom: video lanes
    /// (highest index first), then audio lanes (lowest index first).
    /// This matches every NLE's "video above, audio below" convention.
    pub fn display_tracks(&self) -> Vec<&Track> {
        let mut video: Vec<&Track> =
            self.tracks.iter().filter(|t| t.kind == TrackKind::Video).collect();
        video.sort_by(|a, b| b.index.cmp(&a.index));
        let mut audio: Vec<&Track> =
            self.tracks.iter().filter(|t| t.kind == TrackKind::Audio).collect();
        audio.sort_by(|a, b| a.index.cmp(&b.index));
        video.into_iter().chain(audio).collect()
    }

    /// Clips on `track`, sorted left-to-right by start.
    pub fn clips_on(&self, track: TrackId) -> Vec<&Clip> {
        let mut v: Vec<&Clip> =
            self.clips.iter().filter(|c| c.track == track).collect();
        v.sort_by(|a, b| a.start.partial_cmp(&b.start).unwrap_or(std::cmp::Ordering::Equal));
        v
    }

    /// Total timeline length = the furthest clip end (min 1s so an empty
    /// project still has a ruler to look at).
    pub fn duration(&self) -> f64 {
        self.clips
            .iter()
            .map(|c| c.end())
            .fold(0.0_f64, f64::max)
            .max(1.0)
    }

    /// The video clip visible at timeline time `t`: the one on the
    /// highest video track (largest index) whose span covers `t`. This
    /// is what the preview canvas renders.
    pub fn top_video_clip_at(&self, t: f64) -> Option<&Clip> {
        let mut best: Option<(&Clip, u32)> = None;
        for c in &self.clips {
            let Some(track) = self.track(c.track) else { continue };
            if track.kind != TrackKind::Video {
                continue;
            }
            if !c.covers(t) {
                continue;
            }
            if best.map(|(_, idx)| track.index > idx).unwrap_or(true) {
                best = Some((c, track.index));
            }
        }
        best.map(|(c, _)| c)
    }

    /// Append a clip referencing `media`'s full length at timeline `start`
    /// on `track`. Returns the new clip id. Inherits the media's palette.
    pub fn add_clip(&mut self, track: TrackId, media: MediaId, start: f64) -> Option<ClipId> {
        let m = self.media(media)?.clone();
        let id = Uuid::new_v4();
        self.clips.push(Clip {
            id,
            track,
            media,
            start: start.max(0.0),
            in_point: 0.0,
            out_point: m.duration,
            opacity: 1.0,
            volume: 1.0,
            transition: Transition::None,
            palette: m.palette,
            filters: Filters::default(),
            fade_in: 0.0,
            fade_out: 0.0,
            transition_duration: 0.0,
            effects: Vec::new(),
            ..Default::default()
        });
        Some(id)
    }

    /// Split the clip at timeline time `at`. The left part keeps the id
    /// and is shortened; a fresh right part is appended. Returns the new
    /// right-hand clip id, or `None` if `at` isn't strictly inside the
    /// clip (nothing to split).
    pub fn split_clip(&mut self, id: ClipId, at: f64) -> Option<ClipId> {
        let clip = self.clip(id)?.clone();
        if at <= clip.start + MIN_CLIP || at >= clip.end() - MIN_CLIP {
            return None;
        }
        // Seconds into the clip on the timeline, and where that lands in
        // the source — which is not the same distance once the clip is
        // sped up, slowed down or reversed.
        let offset = at - clip.start;
        let split_src = clip.source_time(at);
        let full = clip.duration();
        let mut right = clip.clone();
        right.id = Uuid::new_v4();
        right.start = at;
        // Reversed, the tail of the timeline is the *head* of the source,
        // so the cut moves the out-point rather than the in-point.
        if clip.speed.reverse {
            right.out_point = split_src;
            if let Some(left) = self.clip_mut(id) {
                left.in_point = split_src;
                left.fade_out = 0.0;
            }
        } else {
            right.in_point = split_src;
            if let Some(left) = self.clip_mut(id) {
                left.out_point = split_src;
                left.fade_out = 0.0;
            }
        }
        // A head transition belongs to the new cut, not the right half.
        right.transition = Transition::None;
        // The fade-in stays with the head (left); the fade-out belongs to
        // the tail (right). Each half keeps the relevant ramp only.
        right.fade_in = 0.0;
        // Curves and markers are clip-local, so each half keeps what falls
        // inside it, rebased — and the value at the cut is preserved on
        // both sides, so splitting a ramp does not change the picture.
        right.anim = clip.anim.sliced(offset, full);
        right.markers = clip
            .markers
            .iter()
            .filter(|m| m.t >= offset)
            .map(|m| Marker { t: m.t - offset, ..m.clone() })
            .collect();
        if let Some(left) = self.clip_mut(id) {
            left.anim = clip.anim.sliced(0.0, offset);
            left.markers.retain(|m| m.t < offset);
        }
        let new_id = right.id;
        self.clips.push(right);
        Some(new_id)
    }

    /// Move a clip's left edge to `new_start` (clamped to >= 0). Trimming
    /// is unaffected — the whole source window slides.
    pub fn move_clip(&mut self, id: ClipId, new_start: f64) {
        if let Some(c) = self.clip_mut(id) {
            c.start = new_start.max(0.0);
        }
    }

    /// Move a clip to `track` *and* `new_start` in one step — a vertical drag
    /// across lanes. Rejected (leaving the clip untouched) when the target
    /// track doesn't exist, is locked, or carries the wrong kind for the
    /// clip's media: a video clip only ever lands on a video track, audio on
    /// audio. Returns whether the move happened.
    pub fn move_clip_to(&mut self, id: ClipId, track: TrackId, new_start: f64) -> bool {
        if !self.can_place(id, track) {
            return false;
        }
        if let Some(c) = self.clip_mut(id) {
            c.track = track;
            c.start = new_start.max(0.0);
            return true;
        }
        false
    }

    /// The lane kind a clip belongs on, from the kind of media it references.
    /// Falls back to video for a dangling media reference so a broken clip
    /// still has a home rather than silently migrating to audio.
    pub fn clip_kind(&self, id: ClipId) -> TrackKind {
        self.clip(id)
            .and_then(|c| self.media(c.media))
            .map(|m| if m.kind.is_visual() { TrackKind::Video } else { TrackKind::Audio })
            .unwrap_or(TrackKind::Video)
    }

    /// Whether any track of `kind` is soloed. While one is, the others of that
    /// kind are silenced/hidden — the standard NLE rule, scoped per kind so
    /// soloing a video track does not mute the audio bed.
    pub fn any_solo(&self, kind: TrackKind) -> bool {
        self.tracks.iter().any(|t| t.kind == kind && t.solo)
    }

    /// Whether `track` should be heard (audio) or drawn (video) right now.
    ///
    /// This is the single query the preview, the audio path and the export
    /// renderer should ask — mute and solo interact, and duplicating the rule
    /// is how they drift apart. Solo wins: while anything of that kind is
    /// soloed, a non-soloed track is silent even if it is not muted, and a
    /// soloed track is heard even if it is also muted (soloing is the louder
    /// statement of intent, and the mute flag is preserved for when solo is
    /// released).
    pub fn track_audible(&self, track: TrackId) -> bool {
        let Some(t) = self.track(track) else { return false };
        if self.any_solo(t.kind) {
            return t.solo;
        }
        !t.muted
    }

    /// [`Self::track_audible`] for the track a clip sits on.
    pub fn clip_audible(&self, id: ClipId) -> bool {
        self.clip(id).map(|c| self.track_audible(c.track)).unwrap_or(false)
    }

    /// Whether `track` refuses edits. A missing track counts as locked: there
    /// is nothing there to edit.
    pub fn track_locked(&self, track: TrackId) -> bool {
        self.track(track).map(|t| t.locked).unwrap_or(true)
    }

    /// Whether the clip that owns `id` may currently be edited — false when
    /// its own track is locked.
    pub fn clip_editable(&self, id: ClipId) -> bool {
        self.clip(id).map(|c| !self.track_locked(c.track)).unwrap_or(false)
    }

    /// Whether clip `id` may be placed on `track`: the track must exist, be
    /// unlocked, match the clip's kind, and the clip must not already be
    /// pinned to a locked track of its own.
    pub fn can_place(&self, id: ClipId, track: TrackId) -> bool {
        if !self.clip_editable(id) {
            return false;
        }
        let Some(t) = self.track(track) else { return false };
        !t.locked && t.kind == self.clip_kind(id)
    }

    /// The first unlocked track of `kind`, preferring `near` when it already
    /// qualifies — the "sensible track" a paste or a duplicate lands on.
    pub fn placement_track(&self, kind: TrackKind, near: Option<TrackId>) -> Option<TrackId> {
        if let Some(t) = near.and_then(|id| self.track(id)) {
            if t.kind == kind && !t.locked {
                return Some(t.id);
            }
        }
        self.display_tracks()
            .into_iter()
            .find(|t| t.kind == kind && !t.locked)
            .map(|t| t.id)
    }

    /// Drag the left (in) handle by `delta` seconds. Positive `delta`
    /// trims later into the source (clip shrinks from the left and its
    /// timeline start moves right); negative reveals earlier source.
    /// Clamped so `in_point >= 0` and the clip keeps at least `MIN_CLIP`.
    pub fn trim_in(&mut self, id: ClipId, delta: f64) {
        if let Some(c) = self.clip_mut(id) {
            let max_delta = (c.out_point - c.in_point) - MIN_CLIP; // can't cross out
            let min_delta = -c.in_point; // can't go before source 0
            let d = delta.clamp(min_delta, max_delta);
            c.in_point += d;
            // `d` is source seconds; the timeline start moves by however
            // long those seconds take to play, which the rate decides.
            let on_timeline = d / c.speed.rate();
            c.start += on_timeline;
            // Curves are anchored to the clip's start, so pulling the head
            // in has to rebase them or the animation slides off the frames
            // it was drawn against.
            if !c.anim.is_empty() {
                c.anim = c.anim.sliced(on_timeline, c.duration() + on_timeline);
            }
            c.markers.retain(|m| m.t >= on_timeline);
            for m in c.markers.iter_mut() {
                m.t -= on_timeline;
            }
        }
    }

    /// Drag the right (out) handle by `delta` seconds. Clamped so the
    /// clip stays within the source length and keeps at least `MIN_CLIP`.
    pub fn trim_out(&mut self, id: ClipId, delta: f64) {
        let media_dur = self
            .clip(id)
            .and_then(|c| self.media(c.media))
            .map(|m| m.duration);
        if let (Some(c), Some(media_dur)) = (self.clip_mut(id), media_dur) {
            let max_out = media_dur;
            let min_out = c.in_point + MIN_CLIP;
            c.out_point = (c.out_point + delta).clamp(min_out, max_out);
        }
    }

    pub fn delete_clip(&mut self, id: ClipId) {
        self.clips.retain(|c| c.id != id);
    }

    /// Delete a clip and close the gap: every later clip *on the same
    /// track* shifts left by the deleted clip's duration, so there's no
    /// hole where it was. Returns the ids of the clips that moved (so the
    /// caller can replicate exactly those through the CRDT) — empty if the
    /// clip didn't exist.
    pub fn ripple_delete(&mut self, id: ClipId) -> Vec<ClipId> {
        let Some(clip) = self.clip(id).cloned() else { return Vec::new() };
        let gap = clip.duration();
        let track = clip.track;
        let after = clip.start;
        self.clips.retain(|c| c.id != id);
        let mut moved = Vec::new();
        for c in self.clips.iter_mut() {
            if c.track == track && c.start >= after - 1e-9 {
                c.start = (c.start - gap).max(0.0);
                moved.push(c.id);
            }
        }
        moved
    }

    /// Candidate snap targets (seconds) a dragged clip edge should magnetise
    /// to: the playhead, plus the start/end of every *other* clip. The
    /// caller picks the nearest within a pixel threshold. `exclude` skips
    /// the clip being dragged so it never snaps to itself.
    pub fn snap_candidates(&self, exclude: ClipId, playhead: f64) -> Vec<f64> {
        let mut out = vec![0.0, playhead];
        for c in &self.clips {
            if c.id == exclude {
                continue;
            }
            out.push(c.start);
            out.push(c.end());
        }
        out
    }

    /// Snap `t` to the nearest candidate within `tol` seconds, else return
    /// `t` unchanged. Pure helper so the snapping is unit-testable.
    pub fn snap_to_candidates(t: f64, candidates: &[f64], tol: f64) -> f64 {
        let mut best: Option<(f64, f64)> = None; // (candidate, distance)
        for &c in candidates {
            let d = (c - t).abs();
            if d <= tol && best.map(|(_, bd)| d < bd).unwrap_or(true) {
                best = Some((c, d));
            }
        }
        best.map(|(c, _)| c).unwrap_or(t)
    }

    /// Paste `clip` as a new independent clip starting at `start` on
    /// `track` (the clip keeps its trim window, filters and fades). Returns
    /// the new id. Used by copy/paste — the source clip is unchanged.
    pub fn paste_clip(&mut self, mut clip: Clip, track: TrackId, start: f64) -> ClipId {
        clip.id = Uuid::new_v4();
        clip.track = track;
        clip.start = start.max(0.0);
        let id = clip.id;
        self.clips.push(clip);
        id
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn project_with_clip() -> (Project, MediaId, TrackId, ClipId) {
        let mut p = Project::new("t");
        let m = MediaItem {
            id: Uuid::new_v4(),
            name: "src".into(),
            kind: MediaKind::Video,
            duration: 20.0,
            width: 1920,
            height: 1080,
            fps: 30.0,
            palette: 0,
            path: media::workspace_path("src.mov"),
            hydration: Hydration::Projected,
        };
        let mid = m.id;
        p.media.push(m);
        let t = Track {
            id: Uuid::new_v4(),
            name: "V1".into(),
            kind: TrackKind::Video,
            index: 1,
            muted: false,
            locked: false, solo: false,
        };
        let tid = t.id;
        p.tracks.push(t);
        let cid = p.add_clip(tid, mid, 0.0).unwrap();
        (p, mid, tid, cid)
    }

    #[test]
    fn add_clip_uses_full_source_length() {
        let (p, _m, _t, cid) = project_with_clip();
        let c = p.clip(cid).unwrap();
        assert_eq!(c.in_point, 0.0);
        assert_eq!(c.out_point, 20.0);
        assert_eq!(c.duration(), 20.0);
        assert_eq!(c.end(), 20.0);
    }

    #[test]
    fn split_produces_two_contiguous_clips() {
        let (mut p, _m, _t, cid) = project_with_clip();
        let right = p.split_clip(cid, 8.0).unwrap();
        let left = p.clip(cid).unwrap();
        let right = p.clip(right).unwrap();
        // Left ends exactly where right begins on the timeline.
        assert!((left.end() - 8.0).abs() < 1e-9);
        assert!((right.start - 8.0).abs() < 1e-9);
        // Source is contiguous across the cut.
        assert!((left.out_point - right.in_point).abs() < 1e-9);
        // Total duration is preserved.
        assert!((left.duration() + right.duration() - 20.0).abs() < 1e-9);
    }

    #[test]
    fn split_outside_span_is_rejected() {
        let (mut p, _m, _t, cid) = project_with_clip();
        assert!(p.split_clip(cid, 0.0).is_none());
        assert!(p.split_clip(cid, 25.0).is_none());
        assert_eq!(p.clips.len(), 1);
    }

    #[test]
    fn trim_in_moves_start_and_in_together() {
        let (mut p, _m, _t, cid) = project_with_clip();
        p.trim_in(cid, 3.0);
        let c = p.clip(cid).unwrap();
        assert!((c.in_point - 3.0).abs() < 1e-9);
        assert!((c.start - 3.0).abs() < 1e-9);
        // right edge unchanged
        assert!((c.end() - 20.0).abs() < 1e-9);
    }

    #[test]
    fn trim_out_clamps_to_source_length() {
        let (mut p, _m, _t, cid) = project_with_clip();
        p.trim_out(cid, 50.0); // way past the 20s source
        let c = p.clip(cid).unwrap();
        assert!((c.out_point - 20.0).abs() < 1e-9);
    }

    #[test]
    fn trim_in_cannot_cross_out_point() {
        let (mut p, _m, _t, cid) = project_with_clip();
        p.trim_in(cid, 1000.0);
        let c = p.clip(cid).unwrap();
        assert!(c.duration() >= MIN_CLIP - 1e-9);
    }

    #[test]
    fn top_video_clip_prefers_higher_track() {
        let (mut p, mid, _t, _cid) = project_with_clip();
        // Add a higher video track with an overlapping clip.
        let t2 = Track {
            id: Uuid::new_v4(),
            name: "V2".into(),
            kind: TrackKind::Video,
            index: 2,
            muted: false,
            locked: false, solo: false,
        };
        let t2id = t2.id;
        p.tracks.push(t2);
        let over = p.add_clip(t2id, mid, 0.0).unwrap();
        let top = p.top_video_clip_at(5.0).unwrap();
        assert_eq!(top.id, over, "V2 clip must win over V1 at the same time");
    }

    #[test]
    fn display_tracks_order_video_above_audio() {
        let mut p = Project::new("t");
        for (name, kind, index) in [
            ("V1", TrackKind::Video, 1),
            ("V2", TrackKind::Video, 2),
            ("A1", TrackKind::Audio, 1),
            ("A2", TrackKind::Audio, 2),
        ] {
            p.tracks.push(Track {
                id: Uuid::new_v4(),
                name: name.into(),
                kind,
                index,
                muted: false,
                locked: false, solo: false,
            });
        }
        let order: Vec<&str> =
            p.display_tracks().iter().map(|t| t.name.as_str()).collect();
        assert_eq!(order, vec!["V2", "V1", "A1", "A2"]);
    }

    #[test]
    fn ripple_delete_closes_the_gap() {
        let (mut p, mid, tid, first) = project_with_clip();
        // first occupies [0,20]. Place two more after it.
        let second = p.add_clip(tid, mid, 20.0).unwrap();
        let third = p.add_clip(tid, mid, 40.0).unwrap();
        let first_dur = p.clip(first).unwrap().duration();
        let moved = p.ripple_delete(first);
        // Both later clips shifted left by the deleted duration.
        assert!(moved.contains(&second) && moved.contains(&third));
        assert!((p.clip(second).unwrap().start - (20.0 - first_dur)).abs() < 1e-9);
        assert!((p.clip(third).unwrap().start - (40.0 - first_dur)).abs() < 1e-9);
        assert_eq!(p.clips.len(), 2);
    }

    #[test]
    fn snap_magnetises_within_tolerance_only() {
        let (p, _m, _t, cid) = project_with_clip();
        // Candidates include 0.0 and the clip end (20.0) and playhead 7.0.
        let cands = p.snap_candidates(Uuid::new_v4(), 7.0);
        // 19.9 is within 0.2 of the 20.0 clip end → snaps.
        assert!((Project::snap_to_candidates(19.9, &cands, 0.2) - 20.0).abs() < 1e-9);
        // 7.1 snaps to the playhead at 7.0.
        assert!((Project::snap_to_candidates(7.1, &cands, 0.2) - 7.0).abs() < 1e-9);
        // 12.0 is far from every candidate → unchanged.
        assert!((Project::snap_to_candidates(12.0, &cands, 0.2) - 12.0).abs() < 1e-9);
        // Excluding the clip removes its edges from the candidate set.
        let excl = p.snap_candidates(cid, 7.0);
        assert!((Project::snap_to_candidates(19.9, &excl, 0.2) - 19.9).abs() < 1e-9);
    }

    #[test]
    fn paste_clip_is_independent_of_source() {
        let (mut p, _m, tid, cid) = project_with_clip();
        let src = p.clip(cid).unwrap().clone();
        let pasted = p.paste_clip(src, tid, 30.0);
        assert_ne!(pasted, cid, "pasted clip gets a fresh id");
        assert!((p.clip(pasted).unwrap().start - 30.0).abs() < 1e-9);
        // Mutating the paste leaves the source clip untouched.
        p.clip_mut(pasted).unwrap().opacity = 0.1;
        assert!((p.clip(cid).unwrap().opacity - 1.0).abs() < 1e-9);
    }

    #[test]
    fn fade_envelope_ramps_at_head_and_tail() {
        let (mut p, _m, _t, cid) = project_with_clip(); // [0,20]
        {
            let c = p.clip_mut(cid).unwrap();
            c.fade_in = 2.0;
            c.fade_out = 4.0;
        }
        let c = p.clip(cid).unwrap();
        assert!(c.envelope(0.0) < 0.01, "silent at the very head");
        assert!((c.envelope(1.0) - 0.5).abs() < 1e-6, "half through a 2s fade-in");
        assert!((c.envelope(10.0) - 1.0).abs() < 1e-6, "full in the middle");
        assert!((c.envelope(18.0) - 0.5).abs() < 1e-6, "half through a 4s fade-out");
        assert!(c.envelope(20.0) < 0.01, "silent at the tail");
    }

    #[test]
    fn split_keeps_fade_in_on_head_and_fade_out_on_tail() {
        let (mut p, _m, _t, cid) = project_with_clip(); // [0,20]
        {
            let c = p.clip_mut(cid).unwrap();
            c.fade_in = 2.0;
            c.fade_out = 3.0;
        }
        let right = p.split_clip(cid, 8.0).unwrap();
        let left = p.clip(cid).unwrap();
        let right = p.clip(right).unwrap();
        assert!((left.fade_in - 2.0).abs() < 1e-9, "head keeps fade-in");
        assert_eq!(left.fade_out, 0.0, "head loses tail fade");
        assert_eq!(right.fade_in, 0.0, "tail loses head fade");
        assert!((right.fade_out - 3.0).abs() < 1e-9, "tail keeps fade-out");
    }

    #[test]
    fn effects_stack_add_remove_and_reorder() {
        let (mut p, _m, _t, cid) = project_with_clip();
        let c = p.clip_mut(cid).unwrap();
        assert!(c.effects.is_empty(), "a fresh clip has no effects");
        c.add_effect(EffectKind::Grayscale);
        c.add_effect(EffectKind::Sepia);
        c.add_effect(EffectKind::Invert);
        let kinds = |c: &Clip| c.effects.iter().map(|e| e.kind).collect::<Vec<_>>();
        assert_eq!(kinds(c), vec![EffectKind::Grayscale, EffectKind::Sepia, EffectKind::Invert]);
        // A fresh effect carries its kind's default intensity.
        assert!((c.effects[0].amount - EffectKind::Grayscale.default_amount()).abs() < 1e-9);
        // Reorder: move Invert (idx 2) up.
        c.move_effect_up(2);
        assert_eq!(kinds(c), vec![EffectKind::Grayscale, EffectKind::Invert, EffectKind::Sepia]);
        // Remove the head.
        c.remove_effect(0);
        assert_eq!(kinds(c), vec![EffectKind::Invert, EffectKind::Sepia]);
        // Out-of-range moves / removes are no-ops.
        c.move_effect_up(0);
        c.move_effect_down(1);
        c.remove_effect(9);
        assert_eq!(kinds(c), vec![EffectKind::Invert, EffectKind::Sepia]);
    }

    #[test]
    fn filters_default_is_identity() {
        assert!(Filters::default().is_identity());
        assert!((Filters::gain(0.5) - 1.0).abs() < 1e-9, "0.5 slider = unity gain");
        assert!(Filters::gain(0.0) < 1e-9);
        assert!((Filters::gain(1.0) - 2.0).abs() < 1e-9);
    }

    /// A second video track and an audio track alongside the fixture's V1.
    fn extra_tracks(p: &mut Project) -> (TrackId, TrackId) {
        let v2 = Track {
            id: Uuid::new_v4(),
            name: "V2".into(),
            kind: TrackKind::Video,
            index: 2,
            muted: false,
            locked: false, solo: false,
        };
        let a1 = Track {
            id: Uuid::new_v4(),
            name: "A1".into(),
            kind: TrackKind::Audio,
            index: 1,
            muted: false,
            locked: false, solo: false,
        };
        let (v, a) = (v2.id, a1.id);
        p.tracks.push(v2);
        p.tracks.push(a1);
        (v, a)
    }

    #[test]
    fn move_clip_to_relocates_across_video_tracks() {
        let (mut p, _m, v1, cid) = project_with_clip();
        let (v2, _a1) = extra_tracks(&mut p);
        assert!(p.move_clip_to(cid, v2, 3.5), "a video clip lands on another video track");
        let c = p.clip(cid).unwrap();
        assert_eq!(c.track, v2, "the clip now belongs to the target track");
        assert!((c.start - 3.5).abs() < 1e-9, "and moved to the drop time");
        assert!(p.clips_on(v1).is_empty(), "it left the source track");
        assert_eq!(p.clips_on(v2).len(), 1);
    }

    #[test]
    fn move_clip_to_refuses_a_kind_mismatch() {
        let (mut p, _m, v1, cid) = project_with_clip();
        let (_v2, a1) = extra_tracks(&mut p);
        assert!(!p.move_clip_to(cid, a1, 2.0), "a video clip cannot land on an audio track");
        let c = p.clip(cid).unwrap();
        assert_eq!(c.track, v1, "the clip stayed where it was");
        assert!((c.start - 0.0).abs() < 1e-9, "and did not move in time either");
    }

    #[test]
    fn move_clip_to_refuses_a_locked_destination_and_a_locked_source() {
        let (mut p, _m, v1, cid) = project_with_clip();
        let (v2, _a1) = extra_tracks(&mut p);
        // Locked destination.
        p.tracks.iter_mut().find(|t| t.id == v2).unwrap().locked = true;
        assert!(!p.move_clip_to(cid, v2, 4.0), "a locked track refuses a drop");
        assert_eq!(p.clip(cid).unwrap().track, v1);
        // Locked source: even an unlocked destination refuses.
        p.tracks.iter_mut().find(|t| t.id == v2).unwrap().locked = false;
        p.tracks.iter_mut().find(|t| t.id == v1).unwrap().locked = true;
        assert!(!p.move_clip_to(cid, v2, 4.0), "a clip on a locked track cannot be moved off it");
        assert_eq!(p.clip(cid).unwrap().track, v1);
    }

    #[test]
    fn clip_editable_follows_its_track_lock() {
        let (mut p, _m, v1, cid) = project_with_clip();
        assert!(p.clip_editable(cid));
        p.tracks.iter_mut().find(|t| t.id == v1).unwrap().locked = true;
        assert!(!p.clip_editable(cid), "a clip on a locked track is not editable");
        assert!(p.track_locked(v1));
        assert!(p.track_locked(Uuid::new_v4()), "a missing track is treated as locked");
    }

    #[test]
    fn placement_track_prefers_the_hint_then_falls_back_by_kind() {
        let (mut p, _m, v1, _cid) = project_with_clip();
        let (v2, a1) = extra_tracks(&mut p);
        assert_eq!(p.placement_track(TrackKind::Video, Some(v2)), Some(v2), "an eligible hint wins");
        assert_eq!(
            p.placement_track(TrackKind::Audio, Some(v2)),
            Some(a1),
            "a kind-mismatched hint falls back to a track of the right kind",
        );
        // Locking every video track leaves nowhere to place a video clip.
        for t in p.tracks.iter_mut().filter(|t| t.kind == TrackKind::Video) {
            t.locked = true;
        }
        assert_eq!(p.placement_track(TrackKind::Video, Some(v1)), None);
    }

    #[test]
    fn mute_silences_a_track_and_solo_overrides_it() {
        let (mut p, _m, v1, _cid) = project_with_clip();
        let (v2, a1) = extra_tracks(&mut p);
        // Nothing muted or soloed: everything plays.
        assert!(p.track_audible(v1) && p.track_audible(v2) && p.track_audible(a1));
        // Mute one.
        p.tracks.iter_mut().find(|t| t.id == v1).unwrap().muted = true;
        assert!(!p.track_audible(v1), "a muted track is silent");
        assert!(p.track_audible(v2), "its neighbours are not");
        // Solo the other video track: the muted one stays silent, and so does
        // every other video track — but the audio bed is untouched, because
        // solo is scoped per kind.
        p.tracks.iter_mut().find(|t| t.id == v2).unwrap().solo = true;
        assert!(p.any_solo(TrackKind::Video));
        assert!(!p.any_solo(TrackKind::Audio));
        assert!(p.track_audible(v2), "the soloed track plays");
        assert!(!p.track_audible(v1), "a non-soloed track of that kind does not");
        assert!(p.track_audible(a1), "audio is unaffected by a video solo");
    }

    #[test]
    fn solo_wins_over_mute_on_the_same_track() {
        let (mut p, _m, v1, _cid) = project_with_clip();
        {
            let t = p.tracks.iter_mut().find(|t| t.id == v1).unwrap();
            t.muted = true;
            t.solo = true;
        }
        assert!(
            p.track_audible(v1),
            "soloing a muted track plays it, and keeps the mute flag for when solo is released",
        );
        assert!(p.track(v1).unwrap().muted, "the mute flag is preserved");
    }

    #[test]
    fn clip_audible_follows_its_track() {
        let (mut p, _m, v1, cid) = project_with_clip();
        assert!(p.clip_audible(cid));
        p.tracks.iter_mut().find(|t| t.id == v1).unwrap().muted = true;
        assert!(!p.clip_audible(cid));
    }

    #[test]
    fn clip_kind_reads_through_to_the_media() {
        let (p, _m, _v1, cid) = project_with_clip();
        assert_eq!(p.clip_kind(cid), TrackKind::Video);
    }
}
