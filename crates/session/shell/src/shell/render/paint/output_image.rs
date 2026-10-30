//! One output's pixels on the CPU, for an output presented by readback.
//!
//! An output the renderer cannot scan out directly (its display hangs off another GPU, as an
//! external port wired to a laptop's discrete GPU does) is shown by copying pixels into its
//! dumb buffers. [`VelloDrmBridge::refresh_output_image`] renders that output alone, through
//! its [`OutputView`], into its own retained surface, as the direct path does, and reads back
//! only what changed on it: the frame's damage on that output when the surface repaired it
//! sparsely, the whole output otherwise.
//!
//! Rendering the bounding box of every output and reading it back whole (the combined
//! readback) costs every output's pixels for each one shown this way: on a laptop panel beside
//! a 4K monitor it rendered the panel a second time, rasterised the empty box below it, and
//! read 50 MB back each frame, even for a caret blinking on the other display.

use std::sync::Arc;
use std::time::Instant;

use vello::{FineDamageRect, Scene, wgpu};

use super::output_view::backdrop_through;
use super::{
    BridgeError, BridgePhaseTiming, CompletionPhase, OutputView, SceneDamage, VelloDrmBridge,
};
use crate::shell::render::OutputId;

/// The most rectangles one refresh reads back on their own; more are merged first.
const MAX_READ_RECTS: usize = 8;

/// An output's tight RGBA image and the revision of the scene it shows.
#[derive(Default)]
pub(super) struct OutputImage {
    rgba: Vec<u8>,
    /// The revision the image shows, which the surface's pixels hold too; `None` while it
    /// shows nothing current (never read, or the surface rendered without a readback since).
    revision: Option<u64>,
    /// Reused for every readback of this output.
    staging: Option<wgpu::Buffer>,
}

impl OutputImage {
    /// The image no longer shows the surface: the next refresh reads it whole.
    pub(super) fn forget(&mut self) {
        self.revision = None;
    }
}

/// What [`VelloDrmBridge::refresh_output_image`] changed in an output's image.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OutputImageChange {
    /// Nothing: no damage falls on the output, and its image already showed the previous
    /// frame.
    Unchanged,
    /// Only pixels inside this rectangle, in the output's own pixels.
    Region(FineDamageRect),
    /// The whole image.
    Whole,
}

