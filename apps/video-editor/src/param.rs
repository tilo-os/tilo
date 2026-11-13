//! Animatable parameters: a numeric field on a clip keeps its constant value,
//! and a curve on the clip overrides it. Every reader asks
//! [`Clip::param_at`](crate::model::Clip::param_at), so a value is animated
//! wherever it is used.
//!
//! A keyframe's `t` is seconds from the clip's start, not timeline seconds, so
//! moving or splitting a clip carries its animation with it.

use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum Interp {
    #[default]
    Linear,
    /// Hold this key's value until the next key, then jump.
    Hold,
    Ease,
}

impl Interp {
    pub const ALL: [Interp; 3] = [Interp::Linear, Interp::Hold, Interp::Ease];

    pub fn as_str(self) -> &'static str {
        match self {
            Interp::Linear => "linear",
            Interp::Hold => "hold",
            Interp::Ease => "ease",
        }
    }
    pub fn message(self) -> &'static str {
        match self {
            Interp::Linear => "video-editor-interp-linear",
            Interp::Hold => "video-editor-interp-hold",
            Interp::Ease => "video-editor-interp-ease",
        }
    }
    pub fn from_name(s: &str) -> Self {
        match s {
            "hold" | "step" | "constant" => Interp::Hold,
            "ease" | "smooth" => Interp::Ease,
            _ => Interp::Linear,
        }
    }

    fn shape(self, u: f64) -> f64 {
        let u = u.clamp(0.0, 1.0);
        match self {
            Interp::Linear => u,
            Interp::Hold => 0.0,
            // smoothstep: zero slope at both ends.
            Interp::Ease => u * u * (3.0 - 2.0 * u),
        }
    }
}

/// A value at a clip-local time, and how the curve leaves it.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Keyframe {
    /// Seconds from the clip's start.
    pub t: f64,
    pub v: f64,
    #[serde(default)]
    pub interp: Interp,
}

impl Keyframe {
    pub fn new(t: f64, v: f64) -> Self {
        Self {
            t,
            v,
            interp: Interp::Linear,
        }
    }
}

/// Which value a curve drives, written as its name: `opacity`, `pos_x`,
/// `effect:<id>`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Param {
    Opacity,
    Volume,
    Brightness,
    Contrast,
    Saturation,
    ScaleX,
    ScaleY,
    PosX,
    PosY,
    Rotation,
    /// How wide a band of chroma is removed.
    KeyTolerance,
    /// Width of the key's soft edge.
    KeySoftness,
    /// How much of the key colour is pulled out of what survives.
    KeySpill,
    /// Mask centre, offset from the frame centre in frame widths and heights.
    MaskX,
    MaskY,
    /// Mask extent, as a fraction of the frame.
    MaskW,
    MaskH,
    /// Mask rotation, degrees.
    MaskRotation,
    /// Softness of the mask edge.
    MaskFeather,
    /// How much of the tone curve is mixed in: 0 bypasses it, 1 is the curve as
    /// drawn. Its shape is not animatable, so this is the knob a ramp drives.
    CurveMix,
    /// One effect instance of the clip's stack, by its id, so the curve stays
    /// with it however the stack is reordered, here or on a peer.
    EffectAmount(uuid::Uuid),
}

impl Serialize for Param {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.as_str())
    }
}

impl<'de> Deserialize<'de> for Param {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let name = String::deserialize(deserializer)?;
        Param::from_name(&name)
            .ok_or_else(|| serde::de::Error::custom(format!("`{name}` is no animatable value")))
    }
}

impl ui::tilo_resource::JsonSchema for Param {
    fn json_schema() -> serde_json::Value {
        let mut names: Vec<String> = Param::SIMPLE.iter().map(|p| p.as_str()).collect();
        names.push("effect:<effect id>".into());
        serde_json::json!({
            "type": "string",
            "description": "An animatable value by name; `effect:<id>` is the strength of \
                            the clip's effect with that id",
            "examples": names,
        })
    }
}

