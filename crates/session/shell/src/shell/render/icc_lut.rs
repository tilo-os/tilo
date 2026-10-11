//! An output's ICC calibration on the GPU.
//!
//! The readback presenter calibrates an SDR output by running its qcms transform (sRGB to the
//! panel, perceptual intent) over every pixel on the CPU (`paint_one_scanout`). [`IccLut`]
//! samples that same transform once, when the profile is chosen, into a 3D table that the
//! SDR final pass (`OrientedCopy` in [`super::orientation`]) interpolates per pixel on the GPU
//! (`shell/output_color_lut.wgsl`), so a calibrated output renders direct. qcms stays the
//! only reader of ICC files: the table is qcms's own output at its grid points.
//!
//! qcms takes 8-bit input only, so the grid points are 8-bit codes: [`GRID_POINTS`] per axis,
//! spaced evenly in a gamma-2.2 domain (`linear^(1/2.2)`) rather than in code values, which
//! puts more of them near black, where a panel's curves bend hardest. A 256-entry table gives
//! each input code its exact position on that grid. Interpolation is tetrahedral and reads
//! texels with `textureLoad`, so the result does not depend on the GPU's filter precision.
//!
//! **Tolerance against qcms** (`tests::gpu_lut_matches_qcms_across_representative_profiles`,
//! and a CPU mirror of the shader over all 16,777,216 sRGB colours): profiles that keep sRGB
//! inside the panel's gamut (sRGB, Display P3, Adobe RGB at gamma 2.2, BT.2020 at gamma 2.4,
//! a table TRC) differ from qcms by at most one 8-bit code value per channel. A lut16
//! (`B2A0`) profile, which qcms itself interpolates on its own 33-point grid, differs by at
//! most 3. Where qcms clips sRGB into a smaller gamut, the clipping kinks cost at most 10
//! code values in saturated colours. Mean ΔE2000 on the panel stays at most 0.15 for all of
//! them. The largest ΔE2000, about 3, is a one-code difference in near-black, near-neutral
//! colours: the panel's own quantisation step.

use std::sync::atomic::{AtomicU64, Ordering};

use vello::wgpu;

/// Grid points per axis. The smallest count at which in-gamut profiles stay within one code
/// value of qcms; at 33 they reach 2.
pub const GRID_POINTS: u32 = 65;

/// The grid is even in `linear^(1 / GRID_GAMMA)`.
const GRID_GAMMA: f32 = 2.2;

/// Process-unique identities, so the GPU side can tell one table from another.
static NEXT_ID: AtomicU64 = AtomicU64::new(1);

/// sRGB to panel, sampled from an output's qcms transform.
pub struct IccLut {
    id: u64,
    /// Grid points per axis.
    size: u32,
    /// The grid position (`cell + fraction`) of each 8-bit input code.
    coordinates: Vec<f32>,
    /// `size³` RGBA8 texels: red varies fastest, then green, then blue.
    texels: Vec<u8>,
}

impl IccLut {
    /// Run `transform` (sRGB to panel, `RGBA8`) over the grid.
    pub fn from_transform(transform: &qcms::Transform) -> Self {
        let codes = grid_codes();
        let mut texels = Vec::with_capacity(codes.len().saturating_pow(3).saturating_mul(4));
        for &blue in &codes {
            for &green in &codes {
                for &red in &codes {
                    texels.extend_from_slice(&[red, green, blue, u8::MAX]);
                }
            }
        }
        transform.apply(&mut texels);
        Self {
            id: NEXT_ID.fetch_add(1, Ordering::Relaxed),
            size: u32::try_from(codes.len()).unwrap_or(0),
            coordinates: grid_coordinates(&codes),
            texels,
        }
    }

    /// Identifies this table for as long as the process runs.
    pub fn id(&self) -> u64 {
        self.id
    }