impl VelloDrmBridge {
    /// Bring the image of the output `view` shows up to `scene`: render the output alone into
    /// its retained surface (sparse when the surface holds the previous revision), then read
    /// back what the render changed. The image is [`Self::output_image`].
    ///
    /// The surface is the one [`Self::paint_output_until`] renders for the output, so an
    /// output that moves between the direct path and this one keeps it, and reads back whole
    /// the first time.
    ///
    /// # Errors
    ///
    /// The surface cannot be allocated, or the render or the readback fails. The image then
    /// shows nothing current, and the next refresh renders and reads the output whole.
    pub fn refresh_output_image(
        &mut self,
        scene: &Scene,
        view: &OutputView,
        damage: SceneDamage<'_>,
        deadline: Instant,
    ) -> Result<OutputImageChange, BridgeError> {
        if let Some(error) = self.take_device_error() {
            return Err(error);
        }
        if self.target.poisoned {
            let (width, height) = self.target.size;
            self.reconstruct_target(width, height)?;
        }
        let mut surface = self.take_output_surface(view)?;
        let mapped = damage.regions.and_then(|regions| view.damage(regions));
        let current = surface.retained_target_valid
            && surface.image.revision.is_some()
            && surface.image.revision == surface.last_successful_revision
            && surface.image.rgba.len() == tight_len(view.size);
        // The image, the surface and what the output shows already are this frame.
        if current
            && mapped.as_ref().is_some_and(Vec::is_empty)
            && damage.previous_revision.is_some()
            && surface.last_successful_revision == damage.previous_revision
            && surface.owed_damage.is_empty()
        {
            surface.last_successful_revision = Some(damage.revision);
            surface.image.revision = Some(damage.revision);
            self.output_surfaces.push((view.id, surface));
            return Ok(OutputImageChange::Unchanged);
        }
        let mut view_scene = Scene::new();
        view_scene.append(scene, Some(view.transform));
        let backdrop = self.target.backdrop.clone();
        self.target.backdrop = backdrop
            .as_deref()
            .map(|frame| Arc::new(backdrop_through(frame, view.transform)));
        surface.swap(&mut self.target);
        // Scroll copies are planned for a lone output's view of the scene, not this one.
        self.target.stage_scroll_copies(None, None);
        let regions = self.target.take_owed_damage(mapped.as_deref()).or(mapped);
        self.last_phase_timing = None;
        let render_started = Instant::now();
        let rendered = self.target.render_until(
            &view_scene,
            SceneDamage {
                revision: damage.revision,
                previous_revision: damage.previous_revision,
                regions: regions.as_deref(),
            },
            deadline,
        );
        let read = rendered.and_then(|stats| {
            let sparse = stats.sparse_attempts > 0 && stats.full_attempts == 0;
            let change = match regions.as_deref() {
                Some(regions) if current && sparse => {
                    bounds(regions).map_or(OutputImageChange::Whole, OutputImageChange::Region)
                }
                _ => OutputImageChange::Whole,
            };
            let rects = match (change, regions) {
                (OutputImageChange::Region(_), Some(regions)) => {
                    super::coalesce_damage(regions, MAX_READ_RECTS)
                }
                _ => vec![FineDamageRect {
                    x: 0,
                    y: 0,
                    width: view.size.0,
                    height: view.size.1,
                }],
            };
            let submit_us = render_started.elapsed().as_micros();
            let read = read_into(
                &self.target.device,
                &self.target.queue,
                &self.target.texture,
                &mut surface.image,
                view.size,
                &rects,
            )?;
            if self.phase_timing_enabled {
                self.last_phase_timing = Some(BridgePhaseTiming {
                    submit_us,
                    completed_readback_map_us: read.map_us,
                    cpu_copy_swizzle_us: read.copy_us,
                    readback_bytes: read.bytes,
                    cpu_output_bytes: read.bytes,
                    ..BridgePhaseTiming::default()
                });
            }
            Ok(change)
        });
        surface.swap(&mut self.target);
        self.target.backdrop = backdrop;
        match &read {
            Ok(_) => surface.image.revision = surface.last_successful_revision,
            Err(_) => surface.image.forget(),
        }
        self.output_surfaces.push((view.id, surface));
        read
    }

    /// The output skips this frame: its previous flip has not retired, so it could not show
    /// it. When its surface and image hold the previous revision, the frame's damage on it
    /// is owed to its next refresh, which renders and reads it back with its own; otherwise
    /// the next refresh is whole.
    pub fn defer_output_image(&mut self, view: &OutputView, damage: Option<SceneDamage<'_>>) {
        let Some((_, surface)) = self
            .output_surfaces
            .iter_mut()
            .find(|(id, _)| *id == view.id)
        else {
            return;
        };
        let current = surface.retained_target_valid
            && surface.size == view.size
            && surface.transform == view.transform
            && surface.image.revision.is_some()
            && surface.image.revision == surface.last_successful_revision;
        let mapped = damage.and_then(|damage| {
            let previous = damage.previous_revision?;
            let mapped = view.damage(damage.regions?)?;
            (surface.last_successful_revision == Some(previous)).then_some((damage, mapped))
        });
        match mapped {
            Some((damage, mapped)) if current => {
                let owed = std::mem::take(&mut surface.owed_damage);
                surface.owed_damage = super::coalesce_damage(
                    [owed, mapped].concat(),
                    super::output_view::MAX_OWED_RECTS,
                );
                surface.last_successful_revision = Some(damage.revision);
                surface.image.revision = Some(damage.revision);
            }
            _ => surface.invalidate(),
        }
    }

    /// The tight RGBA image [`Self::refresh_output_image`] keeps for output `id`, upright,
    /// when it shows the surface's pixels.
    pub fn output_image(&self, id: OutputId) -> Option<&[u8]> {
        self.output_surfaces
            .iter()
            .find(|(output, _)| *output == id)
            .map(|(_, surface)| &surface.image)
            .filter(|image| image.revision.is_some())
            .map(|image| image.rgba.as_slice())
    }
}

/// The smallest rectangle holding every one of `rects`, or `None` for none.
fn bounds(rects: &[FineDamageRect]) -> Option<FineDamageRect> {
    let first = rects.first()?;
    let (mut x0, mut y0) = (first.x, first.y);
    let (mut x1, mut y1) = (x0, y0);
    for rect in rects {
        x0 = x0.min(rect.x);
        y0 = y0.min(rect.y);
        x1 = x1.max(rect.x.saturating_add(rect.width));
        y1 = y1.max(rect.y.saturating_add(rect.height));
    }
    Some(FineDamageRect {
        x: x0,
        y: y0,
        width: x1.saturating_sub(x0),
        height: y1.saturating_sub(y0),
    })
}

