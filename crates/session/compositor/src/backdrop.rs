//! The blurred backdrop translucent chrome is drawn over.
//!
//! Floating chrome (quick settings, menus, the launcher, notifications) is
//! made of a material that shows a blur of what lies behind it. A hosted
//! app cannot see behind itself: the windows and the surfaces below are the
//! compositor's. So a hosted surface names the regions it draws the material
//! in ([`BackdropRequest`], from [`crate::SurfaceHost::backdrops`]) and the
//! compositor hands back, after each frame, a blurred image of what lies
//! below each ([`BackdropImage`], [`crate::SurfaceHost::set_backdrops`]),
//! drawn at the request's texel size, only when what lies below changed.
//! The app draws it in its material, under its tint, the next frame.
//!
//! What lies below is drawn on the GPU at the request's texel size and
//! blurred there: halved until its deviation is a few texels, blurred
//! separably with a Gaussian and scaled back. The backdrop is a
//! low-frequency signal, so this is indistinguishable from a full-resolution
//! blur at a fraction of the work. Only the finished image is read back, at
//! the texel size the app asked for.

#![allow(
    clippy::arithmetic_side_effects,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::cast_precision_loss,
    reason = "image arithmetic on texel indices and colour values of small backdrop images"
)]

use smithay::backend::allocator::Fourcc;
use smithay::backend::egl::EGLContext;
use smithay::backend::renderer::damage::OutputDamageTracker;
use smithay::backend::renderer::element::RenderElement;
use smithay::backend::renderer::gles::{
    GlesRenderer, GlesTexProgram, GlesTexture, Uniform, UniformName, UniformType, ffi,
};
use smithay::backend::renderer::{Bind, ExportMem, Frame, Offscreen, Renderer, Texture};
use smithay::utils::{Buffer, Logical, Physical, Point, Rectangle, Size, Transform};

use crate::backend::BackendError;
use crate::render::{BACKGROUND, below_host};
use crate::state::State;

/// The shader each of the blur's two passes runs.
const SHADER: &str = include_str!("blur.glsl");

/// Pairs of texels one pass reads either side of its centre: the blur reaches sixteen texels, past
/// the three deviations it stops at.
const PAIRS: usize = 8;

/// The blur stops halving once its deviation is at most this many texels.
const MAX_SIGMA: f64 = 4.0;

/// Upper bound on halvings.
const MAX_HALVINGS: u32 = 6;

/// A region a hosted surface draws its material in.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BackdropRequest {
    /// The region, logical `(x, y, width, height)` .
    pub rect: (f64, f64, f64, f64),
    /// The size of the image it takes, in texels.
    pub texels: (u32, u32),
    /// The blur's standard deviation, in texels.
    pub blur: f64,
}

/// What lies below a [`BackdropRequest`]'s region, blurred: rows of premultiplied R, G, B, A at its
/// texel size.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BackdropImage {
    pub rgba: Vec<u8>,
    pub texels: (u32, u32),
}

/// Which GL context made a texture or a program: neither is usable in another.
type Context = usize;

fn context_of(renderer: &GlesRenderer) -> Context {
    let context: &EGLContext = renderer.egl_context();
    std::ptr::from_ref(context) as usize
}

/// What one region's last image saw, to skip unchanged ones.
struct Watch {
    key: (usize, usize),
    request: BackdropRequest,
    /// The context the images below were made in.
    context: Context,
    tracker: OutputDamageTracker,
    /// The region as drawn and blurred, at the sizes it takes along the way.
    images: Option<Images>,
}

/// One region's image: what lies below it at the request's texel size, the halved copies the blur
/// runs on, and the pass the other direction writes.
struct Images {
    /// The texel size of each level: the request's, then each half the one before.
    sizes: Vec<(u32, u32)>,
    /// What lies below, drawn at `sizes[0]` and blurred from `sizes.last()` back up to it.
    levels: Vec<GlesTexture>,
    /// The pass the other direction writes while a level is blurred.
    across: GlesTexture,
}