impl Param {
    pub const SIMPLE: [Param; 20] = [
        Param::Opacity,
        Param::Volume,
        Param::Brightness,
        Param::Contrast,
        Param::Saturation,
        Param::ScaleX,
        Param::ScaleY,
        Param::PosX,
        Param::PosY,
        Param::Rotation,
        Param::KeyTolerance,
        Param::KeySoftness,
        Param::KeySpill,
        Param::MaskX,
        Param::MaskY,
        Param::MaskW,
        Param::MaskH,
        Param::MaskRotation,
        Param::MaskFeather,
        Param::CurveMix,
    ];

    pub fn as_str(self) -> String {
        match self {
            Param::Opacity => "opacity".into(),
            Param::Volume => "volume".into(),
            Param::Brightness => "brightness".into(),
            Param::Contrast => "contrast".into(),
            Param::Saturation => "saturation".into(),
            Param::ScaleX => "scale_x".into(),
            Param::ScaleY => "scale_y".into(),
            Param::PosX => "pos_x".into(),
            Param::PosY => "pos_y".into(),
            Param::Rotation => "rotation".into(),
            Param::KeyTolerance => "key_tolerance".into(),
            Param::KeySoftness => "key_softness".into(),
            Param::KeySpill => "key_spill".into(),
            Param::MaskX => "mask_x".into(),
            Param::MaskY => "mask_y".into(),
            Param::MaskW => "mask_w".into(),
            Param::MaskH => "mask_h".into(),
            Param::MaskRotation => "mask_rotation".into(),
            Param::MaskFeather => "mask_feather".into(),
            Param::CurveMix => "curve_mix".into(),
            Param::EffectAmount(id) => format!("effect:{id}"),
        }
    }

    pub fn from_name(s: &str) -> Option<Self> {
        Some(match s {
            "opacity" => Param::Opacity,
            "volume" => Param::Volume,
            "brightness" => Param::Brightness,
            "contrast" => Param::Contrast,
            "saturation" => Param::Saturation,
            "scale_x" => Param::ScaleX,
            "scale_y" => Param::ScaleY,
            "pos_x" => Param::PosX,
            "pos_y" => Param::PosY,
            "rotation" => Param::Rotation,
            "key_tolerance" => Param::KeyTolerance,
            "key_softness" => Param::KeySoftness,
            "key_spill" => Param::KeySpill,
            "mask_x" => Param::MaskX,
            "mask_y" => Param::MaskY,
            "mask_w" => Param::MaskW,
            "mask_h" => Param::MaskH,
            "mask_rotation" => Param::MaskRotation,
            "mask_feather" => Param::MaskFeather,
            "curve_mix" => Param::CurveMix,
            other => {
                let id = other.strip_prefix("effect:")?;
                Param::EffectAmount(id.parse().ok()?)
            }
        })
    }
}

/// The curve for one parameter. Keys are sorted by time.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ParamTrack {
    pub param: Param,
    pub keys: Vec<Keyframe>,
}

impl ParamTrack {
    pub fn new(param: Param) -> Self {
        Self {
            param,
            keys: Vec::new(),
        }
    }

    /// Sample at clip-local time `t`: the first key's value before it and the
    /// last key's after it, so a curve never extrapolates. `None` with no keys,
    /// and the caller then reads the constant field.
    pub fn sample(&self, t: f64) -> Option<f64> {
        let (Some(first), Some(last)) = (self.keys.first(), self.keys.last()) else {
            return None;
        };
        if t <= first.t {
            return Some(first.v);
        }
        if t >= last.t {
            return Some(last.v);
        }
        // Keys are sorted; find the segment `t` lands in.
        let i = match self
            .keys
            .binary_search_by(|k| k.t.partial_cmp(&t).unwrap_or(std::cmp::Ordering::Equal))
        {
            Ok(exact) => return self.keys.get(exact).map(|k| k.v),
            // `Err(n)` is the insertion point, so the left key is n-1.
            Err(n) => n.saturating_sub(1),
        };
        // `t` lies strictly between the first and last keys, so the insertion
        // point is in `1..len` and the segment `[i, i + 1]` exists.
        let Some(&[left, right]) = self.keys.get(i..i + 2) else {
            return Some(last.v);
        };
        let span = right.t - left.t;
        if span <= 1e-12 {
            // Two keys at the same instant: the later one wins, which is
            // what a hard cut in a curve should look like.
            return Some(right.v);
        }
        let eased = left.interp.shape((t - left.t) / span);
        Some(left.v + (right.v - left.v) * eased)
    }