    /// Copy the table to `device` as the textures `output_color_lut.wgsl` reads.
    pub fn upload(&self, device: &wgpu::Device, queue: &wgpu::Queue) -> IccLutTextures {
        let textures = IccLutTextures::new(device, self.size);
        queue.write_texture(
            textures.lut.as_image_copy(),
            &self.texels,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(self.size.saturating_mul(4)),
                rows_per_image: Some(self.size),
            },
            textures.lut.size(),
        );
        let coordinates: Vec<u8> = self
            .coordinates
            .iter()
            .flat_map(|position| position.to_le_bytes())
            .collect();
        queue.write_texture(
            textures.coordinates.as_image_copy(),
            &coordinates,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: None,
                rows_per_image: None,
            },
            textures.coordinates.size(),
        );
        textures
    }
}

/// An [`IccLut`] on the GPU.
pub struct IccLutTextures {
    lut: wgpu::Texture,
    coordinates: wgpu::Texture,
    lut_view: wgpu::TextureView,
    coordinates_view: wgpu::TextureView,
}

impl IccLutTextures {
    /// Textures for a table of `size³` points, uninitialised (wgpu clears them to zero).
    /// With `size` 1 they are the placeholder an uncalibrated pass binds and never reads.
    pub fn new(device: &wgpu::Device, size: u32) -> Self {
        let lut = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("tilo-icc-lut"),
            size: wgpu::Extent3d {
                width: size,
                height: size,
                depth_or_array_layers: size,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D3,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        let coordinates = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("tilo-icc-lut-coordinates"),
            size: wgpu::Extent3d {
                width: 256,
                height: 1,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D1,
            format: wgpu::TextureFormat::R32Float,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        let lut_view = lut.create_view(&wgpu::TextureViewDescriptor::default());
        let coordinates_view = coordinates.create_view(&wgpu::TextureViewDescriptor::default());
        Self {
            lut,
            coordinates,
            lut_view,
            coordinates_view,
        }
    }

    /// The `texture_3d<f32>` the shader interpolates.
    pub fn lut_view(&self) -> &wgpu::TextureView {
        &self.lut_view
    }

    /// The `texture_1d<f32>` of grid positions per input code.
    pub fn coordinates_view(&self) -> &wgpu::TextureView {
        &self.coordinates_view
    }
}

/// `code` in the domain the grid is even in.
fn grid_domain(code: u8) -> f32 {
    let encoded = f32::from(code) / 255.0;
    let linear = if encoded <= 0.040_45 {
        encoded / 12.92
    } else {
        ((encoded + 0.055) / 1.055).powf(2.4)
    };
    linear.powf(GRID_GAMMA.recip())
}

/// The 8-bit codes nearest to [`GRID_POINTS`] even steps of [`grid_domain`], from 0 to 255.
fn grid_codes() -> Vec<u8> {
    let last = f32::from(u16::try_from(GRID_POINTS.saturating_sub(1)).unwrap_or(u16::MAX));
    let mut codes: Vec<u8> = (0..GRID_POINTS)
        .map(|point| {
            let target = f32::from(u16::try_from(point).unwrap_or(u16::MAX)) / last;
            (0..=u8::MAX)
                .min_by(|a, b| {
                    (grid_domain(*a) - target)
                        .abs()
                        .total_cmp(&(grid_domain(*b) - target).abs())
                })
                .unwrap_or(0)
        })
        .collect();
    codes.dedup();
    codes
}

/// The grid position of every 8-bit code: the index of the cell it falls in plus how far
/// across that cell it lies, in [`grid_domain`].
fn grid_coordinates(codes: &[u8]) -> Vec<f32> {
    let mut coordinates = vec![0.0_f32; 256];
    for (cell, pair) in (0_u8..=u8::MAX).zip(codes.windows(2)) {
        let &[low, high] = pair else {
            continue;
        };
        let (start, end) = (grid_domain(low), grid_domain(high));
        for code in low..=high {
            if let Some(slot) = coordinates.get_mut(usize::from(code)) {
                *slot = f32::from(cell) + (grid_domain(code) - start) / (end - start);
            }
        }
    }
    coordinates
}

#[cfg(test)]
pub(crate) mod tests;
