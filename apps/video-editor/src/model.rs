//! Pure data model for the editor — media bin, tracks, clips, project.
//!
//! The project is the editor's model: `#[derive(Model)]` stores it in the
//! record's document field by field, media, tracks, clips and markers keyed
//! by their own ids ([`Keyed`]), so collaborators' edits merge per field.
//! All the non-trivial editing math (split, trim, move, ripple duration)
//! lives here behind unit tests; the verbs ([`crate::resource`]) run it.

use serde::{Deserialize, Serialize};
use ui::tilo_resource::{self, Id, Identified, Keyed, Model, Ordered};
use uuid::Uuid;

use crate::curve::Curve;
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
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize, Model)]
pub struct Filters {
    pub brightness: f64,
    pub contrast: f64,
    pub saturation: f64,
}

impl Default for Filters {
    fn default() -> Self {
        // 0.5 is the neutral midpoint of every slider.
        Self {
            brightness: 0.5,
            contrast: 0.5,
            saturation: 0.5,
        }
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
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize, Model)]
pub struct ClipEffect {
    /// Immutable instance identity. Unlike the kind, this distinguishes two
    /// copies of the same effect and survives reordering, so a resource/action
    /// can target the exact durable stack entry rather than a mutable index.
    pub id: Uuid,
    pub kind: EffectKind,
    pub amount: f64,
}

impl ClipEffect {
    pub fn new(kind: EffectKind) -> Self {
        Self {
            id: tilo_resource::fresh_uuid(),
            kind,
            amount: kind.default_amount(),
        }
    }
}

/// The kinds of stackable effect a clip can carry. Each transforms the graded
/// colour; the preview and the (gated) export apply them in list order.
#[derive(
    Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Model, ui::tilo_resource::JsonSchema,
)]
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
    pub fn from_name(s: &str) -> Option<Self> {
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
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize, Model)]
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
        let (l, r) = if r - l < 1e-4 {
            (l.min(1.0 - 1e-4), l.min(1.0 - 1e-4) + 1e-4)
        } else {
            (l, r)
        };
        let (t, b) = if b - t < 1e-4 {
            (t.min(1.0 - 1e-4), t.min(1.0 - 1e-4) + 1e-4)
        } else {
            (t, b)
        };
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
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize, Model)]
pub struct Speed {
    pub rate: f64,
    /// Play the source window backwards. The clip still occupies the same
    /// stretch of timeline; it just reads from the out-point down.
    pub reverse: bool,
}

impl Default for Speed {
    fn default() -> Self {
        Self {
            rate: 1.0,
            reverse: false,
        }
    }
}

/// Slowest and fastest rate the model accepts. Below the floor a clip's
/// timeline length explodes; above the ceiling the source is skipped so
/// coarsely that the result is noise.
pub const MIN_SPEED: f64 = 0.05;
pub const MAX_SPEED: f64 = 20.0;

impl Speed {
    pub fn new(rate: f64, reverse: bool) -> Self {
        Self {
            rate: rate.clamp(MIN_SPEED, MAX_SPEED),
            reverse,
        }
    }
    /// The rate, guaranteed usable as a divisor.
    pub fn rate(&self) -> f64 {
        if self.rate.is_finite() {
            self.rate.clamp(MIN_SPEED, MAX_SPEED)
        } else {
            1.0
        }
    }
    pub fn is_normal(&self) -> bool {
        !self.reverse && (self.rate() - 1.0).abs() < 1e-9
    }
    /// Signed rate: negative when playing backwards. What the source-time
    /// map and the audio resampler both step by.
    pub fn signed(&self) -> f64 {
        if self.reverse {
            -self.rate()
        } else {
            self.rate()
        }
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

/// A colour key: the layer stops contributing wherever its picture is
/// close enough — in *chroma*, not in brightness — to [`Self::color`].
///
/// Keying on chroma rather than on RGB distance is what lets a real green
/// screen work: the lit and the shadowed parts of one backing cloth differ
/// enormously in brightness and barely at all in hue, so a straight RGB
/// distance either keeps the shadows or eats the subject.
///
/// The key runs on the **source** picture, before the grade and the effect
/// chain — the order a keyer sits in on a real desk, and the reason grading
/// a keyed clip does not re-open the matte.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize, Model)]
pub struct ChromaKey {
    pub enabled: bool,
    /// The colour to remove, in 0..1 RGB. Only its chroma is used; its
    /// brightness is ignored, which is why a swatch sampled off a lit patch
    /// of screen still keys the shadowed corners.
    pub color: [f64; 3],
    /// Chroma distance that is fully removed, 0..1. Nothing is keyed at 0.
    pub tolerance: f64,
    /// Width of the soft edge past `tolerance`, in the same units. This is
    /// what gives hair and motion blur a partial matte rather than a
    /// staircase.
    pub softness: f64,
    /// How much of the key colour is pulled back out of what survives,
    /// 0..1. Green bouncing off a subject standing near the screen tints
    /// them; this removes that tint without touching their brightness.
    pub spill: f64,
}

/// The colours a keyer is usually pointed at, offered as swatches. A real
/// plate is keyed by sampling it, but a plausible starting point beats an
/// empty colour well.
pub const KEY_PRESETS: [(&str, [f64; 3]); 3] = [
    ("Green", [0.06, 0.72, 0.20]),
    ("Blue", [0.06, 0.22, 0.72]),
    ("Magenta", [0.72, 0.06, 0.55]),
];

impl Default for ChromaKey {
    fn default() -> Self {
        Self {
            enabled: false,
            color: KEY_PRESETS[0].1,
            tolerance: 0.22,
            softness: 0.10,
            spill: 0.5,
        }
    }
}

impl ChromaKey {
    /// Whether this key actually removes anything. A key that is switched
    /// off, or one whose tolerance has been wound to zero, is not doing
    /// work — the compositor can skip it and the inspector says so.
    pub fn is_active(&self) -> bool {
        self.enabled && self.tolerance > 1e-6
    }

    /// BT.709 chroma of an RGB triple as `(cb, cr)`, each in -0.5..0.5.
    /// Mirrored by the shader's `chroma()` so the two cannot drift.
    pub fn chroma(rgb: [f64; 3]) -> (f64, f64) {
        let y = 0.2126 * rgb[0] + 0.7152 * rgb[1] + 0.0722 * rgb[2];
        ((rgb[2] - y) / 1.8556, (rgb[0] - y) / 1.5748)
    }

    /// Below this brightness a pixel's hue stops meaning anything — the
    /// chroma left in near-black is noise — so the normalisation stops
    /// dividing and lets those pixels fall out of the key rather than
    /// amplifying their noise into it.
    pub const DARK_FLOOR: f64 = 0.06;

    /// Chroma normalised by brightness.
    ///
    /// Plain Cb/Cr scale with exposure: a shadowed patch of a green screen
    /// has the same *hue* as a lit one and a much smaller Cb/Cr, so an
    /// absolute chroma distance keys the lit cloth and keeps the shadows —
    /// the classic green-screen failure. Dividing by luma removes the
    /// exposure term, so one tolerance covers the whole cloth.
    pub fn norm_chroma(rgb: [f64; 3]) -> (f64, f64) {
        let y = (0.2126 * rgb[0] + 0.7152 * rgb[1] + 0.0722 * rgb[2]).max(Self::DARK_FLOOR);
        let (cb, cr) = Self::chroma(rgb);
        (cb / y, cr / y)
    }

    /// How far `rgb` sits from the key colour in the space the matte is
    /// cut in. `tolerance` and `softness` are measured in these units, so
    /// this is the number to reason about when choosing either.
    pub fn distance(&self, rgb: [f64; 3]) -> f64 {
        let (pb, pr) = Self::norm_chroma(rgb);
        let (kb, kr) = Self::norm_chroma(self.color);
        ((pb - kb).powi(2) + (pr - kr).powi(2)).sqrt()
    }

    /// How much of a pixel survives this key: 0 = removed, 1 = kept.
    ///
    /// The CPU twin of the shader's keyer, so a matte can be reasoned about
    /// — and tested — without a device.
    pub fn coverage(&self, rgb: [f64; 3]) -> f64 {
        if !self.is_active() {
            return 1.0;
        }
        let d = self.distance(rgb);
        let lo = self.tolerance.max(0.0);
        let hi = lo + self.softness.max(0.0);
        if d <= lo {
            return 0.0;
        }
        if d >= hi {
            return 1.0;
        }
        let u = (d - lo) / (hi - lo);
        u * u * (3.0 - 2.0 * u)
    }

    /// Pull `spill` of the key colour's chroma back out of `rgb`, leaving
    /// its brightness alone. The CPU twin of the shader's `despill`.
    pub fn despilled(&self, rgb: [f64; 3]) -> [f64; 3] {
        let amount = self.spill.clamp(0.0, 1.0);
        if !self.is_active() || amount <= 1e-6 {
            return rgb;
        }
        let (kb, kr) = Self::chroma(self.color);
        let klen = (kb * kb + kr * kr).sqrt();
        if klen < 1e-6 {
            return rgb;
        }
        let (ub, ur) = (kb / klen, kr / klen);
        let (pb, pr) = Self::chroma(rgb);
        // Only the component pointing *at* the key is spill; a pixel whose
        // chroma leans the other way is left alone.
        let along = pb * ub + pr * ur;
        if along <= 0.0 {
            return rgb;
        }
        let cb = pb - amount * along * ub;
        let cr = pr - amount * along * ur;
        let y = 0.2126 * rgb[0] + 0.7152 * rgb[1] + 0.0722 * rgb[2];
        let r = y + 1.5748 * cr;
        let b = y + 1.8556 * cb;
        let g = (y - 0.2126 * r - 0.0722 * b) / 0.7152;
        [r.clamp(0.0, 1.0), g.clamp(0.0, 1.0), b.clamp(0.0, 1.0)]
    }
}

/// The shape a [`Mask`] cuts.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default, Serialize, Deserialize, Model)]
pub enum MaskShape {
    /// No mask at all — the layer covers wherever its picture lands.
    #[default]
    None,
    Rect,
    Ellipse,
}

impl MaskShape {
    pub const ALL: [MaskShape; 3] = [MaskShape::None, MaskShape::Rect, MaskShape::Ellipse];

    pub fn as_str(self) -> &'static str {
        match self {
            MaskShape::None => "none",
            MaskShape::Rect => "rect",
            MaskShape::Ellipse => "ellipse",
        }
    }
    pub fn label(self) -> &'static str {
        match self {
            MaskShape::None => "Off",
            MaskShape::Rect => "Rect",
            MaskShape::Ellipse => "Ellipse",
        }
    }
    pub fn from_name(s: &str) -> Self {
        match s {
            "rect" | "rectangle" | "box" => MaskShape::Rect,
            "ellipse" | "oval" | "circle" => MaskShape::Ellipse,
            _ => MaskShape::None,
        }
    }
    /// Index the shader switches on.
    pub fn index(self) -> u32 {
        match self {
            MaskShape::None => 0,
            MaskShape::Rect => 1,
            MaskShape::Ellipse => 2,
        }
    }
}

/// A feathered shape that limits where a layer contributes.
///
/// The mask is positioned in **frame** space, not in the layer's own —
/// `x`/`y` are offsets from the frame centre in frame widths and heights,
/// exactly like [`Transform::pos_x`], and `w`/`h` are fractions of the
/// frame. That is deliberate: a frame-space mask composes with the
/// transform instead of being confounded by it, so a layer can be moved
/// *behind* a stationary hole — the classic reveal — and a feather keeps
/// its width however the layer under it is scaled.
///
/// For the common case where the mask is wanted *on* the picture rather
/// than on the frame, [`Mask::over`] seeds one onto a transform's
/// on-screen box, which is one click in the inspector.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize, Model)]
pub struct Mask {
    pub shape: MaskShape,
    /// Centre, as an offset from the frame centre in frame widths/heights.
    pub x: f64,
    pub y: f64,
    /// Full width and height, as fractions of the frame.
    pub w: f64,
    pub h: f64,
    /// Clockwise rotation of the shape, degrees.
    pub rotation: f64,
    /// Softness of the edge, 0..1 of the shape's own half-extent.
    pub feather: f64,
    /// Keep the outside instead of the inside — a hole rather than a window.
    pub invert: bool,
}

impl Default for Mask {
    fn default() -> Self {
        Self {
            shape: MaskShape::None,
            x: 0.0,
            y: 0.0,
            w: 0.6,
            h: 0.6,
            rotation: 0.0,
            feather: 0.12,
            invert: false,
        }
    }
}

impl Mask {
    pub fn is_active(&self) -> bool {
        self.shape != MaskShape::None
    }

    /// A mask sized and placed over what `transform` puts on screen, for a
    /// source of aspect `src_aspect` inside a frame of aspect `frame_aspect`.
    ///
    /// This is what makes "round off my picture-in-picture" one click: the
    /// shape lands on the placed picture rather than in the middle of the
    /// frame, and the user shapes it from there.
    pub fn over(
        shape: MaskShape,
        transform: &Transform,
        src_aspect: f64,
        frame_aspect: f64,
    ) -> Self {
        // The aspect-fit ("contain") occupancy the compositor places at the
        // identity transform, mirrored from `composite.wgsl`.
        let (mut ox, mut oy) = (1.0_f64, 1.0_f64);
        if src_aspect > 0.0 && frame_aspect > 0.0 {
            if src_aspect > frame_aspect {
                oy = frame_aspect / src_aspect;
            } else {
                ox = src_aspect / frame_aspect;
            }
        }
        let (l, t, r, b) = transform.crop_rect();
        Self {
            shape,
            x: transform.pos_x,
            y: transform.pos_y,
            w: (ox * transform.scale_x.abs() * (r - l)).clamp(0.02, 4.0),
            h: (oy * transform.scale_y.abs() * (b - t)).clamp(0.02, 4.0),
            rotation: transform.rotation,
            ..Self::default()
        }
    }

    /// How much of the frame point `(u, v)` — in 0..1 frame coordinates —
    /// this mask keeps. The CPU twin of the shader's `mask_cover`.
    pub fn coverage(&self, u: f64, v: f64, frame_aspect: f64) -> f64 {
        if !self.is_active() {
            return 1.0;
        }
        let fa = if frame_aspect > 1e-6 {
            frame_aspect
        } else {
            1.0
        };
        let dx = u - 0.5 - self.x;
        let dy = v - 0.5 - self.y;
        // Rotate in aspect-corrected space so a rotated shape is not sheared.
        let (ax, ay) = (dx * fa, dy);
        let ang = -self.rotation.to_radians();
        let (ca, sa) = (ang.cos(), ang.sin());
        let (rx, ry) = (ax * ca - ay * sa, ax * sa + ay * ca);
        let hw = (self.w * 0.5 * fa).abs().max(1e-6);
        let hh = (self.h * 0.5).abs().max(1e-6);
        let (qx, qy) = (rx / hw, ry / hh);
        // A feather of zero would make the edge a step, which aliases; the
        // floor keeps it one narrow band wide instead.
        let f = self.feather.clamp(1e-3, 1.0);
        let edge = |t: f64| {
            let u = ((t - (1.0 - f)) / f).clamp(0.0, 1.0);
            1.0 - u * u * (3.0 - 2.0 * u)
        };
        let inside = match self.shape {
            MaskShape::Rect => edge(qx.abs()) * edge(qy.abs()),
            MaskShape::Ellipse => edge((qx * qx + qy * qy).sqrt()),
            MaskShape::None => 1.0,
        };
        if self.invert { 1.0 - inside } else { inside }
    }
}

/// A named point of interest, at a time. Timeline markers hang off the
/// project in absolute seconds; clip markers hang off a clip in clip-local
/// seconds so they travel with the footage.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, Model)]
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
        Self {
            id: tilo_resource::fresh_uuid(),
            t,
            name: name.into(),
            palette,
            note: String::new(),
        }
    }
}

/// Horizontal placement of a title's lines within its box.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, Model)]
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
    pub fn from_name(s: &str) -> Self {
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
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, Model)]
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
        Self {
            content: content.into(),
            ..Self::default()
        }
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
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Model)]
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
    pub fn from_name(s: &str) -> Self {
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
#[derive(
    Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Model, ui::tilo_resource::JsonSchema,
)]
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
    pub fn from_name(s: &str) -> Self {
        match s {
            "audio" => TrackKind::Audio,
            _ => TrackKind::Video,
        }
    }
}

/// Edit-point transition applied at a clip's head. `None` is a hard cut.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Model)]
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
    pub fn from_name(s: &str) -> Self {
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
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, Model)]
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
    /// Whether a *picture* source also carries a sound track.
    ///
    /// The timeline puts a video file on a video track, and the mixer used to
    /// read audio only from audio tracks — so a talking-head clip was silent
    /// in the preview and silent in the exported file, with nothing on screen
    /// admitting it. This is what lets the mixer find that sound.
    ///
    /// False for a still, and false for a sound-only file: those land on an
    /// audio track, which the mixer already read. It is also false for a
    /// container whose header the probe does not parse — "no sound track was
    /// found", not "there is none".
    #[serde(default)]
    pub has_audio: bool,
}

