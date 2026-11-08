//! Compositing HDR layers on an output driven in HDR.
//!
//! An HDR output shows a described surface at the buffer's own precision
//! instead of its tone-mapped rendition: the scene is drawn twice, once with
//! every described surface replaced by black ([`crate::hdr::black`]) and once
//! with each replaced by its channel colour ([`crate::hdr::lit`]), and the
//! difference between the two in a layer's channel is how much of that layer
//! reaches each pixel — its rounded corners, the window's opacity and anything
//! drawn over it included. Each covering layer's plane is then sampled at its
//! own precision, decoded from the description its client attached into linear
//! BT.2020 light, and added at that weight; what the layers leave is SDR
//! content at the output's SDR white. The sum is encoded to PQ once.
//!
//! The same math runs on the CPU for the headless output
//! ([`tilo_color_pipeline::hdr_surface::composite_hdr_pixel`] with
//! [`CompositeTarget::Pq`](tilo_color_pipeline::hdr_surface::CompositeTarget));
//! the shader in [`super::color`] is this side of it.

use std::collections::HashMap;

use smithay::backend::allocator::Fourcc;
use smithay::backend::renderer::ImportMem as _;
use smithay::backend::renderer::gles::{GlesTexture, Uniform, UniformName, UniformType};
use smithay::utils::{Buffer, Size};

use super::Renderer;
use tilo_color_pipeline::hdr_surface::{
    HdrLayer, HdrPlane, HdrTexelFormat, MAX_HDR_LAYERS, f32_to_half,
};

/// Layers the shader composites; the recorder never hands it more.
pub(super) const LAYERS: usize = MAX_HDR_LAYERS;

/// The texture unit the first layer's plane is bound to; the pass's own textures take the units
/// below it.
pub(super) const PLANE_UNIT: i32 = 3;

/// One layer as the pass draws it: its plane's samples, and the uniforms that place it and say what
/// its samples mean.
pub(super) struct Light {
    /// The plane at the buffer's precision: one half-float RGBA texel per pixel, the signal in the
    /// layer's own transfer.
    pub texture: GlesTexture,
    /// This layer's uniforms, named for the slot it was given.
    pub uniforms: Vec<Uniform<'static>>,
}

/// The planes an HDR output draws, kept by plane id while they are on screen.
#[derive(Default)]
pub(super) struct Lights {
    kept: HashMap<u64, GlesTexture>,
}

impl Lights {
    /// The layers `layers` as the pass draws them, uploading the planes that are new since the last
    /// frame. An error means the frame is drawn without them, showing the layers' tone-mapped
    /// renditions as an SDR output does.
    pub(super) fn prepare(
        &mut self,
        renderer: &mut Renderer<'_>,
        layers: &[HdrLayer],
    ) -> Result<Vec<Light>, String> {
        // A plane that left the screen is forgotten with its texture.
        self.kept
            .retain(|id, _| layers.iter().any(|layer| layer.plane.id() == *id));
        let mut lights = Vec::with_capacity(layers.len().min(LAYERS));
        for layer in layers.iter().take(LAYERS) {
            let id = layer.plane.id();
            let texture = if let Some(texture) = self.kept.get(&id) {
                texture.clone()
            } else {
                let texture = upload(renderer, &layer.plane)?;
                self.kept.insert(id, texture.clone());
                texture
            };
            lights.push(Light {
                texture,
                uniforms: uniforms_of(lights.len(), layer),
            });
        }
        Ok(lights)
    }
}

/// The uniforms the pass's shader reads the layers through: the three planes, and per slot where
/// its texels land on screen and what they hold.
pub(super) fn uniform_names() -> Vec<UniformName<'static>> {
    let mut names = vec![UniformName::new("lit_tex", UniformType::_1i)];
    for slot in 0..LAYERS {
        names.push(UniformName::new(format!("plane{slot}"), UniformType::_1i));
    }
    for slot in 0..LAYERS {
        for (name, type_) in [
            ("layer_row_x", UniformType::_4f),
            ("layer_row_y", UniformType::_4f),
            ("layer_bounds", UniformType::_4f),
            ("layer_size", UniformType::_2f),
            ("layer_channel", UniformType::_1f),
            ("layer_transfer", UniformType::_1f),
            ("layer_row_r", UniformType::_3f),
            ("layer_row_g", UniformType::_3f),
            ("layer_row_b", UniformType::_3f),
        ] {
            names.push(UniformName::new(format!("{name}[{slot}]"), type_));
        }
    }
    names
}