    /// Set the key at `t`, replacing one within a microsecond, so re-keying an
    /// instant edits rather than stacks.
    pub fn set_key(&mut self, t: f64, v: f64, interp: Interp) {
        let key = Keyframe { t, v, interp };
        if let Some(existing) = self.keys.iter_mut().find(|k| (k.t - t).abs() < 1e-6) {
            *existing = key;
        } else {
            let at = self
                .keys
                .iter()
                .position(|k| k.t > t)
                .unwrap_or(self.keys.len());
            self.keys.insert(at, key);
        }
    }

    pub fn remove_key_near(&mut self, t: f64, tol: f64) -> bool {
        let Some((i, _)) = self
            .keys
            .iter()
            .enumerate()
            .filter(|(_, k)| (k.t - t).abs() <= tol)
            .min_by(|a, b| {
                (a.1.t - t)
                    .abs()
                    .partial_cmp(&(b.1.t - t).abs())
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
        else {
            return false;
        };
        self.keys.remove(i);
        true
    }

    /// Keep the keys inside `[from, to)` and rebase them onto a clip that now
    /// starts `from` later, with a key at each edge holding the value the
    /// original curve had there, so a cut changes nothing at the cut.
    #[must_use]
    pub fn sliced(&self, from: f64, to: f64) -> Self {
        let mut out = ParamTrack::new(self.param);
        if self.keys.is_empty() {
            return out;
        }
        let edge_interp = |t: f64| {
            self.keys
                .iter()
                .rev()
                .find(|k| k.t <= t)
                .map(|k| k.interp)
                .unwrap_or_default()
        };
        if let Some(v) = self.sample(from) {
            out.keys.push(Keyframe {
                t: 0.0,
                v,
                interp: edge_interp(from),
            });
        }
        for k in &self.keys {
            if k.t > from && k.t < to {
                out.set_key(k.t - from, k.v, k.interp);
            }
        }
        if let Some(v) = self.sample(to) {
            out.set_key((to - from).max(0.0), v, edge_interp(to));
        }
        out
    }
}

/// Every curve on one clip. Empty means nothing is animated.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct ClipAnimation {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tracks: Vec<ParamTrack>,
}

impl ClipAnimation {
    pub fn is_empty(&self) -> bool {
        self.tracks.iter().all(|t| t.keys.is_empty())
    }

    pub fn track(&self, param: Param) -> Option<&ParamTrack> {
        self.tracks.iter().find(|t| t.param == param)
    }

    pub fn track_mut(&mut self, param: Param) -> Option<&mut ParamTrack> {
        self.tracks.iter_mut().find(|t| t.param == param)
    }

    #[expect(
        clippy::indexing_slicing,
        reason = "`i` is either an existing track's position or the one just pushed"
    )]
    pub fn ensure(&mut self, param: Param) -> &mut ParamTrack {
        let i = if let Some(i) = self.tracks.iter().position(|t| t.param == param) {
            i
        } else {
            self.tracks.push(ParamTrack::new(param));
            self.tracks.len() - 1
        };
        &mut self.tracks[i]
    }

    pub fn sample(&self, param: Param, t: f64) -> Option<f64> {
        self.track(param).and_then(|tr| tr.sample(t))
    }

    pub fn is_animated(&self, param: Param) -> bool {
        self.track(param).is_some_and(|t| !t.keys.is_empty())
    }

    pub fn animated_params(&self) -> Vec<Param> {
        self.tracks
            .iter()
            .filter(|t| !t.keys.is_empty())
            .map(|t| t.param)
            .collect()
    }

    /// Drop the curve, returning the parameter to its constant.
    pub fn clear(&mut self, param: Param) {
        self.tracks.retain(|t| t.param != param);
    }

    pub fn forget_effect(&mut self, id: uuid::Uuid) {
        self.tracks
            .retain(|track| track.param != Param::EffectAmount(id));
    }

    pub fn clear_effect_slots(&mut self) {
        self.tracks
            .retain(|track| !matches!(track.param, Param::EffectAmount(_)));
    }

    /// See [`ParamTrack::sliced`].
    #[must_use]
    pub fn sliced(&self, from: f64, to: f64) -> Self {
        Self {
            tracks: self
                .tracks
                .iter()
                .filter(|t| !t.keys.is_empty())
                .map(|t| t.sliced(from, to))
                .collect(),
        }
    }

    /// Every keyframe time on any curve, sorted and de-duplicated.
    pub fn key_times(&self) -> Vec<f64> {
        let mut ts: Vec<f64> = self
            .tracks
            .iter()
            .flat_map(|t| t.keys.iter().map(|k| k.t))
            .collect();
        ts.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        ts.dedup_by(|a, b| (*a - *b).abs() < 1e-6);
        ts
    }
}

