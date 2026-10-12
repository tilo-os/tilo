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
pub mod title;

use std::collections::HashMap;
use std::sync::Arc;

use vello::wgpu;

use crate::model::MediaId;

pub use compositor::{MAX_SHADER_EFFECTS, VideoCompositor, Viewport, upload_rgba};
#[cfg(feature = "decode")]
pub use media_source::{Fetch, MediaFrameSource};
pub use title::TitleRaster;

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
        Self {
            produce,
            cache: HashMap::new(),
        }
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
    ///
    /// # Errors
    ///
    /// The adapter's own message when no usable GPU is available.
    pub fn new() -> Result<Self, String> {
        let gpu = ui::offscreen::shared_gpu().map_err(|e| e.to_string())?;
        Ok(Self {
            gpu,
            compositor: VideoCompositor::new(),
        })
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
        Transform, Transition,
    };
    use crate::preview::{Rgb, palette_rgb};
    use uuid::Uuid;

    #[cfg(feature = "decode")]
    mod generated_clip {
        include!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/support/generated_clip.rs"
        ));
    }

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
            f64::from(buf[i]) / 255.0,
            f64::from(buf[i + 1]) / 255.0,
            f64::from(buf[i + 2]) / 255.0,
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
            locked: false,
            solo: false,
            gain_db: 0.0,
            pan: 0.0,
        });
        id
    }

    /// A [`FrameSource`] handing every layer the same known picture, so a
    /// test can assert on *where the picture landed* rather than on a
    /// stand-in fill. `paint(u, v) -> (r, g, b, a)` in 0..255.
    #[expect(
        clippy::many_single_char_names,
        reason = "pixel maths: frame w/h, texel index i, its u/v, and the colour c"
    )]
    fn painted(
        w: u32,
        h: u32,
        paint: impl Fn(f64, f64) -> [u8; 4] + 'static,
    ) -> CpuFrames<impl FnMut(Uuid, f64) -> Option<CpuFrame>> {
        let mut rgba = vec![0u8; (w * h * 4) as usize];
        for y in 0..h {
            for x in 0..w {
                let u = (f64::from(x) + 0.5) / f64::from(w);
                let v = (f64::from(y) + 0.5) / f64::from(h);
                let c = paint(u, v);
                let i = ((y * w + x) * 4) as usize;
                rgba[i..i + 4].copy_from_slice(&c);
            }
        }
        let shared = std::sync::Arc::new(rgba);
        CpuFrames::new(move |_media, _t| {
            Some(CpuFrame {
                width: w,
                height: h,
                rgba: shared.clone(),
                identity: 0,
            })
        })
    }

    /// A flat mid-green picture, square, so a transform's effect on it is
    /// unambiguous: green means "the clip is here", black means "it is not".
    fn green() -> CpuFrames<impl FnMut(Uuid, f64) -> Option<CpuFrame>> {
        painted(32, 32, |_u, _v| [40, 200, 90, 255])
    }

    fn is_green(c: Rgb) -> bool {
        c.g > 0.5 && c.g > c.r + 0.2 && c.g > c.b + 0.2
    }
    fn is_black(c: Rgb) -> bool {
        c.luma() < 0.06
    }

    /// A one-clip project whose media is square, so the identity transform
    /// aspect-fits it to the full height of a square frame.
    fn square_clip() -> (Project, Uuid) {
        let mut p = Project::new("t");
        let mid = Uuid::new_v4();
        p.media.push(MediaItem {
            id: mid,
            name: "sq".into(),
            kind: MediaKind::Video,
            duration: 30.0,
            width: 32,
            height: 32,
            fps: 30.0,
            palette: 2,
            path: crate::media::workspace_path("sq.mp4"),
            ..Default::default()
        });
        let t = track(&mut p, 1);
        let cid = p.add_clip(t, mid, 0.0).unwrap();
        p.clip_mut(cid).unwrap().out_point = 10.0;
        (p, cid)
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "merged scenario: each former test is a labelled case block with its own fixture"
    )]
    fn layer_transforms_scale_move_rotate_and_crop() {
        // Case `the_identity_transform_still_fills_the_frame`.
        {
            // The whole geometry feature has to be free at rest: an untouched
            // clip must composite exactly as it did before there was a
            // transform at all.
            let (p, _) = square_clip();
            let mut c = compositor();
            let buf = c.frame(&p, 1.0, 64, 64, &mut green());
            for (x, y) in [(2, 2), (32, 32), (61, 61), (2, 61)] {
                assert!(
                    is_green(px(&buf, 64, x, y)),
                    "identity fills the frame; ({x},{y}) was {:?}",
                    px(&buf, 64, x, y),
                );
            }
        }
        // Case `scaling_down_shrinks_the_picture_and_leaves_black_around_it`.
        {
            let (mut p, cid) = square_clip();
            p.clip_mut(cid).unwrap().transform = Transform {
                scale_x: 0.5,
                scale_y: 0.5,
                ..Transform::default()
            };
            let mut c = compositor();
            let buf = c.frame(&p, 1.0, 64, 64, &mut green());
            // Half scale on a square source in a square frame: the picture
            // occupies the middle half, x and y in 16..48.
            assert!(
                is_green(px(&buf, 64, 32, 32)),
                "the centre still has picture"
            );
            assert!(is_green(px(&buf, 64, 20, 32)), "just inside the left edge");
            assert!(is_black(px(&buf, 64, 8, 32)), "well outside is black");
            assert!(is_black(px(&buf, 64, 32, 4)), "and above it");
            assert!(is_black(px(&buf, 64, 60, 60)), "and the corner");
        }
        // Case `position_moves_the_picture_by_frame_widths`.
        {
            let (mut p, cid) = square_clip();
            // A quarter-size clip pushed a quarter of the frame to the right
            // and a quarter up.
            p.clip_mut(cid).unwrap().transform = Transform {
                scale_x: 0.25,
                scale_y: 0.25,
                pos_x: 0.25,
                pos_y: -0.25,
                ..Transform::default()
            };
            let mut c = compositor();
            let buf = c.frame(&p, 1.0, 64, 64, &mut green());
            // Centre of the clip: 0.5 + 0.25 = 0.75 across, 0.5 - 0.25 = 0.25
            // down → pixel (48, 16).
            assert!(
                is_green(px(&buf, 64, 48, 16)),
                "the clip sits up and to the right; got {:?}",
                px(&buf, 64, 48, 16),
            );
            assert!(
                is_black(px(&buf, 64, 32, 32)),
                "and no longer at the centre"
            );
            assert!(is_black(px(&buf, 64, 16, 48)), "nor opposite it");
        }
        // Case `a_corner_pip_lands_in_that_corner_and_only_there`.
        {
            let (mut p, cid) = square_clip();
            // Bottom-left, a fifth of the frame, with a small margin.
            p.clip_mut(cid).unwrap().transform = Transform::pip(0.2, (-1.0, 1.0), 0.03);
            let mut c = compositor();
            let buf = c.frame(&p, 1.0, 64, 64, &mut green());
            // pos = -(0.5 - 0.1 - 0.03) = -0.37 → centre at 0.13 across, 0.87
            // down → pixel (8, 55).
            assert!(
                is_green(px(&buf, 64, 8, 55)),
                "the inset sits bottom-left; got {:?}",
                px(&buf, 64, 8, 55),
            );
            for (x, y) in [(32, 32), (55, 8), (55, 55), (8, 8)] {
                assert!(
                    is_black(px(&buf, 64, x, y)),
                    "a picture-in-picture covers only its corner; ({x},{y}) was {:?}",
                    px(&buf, 64, x, y),
                );
            }
        }
        // Case `rotating_a_quarter_turn_moves_the_source_left_half_to_the_top`.
        {
            // Half red, half blue, split down the middle. A clockwise quarter
            // turn must put the left (red) half on top.
            let (mut p, cid) = square_clip();
            let mut src = painted(32, 32, |u, _v| {
                if u < 0.5 {
                    [220, 40, 40, 255]
                } else {
                    [40, 40, 220, 255]
                }
            });
            let mut c = compositor();
            let flat = c.frame(&p, 1.0, 64, 64, &mut src);
            let left = px(&flat, 64, 12, 32);
            assert!(
                left.r > left.b + 0.2,
                "unrotated, red is on the left: {left:?}"
            );

            p.clip_mut(cid).unwrap().transform = Transform {
                rotation: 90.0,
                ..Transform::default()
            };
            let turned = c.frame(&p, 1.0, 64, 64, &mut src);
            let top = px(&turned, 64, 32, 12);
            let bottom = px(&turned, 64, 32, 52);
            assert!(
                top.r > top.b + 0.2,
                "a clockwise quarter turn puts the source's left half on top: {top:?}",
            );
            assert!(
                bottom.b > bottom.r + 0.2,
                "and its right half at the bottom: {bottom:?}",
            );
        }
        // Case `cropping_cuts_the_edge_off_without_moving_what_remains`.
        {
            let (mut p, cid) = square_clip();
            let mut c = compositor();
            let full = c.frame(&p, 1.0, 64, 64, &mut green());
            assert!(
                is_green(px(&full, 64, 4, 32)),
                "uncropped, the left edge has picture"
            );

            // Cut the left third away.
            p.clip_mut(cid).unwrap().transform = Transform {
                crop_left: 1.0 / 3.0,
                ..Transform::default()
            };
            let cropped = c.frame(&p, 1.0, 64, 64, &mut green());
            assert!(
                is_black(px(&cropped, 64, 4, 32)),
                "the cropped-away third is gone; got {:?}",
                px(&cropped, 64, 4, 32),
            );
            assert!(
                is_green(px(&cropped, 64, 32, 32)),
                "what is left of the picture has not moved",
            );
            assert!(
                is_green(px(&cropped, 64, 60, 32)),
                "including its right edge"
            );
        }
        // Case `a_transformed_layer_lets_the_one_beneath_it_show_through`.
        {
            // The point of geometry on a layer stack: an inset clip must reveal
            // what is under it, not letterbox it away.
            let mut p = Project::new("t");
            let under = Uuid::new_v4();
            let over = Uuid::new_v4();
            for (id, pal) in [(under, 0u8), (over, 3u8)] {
                p.media.push(MediaItem {
                    id,
                    name: "m".into(),
                    kind: MediaKind::Video,
                    duration: 30.0,
                    width: 32,
                    height: 32,
                    fps: 30.0,
                    palette: pal,
                    path: crate::media::workspace_path("m.mp4"),
                    ..Default::default()
                });
            }
            let v1 = track(&mut p, 1);
            let v2 = track(&mut p, 2);
            let lower = p.add_clip(v1, under, 0.0).unwrap();
            let upper = p.add_clip(v2, over, 0.0).unwrap();
            p.clip_mut(lower).unwrap().out_point = 10.0;
            p.clip_mut(upper).unwrap().out_point = 10.0;
            p.clip_mut(upper).unwrap().transform = Transform::pip(0.3, (1.0, -1.0), 0.04);

            let mut c = compositor();
            // No sources at all: both layers draw their representative fill, so
            // this also proves the stand-in respects the transform — a
            // picture-in-picture reads as one in a hermetic capture.
            let buf = c.frame(&p, 1.0, 64, 64, &mut NoSources);
            let centre = px(&buf, 64, 32, 40);
            let inset = px(&buf, 64, 51, 13);
            let lower_pal = palette_rgb(0);
            let upper_pal = palette_rgb(3);
            // Compare which palette each region is nearer to.
            let near = |c: Rgb, a: Rgb, b: Rgb| {
                let d = |x: Rgb, y: Rgb| (x.r - y.r).abs() + (x.g - y.g).abs() + (x.b - y.b).abs();
                d(c, a) < d(c, b)
            };
            assert!(
                near(centre, lower_pal, upper_pal),
                "the centre shows the lower clip, not the inset: {centre:?}",
            );
            assert!(
                near(inset, upper_pal, lower_pal),
                "the inset corner shows the upper clip: {inset:?}",
            );
        }
        // Case `a_source_with_alpha_is_cut_by_it_and_video_never_is`.
        {
            // An image's transparent margin must let the layer beneath through;
            // a decoded video frame that happens to carry zero alpha must not
            // vanish. (A title is not used here: its picture is rasterised by
            // the compositor, so it never reaches the frame source at all.)
            let mut p = Project::new("t");
            let mid = Uuid::new_v4();
            p.media.push(MediaItem {
                id: mid,
                name: "logo.png".into(),
                kind: MediaKind::Image,
                duration: 10.0,
                width: 32,
                height: 32,
                fps: 30.0,
                palette: 1,
                path: crate::media::workspace_path("logo.png"),
                ..Default::default()
            });
            let t = track(&mut p, 1);
            let cid = p.add_clip(t, mid, 0.0).unwrap();
            p.clip_mut(cid).unwrap().out_point = 5.0;

            // Opaque green in the left half, fully transparent in the right.
            let mut src = painted(32, 32, |u, _v| {
                if u < 0.5 {
                    [40, 200, 90, 255]
                } else {
                    [40, 200, 90, 0]
                }
            });
            let mut c = compositor();
            let buf = c.frame(&p, 1.0, 64, 64, &mut src);
            assert!(is_green(px(&buf, 64, 16, 32)), "the opaque half draws");
            assert!(
                is_black(px(&buf, 64, 48, 32)),
                "the transparent half does not; got {:?}",
                px(&buf, 64, 48, 32),
            );

            // The same picture on a video source keeps its transparent half,
            // because video alpha is not honoured.
            p.media[0].kind = MediaKind::Video;
            let buf = c.frame(&p, 1.0, 64, 64, &mut src);
            assert!(
                is_green(px(&buf, 64, 48, 32)),
                "a video frame is opaque whatever its alpha channel says; got {:?}",
                px(&buf, 64, 48, 32),
            );
        }
    }

    /// Place a title over `p` on track index `index`, returning its clip.
    fn title_clip(p: &mut Project, index: u32, text: crate::model::TextSource) -> Uuid {
        let mid = Uuid::new_v4();
        p.media.push(MediaItem {
            id: mid,
            name: "Title".into(),
            kind: MediaKind::Text,
            duration: 6.0,
            width: 0,
            height: 0,
            fps: 30.0,
            palette: 4,
            // A title has no bytes: nothing to hydrate, nothing to decode.
            path: String::new(),
            text: Some(text),
            ..Default::default()
        });
        let t = track(p, index);
        let cid = p.add_clip(t, mid, 0.0).unwrap();
        p.clip_mut(cid).unwrap().out_point = 6.0;
        cid
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "merged scenario: each former test is a labelled case block with its own fixture"
    )]
    fn titles_composite_as_placed_glyph_layers() {
        // Case `a_title_draws_its_glyphs_over_the_clip_beneath_it`.
        {
            let mut p = Project::new("t");
            // A video bed on V1 …
            let bed = media(&mut p, 0);
            let v1 = track(&mut p, 1);
            let under = p.add_clip(v1, bed, 0.0).unwrap();
            p.clip_mut(under).unwrap().out_point = 6.0;
            // … and a big white title over it on V2.
            title_clip(
                &mut p,
                2,
                crate::model::TextSource {
                    content: "IIIIIIII".into(),
                    size: 0.7,
                    color: [1.0, 1.0, 1.0, 1.0],
                    ..crate::model::TextSource::default()
                },
            );

            let mut c = compositor();
            let buf = c.frame(&p, 1.0, 96, 96, &mut green());
            let white = buf
                .chunks_exact(4)
                .filter(|q| q[0] > 235 && q[1] > 235 && q[2] > 235)
                .count();
            assert!(
                white > 40,
                "the title's glyphs reach the composite as white pixels, got {white}",
            );
            // And the bed is still visible where the glyphs are not: a title is
            // a layer with alpha, not an opaque card.
            let bed_px = buf
                .chunks_exact(4)
                .filter(|q| {
                    is_green(Rgb::new(
                        f64::from(q[0]) / 255.0,
                        f64::from(q[1]) / 255.0,
                        f64::from(q[2]) / 255.0,
                    ))
                })
                .count();
            assert!(
                bed_px > 3000,
                "the clip beneath shows through the title's transparent field, got {bed_px}",
            );
        }
        // Case `a_title_needs_no_frame_source_at_all`.
        {
            // The hermetic path: with `NoSources` every other layer falls back
            // to a stand-in fill, but a title's picture comes from the document
            // and must be really drawn.
            let mut p = Project::new("t");
            title_clip(
                &mut p,
                1,
                crate::model::TextSource {
                    content: "HELLO".into(),
                    size: 0.5,
                    color: [1.0, 0.2, 0.2, 1.0],
                    ..crate::model::TextSource::default()
                },
            );
            let mut c = compositor();
            let buf = c.frame(&p, 1.0, 96, 96, &mut NoSources);
            let red = buf
                .chunks_exact(4)
                .filter(|q| q[0] > 200 && q[1] < 110 && q[2] < 110)
                .count();
            assert!(
                red > 30,
                "the title renders with no decoder, got {red} red pixels"
            );
            // Nothing else is on the timeline, so the rest is letterbox black —
            // not the two-band stand-in a source-less clip would otherwise draw.
            let black = buf
                .chunks_exact(4)
                .filter(|q| q[0] < 12 && q[1] < 12 && q[2] < 12)
                .count();
            assert!(
                black > 5000,
                "the field around the glyphs stays black, got {black}"
            );
        }
        // Case `a_title_can_be_placed_and_scaled_like_any_other_layer`.
        {
            // Geometry composes with titles: this is how a lower third is made.
            let mut p = Project::new("t");
            let cid = title_clip(
                &mut p,
                1,
                crate::model::TextSource {
                    content: "IIII".into(),
                    size: 0.5,
                    ..crate::model::TextSource::default()
                },
            );
            let mut c = compositor();
            let centred = c.frame(&p, 1.0, 96, 96, &mut NoSources);
            let rows_lit = |buf: &[u8]| {
                let mut top = 0usize;
                let mut bottom = 0usize;
                for (i, q) in buf.chunks_exact(4).enumerate() {
                    if q[3] == 255 && q[0] > 200 {
                        if i / 96 < 48 { top += 1 } else { bottom += 1 }
                    }
                }
                (top, bottom)
            };
            let (t0, b0) = rows_lit(&centred);
            assert!(t0 + b0 > 20, "the centred title is on screen: {t0}/{b0}");

            // Push it into the lower third.
            p.clip_mut(cid).unwrap().transform = Transform {
                scale_x: 0.5,
                scale_y: 0.5,
                pos_y: 0.3,
                ..Transform::default()
            };
            let lower = c.frame(&p, 1.0, 96, 96, &mut NoSources);
            let (t1, b1) = rows_lit(&lower);
            assert!(
                b1 > t1 && t1 == 0,
                "a transformed title moves to the lower third: {t1} above, {b1} below",
            );
        }
    }

    #[test]
    #[expect(
        clippy::many_single_char_names,
        reason = "test shorthands: project p, media m, track t, clip c and its filters f"
    )]
    #[allow(
        clippy::too_many_lines,
        reason = "merged scenario: each former test is a labelled case block with its own fixture"
    )]
    fn grade_and_curves_match_the_cpu_and_reach_the_frame() {
        // Case `empty_timeline_composites_to_black`.
        {
            let p = Project::new("t");
            let mut c = compositor();
            let buf = c.frame(&p, 0.0, 16, 16, &mut NoSources);
            assert_eq!(buf.len(), 16 * 16 * 4);
            assert!(
                buf.chunks(4)
                    .all(|q| q[0] < 4 && q[1] < 4 && q[2] < 4 && q[3] == 255),
                "no clips means an opaque black frame",
            );
        }
        // Case `a_clip_composites_its_palette_and_the_grade_moves_it`.
        {
            let mut p = Project::new("t");
            let m = media(&mut p, 3); // a warm amber slot
            let t = track(&mut p, 1);
            let cid = p.add_clip(t, m, 0.0).unwrap();
            p.clip_mut(cid).unwrap().out_point = 10.0;

            let mut c = compositor();
            let neutral = c.frame(&p, 2.0, 32, 32, &mut NoSources);
            // The clip's palette colour is on the canvas, not black.
            let mid = px(&neutral, 32, 16, 20);
            assert!(
                mid.luma() > 0.1,
                "a clip renders visible colour, got {mid:?}"
            );

            p.clip_mut(cid).unwrap().filters = Filters {
                brightness: 0.95,
                contrast: 0.5,
                saturation: 0.5,
            };
            let bright = c.frame(&p, 2.0, 32, 32, &mut NoSources);
            assert!(
                px(&bright, 32, 16, 20).luma() > mid.luma() + 0.02,
                "raising brightness brightens the composited frame",
            );

            p.clip_mut(cid).unwrap().filters = Filters {
                brightness: 0.5,
                contrast: 0.5,
                saturation: 0.0,
            };
            let flat = px(&c.frame(&p, 2.0, 32, 32, &mut NoSources), 32, 16, 20);
            assert!(
                (flat.r - flat.g).abs() < 0.03 && (flat.g - flat.b).abs() < 0.03,
                "full desaturation collapses the channels, got {flat:?}",
            );
        }
        // Case `the_shader_grade_matches_the_cpu_grade`.
        {
            // The shader is the production path; this pins its arithmetic to
            // `Rgb::graded`, which the inspector and every CPU test rely on.
            let mut p = Project::new("t");
            let m = media(&mut p, 0);
            let t = track(&mut p, 1);
            let cid = p.add_clip(t, m, 0.0).unwrap();
            let f = Filters {
                brightness: 0.66,
                contrast: 0.58,
                saturation: 0.34,
            };
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
        // Case `the_shader_curve_matches_the_cpu_curve`.
        {
            use crate::curve::Curve;
            // The shader is the production path for both preview and export;
            // this pins its table lookup to `Curve::apply`, which the curve
            // editor draws and every CPU test relies on.
            let (mut p, cid) = square_clip();
            let mut curve = Curve::default();
            curve.master.set_point(0.5, 0.85);
            curve.master.set_point(0.25, 0.1);
            p.clip_mut(cid).unwrap().curves = curve.clone();

            let mut c = compositor();
            for level in [64u8, 128, 200] {
                let buf = c.frame(&p, 1.0, 32, 32, &mut flat(level));
                let got = px(&buf, 32, 16, 16);
                let t = f64::from(level) / 255.0;
                let (wr, wg, wb) = curve.apply(t, t, t);
                assert!(
                    (got.r - wr).abs() < 0.02
                        && (got.g - wg).abs() < 0.02
                        && (got.b - wb).abs() < 0.02,
                    "level {level}: shader curve {got:?} should match the CPU curve \
                 ({wr:.3}, {wg:.3}, {wb:.3})",
                );
            }
        }
        // Case `a_per_channel_curve_reaches_the_composited_frame`.
        {
            use crate::curve::Curve;
            // Per-channel comes free in the RGBA table, so it is exactly the
            // sort of thing that can be baked and never bound. Lift red only:
            // a neutral grey has to come out red-cast on the canvas.
            let (mut p, cid) = square_clip();
            let mut curve = Curve::default();
            curve.red.set_point(0.5, 0.9);
            p.clip_mut(cid).unwrap().curves = curve;

            let mut c = compositor();
            let got = px(&c.frame(&p, 1.0, 32, 32, &mut flat(128)), 32, 16, 16);
            assert!(got.r > 0.85, "red was lifted: {got:?}");
            assert!((got.g - 0.502).abs() < 0.02, "green untouched: {got:?}");
            assert!((got.b - 0.502).abs() < 0.02, "blue untouched: {got:?}");
        }
        // Case `each_layer_looks_up_its_own_row_of_the_table`.
        {
            use crate::curve::Curve;
            // One table serves the whole composite, one row per layer. Get the
            // row index wrong and both layers grade with the same curve — which
            // a single-layer test cannot see. So: two layers of the *same* grey,
            // one lifted and one crushed, with the upper one shrunk into the
            // middle so both are on screen at once.
            let mut p = Project::new("t");
            let mid = Uuid::new_v4();
            p.media.push(MediaItem {
                id: mid,
                name: "sq".into(),
                kind: MediaKind::Video,
                duration: 30.0,
                width: 32,
                height: 32,
                fps: 30.0,
                palette: 2,
                path: crate::media::workspace_path("sq.mp4"),
                ..Default::default()
            });
            let v1 = track(&mut p, 1);
            let v2 = track(&mut p, 2);
            let under = p.add_clip(v1, mid, 0.0).unwrap();
            let over = p.add_clip(v2, mid, 0.0).unwrap();

            let mut lift = Curve::default();
            lift.master.set_point(0.5, 0.9);
            let mut crush = Curve::default();
            crush.master.set_point(0.5, 0.1);
            {
                let c = p.clip_mut(under).unwrap();
                c.out_point = 10.0;
                c.curves = lift;
            }
            {
                let c = p.clip_mut(over).unwrap();
                c.out_point = 10.0;
                c.curves = crush;
                c.transform = Transform {
                    scale_x: 0.4,
                    scale_y: 0.4,
                    ..Transform::default()
                };
            }

            let mut c = compositor();
            let buf = c.frame(&p, 1.0, 64, 64, &mut flat(128));
            let centre = px(&buf, 64, 32, 32);
            let corner = px(&buf, 64, 4, 32);
            assert!(
                centre.luma() < 0.2,
                "the upper layer's own curve crushed it: {centre:?}"
            );
            assert!(
                corner.luma() > 0.8,
                "the lower layer's own curve lifted it: {corner:?}"
            );
        }
        // Case `an_animated_curve_amount_ramps_the_grade_across_the_clip`.
        {
            use crate::curve::Curve;
            use crate::param::{Interp, Param};
            // `Param::CurveMix` is the keyframable knob, and the mix is folded
            // into the bake — so an animated amount has to re-bake the table
            // between frames rather than being ignored.
            let (mut p, cid) = square_clip();
            let mut curve = Curve::default();
            curve.master.set_point(0.5, 1.0);
            {
                let c = p.clip_mut(cid).unwrap();
                c.curves = curve;
                c.set_key_at(Param::CurveMix, 0.0, 0.0, Interp::Linear);
                c.set_key_at(Param::CurveMix, 4.0, 1.0, Interp::Linear);
            }

            let mut c = compositor();
            let at = |c: &mut HeadlessCompositor, t: f64| {
                px(&c.frame(&p, t, 32, 32, &mut flat(128)), 32, 16, 16).luma()
            };
            let (start, middle, end) = (at(&mut c, 0.0), at(&mut c, 2.0), at(&mut c, 4.0));
            assert!(
                (start - 0.502).abs() < 0.02,
                "mixed all the way out, the grey is untouched: {start}",
            );
            assert!(
                middle > start + 0.15,
                "half way through it has lifted: {start} → {middle}"
            );
            assert!(
                end > middle + 0.15,
                "and further by the end: {middle} → {end}"
            );
            assert!(
                end > 0.9,
                "at full strength the midtone reaches the point drawn: {end}"
            );
        }
        // Case `a_curve_mixed_all_the_way_out_composites_exactly_as_no_curve_at_all`.
        {
            use crate::curve::Curve;
            // The bypass has to be a real bypass, not "a table that is nearly
            // the identity": an ungraded clip must not shift by a code unit
            // because curves exist.
            let (mut p, cid) = square_clip();
            let mut c = compositor();
            let plain = c.frame(&p, 1.0, 32, 32, &mut flat(137));

            let mut curve = Curve::default();
            curve.master.set_point(0.5, 0.95);
            curve.mix = 0.0;
            p.clip_mut(cid).unwrap().curves = curve;
            let bypassed = c.frame(&p, 1.0, 32, 32, &mut flat(137));
            assert_eq!(plain, bypassed, "mix = 0 is byte-identical to no curve");
        }
    }

    // --- tone curves ------------------------------------------------------
    //
    // A curve reaches the GPU as a baked 256-entry row, so what has to be
    // proved here is not "some colour moved" but that the *table* is the one
    // the CPU baked, addressed by the right row. Every test below paints a
    // flat known grey, so the expected pixel is a number rather than an
    // inference from a palette fill.

    /// A flat mid-grey picture: one tone in, so a curve's answer for that
    /// tone is the whole frame.
    fn flat(level: u8) -> CpuFrames<impl FnMut(Uuid, f64) -> Option<CpuFrame>> {
        painted(32, 32, move |_u, _v| [level, level, level, 255])
    }

    #[test]
    #[expect(
        clippy::many_single_char_names,
        reason = "test shorthands: project p, media m, track t, clip c, and the two frames a/b"
    )]
    #[allow(
        clippy::too_many_lines,
        reason = "merged scenario: each former test is a labelled case block with its own fixture"
    )]
    fn tracks_effects_and_envelopes_shape_the_composite() {
        // Case `every_video_track_contributes_a_layer`.
        {
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
            assert!(
                opaque.r > opaque.g,
                "an opaque top layer covers the one below: {opaque:?}"
            );

            p.clip_mut(c2).unwrap().opacity = 0.5;
            let blended = px(&c.frame(&p, 2.0, 32, 32, &mut NoSources), 32, 16, 20);
            assert!(
                blended.g > opaque.g + 0.05,
                "a half-transparent top layer lets the lower track show through: {blended:?} vs {opaque:?}",
            );
        }
        // Case `a_muted_video_track_is_not_drawn`.
        {
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
        // Case `the_vignette_darkens_the_corners_and_spares_the_centre`.
        {
            // The CPU stand-in could only dim the whole frame flat; the shader
            // has real spatial falloff, so corner and centre must diverge.
            let mut p = Project::new("t");
            let m = media(&mut p, 6); // a flat grey slot: no vertical banding bias
            let t = track(&mut p, 1);
            let cid = p.add_clip(t, m, 0.0).unwrap();
            p.clip_mut(cid).unwrap().out_point = 10.0;

            let mut c = compositor();
            let plain = c.frame(&p, 1.0, 64, 64, &mut NoSources);
            p.clip_mut(cid).unwrap().effects = vec![ClipEffect {
                id: uuid::Uuid::new_v4(),
                kind: EffectKind::Vignette,
                amount: 1.0,
            }]
            .into();
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
        // Case `effects_apply_in_stack_order`.
        {
            let mut p = Project::new("t");
            let m = media(&mut p, 5);
            let t = track(&mut p, 1);
            let cid = p.add_clip(t, m, 0.0).unwrap();
            p.clip_mut(cid).unwrap().out_point = 10.0;
            let mut c = compositor();

            // Sepia last tints a neutral grey warm; sepia first is flattened
            // back to grey by the grayscale that follows it.
            p.clip_mut(cid).unwrap().effects = vec![
                ClipEffect {
                    id: uuid::Uuid::new_v4(),
                    kind: EffectKind::Grayscale,
                    amount: 1.0,
                },
                ClipEffect {
                    id: uuid::Uuid::new_v4(),
                    kind: EffectKind::Sepia,
                    amount: 1.0,
                },
            ]
            .into();
            let gray_then_sepia = px(&c.frame(&p, 1.0, 32, 32, &mut NoSources), 32, 16, 20);
            p.clip_mut(cid).unwrap().move_effect_down(0);
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
        // Case `the_fade_envelope_dims_the_composite`.
        {
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
            assert!(
                head < mid * 0.2,
                "the head of a fade-in is near black: {head} vs {mid}"
            );
        }
        // Case `the_same_input_always_composites_the_same_frame`.
        {
            // `--shots` determinism, asserted on the production path.
            let mut p = Project::new("t");
            let m = media(&mut p, 4);
            let t = track(&mut p, 1);
            let cid = p.add_clip(t, m, 0.0).unwrap();
            p.clip_mut(cid).unwrap().out_point = 10.0;
            p.clip_mut(cid).unwrap().effects = vec![
                ClipEffect {
                    id: uuid::Uuid::new_v4(),
                    kind: EffectKind::Sepia,
                    amount: 0.7,
                },
                ClipEffect {
                    id: uuid::Uuid::new_v4(),
                    kind: EffectKind::Vignette,
                    amount: 0.5,
                },
            ]
            .into();
            let mut c = compositor();
            let a = c.frame(&p, 3.25, 48, 48, &mut NoSources);
            let b = c.frame(&p, 3.25, 48, 48, &mut NoSources);
            assert_eq!(a, b, "the composite is byte-for-byte reproducible");
        }
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
    #[allow(
        clippy::too_many_lines,
        reason = "merged scenario: each former test is a labelled case block with its own fixture"
    )]
    fn gpu_transitions_render_distinct_frames() {
        // Case `each_transition_renders_a_different_frame`.
        {
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
        // Case `a_dissolve_samples_both_clips`.
        {
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
            let end = px(
                &c.frame(
                    &p,
                    5.0 + crate::preview::TRANSITION_SECS + 0.01,
                    32,
                    32,
                    &mut NoSources,
                ),
                32,
                16,
                20,
            );
            assert!(
                start.g > start.r,
                "the dissolve opens on the outgoing clip: {start:?}"
            );
            assert!(end.r > end.g, "and lands on the incoming clip: {end:?}");
            assert!(
                mid.r > start.r && mid.g > end.g,
                "the middle is a real blend: {mid:?}"
            );
        }
        // Case `a_fade_in_rises_out_of_black_without_the_previous_clip`.
        {
            let (p, _) = two_clip_project(Transition::FadeIn);
            let mut c = compositor();
            let early = px(&c.frame(&p, 5.02, 32, 32, &mut NoSources), 32, 16, 20);
            let late = px(
                &c.frame(
                    &p,
                    5.0 + crate::preview::TRANSITION_SECS * 0.9,
                    32,
                    32,
                    &mut NoSources,
                ),
                32,
                16,
                20,
            );
            assert!(
                early.luma() < 0.06,
                "a fade opens on black, not the previous clip: {early:?}"
            );
            assert!(
                late.luma() > early.luma() + 0.1,
                "and rises out of it: {late:?}"
            );
            assert!(late.r > late.g, "what rises is the incoming clip: {late:?}");
        }
        // Case `a_dip_passes_through_black`.
        {
            let (p, _) = two_clip_project(Transition::DipToBlack);
            let mut c = compositor();
            let dur = crate::preview::TRANSITION_SECS;
            let quarter = px(
                &c.frame(&p, 5.0 + dur * 0.25, 32, 32, &mut NoSources),
                32,
                16,
                20,
            );
            let middle = px(
                &c.frame(&p, 5.0 + dur * 0.5, 32, 32, &mut NoSources),
                32,
                16,
                20,
            );
            let three_q = px(
                &c.frame(&p, 5.0 + dur * 0.75, 32, 32, &mut NoSources),
                32,
                16,
                20,
            );
            assert!(
                middle.luma() < 0.03,
                "the dip bottoms out at black: {middle:?}"
            );
            assert!(
                quarter.g > quarter.r,
                "it leaves on the outgoing clip: {quarter:?}"
            );
            assert!(
                three_q.r > three_q.g,
                "and returns on the incoming one: {three_q:?}"
            );
        }
        // Case `a_wipe_is_geometry_not_a_flash`.
        {
            let (p, t) = two_clip_project(Transition::Wipe);
            let mut c = compositor();
            let buf = c.frame(&p, t, 64, 64, &mut NoSources);
            // Halfway through a left-to-right wipe: the left side is the
            // incoming clip, the right side is still the outgoing one.
            let left = px(&buf, 64, 6, 30);
            let right = px(&buf, 64, 57, 30);
            assert!(
                left.r > left.g,
                "the revealed side shows the incoming clip: {left:?}"
            );
            assert!(
                right.g > right.r,
                "the unreached side still shows the outgoing one: {right:?}"
            );
            // A soft edge: somewhere near the middle the two are mixed.
            let edge = px(&buf, 64, 32, 30);
            assert!(
                edge.r > 0.1 && edge.g > 0.1,
                "the wipe edge is feathered, not a hard step: {edge:?}",
            );
        }
    }

    #[test]
    fn frame_sources_replace_or_fall_back_to_the_representative_fill() {
        // Case `a_resolved_source_texture_replaces_the_representative_fill`.
        {
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
                    rgba: Arc::new([255, 0, 255, 255].repeat(16)),
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
        // Case `a_source_that_yields_nothing_falls_back_to_the_representative_fill`.
        {
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
        // Case `clip_layer_reads_the_clip_at_its_own_source_time`.
        {
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

    /// Deterministic moving footage encoded in-process for decode fidelity.
    #[cfg(feature = "decode")]
    fn demo_clip() -> std::path::PathBuf {
        generated_clip::encoded_video()
    }

    /// Mean horizontal neighbour variation. The representative fill is
    /// built from horizontal bands, so it is constant along every row and
    /// this collapses to zero; a real decoded picture never does. It is
    /// the one measurement that tells "showing the footage" apart from
    /// "showing a coloured rectangle".
    #[cfg(feature = "decode")]
    fn horizontal_detail(buf: &[u8], w: u32, h: u32) -> f64 {
        let mut total = 0.0;
        for y in 0..h as usize {
            for x in 1..w as usize {
                let a = (y * w as usize + x - 1) * 4;
                let b = (y * w as usize + x) * 4;
                for c in 0..3 {
                    total += f64::from(buf[a + c].abs_diff(buf[b + c]));
                }
            }
        }
        total / f64::from(w * h)
    }

    #[test]
    #[serial_test::serial(video_editor_gst)]
    fn decoded_footage_reaches_the_canvas_through_the_gpu() {
        // Case `a_hydrated_source_puts_real_footage_on_the_canvas`:
        // With a hydrated real source under the playhead, the canvas must
        // show the decoded picture — not the representative fallback.
        //
        // This is the failure mode a screenshot catches and a unit test
        // usually does not: the editor still renders, still looks plausible,
        // and is showing the user a coloured rectangle instead of their
        // footage. It drives the real decoder and asserts on what the
        // production compositor puts on the canvas.
        #[cfg(feature = "decode")]
        'case: {
            use crate::model::{MediaItem, MediaKind, Track, TrackKind};
            if !generated_clip::media_stack() {
                break 'case;
            }

            let path = demo_clip();
            let mut p = Project::new("live");
            p.width = 1920;
            p.height = 1080;
            let mid = Uuid::new_v4();
            p.media.push(MediaItem {
                id: mid,
                name: "demo".into(),
                kind: MediaKind::Video,
                duration: generated_clip::DURATION_SECS,
                width: generated_clip::WIDTH,
                height: generated_clip::HEIGHT,
                fps: f64::from(generated_clip::FPS),
                palette: 0,
                path: path.to_string_lossy().into_owned(),
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
                gain_db: 0.0,
                pan: 0.0,
            });
            p.add_clip(tid, mid, 0.0).unwrap();

            // Drive the real decoder the way the app's warm-up does: pump until
            // a frame for the clip under the playhead lands, or give up loudly.
            let playhead = 2.0;
            let mut decoder = crate::preview::PreviewDecoder::new();
            assert!(
                decoder.is_real_decode(),
                "the decode feature must stand up a real pipeline, not the synthesized fallback",
            );
            let deadline = tilo_test_env::hang_deadline();
            let mut ready = false;
            while ui::time::now() < deadline {
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
            assert_eq!(
                decoded_media,
                Some(mid),
                "the frame belongs to the clip under the playhead"
            );
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
        // Case `the_apps_own_frame_source_puts_real_footage_on_the_canvas`:
        // The canvas, through the source the *app* actually composites with.
        //
        // The test above hands the compositor frames it decoded itself with
        // `PreviewDecoder`. That decoder no longer paints anything: the app
        // composites through [`MediaFrameSource`]. So that test's name promises
        // canvas coverage it does not have, and it stayed green while the shipped
        // preview fell back to the representative fill — measured on the demo
        // clip, the captured canvas's horizontal detail dropped from 11.4 to 0.56
        // and no test noticed.
        //
        // This drives the real pair. It also pins the reason it broke: a capture
        // renders exactly once, so it must ask for `Exact` and wait. `Responsive`
        // is right for the live editor — a scrub must never stall the UI thread —
        // but asking it for a picture that has not arrived yet and then rendering
        // immediately is precisely how a stand-in gets captured as though it were
        // the footage.
        #[cfg(feature = "decode")]
        'case: {
            if !generated_clip::media_stack() {
                break 'case;
            }
            let path = demo_clip();
            let mut p = Project::new("live");
            p.width = 1920;
            p.height = 1080;
            let mid = Uuid::new_v4();
            p.media.push(crate::model::MediaItem {
                id: mid,
                name: "demo".into(),
                kind: crate::model::MediaKind::Video,
                duration: generated_clip::DURATION_SECS,
                width: generated_clip::WIDTH,
                height: generated_clip::HEIGHT,
                fps: f64::from(generated_clip::FPS),
                path: path.to_string_lossy().into_owned(),
                ..Default::default()
            });
            let tid = Uuid::new_v4();
            p.tracks.push(crate::model::Track {
                id: tid,
                name: "V1".into(),
                kind: crate::model::TrackKind::Video,
                index: 1,
                ..Default::default()
            });
            p.add_clip(tid, mid, 0.0).unwrap();

            let (w, h) = (192u32, 108u32);
            let mut c = compositor();
            let mut sources = MediaFrameSource::new(&p, Fetch::Exact);
            let live = c.frame(&p, 2.0, w, h, &mut sources);

            assert!(
                sources.served_real_picture(),
                "the frame source reported no decoded picture — the canvas is a stand-in",
            );
            let detail = horizontal_detail(&live, w, h);
            println!("app-path canvas horizontal detail = {detail:.3}");
            assert!(
                detail > 2.0,
                "the canvas is flat along its rows ({detail:.3}): the app's own source \
             served the representative fill, not the footage",
            );
        }
        // Case `a_reused_readback_buffer_returns_the_same_picture`:
        // Reusing the readback buffer must not change a single pixel, including
        // across a change of output size.
        //
        // The buffer the finished frame is copied into is kept between frames
        // rather than rebuilt, because an export allocates and frees 8 MB per
        // frame otherwise. The risk that buys is staleness: a buffer sized for
        // one output serving another would read back the wrong rows, or read
        // somebody else's leftovers into the picture. So render the same frame
        // at two sizes, alternating, and require each to be identical to what a
        // compositor that had only ever seen that size produces.
        {
            let p = crate::seed::sample_project();
            let small = (160u32, 90u32);
            let large = (320u32, 180u32);

            // References, each from a compositor that has seen one size only.
            let want_small = compositor().frame(&p, 6.0, small.0, small.1, &mut NoSources);
            let want_large = compositor().frame(&p, 6.0, large.0, large.1, &mut NoSources);
            assert_eq!(want_small.len(), (small.0 * small.1 * 4) as usize);
            assert_eq!(want_large.len(), (large.0 * large.1 * 4) as usize);
            assert!(
                want_small.iter().any(|&b| b != 0),
                "the reference frame is blank, so comparing against it proves nothing",
            );

            // One compositor, alternating sizes, so the buffer is reused within a
            // size and rebuilt across one.
            let mut c = compositor();
            for round in 0..3 {
                let got_small = c.frame(&p, 6.0, small.0, small.1, &mut NoSources);
                assert_eq!(
                    got_small, want_small,
                    "round {round}: the small frame differs after the buffer was reused",
                );
                let got_large = c.frame(&p, 6.0, large.0, large.1, &mut NoSources);
                assert_eq!(
                    got_large, want_large,
                    "round {round}: the large frame differs after the buffer was resized",
                );
            }
        }
        // Case `a_file_that_never_decodes_is_given_up_on`:
        // A file the decoder can never read is given up on, rather than retried
        // for the life of the editor.
        //
        // The media bin holds whatever the project says it holds. Import refuses
        // a non-media file and says why, but that guard runs only at import: a
        // document persisted before it existed, or one replicated from a peer,
        // carries its items straight past it. Such an item reached the decoder on
        // every frame, each attempt paying the full frame timeout, so an editor
        // holding one stalled on a fixed cadence indefinitely — which is exactly
        // what a user reported as a "pseudo video" sitting in the clips.
        //
        // The fixture is an HTML page named `.mp4`, the real shape of the report:
        // a saved web page whose extension says video. It is deliberately *not*
        // checked through `probe`, because probing is the guard that already
        // works — this asserts the decode path gives up on its own.
        #[cfg(feature = "decode")]
        {
            use crate::model::{MediaItem, MediaKind, Track, TrackKind};

            // A unique name under the OS temp dir rather than a `tempfile`
            // dependency the crate does not otherwise carry; removed at the end.
            let path =
                std::env::temp_dir().join(format!("tilo-ve-notmedia-{}.mp4", Uuid::new_v4()));
            std::fs::write(
                &path,
                b"<!DOCTYPE html>\n<html><head><title>Not a video</title></head>\
              <body><p>A saved web page.</p></body></html>\n",
            )
            .expect("write the fixture");

            let mut p = Project::new("salvage");
            p.width = 192;
            p.height = 108;
            let mid = Uuid::new_v4();
            p.media.push(MediaItem {
                id: mid,
                name: "holiday.mp4".into(),
                kind: MediaKind::Video,
                duration: 10.0,
                width: 1920,
                height: 1080,
                fps: 30.0,
                path: path.to_string_lossy().into_owned(),
                ..Default::default()
            });
            let tid = Uuid::new_v4();
            p.tracks.push(Track {
                id: tid,
                name: "V1".into(),
                kind: TrackKind::Video,
                index: 1,
                ..Default::default()
            });
            let cid = p.add_clip(tid, mid, 0.0).unwrap();
            p.clip_mut(cid).unwrap().out_point = 10.0;

            let (w, h) = (192u32, 108u32);
            let mut c = compositor();
            // A short timeout so the give-up threshold is reached in test time
            // rather than in the production frame budget; the policy under test
            // is the count, not the wait.
            let mut sources = MediaFrameSource::new(&p, Fetch::Responsive)
                .with_timeout(std::time::Duration::from_millis(50));

            // Ask past the threshold, then keep asking.
            let rounds = crate::thumbnail::FAILURES_BEFORE_GIVING_UP + 3;
            for i in 0..rounds {
                let _ = c.frame(&p, f64::from(i) * 0.5, w, h, &mut sources);
            }

            assert!(
                sources.is_undecodable(mid),
                "a source that answered nothing every time was still being treated as decodable",
            );

            // The real claim: it stopped *asking*. Written off but still opening a
            // pipeline every frame would leave the stall exactly as it was, and
            // `is_undecodable` alone cannot tell the two apart.
            let settled = sources.decode_attempts();
            for i in 0..5 {
                let _ = c.frame(&p, f64::from(i) * 0.5, w, h, &mut sources);
            }
            assert_eq!(
                sources.decode_attempts(),
                settled,
                "the editor kept re-opening a pipeline for a file it had already given up on",
            );
            assert!(
                !sources.served_real_picture(),
                "nothing decoded, so no real picture can have been served",
            );
            std::fs::remove_file(&path).ok();
        }
        // Case `the_media_source_paints_a_decoded_frame_through_the_gpu`:
        // The compositor painting a real hardware-decoded frame.
        //
        // This is the whole point of the media seam: a decoder emits NV12, the
        // planes go to the GPU untouched, and the YUV arithmetic happens in a
        // shader. If the planar branch ever broke — or quietly fell back to a
        // CPU RGBA convert — the picture would still appear and nothing else
        // would complain, so assert on what reaches the canvas *and* on which
        // payload carried it.
        #[cfg(feature = "decode")]
        'case: {
            use crate::model::{MediaItem, MediaKind, Track, TrackKind};
            use media::{FramePayload, SessionPool};
            if !generated_clip::media_stack() {
                break 'case;
            }

            let path = demo_clip();
            let mut p = Project::new("hw");
            p.width = 1920;
            p.height = 1080;
            let mid = Uuid::new_v4();
            p.media.push(MediaItem {
                id: mid,
                name: "demo".into(),
                kind: MediaKind::Video,
                duration: generated_clip::DURATION_SECS,
                width: generated_clip::WIDTH,
                height: generated_clip::HEIGHT,
                fps: f64::from(generated_clip::FPS),
                palette: 0,
                path: path.to_string_lossy().into_owned(),
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
                gain_db: 0.0,
                pan: 0.0,
            });
            p.add_clip(tid, mid, 0.0).unwrap();

            // A generous deadline: this box shares its CPU with other builds,
            // and a cold pipeline plus an accurate seek can outrun the 2s the
            // engine defaults to. The assertions below are unchanged — only how
            // long the decoder is given to answer.
            let mut src =
                MediaFrameSource::new(&p, Fetch::Exact).with_timeout(tilo_test_env::hang_bound());
            assert_eq!(src.decodable(), 1, "the hydrated local file is decodable");

            let mut c = compositor();
            let (w, h) = (192u32, 108u32);
            let live = c.frame(&p, 2.0, w, h, &mut src);
            let fallback = c.frame(&p, 2.0, w, h, &mut NoSources);

            assert_ne!(live, fallback, "the decoded frame must reach the canvas");
            let live_detail = horizontal_detail(&live, w, h);
            let report = src
                .hw_report(mid)
                .expect("the session reports what it plugged");
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
                .frame_at(2_000, std::time::Duration::from_secs(10))
                .expect("a frame at 2s");
            drop(s);
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
        // Case `two_layers_of_one_source_get_their_own_textures`:
        // Two clips of the same source at different instants must paint two
        // different pictures in one composite.
        //
        // The converter reuses its output texture across frames, so a single
        // shared converter would have the second layer overwrite the first and
        // both would show the same picture — invisible in a still, and exactly
        // the kind of thing a dissolve depends on.
        #[cfg(feature = "decode")]
        'case: {
            use crate::model::{MediaItem, MediaKind, Track, TrackKind};
            if !generated_clip::media_stack() {
                break 'case;
            }

            let path = demo_clip();
            let mut p = Project::new("two");
            let mid = Uuid::new_v4();
            p.media.push(MediaItem {
                id: mid,
                name: "demo".into(),
                kind: MediaKind::Video,
                duration: generated_clip::DURATION_SECS,
                width: generated_clip::WIDTH,
                height: generated_clip::HEIGHT,
                fps: f64::from(generated_clip::FPS),
                palette: 0,
                path: path.to_string_lossy().into_owned(),
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
                    gain_db: 0.0,
                    pan: 0.0,
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
                c.in_point = 0.0;
                c.out_point = 2.0;
            }
            let upper = p.add_clip(v2, mid, 0.0).unwrap();
            {
                let c = p.clip_mut(upper).unwrap();
                c.in_point = 2.0;
                c.out_point = 4.0;
                c.opacity = 0.5;
                c.effects = vec![ClipEffect {
                    id: uuid::Uuid::new_v4(),
                    kind: EffectKind::Invert,
                    amount: 1.0,
                }]
                .into();
            }

            let mut src =
                MediaFrameSource::new(&p, Fetch::Exact).with_timeout(tilo_test_env::hang_bound());
            let mut c = compositor();
            let both = c.frame(&p, 1.0, 96, 54, &mut src);

            // With the upper layer half-transparent over the lower one, the
            // result must be neither layer alone.
            let mut only_lower = p.clone();
            only_lower
                .tracks
                .iter_mut()
                .find(|t| t.id == v2)
                .unwrap()
                .muted = true;
            let lower_only = c.frame(&only_lower, 1.0, 96, 54, &mut src);

            let mut only_upper = p.clone();
            only_upper
                .tracks
                .iter_mut()
                .find(|t| t.id == v1)
                .unwrap()
                .muted = true;
            only_upper.clip_mut(upper).unwrap().opacity = 1.0;
            let upper_only = c.frame(&only_upper, 1.0, 96, 54, &mut src);

            assert_ne!(
                both, lower_only,
                "the upper layer contributes to the composite"
            );
            assert_ne!(both, upper_only, "so does the lower one");
            assert_ne!(
                lower_only, upper_only,
                "the two layers are drawn from different instants of the source — a shared \
             converter texture would make them identical",
            );
        }
    }

    // ----------------------------------------------------------------------
    // Colour key and shape mask, on the real compute pass.
    //
    // These are compositing features, so what has to be proved is not that a
    // number reached a uniform but that the layer *underneath* becomes
    // visible where the top layer was cut. Every test here stacks two clips
    // with different pictures and reads back the frame.
    // ----------------------------------------------------------------------

    /// A [`FrameSource`] that paints each media its own picture, so a
    /// two-layer stack can be told apart pixel by pixel.
    #[expect(
        clippy::many_single_char_names,
        reason = "pixel maths: frame w/h, texel index i, its u/v, and the colour c"
    )]
    fn per_media(
        w: u32,
        h: u32,
        paint: impl Fn(Uuid, f64, f64) -> [u8; 4] + Send + 'static,
    ) -> CpuFrames<impl FnMut(Uuid, f64) -> Option<CpuFrame>> {
        let mut cache: HashMap<Uuid, std::sync::Arc<Vec<u8>>> = HashMap::new();
        CpuFrames::new(move |media, _t| {
            let rgba = cache.entry(media).or_insert_with(|| {
                let mut buf = vec![0u8; (w * h * 4) as usize];
                for y in 0..h {
                    for x in 0..w {
                        let u = (f64::from(x) + 0.5) / f64::from(w);
                        let v = (f64::from(y) + 0.5) / f64::from(h);
                        let c = paint(media, u, v);
                        let i = ((y * w + x) * 4) as usize;
                        buf[i..i + 4].copy_from_slice(&c);
                    }
                }
                std::sync::Arc::new(buf)
            });
            Some(CpuFrame {
                width: w,
                height: h,
                rgba: rgba.clone(),
                identity: 0,
            })
        })
    }

    /// Two square clips stacked V1 under V2, both covering the playhead.
    /// Returns `(project, under_clip, over_clip, under_media, over_media)`.
    fn stacked() -> (Project, Uuid, Uuid, Uuid, Uuid) {
        let mut p = Project::new("stack");
        let add_media = |p: &mut Project, name: &str| {
            let id = Uuid::new_v4();
            p.media.push(MediaItem {
                id,
                name: name.into(),
                kind: MediaKind::Video,
                duration: 30.0,
                width: 32,
                height: 32,
                fps: 30.0,
                palette: 2,
                path: crate::media::workspace_path("s.mp4"),
                ..Default::default()
            });
            id
        };
        let under_m = add_media(&mut p, "under");
        let over_m = add_media(&mut p, "over");
        let v1 = track(&mut p, 1);
        let v2 = track(&mut p, 2);
        let under = p.add_clip(v1, under_m, 0.0).unwrap();
        let over = p.add_clip(v2, over_m, 0.0).unwrap();
        p.clip_mut(under).unwrap().out_point = 10.0;
        p.clip_mut(over).unwrap().out_point = 10.0;
        (p, under, over, under_m, over_m)
    }

    /// The key green the presets offer, as bytes, and the red beneath it.
    const PLATE: [u8; 4] = [15, 184, 51, 255];
    const BENEATH: [u8; 4] = [210, 40, 40, 255];
    /// The "subject" standing on the plate: an orange bar down the middle.
    const SUBJECT: [u8; 4] = [235, 130, 30, 255];

    fn is_reddish(c: Rgb) -> bool {
        c.r > 0.5 && c.r > c.g + 0.2 && c.r > c.b + 0.2
    }

    /// A stack whose lower layer is flat red and whose upper layer is the
    /// key green with an orange bar across its middle third.
    fn plate_over_red() -> (
        Project,
        Uuid,
        CpuFrames<impl FnMut(Uuid, f64) -> Option<CpuFrame>>,
    ) {
        let (p, _under, over, under_m, _over_m) = stacked();
        let src = per_media(32, 32, move |media, _u, v| {
            if media == under_m {
                BENEATH
            } else if (0.4..0.6).contains(&v) {
                SUBJECT
            } else {
                PLATE
            }
        });
        (p, over, src)
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "merged scenario: each former test is a labelled case block with its own fixture"
    )]
    fn gpu_keys_and_masks_composite_the_layer_beneath() {
        // Case `without_a_key_the_upper_layer_hides_the_one_beneath`.
        {
            // The control the two keyed tests below are read against: with no
            // key, the plate covers everything and nothing red survives.
            let (p, _over, mut src) = plate_over_red();
            let mut c = compositor();
            let buf = c.frame(&p, 1.0, 64, 64, &mut src);
            for y in [4, 20, 60] {
                assert!(
                    !is_reddish(px(&buf, 64, 32, y)),
                    "unkeyed, the layer beneath must not show at y={y}: {:?}",
                    px(&buf, 64, 32, y),
                );
            }
            assert!(
                is_green(px(&buf, 64, 32, 4)),
                "the plate is what is on screen"
            );
        }
        // Case `keying_the_plate_reveals_the_layer_beneath_and_keeps_the_subject`.
        {
            let (mut p, over, mut src) = plate_over_red();
            p.clip_mut(over).unwrap().key = crate::model::ChromaKey {
                enabled: true,
                color: [
                    f64::from(PLATE[0]) / 255.0,
                    f64::from(PLATE[1]) / 255.0,
                    f64::from(PLATE[2]) / 255.0,
                ],
                tolerance: 0.25,
                softness: 0.1,
                spill: 0.0,
            };
            let mut c = compositor();
            let buf = c.frame(&p, 1.0, 64, 64, &mut src);
            // Above and below the bar the plate is gone, so V1's red shows.
            for y in [4, 60] {
                let got = px(&buf, 64, 32, y);
                assert!(
                    is_reddish(got),
                    "the keyed plate must let the layer beneath through at y={y}: {got:?}",
                );
            }
            // Across the bar the subject is untouched — a key that ate the
            // subject would pass the check above and still be useless.
            let bar = px(&buf, 64, 32, 32);
            assert!(
                bar.r > 0.6 && bar.g > 0.3 && bar.g < 0.75 && bar.b < 0.3,
                "the subject survives the key: {bar:?}",
            );
        }
        // Case `an_unevenly_lit_plate_keys_top_to_bottom`:
        // The exposure invariance, proved where it matters: on the composite.
        // A plate lit unevenly — bright at the top, a fifth of that at the
        // bottom — has to key all the way down, on **one** tolerance.
        //
        // The tolerance here is deliberately tight: it is what a clean plate
        // needs and no more. A keyer measuring absolute Cb/Cr puts the dim
        // rows several times that distance from the key and keeps them, so a
        // slack tolerance would let one pass while looking like it had proved
        // something.
        {
            let (p, _under, over, under_m, _over_m) = stacked();
            let mut src = per_media(32, 32, move |media, _u, v| {
                if media == under_m {
                    return BENEATH;
                }
                // Full exposure at the top down to a fifth of it at the bottom.
                let gain = 1.0 - 0.8 * v;
                [
                    (f64::from(PLATE[0]) * gain) as u8,
                    (f64::from(PLATE[1]) * gain) as u8,
                    (f64::from(PLATE[2]) * gain) as u8,
                    255,
                ]
            });
            let mut p = p;
            p.clip_mut(over).unwrap().key = crate::model::ChromaKey {
                enabled: true,
                color: [
                    f64::from(PLATE[0]) / 255.0,
                    f64::from(PLATE[1]) / 255.0,
                    f64::from(PLATE[2]) / 255.0,
                ],
                tolerance: 0.12,
                softness: 0.05,
                spill: 0.0,
            };
            let mut c = compositor();
            let buf = c.frame(&p, 1.0, 64, 64, &mut src);
            for y in [2, 16, 32, 48, 58] {
                let got = px(&buf, 64, 32, y);
                assert!(
                    is_reddish(got),
                    "the plate keys at every exposure; y={y} was {got:?} — an absolute \
                 chroma distance would have kept the shadowed rows",
                );
            }
        }
        // Case `spill_removal_takes_the_key_hue_off_the_subject`.
        {
            let (p, _under, over, under_m, _over_m) = stacked();
            // A subject washed with green light, over red.
            let tinted: [u8; 4] = [160, 150, 95, 255];
            let mut src = per_media(
                32,
                32,
                move |media, _u, _v| {
                    if media == under_m { BENEATH } else { tinted }
                },
            );
            let key = |spill: f64| crate::model::ChromaKey {
                enabled: true,
                color: [
                    f64::from(PLATE[0]) / 255.0,
                    f64::from(PLATE[1]) / 255.0,
                    f64::from(PLATE[2]) / 255.0,
                ],
                // Wide enough to be keying, but nowhere near the tinted subject.
                tolerance: 0.2,
                softness: 0.05,
                spill,
            };
            let mut c = compositor();
            let mut p = p;
            p.clip_mut(over).unwrap().key = key(0.0);
            let plain = px(&c.frame(&p, 1.0, 64, 64, &mut src), 64, 32, 32);
            p.clip_mut(over).unwrap().key = key(1.0);
            let clean = px(&c.frame(&p, 1.0, 64, 64, &mut src), 64, 32, 32);

            assert!(
                !is_reddish(plain) && !is_reddish(clean),
                "the subject is kept either way — spill removal is not a matte",
            );
            // Green comes down relative to the other two channels.
            let cast = |c: Rgb| c.g - c.r.midpoint(c.b);
            assert!(
                cast(clean) < cast(plain) - 0.02,
                "spill removal pulls the green cast down: {:.3} → {:.3}",
                cast(plain),
                cast(clean),
            );
            assert!(
                (clean.luma() - plain.luma()).abs() < 0.05,
                "and leaves the exposure alone: {:.3} → {:.3}",
                plain.luma(),
                clean.luma(),
            );
        }
        // Case `an_ellipse_mask_cuts_the_upper_layer_to_a_disc`.
        {
            let (p, _under, over, under_m, _over_m) = stacked();
            let mut src = per_media(32, 32, move |media, _u, _v| {
                if media == under_m {
                    BENEATH
                } else {
                    [40, 200, 90, 255]
                }
            });
            let mut p = p;
            p.clip_mut(over).unwrap().mask = crate::model::Mask {
                shape: crate::model::MaskShape::Ellipse,
                w: 0.5,
                h: 0.5,
                feather: 0.05,
                ..crate::model::Mask::default()
            };
            let mut c = compositor();
            let buf = c.frame(&p, 1.0, 64, 64, &mut src);
            assert!(
                is_green(px(&buf, 64, 32, 32)),
                "inside the disc the upper layer shows"
            );
            for (x, y) in [(2, 2), (61, 2), (2, 61), (61, 61), (32, 2)] {
                assert!(
                    is_reddish(px(&buf, 64, x, y)),
                    "outside the disc the layer beneath shows at ({x},{y}): {:?}",
                    px(&buf, 64, x, y),
                );
            }
        }
        // Case `inverting_the_mask_swaps_which_layer_is_where`.
        {
            let (p, _under, over, under_m, _over_m) = stacked();
            let mut src = per_media(32, 32, move |media, _u, _v| {
                if media == under_m {
                    BENEATH
                } else {
                    [40, 200, 90, 255]
                }
            });
            let mut p = p;
            p.clip_mut(over).unwrap().mask = crate::model::Mask {
                shape: crate::model::MaskShape::Ellipse,
                w: 0.5,
                h: 0.5,
                feather: 0.05,
                invert: true,
                ..crate::model::Mask::default()
            };
            let mut c = compositor();
            let buf = c.frame(&p, 1.0, 64, 64, &mut src);
            assert!(
                is_reddish(px(&buf, 64, 32, 32)),
                "the centre is now the hole"
            );
            assert!(
                is_green(px(&buf, 64, 2, 2)),
                "and the corner keeps the upper layer"
            );
        }
        // Case `a_rect_mask_is_not_the_same_composite_as_an_ellipse`:
        // A rect and an ellipse of one size have to composite differently, or
        // the shape control does nothing. They differ exactly at the corners
        // of the box.
        {
            let (p, _under, over, under_m, _over_m) = stacked();
            let mut src = per_media(32, 32, move |media, _u, _v| {
                if media == under_m {
                    BENEATH
                } else {
                    [40, 200, 90, 255]
                }
            });
            let base = crate::model::Mask {
                w: 0.8,
                h: 0.8,
                feather: 0.02,
                ..crate::model::Mask::default()
            };
            let mut c = compositor();
            let mut p = p;
            // Just inside the corner of the 0.8-wide box.
            let (x, y) = (55, 55);
            p.clip_mut(over).unwrap().mask = crate::model::Mask {
                shape: crate::model::MaskShape::Rect,
                ..base
            };
            let rect = px(&c.frame(&p, 1.0, 64, 64, &mut src), 64, x, y);
            p.clip_mut(over).unwrap().mask = crate::model::Mask {
                shape: crate::model::MaskShape::Ellipse,
                ..base
            };
            let ellipse = px(&c.frame(&p, 1.0, 64, 64, &mut src), 64, x, y);
            assert!(is_green(rect), "the rect reaches its own corner: {rect:?}");
            assert!(is_reddish(ellipse), "the ellipse does not: {ellipse:?}");
        }
        // Case `feathering_a_mask_puts_a_real_ramp_at_its_edge`.
        {
            let (p, _under, over, under_m, _over_m) = stacked();
            let mut src = per_media(32, 32, move |media, _u, _v| {
                if media == under_m {
                    BENEATH
                } else {
                    [40, 200, 90, 255]
                }
            });
            let mut c = compositor();
            let mut p = p;
            let base = crate::model::Mask {
                shape: crate::model::MaskShape::Rect,
                w: 0.6,
                h: 2.0,
                ..crate::model::Mask::default()
            };
            // Recover the blend fraction column by column from the green
            // channel, whose two endpoints are known, and count the columns
            // that are genuinely part-way between the layers. Reading the mix
            // itself rather than asking "which colour family is this" is what
            // makes a one-pixel edge and a ten-pixel ramp distinguishable.
            let (lo, hi) = (40.0 / 255.0, 200.0 / 255.0);
            let ramp = |buf: &[u8]| {
                (0..64)
                    .filter(|x| {
                        let t = (px(buf, 64, *x, 32).g - lo) / (hi - lo);
                        (0.05..0.95).contains(&t)
                    })
                    .count()
            };
            p.clip_mut(over).unwrap().mask = crate::model::Mask {
                feather: 0.005,
                ..base
            };
            let hard = ramp(&c.frame(&p, 1.0, 64, 64, &mut src));
            p.clip_mut(over).unwrap().mask = crate::model::Mask {
                feather: 0.5,
                ..base
            };
            let soft = ramp(&c.frame(&p, 1.0, 64, 64, &mut src));
            assert!(
                hard <= 4,
                "an unfeathered edge is a cut, not a ramp: {hard} columns of blend",
            );
            assert!(
                soft > hard + 8,
                "feathering widens the ramp: {hard} → {soft} columns of blend",
            );
        }
        // Case `a_keyframed_mask_is_in_a_different_place_in_each_second`:
        // The reason both of these are parameters rather than plain fields: a
        // mask has to be able to follow a subject across a shot.
        {
            let (p, _under, over, under_m, _over_m) = stacked();
            let mut src = per_media(32, 32, move |media, _u, _v| {
                if media == under_m {
                    BENEATH
                } else {
                    [40, 200, 90, 255]
                }
            });
            let mut p = p;
            {
                let c = p.clip_mut(over).unwrap();
                c.mask = crate::model::Mask {
                    shape: crate::model::MaskShape::Ellipse,
                    w: 0.4,
                    h: 0.4,
                    feather: 0.05,
                    ..crate::model::Mask::default()
                };
                c.set_key_at(
                    crate::param::Param::MaskX,
                    0.0,
                    -0.3,
                    crate::param::Interp::Linear,
                );
                c.set_key_at(
                    crate::param::Param::MaskX,
                    4.0,
                    0.3,
                    crate::param::Interp::Linear,
                );
            }
            let mut c = compositor();
            let early = c.frame(&p, 0.0, 64, 64, &mut src);
            let late = c.frame(&p, 4.0, 64, 64, &mut src);
            // Left of centre early, right of centre late.
            assert!(
                is_green(px(&early, 64, 13, 32)),
                "the disc starts on the left"
            );
            assert!(is_reddish(px(&early, 64, 51, 32)), "and not on the right");
            assert!(
                is_reddish(px(&late, 64, 13, 32)),
                "by the end it has left the left"
            );
            assert!(is_green(px(&late, 64, 51, 32)), "and arrived on the right");
        }
    }
}
