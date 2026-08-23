//! GPU compositing for the timeline — one renderer for the preview and
//! the export.
//!
//! Everything visual about a frame happens here: the layer stack (every
//! visible clip on every visible video track, bottom to top), the colour
//! grade, the stackable effect chain, the fade/opacity envelope, and the
//! transitions, all in a wgpu compute pipeline. [`crate::preview`]
//! decides *what* the stack is (pure, testable, no device);
//! [`compositor::VideoCompositor`] draws it.
//!
//! ## Where pictures come from
//!
//! The compositor never decodes anything. It asks a [`FrameSource`] for
//! `(media, source_time) -> texture` and, when the source has nothing,
//! falls back to the clip's deterministic representative fill. That one
//! seam covers every case:
//!
//!   * `--shots` / hermetic CI — [`NoSources`], so no decoder is needed
//!     and the frame is reproducible.
//!   * The live preview — [`CpuFrames`] wrapping the frames
//!     [`crate::preview::PreviewDecoder`] already pulls.
//!   * Export — [`CpuFrames`] wrapping a sequential decode of the real
//!     media (see [`crate::export`]).
//!   * A future zero-copy path — a `FrameSource` handing back an imported
//!     dmabuf texture, with nothing else in this module changing.

pub mod compositor;
/// Decoded pictures from the real media engine — the source the editor uses.
#[cfg(feature = "decode")]
pub mod media_source;

use std::collections::HashMap;
use std::sync::Arc;

use vello::wgpu;

use crate::model::MediaId;

pub use compositor::{MAX_SHADER_EFFECTS, VideoCompositor, Viewport, upload_rgba};
#[cfg(feature = "decode")]
pub use media_source::{Fetch, MediaFrameSource};

/// A picture the compositor can sample, plus the size it was decoded at
/// (which drives the aspect fit into the project frame).
#[derive(Clone)]
pub struct SourceTexture {
    /// Kept alive alongside the view for as long as the caller holds this.
    pub texture: Arc<wgpu::Texture>,
    pub view: Arc<wgpu::TextureView>,
    pub width: u32,
    pub height: u32,
}

/// Resolves a layer's picture. Returning `None` is normal, not an error:
/// the compositor then draws the layer's representative fill, so an
/// un-hydrated reference or a build with no decoder still renders.
pub trait FrameSource {
    /// Called once before the layers of a composite are resolved.
    ///
    /// A source that hands out textures it reuses across frames needs to know
    /// where a composite begins, so two layers in the same frame never get
    /// handed the same reusable texture.
    fn begin_frame(&mut self) {}

    fn resolve(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        media: MediaId,
        source_time: f64,
    ) -> Option<SourceTexture>;
}

/// No pictures at all — every layer draws its representative fill. The
/// hermetic path: deterministic, and it needs no media pipeline.
pub struct NoSources;

impl FrameSource for NoSources {
    fn resolve(
        &mut self,
        _device: &wgpu::Device,
        _queue: &wgpu::Queue,
        _media: MediaId,
        _source_time: f64,
    ) -> Option<SourceTexture> {
        None
    }
}

/// Adapts any producer of CPU RGBA frames into a [`FrameSource`],
/// uploading each picture once and caching it by identity.
///
/// The closure returns `(width, height, rgba, identity)`. `identity` is
/// whatever distinguishes one decoded picture from another for that media
/// — a presentation timestamp is the natural choice — and is the cache
/// key, so holding a paused playhead re-uploads nothing.
pub struct CpuFrames<F> {
    produce: F,
    cache: HashMap<(MediaId, u64), SourceTexture>,
}

/// One decoded picture handed over by a [`CpuFrames`] producer.
pub struct CpuFrame {
    pub width: u32,
    pub height: u32,
    pub rgba: Arc<Vec<u8>>,
    /// Cache key within this media — a presentation timestamp in
    /// milliseconds is the usual choice.
    pub identity: u64,
}

impl<F> CpuFrames<F>
where
    F: FnMut(MediaId, f64) -> Option<CpuFrame>,
{
    pub fn new(produce: F) -> Self {
        Self { produce, cache: HashMap::new() }
    }

    /// Drop cached uploads. Worth calling when the playhead jumps far, so
    /// a long scrub does not pin every frame it passed.
    pub fn clear(&mut self) {
        self.cache.clear();
    }
}

impl<F> FrameSource for CpuFrames<F>
where
    F: FnMut(MediaId, f64) -> Option<CpuFrame>,
{
    fn resolve(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        media: MediaId,
        source_time: f64,
    ) -> Option<SourceTexture> {
        let frame = (self.produce)(media, source_time)?;
        let key = (media, frame.identity);
        if let Some(hit) = self.cache.get(&key) {
            return Some(hit.clone());
        }
        let tex = upload_rgba(device, queue, frame.width, frame.height, &frame.rgba)?;
        // A handful of distinct pictures is all a frame ever needs; keep
        // the map from growing across a long export.
        if self.cache.len() > 16 {
            self.cache.clear();
        }
        self.cache.insert(key, tex.clone());
        Some(tex)
    }
}