impl Default for MediaItem {
    fn default() -> Self {
        Self {
            id: Uuid::nil(),
            name: String::new(),
            kind: MediaKind::Video,
            has_audio: false,
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
    #[expect(
        clippy::trivially_copy_pass_by_ref,
        reason = "serde's `serialize_with` hands the field over by reference"
    )]
    pub fn serialize<S: Serializer>(h: &Hydration, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(h.as_str())
    }
    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Hydration, D::Error> {
        Ok(Hydration::from_name(&String::deserialize(d)?))
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
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, Model)]
#[model(check = Track::valid)]
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
    /// The lane's fader, in **decibels**, `0.0` being unity.
    ///
    /// Decibels rather than a linear fraction because a linear fader is the
    /// wrong control for level: half way along a `0..1` track is -6 dB, which
    /// is most of the useful range crushed into the top fifth, and a `0..2`
    /// track is worse — it reads as "200%", which is not a thing loudness
    /// does. Every mixing desk anyone has used is marked in dB, and a person
    /// mixing a voice under music thinks "bring the music down 6" rather than
    /// "multiply it by 0.501".
    ///
    /// Defaults to `0.0` and `#[serde(default)]`, so a project written before
    /// faders existed loads at unity and sounds exactly as it did.
    #[serde(default)]
    pub gain_db: f64,
    /// Where the lane sits across the stereo field: `-1` hard left, `0`
    /// centre, `+1` hard right. Defaults to centre for the same reason.
    #[serde(default)]
    pub pan: f64,
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
            gain_db: 0.0,
            pan: 0.0,
        }
    }
}

/// The quietest fader position, below which the lane is **off** rather than
/// merely quiet. -60 dB is a thousandth of full scale: past it nothing is
/// audible under anything else, and a fader that crawled on toward -∞ would
/// spend most of its travel on differences nobody can hear.
pub const MIN_GAIN_DB: f64 = -60.0;

/// The loudest. **+12 dB is four times unity**, and the reason there is a
/// ceiling at all is that a fader with none is a way to destroy a mix by
/// accident: at +30 dB every source is square-wave clipped and there is no
/// position on the track that is not.
///
/// +12 is where the desks land (Pro Tools and Reaper both stop there) because
/// it is the most a *recorded* signal usefully takes: a lav that peaked at
/// -12 dBFS reaches full scale here, and anything needing more than that
/// wants re-recording or normalising, not a fader. Above unity has to be
/// reachable — a voiceover under music is unmixable if the only direction is
/// down — and this is how far it reaches.
pub const MAX_GAIN_DB: f64 = 12.0;

/// A fader position as a linear amplitude multiplier.
///
/// At or below [`MIN_GAIN_DB`] this is exactly `0.0`: the bottom of the track
/// is silence, not a very small number, so a lane turned all the way down
/// contributes nothing at all rather than a rounding error.
pub fn db_to_linear(db: f64) -> f64 {
    if !db.is_finite() || db <= MIN_GAIN_DB {
        return 0.0;
    }
    10f64.powf(db / 20.0)
}

/// The inverse, for a readout. Silence reads as [`MIN_GAIN_DB`] rather than
/// `-inf`, which is what the track's bottom stop means.
pub fn linear_to_db(gain: f64) -> f64 {
    if gain <= 0.0 {
        return MIN_GAIN_DB;
    }
    (20.0 * gain.log10()).max(MIN_GAIN_DB)
}

impl Track {
    /// This lane's fader as a linear multiplier, clamped to the range the
    /// control offers. A value outside it can only arrive from a peer running
    /// a build with a different ceiling, and honouring it would let one
    /// participant hear a mix nobody else can reach.
    pub fn gain(&self) -> f64 {
        db_to_linear(self.gain_db.clamp(MIN_GAIN_DB, MAX_GAIN_DB))
    }

    /// The pan matrix: what this lane's signal is multiplied by in the left
    /// and the right channel.
    ///
    /// This is the **0 dB pan law** — the channel you pan toward stays at
    /// unity and the one you pan away from is attenuated to nothing. The
    /// alternative, a constant-power law, keeps a mono source's *perceived*
    /// loudness even across the sweep, but it does so by making the ends
    /// 3 dB louder than the centre, and a law that can raise a level is a law
    /// that can turn a mix that was just under full scale into one that
    /// clips. Given that silent clipping is the fault this whole surface
    /// exists to end, pan here can never add gain. The price is real and
    /// worth naming: a hard-panned mono source is about 3 dB quieter than the
    /// same source centred.
    ///
    /// On stereo material this acts as a balance, which is what a lane fader's
    /// pan does everywhere.
    pub fn pan_gains(&self) -> (f64, f64) {
        let p = self.pan.clamp(-1.0, 1.0);
        (1.0 - p.max(0.0), 1.0 + p.min(0.0))
    }

    /// How the fader reads on screen: `"0.0 dB"`, `"+4.5 dB"`, `"-12.0 dB"`,
    /// and `"-∞"` at the bottom stop, where the lane is off.
    pub fn gain_words(&self) -> String {
        let db = self.gain_db.clamp(MIN_GAIN_DB, MAX_GAIN_DB);
        if db <= MIN_GAIN_DB {
            return "-∞ dB".into();
        }
        format!("{db:+.1} dB")
    }

    /// How the pan reads: `"C"` at centre, `"L40"` / `"R25"` either side —
    /// the notation every desk uses, and shorter than any sentence.
    pub fn pan_words(&self) -> String {
        let p = self.pan.clamp(-1.0, 1.0);
        let n = (p.abs() * 100.0).round();
        if n < 0.5 {
            return "C".into();
        }
        if p < 0.0 {
            format!("L{n:.0}")
        } else {
            format!("R{n:.0}")
        }
    }

    /// Whether this lane is anywhere other than unity and centre — what a
    /// summary line has to mention and a resting lane does not.
    pub fn mix_is_set(&self) -> bool {
        self.gain_db.abs() > 1e-9 || self.pan.abs() > 1e-9
    }
}

/// A placed clip: a reference to media, positioned on a track, sourcing a
/// `[in_point, out_point]` window of that media.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, Model)]
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
    /// Tone curves, applied straight after the three grade axes. The
    /// straight line by default, which is why an untouched clip binds no
    /// lookup table at all — see [`crate::curve`].
    #[serde(default, skip_serializing_if = "Curve::is_identity")]
    pub curves: Curve,
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
    pub effects: Ordered<ClipEffect>,
    /// Where the picture lands in the frame: crop, scale, rotation,
    /// position, anchor. Identity is the aspect-fit the compositor has
    /// always drawn, so an untouched clip is pixel-identical to before.
    #[serde(default)]
    pub transform: Transform,
    /// Playback rate and direction. Normal by default.
    #[serde(default)]
    pub speed: Speed,
    /// Colour key. Disabled by default, so a clip that has never been keyed
    /// composites exactly as before.
    #[serde(default)]
    pub key: ChromaKey,
    /// Shape mask limiting where this layer contributes. [`MaskShape::None`]
    /// by default — no mask at all.
    #[serde(default)]
    pub mask: Mask,
    /// Keyframe curves. A parameter with a curve here ignores its constant
    /// field — see [`Clip::param_at`], the one door every reader goes
    /// through.
    #[serde(default, skip_serializing_if = "ClipAnimation::is_empty")]
    pub anim: ClipAnimation,
    /// Markers pinned to this clip, in clip-local seconds.
    #[serde(default)]
    pub markers: Keyed<Marker>,
    /// Which **link group** this clip belongs to, if any. Two clips carrying
    /// the same id are linked: the picture and the sound of one shot, which
    /// move, trim, split, delete and re-time together.
    ///
    /// Deliberately a shared group id rather than "my partner is clip X". A
    /// pointer has to be repaired when one half is deleted, and this document
    /// is a CRDT two people edit at once, so the repair is a conflict waiting
    /// to happen. A group id degrades safely instead: delete one half and the
    /// other is simply a clip whose group has one member, which is
    /// indistinguishable from an unlinked clip everywhere it matters (see
    /// [`Project::link_partners`]).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub link: Option<Uuid>,
    /// True when this picture clip's own sound has been split off onto an
    /// audio clip, so the mixer must not also read it from here.
    ///
    /// It is not the same question as "am I linked", and it must not be
    /// answered by asking the link, for two reasons:
    ///
    ///  * **Unlinking would double the sound.** Break the link and the picture
    ///    clip would start carrying sound the audio clip is already carrying —
    ///    the shot heard twice, at double level, and as an echo the moment the
    ///    two are slid apart.
    ///  * **Linking a music bed to a shot must not silence the shot.** A link
    ///    is a general "these move together"; only splitting the sound off
    ///    actually moves where the sound comes from.
    ///
    /// False in every project written before linked audio existed, which is
    /// exactly why those still play: they have no audio clip, so their picture
    /// clip still carries the sound.
    #[serde(default, skip_serializing_if = "is_false")]
    pub sound_split: bool,
    /// Whether edits made to one member of this clip's group reach the others.
    ///
    /// Unlinking clears **this**, not [`Self::link`], so the group survives as
    /// a record of what came from one placement. That is what lets the editor
    /// answer a question it otherwise could not: *is this shot's sound already
    /// on the timeline?* Clearing the id instead made an unlinked picture clip
    /// indistinguishable from one whose sound half had been deleted, so it was
    /// offered "Split off audio" again and taking it made a second, duplicate
    /// audio clip.
    ///
    /// With the group kept, the two cases separate exactly: a partner still
    /// shares the id (offer to re-link) or none does (offer the split). No
    /// pointer, so the CRDT argument for a group id still holds — deleting a
    /// half removes a member and repairs nothing.
    ///
    /// Defaults true so a clip written before this, and every clip with no
    /// group at all, behaves as it always did.
    #[serde(default = "yes_default", skip_serializing_if = "is_true")]
    pub link_follows: bool,
}

/// `serde(default)` for a `bool` that defaults to true.
pub(crate) fn yes_default() -> bool {
    true
}

/// `skip_serializing_if` for a `bool` that defaults to true.
#[expect(
    clippy::trivially_copy_pass_by_ref,
    reason = "serde's `skip_serializing_if` hands the field over by reference"
)]
fn is_true(b: &bool) -> bool {
    *b
}

/// `skip_serializing_if` for a `bool` that defaults to false.
#[expect(
    clippy::trivially_copy_pass_by_ref,
    reason = "serde's `skip_serializing_if` hands the field over by reference"
)]
fn is_false(b: &bool) -> bool {
    !*b
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
            curves: Curve::default(),
            fade_in: 0.0,
            fade_out: 0.0,
            transition_duration: 0.0,
            effects: Ordered::new(),
            transform: Transform::default(),
            speed: Speed::default(),
            key: ChromaKey::default(),
            mask: Mask::default(),
            anim: ClipAnimation::default(),
            markers: Keyed::new(),
            link: None,
            link_follows: true,
            sound_split: false,
        }
    }
}

/// Default incoming-transition length, seconds, for a clip that has not been
/// given one. Matches the constant the compositor used to hardcode.
pub const DEFAULT_TRANSITION_SECS: f64 = 0.6;