ui::tilo_resource::json_model!(ClipAnimation);

#[cfg(test)]
mod tests {
    use super::*;

    fn track(keys: &[(f64, f64)]) -> ParamTrack {
        let mut t = ParamTrack::new(Param::Opacity);
        for (at, v) in keys {
            t.set_key(*at, *v, Interp::Linear);
        }
        t
    }

    #[test]
    fn keyframe_interpolation_modes_evaluate_correctly() {
        {
            assert_eq!(ParamTrack::new(Param::Opacity).sample(3.0), None);
        }
        {
            let t = track(&[(1.0, 0.0), (3.0, 1.0)]);
            assert_eq!(t.sample(0.0), Some(0.0), "before the first key it holds");
            assert_eq!(t.sample(1.0), Some(0.0));
            assert!(
                (t.sample(2.0).unwrap() - 0.5).abs() < 1e-9,
                "midpoint is the mean"
            );
            assert!((t.sample(2.5).unwrap() - 0.75).abs() < 1e-9);
            assert_eq!(t.sample(3.0), Some(1.0));
            assert_eq!(t.sample(99.0), Some(1.0), "after the last key it holds");
        }
        {
            let mut t = ParamTrack::new(Param::Opacity);
            t.set_key(0.0, 0.2, Interp::Hold);
            t.set_key(2.0, 0.9, Interp::Linear);
            assert_eq!(t.sample(1.0), Some(0.2), "held right up to the next key");
            assert_eq!(t.sample(1.999), Some(0.2));
            assert_eq!(t.sample(2.0), Some(0.9), "and jumps on it");
        }
        {
            let mut t = ParamTrack::new(Param::Opacity);
            t.set_key(0.0, 0.0, Interp::Ease);
            t.set_key(1.0, 1.0, Interp::Linear);
            let early = t.sample(0.1).unwrap();
            let mid = t.sample(0.5).unwrap();
            let late = t.sample(0.9).unwrap();
            assert!(early < 0.1, "eased start moves slower than linear: {early}");
            assert!((mid - 0.5).abs() < 1e-9, "symmetric at the midpoint");
            assert!(late > 0.9, "eased end has already caught up: {late}");
        }
    }