/// A compositor bound to the process-wide shared wgpu device.
///
/// Uses [`ui::offscreen::shared_gpu`] rather than building an Instance of
/// its own — parallel `wgpu::Instance` creation races inside Mesa and has
/// crashed this workspace's test suite before.
pub struct HeadlessCompositor {
    gpu: ui::offscreen::SharedGpu,
    compositor: VideoCompositor,
}

impl HeadlessCompositor {
    /// Borrow the shared device. Fails (rather than panicking) on a box
    /// with no usable adapter, so callers can report it honestly.
    pub fn new() -> Result<Self, String> {
        let gpu = ui::offscreen::shared_gpu().map_err(|e| e.to_string())?;
        Ok(Self { gpu, compositor: VideoCompositor::new() })
    }

    /// Composite the timeline at `playhead` and hand back a tightly-packed
    /// `width * height * 4` RGBA frame.
    pub fn frame(
        &mut self,
        project: &crate::model::Project,
        playhead: f64,
        width: u32,
        height: u32,
        sources: &mut dyn FrameSource,
    ) -> Vec<u8> {
        self.compositor.render_rgba(
            &self.gpu.device,
            &self.gpu.queue,
            project,
            playhead,
            width,
            height,
            sources,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{
        ClipEffect, EffectKind, Filters, MediaItem, MediaKind, Project, Track, TrackKind,
        Transition,
    };
    use crate::preview::{Rgb, palette_rgb};
    use uuid::Uuid;

    /// The compositor under test, or a skip when this box has no adapter.
    /// A missing GPU is an environment fact, not a passing test — every
    /// caller here reports it loudly rather than asserting nothing.
    fn compositor() -> HeadlessCompositor {
        match HeadlessCompositor::new() {
            Ok(c) => c,
            Err(e) => panic!("the composite tests need a wgpu adapter (llvmpipe is enough): {e}"),
        }
    }

    fn px(buf: &[u8], w: u32, x: u32, y: u32) -> Rgb {
        let i = ((y * w + x) * 4) as usize;
        Rgb::new(
            buf[i] as f64 / 255.0,
            buf[i + 1] as f64 / 255.0,
            buf[i + 2] as f64 / 255.0,
        )
    }

    fn media(p: &mut Project, palette: u8) -> Uuid {
        let id = Uuid::new_v4();
        p.media.push(MediaItem {
            id,
            name: format!("m{palette}"),
            kind: MediaKind::Video,
            duration: 30.0,
            width: 16,
            height: 9,
            fps: 30.0,
            palette,
            path: crate::media::workspace_path("m.mp4"),
            hydration: crate::media::Hydration::Projected,
            ..Default::default()
        });
        id
    }

    fn track(p: &mut Project, index: u32) -> Uuid {
        let id = Uuid::new_v4();
        p.tracks.push(Track {
            id,
            name: format!("V{index}"),
            kind: TrackKind::Video,
            index,
            muted: false,
            locked: false, solo: false,
        });
        id
    }

    #[test]
    fn empty_timeline_composites_to_black() {
        let p = Project::new("t");
        let mut c = compositor();
        let buf = c.frame(&p, 0.0, 16, 16, &mut NoSources);
        assert_eq!(buf.len(), 16 * 16 * 4);
        assert!(
            buf.chunks(4).all(|q| q[0] < 4 && q[1] < 4 && q[2] < 4 && q[3] == 255),
            "no clips means an opaque black frame",
        );
    }

    #[test]
    fn a_clip_composites_its_palette_and_the_grade_moves_it() {
        let mut p = Project::new("t");
        let m = media(&mut p, 3); // a warm amber slot
        let t = track(&mut p, 1);
        let cid = p.add_clip(t, m, 0.0).unwrap();
        p.clip_mut(cid).unwrap().out_point = 10.0;

        let mut c = compositor();
        let neutral = c.frame(&p, 2.0, 32, 32, &mut NoSources);
        // The clip's palette colour is on the canvas, not black.
        let mid = px(&neutral, 32, 16, 20);
        assert!(mid.luma() > 0.1, "a clip renders visible colour, got {mid:?}");

        p.clip_mut(cid).unwrap().filters =
            Filters { brightness: 0.95, contrast: 0.5, saturation: 0.5 };
        let bright = c.frame(&p, 2.0, 32, 32, &mut NoSources);
        assert!(
            px(&bright, 32, 16, 20).luma() > mid.luma() + 0.02,
            "raising brightness brightens the composited frame",
        );

        p.clip_mut(cid).unwrap().filters =
            Filters { brightness: 0.5, contrast: 0.5, saturation: 0.0 };
        let flat = px(&c.frame(&p, 2.0, 32, 32, &mut NoSources), 32, 16, 20);
        assert!(
            (flat.r - flat.g).abs() < 0.03 && (flat.g - flat.b).abs() < 0.03,
            "full desaturation collapses the channels, got {flat:?}",
        );
    }

    #[test]
    fn the_shader_grade_matches_the_cpu_grade() {
        // The shader is the production path; this pins its arithmetic to
        // `Rgb::graded`, which the inspector and every CPU test rely on.
        let mut p = Project::new("t");
        let m = media(&mut p, 0);
        let t = track(&mut p, 1);
        let cid = p.add_clip(t, m, 0.0).unwrap();
        let f = Filters { brightness: 0.66, contrast: 0.58, saturation: 0.34 };
        p.clip_mut(cid).unwrap().out_point = 10.0;
        p.clip_mut(cid).unwrap().filters = f;

        let mut c = compositor();
        let buf = c.frame(&p, 1.0, 64, 64, &mut NoSources);
        // Row 40 of 64 sits below the upper band and outside the mid band
        // (0.62..0.72 → rows 40..46 — so pick row 50), where the fill is
        // just the darkened base.
        let got = px(&buf, 64, 32, 50);
        let base = palette_rgb(0);
        let want = Rgb::new(base.r * 0.82, base.g * 0.82, base.b * 0.82).graded(&f);
        assert!(
            (got.r - want.r).abs() < 0.02
                && (got.g - want.g).abs() < 0.02
                && (got.b - want.b).abs() < 0.02,
            "shader grade {got:?} should match the CPU grade {want:?}",
        );
    }

    #[test]
    fn every_video_track_contributes_a_layer() {
        // The old single-clip path threw away everything below the top
        // track. A half-transparent upper layer must now let the lower one
        // through.
        let mut p = Project::new("t");
        let lower = media(&mut p, 2); // green
        let upper = media(&mut p, 5); // red
        let v1 = track(&mut p, 1);
        let v2 = track(&mut p, 2);
        let c1 = p.add_clip(v1, lower, 0.0).unwrap();
        p.clip_mut(c1).unwrap().out_point = 10.0;
        let c2 = p.add_clip(v2, upper, 0.0).unwrap();
        p.clip_mut(c2).unwrap().out_point = 10.0;

        let mut c = compositor();
        let opaque = px(&c.frame(&p, 2.0, 32, 32, &mut NoSources), 32, 16, 20);
        // Opaque top layer: the red clip wins outright.
        assert!(opaque.r > opaque.g, "an opaque top layer covers the one below: {opaque:?}");

        p.clip_mut(c2).unwrap().opacity = 0.5;
        let blended = px(&c.frame(&p, 2.0, 32, 32, &mut NoSources), 32, 16, 20);
        assert!(
            blended.g > opaque.g + 0.05,
            "a half-transparent top layer lets the lower track show through: {blended:?} vs {opaque:?}",
        );
    }

    #[test]
    fn a_muted_video_track_is_not_drawn() {
        let mut p = Project::new("t");
        let m = media(&mut p, 3);
        let t = track(&mut p, 1);
        let cid = p.add_clip(t, m, 0.0).unwrap();
        p.clip_mut(cid).unwrap().out_point = 10.0;
        let mut c = compositor();
        assert!(px(&c.frame(&p, 1.0, 16, 16, &mut NoSources), 16, 8, 10).luma() > 0.1);

        p.tracks[0].muted = true;
        let hidden = c.frame(&p, 1.0, 16, 16, &mut NoSources);
        assert!(
            hidden.chunks(4).all(|q| q[0] < 4 && q[1] < 4 && q[2] < 4),
            "a muted video track contributes nothing",
        );

        // Locking is an editing lock, never a visibility one.
        p.tracks[0].muted = false;
        p.tracks[0].locked = true;
        assert!(
            px(&c.frame(&p, 1.0, 16, 16, &mut NoSources), 16, 8, 10).luma() > 0.1,
            "a locked track still renders",
        );
    }

    #[test]
    fn the_vignette_darkens_the_corners_and_spares_the_centre() {
        // The CPU stand-in could only dim the whole frame flat; the shader
        // has real spatial falloff, so corner and centre must diverge.
        let mut p = Project::new("t");
        let m = media(&mut p, 6); // a flat grey slot: no vertical banding bias
        let t = track(&mut p, 1);
        let cid = p.add_clip(t, m, 0.0).unwrap();
        p.clip_mut(cid).unwrap().out_point = 10.0;

        let mut c = compositor();
        let plain = c.frame(&p, 1.0, 64, 64, &mut NoSources);
        p.clip_mut(cid).unwrap().effects =
            vec![ClipEffect { kind: EffectKind::Vignette, amount: 1.0 }];
        let vign = c.frame(&p, 1.0, 64, 64, &mut NoSources);

        let corner_before = px(&plain, 64, 1, 62).luma();
        let corner_after = px(&vign, 64, 1, 62).luma();
        let centre_before = px(&plain, 64, 32, 32).luma();
        let centre_after = px(&vign, 64, 32, 32).luma();
        assert!(
            corner_after < corner_before * 0.5,
            "the corner is strongly darkened: {corner_before} → {corner_after}",
        );
        assert!(
            (centre_after - centre_before).abs() < 0.02,
            "the centre is left alone: {centre_before} → {centre_after}",
        );
    }

    /// Two abutting clips of very different colours on one track, with
    /// `kind` as the incoming clip's transition.
    fn two_clip_project(kind: Transition) -> (Project, f64) {
        let mut p = Project::new("t");
        let a = media(&mut p, 2); // green
        let b = media(&mut p, 5); // red
        let v1 = track(&mut p, 1);
        let ca = p.add_clip(v1, a, 0.0).unwrap();
        p.clip_mut(ca).unwrap().out_point = 5.0;
        let cb = p.add_clip(v1, b, 5.0).unwrap();
        p.clip_mut(cb).unwrap().out_point = 5.0;
        p.clip_mut(cb).unwrap().transition = kind;
        // Halfway through the incoming clip's transition.
        (p, 5.0 + crate::preview::TRANSITION_SECS * 0.5)
    }

    #[test]
    fn each_transition_renders_a_different_frame() {
        let mut c = compositor();
        let mut shots = Vec::new();
        for kind in [
            Transition::CrossDissolve,
            Transition::FadeIn,
            Transition::DipToBlack,
            Transition::Wipe,
        ] {
            let (p, t) = two_clip_project(kind);
            shots.push((kind, c.frame(&p, t, 48, 48, &mut NoSources)));
        }
        for i in 0..shots.len() {
            for j in (i + 1)..shots.len() {
                let differing = shots[i]
                    .1
                    .chunks(4)
                    .zip(shots[j].1.chunks(4))
                    .filter(|(x, y)| {
                        x[0].abs_diff(y[0]) > 6
                            || x[1].abs_diff(y[1]) > 6
                            || x[2].abs_diff(y[2]) > 6
                    })
                    .count();
                assert!(
                    differing > 48 * 48 / 20,
                    "{:?} and {:?} must not render the same frame ({differing} px differ)",
                    shots[i].0,
                    shots[j].0,
                );
            }
        }
    }

    #[test]
    fn a_dissolve_samples_both_clips() {
        // The whole point: mid-dissolve the frame carries the outgoing
        // clip's colour AND the incoming one's, which a single-clip
        // compositor can never produce.
        let (p, t) = two_clip_project(Transition::CrossDissolve);
        let mut c = compositor();
        let mid = px(&c.frame(&p, t, 32, 32, &mut NoSources), 32, 16, 20);
        // Green outgoing, red incoming: halfway both channels are lit.
        assert!(
            mid.r > 0.15 && mid.g > 0.15,
            "mid-dissolve carries both the outgoing green and incoming red: {mid:?}",
        );
        // ...and it is genuinely between the two ends.
        let start = px(&c.frame(&p, 5.001, 32, 32, &mut NoSources), 32, 16, 20);
        let end = px(&c.frame(&p, 5.0 + crate::preview::TRANSITION_SECS + 0.01, 32, 32, &mut NoSources), 32, 16, 20);
        assert!(start.g > start.r, "the dissolve opens on the outgoing clip: {start:?}");
        assert!(end.r > end.g, "and lands on the incoming clip: {end:?}");
        assert!(mid.r > start.r && mid.g > end.g, "the middle is a real blend: {mid:?}");
    }

    #[test]
    fn a_fade_in_rises_out_of_black_without_the_previous_clip() {
        let (p, _) = two_clip_project(Transition::FadeIn);
        let mut c = compositor();
        let early = px(&c.frame(&p, 5.02, 32, 32, &mut NoSources), 32, 16, 20);
        let late = px(
            &c.frame(&p, 5.0 + crate::preview::TRANSITION_SECS * 0.9, 32, 32, &mut NoSources),
            32,
            16,
            20,
        );
        assert!(early.luma() < 0.06, "a fade opens on black, not the previous clip: {early:?}");
        assert!(late.luma() > early.luma() + 0.1, "and rises out of it: {late:?}");
        assert!(late.r > late.g, "what rises is the incoming clip: {late:?}");
    }

    #[test]
    fn a_dip_passes_through_black() {
        let (p, _) = two_clip_project(Transition::DipToBlack);
        let mut c = compositor();
        let dur = crate::preview::TRANSITION_SECS;
        let quarter = px(&c.frame(&p, 5.0 + dur * 0.25, 32, 32, &mut NoSources), 32, 16, 20);
        let middle = px(&c.frame(&p, 5.0 + dur * 0.5, 32, 32, &mut NoSources), 32, 16, 20);
        let three_q = px(&c.frame(&p, 5.0 + dur * 0.75, 32, 32, &mut NoSources), 32, 16, 20);
        assert!(middle.luma() < 0.03, "the dip bottoms out at black: {middle:?}");
        assert!(quarter.g > quarter.r, "it leaves on the outgoing clip: {quarter:?}");
        assert!(three_q.r > three_q.g, "and returns on the incoming one: {three_q:?}");
    }

    #[test]
    fn a_wipe_is_geometry_not_a_flash() {
        let (p, t) = two_clip_project(Transition::Wipe);
        let mut c = compositor();
        let buf = c.frame(&p, t, 64, 64, &mut NoSources);
        // Halfway through a left-to-right wipe: the left side is the
        // incoming clip, the right side is still the outgoing one.
        let left = px(&buf, 64, 6, 30);
        let right = px(&buf, 64, 57, 30);
        assert!(left.r > left.g, "the revealed side shows the incoming clip: {left:?}");
        assert!(right.g > right.r, "the unreached side still shows the outgoing one: {right:?}");
        // A soft edge: somewhere near the middle the two are mixed.
        let edge = px(&buf, 64, 32, 30);
        assert!(
            edge.r > 0.1 && edge.g > 0.1,
            "the wipe edge is feathered, not a hard step: {edge:?}",
        );
    }

    #[test]
    fn a_resolved_source_texture_replaces_the_representative_fill() {
        // Proof the injected source is what gets composited: a source that
        // hands back a flat magenta picture must show up as magenta, not
        // as the clip's palette colour.
        let mut p = Project::new("t");
        let m = media(&mut p, 2); // green palette
        let t = track(&mut p, 1);
        let cid = p.add_clip(t, m, 0.0).unwrap();
        p.clip_mut(cid).unwrap().out_point = 10.0;

        let mut c = compositor();
        let mut src = CpuFrames::new(|_media, _time| {
            Some(CpuFrame {
                width: 4,
                height: 4,
                rgba: Arc::new(vec![255, 0, 255, 255].repeat(16)),
                identity: 0,
            })
        });
        // A 4×4 (1:1) source in a 16:9-ish frame letterboxes, so sample
        // the centre where the picture actually lands.
        let got = px(&c.frame(&p, 1.0, 32, 32, &mut src), 32, 16, 16);
        assert!(
            got.r > 0.8 && got.b > 0.8 && got.g < 0.2,
            "the injected picture is what got composited, got {got:?}",
        );
    }

    #[test]
    fn a_source_that_yields_nothing_falls_back_to_the_representative_fill() {
        let mut p = Project::new("t");
        let m = media(&mut p, 3);
        let t = track(&mut p, 1);
        let cid = p.add_clip(t, m, 0.0).unwrap();
        p.clip_mut(cid).unwrap().out_point = 10.0;
        let mut c = compositor();
        let mut empty = CpuFrames::new(|_m, _t| None);
        let with_none = c.frame(&p, 1.0, 32, 32, &mut empty);
        let hermetic = c.frame(&p, 1.0, 32, 32, &mut NoSources);
        assert_eq!(
            with_none, hermetic,
            "an un-hydrated source renders exactly the hermetic frame",
        );
    }

    #[test]
    fn the_same_input_always_composites_the_same_frame() {
        // `--shots` determinism, asserted on the production path.
        let mut p = Project::new("t");
        let m = media(&mut p, 4);
        let t = track(&mut p, 1);
        let cid = p.add_clip(t, m, 0.0).unwrap();
        p.clip_mut(cid).unwrap().out_point = 10.0;
        p.clip_mut(cid).unwrap().effects = vec![
            ClipEffect { kind: EffectKind::Sepia, amount: 0.7 },
            ClipEffect { kind: EffectKind::Vignette, amount: 0.5 },
        ];
        let mut c = compositor();
        let a = c.frame(&p, 3.25, 48, 48, &mut NoSources);
        let b = c.frame(&p, 3.25, 48, 48, &mut NoSources);
        assert_eq!(a, b, "the composite is byte-for-byte reproducible");
    }

    #[test]
    fn effects_apply_in_stack_order() {
        let mut p = Project::new("t");
        let m = media(&mut p, 5);
        let t = track(&mut p, 1);
        let cid = p.add_clip(t, m, 0.0).unwrap();
        p.clip_mut(cid).unwrap().out_point = 10.0;
        let mut c = compositor();

        // Sepia last tints a neutral grey warm; sepia first is flattened
        // back to grey by the grayscale that follows it.
        p.clip_mut(cid).unwrap().effects = vec![
            ClipEffect { kind: EffectKind::Grayscale, amount: 1.0 },
            ClipEffect { kind: EffectKind::Sepia, amount: 1.0 },
        ];
        let gray_then_sepia = px(&c.frame(&p, 1.0, 32, 32, &mut NoSources), 32, 16, 20);
        p.clip_mut(cid).unwrap().effects.swap(0, 1);
        let sepia_then_gray = px(&c.frame(&p, 1.0, 32, 32, &mut NoSources), 32, 16, 20);
        assert!(
            gray_then_sepia.r - gray_then_sepia.b > 0.08,
            "sepia applied last leaves a warm cast: {gray_then_sepia:?}",
        );
        assert!(
            (sepia_then_gray.r - sepia_then_gray.b).abs() < 0.03,
            "grayscale applied last flattens the sepia back out: {sepia_then_gray:?}",
        );
        assert_ne!(
            gray_then_sepia, sepia_then_gray,
            "swapping the stack changes the composited result",
        );
    }

    #[test]
    fn the_fade_envelope_dims_the_composite() {
        let mut p = Project::new("t");
        let m = media(&mut p, 3);
        let t = track(&mut p, 1);
        let cid = p.add_clip(t, m, 0.0).unwrap();
        {
            let c = p.clip_mut(cid).unwrap();
            c.out_point = 10.0;
            c.fade_in = 2.0;
        }
        let mut c = compositor();
        let head = px(&c.frame(&p, 0.05, 32, 32, &mut NoSources), 32, 16, 20).luma();
        let mid = px(&c.frame(&p, 5.0, 32, 32, &mut NoSources), 32, 16, 20).luma();
        assert!(head < mid * 0.2, "the head of a fade-in is near black: {head} vs {mid}");
    }


    /// The bundled demo clip, if this checkout has it.
    #[cfg(feature = "decode")]
    fn demo_clip() -> std::path::PathBuf {
        if let Some(p) = std::env::var_os("QOS_VE_TEST_CLIP") {
            let p = std::path::PathBuf::from(p);
            assert!(p.exists(), "$QOS_VE_TEST_CLIP does not exist: {}", p.display());
            return p;
        }
        let mut dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        for _ in 0..5 {
            let candidate = dir.join(".claude/demo-artifacts/qos-demo-57s.mp4");
            if candidate.exists() {
                return candidate;
            }
            if !dir.pop() {
                break;
            }
        }
        panic!(
            "this test needs real footage at .claude/demo-artifacts/qos-demo-57s.mp4, \
             or $QOS_VE_TEST_CLIP pointing at a clip — it cannot verify decode without it",
        );
    }

    /// Mean horizontal neighbour variation. The representative fill is
    /// built from horizontal bands, so it is constant along every row and
    /// this collapses to zero; a real decoded picture never does. It is
    /// the one measurement that tells "showing the footage" apart from
    /// "showing a coloured rectangle".
    fn horizontal_detail(buf: &[u8], w: u32, h: u32) -> f64 {
        let mut total = 0.0;
        for y in 0..h as usize {
            for x in 1..w as usize {
                let a = (y * w as usize + x - 1) * 4;
                let b = (y * w as usize + x) * 4;
                for c in 0..3 {
                    total += buf[a + c].abs_diff(buf[b + c]) as f64;
                }
            }
        }
        total / (w * h) as f64
    }

    /// With a hydrated real source under the playhead, the canvas must
    /// show the decoded picture — not the representative fallback.
    ///
    /// This is the failure mode a screenshot catches and a unit test
    /// usually does not: the editor still renders, still looks plausible,
    /// and is showing the user a coloured rectangle instead of their
    /// footage. It drives the real decoder and asserts on what the
    /// production compositor puts on the canvas.
    #[cfg(feature = "decode")]
    #[test]
    #[serial_test::serial(video_editor_gst)]
    fn a_hydrated_source_puts_real_footage_on_the_canvas() {
        use crate::model::{MediaItem, MediaKind, Track, TrackKind};

        let path = demo_clip();
        let mut p = Project::new("live");
        p.width = 1920;
        p.height = 1080;
        let mid = Uuid::new_v4();
        p.media.push(MediaItem {
            id: mid,
            name: "demo".into(),
            kind: MediaKind::Video,
            duration: 57.0,
            width: 1920,
            height: 1080,
            fps: 30.0,
            palette: 0,
            path: path.to_string_lossy().into_owned(),
            hydration: crate::media::Hydration::Local,
            ..Default::default()
        });
        let tid = Uuid::new_v4();
        p.tracks.push(Track {
            id: tid,
            name: "V1".into(),
            kind: TrackKind::Video,
            index: 1,
            muted: false,
            locked: false, solo: false,
        });
        let cid = p.add_clip(tid, mid, 0.0).unwrap();
        p.clip_mut(cid).unwrap().out_point = 57.0;

        // Drive the real decoder the way the app's warm-up does: pump until
        // a frame for the clip under the playhead lands, or give up loudly.
        let playhead = 22.0;
        let mut decoder = crate::preview::PreviewDecoder::new();
        assert!(
            decoder.is_real_decode(),
            "the decode feature must stand up a real pipeline, not the synthesized fallback",
        );
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
        let mut ready = false;
        while std::time::Instant::now() < deadline {
            if decoder.update(&p, playhead) {
                ready = true;
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        assert!(
            ready,
            "the decoder produced no frame for a hydrated local file — the canvas would \
             fall back to the representative fill and show a coloured rectangle",
        );

        // Hand that frame to the compositor exactly as the canvas does.
        let frame = decoder.frame().expect("decoded frame").clone();
        let decoded_media = decoder.frame_media();
        assert_eq!(decoded_media, Some(mid), "the frame belongs to the clip under the playhead");
        let mut sources = CpuFrames::new(move |media, _t| {
            (media == mid).then(|| CpuFrame {
                width: frame.width,
                height: frame.height,
                rgba: frame.rgba.clone(),
                identity: frame.pts_ms,
            })
        });

        let mut c = compositor();
        let (w, h) = (192u32, 108u32);
        let live = c.frame(&p, playhead, w, h, &mut sources);
        let fallback = c.frame(&p, playhead, w, h, &mut NoSources);

        assert_ne!(
            live, fallback,
            "the composite with a decoded frame must differ from the representative fallback",
        );
        let live_detail = horizontal_detail(&live, w, h);
        let fallback_detail = horizontal_detail(&fallback, w, h);
        println!(
            "canvas horizontal detail: decoded={live_detail:.3}  fallback={fallback_detail:.3}",
        );
        assert!(
            fallback_detail < 0.5,
            "sanity: the representative fill is flat along its rows ({fallback_detail:.3})",
        );
        assert!(
            live_detail > 2.0,
            "the canvas is showing a flat fill ({live_detail:.3}), not decoded footage",
        );
    }


    /// The compositor painting a real hardware-decoded frame.
    ///
    /// This is the whole point of the media seam: a decoder emits NV12, the
    /// planes go to the GPU untouched, and the YUV arithmetic happens in a
    /// shader. If the planar branch ever broke — or quietly fell back to a
    /// CPU RGBA convert — the picture would still appear and nothing else
    /// would complain, so assert on what reaches the canvas *and* on which
    /// payload carried it.
    #[cfg(feature = "decode")]
    #[test]
    #[serial_test::serial(video_editor_gst)]
    fn the_media_source_paints_a_decoded_frame_through_the_gpu() {
        use crate::model::{MediaItem, MediaKind, Track, TrackKind};
        use media::{FramePayload, SessionPool};

        let path = demo_clip();
        let mut p = Project::new("hw");
        p.width = 1920;
        p.height = 1080;
        let mid = Uuid::new_v4();
        p.media.push(MediaItem {
            id: mid,
            name: "demo".into(),
            kind: MediaKind::Video,
            duration: 57.0,
            width: 1920,
            height: 1080,
            fps: 30.0,
            palette: 0,
            path: path.to_string_lossy().into_owned(),
            hydration: crate::media::Hydration::Local,
            ..Default::default()
        });
        let tid = Uuid::new_v4();
        p.tracks.push(Track {
            id: tid,
            name: "V1".into(),
            kind: TrackKind::Video,
            index: 1,
            muted: false,
            locked: false,
            solo: false,
        });
        let cid = p.add_clip(tid, mid, 0.0).unwrap();
        p.clip_mut(cid).unwrap().out_point = 57.0;

        // A generous deadline: this box shares its CPU with other builds,
        // and a cold pipeline plus an accurate seek can outrun the 2s the
        // engine defaults to. The assertions below are unchanged — only how
        // long the decoder is given to answer.
        let mut src = MediaFrameSource::new(&p, Fetch::Exact)
            .with_timeout(std::time::Duration::from_secs(20));
        assert_eq!(src.decodable(), 1, "the hydrated local file is decodable");

        let mut c = compositor();
        let (w, h) = (192u32, 108u32);
        let live = c.frame(&p, 22.0, w, h, &mut src);
        let fallback = c.frame(&p, 22.0, w, h, &mut NoSources);

        assert_ne!(live, fallback, "the decoded frame must reach the canvas");
        let live_detail = horizontal_detail(&live, w, h);
        let report = src.hw_report(mid).expect("the session reports what it plugged");
        println!("decoder={report:?}  canvas detail={live_detail:.3}");
        assert!(
            live_detail > 2.0,
            "the canvas is showing a flat fill ({live_detail:.3}), not decoded footage",
        );

        // Which payload actually carried it. On a hardware decoder that is
        // NV12 planes; software decode lands as RGBA. A Dmabuf would mean the
        // source should have been demoted to a software session instead.
        let source = crate::media::engine_source(&p.media[0].source().abs_path);
        let mut pool = SessionPool::for_editor();
        let session = pool.session(&source);
        let mut s = session.lock().unwrap();
        s.set_fps(30.0);
        let frame = s
            .frame_at(22_000, std::time::Duration::from_secs(10))
            .expect("a frame at 22s");
        match &frame.payload {
            FramePayload::Planar(img) => {
                assert_eq!(img.planes.len(), 2, "NV12 is luma plus interleaved chroma");
                // The planes really are subsampled — a CPU-converted RGBA
                // frame masquerading as planar would not be.
                let luma = &img.planes[0];
                let chroma = &img.planes[1];
                assert!(
                    chroma.width <= luma.width.div_ceil(2)
                        && chroma.height <= luma.height.div_ceil(2),
                    "chroma is half resolution in both axes: {luma:?} {chroma:?}",
                );
            }
            FramePayload::Rgba(bytes) => {
                assert_eq!(
                    bytes.len(),
                    frame.width as usize * frame.height as usize * 4,
                    "the software fallback delivers a full RGBA frame",
                );
            }
            FramePayload::Dmabuf(_) => panic!(
                "a GPU-resident frame reached the paint path; it should have been \
                 demoted to a software session (this stack cannot import one)",
            ),
        }
    }

    /// Two clips of the same source at different instants must paint two
    /// different pictures in one composite.
    ///
    /// The converter reuses its output texture across frames, so a single
    /// shared converter would have the second layer overwrite the first and
    /// both would show the same picture — invisible in a still, and exactly
    /// the kind of thing a dissolve depends on.
    #[cfg(feature = "decode")]
    #[test]
    #[serial_test::serial(video_editor_gst)]
    fn two_layers_of_one_source_get_their_own_textures() {
        use crate::model::{MediaItem, MediaKind, Track, TrackKind};

        let path = demo_clip();
        let mut p = Project::new("two");
        let mid = Uuid::new_v4();
        p.media.push(MediaItem {
            id: mid,
            name: "demo".into(),
            kind: MediaKind::Video,
            duration: 57.0,
            width: 1920,
            height: 1080,
            fps: 30.0,
            palette: 0,
            path: path.to_string_lossy().into_owned(),
            hydration: crate::media::Hydration::Local,
            ..Default::default()
        });
        let mut track = |index: u32| {
            let id = Uuid::new_v4();
            p.tracks.push(Track {
                id,
                name: format!("V{index}"),
                kind: TrackKind::Video,
                index,
                muted: false,
                locked: false,
                solo: false,
            });
            id
        };
        let v1 = track(1);
        let v2 = track(2);
        // Two instants far enough apart to be different pictures, and the
        // upper layer inverted so a shared texture is unmistakable.
        let lower = p.add_clip(v1, mid, 0.0).unwrap();
        {
            let c = p.clip_mut(lower).unwrap();
            c.in_point = 2.0;
            c.out_point = 12.0;
        }
        let upper = p.add_clip(v2, mid, 0.0).unwrap();
        {
            let c = p.clip_mut(upper).unwrap();
            c.in_point = 40.0;
            c.out_point = 50.0;
            c.opacity = 0.5;
            c.effects = vec![ClipEffect { kind: EffectKind::Invert, amount: 1.0 }];
        }

        let mut src = MediaFrameSource::new(&p, Fetch::Exact)
            .with_timeout(std::time::Duration::from_secs(20));
        let mut c = compositor();
        let both = c.frame(&p, 1.0, 96, 54, &mut src);

        // With the upper layer half-transparent over the lower one, the
        // result must be neither layer alone.
        let mut only_lower = p.clone();
        only_lower.tracks.iter_mut().find(|t| t.id == v2).unwrap().muted = true;
        let lower_only = c.frame(&only_lower, 1.0, 96, 54, &mut src);

        let mut only_upper = p.clone();
        only_upper.tracks.iter_mut().find(|t| t.id == v1).unwrap().muted = true;
        only_upper.clip_mut(upper).unwrap().opacity = 1.0;
        let upper_only = c.frame(&only_upper, 1.0, 96, 54, &mut src);

        assert_ne!(both, lower_only, "the upper layer contributes to the composite");
        assert_ne!(both, upper_only, "so does the lower one");
        assert_ne!(
            lower_only, upper_only,
            "the two layers are drawn from different instants of the source — a shared \
             converter texture would make them identical",
        );
    }

    #[test]
    fn clip_layer_reads_the_clip_at_its_own_source_time() {
        // Two clips of the same media at different in-points must ask the
        // source for different instants — otherwise trimming would be
        // invisible in the composite.
        let mut p = Project::new("t");
        let m = media(&mut p, 1);
        let v1 = track(&mut p, 1);
        let a = p.add_clip(v1, m, 0.0).unwrap();
        {
            let c = p.clip_mut(a).unwrap();
            c.in_point = 4.0;
            c.out_point = 9.0;
        }
        let asked = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let sink = asked.clone();
        let mut src = CpuFrames::new(move |_m, t| {
            sink.lock().unwrap().push(t);
            None
        });
        let mut c = compositor();
        let _ = c.frame(&p, 2.0, 16, 16, &mut src);
        let seen = asked.lock().unwrap().clone();
        assert_eq!(seen.len(), 1, "one layer, one source request: {seen:?}");
        assert!(
            (seen[0] - 6.0).abs() < 1e-9,
            "playhead 2s into a clip cut at 4s asks the source for 6s, got {seen:?}",
        );
    }
}