/// The regions served so far.
#[derive(Default)]
pub(crate) struct Backdrops {
    /// The blur's two passes, in the context they were compiled in.
    program: Option<(Context, GlesTexProgram)>,
    watches: Vec<Watch>,
}

/// Draw what lies below hosted surface `host`'s region `slot` and blur it;
/// `None` when nothing there changed since its last image.
pub(crate) fn draw(
    renderer: &mut GlesRenderer,
    state: &mut State,
    (host, slot): (usize, usize),
    request: &BackdropRequest,
) -> Result<Option<BackdropImage>, BackendError> {
    let (x, y, width, height) = request.rect;
    let texels = request.texels;
    if width <= 0.0 || height <= 0.0 || texels.0 == 0 || texels.1 == 0 {
        return Ok(None);
    }
    let scale = f64::from(texels.0) / width;
    let size = Size::<i32, Physical>::from((
        i32::try_from(texels.0).unwrap_or(i32::MAX),
        i32::try_from(texels.1).unwrap_or(i32::MAX),
    ));
    let origin = Point::<f64, Logical>::from((x, y));
    let elements = below_host(
        renderer,
        state,
        host,
        origin,
        scale,
        Rectangle::from_size(size),
    );
    let Backdrops { program, watches } = &mut state.backdrops;
    let context = context_of(renderer);
    // A texture is only usable in the context that made it, so a region drawn on another one starts
    // over.
    let fresh = !watches.iter().any(|watch| {
        watch.key == (host, slot) && watch.request == *request && watch.context == context
    });
    if fresh {
        watches.retain(|watch| watch.key != (host, slot));
        watches.push(Watch {
            key: (host, slot),
            request: *request,
            context,
            tracker: OutputDamageTracker::new(size, scale, Transform::Normal),
            images: None,
        });
    }
    let Some(watch) = watches.iter_mut().find(|watch| watch.key == (host, slot)) else {
        return Ok(None);
    };
    let changed = match watch.tracker.damage_output(1, &elements) {
        Ok((Some(damage), _)) => !damage.is_empty(),
        Ok((None, _)) => false,
        Err(_) => true,
    };
    if !fresh && !changed {
        return Ok(None);
    }
    let halvings = halvings(request.blur, texels);
    if watch
        .images
        .as_ref()
        .is_none_or(|images| images.sizes.len() != usize::try_from(halvings).unwrap_or(0) + 1)
    {
        watch.images = Some(Images::new(renderer, texels, halvings)?);
    }
    let Some(images) = watch.images.as_mut() else {
        return Ok(None);
    };
    let Some(image) = images.levels.first_mut() else {
        return Ok(None);
    };
    draw_below(renderer, &elements, size, scale, image)?;
    blur_image(renderer, images, request.blur, context, program)?;
    let rgba = read_back(renderer, images, texels)?;
    Ok(Some(BackdropImage { rgba, texels }))
}

/// Halve the image down to the level its deviation is a few texels at, blur it there, and scale the
/// blur back up to the image's own size.
fn blur_image(
    renderer: &mut GlesRenderer,
    images: &mut Images,
    sigma: f64,
    context: Context,
    program: &mut Option<(Context, GlesTexProgram)>,
) -> Result<(), BackendError> {
    for level in 1..images.levels.len() {
        scale_between(renderer, images, level - 1, level)?;
    }
    if sigma > 0.0 {
        let level = images.levels.len().saturating_sub(1);
        let (columns, rows) = images.sizes.get(level).copied().unwrap_or((1, 1));
        let taps = taps(sigma / f64::from(1_u32 << u32::try_from(level).unwrap_or(0)));
        if program.as_ref().is_none_or(|(made, _)| *made != context) {
            let names = uniform_names();
            let compiled = renderer
                .compile_custom_texture_shader(SHADER, &names)
                .map_err(|error| BackendError(format!("blur shader: {error:?}")))?;
            *program = Some((context, compiled));
        }
        let Some((_, program)) = program.as_ref() else {
            return Ok(());
        };
        let across = (1.0 / columns.max(1) as f32, 0.0);
        let down = (0.0, 1.0 / rows.max(1) as f32);
        let Some(image) = images.levels.get(level) else {
            return Ok(());
        };
        blur_pass(renderer, image, &mut images.across, program, &taps, across)?;
        let Some(image) = images.levels.get_mut(level) else {
            return Ok(());
        };
        blur_pass(renderer, &images.across, image, program, &taps, down)?;
    }
    for level in (0..images.levels.len().saturating_sub(1)).rev() {
        scale_between(renderer, images, level + 1, level)?;
    }
    Ok(())
}