fn tight_len((width, height): (u32, u32)) -> usize {
    usize::try_from(width)
        .unwrap_or(0)
        .saturating_mul(usize::try_from(height).unwrap_or(0))
        .saturating_mul(4)
}

/// What one readback moved, and how long it waited and copied.
struct Read {
    /// Pixel bytes copied out of the texture.
    bytes: u64,
    /// From submitting the copy to the mapped buffer.
    map_us: u128,
    /// Copying the mapped rows into the image.
    copy_us: u128,
}

/// Copy `rects` of `texture` (a `size` RGBA surface) into `image` in one submission: each
/// rectangle into its own stretch of the image's staging buffer, mapped once.
fn read_into(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    texture: &wgpu::Texture,
    image: &mut OutputImage,
    size: (u32, u32),
    rects: &[FineDamageRect],
) -> Result<Read, BridgeError> {
    let started = Instant::now();
    let (mut placed, mut used, capacity) = place(rects, size)?;
    let whole_row = size
        .0
        .saturating_mul(4)
        .div_ceil(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT)
        .saturating_mul(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT);
    let whole = FineDamageRect {
        x: 0,
        y: 0,
        width: size.0,
        height: size.1,
    };
    let needed = tight_len(size);
    if image.rgba.len() != needed {
        image.rgba.clear();
        image.rgba.resize(needed, 0);
        image.revision = None;
        if placed.first().map(|(rect, _, _)| *rect) != Some(whole) {
            placed = vec![(whole, 0, whole_row)];
            used = capacity;
        }
    }
    let staging = match &image.staging {
        Some(buffer) if buffer.size() == capacity => buffer.clone(),
        _ => {
            let buffer = device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("tilo-desktop-drm-output-readback"),
                size: capacity,
                usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
                mapped_at_creation: false,
            });
            image.staging = Some(buffer.clone());
            buffer
        }
    };
    let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
        label: Some("tilo-desktop-drm-output-readback"),
    });
    for &(rect, offset, padded) in &placed {
        encoder.copy_texture_to_buffer(
            wgpu::TexelCopyTextureInfo {
                texture,
                mip_level: 0,
                origin: wgpu::Origin3d {
                    x: rect.x,
                    y: rect.y,
                    z: 0,
                },
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::TexelCopyBufferInfo {
                buffer: &staging,
                layout: wgpu::TexelCopyBufferLayout {
                    offset,
                    bytes_per_row: Some(padded),
                    rows_per_image: Some(rect.height),
                },
            },
            wgpu::Extent3d {
                width: rect.width,
                height: rect.height,
                depth_or_array_layers: 1,
            },
        );
    }
    let submission = queue.submit(Some(encoder.finish()));
    let slice = staging.slice(..used);
    map_wait(device, &staging, &slice, submission)?;
    let mapped_at = Instant::now();
    let copied = {
        let mapped = slice.get_mapped_range();
        copy_rows(&mapped, &mut image.rgba, size.0, &placed)
    };
    staging.unmap();
    let bytes = copied?;
    Ok(Read {
        bytes,
        map_us: mapped_at.saturating_duration_since(started).as_micros(),
        copy_us: mapped_at.elapsed().as_micros(),
    })
}

/// Copy each placed rectangle's rows out of the mapped staging buffer into the tight image
/// `width` pixels wide. Returns the pixel bytes copied.
fn copy_rows(
    mapped: &[u8],
    rgba: &mut [u8],
    width: u32,
    placed: &[(FineDamageRect, u64, u32)],
) -> Result<u64, BridgeError> {
    let stride = usize::try_from(width).unwrap_or(0).saturating_mul(4);
    let mut bytes = 0_u64;
    for &(rect, offset, padded) in placed {
        let row = usize::try_from(rect.width).unwrap_or(0).saturating_mul(4);
        let x = usize::try_from(rect.x).unwrap_or(0).saturating_mul(4);
        for line in 0..usize::try_from(rect.height).unwrap_or(0) {
            let src = usize::try_from(offset)
                .unwrap_or(usize::MAX)
                .saturating_add(line.saturating_mul(usize::try_from(padded).unwrap_or(0)));
            let dst = usize::try_from(rect.y)
                .unwrap_or(0)
                .saturating_add(line)
                .saturating_mul(stride)
                .saturating_add(x);
            let (Some(from), Some(to)) = (
                mapped.get(src..src.saturating_add(row)),
                rgba.get_mut(dst..dst.saturating_add(row)),
            ) else {
                return Err(BridgeError::Map(
                    "output readback row is outside its buffer".to_owned(),
                ));
            };
            to.copy_from_slice(from);
        }
        bytes = bytes.saturating_add(
            u64::from(rect.width)
                .saturating_mul(u64::from(rect.height))
                .saturating_mul(4),
        );
    }
    Ok(bytes)
}