/// The plane's samples as a half-float RGBA texture: the signal in its own transfer, alpha opaque.
fn upload(renderer: &mut Renderer<'_>, plane: &HdrPlane) -> Result<GlesTexture, String> {
    let width = i32::try_from(plane.width()).map_err(|_| format!("{} wide", plane.width()))?;
    let height = i32::try_from(plane.height()).map_err(|_| format!("{} high", plane.height()))?;
    let size = Size::<i32, Buffer>::from((width, height));
    let bytes = signal_bytes(plane);
    renderer
        .as_mut()
        .import_memory(&bytes, Fourcc::Abgr16161616f, size, false)
        .map_err(|error| format!("{error:?}"))
}

/// The plane's samples as half-float RGBA texels (red, green, blue, one),
/// normalised in the plane's own transfer. A plane already stored that way is
/// copied as it is, which is the only shape the compositor builds.
fn signal_bytes(plane: &HdrPlane) -> Vec<u8> {
    if plane.format() == HdrTexelFormat::Rgba16Float {
        return plane.bytes().to_vec();
    }
    let mut bytes = Vec::with_capacity(
        (plane.width() as usize)
            .saturating_mul(plane.height() as usize)
            .saturating_mul(8),
    );
    for y in 0..plane.height() {
        for x in 0..plane.width() {
            let [r, g, b] = plane.signal_at(x, y);
            for value in [r, g, b, 1.0] {
                bytes.extend_from_slice(&f32_to_half(value).to_le_bytes());
            }
        }
    }
    bytes
}

/// The uniforms of the layer in `slot` : where its texels are on screen, how much of it shows, and
/// how its signal turns into light.
#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    reason = "a layer's affine and extent are screen coordinates and texel counts, far inside f32's exact range"
)]
fn uniforms_of(slot: usize, layer: &HdrLayer) -> Vec<Uniform<'static>> {
    // Scene pixel → plane texel: x' = m00·x + m10·y + tx, and y' likewise.
    let [m00, m01, m10, m11, tx, ty] = layer.scene_to_image.as_coeffs();
    let [row_r, row_g, row_b] = layer.plane.signal().to_bt2020().m;
    let row = |name: &str, [x, y, translate]: [f32; 3]| {
        Uniform::new(format!("{name}[{slot}]"), [x, y, translate, 0.0_f32])
    };
    vec![
        row("layer_row_x", [m00 as f32, m10 as f32, tx as f32]),
        row("layer_row_y", [m01 as f32, m11 as f32, ty as f32]),
        Uniform::new(
            format!("layer_bounds[{slot}]"),
            [
                layer.bounds.x0 as f32,
                layer.bounds.y0 as f32,
                layer.bounds.x1 as f32,
                layer.bounds.y1 as f32,
            ],
        ),
        Uniform::new(
            format!("layer_size[{slot}]"),
            [layer.plane.width() as f32, layer.plane.height() as f32],
        ),
        Uniform::new(format!("layer_channel[{slot}]"), f32::from(layer.channel)),
        Uniform::new(
            format!("layer_transfer[{slot}]"),
            layer.plane.signal().transfer.shader_code() as f32,
        ),
        Uniform::new(format!("layer_row_r[{slot}]"), row_r),
        Uniform::new(format!("layer_row_g[{slot}]"), row_g),
        Uniform::new(format!("layer_row_b[{slot}]"), row_b),
    ]
}

#[cfg(test)]
#[allow(
    clippy::float_cmp,
    reason = "screen coordinates and half-float sample values here are exact"
)]
mod tests {
    use super::*;
    use neighbour_wire::colorimetry::Colorimetry;
    use std::sync::Arc;
    use tilo_color_pipeline::hdr_surface::{
        Affine, HdrLayer, HdrSurface, Rect, SurfaceSignal, half_to_f32,
    };
    use tilo_color_pipeline::hdr_tonemap::HdrTonemapConfig;

    /// A described 2×2 surface: the plane and the rendition a layer carries.
    fn described() -> HdrSurface {
        let signal =
            SurfaceSignal::for_colorimetry(Colorimetry::hdr10(), HdrTonemapConfig::default())
                .expect("PQ is HDR");
        HdrSurface::from_signal_rgb(2, 2, [[0.0, 0.5, 1.0]; 4], signal).expect("a plane")
    }