/// Copy the image back at the texel size the request asked for.
fn read_back(
    renderer: &mut GlesRenderer,
    images: &Images,
    texels: (u32, u32),
) -> Result<Vec<u8>, BackendError> {
    let Some(image) = images.levels.first() else {
        return Ok(Vec::new());
    };
    let region = Rectangle::<i32, Buffer>::from_size(Size::from((
        i32::try_from(texels.0).unwrap_or(i32::MAX),
        i32::try_from(texels.1).unwrap_or(i32::MAX),
    )));
    let mapping = renderer
        .copy_texture(image, region, Fourcc::Abgr8888)
        .map_err(|error| BackendError(format!("read the backdrop back: {error:?}")))?;
    Ok(renderer
        .map_texture(&mapping)
        .map_err(|error| BackendError(format!("map the backdrop: {error:?}")))?
        .to_vec())
}

impl dyn crate::Backend {
    /// Hand every hosted surface that draws translucent material the backdrops that changed.
    pub(crate) fn serve_backdrops(&mut self, state: &mut State) {
        let requests: Vec<(usize, Vec<BackdropRequest>)> = state
            .hosts
            .iter()
            .enumerate()
            .map(|(index, host)| (index, host.backdrops()))
            .collect();
        let live: Vec<(usize, usize)> = requests
            .iter()
            .flat_map(|(host, list)| (0..list.len()).map(move |slot| (*host, slot)))
            .collect();
        state
            .backdrops
            .watches
            .retain(|watch| live.contains(&watch.key));
        for (host, list) in requests {
            if list.is_empty() {
                continue;
            }
            let images: Vec<Option<BackdropImage>> = list
                .iter()
                .enumerate()
                .map(
                    |(slot, request)| match self.backdrop(state, (host, slot), request) {
                        Ok(image) => image,
                        Err(error) => {
                            tracing::debug!(%error, "backdrop not drawn");
                            None
                        }
                    },
                )
                .collect();
            if images.iter().any(Option::is_some)
                && let Some(surface) = state.hosts.get_mut(host)
            {
                surface.set_backdrops(images);
            }
        }
    }
}

impl Images {
    /// The textures a request of `texels` takes: its own size, then each level half the one before,
    /// as many as `halvings` says.
    fn new(
        renderer: &mut GlesRenderer,
        texels: (u32, u32),
        halvings: u32,
    ) -> Result<Self, BackendError> {
        let mut sizes = vec![texels];
        for _ in 0..halvings {
            let Some((columns, rows)) = sizes.last().copied() else {
                break;
            };
            sizes.push(((columns / 2).max(1), (rows / 2).max(1)));
        }
        let mut levels = Vec::with_capacity(sizes.len());
        for size in &sizes {
            levels.push(texture(renderer, *size)?);
        }
        let across = texture(renderer, sizes.last().copied().unwrap_or(texels))?;
        Ok(Self {
            sizes,
            levels,
            across,
        })
    }
}

/// A zeroed image of `size` texels to draw a backdrop in.
fn texture(
    renderer: &mut GlesRenderer,
    (columns, rows): (u32, u32),
) -> Result<GlesTexture, BackendError> {
    let size = Size::<i32, Buffer>::from((
        i32::try_from(columns).unwrap_or(i32::MAX),
        i32::try_from(rows).unwrap_or(i32::MAX),
    ));
    <GlesRenderer as Offscreen<GlesTexture>>::create_buffer(renderer, Fourcc::Abgr8888, size)
        .map_err(|error| BackendError(format!("backdrop image: {error:?}")))
}

