//! The masked pictures of picked parts. Once the person finished picking,
//! the next frame (drawn without the picker) is copied from the GPU target
//! where each part is, its content texts are painted over on this device,
//! and the crops are stacked into the one picture the report carries. The
//! composer opens on the report when the copies are in; one that fails
//! leaves the report without a picture.

use std::cell::RefCell;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use tilo_lock::MutexRecover as _;

use tilo_app::feedback::PickedReport;
use tilo_app::feedback::report::{CropCapture, ReportDraft, compose_crops};
use ui::kurbo::Rect;

/// What the picked parts look like, wanted from the next presented frame.
pub(crate) struct Wanted {
    pub draft: ReportDraft,
    pub crops: Crops,
    /// Asks for a frame once the copies are in.
    pub wake: Arc<dyn Fn() + Send + Sync>,
}

thread_local! {
    static WANTED: RefCell<Option<Wanted>> = const { RefCell::new(None) };
    static READY: RefCell<Option<PickedReport>> = const { RefCell::new(None) };
}

/// Ask for the pictures of `wanted`'s parts from the next presented frame.
pub(crate) fn want(wanted: Wanted) {
    WANTED.with_borrow_mut(|slot| *slot = Some(wanted));
}

/// The report with its picture, once the copies are in.
pub(crate) fn take_ready() -> Option<PickedReport> {
    READY.with_borrow_mut(Option::take)
}

/// Each pick's place and where content is in it, in logical pixels.
pub(crate) type Crops = Vec<(Rect, Vec<Rect>)>;

struct Pending {
    buffer: wgpu::Buffer,
    width: u32,
    height: u32,
    padded: u32,
    sensitive: Vec<tilo_app::feedback::PixelRect>,
}

/// Copy what the wanted parts look like from `target`, the frame just
/// presented, at `dpr` device pixels to a logical one.
pub(crate) fn service(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    target: &wgpu::Texture,
    dpr: f64,
) {
    let Some(wanted) = WANTED.with_borrow_mut(Option::take) else {
        return;
    };
    let wake = wanted.wake;
    let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
        label: Some("feedback crops"),
    });
    let mut copies = Vec::new();
    for (rect, content) in &wanted.crops {
        let Some(copy) = copy_crop(device, &mut encoder, target, (*rect, content), dpr) else {
            return finish(wanted.draft, None, wake.as_ref());
        };
        copies.push(copy);
    }
    queue.submit([encoder.finish()]);
    // wgpu hands map callbacks over as if across threads.
    let copies = Arc::new(copies);
    let left = Arc::new(AtomicUsize::new(copies.len()));
    let failed = Arc::new(AtomicBool::new(false));
    let draft = Arc::new(Mutex::new(Some(wanted.draft)));
    for copy in copies.iter() {
        let (copies, left, failed, draft, wake) = (
            Arc::clone(&copies),
            Arc::clone(&left),
            Arc::clone(&failed),
            Arc::clone(&draft),
            Arc::clone(&wake),
        );
        copy.buffer
            .slice(..)
            .map_async(wgpu::MapMode::Read, move |result| {
                if result.is_err() {
                    failed.store(true, Ordering::Relaxed);
                }
                if left.fetch_sub(1, Ordering::AcqRel) > 1 {
                    return;
                }
                let Some(draft) = draft.lock_recover().take() else {
                    return;
                };
                let picture = (!failed.load(Ordering::Relaxed))
                    .then(|| compose_crops(copies.iter().map(read).collect()).ok())
                    .flatten();
                finish(draft, picture, wake.as_ref());
            });
    }
}

/// Record the copy of one part (`rect`, with `content` in it) from
/// `target`; `None` when nothing of it is on the target.
fn copy_crop(
    device: &wgpu::Device,
    encoder: &mut wgpu::CommandEncoder,
    target: &wgpu::Texture,
    (rect, content): (Rect, &[Rect]),
    dpr: f64,
) -> Option<Pending> {
    let bounds = Rect::new(
        0.0,
        0.0,
        f64::from(target.width()),
        f64::from(target.height()),
    );
    let place = scaled(rect.inflate(4.0, 4.0), dpr)
        .intersect(bounds)
        .round();
    let (width, height) = (px(place.width()), px(place.height()));
    if width == 0 || height == 0 {
        return None;
    }
    let padded = width
        .saturating_mul(4)
        .next_multiple_of(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT);
    let buffer = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("feedback crop"),
        size: u64::from(padded).saturating_mul(u64::from(height)),
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });
    encoder.copy_texture_to_buffer(
        wgpu::TexelCopyTextureInfo {
            texture: target,
            mip_level: 0,
            origin: wgpu::Origin3d {
                x: px(place.x0),
                y: px(place.y0),
                z: 0,
            },
            aspect: wgpu::TextureAspect::All,
        },
        wgpu::TexelCopyBufferInfo {
            buffer: &buffer,
            layout: wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(padded),
                rows_per_image: Some(height),
            },
        },
        wgpu::Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        },
    );
    let sensitive = content
        .iter()
        .map(|area| scaled(*area, dpr).intersect(place))
        .filter(|area| area.width() >= 1.0 && area.height() >= 1.0)
        .map(|area| {
            let area = Rect::new(
                (area.x0 - place.x0).floor(),
                (area.y0 - place.y0).floor(),
                (area.x1 - place.x0).ceil().min(f64::from(width)),
                (area.y1 - place.y0).ceil().min(f64::from(height)),
            );
            tilo_app::feedback::PixelRect {
                x: px(area.x0),
                y: px(area.y0),
                width: px(area.width()),
                height: px(area.height()),
            }
        })
        .collect();
    Some(Pending {
        buffer,
        width,
        height,
        padded,
        sensitive,
    })
}

/// A whole, non-negative pixel coordinate on the target.
#[expect(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "clamped to u32's range first"
)]
fn px(value: f64) -> u32 {
    value.round().clamp(0.0, f64::from(u32::MAX)) as u32
}

fn finish(
    draft: ReportDraft,
    picture: Option<(
        tilo_app::feedback::MaskedScreenshot,
        Vec<tilo_app::feedback::report::CropRef>,
    )>,
    wake: &(dyn Fn() + Send + Sync),
) {
    READY.with_borrow_mut(|slot| *slot = Some(PickedReport { draft, picture }));
    wake();
}

/// One copied crop, its rows unpadded.
fn read(copy: &Pending) -> CropCapture {
    let mapped = copy.buffer.slice(..).get_mapped_range();
    let row = (copy.width as usize).saturating_mul(4);
    let mut rgba = Vec::with_capacity(row.saturating_mul(copy.height as usize));
    for line in mapped
        .chunks(copy.padded as usize)
        .take(copy.height as usize)
    {
        rgba.extend_from_slice(line.get(..row).unwrap_or(line));
    }
    drop(mapped);
    copy.buffer.unmap();
    CropCapture {
        width: copy.width,
        height: copy.height,
        rgba,
        sensitive: copy.sensitive.clone(),
    }
}

fn scaled(rect: Rect, dpr: f64) -> Rect {
    Rect::new(rect.x0 * dpr, rect.y0 * dpr, rect.x1 * dpr, rect.y1 * dpr)
}