    /// A layer of `surface` 's plane drawn at `at` , one texel per pixel.
    fn layer(surface: &HdrSurface, channel: u8, at: (f64, f64)) -> HdrLayer {
        let image_to_scene = Affine::translate(at);
        HdrLayer {
            plane: Arc::clone(surface.plane()),
            image: surface.rendition().clone(),
            image_to_scene,
            scene_to_image: image_to_scene.inverse(),
            bounds: Rect::new(at.0, at.1, at.0 + 2.0, at.1 + 2.0),
            channel,
        }
    }

    fn value(uniforms: &[Uniform<'static>], name: &str) -> Vec<f32> {
        let uniform = uniforms
            .iter()
            .find(|uniform| *uniform.name == *name)
            .expect("a uniform by that name");
        match &uniform.value {
            smithay::backend::renderer::gles::UniformValue::_1f(v) => vec![*v],
            smithay::backend::renderer::gles::UniformValue::_2f(a, b) => vec![*a, *b],
            smithay::backend::renderer::gles::UniformValue::_3f(a, b, c) => vec![*a, *b, *c],
            smithay::backend::renderer::gles::UniformValue::_4f(a, b, c, d) => {
                vec![*a, *b, *c, *d]
            }
            other => panic!("{name} is not a float uniform: {other:?}"),
        }
    }

    #[test]
    fn a_layer_says_where_its_texels_land_and_what_they_hold() {
        let surface = described();
        let drawn = layer(&surface, 1, (40.0, 10.0));
        let uniforms = uniforms_of(2, &drawn);
        assert_eq!(
            value(&uniforms, "layer_bounds[2]"),
            [40.0, 10.0, 42.0, 12.0]
        );
        assert_eq!(value(&uniforms, "layer_size[2]"), [2.0, 2.0]);
        assert_eq!(value(&uniforms, "layer_channel[2]"), [1.0]);
        assert_eq!(
            value(&uniforms, "layer_transfer[2]"),
            [0.0],
            "PQ is the shader's code 0"
        );
        // A scene pixel maps to the plane texel under it: the layer's top left pixel centre is its
        // first texel's centre, and so on.
        let row_x = value(&uniforms, "layer_row_x[2]");
        let row_y = value(&uniforms, "layer_row_y[2]");
        let texel = |x: f32, y: f32| {
            [
                row_x[0] * x + row_x[1] * y + row_x[2],
                row_y[0] * x + row_y[1] * y + row_y[2],
            ]
        };
        assert_eq!(texel(40.5, 10.5), [0.5, 0.5]);
        assert_eq!(texel(41.5, 11.5), [1.5, 1.5]);
        // The plane's light is already BT.2020, the space the shader adds in.
        assert_eq!(
            value(&uniforms, "layer_row_r[2]"),
            [1.0, 0.0, 0.0],
            "BT.2020 primaries need no conversion"
        );
        assert_eq!(value(&uniforms, "layer_row_g[2]"), [0.0, 1.0, 0.0]);
        assert_eq!(value(&uniforms, "layer_row_b[2]"), [0.0, 0.0, 1.0]);
    }

    #[test]
    fn the_uniform_names_cover_every_slot_the_shader_reads() {
        let names: Vec<String> = uniform_names()
            .into_iter()
            .map(|name| name.name.into_owned())
            .collect();
        for expected in [
            "lit_tex",
            "plane0",
            "plane2",
            "layer_row_x[0]",
            "layer_bounds[2]",
            "layer_row_b[2]",
        ] {
            assert!(names.iter().any(|name| name == expected), "{expected}");
        }
        assert_eq!(
            names
                .iter()
                .filter(|name| name.starts_with("layer_"))
                .count(),
            LAYERS * 9
        );
    }

    #[test]
    fn a_plane_that_already_holds_half_floats_is_taken_as_it_is() {
        let surface = described();
        let plane = surface.plane();
        assert_eq!(plane.format(), HdrTexelFormat::Rgba16Float);
        let bytes = signal_bytes(plane);
        assert_eq!(bytes.len(), 2 * 2 * 8);
        let first = [0_usize, 2, 4, 6]
            .map(|at| half_to_f32(u16::from_le_bytes([bytes[at], bytes[at + 1]])));
        assert_eq!(first, [0.0, 0.5, 1.0, 1.0], "red, green, blue, opaque");
    }
}