/// How many times a backdrop of `sigma` texels is halved before it is blurred, and its size along
/// the way.
fn halvings(sigma: f64, (columns, rows): (u32, u32)) -> u32 {
    let mut halvings = 0;
    while sigma > 0.0
        && sigma / f64::from(1_u32 << halvings) > MAX_SIGMA
        && halvings < MAX_HALVINGS
        && (columns >> (halvings + 1)) > 0
        && (rows >> (halvings + 1)) > 0
    {
        halvings += 1;
    }
    halvings
}

/// The taps of a Gaussian of `sigma` texels: the centre texel's weight, then each pair's, the pair
/// weighing the two texels one fetch reads.
fn taps(sigma: f64) -> [f32; PAIRS + 1] {
    let gauss = |d: f64| (-(d * d) / (2.0 * sigma * sigma)).exp();
    let mut taps = [0.0_f32; PAIRS + 1];
    let mut total = 0.0_f64;
    for (pair, tap) in taps.iter_mut().enumerate() {
        // The centre texel stands alone; each tap past it is a pair.
        let (weight, sides) = if pair == 0 {
            (gauss(0.0), 1.0)
        } else {
            let lower = pair as f64 * 2.0 - 1.0;
            (gauss(lower) + gauss(lower + 1.0), 2.0)
        };
        *tap = weight as f32;
        total += weight * sides;
    }
    // Whatever the truncation left out, the taps divide the light between them: both sides of the
    // centre read the same taps.
    if total > 0.0 {
        for tap in &mut taps {
            *tap = (f64::from(*tap) / total) as f32;
        }
    }
    taps
}

/// The uniforms the blur's shader declares.
fn uniform_names() -> Vec<UniformName<'static>> {
    let mut names = Vec::with_capacity(PAIRS + 2);
    names.push(UniformName::new("centre", UniformType::_1f));
    for pair in 1..=PAIRS {
        names.push(UniformName::new(format!("pair{pair}"), UniformType::_1f));
    }
    names.push(UniformName::new("step", UniformType::_2f));
    names
}

/// Draw what lies below `elements` into `target` , over the wallpaper.
fn draw_below<E>(
    renderer: &mut GlesRenderer,
    elements: &[E],
    size: Size<i32, Physical>,
    scale: f64,
    target: &mut GlesTexture,
) -> Result<(), BackendError>
where
    E: RenderElement<GlesRenderer>,
{
    let mut bound = renderer
        .bind(target)
        .map_err(|error| BackendError(format!("bind the backdrop: {error:?}")))?;
    let mut tracker = OutputDamageTracker::new(size, scale, Transform::Normal);
    tracker
        .render_output(renderer, &mut bound, 0, elements, BACKGROUND)
        .map_err(|error| BackendError(error.to_string()))?;
    Ok(())
}

/// One pass of the blur along one axis, `taps` wide.
fn blur_pass(
    renderer: &mut GlesRenderer,
    source: &GlesTexture,
    target: &mut GlesTexture,
    program: &GlesTexProgram,
    taps: &[f32; PAIRS + 1],
    step: (f32, f32),
) -> Result<(), BackendError> {
    let mut uniforms = Vec::with_capacity(PAIRS + 2);
    uniforms.push(Uniform::new("centre", taps[0]));
    for (pair, weight) in taps.iter().enumerate().skip(1) {
        uniforms.push(Uniform::new(format!("pair{pair}"), *weight));
    }
    uniforms.push(Uniform::new("step", step));
    resample(renderer, source, target, Some(program), &uniforms)
}