/// How close a keyframe has to be to an instant to count as sitting *on*
/// it: half a frame at 60fps. One constant so "is there a key here",
/// "remove the key here" and "re-shape the key here" can never disagree
/// about which key they mean.
pub const KEY_TOL: f64 = 1.0 / 60.0;

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

    /// The current index of one immutable effect instance.
    pub fn effect_index(&self, id: Uuid) -> Option<usize> {
        self.effects.iter().position(|effect| effect.id == id)
    }

    /// Remove the named effect instance. A stale id is a no-op rather than a
    /// chance to remove whichever row slid into its old position.
    pub fn remove_effect_id(&mut self, id: Uuid) -> bool {
        let Some(index) = self.effect_index(id) else {
            return false;
        };
        self.remove_effect(index);
        true
    }

    /// Move the named effect instance one slot toward the beginning/end of
    /// the stack. The identity and its positional animation curve move
    /// together.
    pub fn move_effect_id(&mut self, id: Uuid, up: bool) -> bool {
        let Some(index) = self.effect_index(id) else {
            return false;
        };
        let target = if up {
            index.checked_sub(1)
        } else {
            (index + 1 < self.effects.len()).then_some(index + 1)
        };
        let Some(target) = target else {
            return false;
        };
        self.effects.swap(index, target);
        self.anim.swap_effect_slots(index as u32, target as u32);
        true
    }
    /// Remove the effect at `idx` (no-op if out of range).
    pub fn remove_effect(&mut self, idx: usize) {
        if idx < self.effects.len() {
            self.effects.remove(idx);
            self.anim.remove_effect_slot(idx as u32);
        }
    }
    /// Move the effect at `idx` one slot earlier (toward the start of the
    /// stack), changing the order it's applied. No-op at the top / out of range.
    pub fn move_effect_up(&mut self, idx: usize) {
        if idx > 0 && idx < self.effects.len() {
            self.effects.swap(idx, idx - 1);
            self.anim.swap_effect_slots(idx as u32, (idx - 1) as u32);
        }
    }
    /// Move the effect at `idx` one slot later. No-op at the bottom / out of range.
    pub fn move_effect_down(&mut self, idx: usize) {
        if idx + 1 < self.effects.len() {
            self.effects.swap(idx, idx + 1);
            self.anim.swap_effect_slots(idx as u32, (idx + 1) as u32);
        }
    }

    /// Clear the stack and every animation curve belonging to it, preserving
    /// unrelated clip animation.
    pub fn clear_effects(&mut self) {
        self.effects.clear();
        self.anim.clear_effect_slots();
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
            Param::KeyTolerance => self.key.tolerance,
            Param::KeySoftness => self.key.softness,
            Param::KeySpill => self.key.spill,
            Param::MaskX => self.mask.x,
            Param::MaskY => self.mask.y,
            Param::MaskW => self.mask.w,
            Param::MaskH => self.mask.h,
            Param::MaskRotation => self.mask.rotation,
            Param::MaskFeather => self.mask.feather,
            Param::CurveMix => self.curves.mix,
            Param::EffectAmount(i) => self.effects.get(i as usize)?.amount,
        })
    }

    /// Write `param`'s **constant** field — the write-side twin of
    /// [`Self::param_const`], and the other half of what a control bound to a
    /// parameter needs. Silently ignores an `EffectAmount` past the end of
    /// the stack, for the same reason reading one returns `None`.
    ///
    /// This writes the constant only. A caller editing an *animated*
    /// parameter wants [`Self::set_key_at`] instead, or the value it writes
    /// will be overridden by the curve the moment it is read back.
    pub fn set_param_const(&mut self, param: Param, v: f64) {
        match param {
            Param::Opacity => self.opacity = v,
            Param::Volume => self.volume = v,
            Param::Brightness => self.filters.brightness = v,
            Param::Contrast => self.filters.contrast = v,
            Param::Saturation => self.filters.saturation = v,
            Param::ScaleX => self.transform.scale_x = v,
            Param::ScaleY => self.transform.scale_y = v,
            Param::PosX => self.transform.pos_x = v,
            Param::PosY => self.transform.pos_y = v,
            Param::Rotation => self.transform.rotation = v,
            Param::KeyTolerance => self.key.tolerance = v,
            Param::KeySoftness => self.key.softness = v,
            Param::KeySpill => self.key.spill = v,
            Param::MaskX => self.mask.x = v,
            Param::MaskY => self.mask.y = v,
            Param::MaskW => self.mask.w = v,
            Param::MaskH => self.mask.h = v,
            Param::MaskRotation => self.mask.rotation = v,
            Param::MaskFeather => self.mask.feather = v,
            Param::CurveMix => self.curves.mix = v,
            Param::EffectAmount(i) => {
                if let Some(e) = self.effects.get_mut(i as usize) {
                    e.amount = v;
                }
            }
        }
    }

    /// Set `param` to `v` at `playhead`, keying it when it is animated and
    /// writing its constant when it is not.
    ///
    /// This is what a slider drag calls, and it is why keying a parameter
    /// once turns every later drag into a new keyframe: an animated
    /// parameter's constant is dead, so writing it would look like the
    /// control had stopped working.
    pub fn set_param_at(&mut self, param: Param, playhead: f64, v: f64) {
        if self.anim.is_animated(param) {
            let interp = self.key_interp_at(param, playhead).unwrap_or_default();
            self.set_key_at(param, playhead, v, interp);
        } else {
            self.set_param_const(param, v);
        }
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

    /// Remove `param`'s keyframe near `playhead` (within [`KEY_TOL`]).
    /// Returns whether one went.
    pub fn remove_key_at(&mut self, param: Param, playhead: f64) -> bool {
        let local = playhead - self.start;
        let Some(track) = self.anim.track_mut(param) else {
            return false;
        };
        let gone = track.remove_key_near(local, KEY_TOL);
        if gone && track.keys.is_empty() {
            self.anim.clear(param);
        }
        gone
    }

    /// Whether `param` has a key sitting on `playhead` (within
    /// [`KEY_TOL`]). This is what tells a keyframe control apart from
    /// "animated, but the playhead is between keys" — three states the UI
    /// has to keep distinct, so it goes through one predicate.
    pub fn has_key_at(&self, param: Param, playhead: f64) -> bool {
        let local = playhead - self.start;
        self.anim
            .track(param)
            .is_some_and(|t| t.keys.iter().any(|k| (k.t - local).abs() <= KEY_TOL))
    }

    /// The interpolation of `param`'s key at `playhead`, or `None` when
    /// there is no key there to have one.
    pub fn key_interp_at(&self, param: Param, playhead: f64) -> Option<Interp> {
        let local = playhead - self.start;
        self.anim
            .track(param)?
            .keys
            .iter()
            .find(|k| (k.t - local).abs() <= KEY_TOL)
            .map(|k| k.interp)
    }

    /// Re-shape how `param`'s key at `playhead` leaves itself. Returns
    /// whether there was a key there to re-shape.
    pub fn set_interp_at(&mut self, param: Param, playhead: f64, interp: Interp) -> bool {
        let local = playhead - self.start;
        let Some(track) = self.anim.track_mut(param) else {
            return false;
        };
        let Some(k) = track
            .keys
            .iter_mut()
            .find(|k| (k.t - local).abs() <= KEY_TOL)
        else {
            return false;
        };
        k.interp = interp;
        true
    }

    /// Every instant this clip has a key on, in **absolute timeline
    /// seconds**, sorted and deduplicated across parameters.
    ///
    /// Absolute because the callers are the timeline (drawing ticks on the
    /// clip body) and the playhead (stepping between keys), both of which
    /// work in timeline time. Deduplicated because two parameters keyed
    /// together are one instant to a reader, not two.
    pub fn key_times(&self) -> Vec<f64> {
        let mut out: Vec<f64> = self
            .anim
            .tracks
            .iter()
            .flat_map(|t| t.keys.iter().map(|k| self.start + k.t))
            .collect();
        out.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        out.dedup_by(|a, b| (*a - *b).abs() <= KEY_TOL);
        out
    }

    /// The first key strictly after `playhead`, absolute. `None` at or past
    /// the last one — navigation stops at the end rather than wrapping,
    /// because a wrap reads as "nothing happened" when there is one key.
    pub fn next_key_after(&self, playhead: f64) -> Option<f64> {
        self.key_times()
            .into_iter()
            .find(|t| *t > playhead + KEY_TOL)
    }

    /// The last key strictly before `playhead`, absolute.
    pub fn prev_key_before(&self, playhead: f64) -> Option<f64> {
        self.key_times()
            .into_iter()
            .rev()
            .find(|t| *t < playhead - KEY_TOL)
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

    /// The tone curve at `playhead`, with an animated [`Param::CurveMix`]
    /// substituted in. The shapes themselves are static — only how much of
    /// them lands is a curve of its own.
    ///
    /// The result is [`Curve::normalised`] whatever the source, so a
    /// document or a peer cannot hand the bake a spline with points out of
    /// order or off the ends.
    pub fn curves_at(&self, playhead: f64) -> Curve {
        let mut c = self.curves.normalised();
        if !self.anim.is_empty() {
            c.mix = self.param_at(Param::CurveMix, playhead).clamp(0.0, 1.0);
        }
        c
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

    /// The colour key at `playhead`, with any animated axis substituted in.
    /// `enabled` and the key colour are not animatable — a key that
    /// switched itself on mid-shot, or drifted hue, is a re-key rather than
    /// an animation.
    pub fn key_at(&self, playhead: f64) -> ChromaKey {
        if self.anim.is_empty() {
            return self.key;
        }
        ChromaKey {
            tolerance: self.param_at(Param::KeyTolerance, playhead),
            softness: self.param_at(Param::KeySoftness, playhead),
            spill: self.param_at(Param::KeySpill, playhead),
            ..self.key
        }
    }

    /// The mask at `playhead`. Everything geometric animates — a mask that
    /// tracks a moving subject is the whole point — while `shape` and
    /// `invert` stay static, since neither has an in-between value.
    pub fn mask_at(&self, playhead: f64) -> Mask {
        if self.anim.is_empty() {
            return self.mask;
        }
        Mask {
            x: self.param_at(Param::MaskX, playhead),
            y: self.param_at(Param::MaskY, playhead),
            w: self.param_at(Param::MaskW, playhead),
            h: self.param_at(Param::MaskH, playhead),
            rotation: self.param_at(Param::MaskRotation, playhead),
            feather: self.param_at(Param::MaskFeather, playhead),
            ..self.mask
        }
    }

    /// The effect stack at `playhead`, with animated intensities applied.
    pub fn effects_at(&self, playhead: f64) -> Vec<ClipEffect> {
        if self.anim.is_empty() {
            return self.effects.to_vec();
        }
        self.effects
            .iter()
            .enumerate()
            .map(|(i, e)| ClipEffect {
                id: e.id,
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
        raw.clamp(
            self.in_point.min(self.out_point),
            self.out_point.max(self.in_point),
        )
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
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, Model)]
pub struct Project {
    pub name: String,
    pub fps: f64,
    pub width: u32,
    pub height: u32,
    /// Whether the frame size and rate above were *chosen* — by a person, or
    /// by a project file that carried them — rather than being the blank
    /// project's stand-in.
    ///
    /// A blank project has to start at some shape, and 1920×1080 at 30 is the
    /// least surprising one; but a project that has never been told its
    /// format is not the same thing as one that was told 16:9. Without this
    /// flag the two are indistinguishable, and [`Self::adopt_format`] would
    /// have to choose between never adopting a clip's shape (which is how
    /// vertical footage ended up letterboxed with no way out) and overriding
    /// a format the user had just set by hand.
    #[serde(default)]
    pub format_explicit: bool,
    /// Timeline **in** mark, in absolute seconds: where a render of the
    /// timeline starts. `None` is the head of the timeline.
    #[serde(default)]
    pub mark_in: Option<f64>,
    /// Timeline **out** mark. `None` is the end of the timeline.
    #[serde(default)]
    pub mark_out: Option<f64>,
    pub media: Keyed<MediaItem>,
    pub tracks: Keyed<Track>,
    pub clips: Keyed<Clip>,
    /// Timeline markers, in absolute seconds, kept sorted by time.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub markers: Keyed<Marker>,
}

/// The narrowest and widest frame dimension a project may be set to.
///
/// Not a guess: the low stop keeps a frame big enough for the even-dimension
/// rounding every H.264 encoder needs, and the high stop is 8K, past which
/// the compositor's readback buffer for one frame is measured in hundreds of
/// megabytes. A value outside the pair is clamped rather than refused — "as
/// big as it goes" is a legible intent.
pub const MIN_FRAME: u32 = 16;
pub const MAX_FRAME: u32 = 8192;
/// The slowest and fastest project rate. One frame per second is a legitimate
/// timelapse; 240 is the fastest rate any consumer camera shoots.
pub const MIN_FPS: f64 = 1.0;
pub const MAX_FPS: f64 = 240.0;

impl Project {
    pub fn new(name: &str) -> Self {
        Self {
            name: name.to_string(),
            fps: 30.0,
            width: 1920,
            height: 1080,
            format_explicit: false,
            mark_in: None,
            mark_out: None,
            media: Keyed::new(),
            tracks: Keyed::new(),
            clips: Keyed::new(),
            markers: Keyed::new(),
        }
    }

    // --- format ----------------------------------------------------------

    /// Set the frame size, clamped to what can be encoded and rendered, and
    /// rounded to even dimensions (every H.264 encoder requires them, and
    /// `output_dims` would otherwise round the aspect back off by half a
    /// pixel). Marks the format as deliberately chosen.
    pub fn set_frame_size(&mut self, width: u32, height: u32) {
        let even = |v: u32| {
            let v = v.clamp(MIN_FRAME, MAX_FRAME);
            v - (v & 1)
        };
        self.width = even(width);
        self.height = even(height);
        self.format_explicit = true;
    }

    /// Set the project rate, clamped to a rate that can be played back.
    pub fn set_fps(&mut self, fps: f64) {
        self.fps = if fps.is_finite() {
            fps.clamp(MIN_FPS, MAX_FPS)
        } else {
            self.fps
        };
        self.format_explicit = true;
    }

    /// The project's frame aspect, falling back to 16:9 for a degenerate
    /// height rather than dividing by zero.
    pub fn aspect(&self) -> f64 {
        if self.height > 0 {
            f64::from(self.width) / f64::from(self.height)
        } else {
            16.0 / 9.0
        }
    }

    /// Take this media's shape as the project's own — what a blank project
    /// does when its first clip lands, so footage that is vertical, square or
    /// 4K does not have to be letterboxed into a 16:9 frame nobody chose.
    ///
    /// Refused once the format has been chosen deliberately, and refused for
    /// a source that does not carry a shape (audio, a title, a container that
    /// declared nothing) — adopting zeroes would leave the project with no
    /// frame at all.
    pub fn adopt_format(&mut self, m: &MediaItem) -> bool {
        if self.format_explicit || m.width == 0 || m.height == 0 {
            return false;
        }
        self.set_frame_size(m.width, m.height);
        // A container that did not say what rate it was shot at leaves the
        // project's own rate alone: 0 fps is not a timeline anyone can edit.
        if m.fps > 0.0 {
            self.set_fps(m.fps);
        }
        true
    }

    // --- in / out marks --------------------------------------------------

    /// Mark **in** at `t`. An in mark at or past the out mark would describe
    /// an empty render, so it drops the out mark rather than silently
    /// producing a zero-frame file.
    pub fn set_mark_in(&mut self, t: f64) {
        let t = t.max(0.0);
        self.mark_in = Some(t);
        if self.mark_out.is_some_and(|o| o <= t) {
            self.mark_out = None;
        }
    }

    /// Mark **out** at `t`, on the same rule as [`Self::set_mark_in`].
    pub fn set_mark_out(&mut self, t: f64) {
        let t = t.max(0.0);
        self.mark_out = Some(t);
        if self.mark_in.is_some_and(|i| i >= t) {
            self.mark_in = None;
        }
    }

    pub fn clear_marks(&mut self) {
        self.mark_in = None;
        self.mark_out = None;
    }

    pub fn has_marks(&self) -> bool {
        self.mark_in.is_some() || self.mark_out.is_some()
    }

    /// The span a render covers: the marks when they describe a real span,
    /// and the whole timeline otherwise. One door, so the export, the
    /// estimate and the on-screen band cannot disagree about what will be
    /// rendered.
    pub fn export_range(&self) -> (f64, f64) {
        let dur = self.duration();
        let a = self.mark_in.unwrap_or(0.0).clamp(0.0, dur);
        let b = self.mark_out.map_or(dur, |o| o.clamp(0.0, dur));
        if b > a { (a, b) } else { (0.0, dur) }
    }

    /// How long a render of this project runs — the marked span, or the whole
    /// timeline when nothing is marked.
    pub fn export_duration(&self) -> f64 {
        let (a, b) = self.export_range();
        (b - a).max(0.0)
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

    /// The next / previous marked instant after / before `t`, in absolute
    /// seconds, across **every** marker — the project's own and each
    /// clip's, rebased onto where its clip sits.
    ///
    /// [`Self::next_marker`] answers with a project marker so the caller can
    /// name it; these answer with a time, because navigation only needs to
    /// know where to go and a clip marker is just as valid a destination.
    pub fn next_marker_at(&self, t: f64) -> Option<f64> {
        self.all_markers()
            .into_iter()
            .map(|(at, _, _)| at)
            .filter(|at| *at > t + 1e-4)
            .min_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal))
    }

    pub fn prev_marker_at(&self, t: f64) -> Option<f64> {
        self.all_markers()
            .into_iter()
            .map(|(at, _, _)| at)
            .filter(|at| *at < t - 1e-4)
            .max_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal))
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

    /// Every lane in `lanes` with its fader brought **down** by `db`, as the
    /// whole-track writes that change is.
    ///
    /// Every lane moves by the same number, and that is what makes this a trim
    /// rather than a re-mix: a mix is a sum, scaling every layer of a sum by
    /// one factor scales the sum by exactly that factor, so the balance
    /// between the lanes is left precisely where the person mixing put it and
    /// only the total comes down.
    ///
    /// Moving only the lanes that were *caught* over would do neither. It
    /// changes the balance, and it does not even guarantee the sum fits —
    /// a mix goes over whenever the lanes add up past full scale, which two
    /// lanes at two-thirds each manage without either one being over on its
    /// own.
    ///
    /// `db` at or below zero is nothing to do and writes nothing. A lane that
    /// lands on [`MIN_GAIN_DB`] has hit the bottom of its travel and is off,
    /// which can only make the sum quieter than asked for — so the result is
    /// "at or under the target", never over it.
    pub fn lanes_trimmed_by(&self, lanes: &[TrackId], db: f64) -> Vec<Track> {
        if !db.is_finite() || db <= 0.0 {
            return Vec::new();
        }
        let mut seen: Vec<TrackId> = Vec::new();
        let mut out = Vec::new();
        for id in lanes {
            if seen.contains(id) {
                continue;
            }
            seen.push(*id);
            let Some(t) = self.track(*id) else { continue };
            let mut next = t.clone();
            next.gain_db = (t.gain_db - db).clamp(MIN_GAIN_DB, MAX_GAIN_DB);
            out.push(next);
        }
        out
    }

    /// Tracks in vertical display order, top → bottom: video lanes
    /// (highest index first), then audio lanes (lowest index first).
    /// This matches every NLE's "video above, audio below" convention.
    pub fn display_tracks(&self) -> Vec<&Track> {
        let mut video: Vec<&Track> = self
            .tracks
            .iter()
            .filter(|t| t.kind == TrackKind::Video)
            .collect();
        video.sort_by_key(|t| std::cmp::Reverse(t.index));
        let mut audio: Vec<&Track> = self
            .tracks
            .iter()
            .filter(|t| t.kind == TrackKind::Audio)
            .collect();
        audio.sort_by_key(|a| a.index);
        video.into_iter().chain(audio).collect()
    }

    /// Clips on `track`, sorted left-to-right by start.
    pub fn clips_on(&self, track: TrackId) -> Vec<&Clip> {
        let mut v: Vec<&Clip> = self.clips.iter().filter(|c| c.track == track).collect();
        v.sort_by(|a, b| {
            a.start
                .partial_cmp(&b.start)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        v
    }

    /// Total timeline length = the furthest clip end (min 1s so an empty
    /// project still has a ruler to look at).
    pub fn duration(&self) -> f64 {
        self.clips
            .iter()
            .map(Clip::end)
            .fold(0.0_f64, f64::max)
            .max(1.0)
    }

    /// The video clip visible at timeline time `t`: the one on the
    /// highest video track (largest index) whose span covers `t`. This
    /// is what the preview canvas renders.
    pub fn top_video_clip_at(&self, t: f64) -> Option<&Clip> {
        let mut best: Option<(&Clip, u32)> = None;
        for c in &self.clips {
            let Some(track) = self.track(c.track) else {
                continue;
            };
            if track.kind != TrackKind::Video {
                continue;
            }
            if !c.covers(t) {
                continue;
            }
            if best.is_none_or(|(_, idx)| track.index > idx) {
                best = Some((c, track.index));
            }
        }
        best.map(|(c, _)| c)
    }

    /// Append a clip referencing `media`'s full length at timeline `start`
    /// on `track`. Returns the new clip id. Inherits the media's palette.
    pub fn add_clip(&mut self, track: TrackId, media: MediaId, start: f64) -> Option<ClipId> {
        let m = self.media(media)?.clone();
        let id = tilo_resource::fresh_uuid();
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
            effects: Ordered::new(),
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
        right.id = tilo_resource::fresh_uuid();
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
            .map(|m| Marker {
                t: m.t - offset,
                ..m.clone()
            })
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

    /// The lane kind a clip belongs on: the kind of the track it already sits
    /// on, and only failing that the kind of media it references.
    ///
    /// The track has to be asked first because one source can legitimately be
    /// placed as two clips. The audio half of a linked pair references a video
    /// file and lives on an audio lane; deriving its kind from the media would
    /// call it a video clip, and [`Self::can_place`] would then refuse to let
    /// it move along its own lane. Falls back to video for a clip whose track
    /// *and* media are both gone, so a broken clip still has a home rather
    /// than silently migrating to audio.
    pub fn clip_kind(&self, id: ClipId) -> TrackKind {
        if let Some(kind) = self
            .clip(id)
            .and_then(|c| self.track(c.track))
            .map(|t| t.kind)
        {
            return kind;
        }
        self.media_kind_of(id)
    }

    /// The lane kind the clip's *source* asks for, ignoring where it currently
    /// sits — what a fresh placement of that media would choose.
    pub fn media_kind_of(&self, id: ClipId) -> TrackKind {
        self.clip(id)
            .and_then(|c| self.media(c.media))
            .map_or(TrackKind::Video, |m| {
                if m.kind.is_visual() {
                    TrackKind::Video
                } else {
                    TrackKind::Audio
                }
            })
    }

    // --- linked audio and video ------------------------------------------

    /// Every clip in `id`'s link group, itself included, in a settled order
    /// (by start, then by id) so two peers agree about it.
    ///
    /// A clip with no link, or whose group has lost every other member, is a
    /// group of one — which is exactly how a half-deleted pair degrades.
    pub fn link_group(&self, id: ClipId) -> Vec<ClipId> {
        let Some(clip) = self.clip(id) else {
            return Vec::new();
        };
        let Some(link) = clip.link else {
            return vec![id];
        };
        // A clip whose link no longer follows is a group of one for every
        // purpose that moves clips about — the group id survives only so the
        // editor can still tell "its sound is on the timeline, unlinked" from
        // "its sound is gone". See `group_members`.
        if !clip.link_follows {
            return vec![id];
        }
        let mut v: Vec<(f64, ClipId)> = self
            .clips
            .iter()
            .filter(|c| c.link == Some(link) && c.link_follows)
            .map(|c| (c.start, c.id))
            .collect();
        v.sort_by(|a, b| {
            a.0.partial_cmp(&b.0)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| a.1.cmp(&b.1))
        });
        v.into_iter().map(|(_, c)| c).collect()
    }

    /// The *other* clips linked to `id`. Empty when nothing is linked to it.
    pub fn link_partners(&self, id: ClipId) -> Vec<ClipId> {
        let mut v = self.link_group(id);
        v.retain(|c| *c != id);
        v
    }

    /// Whether `id` has at least one partner — the question every affordance
    /// asks, because a group of one is not a link, it is a clip.
    pub fn is_linked(&self, id: ClipId) -> bool {
        !self.link_partners(id).is_empty()
    }

    /// Everything that came from the same placement as `id`, **including
    /// clips whose link no longer follows**, and excluding `id` itself.
    ///
    /// [`Self::link_partners`] answers "what moves when this moves"; this
    /// answers "what came from here". They differ only after an unlink, and
    /// that difference is the whole point: it is what lets the editor offer
    /// to re-link a shot to a sound clip that is still sitting on the
    /// timeline, instead of offering to split off a second copy of it.
    pub fn group_members(&self, id: ClipId) -> Vec<ClipId> {
        let Some(link) = self.clip(id).and_then(|c| c.link) else {
            return Vec::new();
        };
        self.clips
            .iter()
            .filter(|c| c.link == Some(link) && c.id != id)
            .map(|c| c.id)
            .collect()
    }

    /// The sound clip this picture clip's audio was split onto, if it is
    /// still on the timeline — linked or merely unlinked.
    pub fn detached_sound(&self, id: ClipId) -> Option<ClipId> {
        self.group_members(id)
            .into_iter()
            .find(|p| self.clip_kind(*p) == TrackKind::Audio)
    }

    /// Put every clip in `ids` into one fresh link group, returning its id.
    ///
    /// Refused, changing nothing, for fewer than two clips that actually
    /// exist: linking a clip to itself is not a state the rest of the editor
    /// should have to recognise. The id is minted here, so a caller that
    /// dispatches the resulting clips carries it on the wire rather than
    /// re-rolling it on every peer.
    pub fn link_clips(&mut self, ids: &[ClipId]) -> Option<Uuid> {
        let mut live: Vec<ClipId> = Vec::new();
        for id in ids {
            if self.clip(*id).is_some() && !live.contains(id) {
                live.push(*id);
            }
        }
        if live.len() < 2 {
            return None;
        }
        let link = tilo_resource::fresh_uuid();
        for id in &live {
            if let Some(c) = self.clip_mut(*id) {
                c.link = Some(link);
                // Linking is the act of making edits follow again.
                c.link_follows = true;
            }
        }
        Some(link)
    }

    /// Take every clip in `ids` out of whatever group it was in, returning the
    /// clips that actually changed so the caller replicates exactly those.
    ///
    /// A group left holding one member is left alone: it already reads as
    /// unlinked everywhere ([`Self::is_linked`]), and clearing it would be a
    /// second write nobody asked for.
    /// Take `ids` out of their group entirely — id and following both.
    ///
    /// Different from [`Self::unlink_clips`], and the difference matters. An
    /// unlink is a user saying "stop moving together", and the group survives
    /// so the editor can still find the sound a shot was split from. This is
    /// for a clip that was never part of that placement at all — the tail of
    /// a cut, which is a new clip — and leaving it holding the group would
    /// tell it its sound is already on the timeline when the sound it can see
    /// belongs to the head.
    pub fn detach_from_group(&mut self, ids: &[ClipId]) {
        for id in ids {
            if let Some(c) = self.clip_mut(*id) {
                c.link = None;
                c.link_follows = true;
            }
        }
    }

    pub fn unlink_clips(&mut self, ids: &[ClipId]) -> Vec<ClipId> {
        let mut changed = Vec::new();
        for id in ids {
            // The group id stays. Clearing it made an unlinked picture clip
            // indistinguishable from one whose sound half had been deleted,
            // so the editor offered to split the sound off again and made a
            // duplicate audio clip.
            if let Some(c) = self.clip_mut(*id)
                && c.link.is_some()
                && std::mem::replace(&mut c.link_follows, false)
            {
                changed.push(*id);
            }
        }
        changed
    }

    /// Place the *sound* of `video_clip` on `audio_track` as its linked half:
    /// the same source, the same `start`, the same trim window, the same rate.
    ///
    /// Returns the new clip's id, or `None` when the picture clip is gone or
    /// the two are already linked. The pair shares a freshly minted group id,
    /// so neither half points at the other.
    pub fn add_linked_audio(&mut self, video_clip: ClipId, audio_track: TrackId) -> Option<ClipId> {
        let mut audio = self.clip(video_clip)?.clone();
        let link = tilo_resource::fresh_uuid();
        audio.id = tilo_resource::fresh_uuid();
        audio.track = audio_track;
        audio.link = Some(link);
        // The sound carries the shot's timing and level, not its picture: a
        // grade, a mask, a key, a geometry and an opacity ramp all describe
        // frames the audio half never draws, and carrying them would make the
        // two halves disagree about what an "identical" clip is.
        audio.filters = Filters::default();
        audio.curves = Curve::default();
        audio.effects = Ordered::new();
        audio.transform = Transform::default();
        audio.key = ChromaKey::default();
        audio.mask = Mask::default();
        audio.opacity = 1.0;
        audio.transition = Transition::None;
        audio.transition_duration = 0.0;
        audio.anim = ClipAnimation::default();
        // The sound now comes from here, so this clip is the one that carries
        // it — never a clip whose own sound was split off somewhere else.
        audio.sound_split = false;
        let id = audio.id;
        self.clips.push(audio);
        if let Some(v) = self.clip_mut(video_clip) {
            v.link = Some(link);
            // Durable, and deliberately not undone by unlinking: the sound has
            // moved to a clip of its own, and reading it from both would play
            // the shot twice.
            v.sound_split = true;
        }
        Some(id)
    }

    /// Split every clip in `id`'s link group at timeline time `at`, returning
    /// the `(head, tail)` pair each one produced.
    ///
    /// The heads stay in the group they were already in and the tails become a
    /// group of their own, because the alternative — four clips in one group —
    /// would make dragging the second half of the shot drag the first half
    /// with it. A tail with no sibling is left unlinked rather than in a group
    /// of one, so it does not stay tied to the head it was cut from.
    pub fn split_linked(&mut self, id: ClipId, at: f64) -> Vec<(ClipId, ClipId)> {
        let group = self.link_group(id);
        self.split_group(&group, at)
    }

    /// [`Self::split_linked`] over an explicit set of clips — what a caller
    /// that has already dropped the members a track lock forbids editing hands
    /// in, so a link cannot be used to cut through a locked lane.
    pub fn split_group(&mut self, ids: &[ClipId], at: f64) -> Vec<(ClipId, ClipId)> {
        let mut pairs = Vec::new();
        for gid in ids {
            if let Some(rid) = self.split_clip(*gid, at) {
                pairs.push((*gid, rid));
            }
        }
        let tails: Vec<ClipId> = pairs.iter().map(|(_, r)| *r).collect();
        if tails.len() < 2 {
            self.detach_from_group(&tails);
        } else {
            self.link_clips(&tails);
        }
        pairs
    }

    /// Delete every clip in `id`'s link group, leaving the gaps where they
    /// were. Returns the ids removed.
    pub fn delete_linked(&mut self, id: ClipId) -> Vec<ClipId> {
        let group = self.link_group(id);
        self.clips.retain(|c| !group.contains(&c.id));
        group
    }

    /// Ripple-delete every clip in `id`'s link group, closing the gap on each
    /// clip's own lane. Returns the ids removed and the ids that shifted.
    ///
    /// Deleted last-first so each remaining clip's start is still valid while
    /// the earlier ones are there — the same order a multi-clip ripple uses.
    pub fn ripple_delete_linked(&mut self, id: ClipId) -> (Vec<ClipId>, Vec<ClipId>) {
        let group = self.link_group(id);
        let mut moved: Vec<ClipId> = Vec::new();
        for gid in group.iter().rev() {
            for m in self.ripple_delete(*gid) {
                if !moved.contains(&m) {
                    moved.push(m);
                }
            }
        }
        moved.retain(|m| !group.contains(m));
        (group, moved)
    }

    /// Trim the head of `id` and of every clip linked to it by the *same*
    /// amount — the amount `id` itself could actually take after clamping.
    /// Returns that amount.
    pub fn trim_in_linked(&mut self, id: ClipId, delta: f64) -> f64 {
        let also = self.link_partners(id);
        self.trim_in_with(id, delta, &also)
    }

    /// [`Self::trim_in_linked`] over an explicit set of followers.
    pub fn trim_in_with(&mut self, id: ClipId, delta: f64, also: &[ClipId]) -> f64 {
        let applied = self.trim_in(id, delta);
        if applied != 0.0 {
            for other in also {
                if *other != id {
                    self.trim_in(*other, applied);
                }
            }
        }
        applied
    }

    /// [`Self::trim_in_linked`] for the tail.
    pub fn trim_out_linked(&mut self, id: ClipId, delta: f64) -> f64 {
        let also = self.link_partners(id);
        self.trim_out_with(id, delta, &also)
    }

    /// [`Self::trim_out_linked`] over an explicit set of followers.
    pub fn trim_out_with(&mut self, id: ClipId, delta: f64, also: &[ClipId]) -> f64 {
        let applied = self.trim_out(id, delta);
        if applied != 0.0 {
            for other in also {
                if *other != id {
                    self.trim_out(*other, applied);
                }
            }
        }
        applied
    }

    /// Re-time `id` and every clip linked to it to the same rate and
    /// direction.
    ///
    /// Speed is the one field where following the link is not a convenience:
    /// change the picture's rate and not the sound's and the two drift apart
    /// by a growing amount for the rest of the shot, which is a fault a user
    /// cannot diagnose from anything on screen.
    pub fn set_speed_linked(&mut self, id: ClipId, rate: f64, reverse: bool) {
        let group = self.link_group(id);
        self.set_speed_group(&group, rate, reverse);
    }

    /// [`Self::set_speed_linked`] over an explicit set of clips.
    pub fn set_speed_group(&mut self, ids: &[ClipId], rate: f64, reverse: bool) {
        for gid in ids {
            if let Some(c) = self.clip_mut(*gid) {
                c.set_speed(rate, reverse);
            }
        }
    }

    /// Whether `id` is a picture clip whose source carries sound and which has
    /// no **linked** audio clip carrying it — the state the "split off audio"
    /// affordance exists for.
    ///
    /// Deliberately asks the link rather than [`Clip::sound_split`]. The flag
    /// says the sound left the picture; it does not say the clip it left for
    /// still exists. Deleting the audio lane takes every sound half with it,
    /// and a rule that read the flag would leave every shot on the timeline
    /// permanently silent with nothing on screen offering a way back. Asking
    /// the link instead makes that recoverable: press it and the sound gets a
    /// clip again.
    ///
    /// True for every clip in a project saved before linked audio existed,
    /// which is how those get a sound clip at all.
    pub fn can_split_audio(&self, id: ClipId) -> bool {
        let Some(clip) = self.clip(id) else {
            return false;
        };
        if self.clip_kind(id) != TrackKind::Video {
            return false;
        }
        // Asked of the *placement*, not the link: a shot whose sound was
        // split off and then unlinked still has that sound on the timeline,
        // and offering to split it again would make a second copy.
        if self.detached_sound(id).is_some() {
            return false;
        }
        self.media(clip.media)
            .is_some_and(|m| m.kind.is_visual() && m.has_audio)
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
        let Some(t) = self.track(track) else {
            return false;
        };
        if self.any_solo(t.kind) {
            return t.solo;
        }
        !t.muted
    }

    /// [`Self::track_audible`] for the track a clip sits on.
    pub fn clip_audible(&self, id: ClipId) -> bool {
        self.clip(id).is_some_and(|c| self.track_audible(c.track))
    }

    /// Whether `track` refuses edits. A missing track counts as locked: there
    /// is nothing there to edit.
    pub fn track_locked(&self, track: TrackId) -> bool {
        self.track(track).is_none_or(|t| t.locked)
    }

    /// Whether the clip that owns `id` may currently be edited — false when
    /// its own track is locked.
    pub fn clip_editable(&self, id: ClipId) -> bool {
        self.clip(id).is_some_and(|c| !self.track_locked(c.track))
    }

    /// Whether clip `id` may be placed on `track`: the track must exist, be
    /// unlocked, match the clip's kind, and the clip must not already be
    /// pinned to a locked track of its own.
    pub fn can_place(&self, id: ClipId, track: TrackId) -> bool {
        if !self.clip_editable(id) {
            return false;
        }
        let Some(t) = self.track(track) else {
            return false;
        };
        !t.locked && t.kind == self.clip_kind(id)
    }

    /// The first unlocked track of `kind`, preferring `near` when it already
    /// qualifies — the "sensible track" a paste or a duplicate lands on.
    pub fn placement_track(&self, kind: TrackKind, near: Option<TrackId>) -> Option<TrackId> {
        if let Some(t) = near.and_then(|id| self.track(id))
            && t.kind == kind
            && !t.locked
        {
            return Some(t.id);
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
    ///
    /// Returns the delta it *actually* applied after clamping, in source
    /// seconds. A caller trimming several clips at once (the halves of a
    /// linked pair) has to hand the rest the amount the first one really
    /// moved, or a clip that clamped and a clip that did not would drift apart
    /// by exactly the difference — which is the desync a link exists to
    /// prevent.
    pub fn trim_in(&mut self, id: ClipId, delta: f64) -> f64 {
        let mut applied = 0.0;
        if let Some(c) = self.clip_mut(id) {
            let max_delta = (c.out_point - c.in_point) - MIN_CLIP; // can't cross out
            let min_delta = -c.in_point; // can't go before source 0
            let d = delta.clamp(min_delta, max_delta);
            applied = d;
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
            for m in &mut c.markers {
                m.t -= on_timeline;
            }
        }
        applied
    }

    /// Drag the right (out) handle by `delta` seconds. Clamped so the
    /// clip stays within the source length and keeps at least `MIN_CLIP`.
    /// Returns the delta actually applied, for the same reason
    /// [`Self::trim_in`] does.
    pub fn trim_out(&mut self, id: ClipId, delta: f64) -> f64 {
        let media_dur = self
            .clip(id)
            .and_then(|c| self.media(c.media))
            .map(|m| m.duration);
        let mut applied = 0.0;
        if let (Some(c), Some(media_dur)) = (self.clip_mut(id), media_dur) {
            let max_out = media_dur;
            let min_out = c.in_point + MIN_CLIP;
            let before = c.out_point;
            c.out_point = (c.out_point + delta).clamp(min_out, max_out);
            applied = c.out_point - before;
        }
        applied
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
        let Some(clip) = self.clip(id).cloned() else {
            return Vec::new();
        };
        let gap = clip.duration();
        let track = clip.track;
        let after = clip.start;
        self.clips.retain(|c| c.id != id);
        let mut moved = Vec::new();
        for c in &mut self.clips {
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
            if d <= tol && best.is_none_or(|(_, bd)| d < bd) {
                best = Some((c, d));
            }
        }
        best.map_or(t, |(c, _)| c)
    }

    /// Paste `clip` as a new independent clip starting at `start` on
    /// `track` (the clip keeps its trim window, filters and fades). Returns
    /// the new id. Used by copy/paste — the source clip is unchanged.
    ///
    /// Independent includes *unlinked*: carrying the source's link group over
    /// would tie the copy to the clip it was copied from, so dragging the
    /// original would drag the copy across the timeline. A caller pasting a
    /// whole linked pair re-links the copies to each other afterwards.
    pub fn paste_clip(&mut self, mut clip: Clip, track: TrackId, start: f64) -> ClipId {
        clip.id = tilo_resource::fresh_uuid();
        clip.track = track;
        clip.start = start.max(0.0);
        clip.link = None;
        let id = clip.id;
        self.clips.push(clip);
        id
    }
}

/// The id a clip, track, source or marker is stored and published under.
pub fn key(id: Uuid) -> Id {
    Id::from_raw(id.to_string())
}

/// A source in the bin sorts by its name.
impl Identified for MediaItem {
    fn id(&self) -> Id {
        key(self.id)
    }
    fn order(&self, other: &Self) -> std::cmp::Ordering {
        self.name.cmp(&other.name)
    }
}

/// An effect is where it was put in its clip's stack.
impl Identified for ClipEffect {
    fn id(&self) -> Id {
        key(self.id)
    }
}

/// A track sorts by the lane it occupies.
impl Identified for Track {
    fn id(&self) -> Id {
        key(self.id)
    }
    fn order(&self, other: &Self) -> std::cmp::Ordering {
        self.index
            .cmp(&other.index)
            .then_with(|| self.name.cmp(&other.name))
    }
}

/// A clip sorts by where it sits in time.
impl Identified for Clip {
    fn id(&self) -> Id {
        key(self.id)
    }
    fn order(&self, other: &Self) -> std::cmp::Ordering {
        self.start
            .total_cmp(&other.start)
            .then_with(|| self.in_point.total_cmp(&other.in_point))
    }
}

/// A timeline marker sorts by its time.
impl Identified for Marker {
    fn id(&self) -> Id {
        key(self.id)
    }
    fn order(&self, other: &Self) -> std::cmp::Ordering {
        self.t.total_cmp(&other.t)
    }
}

#[cfg(test)]
mod key_and_mask_tests {
    use super::*;
    use crate::param::{Interp, Param};

    const GREEN: [f64; 3] = [0.06, 0.72, 0.20];

    fn keyer() -> ChromaKey {
        ChromaKey {
            enabled: true,
            ..ChromaKey::default()
        }
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "merged scenario: each former test is a labelled case block with its own fixture"
    )]
    #[expect(
        clippy::float_cmp,
        reason = "the value is stored, clamped to a literal bound, or a fully-in/fully-out coverage of exactly 0 or 1"
    )]
    fn chroma_key_removes_the_key_hue_with_soft_edges_and_despill() {
        // Case `an_inactive_key_removes_nothing_at_all`.
        {
            let off = ChromaKey::default();
            assert!(!off.enabled);
            assert_eq!(
                off.coverage(GREEN),
                1.0,
                "a key that is off keeps its own colour"
            );
            // On, but wound to zero tolerance: still nothing, and it says so.
            let zero = ChromaKey {
                enabled: true,
                tolerance: 0.0,
                ..ChromaKey::default()
            };
            assert!(!zero.is_active());
            assert_eq!(zero.coverage(GREEN), 1.0);
        }
        // Case `the_key_colour_goes_and_an_unrelated_colour_stays`.
        {
            let k = keyer();
            assert_eq!(
                k.coverage(GREEN),
                0.0,
                "the colour being keyed is removed outright"
            );
            for keep in [[0.9, 0.5, 0.15], [0.8, 0.1, 0.7], [0.1, 0.2, 0.9]] {
                assert_eq!(k.coverage(keep), 1.0, "{keep:?} is nothing like the key");
            }
        }
        // Case `brightness_does_not_change_whether_a_colour_keys`:
        // The reason the key measures chroma and not RGB distance: one green
        // screen is never one green. A lit patch and a shadowed patch of the
        // same cloth are far apart in RGB and close together in hue, and both
        // have to key.
        {
            let k = keyer();
            for gain in [0.2_f64, 0.35, 0.6, 1.0, 1.35] {
                let shade = [
                    (GREEN[0] * gain).clamp(0.0, 1.0),
                    (GREEN[1] * gain).clamp(0.0, 1.0),
                    (GREEN[2] * gain).clamp(0.0, 1.0),
                ];
                assert_eq!(
                    k.coverage(shade),
                    0.0,
                    "green at {gain}× exposure is still the same green: {shade:?}",
                );
            }
            // And an RGB-distance keyer would have kept the dark one: prove the
            // shades really are far apart in plain RGB, so the invariance above
            // is the keyer's doing and not an accident of the numbers.
            let dark = [GREEN[0] * 0.2, GREEN[1] * 0.2, GREEN[2] * 0.2];
            let rgb_gap: f64 = GREEN
                .iter()
                .zip(&dark)
                .map(|(a, b)| (a - b).powi(2))
                .sum::<f64>()
                .sqrt();
            assert!(
                rgb_gap > 0.3,
                "the two shades are {rgb_gap:.2} apart in RGB"
            );
        }
        // Case `near_black_falls_out_of_the_key_instead_of_keying_on_noise`:
        // The one place brightness *does* decide the matte: near black there
        // is no reliable hue left, so those pixels are kept rather than having
        // their noise amplified into the key.
        {
            let k = keyer();
            assert_eq!(
                k.coverage([0.0, 0.0, 0.0]),
                1.0,
                "black is not a colour to key on"
            );
            let almost = [GREEN[0] * 0.02, GREEN[1] * 0.02, GREEN[2] * 0.02];
            assert!(
                k.coverage(almost) > 0.0,
                "a near-black pixel is not confidently green: {almost:?}",
            );
        }
        // Case `softness_gives_the_edge_partial_coverage_rather_than_a_step`.
        {
            // A green diluted halfway toward grey: near the key but not on it.
            let edge = [
                GREEN[0].midpoint(0.5),
                GREEN[1].midpoint(0.5),
                GREEN[2].midpoint(0.5),
            ];
            // Place the band around wherever that colour actually falls, read
            // off the keyer itself rather than recomputed here — a test that
            // re-derives the distance would pass while the keyer was wrong.
            let d = keyer().distance(edge);
            assert!(d > 0.05, "the diluted green is off the key: {d:.3}");
            let hard = ChromaKey {
                tolerance: d * 0.5,
                softness: 0.0,
                ..keyer()
            };
            let soft = ChromaKey {
                tolerance: d * 0.5,
                softness: d,
                ..keyer()
            };
            assert_eq!(
                hard.coverage(edge),
                1.0,
                "with no softness the edge is a step"
            );
            let partial = soft.coverage(edge);
            assert!(
                partial > 0.0 && partial < 1.0,
                "softness must give the edge a partial matte, got {partial}",
            );
            // And the far side of the soft band is still fully kept.
            assert_eq!(
                soft.coverage([0.5, 0.5, 0.5]),
                1.0,
                "neutral grey is nowhere near"
            );
        }
        // Case `despill_pulls_the_key_hue_out_without_moving_brightness`.
        {
            let k = ChromaKey {
                spill: 1.0,
                ..keyer()
            };
            // A skin-ish tone with a wash of the key green over it.
            let tinted = [0.62, 0.58, 0.36];
            let clean = k.despilled(tinted);
            let luma = |c: [f64; 3]| 0.2126 * c[0] + 0.7152 * c[1] + 0.0722 * c[2];
            assert!(
                (luma(clean) - luma(tinted)).abs() < 0.02,
                "brightness is left alone: {:.3} → {:.3}",
                luma(tinted),
                luma(clean),
            );
            let toward_key = |c: [f64; 3]| {
                let (kb, kr) = ChromaKey::chroma(k.color);
                let n = (kb * kb + kr * kr).sqrt();
                let (pb, pr) = ChromaKey::chroma(c);
                (pb * kb + pr * kr) / n
            };
            assert!(
                toward_key(clean) < toward_key(tinted) - 1e-3,
                "the key hue is reduced: {:.4} → {:.4}",
                toward_key(tinted),
                toward_key(clean),
            );
        }
        // Case `despill_leaves_a_colour_leaning_away_from_the_key_untouched`.
        {
            let k = ChromaKey {
                spill: 1.0,
                ..keyer()
            };
            // Magenta is the opposite side of the chroma plane from green.
            let magenta = [0.7, 0.1, 0.6];
            assert_eq!(k.despilled(magenta), magenta);
        }
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "merged scenario: each former test is a labelled case block with its own fixture"
    )]
    #[expect(
        clippy::float_cmp,
        reason = "the value is stored, clamped to a literal bound, or a fully-in/fully-out coverage of exactly 0 or 1"
    )]
    fn masks_cover_shape_feather_invert_move_and_rotate() {
        // Case `a_mask_that_is_off_covers_everything`.
        {
            let m = Mask::default();
            assert!(!m.is_active());
            for (u, v) in [(0.0, 0.0), (0.5, 0.5), (1.0, 1.0)] {
                assert_eq!(m.coverage(u, v, 16.0 / 9.0), 1.0);
            }
        }
        // Case `an_ellipse_keeps_its_centre_and_drops_its_corners`.
        {
            let m = Mask {
                shape: MaskShape::Ellipse,
                w: 0.5,
                h: 0.5,
                feather: 0.1,
                ..Mask::default()
            };
            assert!(
                (m.coverage(0.5, 0.5, 1.0) - 1.0).abs() < 1e-9,
                "dead centre is kept"
            );
            assert_eq!(
                m.coverage(0.02, 0.02, 1.0),
                0.0,
                "a frame corner is outside it"
            );
            assert_eq!(m.coverage(0.98, 0.5, 1.0), 0.0, "so is the far edge");
        }
        // Case `a_rect_keeps_the_corners_an_ellipse_of_the_same_size_drops`:
        // A rect and an ellipse of the same extent must not be the same mask —
        // they differ exactly at the corners of the box, which is the only
        // place the shape choice shows.
        {
            let base = Mask {
                w: 0.8,
                h: 0.8,
                feather: 0.02,
                ..Mask::default()
            };
            let rect = Mask {
                shape: MaskShape::Rect,
                ..base
            };
            let ellipse = Mask {
                shape: MaskShape::Ellipse,
                ..base
            };
            // Just inside the box's corner: inside the rect, outside the ellipse
            // (the ellipse's radius there is sqrt(2) of its half-extent).
            let (u, v) = (0.5 + 0.36, 0.5 + 0.36);
            assert!(rect.coverage(u, v, 1.0) > 0.9, "the rect keeps its corner");
            assert_eq!(ellipse.coverage(u, v, 1.0), 0.0, "the ellipse does not");
        }
        // Case `feather_ramps_the_edge_instead_of_cutting_it`.
        {
            let sharp = Mask {
                shape: MaskShape::Ellipse,
                w: 0.6,
                h: 0.6,
                feather: 0.001,
                ..Mask::default()
            };
            let soft = Mask {
                feather: 0.6,
                ..sharp
            };
            // Two thirds of the way out along the radius.
            let (u, v) = (0.5 + 0.3 * 0.8, 0.5);
            assert!(
                (sharp.coverage(u, v, 1.0) - 1.0).abs() < 1e-6,
                "unfeathered, still fully inside"
            );
            let ramped = soft.coverage(u, v, 1.0);
            assert!(
                ramped > 0.0 && ramped < 1.0,
                "feathered, the same point is part-way through the edge: {ramped}",
            );
        }
        // Case `inverting_a_mask_swaps_what_it_keeps`.
        {
            let m = Mask {
                shape: MaskShape::Ellipse,
                w: 0.5,
                h: 0.5,
                feather: 0.1,
                ..Mask::default()
            };
            let holed = Mask { invert: true, ..m };
            assert!((m.coverage(0.5, 0.5, 1.0) + holed.coverage(0.5, 0.5, 1.0) - 1.0).abs() < 1e-9);
            assert_eq!(
                holed.coverage(0.5, 0.5, 1.0),
                0.0,
                "the centre is now the hole"
            );
            assert_eq!(
                holed.coverage(0.02, 0.02, 1.0),
                1.0,
                "and the corner is kept"
            );
        }
        // Case `moving_a_mask_moves_what_it_keeps`.
        {
            let centred = Mask {
                shape: MaskShape::Ellipse,
                w: 0.3,
                h: 0.3,
                ..Mask::default()
            };
            let right = Mask { x: 0.3, ..centred };
            assert_eq!(
                right.coverage(0.5, 0.5, 1.0),
                0.0,
                "the centre is no longer inside"
            );
            assert!(
                right.coverage(0.8, 0.5, 1.0) > 0.9,
                "but the point it moved to is"
            );
        }
        // Case `a_rotated_rect_turns_rather_than_shearing`:
        // A rotated rect has to actually rotate. On a wide frame that only
        // works if the rotation happens in aspect-corrected space — otherwise
        // the shape shears and a 90° turn is not the same rect stood on end.
        {
            let fa = 16.0 / 9.0;
            let tall = Mask {
                shape: MaskShape::Rect,
                w: 0.12,
                h: 0.8,
                feather: 0.02,
                ..Mask::default()
            };
            let turned = Mask {
                rotation: 90.0,
                ..tall
            };
            // A point out along the frame's x axis: outside the upright sliver,
            // inside it once the sliver is laid on its side. The distance is
            // scaled by the frame aspect so it matches the shape's own units.
            let u = 0.5 + 0.3 / fa;
            assert_eq!(
                tall.coverage(u, 0.5, fa),
                0.0,
                "outside the narrow upright bar"
            );
            assert!(
                turned.coverage(u, 0.5, fa) > 0.9,
                "inside it once turned on its side"
            );
            // And the ends of the upright bar are now outside it.
            assert_eq!(turned.coverage(0.5, 0.5 + 0.3, fa), 0.0);
        }
        // Case `a_mask_fitted_over_a_pip_lands_on_the_pip`.
        {
            // A quarter-size picture tucked into the top-right corner.
            let t = Transform::pip(0.3, (1.0, -1.0), 0.04);
            let m = Mask::over(MaskShape::Ellipse, &t, 16.0 / 9.0, 16.0 / 9.0);
            assert_eq!(m.shape, MaskShape::Ellipse);
            assert!(
                (m.x - t.pos_x).abs() < 1e-9,
                "centred on where the picture went"
            );
            assert!((m.y - t.pos_y).abs() < 1e-9);
            assert!((m.w - 0.3).abs() < 1e-6, "and sized to it: {}", m.w);
            assert!((m.h - 0.3).abs() < 1e-6);
            // The middle of the inset picture is inside the fitted mask, and the
            // middle of the frame — where a default mask would have gone — is not.
            assert!(m.coverage(0.5 + m.x, 0.5 + m.y, 16.0 / 9.0) > 0.9);
            assert_eq!(m.coverage(0.5, 0.5, 16.0 / 9.0), 0.0);
        }
        // Case `fitting_over_a_narrow_source_follows_the_aspect_fit`:
        // A source narrower than the frame is letterboxed at the identity
        // transform, so a mask fitted over it must be narrower than the frame
        // too — not the full width the frame would suggest.
        {
            let m = Mask::over(MaskShape::Rect, &Transform::default(), 1.0, 16.0 / 9.0);
            assert!(
                (m.w - 9.0 / 16.0).abs() < 1e-6,
                "pillarboxed to the source's aspect: {}",
                m.w
            );
            assert!((m.h - 1.0).abs() < 1e-6, "and full height");
        }
    }

    fn keyed_clip() -> Clip {
        Clip {
            id: Uuid::new_v4(),
            start: 2.0,
            in_point: 0.0,
            out_point: 6.0,
            key: ChromaKey {
                enabled: true,
                tolerance: 0.2,
                softness: 0.1,
                spill: 0.4,
                ..ChromaKey::default()
            },
            mask: Mask {
                shape: MaskShape::Ellipse,
                ..Mask::default()
            },
            ..Default::default()
        }
    }

    #[test]
    #[expect(
        clippy::float_cmp,
        reason = "the value is stored, clamped to a literal bound, or a fully-in/fully-out coverage of exactly 0 or 1"
    )]
    fn key_and_mask_parameters_animate_through_their_named_fields() {
        // Case `key_and_mask_read_their_constants_when_nothing_is_animated`.
        {
            let c = keyed_clip();
            assert_eq!(c.key_at(3.0), c.key);
            assert_eq!(c.mask_at(3.0), c.mask);
        }
        // Case `keying_a_mask_makes_it_travel_with_the_playhead`:
        // The whole reason these are parameters: a mask that cannot move
        // cannot follow a subject, and a key that cannot ramp cannot be
        // eased in over a shot.
        {
            let mut c = keyed_clip();
            // Clip-local 0 and 4 — the clip starts at timeline 2.0.
            c.set_key_at(Param::MaskX, 2.0, -0.4, Interp::Linear);
            c.set_key_at(Param::MaskX, 6.0, 0.4, Interp::Linear);
            assert!(
                (c.mask_at(2.0).x + 0.4).abs() < 1e-9,
                "at the head it is left"
            );
            assert!(c.mask_at(4.0).x.abs() < 1e-9, "half way it is centred");
            assert!(
                (c.mask_at(6.0).x - 0.4).abs() < 1e-9,
                "at the tail it is right"
            );
            // The shape and the invert flag are not animatable and must survive.
            assert_eq!(c.mask_at(4.0).shape, MaskShape::Ellipse);
            // And nothing else on the mask drifted.
            assert!((c.mask_at(4.0).w - c.mask.w).abs() < 1e-9);
        }
        // Case `keying_the_tolerance_ramps_the_matte_open`.
        {
            let mut c = keyed_clip();
            c.set_key_at(Param::KeyTolerance, 2.0, 0.0, Interp::Linear);
            c.set_key_at(Param::KeyTolerance, 6.0, 0.5, Interp::Linear);
            assert!(
                !c.key_at(2.0).is_active(),
                "at the head the key removes nothing"
            );
            assert!((c.key_at(4.0).tolerance - 0.25).abs() < 1e-9);
            assert!(c.key_at(6.0).is_active(), "by the tail it is keying");
            // The key colour is not a curve and must come through untouched.
            assert_eq!(c.key_at(4.0).color, c.key.color);
            assert!(c.key_at(4.0).enabled);
        }
        // Case `every_new_parameter_reads_and_writes_the_field_it_names`.
        {
            let mut c = keyed_clip();
            for (p, v) in [
                (Param::KeyTolerance, 0.31),
                (Param::KeySoftness, 0.17),
                (Param::KeySpill, 0.62),
                (Param::MaskX, -0.22),
                (Param::MaskY, 0.44),
                (Param::MaskW, 0.71),
                (Param::MaskH, 1.3),
                (Param::MaskRotation, 37.0),
                (Param::MaskFeather, 0.09),
            ] {
                c.set_param_const(p, v);
                assert_eq!(
                    c.param_const(p),
                    Some(v),
                    "{p:?} round-trips through its field"
                );
                assert!(
                    (c.param_at(p, 5.0) - v).abs() < 1e-12,
                    "{p:?} reads back at the playhead"
                );
            }
            // The writes landed on the right structures, not on each other.
            assert!((c.key.tolerance - 0.31).abs() < 1e-12);
            assert!((c.mask.rotation - 37.0).abs() < 1e-12);
        }
    }
}