    #[test]
    fn keyframe_editing_and_slicing_preserve_values() {
        {
            let mut t = track(&[(1.0, 0.0)]);
            t.set_key(1.0, 0.7, Interp::Linear);
            assert_eq!(t.keys.len(), 1);
            assert_eq!(t.sample(1.0), Some(0.7));
        }
        {
            let t = track(&[(5.0, 1.0), (1.0, 0.0), (3.0, 0.5)]);
            let times: Vec<f64> = t.keys.iter().map(|k| k.t).collect();
            assert_eq!(times, vec![1.0, 3.0, 5.0]);
            assert!((t.sample(2.0).unwrap() - 0.25).abs() < 1e-9);
        }
        {
            let mut t = track(&[(1.0, 0.0), (3.0, 1.0)]);
            assert!(!t.remove_key_near(2.0, 0.1), "nothing is that close");
            assert!(t.remove_key_near(3.05, 0.2));
            assert_eq!(t.keys.len(), 1);
        }
        {
            // Ramp 0 → 1 over 4 seconds; slice the middle two seconds.
            let t = track(&[(0.0, 0.0), (4.0, 1.0)]);
            let s = t.sliced(1.0, 3.0);
            assert!(
                (s.sample(0.0).unwrap() - 0.25).abs() < 1e-9,
                "left edge keeps its value"
            );
            assert!(
                (s.sample(2.0).unwrap() - 0.75).abs() < 1e-9,
                "right edge too"
            );
            assert!((s.sample(1.0).unwrap() - 0.5).abs() < 1e-9);
        }
        {
            let t = track(&[(0.0, 0.0), (2.0, 1.0), (4.0, 0.0)]);
            let s = t.sliced(1.0, 4.0);
            // The interior key at t=2 lands at 1 in the slice, still at 1.0.
            assert!((s.sample(1.0).unwrap() - 1.0).abs() < 1e-9);
        }
    }

    #[test]
    fn animation_tracks_report_merge_and_round_trip_names() {
        {
            let mut a = ClipAnimation::default();
            assert!(a.is_empty());
            assert_eq!(a.sample(Param::Opacity, 1.0), None);
            a.ensure(Param::Opacity).set_key(0.0, 0.5, Interp::Linear);
            assert!(!a.is_empty());
            assert!(a.is_animated(Param::Opacity));
            assert!(!a.is_animated(Param::Volume));
            assert_eq!(a.animated_params(), vec![Param::Opacity]);
            a.clear(Param::Opacity);
            assert!(!a.is_animated(Param::Opacity));
        }
        {
            let [first, second, third] = [1, 2, 3].map(uuid::Uuid::from_u128);
            let mut animation = ClipAnimation::default();
            animation
                .ensure(Param::EffectAmount(first))
                .set_key(0.0, 0.2, Interp::Linear);
            animation
                .ensure(Param::EffectAmount(second))
                .set_key(0.0, 0.8, Interp::Linear);
            animation
                .ensure(Param::EffectAmount(third))
                .set_key(0.0, 0.4, Interp::Linear);
            animation
                .ensure(Param::Opacity)
                .set_key(0.0, 0.6, Interp::Linear);

            animation.forget_effect(second);
            assert_eq!(animation.sample(Param::EffectAmount(first), 0.0), Some(0.2));
            assert_eq!(animation.sample(Param::EffectAmount(second), 0.0), None);
            assert_eq!(animation.sample(Param::EffectAmount(third), 0.0), Some(0.4));
            assert_eq!(animation.sample(Param::Opacity, 0.0), Some(0.6));

            animation.clear_effect_slots();
            assert_eq!(animation.sample(Param::EffectAmount(first), 0.0), None);
            assert_eq!(animation.sample(Param::EffectAmount(third), 0.0), None);
            assert_eq!(animation.sample(Param::Opacity, 0.0), Some(0.6));
        }
        {
            let mut a = ClipAnimation::default();
            a.ensure(Param::Opacity).set_key(1.0, 0.0, Interp::Linear);
            a.ensure(Param::Opacity).set_key(3.0, 1.0, Interp::Linear);
            a.ensure(Param::ScaleX).set_key(1.0, 1.0, Interp::Linear);
            a.ensure(Param::ScaleX).set_key(2.0, 2.0, Interp::Linear);
            assert_eq!(a.key_times(), vec![1.0, 2.0, 3.0]);
        }
        {
            for p in Param::SIMPLE {
                assert_eq!(Param::from_name(&p.as_str()), Some(p), "{p:?}");
            }
            let e = Param::EffectAmount(uuid::Uuid::from_u128(3));
            assert_eq!(Param::from_name(&e.as_str()), Some(e));
            assert_eq!(Param::from_name("effect3"), None);
            assert_eq!(Param::from_name("nonsense"), None);
        }
        {
            for i in Interp::ALL {
                assert_eq!(Interp::from_name(i.as_str()), i);
            }
        }
    }
}