/// Where each of `rects` inside a `size` surface lands in the staging buffer: its rows at
/// the row pitch a copy needs, one rectangle after the other; the whole surface when they
/// would need more room than it. Returns the placements, the bytes they use, and the
/// buffer's capacity (the whole surface).
#[allow(
    clippy::type_complexity,
    reason = "one placement is a rectangle, its offset and its row pitch"
)]
fn place(
    rects: &[FineDamageRect],
    size: (u32, u32),
) -> Result<(Vec<(FineDamageRect, u64, u32)>, u64, u64), BridgeError> {
    let align = wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
    let whole_row = size
        .0
        .saturating_mul(4)
        .div_ceil(align)
        .saturating_mul(align);
    let capacity = u64::from(whole_row).saturating_mul(u64::from(size.1));
    if capacity == 0 {
        return Err(BridgeError::InvalidSize("empty output readback".to_owned()));
    }
    let inside = |rect: &FineDamageRect| {
        rect.width > 0
            && rect.height > 0
            && rect
                .x
                .checked_add(rect.width)
                .is_some_and(|x1| x1 <= size.0)
            && rect
                .y
                .checked_add(rect.height)
                .is_some_and(|y1| y1 <= size.1)
    };
    // Each rectangle's rows at the row pitch a copy needs, one after the other.
    let mut placed = Vec::with_capacity(rects.len());
    let mut used = 0_u64;
    for rect in rects.iter().filter(|rect| inside(rect)) {
        let padded = rect
            .width
            .saturating_mul(4)
            .div_ceil(align)
            .saturating_mul(align);
        placed.push((*rect, used, padded));
        used = used.saturating_add(u64::from(padded).saturating_mul(u64::from(rect.height)));
    }
    let whole = FineDamageRect {
        x: 0,
        y: 0,
        width: size.0,
        height: size.1,
    };
    // Rectangles that would need more room than the whole surface read it whole.
    if placed.is_empty() || used > capacity {
        placed = vec![(whole, 0, whole_row)];
        used = capacity;
    }
    Ok((placed, used, capacity))
}

/// Wait for `slice` of `staging` to map after `submission`, bounded like every other
/// readback: a fence lost across a suspend never signals.
fn map_wait(
    device: &wgpu::Device,
    staging: &wgpu::Buffer,
    slice: &wgpu::BufferSlice<'_>,
    submission: wgpu::SubmissionIndex,
) -> Result<(), BridgeError> {
    let (tx, rx) = std::sync::mpsc::channel();
    slice.map_async(wgpu::MapMode::Read, move |result| {
        tx.send(result).ok();
    });
    if let Err(error) = device.poll(wgpu::PollType::Wait {
        submission_index: Some(submission),
        timeout: Some(std::time::Duration::from_secs(1)),
    }) {
        staging.unmap();
        return Err(match error {
            wgpu::PollError::Timeout => BridgeError::CompletionTimeout {
                phase: CompletionPhase::Fence,
            },
            other @ wgpu::PollError::WrongSubmissionIndex(..) => {
                BridgeError::Map(format!("output readback: {other:?}"))
            }
        });
    }
    match rx.recv_timeout(std::time::Duration::from_secs(1)) {
        Ok(Ok(())) => {}
        Ok(Err(error)) => {
            staging.unmap();
            return Err(BridgeError::Map(format!("output readback: {error:?}")));
        }
        Err(_) => {
            staging.unmap();
            return Err(BridgeError::CompletionTimeout {
                phase: CompletionPhase::MapCallback,
            });
        }
    }
    Ok(())
}