/// Draw the level `low` onto the level `high` , scaling it as it goes: the texture's linear filter
/// averages the texels it covers, so halving is the box average it should be and doubling
/// interpolates back.
fn scale_between(
    renderer: &mut GlesRenderer,
    images: &mut Images,
    source: usize,
    target: usize,
) -> Result<(), BackendError> {
    if source == target {
        return Ok(());
    }
    let (nearer, farther) = (source.min(target), source.max(target));
    let (below, above) = images.levels.split_at_mut(farther);
    let Some(nearer) = below.get_mut(nearer) else {
        return Ok(());
    };
    let Some(farther) = above.first_mut() else {
        return Ok(());
    };
    if source < target {
        resample(renderer, nearer, farther, None, &[])
    } else {
        resample(renderer, farther, nearer, None, &[])
    }
}

/// Draw all of `source` over all of `target` with `program` , or the plain texture shader when
/// there is none, reading it edge-clamped.
#[allow(
    clippy::significant_drop_tightening,
    reason = "a frame ends by finishing, which hands the graphics context back"
)]
fn resample(
    renderer: &mut GlesRenderer,
    source: &GlesTexture,
    target: &mut GlesTexture,
    program: Option<&GlesTexProgram>,
    uniforms: &[Uniform<'_>],
) -> Result<(), BackendError> {
    let size = target.size();
    let physical = Size::<i32, Physical>::from((size.w, size.h));
    let dst = Rectangle::<i32, Physical>::from_size(physical);
    let src = Rectangle::<f64, Buffer>::from_size(source.size().to_f64());
    let mut bound = renderer
        .bind(target)
        .map_err(|error| BackendError(format!("bind a backdrop level: {error:?}")))?;
    let mut frame = renderer
        .render(&mut bound, physical, Transform::Normal)
        .map_err(|error| BackendError(format!("draw a backdrop level: {error:?}")))?;
    frame
        .with_context(|gl| {
            // The image's edges are its own: a blur reads past them at the
            // edge texel, never at the far side of the image.
            // SAFETY: the renderer's context is current inside a frame, and
            // the texture is bound again for the draw itself.
            unsafe {
                gl.BindTexture(ffi::TEXTURE_2D, source.tex_id());
                for parameter in [ffi::TEXTURE_WRAP_S, ffi::TEXTURE_WRAP_T] {
                    gl.TexParameteri(
                        ffi::TEXTURE_2D,
                        parameter,
                        i32::try_from(ffi::CLAMP_TO_EDGE).unwrap_or(0),
                    );
                }
                gl.BindTexture(ffi::TEXTURE_2D, 0);
            }
        })
        .map_err(|error| BackendError(format!("clamp a backdrop: {error:?}")))?;
    frame
        .render_texture_from_to(
            source,
            src,
            dst,
            &[dst],
            &[],
            Transform::Normal,
            1.0,
            program,
            uniforms,
        )
        .map_err(|error| BackendError(format!("draw a backdrop level: {error:?}")))?;
    // The passes that follow draw into the same context, so they see this one's work without
    // waiting for the GPU: the fence goes unused.
    let _fence = frame
        .finish()
        .map_err(|error| BackendError(format!("finish a backdrop level: {error:?}")))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_blur_taps_divide_the_light_between_them() {
        let taps = taps(2.5);
        let total = taps[0] + 2.0 * taps[1..].iter().sum::<f32>();
        assert!((total - 1.0).abs() < 1e-5, "they sum to one: {total}");
        // A pair's weight covers the two texels its fetch reads.
        let texel = |pair: usize| if pair == 0 { taps[0] } else { taps[pair] / 2.0 };
        assert!(
            texel(0) > texel(1) && texel(1) > texel(2),
            "centred: {taps:?}"
        );
        assert!(taps[PAIRS] > 0.0, "and reach past the last pair");
    }

    #[test]
    fn the_blur_halves_until_its_deviation_is_a_few_texels() {
        assert_eq!(halvings(3.0, (40, 20)), 0, "a small deviation runs whole");
        assert_eq!(halvings(12.0, (640, 400)), 2, "12 texels: 6, then 3");
        assert_eq!(halvings(0.0, (40, 20)), 0, "nothing to blur");
        assert_eq!(
            halvings(12.0, (2, 2)),
            1,
            "an image only halves while it can"
        );
    }
}