#[cfg(test)]
mod tests {

    use crate::param::{Interp, Param};

    /// A one-clip project: a 20s source placed whole at t=0.
    fn speed_fixture() -> (Project, ClipId) {
        let mut p = Project::new("s");
        let mid = Uuid::new_v4();
        p.media.push(MediaItem {
            id: mid,
            name: "v.mp4".into(),
            kind: MediaKind::Video,
            duration: 20.0,
            width: 1920,
            height: 1080,
            fps: 30.0,
            ..Default::default()
        });
        let tid = Uuid::new_v4();
        p.tracks.push(Track {
            id: tid,
            name: "V1".into(),
            index: 1,
            ..Default::default()
        });
        let cid = p.add_clip(tid, mid, 0.0).expect("media exists");
        (p, cid)
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "merged scenario: each former test is a labelled case block with its own fixture"
    )]
    #[expect(
        clippy::float_cmp,
        reason = "the value is stored, clamped to a literal bound, or a fully-in/fully-out coverage of exactly 0 or 1"
    )]
    fn clip_speed_maps_source_time_and_survives_splits_and_trims() {
        // Case `speed_divides_the_timeline_length_and_leaves_the_source_window_alone`.
        {
            let (mut p, cid) = speed_fixture();
            let c = p.clip_mut(cid).unwrap();
            assert!(
                ((c.duration()) - (20.0)).abs() < 1e-9,
                "normal speed is the source window"
            );
            assert!(
                ((c.set_speed(2.0, false)) - (10.0)).abs() < 1e-9,
                "2x halves the timeline length"
            );
            assert!(
                ((c.source_span()) - (20.0)).abs() < 1e-9,
                "but the source window is untouched"
            );
            assert!(
                ((c.set_speed(0.5, false)) - (40.0)).abs() < 1e-9,
                "and half speed doubles it"
            );
            assert!(
                ((c.end()) - (40.0)).abs() < 1e-9,
                "which moves the clip's right edge"
            );
        }
        // Case `the_source_time_map_follows_the_rate_and_the_direction`.
        {
            let (mut p, cid) = speed_fixture();
            {
                let c = p.clip_mut(cid).unwrap();
                c.in_point = 2.0;
                c.out_point = 12.0;
            }
            let at = |p: &Project, t: f64| p.clip(cid).unwrap().source_time(t);
            assert!(
                ((at(&p, 0.0)) - (2.0)).abs() < 1e-9,
                "the head shows the in-point"
            );
            assert!(
                ((at(&p, 3.0)) - (5.0)).abs() < 1e-9,
                "and 3s in is 3s of source"
            );

            p.clip_mut(cid).unwrap().set_speed(2.0, false);
            assert!(
                ((at(&p, 0.0)) - (2.0)).abs() < 1e-9,
                "the head is still the in-point"
            );
            assert!(
                ((at(&p, 3.0)) - (8.0)).abs() < 1e-9,
                "but at 2x, 3s in is 6s of source"
            );

            p.clip_mut(cid).unwrap().set_speed(1.0, true);
            assert!(
                ((at(&p, 0.0)) - (12.0)).abs() < 1e-9,
                "reversed, the head shows the out-point"
            );
            assert!(
                ((at(&p, 3.0)) - (9.0)).abs() < 1e-9,
                "and it walks back through the source"
            );
            assert!(
                ((at(&p, 100.0)) - (2.0)).abs() < 1e-9,
                "held at the in-point rather than running past it"
            );
        }
        // Case `a_clip_never_reads_outside_its_own_window`.
        {
            let (mut p, cid) = speed_fixture();
            {
                let c = p.clip_mut(cid).unwrap();
                c.in_point = 4.0;
                c.out_point = 6.0;
                c.set_speed(4.0, false);
            }
            let c = p.clip(cid).unwrap();
            assert!(
                ((c.duration()) - (0.5)).abs() < 1e-9,
                "a 2s window at 4x is half a second"
            );
            assert!(
                ((c.source_time(0.49)) - (5.96)).abs() < 1e-9,
                "still inside the window"
            );
            assert!(
                ((c.source_time(9.0)) - (6.0)).abs() < 1e-9,
                "and past the end it holds the out-point"
            );
        }
        // Case `splitting_a_sped_up_clip_cuts_the_right_source_frame`.
        {
            let (mut p, cid) = speed_fixture();
            {
                let c = p.clip_mut(cid).unwrap();
                c.out_point = 12.0;
                c.set_speed(3.0, false);
            }
            // 12s of source at 3x is 4s of timeline; split at 1s in.
            assert!(
                ((p.clip(cid).unwrap().duration()) - (4.0)).abs() < 1e-9,
                "p.clip(cid).unwrap().duration() is {}, want {}",
                p.clip(cid).unwrap().duration(),
                4.0
            );
            let right = p.split_clip(cid, 1.0).expect("inside the clip");
            let left = p.clip(cid).unwrap();
            let right = p.clip(right).unwrap();
            assert!(
                ((left.out_point) - (3.0)).abs() < 1e-9,
                "one timeline second at 3x is three source seconds"
            );
            assert!(
                ((right.in_point) - (3.0)).abs() < 1e-9,
                "and the tail picks up exactly there"
            );
            assert!(
                ((right.start) - (1.0)).abs() < 1e-9,
                "right.start is {}, want {}",
                right.start,
                1.0
            );
            assert!(
                (left.duration() + right.duration() - 4.0).abs() < 1e-9,
                "the halves still add up to the original length",
            );
        }
        // Case `splitting_a_reversed_clip_cuts_from_the_other_end`.
        {
            let (mut p, cid) = speed_fixture();
            {
                let c = p.clip_mut(cid).unwrap();
                c.in_point = 0.0;
                c.out_point = 8.0;
                c.set_speed(1.0, true);
            }
            // Reversed, the timeline runs 8 → 0 through the source.
            let right = p.split_clip(cid, 2.0).expect("inside the clip");
            let left = p.clip(cid).unwrap();
            let right = p.clip(right).unwrap();
            assert!(
                ((left.in_point) - (6.0)).abs() < 1e-9,
                "the head half plays source 8 → 6"
            );
            assert!(
                ((left.out_point) - (8.0)).abs() < 1e-9,
                "left.out_point is {}, want {}",
                left.out_point,
                8.0
            );
            assert!(
                ((right.out_point) - (6.0)).abs() < 1e-9,
                "and the tail picks up at 6 going down"
            );
            assert!(
                ((right.in_point) - (0.0)).abs() < 1e-9,
                "right.in_point is {}, want {}",
                right.in_point,
                0.0
            );
        }
        // Case `trimming_a_sped_up_clip_moves_the_start_by_timeline_seconds`.
        {
            let (mut p, cid) = speed_fixture();
            {
                let c = p.clip_mut(cid).unwrap();
                c.start = 10.0;
                c.in_point = 0.0;
                c.out_point = 8.0;
                c.set_speed(2.0, false);
            }
            // Pull the in-point 2 source-seconds later. At 2x that is one
            // timeline second, so the clip's left edge moves by one, not two.
            p.trim_in(cid, 2.0);
            let c = p.clip(cid).unwrap();
            assert!(
                ((c.in_point) - (2.0)).abs() < 1e-9,
                "c.in_point is {}, want {}",
                c.in_point,
                2.0
            );
            assert!(
                ((c.start) - (11.0)).abs() < 1e-9,
                "the left edge moved by the timeline seconds trimmed away"
            );
            assert!(((c.duration()) - (3.0)).abs() < 1e-9, "6s of source at 2x");
        }
        // Case `speed_is_clamped_to_something_playable_and_labels_itself`.
        {
            assert_eq!(Speed::new(0.0, false).rate(), MIN_SPEED);
            assert_eq!(Speed::new(1e9, false).rate(), MAX_SPEED);
            assert_eq!(Speed::new(2.0, false).label(), "2×");
            assert_eq!(Speed::new(0.5, true).label(), "0.50× rev");
            assert!(Speed::default().is_normal());
            assert_eq!(Speed::new(2.0, true).signed(), -2.0);
        }
    }

    // --- keyframes --------------------------------------------------------

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "merged scenario: each former test is a labelled case block with its own fixture"
    )]
    #[expect(
        clippy::float_cmp,
        reason = "the value is stored, clamped to a literal bound, or a fully-in/fully-out coverage of exactly 0 or 1"
    )]
    fn animated_parameters_are_clip_relative_and_read_through_one_door() {
        // Case `a_parameter_reads_its_constant_until_it_is_keyed`.
        {
            let (mut p, cid) = speed_fixture();
            p.clip_mut(cid).unwrap().opacity = 0.4;
            assert_eq!(
                p.clip(cid).unwrap().param_at(Param::Opacity, 3.0),
                0.4,
                "with no curve, the field is the value at every instant",
            );
            {
                let c = p.clip_mut(cid).unwrap();
                c.set_key_at(Param::Opacity, 0.0, 0.0, Interp::Linear);
                c.set_key_at(Param::Opacity, 10.0, 1.0, Interp::Linear);
            }
            let c = p.clip(cid).unwrap();
            assert_eq!(c.param_at(Param::Opacity, 0.0), 0.0, "now the curve wins");
            assert!((c.param_at(Param::Opacity, 5.0) - 0.5).abs() < 1e-9);
            assert_eq!(c.opacity, 0.4, "and the constant is still there underneath");
        }
        // Case `keyframe_times_are_relative_to_the_clip_so_moving_it_carries_them`.
        {
            let (mut p, cid) = speed_fixture();
            {
                let c = p.clip_mut(cid).unwrap();
                c.set_key_at(Param::Opacity, 0.0, 0.0, Interp::Linear);
                c.set_key_at(Param::Opacity, 4.0, 1.0, Interp::Linear);
            }
            let before = p.clip(cid).unwrap().param_at(Param::Opacity, 2.0);
            p.move_clip(cid, 30.0);
            let after = p.clip(cid).unwrap().param_at(Param::Opacity, 32.0);
            assert!(
                ((before) - (after)).abs() < 1e-9,
                "the same point in the clip has the same value"
            );
            assert!((before - 0.5).abs() < 1e-9);
        }
        // Case `splitting_preserves_the_animated_value_on_both_sides_of_the_cut`.
        {
            let (mut p, cid) = speed_fixture();
            {
                let c = p.clip_mut(cid).unwrap();
                c.out_point = 8.0;
                c.set_key_at(Param::Opacity, 0.0, 0.0, Interp::Linear);
                c.set_key_at(Param::Opacity, 8.0, 1.0, Interp::Linear);
            }
            let at_cut = p.clip(cid).unwrap().param_at(Param::Opacity, 3.0);
            let right = p.split_clip(cid, 3.0).expect("inside");
            assert!(
                (p.clip(cid).unwrap().param_at(Param::Opacity, 3.0) - at_cut).abs() < 1e-9,
                "the head half ends on the value it had at the cut",
            );
            assert!(
                (p.clip(right).unwrap().param_at(Param::Opacity, 3.0) - at_cut).abs() < 1e-9,
                "and the tail half starts on it",
            );
            // And each half still animates rather than freezing.
            let tail = p.clip(right).unwrap();
            assert!(
                tail.param_at(Param::Opacity, 7.0) > at_cut,
                "the tail keeps rising"
            );
        }
        // Case `the_grade_and_the_transform_read_through_the_same_door`.
        {
            let (mut p, cid) = speed_fixture();
            {
                let c = p.clip_mut(cid).unwrap();
                c.filters.brightness = 0.3;
                c.transform.scale_x = 0.8;
                c.set_key_at(Param::Brightness, 0.0, 0.1, Interp::Linear);
                c.set_key_at(Param::Brightness, 4.0, 0.9, Interp::Linear);
                c.set_key_at(Param::ScaleX, 0.0, 1.0, Interp::Hold);
                c.set_key_at(Param::ScaleX, 4.0, 2.0, Interp::Linear);
            }
            let c = p.clip(cid).unwrap();
            assert!(
                (c.filters_at(2.0).brightness - 0.5).abs() < 1e-9,
                "the graded axis animates"
            );
            assert_eq!(
                c.filters_at(2.0).contrast,
                0.5,
                "an unkeyed axis keeps its constant",
            );
            assert_eq!(c.transform_at(2.0).scale_x, 1.0, "a held key does not ramp");
            assert_eq!(c.transform_at(4.0).scale_x, 2.0);
            assert_eq!(
                c.transform_at(2.0).scale_y,
                1.0,
                "an unkeyed channel is its constant"
            );
        }
        // Case `an_effects_intensity_can_be_keyed_and_a_missing_effect_is_ignored`.
        {
            let (mut p, cid) = speed_fixture();
            {
                let c = p.clip_mut(cid).unwrap();
                c.add_effect(EffectKind::Vignette);
                c.set_key_at(Param::EffectAmount(0), 0.0, 0.0, Interp::Linear);
                c.set_key_at(Param::EffectAmount(0), 4.0, 1.0, Interp::Linear);
                // A curve on an effect the stack does not have.
                c.set_key_at(Param::EffectAmount(7), 0.0, 0.5, Interp::Linear);
            }
            let c = p.clip(cid).unwrap();
            let mid = c.effects_at(2.0);
            assert_eq!(mid.len(), 1, "the orphan curve adds no effect");
            assert!(
                (mid[0].amount - 0.5).abs() < 1e-9,
                "the real one animates: {mid:?}"
            );
            assert_eq!(
                c.param_const(Param::EffectAmount(7)),
                None,
                "and has no constant either"
            );
        }
        // Case `a_key_control_can_tell_its_three_states_apart`.
        {
            let (mut p, cid) = speed_fixture();
            let c = p.clip_mut(cid).unwrap();
            // Nothing keyed: not animated anywhere.
            assert!(!c.anim.is_animated(Param::Opacity));
            assert!(!c.has_key_at(Param::Opacity, 2.0));
            // Keyed at 2s: animated, and the key is *on* 2s.
            c.set_key_at(Param::Opacity, 2.0, 0.4, Interp::Linear);
            assert!(c.anim.is_animated(Param::Opacity));
            assert!(c.has_key_at(Param::Opacity, 2.0));
            // 5s is animated but between keys — the third, distinct state.
            assert!(!c.has_key_at(Param::Opacity, 5.0));
            // Half a frame away still counts as on the key; a whole frame does not.
            assert!(c.has_key_at(Param::Opacity, 2.0 + KEY_TOL * 0.9));
            assert!(!c.has_key_at(Param::Opacity, 2.0 + 0.1));
        }
        // Case `key_times_are_absolute_deduplicated_and_navigable`.
        {
            let (mut p, cid) = speed_fixture();
            let c = p.clip_mut(cid).unwrap();
            c.start = 10.0;
            // Two parameters keyed at the same instant are one instant.
            c.set_key_at(Param::Opacity, 12.0, 0.5, Interp::Linear);
            c.set_key_at(Param::Brightness, 12.0, 0.7, Interp::Linear);
            c.set_key_at(Param::Opacity, 16.0, 1.0, Interp::Linear);
            assert_eq!(c.key_times(), vec![12.0, 16.0], "absolute, sorted, deduped");
            assert_eq!(c.next_key_after(10.0), Some(12.0));
            assert_eq!(c.next_key_after(12.0), Some(16.0));
            assert_eq!(
                c.next_key_after(16.0),
                None,
                "navigation stops, never wraps"
            );
            assert_eq!(c.prev_key_before(16.0), Some(12.0));
            assert_eq!(c.prev_key_before(12.0), None);
        }
        // Case `interpolation_is_read_and_rewritten_at_the_playhead`.
        {
            let (mut p, cid) = speed_fixture();
            let c = p.clip_mut(cid).unwrap();
            c.set_key_at(Param::PosX, 3.0, 0.25, Interp::Linear);
            assert_eq!(c.key_interp_at(Param::PosX, 3.0), Some(Interp::Linear));
            assert_eq!(c.key_interp_at(Param::PosX, 9.0), None, "no key, no shape");
            assert!(c.set_interp_at(Param::PosX, 3.0, Interp::Ease));
            assert_eq!(c.key_interp_at(Param::PosX, 3.0), Some(Interp::Ease));
            assert!(
                !c.set_interp_at(Param::PosX, 9.0, Interp::Hold),
                "nothing there to re-shape"
            );
            // Re-shaping must not disturb the value the key holds.
            assert!(
                ((c.param_at(Param::PosX, 3.0)) - (0.25)).abs() < 1e-9,
                "c.param_at(Param::PosX, 3.0) is {}, want {}",
                c.param_at(Param::PosX, 3.0),
                0.25
            );
        }
    }

    // --- markers ----------------------------------------------------------

    #[test]
    fn markers_merge_navigate_and_split_with_their_clips() {
        // Case `markers_live_on_the_project_and_on_clips_and_merge_onto_one_timeline`.
        {
            let (mut p, cid) = speed_fixture();
            p.clip_mut(cid).unwrap().start = 10.0;
            p.add_marker(5.0, "intro", 1);
            p.add_marker(1.0, "slate", 2);
            assert_eq!(
                p.markers
                    .iter()
                    .map(|m| m.name.as_str())
                    .collect::<Vec<_>>(),
                vec!["slate", "intro"],
                "project markers are kept in time order",
            );
            // A clip marker is stored clip-local but reads back absolute.
            p.clip_mut(cid)
                .unwrap()
                .add_marker(13.0, "beat", 3)
                .expect("inside the clip");
            assert!(
                ((p.clip(cid).unwrap().markers[0].t) - (3.0)).abs() < 1e-9,
                "stored relative to the clip"
            );
            let all = p.all_markers();
            let times: Vec<f64> = all.iter().map(|(t, _, _)| *t).collect();
            assert_eq!(
                times,
                vec![1.0, 5.0, 13.0],
                "and merges onto one absolute timeline"
            );
            assert_eq!(all[2].2, Some(cid), "carrying which clip it belongs to");
        }
        // Case `a_clip_marker_outside_the_clip_is_refused_rather_than_clamped`.
        {
            let (mut p, cid) = speed_fixture();
            assert!(
                p.clip_mut(cid)
                    .unwrap()
                    .add_marker(999.0, "nope", 0)
                    .is_none(),
                "a marker past the clip is not silently pinned to its end",
            );
            assert!(p.clip(cid).unwrap().markers.is_empty());
        }
        // Case `next_and_previous_marker_do_not_wrap`.
        {
            let mut p = Project::new("m");
            p.add_marker(2.0, "a", 0);
            p.add_marker(8.0, "b", 0);
            assert_eq!(p.next_marker(0.0).map(|m| m.t), Some(2.0));
            assert_eq!(p.next_marker(2.0).map(|m| m.t), Some(8.0), "strictly after");
            assert_eq!(p.next_marker(8.0).map(|m| m.t), None, "no wrap at the end");
            assert_eq!(p.prev_marker(8.0).map(|m| m.t), Some(2.0));
            assert_eq!(p.prev_marker(0.0).map(|m| m.t), None, "nor at the start");
        }
        // Case `a_split_hands_each_half_the_markers_inside_it`.
        {
            let (mut p, cid) = speed_fixture();
            p.clip_mut(cid).unwrap().out_point = 10.0;
            for (t, name) in [(2.0, "early"), (7.0, "late")] {
                p.clip_mut(cid)
                    .unwrap()
                    .add_marker(t, name, 0)
                    .expect("inside");
            }
            let right = p.split_clip(cid, 5.0).expect("inside");
            assert_eq!(
                p.clip(cid)
                    .unwrap()
                    .markers
                    .iter()
                    .map(|m| m.name.as_str())
                    .collect::<Vec<_>>(),
                vec!["early"],
            );
            let tail = p.clip(right).unwrap();
            assert_eq!(tail.markers.len(), 1);
            assert_eq!(tail.markers[0].name, "late");
            assert!(
                ((tail.markers[0].t) - (2.0)).abs() < 1e-9,
                "rebased onto the tail's own start"
            );
        }
        // Case `marker_navigation_crosses_project_and_clip_markers`.
        {
            let (mut p, cid) = speed_fixture();
            p.add_marker(4.0, "act two", 1);
            {
                let c = p.clip_mut(cid).unwrap();
                c.start = 0.0;
                c.add_marker(9.0, "beat", 2);
            }
            // The clip marker is stored clip-local but navigates absolutely.
            assert_eq!(p.next_marker_at(0.0), Some(4.0));
            assert_eq!(
                p.next_marker_at(4.0),
                Some(9.0),
                "a clip marker is a destination too"
            );
            assert_eq!(p.next_marker_at(9.0), None);
            assert_eq!(p.prev_marker_at(9.0), Some(4.0));
            assert_eq!(p.prev_marker_at(4.0), None);
        }
    }

    // --- transform --------------------------------------------------------

    #[test]
    fn transforms_and_filters_default_to_the_identity() {
        // Case `the_default_transform_is_the_identity_and_a_pip_is_not`.
        {
            let t = Transform::default();
            assert!(t.is_identity());
            assert!(!t.is_cropped());
            assert_eq!(t.crop_rect(), (0.0, 0.0, 1.0, 1.0), "nothing cropped away");
            let pip = Transform::pip(0.25, (1.0, -1.0), 0.05);
            assert!(!pip.is_identity());
            assert!(pip.pos_x > 0.0 && pip.pos_y < 0.0, "top-right: {pip:?}");
            assert!((pip.scale_x - 0.25).abs() < 1e-9);
        }
        // Case `a_crop_that_would_meet_itself_still_leaves_a_sliver`.
        {
            let t = Transform {
                crop_left: 0.8,
                crop_right: 0.8,
                ..Transform::default()
            };
            let (l, _, r, _) = t.crop_rect();
            assert!(
                r > l,
                "a fully cropped rect would make the inside-test meaningless: {l}..{r}"
            );
        }
        // Case `filters_default_is_identity`.
        {
            assert!(Filters::default().is_identity());
            assert!(
                (Filters::gain(0.5) - 1.0).abs() < 1e-9,
                "0.5 slider = unity gain"
            );
            assert!(Filters::gain(0.0) < 1e-9);
            assert!((Filters::gain(1.0) - 2.0).abs() < 1e-9);
        }
    }

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
            ..Default::default()
        };
        let mid = m.id;
        p.media.push(m);
        let t = Track {
            id: Uuid::new_v4(),
            name: "V1".into(),
            kind: TrackKind::Video,
            index: 1,
            muted: false,
            locked: false,
            solo: false,
            gain_db: 0.0,
            pan: 0.0,
        };
        let tid = t.id;
        p.tracks.push(t);
        let cid = p.add_clip(tid, mid, 0.0).unwrap();
        (p, mid, tid, cid)
    }

    #[test]
    #[expect(
        clippy::float_cmp,
        reason = "the value is stored, clamped to a literal bound, or a fully-in/fully-out coverage of exactly 0 or 1"
    )]
    fn clip_add_split_and_trim_respect_the_source_window() {
        // Case `add_clip_uses_full_source_length`.
        {
            let (p, _m, _t, cid) = project_with_clip();
            let c = p.clip(cid).unwrap();
            assert_eq!(c.in_point, 0.0);
            assert_eq!(c.out_point, 20.0);
            assert_eq!(c.duration(), 20.0);
            assert_eq!(c.end(), 20.0);
        }
        // Case `split_produces_two_contiguous_clips`.
        {
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
        // Case `split_outside_span_is_rejected`.
        {
            let (mut p, _m, _t, cid) = project_with_clip();
            assert!(p.split_clip(cid, 0.0).is_none());
            assert!(p.split_clip(cid, 25.0).is_none());
            assert_eq!(p.clips.len(), 1);
        }
        // Case `trim_in_moves_start_and_in_together`.
        {
            let (mut p, _m, _t, cid) = project_with_clip();
            p.trim_in(cid, 3.0);
            let c = p.clip(cid).unwrap();
            assert!((c.in_point - 3.0).abs() < 1e-9);
            assert!((c.start - 3.0).abs() < 1e-9);
            // right edge unchanged
            assert!((c.end() - 20.0).abs() < 1e-9);
        }
        // Case `trim_out_clamps_to_source_length`.
        {
            let (mut p, _m, _t, cid) = project_with_clip();
            p.trim_out(cid, 50.0); // way past the 20s source
            let c = p.clip(cid).unwrap();
            assert!((c.out_point - 20.0).abs() < 1e-9);
        }
        // Case `trim_in_cannot_cross_out_point`.
        {
            let (mut p, _m, _t, cid) = project_with_clip();
            p.trim_in(cid, 1000.0);
            let c = p.clip(cid).unwrap();
            assert!(c.duration() >= MIN_CLIP - 1e-9);
        }
    }

    #[test]
    fn timeline_edits_order_ripple_snap_and_paste() {
        // Case `top_video_clip_prefers_higher_track`.
        {
            let (mut p, mid, _t, _cid) = project_with_clip();
            // Add a higher video track with an overlapping clip.
            let t2 = Track {
                id: Uuid::new_v4(),
                name: "V2".into(),
                kind: TrackKind::Video,
                index: 2,
                muted: false,
                locked: false,
                solo: false,
                gain_db: 0.0,
                pan: 0.0,
            };
            let t2id = t2.id;
            p.tracks.push(t2);
            let over = p.add_clip(t2id, mid, 0.0).unwrap();
            let top = p.top_video_clip_at(5.0).unwrap();
            assert_eq!(top.id, over, "V2 clip must win over V1 at the same time");
        }
        // Case `display_tracks_order_video_above_audio`.
        {
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
                    locked: false,
                    solo: false,
                    gain_db: 0.0,
                    pan: 0.0,
                });
            }
            let order: Vec<&str> = p.display_tracks().iter().map(|t| t.name.as_str()).collect();
            assert_eq!(order, vec!["V2", "V1", "A1", "A2"]);
        }
        // Case `ripple_delete_closes_the_gap`.
        {
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
        // Case `snap_magnetises_within_tolerance_only`.
        {
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
        // Case `paste_clip_is_independent_of_source`.
        {
            let (mut p, _m, tid, cid) = project_with_clip();
            let src = p.clip(cid).unwrap().clone();
            let pasted = p.paste_clip(src, tid, 30.0);
            assert_ne!(pasted, cid, "pasted clip gets a fresh id");
            assert!((p.clip(pasted).unwrap().start - 30.0).abs() < 1e-9);
            // Mutating the paste leaves the source clip untouched.
            p.clip_mut(pasted).unwrap().opacity = 0.1;
            assert!((p.clip(cid).unwrap().opacity - 1.0).abs() < 1e-9);
        }
    }

    #[test]
    #[expect(
        clippy::float_cmp,
        reason = "the value is stored, clamped to a literal bound, or a fully-in/fully-out coverage of exactly 0 or 1"
    )]
    fn fades_and_effect_stacks_edit_with_stable_ids() {
        // Case `fade_envelope_ramps_at_head_and_tail`.
        {
            let (mut p, _m, _t, cid) = project_with_clip(); // [0,20]
            {
                let c = p.clip_mut(cid).unwrap();
                c.fade_in = 2.0;
                c.fade_out = 4.0;
            }
            let c = p.clip(cid).unwrap();
            assert!(c.envelope(0.0) < 0.01, "silent at the very head");
            assert!(
                (c.envelope(1.0) - 0.5).abs() < 1e-6,
                "half through a 2s fade-in"
            );
            assert!((c.envelope(10.0) - 1.0).abs() < 1e-6, "full in the middle");
            assert!(
                (c.envelope(18.0) - 0.5).abs() < 1e-6,
                "half through a 4s fade-out"
            );
            assert!(c.envelope(20.0) < 0.01, "silent at the tail");
        }
        // Case `split_keeps_fade_in_on_head_and_fade_out_on_tail`.
        {
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
        // Case `effects_stack_add_remove_and_reorder`.
        {
            let (mut p, _m, _t, cid) = project_with_clip();
            let c = p.clip_mut(cid).unwrap();
            assert!(c.effects.is_empty(), "a fresh clip has no effects");
            c.add_effect(EffectKind::Grayscale);
            c.add_effect(EffectKind::Sepia);
            c.add_effect(EffectKind::Invert);
            let kinds = |c: &Clip| c.effects.iter().map(|e| e.kind).collect::<Vec<_>>();
            assert_eq!(
                kinds(c),
                vec![EffectKind::Grayscale, EffectKind::Sepia, EffectKind::Invert]
            );
            // A fresh effect carries its kind's default intensity.
            assert!((c.effects[0].amount - EffectKind::Grayscale.default_amount()).abs() < 1e-9);
            // Reorder: move Invert (idx 2) up.
            c.move_effect_up(2);
            assert_eq!(
                kinds(c),
                vec![EffectKind::Grayscale, EffectKind::Invert, EffectKind::Sepia]
            );
            // Remove the head.
            c.remove_effect(0);
            assert_eq!(kinds(c), vec![EffectKind::Invert, EffectKind::Sepia]);
            // Out-of-range moves / removes are no-ops.
            c.move_effect_up(0);
            c.move_effect_down(1);
            c.remove_effect(9);
            assert_eq!(kinds(c), vec![EffectKind::Invert, EffectKind::Sepia]);
        }
    }

    /// A second video track and an audio track alongside the fixture's V1.
    fn extra_tracks(p: &mut Project) -> (TrackId, TrackId) {
        let v2 = Track {
            id: Uuid::new_v4(),
            name: "V2".into(),
            kind: TrackKind::Video,
            index: 2,
            muted: false,
            locked: false,
            solo: false,
            gain_db: 0.0,
            pan: 0.0,
        };
        let a1 = Track {
            id: Uuid::new_v4(),
            name: "A1".into(),
            kind: TrackKind::Audio,
            index: 1,
            muted: false,
            locked: false,
            solo: false,
            gain_db: 0.0,
            pan: 0.0,
        };
        let (v, a) = (v2.id, a1.id);
        p.tracks.push(v2);
        p.tracks.push(a1);
        (v, a)
    }

    #[test]
    fn moving_and_placing_clips_respects_kind_and_locks() {
        // Case `move_clip_to_relocates_across_video_tracks`.
        {
            let (mut p, _m, v1, cid) = project_with_clip();
            let (v2, _a1) = extra_tracks(&mut p);
            assert!(
                p.move_clip_to(cid, v2, 3.5),
                "a video clip lands on another video track"
            );
            let c = p.clip(cid).unwrap();
            assert_eq!(c.track, v2, "the clip now belongs to the target track");
            assert!((c.start - 3.5).abs() < 1e-9, "and moved to the drop time");
            assert!(p.clips_on(v1).is_empty(), "it left the source track");
            assert_eq!(p.clips_on(v2).len(), 1);
        }
        // Case `move_clip_to_refuses_a_kind_mismatch`.
        {
            let (mut p, _m, v1, cid) = project_with_clip();
            let (_v2, a1) = extra_tracks(&mut p);
            assert!(
                !p.move_clip_to(cid, a1, 2.0),
                "a video clip cannot land on an audio track"
            );
            let c = p.clip(cid).unwrap();
            assert_eq!(c.track, v1, "the clip stayed where it was");
            assert!(
                (c.start - 0.0).abs() < 1e-9,
                "and did not move in time either"
            );
        }
        // Case `move_clip_to_refuses_a_locked_destination_and_a_locked_source`.
        {
            let (mut p, _m, v1, cid) = project_with_clip();
            let (v2, _a1) = extra_tracks(&mut p);
            // Locked destination.
            p.tracks.iter_mut().find(|t| t.id == v2).unwrap().locked = true;
            assert!(
                !p.move_clip_to(cid, v2, 4.0),
                "a locked track refuses a drop"
            );
            assert_eq!(p.clip(cid).unwrap().track, v1);
            // Locked source: even an unlocked destination refuses.
            p.tracks.iter_mut().find(|t| t.id == v2).unwrap().locked = false;
            p.tracks.iter_mut().find(|t| t.id == v1).unwrap().locked = true;
            assert!(
                !p.move_clip_to(cid, v2, 4.0),
                "a clip on a locked track cannot be moved off it"
            );
            assert_eq!(p.clip(cid).unwrap().track, v1);
        }
        // Case `clip_editable_follows_its_track_lock`.
        {
            let (mut p, _m, v1, cid) = project_with_clip();
            assert!(p.clip_editable(cid));
            p.tracks.iter_mut().find(|t| t.id == v1).unwrap().locked = true;
            assert!(
                !p.clip_editable(cid),
                "a clip on a locked track is not editable"
            );
            assert!(p.track_locked(v1));
            assert!(
                p.track_locked(Uuid::new_v4()),
                "a missing track is treated as locked"
            );
        }
        // Case `placement_track_prefers_the_hint_then_falls_back_by_kind`.
        {
            let (mut p, _m, v1, _cid) = project_with_clip();
            let (v2, a1) = extra_tracks(&mut p);
            assert_eq!(
                p.placement_track(TrackKind::Video, Some(v2)),
                Some(v2),
                "an eligible hint wins"
            );
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
    }

    #[test]
    fn track_mute_solo_and_clip_kind_decide_audibility() {
        // Case `mute_silences_a_track_and_solo_overrides_it`.
        {
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
            assert!(
                !p.track_audible(v1),
                "a non-soloed track of that kind does not"
            );
            assert!(p.track_audible(a1), "audio is unaffected by a video solo");
        }
        // Case `solo_wins_over_mute_on_the_same_track`.
        {
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
        // Case `clip_audible_follows_its_track`.
        {
            let (mut p, _m, v1, cid) = project_with_clip();
            assert!(p.clip_audible(cid));
            p.tracks.iter_mut().find(|t| t.id == v1).unwrap().muted = true;
            assert!(!p.clip_audible(cid));
        }
        // Case `clip_kind_reads_through_to_the_media`.
        {
            let (p, _m, _v1, cid) = project_with_clip();
            assert_eq!(p.clip_kind(cid), TrackKind::Video);
        }
    }

    // --- keyframe navigation (what the inspector's key control reads) ----
}

/// Linked audio and video: the shared group id, what follows it, and what a
/// half-deleted pair degrades to.
#[cfg(test)]
mod link_tests {
    use super::*;

    /// A project holding one picture source whose container carries sound,
    /// placed on V1, plus an empty audio lane to split the sound onto.
    fn shot() -> (Project, ClipId, TrackId) {
        let mut p = Project::new("interview");
        let media = Uuid::new_v4();
        p.media.push(MediaItem {
            id: media,
            name: "Interview_A.mp4".into(),
            kind: MediaKind::Video,
            duration: 30.0,
            has_audio: true,
            width: 1920,
            height: 1080,
            fps: 30.0,
            ..Default::default()
        });
        let v1 = Uuid::new_v4();
        p.tracks.push(Track {
            id: v1,
            name: "V1".into(),
            kind: TrackKind::Video,
            index: 1,
            ..Default::default()
        });
        let a1 = Uuid::new_v4();
        p.tracks.push(Track {
            id: a1,
            name: "A1".into(),
            kind: TrackKind::Audio,
            index: 1,
            ..Default::default()
        });
        let cid = p.add_clip(v1, media, 2.0).expect("place the picture");
        {
            let c = p.clip_mut(cid).expect("the clip");
            c.in_point = 4.0;
            c.out_point = 14.0;
            c.filters.saturation = 0.8;
        }
        (p, cid, a1)
    }

    /// The pair, already split.
    fn pair() -> (Project, ClipId, ClipId) {
        let (mut p, vid, a1) = shot();
        let aid = p.add_linked_audio(vid, a1).expect("split the sound off");
        (p, vid, aid)
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "merged scenario: each former test is a labelled case block with its own fixture"
    )]
    fn linking_and_splitting_sound_forms_a_two_clip_group() {
        // Case `an_unlinked_shot_is_offered_its_sound_back_rather_than_a_second_copy`:
        // Unlinking must not offer to split the sound off a second time.
        //
        // The sound clip is still sitting on the timeline; splitting again makes
        // a duplicate playing the same thing twice. Answering this needs the
        // editor to tell "unlinked, sound still here" from "sound half deleted",
        // which is why an unlink keeps the group id and clears only the
        // following — the two cases then separate exactly, with no pointer to
        // repair and no heuristic over media and spans.
        {
            let (mut p, vid, aid) = pair();
            assert!(
                !p.can_split_audio(vid),
                "a linked shot has nothing to split",
            );

            p.unlink_clips(&[vid, aid]);
            assert!(!p.is_linked(vid), "the two no longer move together");
            assert!(
                !p.can_split_audio(vid),
                "its sound is on the timeline, unlinked — offering the split \
             again makes a duplicate of a clip the user can see",
            );
            assert_eq!(
                p.detached_sound(vid),
                Some(aid),
                "and the editor can name it, which is what it offers to re-link",
            );

            // Re-linking is the way back, and it works from the picture alone.
            let group = p.link_clips(&[vid, aid]).expect("two clips do link");
            assert!(p.is_linked(vid) && p.is_linked(aid));
            assert_eq!(p.clip(vid).expect("picture").link, Some(group));

            // Recovery still works: delete the sound and the split comes back,
            // or a shot whose audio lane was removed would be silent forever.
            p.unlink_clips(&[vid, aid]);
            p.clips.retain(|c| c.id != aid);
            assert_eq!(p.detached_sound(vid), None);
            assert!(
                p.can_split_audio(vid),
                "with its sound gone, the shot must be offered one again",
            );
        }
        // Case `splitting_the_sound_off_makes_a_second_clip_over_the_same_frames`.
        {
            let (p, vid, aid) = pair();
            let v = p.clip(vid).expect("picture");
            let a = p.clip(aid).expect("sound");
            assert_ne!(vid, aid, "the sound is its own clip, not the same one");
            assert_eq!(
                a.media, v.media,
                "both halves read the one source; the bin does not grow a copy"
            );
            assert!((a.start - v.start).abs() < 1e-9, "they start together");
            assert!((a.in_point - v.in_point).abs() < 1e-9);
            assert!((a.out_point - v.out_point).abs() < 1e-9);
            assert_eq!(
                p.track(a.track).map(|t| t.kind),
                Some(TrackKind::Audio),
                "the sound lands on an audio lane, which is what makes it \
             sliddable, levellable and cuttable on its own",
            );
            assert_eq!(v.link, a.link, "one shared group id");
            assert!(v.link.is_some());
            assert!(
                v.sound_split,
                "the picture must stop carrying the sound the audio clip now has",
            );
            assert!(
                !a.sound_split,
                "the sound half is where the sound comes from",
            );
            assert!(
                a.filters.is_identity(),
                "a grade describes frames the sound half never draws: {:?}",
                a.filters,
            );
        }
        // Case `a_link_is_a_group_so_losing_one_half_leaves_an_ordinary_clip`.
        {
            // The whole reason it is a shared id rather than "my partner is clip
            // X": no dangling reference to repair, and no repair to conflict over.
            let (mut p, vid, aid) = pair();
            assert!(p.is_linked(vid) && p.is_linked(aid));
            assert_eq!(p.link_partners(vid), vec![aid]);
            p.delete_clip(aid);
            assert!(
                !p.is_linked(vid),
                "a group with one member is a clip, not a link",
            );
            assert!(p.link_partners(vid).is_empty());
            assert!(
                p.clip(vid)
                    .expect("the picture is still there")
                    .link
                    .is_some(),
                "and nothing had to be written to repair it",
            );
        }
        // Case `linking_needs_two_clips_and_unlinking_leaves_two_ordinary_ones`.
        {
            let (mut p, vid, aid) = pair();
            assert_eq!(
                p.link_clips(&[vid]),
                None,
                "one clip is not a link, and asking must change nothing",
            );
            let changed = p.unlink_clips(&[vid, aid]);
            assert_eq!(changed.len(), 2, "both halves changed: {changed:?}");
            // Two ordinary clips: nothing follows anything.
            assert!(!p.is_linked(vid) && !p.is_linked(aid));
            assert!(p.link_partners(vid).is_empty() && p.link_partners(aid).is_empty());
            // The group id survives the break, which is what still lets the
            // editor find the sound this shot was split from. Asserting
            // `link.is_none()` here used to stand in for "unlinked" — it was the
            // representation, not the claim, and keeping the id is what stops a
            // second copy of the sound being offered.
            assert!(p.clip(vid).expect("picture").link.is_some());
            assert_eq!(p.detached_sound(vid), Some(aid));
            // …and back again, on a fresh group id.
            let group = p.link_clips(&[vid, aid]).expect("two clips do link");
            assert_eq!(p.clip(vid).expect("picture").link, Some(group));
            assert_eq!(p.clip(aid).expect("sound").link, Some(group));
        }
        // Case `a_shot_offers_to_split_its_sound_until_it_has`.
        {
            let (p, vid, _a1) = shot();
            assert!(
                p.can_split_audio(vid),
                "a project made before links existed has to be able to get one",
            );
            let (mut p, vid, aid) = pair();
            assert!(
                !p.can_split_audio(vid),
                "not while a linked clip is already carrying that sound: it would \
             be heard twice",
            );
            assert!(!p.can_split_audio(aid), "and never off a sound clip");
            // Losing the sound half -- deleting the lane it sits on takes every
            // one of them -- has to be recoverable, or every shot on the timeline
            // is permanently silent with nothing offering a way back.
            p.delete_clip(aid);
            assert!(
                p.clip(vid).expect("picture").sound_split,
                "the flag stays: the sound really did leave the picture",
            );
            assert!(
                p.can_split_audio(vid),
                "…but the clip it left for is gone, so the offer has to come back",
            );
        }
        // Case `a_clip_on_an_audio_lane_is_an_audio_clip_whatever_its_source_is`.
        {
            // The sound half references a *video* file. Reading its lane kind off
            // the media would call it a video clip, and `can_place` would then
            // refuse to let it move along the lane it is already on.
            let (p, vid, aid) = pair();
            assert_eq!(p.clip_kind(aid), TrackKind::Audio);
            assert_eq!(
                p.media_kind_of(aid),
                TrackKind::Video,
                "the source really is a picture; it is the placement that differs",
            );
            assert_eq!(p.clip_kind(vid), TrackKind::Video);
            let a_track = p.clip(aid).expect("sound").track;
            assert!(
                p.can_place(aid, a_track),
                "the sound half must be able to move along its own lane",
            );
            assert!(
                !p.can_place(vid, a_track),
                "and a picture clip still may not land on an audio lane",
            );
        }
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "merged scenario: each former test is a labelled case block with its own fixture"
    )]
    fn linked_halves_split_trim_retime_delete_and_paste_together() {
        // Case `splitting_a_linked_shot_gives_the_two_tails_a_group_of_their_own`.
        {
            // Four clips in one group would make dragging the second half of the
            // shot drag the first half with it.
            let (mut p, vid, aid) = pair();
            let was = p.clip(vid).expect("picture").link;
            let pairs = p.split_linked(vid, 6.0);
            assert_eq!(pairs.len(), 2, "both lanes were cut: {pairs:?}");
            let tails: Vec<ClipId> = pairs.iter().map(|(_, t)| *t).collect();
            let tail_link = p.clip(tails[0]).expect("a tail").link;
            assert!(tail_link.is_some());
            assert_ne!(tail_link, was, "the tails are not in the heads' group");
            assert_eq!(
                p.clip(tails[1]).expect("the other tail").link,
                tail_link,
                "but they are in each other's",
            );
            assert_eq!(
                p.clip(vid).expect("head").link,
                was,
                "the heads keep theirs"
            );
            assert_eq!(p.clip(aid).expect("head").link, was);
            assert_eq!(p.link_group(tails[0]).len(), 2);
            assert_eq!(p.link_group(vid).len(), 2);
        }
        // Case `a_lone_tail_comes_away_unlinked_rather_than_tied_to_its_head`.
        {
            let (mut p, vid, _aid) = pair();
            // Only the picture is offered to the cut (a locked sound lane, say).
            let pairs = p.split_group(&[vid], 6.0);
            assert_eq!(pairs.len(), 1);
            let tail = pairs[0].1;
            assert!(
                !p.is_linked(tail),
                "a tail with no sibling must not stay in the group it was cut from",
            );
            // And it must not have inherited the head's *placement* either, or it
            // would be told its sound is already on the timeline and never be
            // offered one.
            assert!(
                p.group_members(tail).is_empty(),
                "the tail carries the head's group: {:?}",
                p.group_members(tail),
            );
        }
        // Case `a_trim_that_clamps_on_one_half_clamps_on_both`.
        {
            // The halves do not have to hold identical windows -- a user can slide
            // the sound. Handing the follower the *raw* delta would move it by
            // more than the clip that was actually dragged, and the two would come
            // out of a single drag desynced by exactly the difference.
            let (mut p, vid, aid) = pair();
            p.clip_mut(vid).expect("picture").in_point = 1.0;
            p.clip_mut(aid).expect("sound").in_point = 6.0;
            let applied = p.trim_in_linked(vid, -5.0);
            assert!(
                (applied - -1.0).abs() < 1e-9,
                "the picture could only give back 1s of head: {applied}",
            );
            assert!((p.clip(vid).expect("picture").in_point - 0.0).abs() < 1e-9);
            assert!(
                (p.clip(aid).expect("sound").in_point - 5.0).abs() < 1e-9,
                "the sound took the same 1s, not the 5s that was asked for",
            );
        }
        // Case `re_timing_one_half_re_times_the_other`.
        {
            // The one field where drifting apart is not cosmetic: the gap grows
            // with every second of the shot and nothing on screen says why.
            let (mut p, vid, aid) = pair();
            p.set_speed_linked(vid, 2.0, true);
            let v = p.clip(vid).expect("picture");
            let a = p.clip(aid).expect("sound");
            assert_eq!(a.speed, v.speed, "both halves play at one rate");
            assert!((a.speed.rate() - 2.0).abs() < 1e-9);
            assert!(a.speed.reverse);
            assert!(
                (a.duration() - v.duration()).abs() < 1e-9,
                "so they still cover the same stretch of timeline",
            );
        }
        // Case `deleting_or_rippling_one_half_takes_the_whole_shot`.
        {
            let (mut p, vid, aid) = pair();
            // A later clip on each lane, to prove the gap really closes on both.
            let (v_track, a_track) = (
                p.clip(vid).expect("picture").track,
                p.clip(aid).expect("sound").track,
            );
            let media = p.clip(vid).expect("picture").media;
            let after_v = p.add_clip(v_track, media, 20.0).expect("a later picture");
            let after_a = p.add_clip(a_track, media, 20.0).expect("a later sound");
            let gap = p.clip(vid).expect("picture").duration();

            let (removed, moved) = p.ripple_delete_linked(vid);
            assert_eq!(removed.len(), 2, "both halves went: {removed:?}");
            assert!(p.clip(vid).is_none() && p.clip(aid).is_none());
            assert!(
                moved.contains(&after_v) && moved.contains(&after_a),
                "{moved:?}"
            );
            for later in [after_v, after_a] {
                assert!(
                    (p.clip(later).expect("later clip").start - (20.0 - gap)).abs() < 1e-9,
                    "the gap has to close on the sound lane as well as the picture one",
                );
            }

            let (mut p, vid, aid) = pair();
            assert_eq!(p.delete_linked(vid).len(), 2);
            assert!(
                p.clip(aid).is_none(),
                "half a shot left behind is the surprise"
            );
        }
        // Case `a_pasted_copy_is_independent_of_the_shot_it_came_from`.
        {
            // Carrying the group over would tie the copy to the original, so
            // dragging the original would drag the copy across the timeline.
            let (mut p, vid, _aid) = pair();
            let clip = p.clip(vid).expect("picture").clone();
            let track = clip.track;
            let copy = p.paste_clip(clip, track, 30.0);
            assert!(p.clip(copy).expect("the copy").link.is_none());
            assert!(!p.is_linked(copy));
        }
    }
}
